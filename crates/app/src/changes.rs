//! Every change in the repository in one tab, file after file. Each piece
//! has buttons to put it in the index or throw it away (or, among staged
//! changes, to take it back out), so what an agent changed can be read and
//! sorted in one place.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{
    Diff, DiffAnnotation, DiffConflictResolution, DiffEvent, DiffFile, DiffFileStatus, DiffLinePosition, DiffSide, DiffState,
};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wercode_core::changes::{self, Changes, FilePatch, Keep, Line, Side, Undo};
use wercode_core::i18n::{t, tf, tn};
use wercode_core::{document, git};

pub enum ChangesEvent {
    /// A button changed the repository; its status should be read again.
    Changed,
    /// A file was asked for, at a line of it.
    Open { path: PathBuf, line: Option<u32> },
}

/// Where the list scrolls once the changes are read again.
#[derive(Clone, Debug)]
struct Landing {
    path: PathBuf,
    /// The file's place in the list, for when it is gone.
    index: usize,
    /// The hunk reaching past this line; `None` for the file's header.
    line: Option<Line>,
    /// The last line of that hunk goes to the top instead of its first,
    /// when the hunk is still there.
    at_end: bool,
    /// A file with merge conflicts; `index` counts among those.
    conflict: bool,
}

/// Something the list drew in the last frame, top first.
#[derive(Clone, Copy, Debug)]
enum Seen {
    Header(usize),
    Footer(usize, usize),
    Conflict(usize),
}

/// What a button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Stage,
    Unstage,
    Discard,
}

/// A file with merge conflicts, as the list shows it.
struct Conflict {
    path: PathBuf,
    name: SharedString,
    /// The text the list was made from.
    text: String,
    /// Conflicts left in it.
    left: usize,
}

/// The files as the list shows them: those with merge conflicts first.
#[derive(Default)]
struct Shown {
    conflicts: Vec<Conflict>,
    conflict_by_name: HashMap<SharedString, usize>,
    patches: Vec<FilePatch>,
    /// The list's name for each file, in the same order.
    names: Vec<SharedString>,
    by_name: HashMap<SharedString, usize>,
    deleted: Vec<bool>,
}

pub struct ChangesView {
    root: PathBuf,
    side: Side,
    state: Entity<DiffState>,
    /// The last read, to tell whether anything changed.
    read: Option<Arc<Changes>>,
    shown: Rc<Shown>,
    /// Files, lines added, lines removed.
    totals: (usize, usize, usize),
    /// Files git listed that the list could not show.
    skipped: usize,
    /// Files with merge conflicts the list could not show.
    unshown_conflicts: usize,
    /// What to say instead of the list: reading, nothing to show, or an error.
    note: Option<String>,
    /// What went wrong with the last button.
    error: Option<String>,
    busy: bool,
    /// What the last discard took away.
    undo: Option<Undo>,
    land: Option<Landing>,
    seen: Rc<RefCell<Vec<Seen>>>,
    reading: bool,
    /// Another read was asked for while one was running.
    again: bool,
    task: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<ChangesEvent> for ChangesView {}

impl Focusable for ChangesView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.focus_handle(cx)
    }
}

fn position(name: &SharedString, line: Line) -> DiffLinePosition {
    let side = if line.new { DiffSide::Modified } else { DiffSide::Original };
    DiffLinePosition::new(name.clone(), side, line.number as usize)
}

/// One id for the buttons of a hunk.
fn key(file: usize, hunk: usize) -> usize {
    file * 100_000 + hunk
}

impl ChangesView {
    pub fn new(root: PathBuf, side: Side, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| DiffState::new([], cx));
        let subscription = cx.subscribe(&state, |this, state, event: &DiffEvent, cx| {
            if let DiffEvent::ConflictResolved(name, ix) = event {
                let chosen = state.read(cx).conflict_resolution(name, *ix);
                this.resolve(name, *ix, chosen, cx);
            }
        });
        let mut this = Self {
            root,
            side,
            state,
            read: None,
            shown: Rc::default(),
            totals: (0, 0, 0),
            skipped: 0,
            unshown_conflicts: 0,
            note: Some(t("Reading changes\u{2026}").into()),
            error: None,
            busy: false,
            undo: None,
            land: None,
            seen: Rc::default(),
            reading: false,
            again: false,
            task: None,
            _subscription: subscription,
        };
        this.reload(cx);
        this
    }

    pub fn side(&self) -> Side {
        self.side
    }

    /// Shows the other side of the index.
    pub fn set_side(&mut self, side: Side, cx: &mut Context<Self>) {
        if self.side == side {
            return;
        }
        self.side = side;
        self.read = None;
        self.undo = None;
        self.error = None;
        self.seen.borrow_mut().clear();
        self.reload(cx);
        cx.notify();
    }

    /// Scrolls to a file, now or once the list has been read.
    pub fn reveal(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let conflict = self.shown.conflicts.iter().position(|c| c.path == path);
        let file = self.shown.patches.iter().position(|p| p.path == path);
        let landing =
            Landing { path, index: conflict.or(file).unwrap_or(0), line: None, at_end: false, conflict: conflict.is_some() };
        if conflict.is_none() && file.is_none() || self.reading {
            self.land = Some(landing);
            return;
        }
        let shown = self.shown.clone();
        self.state.update(cx, |state, cx| land(&shown, &landing, state, cx));
    }

    /// Asks git for the changes again. The list is replaced only when they
    /// differ, so it does not jump while nothing happens.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.reading {
            self.again = true;
            return;
        }
        self.reading = true;
        let (root, side) = (self.root.clone(), self.side);
        self.task = Some(cx.spawn(async move |this, cx| {
            let read = cx.background_executor().spawn(async move { changes::read(&root, side) }).await;
            let _ = this.update(cx, |this, cx| {
                this.reading = false;
                let stale = this.side != side;
                if !stale {
                    this.show(read, cx);
                }
                if std::mem::take(&mut this.again) || stale {
                    this.reload(cx);
                }
            });
        }));
    }

    fn show(&mut self, read: Result<Changes, String>, cx: &mut Context<Self>) {
        let changes = match read {
            Ok(changes) => changes,
            Err(error) => {
                self.note = Some(tf("Could not read the changes: {error}", &[("error", &error)]));
                self.read = None;
                self.shown = Rc::default();
                self.state.update(cx, |state, cx| state.set_files([], cx));
                cx.notify();
                return;
            }
        };
        if self.read.as_deref() == Some(&changes) {
            self.land = None;
            return;
        }
        let landing = self.land.take().or_else(|| self.anchor());
        let mut shown = Shown::default();
        let mut files = Vec::new();
        let mut skipped = 0;
        self.unshown_conflicts = 0;
        for conflicted in &changes.conflicts {
            let name: SharedString = conflicted
                .path
                .strip_prefix(&changes.top)
                .unwrap_or(&conflicted.path)
                .to_string_lossy()
                .replace('\\', "/")
                .into();
            let parsed = conflicted.text.as_deref().and_then(|text| DiffFile::parse_conflicts(name.clone(), text).ok());
            let (Some(file), Some(text)) = (parsed, conflicted.text.clone()) else {
                self.unshown_conflicts += 1;
                continue;
            };
            shown.conflict_by_name.insert(name.clone(), shown.conflicts.len());
            shown.conflicts.push(Conflict { path: conflicted.path.clone(), name, text, left: file.conflicts().len() });
            files.push(file.with_language(document::language(&conflicted.path)));
        }
        for patch in &changes.files {
            let text = String::from_utf8_lossy(&patch.text()).into_owned();
            let Some(file) = DiffFile::parse(&text).ok().filter(|f| f.len() == 1).and_then(|mut f| f.pop()) else {
                skipped += 1;
                continue;
            };
            shown.by_name.insert(file.path().clone(), shown.patches.len());
            shown.names.push(file.path().clone());
            shown.deleted.push(file.status() == DiffFileStatus::Deleted);
            shown.patches.push(patch.clone());
            files.push(file.with_language(document::language(&patch.path)));
        }
        self.totals = (
            shown.patches.len(),
            shown.patches.iter().map(FilePatch::additions).sum(),
            shown.patches.iter().map(FilePatch::deletions).sum(),
        );
        self.skipped = skipped;
        self.note = (shown.patches.is_empty() && shown.conflicts.is_empty()).then(|| match self.side {
            Side::Unstaged => t("Nothing has changed that is not staged.").to_string(),
            Side::Staged => t("Nothing is staged.").to_string(),
        });
        self.shown = Rc::new(shown);
        self.read = Some(Arc::new(changes));
        self.seen.borrow_mut().clear();
        let shown = self.shown.clone();
        self.state.update(cx, |state, cx| {
            state.set_files(files, cx);
            if let Some(landing) = landing {
                land(&shown, &landing, state, cx);
            }
        });
        cx.notify();
    }

    /// Where the reader is: the first thing the list drew last time.
    fn anchor(&self) -> Option<Landing> {
        let first = *self.seen.borrow().first()?;
        Some(match first {
            Seen::Header(file) => Landing {
                path: self.shown.patches.get(file)?.path.clone(),
                index: file,
                line: None,
                at_end: false,
                conflict: false,
            },
            Seen::Footer(file, hunk) => {
                let patch = self.shown.patches.get(file)?;
                let last = patch.hunks.get(hunk)?.last;
                Landing { path: patch.path.clone(), index: file, line: Some(last), at_end: true, conflict: false }
            }
            Seen::Conflict(file) => Landing {
                path: self.shown.conflicts.get(file)?.path.clone(),
                index: file,
                line: None,
                at_end: false,
                conflict: true,
            },
        })
    }

    fn top(&self) -> Option<PathBuf> {
        self.read.as_ref().map(|read| read.top.clone())
    }

    /// Runs a git command in the background, then reads the changes again
    /// and scrolls to `landing`. A discard's result can be undone.
    fn run(
        &mut self,
        landing: Option<Landing>,
        job: impl FnOnce(&Path) -> Result<Option<Undo>, String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(top) = self.top().filter(|_| !self.busy) else { return };
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job(&top) }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(undo) => this.undo = undo,
                    Err(error) => this.error = Some(error),
                }
                this.land = landing;
                this.reload(cx);
                cx.emit(ChangesEvent::Changed);
                cx.notify();
            });
        })
        .detach();
    }

    fn hunk(&mut self, file: usize, hunk: usize, act: Act, cx: &mut Context<Self>) {
        let Some(patch) = self.shown.patches.get(file).cloned() else { return };
        let Some(line) = patch.hunks.get(hunk).map(|h| h.first) else { return };
        let landing = Landing { path: patch.path.clone(), index: file, line: Some(line), at_end: false, conflict: false };
        self.run(
            Some(landing),
            move |top| match act {
                Act::Stage => changes::stage_hunk(top, &patch, hunk).map(|()| None),
                Act::Unstage => changes::unstage_hunk(top, &patch, hunk).map(|()| None),
                Act::Discard => changes::discard_hunk(top, &patch, hunk),
            },
            cx,
        );
    }

    fn file(&mut self, file: usize, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        let Some(patch) = self.shown.patches.get(file).cloned() else { return };
        let landing = Landing { path: patch.path.clone(), index: file, line: None, at_end: false, conflict: false };
        // Without lines to put back (a binary file), a discard cannot be undone: ask first.
        if act == Act::Discard && !patch.untracked && patch.hunks.is_empty() {
            let name = patch.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let answer = window.prompt(
                PromptLevel::Warning,
                &tf("Discard changes in {name}?", &[("name", &name)]),
                Some(t("The file goes back to its last staged or committed text. This cannot be undone.")),
                &[t("Discard"), t("Cancel")],
                cx,
            );
            cx.spawn(async move |this, cx| {
                if answer.await == Ok(0) {
                    let _ = this.update(cx, |this, cx| {
                        this.run(Some(landing), move |top| changes::discard_file(top, &patch), cx);
                    });
                }
            })
            .detach();
            return;
        }
        let path = patch.path.clone();
        self.run(
            Some(landing),
            move |top| match act {
                Act::Stage => git::stage(top, &[path]).map(|()| None),
                Act::Unstage => git::unstage(top, &[path]).map(|()| None),
                Act::Discard => changes::discard_file(top, &patch),
            },
            cx,
        );
    }

    fn all(&mut self, cx: &mut Context<Self>) {
        let side = self.side;
        self.run(
            None,
            move |top| match side {
                Side::Unstaged => git::stage(top, &[]).map(|()| None),
                Side::Staged => git::unstage(top, &[]).map(|()| None),
            },
            cx,
        );
    }

    /// Keeps the side of a merge conflict its buttons chose, in the file
    /// itself; the last one puts the file into the index.
    fn resolve(&mut self, name: &SharedString, ix: usize, chosen: Option<DiffConflictResolution>, cx: &mut Context<Self>) {
        let Some(&file) = self.shown.conflict_by_name.get(name) else { return };
        let keep = match chosen {
            Some(DiffConflictResolution::Current) => Keep::Ours,
            Some(DiffConflictResolution::Incoming) => Keep::Theirs,
            Some(_) => Keep::Both,
            None => return,
        };
        if self.busy {
            // One change to the file at a time: this choice waits for the list to be read again.
            self.state.update(cx, |state, cx| state.resolve_conflict(name, ix, None, cx));
            return;
        }
        let conflict = &self.shown.conflicts[file];
        let (path, text) = (conflict.path.clone(), conflict.text.clone());
        let landing = Landing { path: path.clone(), index: file, line: None, at_end: false, conflict: true };
        self.run(Some(landing), move |top| changes::resolve_conflict(top, &path, &text, ix, keep), cx);
    }

    /// Puts a file whose conflict markers are all gone into the index.
    fn mark_resolved(&mut self, file: usize, cx: &mut Context<Self>) {
        let Some(conflict) = self.shown.conflicts.get(file) else { return };
        let path = conflict.path.clone();
        let landing = Landing { path: path.clone(), index: file, line: None, at_end: false, conflict: true };
        self.run(Some(landing), move |top| git::stage(top, &[path]).map(|()| None), cx);
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        let Some(undo) = self.undo.clone() else { return };
        self.run(None, move |top| changes::undo(top, &undo).map(|()| None), cx);
    }

    /// The file a click on a line asks for, at that line.
    fn open_line(&mut self, at: &DiffLinePosition, cx: &mut Context<Self>) {
        let Some(&file) = self.shown.by_name.get(at.path()) else { return };
        let patch = &self.shown.patches[file];
        let number = at.line() as u32;
        // A removed line is not in the file any more: go to where it was.
        let line = match at.side() {
            DiffSide::Modified => Some(number),
            DiffSide::Original => patch
                .hunks
                .iter()
                .find(|h| h.ends_after(Line { new: false, number }))
                .map(|h| h.new_start.max(1)),
        };
        cx.emit(ChangesEvent::Open { path: patch.path.clone(), line });
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (muted, added, removed) = (theme.muted_foreground, theme.success, theme.danger);
        let side = self.side;
        let tab = |id: &'static str, label: &'static str, which: Side| {
            Button::new(id).ghost().small().label(label).selected(side == which).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| this.set_side(which, cx),
            ))
        };
        let (files, plus, minus) = self.totals;
        let conflicts = self.shown.conflicts.len();
        let danger = theme.danger;
        let busy = self.busy;
        h_flex()
            .flex_none()
            .h(px(36.))
            .px_2()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_1()
                    .child(tab("changes-unstaged", t("Not Staged"), Side::Unstaged))
                    .child(tab("changes-staged", t("Staged"), Side::Staged)),
            )
            .when(files > 0, |d| {
                d.child(div().text_sm().text_color(muted).child(tn(files as u64, "{n} changed file", "{n} changed files", &[])))
                    .child(div().text_sm().text_color(added).child(format!("+{plus}")))
                    .child(div().text_sm().text_color(removed).child(format!("\u{2212}{minus}")))
            })
            .when(conflicts > 0, |d| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(danger)
                        .child(tn(conflicts as u64, "{n} file with conflicts", "{n} files with conflicts", &[])),
                )
            })
            .child(div().flex_1())
            .when(self.undo.is_some(), |d| {
                d.child(
                    Button::new("changes-undo")
                        .ghost()
                        .small()
                        .icon(IconName::Undo)
                        .label(t("Undo"))
                        .tooltip(t("Take back the last change made here"))
                        .disabled(busy)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.undo(cx))),
                )
            })
            .when(files > 0, |d| {
                let (label, icon) = match side {
                    Side::Unstaged => (t("Stage all"), IconName::Plus),
                    Side::Staged => (t("Unstage all"), IconName::Minus),
                };
                d.child(
                    Button::new("changes-all")
                        .ghost()
                        .small()
                        .icon(icon)
                        .label(label)
                        .disabled(busy)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.all(cx))),
                )
            })
    }

    /// Notes under the toolbar: what went wrong, and what is left out.
    fn render_notes(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let theme = cx.theme();
        let read = self.read.as_deref();
        let conflicts = self.unshown_conflicts;
        let hidden = read.map_or(0, |r| r.hidden);
        let mut notes: Vec<(String, bool)> = Vec::new();
        if let Some(error) = &self.error {
            notes.push((error.clone(), true));
        }
        if conflicts > 0 {
            notes.push((
                tn(
                    conflicts as u64,
                    "{n} file with merge conflicts cannot be shown here; resolve it in the editor.",
                    "{n} files with merge conflicts cannot be shown here; resolve them in the editor.",
                    &[],
                ),
                false,
            ));
        }
        if hidden > 0 {
            notes.push((tn(hidden as u64, "{n} more new file is not shown.", "{n} more new files are not shown.", &[]), false));
        }
        if self.skipped > 0 {
            notes.push((tn(self.skipped as u64, "{n} file could not be shown.", "{n} files could not be shown.", &[]), false));
        }
        if notes.is_empty() {
            return None;
        }
        Some(
            v_flex().id("changes-notes").flex_none().px_3().py_1().gap_0p5().border_b_1().border_color(theme.border).children(
                notes.into_iter().map(|(text, bad)| {
                    div().text_xs().text_color(if bad { theme.danger } else { theme.muted_foreground }).child(text)
                }),
            ),
        )
    }

    fn render_list(&self, cx: &mut Context<Self>) -> Diff {
        let this = cx.entity().downgrade();
        let (shown, seen, side, busy) = (self.shown.clone(), self.seen.clone(), self.side, self.busy);
        let mut annotations = Vec::new();
        for (file, patch) in shown.patches.iter().enumerate() {
            for (ix, hunk) in patch.hunks.iter().enumerate() {
                annotations.push(DiffAnnotation::line(("hunk", key(file, ix)), position(&shown.names[file], hunk.last)));
            }
        }
        let header = {
            let (this, shown, seen) = (this.clone(), shown.clone(), seen.clone());
            move |file: &DiffFile, _: &mut Window, cx: &mut App| {
                if let Some(&ix) = shown.conflict_by_name.get(file.path()) {
                    seen.borrow_mut().push(Seen::Conflict(ix));
                    return conflict_header(&this, &shown.conflicts[ix], ix, busy, cx);
                }
                let Some(&ix) = shown.by_name.get(file.path()) else { return h_flex() };
                seen.borrow_mut().push(Seen::Header(ix));
                let patch = &shown.patches[ix];
                let muted = cx.theme().muted_foreground;
                let button = |id: &'static str, icon: IconName, tip: &'static str, act: Option<Act>| {
                    let this = this.clone();
                    Button::new((id, ix)).ghost().xsmall().icon(icon).tooltip(tip).disabled(busy && act.is_some()).on_click(
                        move |_: &ClickEvent, window, cx| {
                            let _ = this.update(cx, |this, cx| match act {
                                Some(act) => this.file(ix, act, window, cx),
                                None => {
                                    let patch = &this.shown.patches[ix];
                                    let line = patch.hunks.first().map(|h| h.new_start.max(1));
                                    cx.emit(ChangesEvent::Open { path: patch.path.clone(), line });
                                }
                            });
                        },
                    )
                };
                h_flex()
                    .gap_0p5()
                    .when(patch.too_large, |d| d.child(div().text_xs().text_color(muted).child(t("Too large to show"))))
                    .when(!shown.deleted[ix], |d| d.child(button("changes-open", IconName::ExternalLink, t("Open File"), None)))
                    .map(|d| match side {
                        Side::Unstaged => d
                            .child(button("changes-discard-file", IconName::Undo2, t("Discard changes"), Some(Act::Discard)))
                            .child(button("changes-stage-file", IconName::Plus, t("Stage"), Some(Act::Stage))),
                        Side::Staged => d.child(button("changes-unstage-file", IconName::Minus, t("Unstage"), Some(Act::Unstage))),
                    })
            }
        };
        let footer = {
            let (this, shown, seen) = (this.clone(), shown.clone(), seen.clone());
            let ids: HashMap<ElementId, (usize, usize)> = shown
                .patches
                .iter()
                .enumerate()
                .flat_map(|(file, patch)| (0..patch.hunks.len()).map(move |ix| (ElementId::from(("hunk", key(file, ix))), (file, ix))))
                .collect();
            move |annotation: &DiffAnnotation, _: &mut Window, _: &mut App| {
                let Some(&(file, ix)) = ids.get(annotation.id()) else { return h_flex() };
                seen.borrow_mut().push(Seen::Footer(file, ix));
                let new_file = shown.patches[file].untracked;
                let button = |id: &'static str, icon: IconName, label: &'static str, act: Act| {
                    let this = this.clone();
                    Button::new((id, key(file, ix))).ghost().xsmall().icon(icon).label(label).disabled(busy).on_click(
                        move |_: &ClickEvent, _, cx| {
                            let _ = this.update(cx, |this, cx| this.hunk(file, ix, act, cx));
                        },
                    )
                };
                h_flex().gap_1().map(|d| match side {
                    Side::Unstaged => d
                        .child(button("changes-stage", IconName::Plus, t("Stage"), Act::Stage))
                        .child(button(
                            "changes-discard",
                            IconName::Undo2,
                            if new_file { t("Move to Trash") } else { t("Discard") },
                            Act::Discard,
                        )),
                    Side::Staged => d.child(button("changes-unstage", IconName::Minus, t("Unstage"), Act::Unstage)),
                })
            }
        };
        let opened = this.clone();
        Diff::new(&self.state)
            .size_full()
            .annotations(annotations)
            .render_annotation(footer)
            .render_header_suffix(header)
            .on_line_click(move |at: &DiffLinePosition, event: &ClickEvent, _, cx: &mut App| {
                // A double click opens the file there; one click is for selecting.
                if event.click_count() >= 2 {
                    let _ = opened.update(cx, |this, cx| this.open_line(at, cx));
                }
            })
    }
}

/// The buttons on the header of a file with merge conflicts.
fn conflict_header(this: &WeakEntity<ChangesView>, conflict: &Conflict, ix: usize, busy: bool, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    let (open, mark) = (this.clone(), this.clone());
    h_flex()
        .gap_0p5()
        .child(div().text_xs().text_color(muted).child(if conflict.left == 0 {
            t("No conflict markers are left").to_string()
        } else {
            tn(conflict.left as u64, "{n} conflict left", "{n} conflicts left", &[])
        }))
        .child(Button::new(("changes-open-conflict", ix)).ghost().xsmall().icon(IconName::ExternalLink).tooltip(t("Open File")).on_click(
            move |_: &ClickEvent, _, cx| {
                let _ = open.update(cx, |this, cx| {
                    let conflict = &this.shown.conflicts[ix];
                    let line = conflict.text.lines().position(|l| l.starts_with("<<<<<<<")).map(|n| n as u32 + 1);
                    cx.emit(ChangesEvent::Open { path: conflict.path.clone(), line });
                });
            },
        ))
        .when(conflict.left == 0, |d| {
            d.child(
                Button::new(("changes-mark-resolved", ix))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .label(t("Mark Resolved"))
                    .tooltip(t("Put the file into the index, which tells git the conflict is over"))
                    .disabled(busy)
                    .on_click(move |_: &ClickEvent, _, cx| {
                        let _ = mark.update(cx, |this, cx| this.mark_resolved(ix, cx));
                    }),
            )
        })
}

/// Scrolls the list to where `landing` says, in the files as they are now.
fn land(shown: &Shown, landing: &Landing, state: &mut DiffState, cx: &mut Context<DiffState>) {
    if let Some(file) = shown.conflicts.iter().position(|c| c.path == landing.path) {
        state.scroll_to_file(&shown.conflicts[file].name, cx);
        return;
    }
    if landing.conflict {
        // Resolved: the next file with conflicts, or the first other file.
        match shown.conflicts.get(landing.index.min(shown.conflicts.len().saturating_sub(1))) {
            Some(next) => state.scroll_to_file(&next.name, cx),
            None => {
                if let Some(first) = shown.names.first() {
                    state.scroll_to_file(first, cx);
                }
            }
        }
        return;
    }
    if shown.patches.is_empty() {
        return;
    }
    let Some(file) = shown.patches.iter().position(|p| p.path == landing.path) else {
        // The file is gone: the one that took its place.
        state.scroll_to_file(&shown.names[landing.index.min(shown.names.len() - 1)], cx);
        return;
    };
    let (name, patch) = (&shown.names[file], &shown.patches[file]);
    let Some(line) = landing.line else {
        state.scroll_to_file(name, cx);
        return;
    };
    match patch.hunks.iter().find(|h| h.ends_after(line)).or(patch.hunks.last()) {
        Some(hunk) => {
            let begun = if line.new { hunk.new_start <= line.number } else { hunk.old_start <= line.number };
            let target = if landing.at_end && begun { hunk.last } else { hunk.first };
            state.scroll_to_line(position(name, target), cx);
        }
        None => state.scroll_to_file(name, cx),
    }
}

impl Render for ChangesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The list fills this in again as it draws.
        self.seen.borrow_mut().clear();
        let theme = cx.theme();
        let (background, muted) = (theme.background, theme.muted_foreground);
        let body = match &self.note {
            Some(note) => v_flex()
                .id("changes-note")
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child(note.clone())
                .into_any_element(),
            None => div().flex_1().min_h_0().child(self.render_list(cx)).into_any_element(),
        };
        v_flex()
            .id("changes-view")
            .size_full()
            .bg(background)
            .child(self.render_toolbar(cx))
            .children(self.render_notes(cx))
            .child(body)
    }
}

#[cfg(test)]
impl ChangesView {
    /// The files shown, by name, once they have been read.
    pub fn names(&self) -> Vec<String> {
        self.shown.names.iter().map(|n| n.to_string()).collect()
    }

    /// Hunks of each file shown.
    pub fn hunks(&self) -> Vec<usize> {
        self.shown.patches.iter().map(|p| p.hunks.len()).collect()
    }

    /// Files with merge conflicts shown, by name, with the conflicts left in each.
    pub fn conflicts(&self) -> Vec<(String, usize)> {
        self.shown.conflicts.iter().map(|c| (c.name.to_string(), c.left)).collect()
    }

    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn can_undo(&self) -> bool {
        self.undo.is_some()
    }

    pub fn is_busy(&self) -> bool {
        self.busy || self.reading
    }
}
