//! A file open for editing in a tab next to the terminals.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent, Position};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use wercode_core::document::{self, Document, Format, LoadError};

actions!(editor, [Save]);

/// How often an open file is checked for changes made by other programs.
const DISK_POLL: Duration = Duration::from_secs(1);

/// Plain Ctrl+S is safe here: the keyboard is in a text editor, not in a shell.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-s", Save, Some("EditorView"))]);
}

pub enum EditorEvent {
    /// The name or the unsaved mark may have changed.
    Changed,
}

pub struct EditorView {
    path: PathBuf,
    state: Entity<EditorState>,
    focus: FocusHandle,
    format: Format,
    /// Modification time of the file as last read or written here.
    modified: Option<SystemTime>,
    loaded: bool,
    dirty: bool,
    /// Another program wrote the file while there are unsaved edits here.
    changed_on_disk: bool,
    /// Why the file is not shown: too large, not text, unreadable.
    problem: Option<String>,
    error: Option<String>,
    /// Where to put the cursor once the text has arrived (0-based line, column).
    pending_position: Option<(u32, u32)>,
    _subscription: Subscription,
    _watch: Task<()>,
}

impl EventEmitter<EditorEvent> for EditorView {}

impl Focusable for EditorView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.problem.is_some() { self.focus.clone() } else { self.state.focus_handle(cx) }
    }
}

impl EditorView {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let language = document::language(&path);
        let state = cx.new(|cx| EditorState::new(window, cx).language(language));
        let subscription = cx.subscribe(&state, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && this.loaded && !this.dirty {
                this.dirty = true;
                cx.emit(EditorEvent::Changed);
                cx.notify();
            }
        });
        let watch = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(DISK_POLL).await;
                let Ok(path) = this.read_with(cx, |this, _| this.path.clone()) else { break };
                let on_disk = cx.background_executor().spawn(async move { document::modified(&path) }).await;
                if this.update_in(cx, |this, window, cx| this.disk_changed(on_disk, window, cx)).is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            path,
            state,
            focus: cx.focus_handle(),
            format: Format::default(),
            modified: None,
            loaded: false,
            dirty: false,
            changed_on_disk: false,
            problem: None,
            error: None,
            pending_position: None,
            _subscription: subscription,
            _watch: watch,
        };
        this.reload(window, cx);
        this
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file was renamed or moved; this tab now belongs to its new path.
    pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.path = path;
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Moves the cursor to a 1-based line and column, as written in compiler output.
    pub fn go_to(&mut self, line: u32, col: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        let position = (line.saturating_sub(1), col.unwrap_or(1).saturating_sub(1));
        if self.loaded {
            self.state.update(cx, |state, cx| {
                state.set_cursor_position(Position::new(position.0, position.1), window, cx)
            });
        } else {
            self.pending_position = Some(position);
        }
    }

    /// Reads the file from disk, replacing what is in the editor.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { document::load(&path) }).await;
            let _ = this.update_in(cx, |this, window, cx| this.show(result, window, cx));
        })
        .detach();
    }

    fn show(&mut self, result: Result<Document, LoadError>, window: &mut Window, cx: &mut Context<Self>) {
        match result {
            Ok(doc) => {
                let first = !self.loaded;
                self.format = doc.format;
                self.modified = doc.modified;
                self.problem = None;
                let position = self.pending_position.take();
                self.state.update(cx, |state, cx| {
                    // A file rewritten by another program keeps its place on screen.
                    let (cursor, scroll) = (state.cursor_position(), state.scroll_offset());
                    state.set_value(doc.text, window, cx);
                    if let Some((line, col)) = position {
                        state.set_cursor_position(Position::new(line, col), window, cx);
                        // The editor can only scroll to a line it has laid out, and
                        // the new text is laid out when the next frame is drawn.
                        cx.on_next_frame(window, move |state, window, cx| {
                            state.set_cursor_position(Position::new(line, col), window, cx);
                        });
                    } else if !first {
                        let had_focus = state.focus_handle(cx).is_focused(window);
                        let before = window.focused(cx);
                        state.set_cursor_position(cursor, window, cx);
                        state.set_scroll_offset(scroll, cx);
                        // Putting the cursor back must not pull the keyboard out of a terminal.
                        if !had_focus && let Some(before) = before {
                            before.focus(window, cx);
                        }
                    }
                });
                self.loaded = true;
            }
            Err(error) => {
                self.modified = document::modified(&self.path);
                self.problem = Some(error.message());
            }
        }
        self.dirty = false;
        self.changed_on_disk = false;
        self.error = None;
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    /// Called with the file's current modification time once a second.
    fn disk_changed(&mut self, on_disk: Option<SystemTime>, window: &mut Window, cx: &mut Context<Self>) {
        if on_disk == self.modified || on_disk.is_none() {
            return;
        }
        // Remember it either way, so the same change is reported once.
        self.modified = on_disk;
        if self.dirty {
            self.changed_on_disk = true;
            cx.notify();
        } else {
            self.reload(window, cx);
        }
    }

    /// Writes the text to disk. `then` runs once it is there.
    pub fn save(
        &mut self,
        then: impl FnOnce(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.loaded || self.problem.is_some() {
            return;
        }
        let text = self.state.read(cx).value().to_string();
        let (path, format) = (self.path.clone(), self.format);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { document::save(&path, &text, format) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(modified) => {
                        this.modified = modified;
                        this.dirty = false;
                        this.changed_on_disk = false;
                        this.error = None;
                        then(window, cx);
                    }
                    Err(e) => this.error = Some(format!("Could not save {}: {e}", this.file_name())),
                }
                cx.emit(EditorEvent::Changed);
                cx.notify();
            });
        })
        .detach();
    }

    fn render_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let bar = || {
            h_flex().flex_none().w_full().px_3().py_1().gap_2().text_sm().border_b_1().border_color(theme.border)
        };
        if let Some(error) = &self.error {
            return Some(bar().id("editor-error").text_color(theme.danger).child(error.clone()).into_any_element());
        }
        if !self.changed_on_disk {
            return None;
        }
        Some(
            bar()
                .id("editor-changed")
                .text_color(theme.warning)
                .child(div().flex_1().min_w_0().child("This file was changed on disk by another program."))
                .child(Button::new("editor-reload").small().label("Reload from Disk").on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| this.reload(window, cx),
                )))
                .child(Button::new("editor-keep").small().ghost().label("Keep My Changes").on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.changed_on_disk = false;
                        cx.notify();
                    },
                )))
                .into_any_element(),
        )
    }
}

impl Render for EditorView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let body = match &self.problem {
            Some(problem) => {
                let path = self.path.clone();
                v_flex()
                    .id("editor-problem")
                    .track_focus(&self.focus)
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(div().text_sm().text_color(muted).child(problem.clone()))
                    .child(
                        Button::new("editor-open-outside")
                            .small()
                            .label("Open in the Default App")
                            .on_click(move |_, _, cx| cx.open_with_system(&path)),
                    )
                    .into_any_element()
            }
            None => Editor::new(&self.state).bordered(false).h(relative(1.)).into_any_element(),
        };
        v_flex()
            .id("editor-view")
            .key_context("EditorView")
            .size_full()
            .bg(theme.background)
            .on_action(cx.listener(|this, _: &Save, window, cx| this.save(|_, _| {}, window, cx)))
            .children(self.render_bar(cx))
            .child(div().flex_1().min_h_0().w_full().child(body))
    }
}

#[cfg(test)]
impl EditorView {
    pub fn text(&self, cx: &App) -> String {
        self.state.read(cx).value().to_string()
    }

    pub fn changed_on_disk(&self) -> bool {
        self.changed_on_disk
    }

    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Whether the find bar of the editor is showing.
    pub fn is_searching(&self, cx: &App) -> bool {
        self.state.read(cx).search_session().is_active()
    }

    /// Replaces every match of `query`, as the Replace All button does.
    pub fn replace_all(&mut self, query: &str, with: &str, window: &mut Window, cx: &mut Context<Self>) -> usize {
        self.state.update(cx, |state, cx| {
            state.set_search_query(query, true, cx);
            state.replace_all_search_matches(with, window, cx)
        })
    }
}
