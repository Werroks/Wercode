//! The centre of the window: tabs. A tab holds terminals, split into panes,
//! or one file open in the editor.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use wercode_core::{file_icons, fs_ops};
use wercode_core::state::{Layout, PaneLayout, TabLayout};

use super::view::{TerminalView, TerminalViewEvent};
use super::{
    ClosePane, CloseTab, NewTab, NextPane, NextTab, PrevPane, PrevTab, SplitDown, SplitRight,
};
use crate::diff::DiffView;
use crate::editor::{EditorEvent, EditorView};
use crate::theme::AppState;

const TAB_BAR_HEIGHT: f32 = 36.0;
/// Thickness of the strip between two panes that can be grabbed.
const DIVIDER: f32 = 5.0;
/// Neither side of a split may shrink below this share.
const MIN_RATIO: f32 = 0.1;

type Pane = Entity<TerminalView>;

struct Split {
    id: usize,
    /// `Horizontal` puts the panes side by side, `Vertical` stacks them.
    axis: Axis,
    /// Share of the space taken by `first`.
    ratio: f32,
    first: Node,
    second: Node,
    /// Where the split was last laid out, for turning a drag into a ratio.
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

enum Node {
    Leaf(Pane),
    Split(Box<Split>),
}

impl Node {
    fn leaves(&self, out: &mut Vec<Pane>) {
        match self {
            Node::Leaf(pane) => out.push(pane.clone()),
            Node::Split(split) => {
                split.first.leaves(out);
                split.second.leaves(out);
            }
        }
    }

    /// Puts `new` next to the pane `target`. Returns false if `target` is not here.
    fn split(&mut self, target: EntityId, axis: Axis, new: &Pane, id: usize) -> bool {
        match self {
            Node::Leaf(pane) if pane.entity_id() == target => {
                let first = Node::Leaf(pane.clone());
                *self = Node::Split(Box::new(Split {
                    id,
                    axis,
                    ratio: 0.5,
                    first,
                    second: Node::Leaf(new.clone()),
                    bounds: Rc::default(),
                }));
                true
            }
            Node::Leaf(_) => false,
            Node::Split(split) => {
                split.first.split(target, axis, new, id) || split.second.split(target, axis, new, id)
            }
        }
    }

    /// The tree without the pane `target`; its neighbour takes the freed space.
    fn without(self, target: EntityId) -> Option<Node> {
        match self {
            Node::Leaf(pane) => (pane.entity_id() != target).then_some(Node::Leaf(pane)),
            Node::Split(split) => {
                let Split { id, axis, ratio, first, second, bounds } = *split;
                match (first.without(target), second.without(target)) {
                    (Some(first), Some(second)) => {
                        Some(Node::Split(Box::new(Split { id, axis, ratio, first, second, bounds })))
                    }
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
        }
    }

    /// What to write down so this part of the tab can be put back later.
    fn layout(&self, cx: &App) -> PaneLayout {
        match self {
            Node::Leaf(pane) => PaneLayout::Terminal { cwd: pane.read(cx).cwd() },
            Node::Split(split) => PaneLayout::Split {
                stacked: split.axis == Axis::Vertical,
                ratio: split.ratio,
                first: Box::new(split.first.layout(cx)),
                second: Box::new(split.second.layout(cx)),
            },
        }
    }

    fn find_split(&mut self, id: usize) -> Option<&mut Split> {
        match self {
            Node::Leaf(_) => None,
            Node::Split(split) => {
                if split.id == id {
                    return Some(split);
                }
                if let Some(found) = split.first.find_split(id) {
                    return Some(found);
                }
                split.second.find_split(id)
            }
        }
    }
}

enum Body {
    Terminals {
        root: Node,
        /// The pane that has (or last had) the keyboard.
        active: Pane,
    },
    Editor(Entity<EditorView>),
    /// The changes to a file, read-only.
    Diff(Entity<DiffView>),
}

struct Tab {
    id: usize,
    body: Body,
}

impl Tab {
    /// The terminals in this tab; none in an editor tab.
    fn panes(&self) -> Vec<Pane> {
        let mut out = Vec::new();
        if let Body::Terminals { root, .. } = &self.body {
            root.leaves(&mut out);
        }
        out
    }

    fn active_pane(&self) -> Option<&Pane> {
        match &self.body {
            Body::Terminals { active, .. } => Some(active),
            Body::Editor(_) | Body::Diff(_) => None,
        }
    }

    fn editor(&self) -> Option<&Entity<EditorView>> {
        match &self.body {
            Body::Terminals { .. } | Body::Diff(_) => None,
            Body::Editor(editor) => Some(editor),
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.body {
            Body::Terminals { active, .. } => active.focus_handle(cx),
            Body::Editor(editor) => editor.focus_handle(cx),
            Body::Diff(diff) => diff.focus_handle(cx),
        }
    }
}

/// Terminals side by side in one tab. More than this and none of them is
/// wide enough to read; open another tab instead.
const MAX_PANES: usize = 4;

#[derive(Clone, Copy)]
enum CloseTarget {
    Pane(EntityId),
    Tab(usize),
}

pub struct TerminalArea {
    root_dir: PathBuf,
    tabs: Vec<Tab>,
    active: usize,
    next_id: usize,
    /// Split whose divider is being dragged.
    dragging: Option<usize>,
    /// A close waiting for the user to confirm, and how many terminals are busy.
    confirm: Option<(CloseTarget, usize)>,
    confirm_focus: FocusHandle,
    subscriptions: HashMap<EntityId, Subscription>,
}

impl Focusable for TerminalArea {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.tabs[self.active].focus_handle(cx)
    }
}

impl TerminalArea {
    /// Opens the centre of the window with the tabs it had last time, or with
    /// one terminal when there is nothing to put back.
    pub fn new(root_dir: PathBuf, layout: Option<&Layout>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            root_dir,
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
            dragging: None,
            confirm: None,
            confirm_focus: cx.focus_handle(),
            subscriptions: HashMap::new(),
        };
        if let Some(layout) = layout {
            this.restore(layout, window, cx);
        }
        if this.tabs.is_empty() {
            this.add_tab(window, cx);
        } else {
            this.active = layout.map_or(0, |l| l.active).min(this.tabs.len() - 1);
            this.focus_active(window, cx);
        }
        this
    }

    /// The open tabs, written down for the next launch.
    pub fn layout(&self, cx: &App) -> Layout {
        // A tab of changes is not kept: by the next launch they are different.
        let kept = |tab: &Tab| !matches!(tab.body, Body::Diff(_));
        let tabs: Vec<TabLayout> = self
            .tabs
            .iter()
            .filter_map(|tab| match &tab.body {
                Body::Terminals { root, .. } => Some(TabLayout::Terminals { panes: root.layout(cx) }),
                Body::Editor(editor) => Some(TabLayout::File { path: editor.read(cx).path().to_path_buf() }),
                Body::Diff(_) => None,
            })
            .collect();
        let before = self.tabs.iter().take(self.active).filter(|tab| kept(tab)).count();
        Layout { active: before.min(tabs.len().saturating_sub(1)), tabs }
    }

    /// Shows the changes to a file in a tab, or the tab that already shows them.
    pub fn open_diff(
        &mut self,
        root: PathBuf,
        path: PathBuf,
        staged: bool,
        untracked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.tabs.iter().position(|t| match &t.body {
            Body::Diff(diff) => diff.read(cx).shows(&path, staged),
            _ => false,
        });
        let index = match open {
            Some(index) => {
                if let Body::Diff(diff) = &self.tabs[index].body {
                    diff.update(cx, |diff, cx| {
                        diff.set_untracked(untracked);
                        diff.reload(cx);
                    });
                }
                index
            }
            None => {
                let diff = cx.new(|cx| DiffView::new(root, path, staged, untracked, cx));
                let id = self.id();
                self.tabs.push(Tab { id, body: Body::Diff(diff) });
                self.tabs.len() - 1
            }
        };
        self.select_tab(index, window, cx);
    }

    /// The repository or the files changed: tabs of changes read them again.
    pub fn refresh_diffs(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            if let Body::Diff(diff) = &tab.body {
                diff.update(cx, |diff, cx| diff.reload(cx));
            }
        }
    }

    fn restore(&mut self, layout: &Layout, window: &mut Window, cx: &mut Context<Self>) {
        for tab in &layout.tabs {
            match tab {
                // A file that is gone is simply not reopened.
                TabLayout::File { path } => {
                    if path.is_file() {
                        self.push_editor(path.clone(), window, cx);
                    }
                }
                TabLayout::Terminals { panes } => {
                    let mut room = MAX_PANES;
                    let Some(root) = self.restore_node(panes, &mut room, window, cx) else { continue };
                    let mut leaves = Vec::new();
                    root.leaves(&mut leaves);
                    let id = self.id();
                    self.tabs.push(Tab { id, body: Body::Terminals { root, active: leaves[0].clone() } });
                }
            }
        }
    }

    fn restore_node(
        &mut self,
        layout: &PaneLayout,
        room: &mut usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Node> {
        match layout {
            PaneLayout::Terminal { cwd } => {
                *room = room.checked_sub(1)?;
                // The folder may have been deleted since; the project folder is still there.
                let cwd = cwd.clone().filter(|dir| dir.is_dir()).unwrap_or_else(|| self.root_dir.clone());
                Some(Node::Leaf(self.spawn_pane(cwd, window, cx)))
            }
            PaneLayout::Split { stacked, ratio, first, second } => {
                let first = self.restore_node(first, room, window, cx);
                let second = self.restore_node(second, room, window, cx);
                match (first, second) {
                    (Some(first), Some(second)) => Some(Node::Split(Box::new(Split {
                        id: self.id(),
                        axis: if *stacked { Axis::Vertical } else { Axis::Horizontal },
                        ratio: if ratio.is_finite() { ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO) } else { 0.5 },
                        first,
                        second,
                        bounds: Rc::default(),
                    }))),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
        }
    }

    /// Adds a tab with `path` open in the editor and returns its index.
    fn push_editor(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> usize {
        let editor = cx.new(|cx| EditorView::new(path, window, cx));
        let subscription = cx.subscribe(&editor, |_, _, _: &EditorEvent, cx| cx.notify());
        self.subscriptions.insert(editor.entity_id(), subscription);
        let id = self.id();
        self.tabs.push(Tab { id, body: Body::Editor(editor) });
        self.tabs.len() - 1
    }

    /// Saves every file that has unsaved edits. `then` runs once all of them
    /// are on disk; if one cannot be written it does not run, and the tab of
    /// that file says why.
    pub fn save_all(
        &mut self,
        then: impl FnOnce(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let unsaved: Vec<_> =
            self.tabs.iter().filter_map(|t| t.editor()).filter(|e| e.read(cx).is_dirty()).cloned().collect();
        if unsaved.is_empty() {
            then(window, cx);
            return;
        }
        let left = Rc::new(Cell::new(unsaved.len()));
        let then = Rc::new(RefCell::new(Some(then)));
        for editor in unsaved {
            let (left, then) = (left.clone(), then.clone());
            let saved = move |window: &mut Window, cx: &mut App| {
                left.set(left.get() - 1);
                if left.get() == 0
                    && let Some(then) = then.borrow_mut().take()
                {
                    then(window, cx);
                }
            };
            editor.update(cx, |editor, cx| editor.save(saved, window, cx));
        }
    }

    /// How many terminals, in all tabs, are running a program right now.
    pub fn busy_count(&self, cx: &App) -> usize {
        self.tabs.iter().flat_map(|t| t.panes()).filter(|p| p.read(cx).is_busy()).count()
    }

    /// How many open files have edits that are not saved.
    pub fn unsaved_count(&self, cx: &App) -> usize {
        self.tabs.iter().filter_map(|t| t.editor()).filter(|e| e.read(cx).is_dirty()).count()
    }

    /// Opens a file in an editor tab, or shows the tab it is already in. The
    /// line and column are 1-based, as compilers print them.
    pub fn open_path(
        &mut self,
        path: PathBuf,
        line: Option<u32>,
        col: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if path.is_dir() {
            cx.open_with_system(&path);
            return;
        }
        let open = self.tabs.iter().position(|t| t.editor().is_some_and(|e| e.read(cx).path() == path));
        let index = match open {
            Some(index) => index,
            None => self.push_editor(path, window, cx),
        };
        if let (Some(line), Some(editor)) = (line, self.tabs[index].editor().cloned()) {
            editor.update(cx, |editor, cx| editor.go_to(line, col, window, cx));
        }
        self.select_tab(index, window, cx);
    }

    /// Types a path into a terminal: the one showing, or else the terminal
    /// tab nearest the end, which is then shown.
    pub fn insert_path(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let showing = self.tabs.get(self.active).is_some_and(|t| t.active_pane().is_some());
        let found = if showing { Some(self.active) } else { self.tabs.iter().rposition(|t| t.active_pane().is_some()) };
        let index = match found {
            Some(index) => index,
            None => {
                self.add_tab(window, cx);
                self.tabs.len() - 1
            }
        };
        let Some(pane) = self.tabs[index].active_pane().cloned() else { return };
        // Relative to where the shell is, so the line stays short.
        let text = fs_ops::for_shell(path, pane.read(cx).cwd().as_deref());
        pane.read(cx).insert_text(&format!("{text} "));
        self.select_tab(index, window, cx);
    }

    /// Opens a new terminal tab and runs `command` in it.
    pub fn run_in_new_tab(&mut self, command: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.add_tab(window, cx);
        if let Some(pane) = self.tabs[self.active].active_pane() {
            pane.read(cx).run(command);
        }
    }

    /// A file or folder was renamed: tabs of files under the old name follow it.
    pub fn path_renamed(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        for editor in self.tabs.iter().filter_map(|t| t.editor()) {
            let moved = editor.read(cx).path().strip_prefix(from).ok().map(|rest| {
                if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) }
            });
            if let Some(path) = moved {
                editor.update(cx, |editor, cx| editor.set_path(path, cx));
            }
        }
        cx.notify();
    }

    /// The tab of changes that is showing, if one is.
    #[cfg(test)]
    pub fn active_diff(&self) -> Option<Entity<DiffView>> {
        match &self.tabs.get(self.active)?.body {
            Body::Diff(diff) => Some(diff.clone()),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Terminals in all tabs.
    #[cfg(test)]
    pub fn pane_count(&self) -> usize {
        self.tabs.iter().map(|t| t.panes().len()).sum()
    }

    fn id(&mut self) -> usize {
        self.next_id += 1;
        self.next_id
    }

    fn spawn_pane(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Pane {
        let pane = cx.new(|cx| TerminalView::new(Some(cwd), cx));
        let subscription = cx.subscribe_in(&pane, window, |this, pane, event, window, cx| match event {
            TerminalViewEvent::TitleChanged => cx.notify(),
            // A shell that ended normally (`exit`) takes its pane with it. After
            // a failure the pane stays, so the reason remains readable.
            TerminalViewEvent::Exited(Some(0)) => this.remove_pane(pane.entity_id(), window, cx),
            TerminalViewEvent::Exited(_) => cx.notify(),
            TerminalViewEvent::OpenPath { path, line, col } => {
                this.open_path(path.clone(), *line, *col, window, cx)
            }
        });
        self.subscriptions.insert(pane.entity_id(), subscription);
        pane
    }

    fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // New tabs start in the project folder; splits follow the pane they came from.
        let pane = self.spawn_pane(self.root_dir.clone(), window, cx);
        let id = self.id();
        self.tabs.push(Tab { id, body: Body::Terminals { root: Node::Leaf(pane.clone()), active: pane } });
        self.active = self.tabs.len() - 1;
        self.focus_active(window, cx);
        cx.notify();
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) {
            let handle = tab.focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.active = index;
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    fn split_active(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        if tab.panes().len() >= MAX_PANES {
            return;
        }
        // Only terminals split; a file takes its whole tab.
        let Some(source) = tab.active_pane().cloned() else { return };
        let cwd = source.read(cx).cwd().unwrap_or_else(|| self.root_dir.clone());
        let pane = self.spawn_pane(cwd, window, cx);
        let id = self.id();
        if let Body::Terminals { root, active } = &mut self.tabs[self.active].body
            && root.split(source.entity_id(), axis, &pane, id)
        {
            *active = pane;
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn cycle_tab(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tabs.len() as isize;
        let next = (self.active as isize + step).rem_euclid(count) as usize;
        self.select_tab(next, window, cx);
    }

    fn cycle_pane(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let panes = tab.panes();
        let Body::Terminals { active, .. } = &mut tab.body else { return };
        let current = panes.iter().position(|p| p == active).unwrap_or(0) as isize;
        let next = (current + step).rem_euclid(panes.len() as isize) as usize;
        *active = panes[next].clone();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Closes a pane or a tab, asking first if that would kill a running program.
    fn request_close(&mut self, target: CloseTarget, window: &mut Window, cx: &mut Context<Self>) {
        if let CloseTarget::Tab(id) = target
            && let Some(editor) = self.tabs.iter().find(|t| t.id == id).and_then(|t| t.editor().cloned())
        {
            self.close_editor(id, editor, window, cx);
            return;
        }
        // Nothing to lose in a tab of changes.
        if let CloseTarget::Tab(id) = target
            && self.tabs.iter().any(|t| t.id == id && matches!(t.body, Body::Diff(_)))
        {
            self.remove_tab(id, window, cx);
            return;
        }
        let panes = match target {
            CloseTarget::Pane(id) => self
                .tabs
                .iter()
                .flat_map(|t| t.panes())
                .filter(|p| p.entity_id() == id)
                .collect::<Vec<_>>(),
            CloseTarget::Tab(id) => {
                self.tabs.iter().filter(|t| t.id == id).flat_map(|t| t.panes()).collect()
            }
        };
        let busy = panes.iter().filter(|p| p.read(cx).is_busy()).count();
        if busy > 0 && cx.global::<AppState>().settings.confirm_close {
            self.confirm = Some((target, busy));
            window.focus(&self.confirm_focus, cx);
            cx.notify();
        } else {
            self.close(target, window, cx);
        }
    }

    fn close(&mut self, target: CloseTarget, window: &mut Window, cx: &mut Context<Self>) {
        match target {
            CloseTarget::Pane(id) => self.remove_pane(id, window, cx),
            CloseTarget::Tab(id) => {
                let panes: Vec<Pane> =
                    self.tabs.iter().filter(|t| t.id == id).flat_map(|t| t.panes()).collect();
                for pane in panes {
                    self.remove_pane(pane.entity_id(), window, cx);
                }
            }
        }
    }

    /// Closes a file's tab; with unsaved edits it asks what to do with them.
    fn close_editor(&mut self, id: usize, editor: Entity<EditorView>, window: &mut Window, cx: &mut Context<Self>) {
        if !editor.read(cx).is_dirty() {
            self.remove_tab(id, window, cx);
            return;
        }
        let title = format!("Save changes to {}?", editor.read(cx).file_name());
        let answer = window.prompt(
            PromptLevel::Warning,
            &title,
            Some("Your changes will be lost if you do not save them."),
            &["Save", "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| match answer.await {
            Ok(0) => {
                let _ = editor.update_in(cx, |editor, window, cx| {
                    let saved = move |window: &mut Window, cx: &mut App| {
                        let _ = this.update(cx, |this, cx| this.remove_tab(id, window, cx));
                    };
                    editor.save(saved, window, cx);
                });
            }
            Ok(1) => {
                let _ = this.update_in(cx, |this, window, cx| this.remove_tab(id, window, cx));
            }
            _ => {}
        })
        .detach();
    }

    fn remove_tab(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else { return };
        let tab = self.tabs.remove(index);
        if let Some(editor) = tab.editor() {
            self.subscriptions.remove(&editor.entity_id());
        }
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        if self.tabs.is_empty() {
            self.add_tab(window, cx);
            return;
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn remove_pane(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|t| t.panes().iter().any(|p| p.entity_id() == id))
        else {
            return;
        };
        self.subscriptions.remove(&id);
        let tab = self.tabs.remove(index);
        let Body::Terminals { root, active } = tab.body else { return };
        let was_active = active.entity_id() == id;
        match root.without(id) {
            Some(root) => {
                let mut panes = Vec::new();
                root.leaves(&mut panes);
                let active = if was_active { panes[0].clone() } else { active };
                self.tabs.insert(index, Tab { id: tab.id, body: Body::Terminals { root, active } });
            }
            None => {
                if self.active > index || self.active >= self.tabs.len() {
                    self.active = self.active.saturating_sub(1);
                }
            }
        }
        if self.tabs.is_empty() {
            // The centre is never empty: closing the last terminal starts a new one.
            self.add_tab(window, cx);
            return;
        }
        // Keep the keyboard where it was unless its pane is the one that went away.
        let focus_lost = !self
            .tabs
            .iter()
            .flat_map(|t| t.panes())
            .any(|p| p.focus_handle(cx).is_focused(window));
        if focus_lost && self.confirm.is_none() {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// Closes the terminal that has the keyboard, or the file in an editor tab.
    fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let target = match tab.active_pane() {
            Some(pane) => CloseTarget::Pane(pane.entity_id()),
            None => CloseTarget::Tab(tab.id),
        };
        self.request_close(target, window, cx);
    }

    fn answer_confirm(&mut self, close: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((target, _)) = self.confirm.take() else { return };
        if close {
            self.close(target, window, cx);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn on_drag(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.dragging else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.dragging = None;
            return;
        }
        let Some(Body::Terminals { root, .. }) = self.tabs.get_mut(self.active).map(|t| &mut t.body) else {
            return;
        };
        let Some(split) = root.find_split(id) else { return };
        let bounds = split.bounds.get();
        let ratio = match split.axis {
            Axis::Horizontal if bounds.size.width > px(0.) => {
                (event.position.x - bounds.origin.x) / bounds.size.width
            }
            Axis::Vertical if bounds.size.height > px(0.) => {
                (event.position.y - bounds.origin.y) / bounds.size.height
            }
            _ => return,
        };
        split.ratio = ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
        cx.notify();
    }

    fn render_node(&self, node: &Node, cx: &mut Context<Self>) -> AnyElement {
        let split = match node {
            Node::Leaf(pane) => return div().size_full().child(pane.clone()).into_any_element(),
            Node::Split(split) => split,
        };
        let id = split.id;
        let horizontal = split.axis == Axis::Horizontal;
        let ratio = split.ratio;
        let bounds = split.bounds.clone();
        let line = cx.theme().border;
        let background = crate::theme::hsla(cx.global::<AppState>().scheme.terminal.background);

        let first = div()
            .flex_none()
            .overflow_hidden()
            .map(|d| if horizontal { d.h_full().w(relative(ratio)) } else { d.w_full().h(relative(ratio)) })
            .child(self.render_node(&split.first, cx));
        let second = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .child(self.render_node(&split.second, cx));
        let divider = div()
            .id(("divider", id))
            .flex_none()
            .flex()
            .bg(background)
            .map(|d| {
                if horizontal {
                    d.h_full().w(px(DIVIDER)).justify_center().cursor_col_resize()
                } else {
                    d.w_full().h(px(DIVIDER)).flex_col().justify_center().cursor_row_resize()
                }
            })
            .child(div().bg(line).map(|d| {
                if horizontal { d.h_full().w(px(1.)) } else { d.w_full().h(px(1.)) }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.dragging = Some(id);
                    cx.stop_propagation();
                }),
            );

        div()
            .size_full()
            .relative()
            .flex()
            .when(!horizontal, |d| d.flex_col())
            .child(
                canvas(move |laid_out, _, _| bounds.set(laid_out), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(first)
            .child(divider)
            .child(second)
            .into_any_element()
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted, foreground, active_bg, hover_bg) = (
            theme.border,
            theme.muted_foreground,
            theme.foreground,
            theme.tab_active,
            theme.secondary_hover,
        );

        let tabs = self.tabs.iter().enumerate().map(|(index, tab)| {
            let tab_id = tab.id;
            let selected = index == self.active;
            let (label, picture, unsaved) = match &tab.body {
                Body::Terminals { active, .. } => (active.read(cx).label(), None, false),
                Body::Editor(editor) => {
                    let editor = editor.read(cx);
                    let name = editor.file_name();
                    let picture = file_icons::for_file(&name).and_then(crate::assets::file_icon);
                    (SharedString::from(name), picture, editor.is_dirty())
                }
                Body::Diff(diff) => {
                    let diff = diff.read(cx);
                    let picture = file_icons::for_file(&diff.file_name()).and_then(crate::assets::file_icon);
                    (SharedString::from(diff.label()), picture, false)
                }
            };
            let panes = tab.panes().len();
            h_flex()
                .id(("tab", tab_id))
                .h_full()
                .min_w(px(64.))
                .max_w(px(240.))
                .flex_shrink(1.)
                .pl_3()
                .pr_1()
                .gap_1()
                .border_r_1()
                .border_color(border)
                .text_sm()
                .text_color(if selected { foreground } else { muted })
                .when(selected, |d| d.bg(active_bg))
                .hover(|s| s.text_color(foreground))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.select_tab(index, window, cx);
                }))
                .on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        this.request_close(CloseTarget::Tab(tab_id), window, cx);
                    }),
                )
                .when_some(picture, |d, picture| d.child(img(picture).flex_none().size(px(14.))))
                .child(div().flex_1().min_w_0().truncate().child(label))
                // A dot marks edits that are not on disk yet.
                .when(unsaved, |d| d.child(div().flex_none().size(px(7.)).rounded_full().bg(foreground)))
                .when(panes > 1, |d| d.child(div().flex_none().text_xs().text_color(muted).child(format!("{panes}"))))
                .child(
                    div()
                        .id(("close-tab", tab_id))
                        .flex_none()
                        .p(px(3.))
                        .rounded_sm()
                        .hover(|s| s.bg(hover_bg))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.request_close(CloseTarget::Tab(tab_id), window, cx);
                        }))
                        .child(Icon::new(IconName::Close).xsmall()),
                )
        });

        let button = |id: &'static str, icon: IconName, tooltip: &'static str| {
            Button::new(id).ghost().xsmall().icon(icon).tooltip(tooltip)
        };
        let on_file = self.tabs.get(self.active).is_some_and(|t| t.active_pane().is_none());
        let close_tip =
            if on_file { "Close this file (Ctrl+Shift+W)" } else { "Close this terminal (Ctrl+Shift+W)" };
        h_flex()
            .h(px(TAB_BAR_HEIGHT))
            .flex_none()
            .w_full()
            .border_b_1()
            .border_color(border)
            .child(h_flex().h_full().flex_1().min_w_0().overflow_hidden().children(tabs).child(
                div().px_2().flex_none().child(
                    button("new-tab", IconName::Plus, "New terminal (Ctrl+Shift+T)").on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.add_tab(window, cx)),
                    ),
                ),
            ))
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .px_2()
                    .child(button("close-pane", IconName::Close, close_tip).on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| this.close_active(window, cx),
                    )))
                    .when(!on_file, |d| {
                        d.child(
                            button("split-right", IconName::PanelRight, "Split right (Ctrl+Shift+D)").on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.split_active(Axis::Horizontal, window, cx)
                                }),
                            ),
                        )
                        .child(
                            button("split-down", IconName::PanelBottom, "Split down (Ctrl+Shift+E)").on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.split_active(Axis::Vertical, window, cx)
                                }),
                            ),
                        )
                    }),
            )
    }

    fn render_confirm(&self, busy: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let text = if busy == 1 {
            "A program is still running in this terminal. Closing it will stop the program.".to_string()
        } else {
            format!("Programs are still running in {busy} terminals. Closing them will stop the programs.")
        };
        div()
            .id("confirm-close")
            .track_focus(&self.confirm_focus)
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui_kit::black().opacity(0.5))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "enter" => this.answer_confirm(true, window, cx),
                    "escape" => this.answer_confirm(false, window, cx),
                    _ => {}
                }
                cx.stop_propagation();
            }))
            .child(
                v_flex()
                    .w(px(400.))
                    .p_4()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .text_color(theme.popover_foreground)
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Close terminal?"))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(text))
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .child(Button::new("confirm-cancel").small().label("Cancel").on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.answer_confirm(false, window, cx)
                                }),
                            ))
                            .child(Button::new("confirm-close").small().danger().label("Close").on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.answer_confirm(true, window, cx)
                                }),
                            )),
                    ),
            )
    }
}

impl Render for TerminalArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Whichever pane was clicked or tabbed into becomes its tab's active pane.
        if let Some(tab) = self.tabs.get_mut(self.active)
            && let Some(focused) =
                tab.panes().into_iter().find(|p| p.focus_handle(cx).is_focused(window))
            && let Body::Terminals { active, .. } = &mut tab.body
        {
            *active = focused;
        }

        let body = match self.tabs.get(self.active).map(|t| &t.body) {
            Some(Body::Terminals { root, .. }) => self.render_node(root, cx),
            Some(Body::Editor(editor)) => editor.clone().into_any_element(),
            Some(Body::Diff(diff)) => diff.clone().into_any_element(),
            None => div().into_any_element(),
        };
        // While a question is on screen the shortcuts below must not act behind it.
        let idle = self.confirm.is_none();

        v_flex()
            .id("terminal-area")
            .key_context("TerminalArea")
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .on_mouse_move(cx.listener(Self::on_drag))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.dragging = None),
            )
            .on_action(cx.listener(move |this, _: &NewTab, window, cx| {
                if idle {
                    this.add_tab(window, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &SplitRight, window, cx| {
                if idle {
                    this.split_active(Axis::Horizontal, window, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &SplitDown, window, cx| {
                if idle {
                    this.split_active(Axis::Vertical, window, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &ClosePane, window, cx| {
                if idle {
                    this.close_active(window, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &CloseTab, window, cx| {
                if idle && let Some(tab) = this.tabs.get(this.active) {
                    let target = CloseTarget::Tab(tab.id);
                    this.request_close(target, window, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &NextTab, window, cx| {
                if idle {
                    this.cycle_tab(1, window, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &PrevTab, window, cx| {
                if idle {
                    this.cycle_tab(-1, window, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &NextPane, window, cx| {
                if idle {
                    this.cycle_pane(1, window, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &PrevPane, window, cx| {
                if idle {
                    this.cycle_pane(-1, window, cx);
                }
            }))
            .child(self.render_tab_bar(cx))
            .child(div().flex_1().min_h_0().w_full().child(body))
            .when_some(self.confirm, |d, (_, busy)| d.child(self.render_confirm(busy, cx)))
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, WindowOptions};
    use wercode_core::config::Settings;

    use super::TerminalArea;

    fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<TerminalArea>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::apply(Settings::default(), cx);
            crate::term::init(cx);
            crate::editor::init(cx);
            let dir = std::env::temp_dir();
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| TerminalArea::new(dir, None, window, cx))
            })
            .unwrap()
        })
    }

    fn press(cx: &mut TestAppContext, window: AnyWindowHandle, keys: &[&str]) {
        cx.update_window(window, |_, window, cx| {
            for key in keys {
                window.press(key, cx);
            }
        })
        .unwrap();
    }

    /// (number of tabs, active tab index, panes in the active tab)
    fn shape(cx: &mut TestAppContext, area: &Entity<TerminalArea>) -> (usize, usize, usize) {
        area.read_with(cx, |a, _| (a.tabs.len(), a.active, a.tabs[a.active].panes().len()))
    }

    #[gpui_kit::test]
    async fn shortcuts_split_switch_and_close(cx: &mut TestAppContext) {
        let (window, area) = open(cx);
        assert_eq!(shape(cx, &area), (1, 0, 1));

        press(cx, window, &["ctrl-shift-d", "ctrl-shift-e"]);
        assert_eq!(shape(cx, &area), (1, 0, 3));

        press(cx, window, &["ctrl-shift-t"]);
        assert_eq!(shape(cx, &area), (2, 1, 1));
        press(cx, window, &["ctrl-tab"]);
        assert_eq!(shape(cx, &area), (2, 0, 3));
        press(cx, window, &["ctrl-shift-tab"]);
        assert_eq!(shape(cx, &area), (2, 1, 1));

        // Closing the only pane of a tab closes the tab.
        press(cx, window, &["ctrl-shift-w"]);
        assert_eq!(shape(cx, &area), (1, 0, 3));

        // Moving between panes changes which one a close removes.
        let before = area.read_with(cx, |a, _| a.tabs[0].active_pane().unwrap().entity_id());
        press(cx, window, &["ctrl-alt-left"]);
        let after = area.read_with(cx, |a, _| a.tabs[0].active_pane().unwrap().entity_id());
        assert_ne!(before, after);
        press(cx, window, &["ctrl-shift-w"]);
        assert_eq!(shape(cx, &area), (1, 0, 2));
        let panes = area.read_with(cx, |a, _| a.tabs[0].panes());
        assert!(panes.iter().all(|p| p.entity_id() != after));

        // The centre is never left empty.
        press(cx, window, &["ctrl-shift-q"]);
        assert_eq!(shape(cx, &area), (1, 0, 1));
    }

    #[gpui_kit::test]
    async fn splits_share_the_space_and_the_divider_moves(cx: &mut TestAppContext) {
        let (window, area) = open(cx);
        let sizes = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
            area.read_with(cx, |a, cx| {
                a.tabs[0].panes().iter().map(|p| p.read(cx).grid_size().unwrap()).collect::<Vec<_>>()
            })
        };
        let whole = sizes(cx)[0];
        assert!(whole.0 > 40 && whole.1 > 10, "one pane fills the window: {whole:?}");

        press(cx, window, &["ctrl-shift-d"]);
        let side = sizes(cx);
        assert_eq!(side.len(), 2);
        // Side by side: same height, each about half the width.
        assert_eq!(side[0].1, whole.1);
        assert_eq!(side[1].1, whole.1);
        assert!(side[0].0.abs_diff(side[1].0) <= 2, "{side:?}");
        assert!(side[0].0 + side[1].0 <= whole.0 && side[0].0 + side[1].0 + 6 > whole.0, "{side:?}");

        // Drag the divider a quarter of the way to the left.
        let bounds = area.read_with(cx, |a, _| match &a.tabs[0].body {
            super::Body::Terminals { root: super::Node::Split(split), .. } => split.bounds.get(),
            _ => panic!("expected a split"),
        });
        let from = gpui_kit::point(bounds.center().x, bounds.center().y);
        let to = gpui_kit::point(bounds.origin.x + bounds.size.width * 0.25, bounds.center().y);
        cx.update_window(window, |_, window, cx| window.drag(from, to, cx)).unwrap();
        let dragged = sizes(cx);
        assert!(dragged[0].0 * 2 < dragged[1].0, "left pane should be about a third: {dragged:?}");

        press(cx, window, &["ctrl-shift-e"]);
        let stacked = sizes(cx);
        assert_eq!(stacked.len(), 3);
        // The right pane was split top and bottom: same width, about half the height each.
        assert_eq!(stacked[1].0, stacked[2].0);
        assert!(stacked[1].1.abs_diff(stacked[2].1) <= 1, "{stacked:?}");
        assert!(stacked[1].1 < whole.1);
    }

    #[gpui_kit::test]
    async fn closing_a_busy_terminal_asks_first(cx: &mut TestAppContext) {
        let (window, area) = open(cx);
        press(cx, window, &["ctrl-shift-d"]);
        let pane = area.read_with(cx, |a, _| a.tabs[0].active_pane().unwrap().clone());
        // Something that keeps running for a while on any Windows or Unix shell.
        let command = if cfg!(windows) { "ping -n 60 127.0.0.1\r" } else { "sleep 60\r" };
        pane.read_with(cx, |p, _| p.insert_text(command));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !pane.read_with(cx, |p, _| p.is_busy()) {
            assert!(std::time::Instant::now() < deadline, "the command never started");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        press(cx, window, &["ctrl-shift-w"]);
        assert!(area.read_with(cx, |a, _| a.confirm.is_some()));
        assert_eq!(shape(cx, &area), (1, 0, 2), "nothing closes before the answer");

        // Shortcuts do not act behind the question; Escape keeps the terminal.
        press(cx, window, &["ctrl-shift-t", "escape"]);
        assert!(area.read_with(cx, |a, _| a.confirm.is_none()));
        assert_eq!(shape(cx, &area), (1, 0, 2));

        press(cx, window, &["ctrl-shift-w", "enter"]);
        assert!(area.read_with(cx, |a, _| a.confirm.is_none()));
        assert_eq!(shape(cx, &area), (1, 0, 1));
    }

    #[gpui_kit::test]
    async fn a_file_opens_in_a_tab_is_edited_and_saved(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (window, area) = open(cx);
        let dir = std::env::temp_dir().join(format!("wercode-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.txt");
        std::fs::write(&path, "one
two
").unwrap();

        let open_it = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| {
                area.update(cx, |a, cx| a.open_path(path.clone(), None, None, window, cx));
            })
            .unwrap();
            cx.run_until_parked();
        };
        open_it(cx);
        assert_eq!(shape(cx, &area), (2, 1, 0));
        // Asking for the same file again shows the tab it is in.
        press(cx, window, &["ctrl-tab"]);
        open_it(cx);
        assert_eq!(shape(cx, &area), (2, 1, 0));
        let editor = area.read_with(cx, |a, _| a.tabs[1].editor().unwrap().clone());
        assert_eq!(editor.read_with(cx, |e, cx| (e.text(cx), e.is_dirty())), ("one
two
".to_string(), false));

        // Typing marks the file unsaved; Ctrl+S writes it with its own line endings.
        cx.update_window(window, |_, window, cx| window.input("zero ", cx)).unwrap();
        cx.run_until_parked();
        assert!(editor.read_with(cx, |e, _| e.is_dirty()));
        assert_eq!(area.read_with(cx, |a, cx| a.unsaved_count(cx)), 1);
        press(cx, window, &["ctrl-s"]);
        cx.run_until_parked();
        assert!(!editor.read_with(cx, |e, _| e.is_dirty()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "zero one
two
");

        // Another program rewrites the file: without edits here it is simply read again.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&path, "from outside
").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        cx.executor().advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        assert_eq!(editor.read_with(cx, |e, cx| (e.text(cx), e.is_dirty())), ("from outside
".to_string(), false));

        // With edits here the text is kept and the change is only announced.
        cx.update_window(window, |_, window, cx| window.input("mine ", cx)).unwrap();
        cx.run_until_parked();
        std::fs::write(&path, "theirs
").unwrap();
        let later = later + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        cx.executor().advance_clock(std::time::Duration::from_secs(2));
        cx.run_until_parked();
        assert!(editor.read_with(cx, |e, cx| e.changed_on_disk() && e.text(cx).contains("mine")));

        // Closing asks about the edits; "Don't Save" closes and leaves the disk alone.
        press(cx, window, &["ctrl-shift-w"]);
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Don't Save");
        cx.run_until_parked();
        assert_eq!(shape(cx, &area), (1, 0, 1));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs
");

        // Something that is not text gets an explanation instead of an editor.
        let image = dir.join("picture.bin");
        std::fs::write(&image, [0u8, 159, 146, 150]).unwrap();
        cx.update_window(window, |_, window, cx| {
            area.update(cx, |a, cx| a.open_path(image.clone(), None, None, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        let editor = area.read_with(cx, |a, _| a.tabs[1].editor().unwrap().clone());
        assert!(editor.read_with(cx, |e, _| e.problem().is_some()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    async fn editor_undoes_redoes_finds_and_replaces(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (window, area) = open(cx);
        let dir = std::env::temp_dir().join(format!("wercode-undo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.txt");
        std::fs::write(&path, "one two one\n").unwrap();
        cx.update_window(window, |_, window, cx| {
            area.update(cx, |a, cx| a.open_path(path.clone(), None, None, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        let editor = area.read_with(cx, |a, _| a.tabs[1].editor().unwrap().clone());
        let text = |cx: &mut TestAppContext| editor.read_with(cx, |e, cx| e.text(cx));

        // Ctrl+Z takes the typing back, Ctrl+Y puts it in again.
        cx.update_window(window, |_, window, cx| window.input("zero ", cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(text(cx), "zero one two one\n");
        press(cx, window, &["ctrl-z"]);
        assert_eq!(text(cx), "one two one\n");
        press(cx, window, &["ctrl-y"]);
        assert_eq!(text(cx), "zero one two one\n");

        // Ctrl+F opens the find bar of the editor, Escape closes it.
        assert!(!editor.read_with(cx, |e, cx| e.is_searching(cx)));
        press(cx, window, &["ctrl-f"]);
        cx.run_until_parked();
        assert!(editor.read_with(cx, |e, cx| e.is_searching(cx)));
        press(cx, window, &["escape"]);
        cx.run_until_parked();
        assert!(!editor.read_with(cx, |e, cx| e.is_searching(cx)));
        press(cx, window, &["ctrl-h"]);
        cx.run_until_parked();
        assert!(editor.read_with(cx, |e, cx| e.is_searching(cx)));

        // Replace All changes every match, and one Ctrl+Z brings them all back.
        let replaced = cx
            .update_window(window, |_, window, cx| editor.update(cx, |e, cx| e.replace_all("ONE", "1", window, cx)))
            .unwrap();
        assert_eq!(replaced, 2);
        assert_eq!(text(cx), "zero 1 two 1\n");
        assert!(editor.read_with(cx, |e, _| e.is_dirty()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    async fn tabs_are_written_down_and_put_back(cx: &mut TestAppContext) {
        use wercode_core::state::{Layout, PaneLayout, TabLayout};
        cx.executor().allow_parking();
        let (window, area) = open(cx);
        let dir = std::env::temp_dir().join(format!("wercode-layout-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, "text\n").unwrap();

        press(cx, window, &["ctrl-shift-d", "ctrl-shift-e", "ctrl-shift-t"]);
        cx.update_window(window, |_, window, cx| {
            area.update(cx, |a, cx| a.open_path(file.clone(), None, None, window, cx));
        })
        .unwrap();
        let layout = area.read_with(cx, |a, cx| a.layout(cx));
        assert_eq!(layout.active, 2);
        assert_eq!(layout.tabs.len(), 3);
        let TabLayout::Terminals { panes } = &layout.tabs[0] else { panic!("{layout:?}") };
        assert_eq!(panes.terminals(), 3);
        // Split right, then the right half split down.
        let PaneLayout::Split { stacked: false, second, .. } = panes else { panic!("{panes:?}") };
        assert!(matches!(**second, PaneLayout::Split { stacked: true, .. }));
        assert_eq!(layout.tabs[2], TabLayout::File { path: file.clone() });

        // A layout from the state file: a folder and a file that are gone are
        // left out, a broken ratio is repaired, and no tab gets a fifth terminal.
        let terminal = |cwd: Option<std::path::PathBuf>| Box::new(PaneLayout::Terminal { cwd });
        let split = |stacked, ratio, first, second| Box::new(PaneLayout::Split { stacked, ratio, first, second });
        let crowded = split(
            false,
            f32::NAN,
            split(true, 0.25, terminal(Some(dir.join("sub"))), terminal(Some(dir.join("gone")))),
            split(true, 5.0, terminal(None), split(false, 0.5, terminal(None), terminal(None))),
        );
        let saved = Layout {
            tabs: vec![
                TabLayout::File { path: dir.join("gone.txt") },
                TabLayout::Terminals { panes: *crowded },
                TabLayout::File { path: file.clone() },
            ],
            active: 7,
        };
        let root = dir.clone();
        let (_, restored) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| TerminalArea::new(root, Some(&saved), window, cx))
            })
            .unwrap()
        });
        let layout = restored.read_with(cx, |a, cx| a.layout(cx));
        assert_eq!(layout.tabs.len(), 2);
        assert_eq!(layout.active, 1);
        assert_eq!(layout.tabs[1], TabLayout::File { path: file.clone() });
        let TabLayout::Terminals { panes } = &layout.tabs[0] else { panic!("{layout:?}") };
        assert_eq!(panes.terminals(), 4);
        let PaneLayout::Split { stacked: false, ratio, first, second } = panes else { panic!("{panes:?}") };
        assert_eq!(*ratio, 0.5);
        assert!(matches!(**first, PaneLayout::Split { stacked: true, ratio, .. } if ratio == 0.25));
        assert!(matches!(**second, PaneLayout::Split { stacked: true, ratio, .. } if ratio == 0.9));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    async fn tab_key_goes_to_the_shell_not_to_focus_navigation(cx: &mut TestAppContext) {
        let (window, area) = open(cx);
        let pane = area.read_with(cx, |a, _| a.tabs[0].active_pane().unwrap().clone());
        press(cx, window, &["tab", "shift-tab"]);
        cx.update_window(window, |_, window, cx| {
            use gpui_kit::Focusable as _;
            assert!(pane.focus_handle(cx).is_focused(window));
        })
        .unwrap();
    }
}
