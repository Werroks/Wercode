//! The terminal widget: paints a [`Frame`] and feeds input back to the shell.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::*;
use wercode_core::terminal::{
    CursorKind, Frame, FrameCell, SelectKind, SpawnOptions, TermEvent, Terminal,
};
use wercode_core::theme::Rgb;

use super::input;
use crate::theme::{AppState, hsla};

const PADDING: f32 = 8.0;

/// Where the grid was last painted, for turning pointer positions into cells.
#[derive(Clone, Copy, Default)]
struct Metrics {
    origin: Point<Pixels>,
    cell_w: Pixels,
    line_h: Pixels,
}

pub enum TerminalViewEvent {
    TitleChanged,
    Exited,
}

pub struct TerminalView {
    term: Option<Arc<Terminal>>,
    focus: FocusHandle,
    metrics: Rc<Cell<Metrics>>,
    title: SharedString,
    error: Option<SharedString>,
    exit_code: Option<Option<u32>>,
    selecting: bool,
    scroll_remainder: f32,
    dirty: Arc<AtomicBool>,
    _events: Option<Task<()>>,
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalView {
    pub fn new(cwd: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let settings = cx.global::<AppState>().settings.clone();
        let shell = wercode_core::shell::resolve(&settings.shell, &settings.shell_args);
        let title: SharedString = shell
            .program
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "shell".into())
            .into();

        let dirty = Arc::new(AtomicBool::new(false));
        let (tx, mut rx) = mpsc::unbounded::<TermEvent>();
        let notify = {
            let dirty = dirty.clone();
            move |event: TermEvent| {
                // Many wakeups between two frames collapse into one message.
                if matches!(event, TermEvent::Wakeup) && dirty.swap(true, Ordering::AcqRel) {
                    return;
                }
                let _ = tx.unbounded_send(event);
            }
        };

        let spawned = Terminal::spawn(
            SpawnOptions { shell, cwd, cols: 80, rows: 24, scrollback: settings.scrollback },
            notify,
        );
        let (term, error) = match spawned {
            Ok(t) => (Some(Arc::new(t)), None),
            Err(e) => (None, Some(SharedString::from(format!("{e:#}")))),
        };

        let events = cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                if this.update(cx, |this, cx| this.on_term_event(event, cx)).is_err() {
                    break;
                }
            }
        });

        Self {
            term,
            focus: cx.focus_handle(),
            metrics: Rc::new(Cell::new(Metrics::default())),
            title,
            error,
            exit_code: None,
            selecting: false,
            scroll_remainder: 0.0,
            dirty,
            _events: Some(events),
        }
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// Types text into the shell as if pasted (used for inserting file paths).
    pub fn insert_text(&self, text: &str) {
        if let Some(term) = &self.term {
            term.paste(text);
        }
    }

    fn on_term_event(&mut self, event: TermEvent, cx: &mut Context<Self>) {
        match event {
            TermEvent::Wakeup => {
                self.dirty.store(false, Ordering::Release);
                cx.notify();
            }
            TermEvent::Title(t) => {
                self.title = t.into();
                cx.emit(TerminalViewEvent::TitleChanged);
                cx.notify();
            }
            TermEvent::Bell => {}
            TermEvent::ClipboardStore(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            TermEvent::Exit(code) => {
                self.exit_code = Some(code);
                cx.emit(TerminalViewEvent::Exited);
                cx.notify();
            }
        }
    }

    fn cell_at(&self, pos: Point<Pixels>) -> Option<(usize, usize, bool)> {
        let m = self.metrics.get();
        if m.cell_w <= px(0.) || m.line_h <= px(0.) {
            return None;
        }
        let (cols, rows) = self.term.as_ref()?.size();
        let x = ((pos.x - m.origin.x) / m.cell_w).max(0.0);
        let y = ((pos.y - m.origin.y) / m.line_h).max(0.0);
        let col = (x.floor() as usize).min(cols.saturating_sub(1));
        let row = (y.floor() as usize).min(rows.saturating_sub(1));
        Some((row, col, x.fract() > 0.5))
    }

    fn copy_selection(&self, cx: &mut Context<Self>) -> bool {
        let Some(text) = self.term.as_ref().and_then(|t| t.selection_text()) else {
            return false;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    fn paste_clipboard(&self, cx: &mut Context<Self>) {
        let Some(term) = &self.term else { return };
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            term.scroll_to_bottom();
            term.paste(&text);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(term) = self.term.clone() else { return };
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        let key = ks.key.as_str();
        if std::env::var_os("WERCODE_DEBUG_KEYS").is_some() {
            eprintln!("key={:?} char={:?} mods={:?}", ks.key, ks.key_char, ks.modifiers);
        }

        // Ctrl+Shift+<key> and Alt-less app shortcuts stay with the app.
        if m.control && m.shift && !m.alt {
            match key {
                "c" => {
                    self.copy_selection(cx);
                    cx.stop_propagation();
                    return;
                }
                "v" => {
                    self.paste_clipboard(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => return,
            }
        }
        if m.shift && !m.control && !m.alt {
            let (_, rows) = term.size();
            let page = rows.saturating_sub(1).max(1) as i32;
            match key {
                "pageup" => {
                    term.scroll_lines(page);
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                "pagedown" => {
                    term.scroll_lines(-page);
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                "insert" => {
                    self.paste_clipboard(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        // Windows convention, same as VS Code: Ctrl+C copies when text is
        // selected and interrupts otherwise; Ctrl+V pastes.
        if cfg!(windows) && m.control && !m.shift && !m.alt {
            if key == "c" && self.copy_selection(cx) {
                term.clear_selection();
                cx.notify();
                cx.stop_propagation();
                return;
            }
            if key == "v" {
                self.paste_clipboard(cx);
                cx.stop_propagation();
                return;
            }
        }

        if self.exit_code.is_some() {
            return;
        }
        if let Some(bytes) = input::encode(ks, term.modes().app_cursor) {
            term.clear_selection();
            term.scroll_to_bottom();
            term.write(&bytes);
            cx.notify();
            cx.stop_propagation();
        }
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let Some(term) = self.term.clone() else { return };
        let Some((row, col, right)) = self.cell_at(event.position) else { return };
        let kind = match event.click_count {
            0 | 1 => SelectKind::Cell,
            2 => SelectKind::Word,
            _ => SelectKind::Line,
        };
        if event.modifiers.shift {
            term.select_update(row, col, right);
        } else {
            term.select_start(row, col, right, kind);
        }
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            self.selecting = false;
            return;
        }
        let Some(term) = self.term.clone() else { return };
        if let Some((row, col, right)) = self.cell_at(event.position) {
            term.select_update(row, col, right);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        self.selecting = false;
        let copy_on_select = cx.global::<AppState>().settings.copy_on_select;
        if copy_on_select {
            self.copy_selection(cx);
        }
    }

    /// Right click: copy the selection if there is one, otherwise paste.
    fn on_right_click(&mut self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.copy_selection(cx) {
            if let Some(term) = &self.term {
                term.clear_selection();
            }
            cx.notify();
        } else {
            self.paste_clipboard(cx);
        }
    }

    fn on_scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(term) = self.term.clone() else { return };
        let line_h = self.metrics.get().line_h;
        if line_h <= px(0.) {
            return;
        }
        let dy = event.delta.pixel_delta(line_h).y;
        self.scroll_remainder += dy / line_h;
        let lines = self.scroll_remainder.trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= lines as f32;

        let modes = term.modes();
        if modes.alt_screen {
            // Full-screen programs have no history: send arrow keys instead.
            let seq: &[u8] = match (lines > 0, modes.app_cursor) {
                (true, false) => b"\x1b[A",
                (true, true) => b"\x1bOA",
                (false, false) => b"\x1b[B",
                (false, true) => b"\x1bOB",
            };
            for _ in 0..lines.unsigned_abs().min(50) {
                term.write(seq);
            }
        } else {
            term.scroll_lines(lines);
        }
        cx.notify();
    }
}

struct Painted {
    frame: Frame,
    lines: Vec<(usize, usize, ShapedLine)>,
    metrics: Metrics,
    focused: bool,
    cursor_color: Hsla,
    default_bg: Rgb,
}

fn same_style(a: &FrameCell, b: &FrameCell) -> bool {
    a.fg == b.fg && a.bold == b.bold && a.italic == b.italic && a.underline == b.underline && a.strike == b.strike
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = cx.global::<AppState>();
        let palette = state.scheme.terminal.clone();
        let muted = hsla(state.scheme.ui.text_muted);
        let danger = hsla(state.scheme.ui.danger);
        let font_family: SharedString = state.settings.font_family.clone().into();
        let font_size = px(state.settings.font_size);
        let line_h = px((state.settings.font_size * state.settings.line_height).round());
        let focused = self.focus.is_focused(window);
        let bg = hsla(palette.background);

        let mut root = div()
            .id("terminal")
            .track_focus(&self.focus)
            .key_context("Terminal")
            .size_full()
            .relative()
            .bg(bg)
            .p(px(PADDING))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_click))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll(cx.listener(Self::on_scroll));

        let Some(term) = self.term.clone() else {
            let message = self.error.clone().unwrap_or_else(|| "Could not start a shell".into());
            return root.child(div().text_color(danger).child(message)).into_any_element();
        };

        let shared = self.metrics.clone();
        let grid = canvas(
            move |bounds, window, _cx| {
                let base = font(font_family.clone());
                let text_system = window.text_system().clone();
                let font_id = text_system.resolve_font(&base);
                let cell_w = text_system
                    .em_advance(font_id, font_size)
                    .unwrap_or(font_size * 0.6);
                let metrics = Metrics { origin: bounds.origin, cell_w, line_h };
                shared.set(metrics);

                let cols = (bounds.size.width / cell_w).floor().max(2.0) as u16;
                let rows = (bounds.size.height / line_h).floor().max(1.0) as u16;
                term.resize(cols, rows);

                let mut frame = term.frame(&palette);
                if focused && let Some((row, col, CursorKind::Block)) = frame.cursor {
                    let cell = &mut frame.cells[row * frame.cols + col];
                    cell.bg = palette.cursor;
                    cell.fg = palette.background;
                }

                // One shaped line per run of identically styled cells. Fixed
                // cell width keeps the grid aligned whatever font falls back.
                let mut lines = Vec::new();
                for row in 0..frame.rows {
                    let cells = &frame.cells[row * frame.cols..(row + 1) * frame.cols];
                    let mut col = 0;
                    while col < cells.len() {
                        let start = col;
                        let first = cells[col];
                        let mut text = String::new();
                        while col < cells.len() && same_style(&cells[col], &first) {
                            let c = cells[col];
                            if c.spacer {
                                col += 1;
                                break;
                            }
                            text.push(if c.c == '\0' { ' ' } else { c.c });
                            col += 1;
                        }
                        if text.trim().is_empty() && !first.underline && !first.strike {
                            continue;
                        }
                        let mut f = base.clone();
                        if first.bold {
                            f = f.bold();
                        }
                        if first.italic {
                            f = f.italic();
                        }
                        let color = hsla(first.fg);
                        let run = TextRun {
                            len: text.len(),
                            font: f,
                            color,
                            background_color: None,
                            underline: first.underline.then(|| UnderlineStyle {
                                thickness: px(1.),
                                color: Some(color),
                                wavy: false,
                            }),
                            strikethrough: first.strike.then(|| StrikethroughStyle {
                                thickness: px(1.),
                                color: Some(color),
                            }),
                        };
                        let shaped =
                            text_system.shape_line(text.into(), font_size, &[run], Some(cell_w));
                        lines.push((row, start, shaped));
                    }
                }

                Painted {
                    frame,
                    lines,
                    metrics,
                    focused,
                    cursor_color: hsla(palette.cursor),
                    default_bg: palette.background,
                }
            },
            move |_bounds, painted: Painted, window, cx| {
                let Metrics { origin, cell_w, line_h } = painted.metrics;
                let frame = &painted.frame;
                let cell_bounds = |row: usize, col: usize, span: usize| {
                    Bounds::new(
                        point(origin.x + cell_w * col as f32, origin.y + line_h * row as f32),
                        size(cell_w * span as f32, line_h),
                    )
                };

                // Backgrounds, merged into one rectangle per run.
                for row in 0..frame.rows {
                    let cells = &frame.cells[row * frame.cols..(row + 1) * frame.cols];
                    let mut col = 0;
                    while col < cells.len() {
                        let bg = cells[col].bg;
                        let start = col;
                        while col < cells.len() && cells[col].bg == bg {
                            col += 1;
                        }
                        if bg != painted.default_bg {
                            window.paint_quad(fill(cell_bounds(row, start, col - start), hsla(bg)));
                        }
                    }
                }

                for (row, col, line) in &painted.lines {
                    let at = point(origin.x + cell_w * *col as f32, origin.y + line_h * *row as f32);
                    let _ = line.paint(at, line_h, TextAlign::Left, None, window, cx);
                }

                if let Some((row, col, kind)) = frame.cursor {
                    let b = cell_bounds(row, col, 1);
                    let color = painted.cursor_color;
                    let t = px(1.);
                    if !painted.focused || kind == CursorKind::Hollow {
                        // Outline only, so it is clear the terminal is not listening.
                        window.paint_quad(fill(Bounds::new(b.origin, size(b.size.width, t)), color));
                        window.paint_quad(fill(
                            Bounds::new(point(b.origin.x, b.origin.y + b.size.height - t), size(b.size.width, t)),
                            color,
                        ));
                        window.paint_quad(fill(Bounds::new(b.origin, size(t, b.size.height)), color));
                        window.paint_quad(fill(
                            Bounds::new(point(b.origin.x + b.size.width - t, b.origin.y), size(t, b.size.height)),
                            color,
                        ));
                    } else if kind == CursorKind::Bar {
                        window.paint_quad(fill(Bounds::new(b.origin, size(px(2.), b.size.height)), color));
                    } else if kind == CursorKind::Underline {
                        window.paint_quad(fill(
                            Bounds::new(
                                point(b.origin.x, b.origin.y + b.size.height - px(2.)),
                                size(b.size.width, px(2.)),
                            ),
                            color,
                        ));
                    }
                }
            },
        )
        .size_full();

        root = root.child(grid);
        if let Some(code) = self.exit_code {
            let text = match code {
                Some(code) => format!("Process exited with code {code}"),
                None => "Process exited".to_string(),
            };
            root = root.child(
                div()
                    .absolute()
                    .bottom(px(PADDING))
                    .right(px(PADDING))
                    .text_color(muted)
                    .text_sm()
                    .child(text),
            );
        }
        root.into_any_element()
    }
}
