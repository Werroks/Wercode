//! A terminal session: a shell running in a PTY plus the emulated screen.
//!
//! The UI never touches `alacritty_terminal` types. It asks for a [`Frame`]
//! (a plain grid of coloured cells) and sends bytes, resizes and selections.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::{Event as AEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{self, Color, CursorShape, NamedColor};
use anyhow::{Context as _, Result};
use portable_pty::{ChildKiller, CommandBuilder, PtySize, native_pty_system};

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
    /// The shell reported its working directory (shell integration).
    Cwd(PathBuf),
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
    /// The program wants mouse events (any of the modes below).
    pub mouse_report: bool,
    /// Report motion while a button is held.
    pub mouse_drag: bool,
    /// Report all motion.
    pub mouse_motion: bool,
    /// Use the SGR encoding (`ESC [ < ...`) rather than the legacy one.
    pub mouse_sgr: bool,
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
    /// Sizes for the PTY; applied by a helper thread once resizing settles.
    pty_resize: Mutex<mpsc::Sender<PtySize>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    size: Mutex<Size>,
    pid: Option<u32>,
    cwd: Arc<Mutex<Option<PathBuf>>>,
}

/// How long the window must stop changing size before the PTY is told.
/// Resizing a ConPTY on every frame of a drag makes the shell redraw in a storm.
const PTY_RESIZE_DELAY: Duration = Duration::from_millis(60);

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
        let pid = child.process_id();
        let cwd = Arc::new(Mutex::new(opts.cwd.clone().filter(|p| p.is_dir())));
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
            let cwd = cwd.clone();
            std::thread::Builder::new()
                .name("pty-reader".into())
                .spawn(move || {
                    let mut parser: ansi::Processor = ansi::Processor::new();
                    let mut osc = crate::osc::CwdScanner::default();
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if let Ok(mut term) = term.lock() {
                                    parser.advance(&mut *term, &buf[..n]);
                                }
                                if let Some(dir) = osc.feed(&buf[..n]) {
                                    if let Ok(mut cwd) = cwd.lock() {
                                        *cwd = Some(dir.clone());
                                    }
                                    notify(TermEvent::Cwd(dir));
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

        // PTY resizer. It owns the master end, so the PTY closes when the
        // `Terminal` (and with it the sender) is dropped.
        let (resize_tx, resize_rx) = mpsc::channel::<PtySize>();
        {
            let master = pair.master;
            std::thread::Builder::new()
                .name("pty-resize".into())
                .spawn(move || {
                    while let Ok(mut wanted) = resize_rx.recv() {
                        loop {
                            match resize_rx.recv_timeout(PTY_RESIZE_DELAY) {
                                Ok(newer) => wanted = newer,
                                Err(mpsc::RecvTimeoutError::Timeout) => break,
                                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                            }
                        }
                        let _ = master.resize(wanted);
                    }
                })
                .context("pty resize thread")?;
        }

        Ok(Terminal {
            term,
            writer,
            pty_resize: Mutex::new(resize_tx),
            killer: Mutex::new(killer),
            size: Mutex::new(size),
            pid,
            cwd,
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
        if let Ok(tx) = self.pty_resize.lock() {
            let _ = tx.send(PtySize {
                rows: new.rows as u16,
                cols: new.cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    /// The shell's working directory: what the OS says where that is cheap to
    /// ask, else what shell integration last reported, else where it started.
    pub fn cwd(&self) -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        if let Some(dir) =
            self.pid.and_then(|pid| std::fs::read_link(format!("/proc/{pid}/cwd")).ok())
        {
            return Some(dir);
        }
        self.cwd.lock().ok().and_then(|c| c.clone())
    }

    /// True while the shell is running something (it has child processes).
    /// Used to ask before closing a terminal that is in the middle of work.
    pub fn has_running_children(&self) -> bool {
        self.pid.is_some_and(crate::process::has_children)
    }

    /// The characters of one viewport row, one per column. The second half of
    /// a double-width character is `\0`.
    pub fn row_text(&self, row: usize) -> Vec<char> {
        let term = self.term.lock().unwrap();
        if row >= term.screen_lines() {
            return Vec::new();
        }
        let line = Line(row as i32 - term.grid().display_offset() as i32);
        (0..term.columns())
            .map(|col| {
                let cell = &term.grid()[line][Column(col)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    '\0'
                } else {
                    cell.c
                }
            })
            .collect()
    }

    pub fn modes(&self) -> Modes {
        let term = self.term.lock().unwrap();
        let m = *term.mode();
        Modes {
            app_cursor: m.contains(TermMode::APP_CURSOR),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            mouse_report: m.intersects(TermMode::MOUSE_MODE),
            mouse_drag: m.contains(TermMode::MOUSE_DRAG),
            mouse_motion: m.contains(TermMode::MOUSE_MOTION),
            mouse_sgr: m.contains(TermMode::SGR_MOUSE),
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

    /// With shell integration the shell tells us where it is after `cd`.
    #[cfg(windows)]
    #[test]
    fn powershell_reports_its_directory() {
        let shell = crate::shell::with_integration(crate::shell::resolve("auto", &[]));
        if !shell.args.iter().any(|a| a == "-NoExit") {
            return; // No PowerShell on this machine.
        }
        let target = std::env::temp_dir();
        let target = std::fs::canonicalize(&target).unwrap_or(target);
        let (tx, rx) = mpsc::channel();
        let term = Terminal::spawn(
            SpawnOptions { shell, cwd: None, cols: 80, rows: 24, scrollback: 100 },
            move |e| {
                let _ = tx.send(e);
            },
        )
        .unwrap();
        let same = |a: &std::path::Path| {
            std::fs::canonicalize(a).map(|a| a == target).unwrap_or(false)
        };
        let deadline = Instant::now() + Duration::from_secs(90);

        // The first report comes with the first prompt. Typing before it can
        // lose the keys on a slow machine: the shell is still starting.
        let mut ready = false;
        while Instant::now() < deadline && !ready {
            ready = matches!(rx.recv_timeout(Duration::from_millis(200)), Ok(TermEvent::Cwd(_)));
        }
        assert!(ready, "the shell never showed a prompt");

        // Typed without the `\\?\` prefix `canonicalize` adds: nobody types it.
        let typed = target.display().to_string();
        let typed = typed.strip_prefix(r"\\?\").unwrap_or(&typed);
        term.write(format!("cd '{typed}'\r").as_bytes());

        let mut seen = false;
        while Instant::now() < deadline && !seen {
            if let Ok(TermEvent::Cwd(dir)) = rx.recv_timeout(Duration::from_millis(200)) {
                seen = same(&dir);
            }
        }
        let palette = crate::theme::load("black").terminal;
        assert!(
            seen,
            "no directory report; last known: {:?}; screen was:\n{}",
            term.cwd(),
            text(&term.frame(&palette))
        );
        assert!(term.cwd().is_some_and(|d| same(&d)));
    }
}
