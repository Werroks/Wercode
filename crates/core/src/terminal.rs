//! A terminal session: a shell running in a PTY plus the emulated screen.
//!
//! The UI never touches `alacritty_terminal` types. It asks for a [`Frame`]
//! (a plain grid of coloured cells) and sends bytes, resizes and selections.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as AEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{self, Color, CursorShape, NamedColor};
use anyhow::{Context as _, Result};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::shell::Shell;
use crate::theme::{Rgb, TerminalColors};

/// Things the UI needs to hear about. Called from background threads.
#[derive(Debug, Clone)]
pub enum TermEvent {
    /// Screen content changed; repaint.
    Wakeup,
    Title(String),
    Bell,
    /// The program asked to put text on the clipboard (OSC 52).
    ClipboardStore(String),
    /// The shell exited, with its exit code if known.
    Exit(Option<u32>),
}

pub struct SpawnOptions {
    pub shell: Shell,
    pub cwd: Option<PathBuf>,
    pub cols: u16,
    pub rows: u16,
    pub scrollback: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorKind {
    Block,
    Bar,
    Underline,
    Hollow,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameCell {
    pub c: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Second half of a double-width character: draw nothing here.
    pub spacer: bool,
}

/// What is on screen right now, ready to paint.
#[derive(Debug, Clone)]
pub struct Frame {
    pub cols: usize,
    pub rows: usize,
    /// `rows * cols` cells, row by row.
    pub cells: Vec<FrameCell>,
    /// (row, col, shape) when the cursor is visible in the viewport.
    pub cursor: Option<(usize, usize, CursorKind)>,
    /// How many lines the view is scrolled back from the bottom.
    pub display_offset: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Modes {
    pub app_cursor: bool,
    pub alt_screen: bool,
    pub bracketed_paste: bool,
    pub mouse_report: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKind {
    Cell,
    Word,
    Line,
}

#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

type Notify = Arc<dyn Fn(TermEvent) + Send + Sync>;
type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

struct Listener {
    writer: Writer,
    notify: Notify,
}

impl EventListener for Listener {
    fn send_event(&self, event: AEvent) {
        match event {
            AEvent::PtyWrite(text) => {
                if let Ok(mut w) = self.writer.lock() {
                    let _ = w.write_all(text.as_bytes());
                    let _ = w.flush();
                }
            }
            AEvent::Title(t) => (self.notify)(TermEvent::Title(t)),
            AEvent::Bell => (self.notify)(TermEvent::Bell),
            AEvent::ClipboardStore(_, text) => (self.notify)(TermEvent::ClipboardStore(text)),
            AEvent::Wakeup => (self.notify)(TermEvent::Wakeup),
            _ => {}
        }
    }
}

pub struct Terminal {
    term: Arc<Mutex<Term<Listener>>>,
    writer: Writer,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    size: Mutex<Size>,
}

impl Terminal {
    /// Starts the shell. `notify` is called from background threads, so it
    /// must only do something cheap such as sending on a channel.
    pub fn spawn(
        opts: SpawnOptions,
        notify: impl Fn(TermEvent) + Send + Sync + 'static,
    ) -> Result<Terminal> {
        let notify: Notify = Arc::new(notify);
        let cols = opts.cols.max(2);
        let rows = opts.rows.max(1);

        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("failed to open a pseudo-terminal")?;

        let mut cmd = CommandBuilder::new(&opts.shell.program);
        cmd.args(&opts.shell.args);
        if let Some(cwd) = opts.cwd.as_ref().filter(|p| p.is_dir()) {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "wercode");

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("failed to start {}", opts.shell.program.display()))?;
        drop(pair.slave);

        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().context("pty reader")?;
        let writer: Writer = Arc::new(Mutex::new(pair.master.take_writer().context("pty writer")?));

        let size = Size { cols: cols as usize, rows: rows as usize };
        let config = Config { scrolling_history: opts.scrollback, ..Config::default() };
        let listener = Listener { writer: writer.clone(), notify: notify.clone() };
        let term = Arc::new(Mutex::new(Term::new(config, &size, listener)));

        // PTY -> emulator. One lock per chunk keeps the UI responsive under heavy output.
        {
            let term = term.clone();
            let notify = notify.clone();
            std::thread::Builder::new()
                .name("pty-reader".into())
                .spawn(move || {
                    let mut parser: ansi::Processor = ansi::Processor::new();
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if let Ok(mut term) = term.lock() {
                                    parser.advance(&mut *term, &buf[..n]);
                                }
                                notify(TermEvent::Wakeup);
                            }
                        }
                    }
                })
                .context("pty reader thread")?;
        }

        // Exit watcher. On Windows the reader does not reliably see EOF.
        {
            let notify = notify.clone();
            std::thread::Builder::new()
                .name("pty-wait".into())
                .spawn(move || {
                    let code = child.wait().ok().map(|s| s.exit_code());
                    notify(TermEvent::Exit(code));
                })
                .context("pty wait thread")?;
        }

        Ok(Terminal {
            term,
            writer,
            master: Mutex::new(pair.master),
            killer: Mutex::new(killer),
            size: Mutex::new(size),
        })
    }

    /// Sends raw bytes to the shell.
    pub fn write(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    /// Pastes text, wrapped in bracketed-paste markers when the program asked for them.
    pub fn paste(&self, text: &str) {
        if self.modes().bracketed_paste {
            // Strip the end marker so pasted text cannot break out of the paste.
            let clean = text.replace("\x1b[201~", "");
            self.write(format!("\x1b[200~{clean}\x1b[201~").as_bytes());
        } else {
            self.write(text.replace("\r\n", "\r").replace('\n', "\r").as_bytes());
        }
    }

    pub fn size(&self) -> (usize, usize) {
        let s = self.size.lock().unwrap();
        (s.cols, s.rows)
    }

    /// Resizes screen and PTY. Does nothing when the size is unchanged.
    pub fn resize(&self, cols: u16, rows: u16) {
        let new = Size { cols: cols.max(2) as usize, rows: rows.max(1) as usize };
        {
            let mut size = self.size.lock().unwrap();
            if size.cols == new.cols && size.rows == new.rows {
                return;
            }
            *size = new;
        }
        if let Ok(mut term) = self.term.lock() {
            term.resize(new);
        }
        if let Ok(master) = self.master.lock() {
            let _ = master.resize(PtySize {
                rows: new.rows as u16,
                cols: new.cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    pub fn modes(&self) -> Modes {
        let term = self.term.lock().unwrap();
        let m = *term.mode();
        Modes {
            app_cursor: m.contains(TermMode::APP_CURSOR),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            mouse_report: m.intersects(TermMode::MOUSE_MODE),
        }
    }

    /// Scrolls the view through history. Positive is up (older output).
    pub fn scroll_lines(&self, lines: i32) {
        if let Ok(mut term) = self.term.lock() {
            term.scroll_display(Scroll::Delta(lines));
        }
    }

    pub fn scroll_to_bottom(&self) {
        if let Ok(mut term) = self.term.lock() {
            term.scroll_display(Scroll::Bottom);
        }
    }

    fn point(term: &Term<Listener>, row: usize, col: usize) -> Point {
        let offset = term.grid().display_offset() as i32;
        let col = col.min(term.columns().saturating_sub(1));
        Point::new(Line(row as i32 - offset), Column(col))
    }

    /// Starts a selection at a viewport cell. `right_half` says which half of
    /// the cell the pointer is in.
    pub fn select_start(&self, row: usize, col: usize, right_half: bool, kind: SelectKind) {
        let mut term = self.term.lock().unwrap();
        let point = Self::point(&term, row, col);
        let ty = match kind {
            SelectKind::Cell => SelectionType::Simple,
            SelectKind::Word => SelectionType::Semantic,
            SelectKind::Line => SelectionType::Lines,
        };
        let side = if right_half { Side::Right } else { Side::Left };
        term.selection = Some(Selection::new(ty, point, side));
    }

    pub fn select_update(&self, row: usize, col: usize, right_half: bool) {
        let mut term = self.term.lock().unwrap();
        let point = Self::point(&term, row, col);
        let side = if right_half { Side::Right } else { Side::Left };
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, side);
        }
    }

    pub fn clear_selection(&self) {
        self.term.lock().unwrap().selection = None;
    }

    /// The selected text, or `None` when nothing (or only whitespace-free emptiness) is selected.
    pub fn selection_text(&self) -> Option<String> {
        let term = self.term.lock().unwrap();
        term.selection_to_string().filter(|s| !s.is_empty())
    }

    /// Takes a snapshot of the visible screen with all colours resolved.
    pub fn frame(&self, palette: &TerminalColors) -> Frame {
        let term = self.term.lock().unwrap();
        let cols = term.columns();
        let rows = term.screen_lines();
        let content = term.renderable_content();
        let offset = content.display_offset as i32;
        let colors = content.colors;

        let blank = FrameCell {
            c: ' ',
            fg: palette.foreground,
            bg: palette.background,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            spacer: false,
        };
        let mut cells = vec![blank; cols * rows];

        for indexed in content.display_iter {
            let row = indexed.point.line.0 + offset;
            let col = indexed.point.column.0;
            if row < 0 || row as usize >= rows || col >= cols {
                continue;
            }
            let cell = indexed.cell;
            let flags = cell.flags;
            let mut fg = resolve(cell.fg, palette, colors);
            let mut bg = resolve(cell.bg, palette, colors);
            if flags.contains(Flags::DIM) {
                fg = fg.dim();
            }
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if content.selection.is_some_and(|s| s.contains(indexed.point)) {
                bg = palette.selection;
                fg = palette.foreground;
            }
            if flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            cells[row as usize * cols + col] = FrameCell {
                c: cell.c,
                fg,
                bg,
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                underline: flags.intersects(Flags::ALL_UNDERLINES),
                strike: flags.contains(Flags::STRIKEOUT),
                spacer: flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
            };
        }

        let cursor = {
            let row = content.cursor.point.line.0 + offset;
            let col = content.cursor.point.column.0;
            let kind = match content.cursor.shape {
                CursorShape::Block => Some(CursorKind::Block),
                CursorShape::Beam => Some(CursorKind::Bar),
                CursorShape::Underline => Some(CursorKind::Underline),
                CursorShape::HollowBlock => Some(CursorKind::Hollow),
                CursorShape::Hidden => None,
            };
            match kind {
                Some(kind) if row >= 0 && (row as usize) < rows && col < cols => {
                    Some((row as usize, col, kind))
                }
                _ => None,
            }
        };

        Frame { cols, rows, cells, cursor, display_offset: content.display_offset }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Ok(mut k) = self.killer.lock() {
            let _ = k.kill();
        }
    }
}

fn resolve(color: Color, palette: &TerminalColors, overrides: &Colors) -> Rgb {
    let from = |c: ansi::Rgb| Rgb(c.r, c.g, c.b);
    match color {
        Color::Spec(rgb) => from(rgb),
        Color::Indexed(i) => overrides[i as usize].map(from).unwrap_or_else(|| palette.indexed(i)),
        Color::Named(name) => {
            if let Some(rgb) = overrides[name] {
                return from(rgb);
            }
            let n = name as usize;
            match name {
                _ if n < 16 => palette.ansi[n],
                NamedColor::Foreground | NamedColor::BrightForeground => palette.foreground,
                NamedColor::Background => palette.background,
                NamedColor::Cursor => palette.cursor,
                NamedColor::DimForeground => palette.foreground.dim(),
                _ => {
                    let i = n.saturating_sub(NamedColor::DimBlack as usize);
                    palette.ansi[i.min(7)].dim()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn text(frame: &Frame) -> String {
        frame
            .cells
            .chunks(frame.cols)
            .map(|row| row.iter().map(|c| c.c).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn runs_a_shell_and_shows_its_output() {
        let palette = crate::theme::load("black").terminal;
        let (tx, rx) = mpsc::channel();
        let term = Terminal::spawn(
            SpawnOptions {
                shell: crate::shell::resolve("auto", &[]),
                cwd: None,
                cols: 80,
                rows: 24,
                scrollback: 100,
            },
            move |e| {
                let _ = tx.send(e);
            },
        )
        .unwrap();

        // Built from two parts so the typed command itself does not match.
        term.write(b"echo wer\"\"code-ok\r");
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = false;
        while Instant::now() < deadline && !seen {
            let _ = rx.recv_timeout(Duration::from_millis(200));
            seen = text(&term.frame(&palette)).contains("wercode-ok");
        }
        assert!(seen, "screen was:\n{}", text(&term.frame(&palette)));

        term.resize(100, 30);
        assert_eq!(term.size(), (100, 30));
        let f = term.frame(&palette);
        assert_eq!((f.cols, f.rows), (100, 30));
    }
}
