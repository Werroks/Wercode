//! A file open for editing in a tab next to the terminals.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::FutureExt as _;
use futures::channel::oneshot;
use gpui_kit::base::input::{
    CompletionProvider, DefinitionProvider, Diagnostic, DiagnosticSeverity, HoverProvider, Rope, RopeExt as _,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Editor, EditorState, GutterDecoration, GutterDecorationCollection, InputEvent, Position,
};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use wercode_core::document::{self, Document, Format, LoadError};
use wercode_core::git;
use wercode_core::i18n::{t, tf};
use wercode_core::linediff::{self, Kind};
use wercode_core::lsp::{Edit, Problem, Servers, Severity, Value, path_from_uri, uri_from_path};

use crate::lsp::CheckerEvent;
use crate::theme::AppState;

actions!(editor, [Save]);

/// How often an open file is checked for changes made by other programs.
const DISK_POLL: Duration = Duration::from_secs(1);
/// Typing reaches the language servers this long after the last key.
const TYPING_PAUSE: Duration = Duration::from_millis(300);
/// How long saving waits for a language server to format the file.
const FORMAT_WAIT: Duration = Duration::from_secs(2);
/// The indent asked of a formatter that has no rule of its own.
const FORMAT_INDENT: u32 = 4;

/// The shortcut for saving is bound with the others, in `keymap`.
pub fn init(_: &mut App) {}

pub enum EditorEvent {
    /// The name or the unsaved mark may have changed.
    Changed,
    /// Another file is wanted in a tab, at a 1-based line and column: the
    /// one where a name of this file is defined.
    Open { path: PathBuf, at: (u32, u32) },
}

/// Asks the language servers about the code of one file: what is under the
/// pointer, and where a name is defined.
struct Asker {
    servers: Servers,
    path: PathBuf,
}

impl Asker {
    /// The line and column of a place in the text. The servers get the text
    /// first, in case typing has not paused yet: they answer about what
    /// they have.
    fn place(&self, text: &Rope, offset: usize) -> Position {
        self.servers.change(&self.path, text.to_string());
        text.offset_to_position(offset)
    }
}

fn range(start: (u32, u32), end: (u32, u32)) -> lsp_types::Range {
    lsp_types::Range::new(Position::new(start.0, start.1), Position::new(end.0, end.1))
}

impl HoverProvider for Asker {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Option<lsp_types::Hover>>> {
        let at = self.place(text, offset);
        let (sender, answer) = oneshot::channel();
        self.servers.hover(&self.path, at.line, at.character, move |hover| {
            let _ = sender.send(hover);
        });
        cx.background_executor().spawn(async move {
            // A server that stopped meanwhile has nothing to say.
            let Some(hover) = answer.await.ok().flatten() else { return Ok(None) };
            let contents = lsp_types::MarkupContent { kind: lsp_types::MarkupKind::Markdown, value: hover.text };
            Ok(Some(lsp_types::Hover {
                contents: lsp_types::HoverContents::Markup(contents),
                range: hover.range.map(|(start, end)| range(start, end)),
            }))
        })
    }
}

impl DefinitionProvider for Asker {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<lsp_types::LocationLink>>> {
        let at = self.place(text, offset);
        let (sender, answer) = oneshot::channel();
        self.servers.definition(&self.path, at.line, at.character, move |found| {
            let _ = sender.send(found);
        });
        cx.background_executor().spawn(async move {
            let found = answer.await.unwrap_or_default();
            let links = found.into_iter().filter_map(|target| {
                let name = range((target.line, target.column), (target.end_line, target.end_column));
                Some(lsp_types::LocationLink {
                    origin_selection_range: None,
                    target_uri: uri_from_path(&target.path).parse().ok()?,
                    target_range: name,
                    target_selection_range: name,
                })
            });
            Ok(links.collect())
        })
    }
}

impl CompletionProvider for Asker {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: lsp_types::CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<lsp_types::CompletionResponse>> {
        let at = self.place(text, offset);
        // The letters of the name typed so far, up to the cursor.
        let typed = match offset.checked_sub(1).and_then(|before| text.word_range(before)) {
            Some(word) if word.start < offset => text.slice(word.start..offset).to_string(),
            _ => String::new(),
        };
        let (sender, answer) = oneshot::channel();
        self.servers.completion(&self.path, at.line, at.character, move |list| {
            let _ = sender.send(list);
        });
        cx.background_executor().spawn(async move {
            let items = match answer.await.unwrap_or(Value::Null) {
                Value::Null => Vec::new(),
                list => match serde_json::from_value(list)? {
                    lsp_types::CompletionResponse::Array(items) => items,
                    lsp_types::CompletionResponse::List(list) => list.items,
                },
            };
            Ok(lsp_types::CompletionResponse::Array(matching(items, &typed)))
        })
    }

    /// Letters of a name, and the marks after which names follow.
    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        new_text.chars().last().is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ':'))
    }
}

/// The suggestions that fit what has been typed, best first. Servers send
/// everything that could stand at the place and leave the choosing to the
/// editor: a name fits when it has the typed letters in their order, and
/// one that starts with them comes before one that only has them inside.
fn matching(mut items: Vec<lsp_types::CompletionItem>, typed: &str) -> Vec<lsp_types::CompletionItem> {
    let typed = typed.to_lowercase();
    let name = |item: &lsp_types::CompletionItem| item.filter_text.as_deref().unwrap_or(&item.label).to_lowercase();
    items.retain(|item| {
        let name = name(item);
        let mut letters = name.chars();
        typed.chars().all(|wanted| letters.any(|letter| letter == wanted))
    });
    // Among equals, the order the server asks for.
    items.sort_by_cached_key(|item| {
        (!name(item).starts_with(&typed), item.sort_text.clone().unwrap_or_else(|| item.label.clone()))
    });
    items
}

/// Lets the editor ask the language servers about the file at `path`. A
/// definition in the same file moves the cursor there; one in another file
/// is asked for with [`EditorEvent::Open`].
fn connect(state: &Entity<EditorState>, servers: &Servers, path: &Path, view: WeakEntity<EditorView>, cx: &mut App) {
    let asker = Rc::new(Asker { servers: servers.clone(), path: path.to_path_buf() });
    let own = path.to_path_buf();
    state.update(cx, |state, _| {
        let lsp = state.lsp_mut();
        lsp.hover_provider = Some(asker.clone());
        lsp.completion_provider = Some(asker.clone());
        lsp.definition_provider = Some(asker);
        lsp.show_document = Some(Rc::new(move |place, _, cx| {
            // Not a file (a web page): the editor opens it in the browser.
            let Some(path) = path_from_uri(place.uri.as_str()) else { return false };
            if path == own {
                return false;
            }
            let at = place.selection.map_or((1, 1), |range| (range.start.line + 1, range.start.character + 1));
            view.update(cx, |_, cx| cx.emit(EditorEvent::Open { path, at })).is_ok()
        }));
    });
}

pub struct EditorView {
    path: PathBuf,
    /// The project the file was opened from; language servers work in it.
    root: PathBuf,
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
    /// The language servers, unless checking is not set up (in some tests).
    servers: Option<Servers>,
    /// The servers have been told the file is open.
    opened: bool,
    /// Sends the text to the servers once typing pauses.
    send_task: Option<Task<()>>,
    /// The file as the last commit has it; `None` outside a repository and
    /// for a file git does not know.
    committed: Option<Arc<String>>,
    /// The marks next to the line numbers: lines added, changed and removed
    /// since that commit.
    marks: GutterDecorationCollection,
    /// Works the marks out again once typing pauses.
    marks_task: Option<Task<()>>,
    /// What the marks stand for: the runs of lines that differ from the commit.
    changes: Vec<linediff::Change>,
    _subscription: Subscription,
    _checks: Option<Subscription>,
    _watch: Task<()>,
}

impl EventEmitter<EditorEvent> for EditorView {}

impl Focusable for EditorView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.problem.is_some() { self.focus.clone() } else { self.state.focus_handle(cx) }
    }
}

impl EditorView {
    pub fn new(path: PathBuf, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let language = document::language(&path);
        let state = cx.new(|cx| EditorState::new(window, cx).language(language));
        let marks = state.update(cx, |state, cx| state.create_gutter_decorations_collection(Vec::new(), cx));
        let subscription = cx.subscribe(&state, |this, _, event: &InputEvent, cx| {
            if !matches!(event, InputEvent::Change) || !this.loaded {
                return;
            }
            this.text_changed(cx);
            this.mark_later(cx);
            if !this.dirty {
                this.dirty = true;
                cx.emit(EditorEvent::Changed);
                cx.notify();
            }
        });
        let checker = crate::lsp::checker(cx);
        let checks = checker.as_ref().map(|checker| {
            cx.subscribe(checker, |this, _, event: &CheckerEvent, cx| {
                let CheckerEvent::Problems(path) = event;
                if *path == this.path {
                    this.show_problems(cx);
                }
            })
        });
        let servers = checker.map(|checker| checker.read(cx).servers().clone());
        if let Some(servers) = &servers {
            connect(&state, servers, &path, cx.weak_entity(), cx);
        }
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
            root,
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
            servers,
            opened: false,
            send_task: None,
            committed: None,
            marks,
            marks_task: None,
            changes: Vec::new(),
            _subscription: subscription,
            _checks: checks,
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
        if let Some(servers) = self.servers.as_ref().filter(|_| self.opened) {
            servers.close(&self.path);
            servers.open(&self.root, &path, self.state.read(cx).value().to_string());
        }
        if let Some(servers) = &self.servers {
            connect(&self.state, servers, &path, cx.weak_entity(), cx);
        }
        self.path = path;
        self.show_problems(cx);
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
                self.read_committed(cx);
                if let Some(servers) = &self.servers {
                    // The editor's own copy: its lines are what the servers count in.
                    let text = self.state.read(cx).value().to_string();
                    if self.opened {
                        servers.change(&self.path, text);
                    } else {
                        servers.open(&self.root, &self.path, text);
                        self.opened = true;
                    }
                }
                self.show_problems(cx);
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

    /// Saves the file: writes the text to disk, after a language server has
    /// laid it out if the settings ask for that. `then` runs once it is there.
    pub fn save(
        &mut self,
        then: impl FnOnce(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.loaded || self.problem.is_some() {
            return;
        }
        let wanted = cx.try_global::<AppState>().is_some_and(|state| state.settings.format_on_save);
        let Some(servers) = self.servers.clone().filter(|_| wanted && self.opened) else {
            return self.write(then, window, cx);
        };
        let before = self.state.read(cx).value().to_string();
        servers.change(&self.path, before.clone());
        let (sender, answer) = oneshot::channel();
        servers.format(&self.path, FORMAT_INDENT, move |edits| {
            let _ = sender.send(edits);
        });
        cx.spawn_in(window, async move |this, cx| {
            // A server that is slow or gone must not keep the file from being saved.
            let late = cx.background_executor().timer(FORMAT_WAIT);
            let edits = futures::select_biased! {
                edits = answer.fuse() => edits.unwrap_or_default(),
                _ = late.fuse() => Vec::new(),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                // The edits are for the text as it was when the server was asked.
                if this.state.read(cx).value().as_ref() == before {
                    this.apply(edits, window, cx);
                }
                this.write(then, window, cx);
            });
        })
        .detach();
    }

    /// Changes the text as a language server said to.
    fn apply(&mut self, mut edits: Vec<Edit>, window: &mut Window, cx: &mut Context<Self>) {
        if edits.is_empty() {
            return;
        }
        // From the end of the file up, so one edit does not move the places of the next.
        edits.sort_by(|a, b| b.start.cmp(&a.start));
        let edits: Vec<lsp_types::TextEdit> =
            edits.into_iter().map(|edit| lsp_types::TextEdit { range: range(edit.start, edit.end), new_text: edit.text }).collect();
        self.state.update(cx, |state, cx| state.apply_lsp_edits(&edits, window, cx));
    }

    /// Writes the text to disk. `then` runs once it is there.
    fn write(
        &mut self,
        then: impl FnOnce(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                        this.send_text(cx);
                        this.read_committed(cx);
                        if let Some(servers) = this.servers.as_ref().filter(|_| this.opened) {
                            servers.saved(&this.path);
                        }
                        // The settings file itself: what was written takes effect now.
                        let settings = cx.try_global::<AppState>().and_then(|state| state.settings_path.clone());
                        if settings.as_deref() == Some(this.path.as_path()) {
                            cx.defer(crate::theme::reload_settings);
                        }
                        then(window, cx);
                    }
                    Err(e) => this.error = Some(tf("Could not save {name}: {error}", &[("name", &this.file_name()), ("error", &e)])),
                }
                cx.emit(EditorEvent::Changed);
                cx.notify();
            });
        })
        .detach();
    }

    /// Git has a new commit, or another branch: the marks are counted from it.
    pub fn git_changed(&mut self, cx: &mut Context<Self>) {
        if self.loaded {
            self.read_committed(cx);
        }
    }

    /// Reads the file as the last commit has it, then marks what differs.
    fn read_committed(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        cx.spawn(async move |this, cx| {
            let committed = cx.background_executor().spawn(async move { git::committed_text(&path) }).await;
            let _ = this.update(cx, |this, cx| {
                this.committed = committed.map(Arc::new);
                this.mark_changes(cx);
            });
        })
        .detach();
    }

    /// Marks the changed lines again once typing pauses.
    fn mark_later(&mut self, cx: &mut Context<Self>) {
        if self.committed.is_none() {
            return;
        }
        self.marks_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TYPING_PAUSE).await;
            let _ = this.update(cx, |this, cx| this.mark_changes(cx));
        }));
    }

    /// Marks the lines that differ from the last commit next to their
    /// numbers: new ones, changed ones, and where lines were taken out.
    fn mark_changes(&mut self, cx: &mut Context<Self>) {
        self.marks_task = None;
        let Some(committed) = self.committed.clone() else {
            self.marks.clear(cx);
            self.changes.clear();
            return;
        };
        let text = self.state.read(cx).text().clone();
        let theme = cx.theme();
        let (added, modified, deleted) = (theme.success, theme.warning, theme.danger);
        let marks = self.marks.clone();
        self.marks_task = Some(cx.spawn(async move |this, cx| {
            let compared = text.clone();
            let changes = cx
                .background_executor()
                .spawn(async move { linediff::changes(&committed, &compared.to_string()) })
                .await;
            // Where a line starts; past the last line, the end of the text.
            let offset = |line: u32| {
                let line = line as usize;
                if line < text.lines_len() { text.line_start_offset(line) } else { text.len() }
            };
            let decorations = changes
                .iter()
                .map(|change| {
                    let color = match change.kind {
                        Kind::Added => added,
                        Kind::Modified => modified,
                        Kind::Deleted => deleted,
                    };
                    GutterDecoration::new(offset(change.start)..offset(change.end)).with_color(color)
                })
                .collect();
            let _ = this.update(cx, |this, cx| {
                this.changes = changes;
                marks.set(decorations, cx);
            });
        }));
    }

    /// Sends the text to the language servers once typing pauses.
    fn text_changed(&mut self, cx: &mut Context<Self>) {
        if self.servers.is_none() || !self.opened {
            return;
        }
        self.send_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TYPING_PAUSE).await;
            let _ = this.update(cx, |this, cx| this.send_text(cx));
        }));
    }

    fn send_text(&mut self, cx: &mut Context<Self>) {
        self.send_task = None;
        let Some(servers) = self.servers.as_ref().filter(|_| self.opened) else { return };
        servers.change(&self.path, self.state.read(cx).value().to_string());
        // The editor drops the underlines with each edit. The last ones stand
        // until the servers answer: one with nothing new to say says nothing.
        self.show_problems(cx);
    }

    /// Underlines what the language servers say about the file.
    fn show_problems(&mut self, cx: &mut Context<Self>) {
        let Some(servers) = &self.servers else { return };
        if !self.loaded {
            return;
        }
        let problems = servers.problems(&self.path);
        self.state.update(cx, |state, cx| {
            let text = state.text().clone();
            if let Some(set) = state.diagnostics_mut() {
                set.reset(&text);
                set.extend(problems.iter().map(diagnostic));
            }
            cx.notify();
        });
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
                .child(div().flex_1().min_w_0().child(t("This file was changed on disk by another program.")))
                .child(Button::new("editor-reload").small().label(t("Reload from Disk")).on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| this.reload(window, cx),
                )))
                .child(Button::new("editor-keep").small().ghost().label(t("Keep My Changes")).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.changed_on_disk = false;
                        cx.notify();
                    },
                )))
                .into_any_element(),
        )
    }
}

impl Drop for EditorView {
    fn drop(&mut self) {
        if let Some(servers) = self.servers.as_ref().filter(|_| self.opened) {
            servers.close(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::matching;

    #[test]
    fn suggestions_follow_what_is_typed() {
        let item = |label: &str, sort: &str| lsp_types::CompletionItem {
            label: label.into(),
            sort_text: Some(sort.into()),
            ..Default::default()
        };
        let all = || vec![item("self::", "a"), item("transform", "b"), item("forecast", "d"), item("For", "c")];
        let labels = |typed: &str| matching(all(), typed).into_iter().map(|i| i.label).collect::<Vec<_>>();
        assert_eq!(labels("for"), ["For", "forecast", "transform"]);
        assert_eq!(labels(""), ["self::", "transform", "For", "forecast"]);
        assert!(labels("xyz").is_empty());
    }
}

/// A problem as the editor shows it: a wavy line, and the message when the
/// pointer rests on it.
fn diagnostic(problem: &Problem) -> Diagnostic {
    let (mut start, mut end) = ((problem.line, problem.column), (problem.end_line, problem.end_column));
    // A problem at a point, such as a missing semicolon, still needs a
    // character to underline: the one before it, or at a line start the next.
    if start == end {
        if start.1 > 0 {
            start.1 -= 1;
        } else {
            end.1 += 1;
        }
    }
    let message = match &problem.code {
        Some(code) => format!("{} ({code})", problem.message),
        None => problem.message.clone(),
    };
    let severity = match problem.severity {
        Severity::Error => DiagnosticSeverity::Error,
        Severity::Warning => DiagnosticSeverity::Warning,
        Severity::Info => DiagnosticSeverity::Info,
        Severity::Hint => DiagnosticSeverity::Hint,
    };
    let diagnostic = Diagnostic::new(Position::new(start.0, start.1)..Position::new(end.0, end.1), message)
        .with_severity(severity);
    match &problem.source {
        Some(source) => diagnostic.with_source(source.clone()),
        None => diagnostic,
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
                            .label(t("Open in the Default App"))
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

    /// The messages of the underlined problems, from the top down.
    pub fn underlined(&self, cx: &App) -> Vec<String> {
        let state = self.state.read(cx);
        state.diagnostics().map(|set| set.iter().map(|d| d.message.to_string()).collect()).unwrap_or_default()
    }

    /// What the language servers say about the code at a 0-based line and
    /// column, as the pointer resting there asks.
    pub fn hover_at(&self, line: u32, column: u32, window: &mut Window, cx: &mut Context<Self>) -> Task<Option<String>> {
        let state = self.state.read(cx);
        let (text, provider) = (state.text().clone(), state.lsp().hover_provider.clone().unwrap());
        let asked = provider.hover(&text, text.position_to_offset(&Position::new(line, column)), window, cx);
        cx.background_executor().spawn(async move {
            match asked.await.unwrap()?.contents {
                lsp_types::HoverContents::Markup(markup) => Some(markup.value),
                _ => None,
            }
        })
    }

    /// The labels a language server offers to complete with at a 0-based
    /// line and column.
    pub fn completions_at(&self, line: u32, column: u32, window: &mut Window, cx: &mut Context<Self>) -> Task<Vec<String>> {
        let state = self.state.read(cx);
        let (text, provider) = (state.text().clone(), state.lsp().completion_provider.clone().unwrap());
        let context = lsp_types::CompletionContext {
            trigger_kind: lsp_types::CompletionTriggerKind::INVOKED,
            trigger_character: None,
        };
        let offset = text.position_to_offset(&Position::new(line, column));
        let asked = provider.completions(&text, offset, context, window, cx);
        cx.background_executor().spawn(async move {
            let lsp_types::CompletionResponse::Array(items) = asked.await.unwrap() else { return Vec::new() };
            items.into_iter().map(|item| item.label).collect()
        })
    }

    /// The runs of lines marked next to the line numbers.
    pub fn marked(&self) -> Vec<(Kind, u32, u32)> {
        self.changes.iter().map(|change| (change.kind, change.start, change.end)).collect()
    }

    /// The line and column of the cursor, 0-based.
    pub fn cursor(&self, cx: &App) -> (u32, u32) {
        let position = self.state.read(cx).cursor_position();
        (position.line, position.character)
    }

    /// Follows a definition the way the editor does once a server has
    /// named the place.
    pub fn follow(view: &Entity<Self>, path: &Path, line: u32, column: u32, window: &mut Window, cx: &mut App) -> bool {
        let show = view.read(cx).state.read(cx).lsp().show_document.clone().unwrap();
        let name = range((line, column), (line, column));
        let place = lsp_types::ShowDocumentParams {
            uri: uri_from_path(path).parse().unwrap(),
            external: Some(false),
            take_focus: Some(true),
            selection: Some(name),
        };
        show(&place, window, cx)
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
