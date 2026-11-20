//! A file open for editing in a tab next to the terminals.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::FutureExt as _;
use futures::channel::oneshot;
use gpui_kit::base::input::{
    CodeActionProvider, CompletionProvider, DefinitionProvider, Diagnostic, DiagnosticSeverity, HoverProvider, Rope,
    RopeExt as _,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Editor, EditorState, GutterDecoration, GutterDecorationCollection, Input, InputEvent, InputState, Position,
    SelectAll,
};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use wercode_core::document::{self, Document, Format, LoadError};
use wercode_core::git;
use wercode_core::i18n::{t, tf};
use wercode_core::linediff::{self, Kind};
use wercode_core::lsp::{Action as ServerAction, Edit, Hint, Problem, Servers, Severity, Symbol, Value, path_from_uri, uri_from_path};

use crate::lsp::CheckerEvent;
use crate::theme::AppState;

actions!(
    editor,
    [
        Save,
        /// Renames the name under the cursor wherever the project uses it.
        RenameSymbol,
        /// Chooses another file to show to the right of this one, in the same tab.
        SplitEditorRight,
        /// Chooses another file to show below this one, in the same tab.
        SplitEditorDown
    ]
);

/// How long the outline of the file and its notes wait after a change.
const OUTLINE_PAUSE: Duration = Duration::from_millis(250);
/// How long a quick fix waits for the servers to say what they offer.
const ACTIONS_WAIT: Duration = Duration::from_secs(5);

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
    view: WeakEntity<EditorView>,
    /// What the servers offered last time they were asked for quick fixes.
    offered: Rc<RefCell<Vec<ServerAction>>>,
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

impl CodeActionProvider for Asker {
    fn id(&self) -> SharedString {
        "language-servers".into()
    }

    fn code_actions(
        &self,
        state: Entity<EditorState>,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<lsp_types::CodeAction>>> {
        let text = state.read(cx).text().clone();
        self.servers.change(&self.path, text.to_string());
        let (start, end) = (text.offset_to_position(range.start), text.offset_to_position(range.end));
        let (sender, answer) = oneshot::channel();
        self.servers.code_actions(&self.path, (start.line, start.character), (end.line, end.character), move |found| {
            let _ = sender.send(found);
        });
        let offered = self.offered.clone();
        cx.spawn(async move |cx| {
            let late = cx.background_executor().timer(ACTIONS_WAIT);
            let mut found = futures::select_biased! {
                found = answer.fuse() => found.unwrap_or_default(),
                _ = late.fuse() => Vec::new(),
            };
            // The fixes the servers think best come first.
            found.sort_by_key(|action| !action.preferred);
            let items = found
                .iter()
                .enumerate()
                .map(|(index, action)| lsp_types::CodeAction {
                    title: action.title.clone(),
                    kind: action.kind.clone().map(lsp_types::CodeActionKind::from),
                    is_preferred: Some(action.preferred),
                    data: Some(serde_json::json!(index)),
                    ..Default::default()
                })
                .collect();
            *offered.borrow_mut() = found;
            Ok(items)
        })
    }

    fn perform_code_action(
        &self,
        _: Entity<EditorState>,
        action: lsp_types::CodeAction,
        _: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        let index = action.data.as_ref().and_then(Value::as_u64).map(|index| index as usize);
        let Some(chosen) = index.and_then(|index| self.offered.borrow().get(index).cloned()) else {
            return Task::ready(Ok(()));
        };
        let (sender, answer) = oneshot::channel();
        self.servers.perform(&chosen, move |result| {
            let _ = sender.send(result);
        });
        let view = self.view.clone();
        cx.spawn(async move |cx| {
            match answer.await {
                Ok(Ok(files)) => cx.update(|cx| crate::lsp::apply_file_edits(files, cx)),
                Ok(Err(error)) => {
                    let _ = view.update(cx, |this, cx| {
                        this.error = Some(error);
                        cx.notify();
                    });
                }
                Err(_) => {}
            }
            Ok(())
        })
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
    let asker = Rc::new(Asker {
        servers: servers.clone(),
        path: path.to_path_buf(),
        view: view.clone(),
        offered: Rc::default(),
    });
    let own = path.to_path_buf();
    state.update(cx, |state, _| {
        let lsp = state.lsp_mut();
        lsp.hover_provider = Some(asker.clone());
        lsp.completion_provider = Some(asker.clone());
        lsp.code_action_providers = vec![asker.clone()];
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
    /// The file is a picture: shown as one, never edited.
    picture_kind: Option<ImageFormat>,
    picture: Option<Arc<Image>>,
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
    /// The line the cursor is on, and who last changed it, once git has said.
    blamed: Option<(u32, Option<git::Blame>)>,
    blame_task: Option<Task<()>>,
    /// The named things in the file, the way they nest, for the path above
    /// the code; and the notes the servers want shown at the ends of lines.
    symbols: Vec<Symbol>,
    hints: Rc<Vec<Hint>>,
    outline_task: Option<Task<()>>,
    /// The field for a new name, while a name is being renamed.
    renaming: Option<Renaming>,
    _cursor: Subscription,
    _subscription: Subscription,
    _checks: Option<Subscription>,
    /// A server started or finished its work: the outline may be ready now.
    _status: Option<Subscription>,
    _watch: Task<()>,
}

/// A rename being typed: the field, where the name starts, and the old name.
struct Renaming {
    input: Entity<InputState>,
    place: (u32, u32),
    old: String,
    error: Option<String>,
    _subscription: Subscription,
}

impl EventEmitter<EditorEvent> for EditorView {}

impl Focusable for EditorView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.problem.is_some() || self.picture_kind.is_some() { self.focus.clone() } else { self.state.focus_handle(cx) }
    }
}

impl EditorView {
    pub fn new(path: PathBuf, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let picture_kind = picture_format(&path);
        let is_picture = picture_kind.is_some();
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
        // The editor says so whenever the cursor moves, among other things.
        let cursor = cx.observe(&state, |this, _, cx| this.cursor_moved(cx));
        let checker = crate::lsp::checker(cx);
        let checks = checker.as_ref().map(|checker| {
            cx.subscribe_in(checker, window, |this, _, event: &CheckerEvent, window, cx| match event {
                CheckerEvent::Problems(path) if *path == this.path => {
                    this.show_problems(cx);
                    // A server that reports is ready to say what is in the file.
                    this.refresh_outline(cx);
                }
                CheckerEvent::Edits(path, edits) if *path == this.path => this.apply(edits.clone(), window, cx),
                _ => {}
            })
        });
        let status = checker.as_ref().filter(|_| !is_picture).map(|checker| cx.observe(checker, |this, _, cx| this.refresh_outline(cx)));
        // A picture has no code for the servers to check.
        let servers = checker.filter(|_| !is_picture).map(|checker| checker.read(cx).servers().clone());
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
            picture_kind,
            picture: None,
            error: None,
            pending_position: None,
            servers,
            opened: false,
            send_task: None,
            committed: None,
            marks,
            marks_task: None,
            changes: Vec::new(),
            blamed: None,
            blame_task: None,
            symbols: Vec::new(),
            hints: Rc::default(),
            outline_task: None,
            renaming: None,
            _cursor: cursor,
            _subscription: subscription,
            _checks: checks,
            _status: status,
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
        if let Some(format) = self.picture_kind {
            cx.spawn_in(window, async move |this, cx| {
                let read = cx.background_executor().spawn(async move { document::load_picture(&path) }).await;
                let _ = this.update(cx, |this, cx| {
                    match read {
                        Ok((bytes, modified)) => {
                            this.picture = Some(Arc::new(Image::from_bytes(format, bytes)));
                            this.modified = modified;
                            this.problem = None;
                        }
                        Err(error) => this.problem = Some(error.message()),
                    }
                    cx.emit(EditorEvent::Changed);
                    cx.notify();
                });
            })
            .detach();
            return;
        }
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
                self.refresh_outline(cx);
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
        if !self.loaded || self.problem.is_some() || self.picture_kind.is_some() {
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
                        // The lines on disk are the lines here again.
                        this.blamed = None;
                        this.cursor_moved(cx);
                        if let Some(servers) = this.servers.as_ref().filter(|_| this.opened) {
                            servers.saved(&this.path);
                        }
                        // The settings file itself: what was written takes effect now.
                        let settings = cx.try_global::<AppState>().and_then(|state| state.settings_path.clone());
                        // A scheme of the user's: its colours show at once too.
                        let scheme = wercode_core::theme::user_dir().is_some_and(|dir| this.path.parent() == Some(dir.as_path()));
                        if settings.as_deref() == Some(this.path.as_path()) || scheme {
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
            self.blamed = None;
            self.cursor_moved(cx);
        }
    }

    /// Who last changed the line the cursor is on, for the status bar.
    /// Nothing while there are unsaved edits: git knows the file on disk,
    /// and its lines may be other lines by now.
    pub fn blame(&self) -> Option<&git::Blame> {
        if self.dirty {
            return None;
        }
        self.blamed.as_ref()?.1.as_ref()
    }

    /// Asks git about the line the cursor is on, once the cursor rests there.
    fn cursor_moved(&mut self, cx: &mut Context<Self>) {
        if !self.loaded || self.problem.is_some() {
            return;
        }
        let line = self.state.read(cx).cursor_position().line;
        if self.blamed.as_ref().is_some_and(|(known, _)| *known == line) {
            return;
        }
        let had = self.blame().is_some();
        self.blamed = Some((line, None));
        if had {
            cx.emit(EditorEvent::Changed);
        }
        let path = self.path.clone();
        self.blame_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TYPING_PAUSE).await;
            let blame = cx.background_executor().spawn(async move { git::blame_line(&path, line) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.blamed.as_ref().is_some_and(|(known, _)| *known == line) && blame.is_some() {
                    this.blamed = Some((line, blame));
                    cx.emit(EditorEvent::Changed);
                }
            });
        }));
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
        self.refresh_outline(cx);
    }

    /// Asks the servers what named things the file has and what notes they
    /// want shown in it.
    fn refresh_outline(&mut self, cx: &mut Context<Self>) {
        let Some(servers) = self.servers.clone().filter(|_| self.opened) else { return };
        let path = self.path.clone();
        let last = (self.state.read(cx).text().lines_len() as u32).saturating_sub(1);
        let hints_wanted = cx.try_global::<AppState>().is_none_or(|state| state.settings.inlay_hints);
        self.outline_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(OUTLINE_PAUSE).await;
            let (sender, symbols) = oneshot::channel();
            servers.symbols(&path, move |found| {
                let _ = sender.send(found);
            });
            let (sender, hints) = oneshot::channel();
            if hints_wanted {
                servers.hints(&path, 0, last, move |found| {
                    let _ = sender.send(found);
                });
            } else {
                let _ = sender.send(Vec::new());
            }
            let (symbols, hints) = (symbols.await.unwrap_or_default(), hints.await.unwrap_or_default());
            let _ = this.update(cx, |this, cx| {
                this.symbols = symbols;
                this.hints = Rc::new(hints);
                cx.notify();
            });
        }));
    }

    /// Opens the field for a new name of the name under the cursor.
    fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.servers.is_none() || !self.loaded || self.problem.is_some() || self.picture_kind.is_some() {
            return;
        }
        let state = self.state.read(cx);
        let text = state.text().clone();
        let offset = text.position_to_offset(&state.cursor_position());
        let word = text
            .word_range(offset)
            .filter(|word| word.start < word.end)
            .or_else(|| offset.checked_sub(1).and_then(|before| text.word_range(before)))
            .filter(|word| word.start < word.end);
        let Some(word) = word else {
            self.error = Some(t("Put the cursor on a name to rename it.").into());
            cx.notify();
            return;
        };
        let old = text.slice(word.clone()).to_string();
        let at = text.offset_to_position(word.start);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(old.clone()));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => this.finish_rename(window, cx),
            InputEvent::Blur => this.cancel_rename(window, cx),
            _ => {}
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        window.dispatch_action(Box::new(SelectAll), cx);
        self.error = None;
        self.renaming = Some(Renaming { input, place: (at.line, at.character), old, error: None, _subscription: subscription });
        cx.notify();
    }

    /// Asks the servers to rename, and makes the changes they send.
    fn finish_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(renaming), Some(servers)) = (&self.renaming, self.servers.clone()) else { return };
        let name = renaming.input.read(cx).value().trim().to_string();
        if name.is_empty() || name == renaming.old {
            return self.cancel_rename(window, cx);
        }
        servers.change(&self.path, self.state.read(cx).value().to_string());
        let (line, column) = renaming.place;
        let (sender, answer) = oneshot::channel();
        servers.rename(&self.path, line, column, &name, move |result| {
            let _ = sender.send(result);
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = answer.await.unwrap_or(Ok(Vec::new()));
            let _ = this.update_in(cx, |this, window, cx| {
                let error = match result {
                    Ok(files) if !files.is_empty() => {
                        this.cancel_rename(window, cx);
                        crate::lsp::apply_file_edits(files, cx);
                        return;
                    }
                    Ok(_) => t("The language server found nothing to rename here.").to_string(),
                    Err(error) => error,
                };
                if let Some(renaming) = &mut this.renaming {
                    renaming.error = Some(error);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming.take().is_some() {
            self.state.update(cx, |state, cx| state.focus(window, cx));
            cx.notify();
        }
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

    /// The path of the file and the named things the cursor is inside,
    /// above the code: `src › main.rs › impl Point › fn len`.
    fn render_crumbs(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.loaded || self.problem.is_some() || self.picture_kind.is_some() {
            return None;
        }
        let theme = cx.theme();
        let (muted, foreground) = (theme.muted_foreground, theme.foreground);
        let relative = self.path.strip_prefix(&self.root).unwrap_or(&self.path);
        let mut parts: Vec<String> = relative.iter().map(|part| part.to_string_lossy().into_owned()).collect();
        if parts.len() > 4 {
            parts.drain(..parts.len() - 4);
            parts.insert(0, "\u{2026}".into());
        }
        let line = self.state.read(cx).cursor_position().line;
        let chain: Vec<String> = symbol_chain(&self.symbols, line).into_iter().map(|symbol| symbol.name.clone()).collect();
        let separator = || div().flex_none().text_color(muted).child("\u{203A}");
        let mut row = h_flex().id("editor-crumbs").flex_none().w_full().h(px(22.)).px_3().gap_1().text_xs().overflow_hidden();
        for (index, part) in parts.into_iter().enumerate() {
            if index > 0 {
                row = row.child(separator());
            }
            row = row.child(div().flex_none().text_color(muted).child(part));
        }
        if !chain.is_empty() {
            let symbols = h_flex()
                .id("editor-crumbs-symbols")
                .gap_1()
                .min_w_0()
                .cursor_pointer()
                .hover(|style| style.text_color(foreground))
                .children(chain.into_iter().enumerate().flat_map(|(index, name)| {
                    let mark = (index > 0).then(|| separator().into_any_element());
                    mark.into_iter().chain(std::iter::once(div().flex_none().truncate().child(name).into_any_element()))
                }))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::symbols::GoToSymbol), cx));
            row = row.child(separator()).child(symbols);
        }
        Some(row.into_any_element())
    }

    /// The field for a new name, while renaming.
    fn render_rename(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let renaming = self.renaming.as_ref()?;
        let theme = cx.theme();
        Some(
            h_flex()
                .id("editor-rename")
                .flex_none()
                .w_full()
                .px_3()
                .py_1()
                .gap_2()
                .text_sm()
                .border_b_1()
                .border_color(theme.border)
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.cancel_rename(window, cx);
                    }
                }))
                .child(div().flex_none().child(tf("Rename {name} to", &[("name", &renaming.old)])))
                .child(div().w(px(260.)).child(Input::new(&renaming.input).small()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(if renaming.error.is_some() { theme.danger } else { theme.muted_foreground })
                        .child(renaming.error.clone().unwrap_or_else(|| {
                            t("Enter renames it in every file of the project; Esc cancels.").to_string()
                        })),
                )
                .into_any_element(),
        )
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

/// The named things that hold `line`, from the outermost in.
fn symbol_chain(symbols: &[Symbol], line: u32) -> Vec<&Symbol> {
    let mut chain = Vec::new();
    let mut level = symbols;
    while let Some(inside) = level.iter().find(|s| s.start.0 <= line && line <= s.end.0) {
        chain.push(inside);
        level = &inside.children;
    }
    chain
}

/// The servers' notes, in grey after the ends of their lines: the editor
/// cannot put text inside a line, so they stand where nothing is in the way.
/// Parameter names are left out; at the end of a line they say nothing.
fn hints_layer(state: Entity<EditorState>, hints: Rc<Vec<Hint>>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let (family, size, color) = (theme.mono_font_family.clone(), theme.mono_font_size, theme.muted_foreground.opacity(0.75));
    canvas(
        |_, _, _| (),
        move |_, _, window, cx| {
            if hints.is_empty() {
                return;
            }
            let (rows, text, text_bounds, visible, line_height) = {
                let state = state.read(cx);
                (state.visible_row_range(), state.text().clone(), state.text_bounds(), state.input_bounds(), state.line_height())
            };
            let (Some(rows), Some(text_bounds), Some(line_height)) = (rows, text_bounds, line_height) else { return };
            if rows.is_empty() {
                return;
            }
            let (first, last) = {
                let map = state.read(cx).display_map();
                (map.display_row_to_buffer_line(rows.start) as u32, map.display_row_to_buffer_line(rows.end - 1) as u32)
            };
            let mut by_line: BTreeMap<u32, Vec<&Hint>> = BTreeMap::new();
            for hint in hints.iter().filter(|h| h.kind != Some(2) && h.line >= first && h.line <= last && !h.label.is_empty()) {
                by_line.entry(hint.line).or_default().push(hint);
            }
            window.with_content_mask(Some(ContentMask { bounds: visible }), |window| {
                for (line, mut list) in by_line {
                    if line as usize >= text.lines_len() || state.read(cx).display_map().is_buffer_line_hidden(line as usize) {
                        continue;
                    }
                    list.sort_by_key(|hint| hint.column);
                    let end = text.offset_to_offset_utf16(text.line_end_offset(line as usize));
                    let at = state.update(cx, |state, cx| state.bounds_for_range(end..end, text_bounds, window, cx));
                    let Some(at) = at else { continue };
                    let label: String = list.iter().map(|hint| hint.label.as_str()).collect::<Vec<_>>().join("  ");
                    let run = TextRun {
                        len: label.len(),
                        font: font(family.clone()),
                        color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let shaped = window.text_system().shape_line(label.into(), size, &[run], None);
                    let origin = point(at.origin.x + size, at.origin.y);
                    let _ = shaped.paint(origin, line_height, TextAlign::Left, None, window, cx);
                }
            });
        },
    )
    .absolute()
    .inset_0()
}

/// How GPUI reads a picture file, for the files a tab shows as pictures.
fn picture_format(path: &Path) -> Option<ImageFormat> {
    Some(match document::picture_kind(path)? {
        "png" => ImageFormat::Png,
        "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "bmp" => ImageFormat::Bmp,
        "ico" => ImageFormat::Ico,
        _ => ImageFormat::Tiff,
    })
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
            None if self.picture_kind.is_some() => div()
                .id("editor-picture")
                .track_focus(&self.focus)
                .size_full()
                .p_4()
                .flex()
                .items_center()
                .justify_center()
                .children(
                    self.picture
                        .clone()
                        .map(|picture| img(picture).max_w_full().max_h_full().object_fit(ObjectFit::ScaleDown)),
                )
                .into_any_element(),
            None => div()
                .relative()
                .size_full()
                .child(Editor::new(&self.state).bordered(false).h(relative(1.)))
                .child(hints_layer(self.state.clone(), self.hints.clone(), cx))
                .into_any_element(),
        };
        v_flex()
            .id("editor-view")
            .key_context("EditorView")
            .size_full()
            .bg(theme.background)
            .on_action(cx.listener(|this, _: &Save, window, cx| this.save(|_, _| {}, window, cx)))
            .on_action(cx.listener(|this, _: &RenameSymbol, window, cx| this.start_rename(window, cx)))
            .children(self.render_crumbs(cx))
            .children(self.render_rename(cx))
            .children(self.render_bar(cx))
            .child(div().flex_1().min_h_0().w_full().child(body))
    }
}

#[cfg(test)]
impl EditorView {
    pub fn text(&self, cx: &App) -> String {
        self.state.read(cx).value().to_string()
    }

    pub fn has_picture(&self) -> bool {
        self.picture.is_some()
    }

    /// The named things the cursor is inside, from the outermost in.
    pub fn crumbs(&self, cx: &App) -> Vec<String> {
        let line = self.state.read(cx).cursor_position().line;
        symbol_chain(&self.symbols, line).iter().map(|symbol| symbol.name.clone()).collect()
    }

    pub fn hint_labels(&self) -> Vec<String> {
        self.hints.iter().map(|hint| hint.label.clone()).collect()
    }

    pub fn is_renaming(&self) -> bool {
        self.renaming.is_some()
    }

    /// Takes the quick fix with this title at the cursor, the way choosing
    /// it in the menu that Ctrl+. opens does. True once it is done.
    pub fn fix(&self, title: &str, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        let state = self.state.clone();
        let provider = state.read(cx).lsp().code_action_providers[0].clone();
        let offset = {
            let state = state.read(cx);
            state.text().position_to_offset(&state.cursor_position())
        };
        let asked = provider.code_actions(state.clone(), offset..offset, window, cx);
        let title = title.to_string();
        cx.spawn_in(window, async move |_, cx| {
            let actions = asked.await.unwrap_or_default();
            let Some(action) = actions.into_iter().find(|action| action.title == title) else { return false };
            let Ok(done) = cx.update(|window, cx| provider.perform_code_action(state, action, true, window, cx)) else {
                return false;
            };
            done.await.is_ok()
        })
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
