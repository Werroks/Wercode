//! The centre of the window: tabs. A tab holds terminals, split into panes,
//! or a file open in the editor, with maybe a second one beside it. The tabs sit in one row, or, when the
//! settings ask for it, files sit above and the terminals in a panel below.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use wercode_core::changes::Side;
use wercode_core::{file_icons, fs_ops, shell};
use wercode_core::state::{self, Layout, PaneLayout, TabLayout};

use super::view::{TerminalView, TerminalViewEvent};
use super::{
    ClosePane, CloseTab, Find, NewTab, PaneDown, PaneLeft, PaneRight, PaneUp, RenameTab, NextPane, NextTab, PrevPane, PrevTab, SplitDown, SplitRight,
    ToggleTerminal,
};
use crate::changes::{ChangesEvent, ChangesView};
use crate::diff::DiffView;
use crate::editor::{EditorEvent, EditorView, SplitEditorDown, SplitEditorRight};
use crate::panel::DraggedPath;
use crate::theme::AppState;
use wercode_core::i18n::{t, tf, tn};

const TAB_BAR_HEIGHT: f32 = 36.0;
/// Thickness of the strip between two panes that can be grabbed.
const DIVIDER: f32 = 5.0;
/// Neither side of a split may shrink below this share.
const MIN_RATIO: f32 = 0.1;
/// Share of the height the files take over a terminal panel, to begin with.
const DOCK_RATIO: f32 = 0.62;
/// Neither the files nor the terminal panel may shrink below this share.
const MIN_DOCK: f32 = 0.15;

/// Which tabs a tab bar shows.
#[derive(Clone, Copy, PartialEq)]
enum Group {
    /// One row for everything.
    All,
    Files,
    Terminals,
}

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

    /// Where each pane sits, as its left, top, right and bottom edge in
    /// fractions of the space `within`.
    fn places(&self, within: [f32; 4], out: &mut Vec<(Pane, [f32; 4])>) {
        match self {
            Node::Leaf(pane) => out.push((pane.clone(), within)),
            Node::Split(split) => {
                let [left, top, right, bottom] = within;
                let (first, second) = if split.axis == Axis::Horizontal {
                    let middle = left + (right - left) * split.ratio;
                    ([left, top, middle, bottom], [middle, top, right, bottom])
                } else {
                    let middle = top + (bottom - top) * split.ratio;
                    ([left, top, right, middle], [left, middle, right, bottom])
                };
                split.first.places(first, out);
                split.second.places(second, out);
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
            Node::Leaf(pane) => {
                let pane = pane.read(cx);
                PaneLayout::Terminal { cwd: pane.cwd(), shell: pane.chosen_shell() }
            }
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

/// A second file shown beside the first in one tab.
struct Beside {
    editor: Entity<EditorView>,
    /// `Horizontal` puts the files side by side, `Vertical` one above the other.
    axis: Axis,
    /// Share of the space taken by the first file.
    ratio: f32,
    /// Where the two were last laid out, for turning a drag into a ratio.
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

enum Body {
    Terminals {
        root: Node,
        /// The pane that has (or last had) the keyboard.
        active: Pane,
    },
    Editor {
        first: Entity<EditorView>,
        beside: Option<Beside>,
        /// The second file has (or last had) the keyboard.
        second_active: bool,
    },
    /// The changes to a file, read-only.
    Diff(Entity<DiffView>),
    /// Every change in the repository, with buttons for each piece.
    Changes(Entity<ChangesView>),
}

/// What the tabs tell the window.
pub enum AreaEvent {
    /// A tab changed the repository (staged or threw away a change).
    GitChanged,
    /// A file is to be chosen to show beside the one showing.
    PickBeside(Axis),
}

struct Tab {
    id: usize,
    /// A name the user gave a terminal tab; without one it shows what runs in it.
    name: Option<String>,
    body: Body,
}

/// A tab being dragged to another place in the row.
#[derive(Clone)]
pub struct DraggedTab {
    id: usize,
    label: SharedString,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .text_sm()
            .child(self.label.clone())
    }
}

/// The field on a tab while it is being renamed.
struct Renaming {
    tab: usize,
    input: Entity<InputState>,
    _subscription: Subscription,
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

    /// A tab of terminals, as opposed to a file or the changes to one.
    fn is_terminal(&self) -> bool {
        matches!(self.body, Body::Terminals { .. })
    }

    /// The keyboard is somewhere in this tab.
    fn has_focus(&self, window: &Window, cx: &App) -> bool {
        match &self.body {
            Body::Terminals { .. } => self.panes().iter().any(|p| p.focus_handle(cx).is_focused(window)),
            Body::Editor { .. } => self.editors().iter().any(|e| e.focus_handle(cx).contains_focused(window, cx)),
            _ => self.focus_handle(cx).contains_focused(window, cx),
        }
    }

    fn active_pane(&self) -> Option<&Pane> {
        match &self.body {
            Body::Terminals { active, .. } => Some(active),
            Body::Editor { .. } | Body::Diff(_) | Body::Changes(_) => None,
        }
    }

    /// The file that has (or last had) the keyboard in an editor tab.
    fn editor(&self) -> Option<&Entity<EditorView>> {
        match &self.body {
            Body::Terminals { .. } | Body::Diff(_) | Body::Changes(_) => None,
            Body::Editor { first, beside, second_active } => {
                Some(beside.as_ref().filter(|_| *second_active).map_or(first, |beside| &beside.editor))
            }
        }
    }

    /// Every file open in this tab: one, or two side by side.
    fn editors(&self) -> Vec<&Entity<EditorView>> {
        match &self.body {
            Body::Editor { first, beside, .. } => std::iter::once(first).chain(beside.iter().map(|b| &b.editor)).collect(),
            _ => Vec::new(),
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.body {
            Body::Terminals { active, .. } => active.focus_handle(cx),
            Body::Editor { .. } => self.editor().map(|e| e.focus_handle(cx)).unwrap_or_else(|| cx.focus_handle()),
            Body::Diff(diff) => diff.focus_handle(cx),
            Body::Changes(changes) => changes.focus_handle(cx),
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
    /// The file tab and the terminal tab that were showing last, by tab id.
    /// With the terminal below the files, one of each is on screen.
    last_file: Option<usize>,
    last_terminal: Option<usize>,
    /// Share of the height the files take when the terminal sits below them.
    dock_ratio: f32,
    /// The terminal below the files is put away.
    terminal_hidden: bool,
    /// The line between the files and the terminal is being dragged.
    dock_dragging: bool,
    /// Where the files and the terminal were last laid out together.
    dock_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// A terminal tab whose name is being typed.
    renaming: Option<Renaming>,
    /// The tab whose line between two files is being dragged, by id.
    beside_dragging: Option<usize>,
}

impl EventEmitter<AreaEvent> for TerminalArea {}

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
            last_file: None,
            last_terminal: None,
            dock_ratio: layout
                .and_then(|l| l.dock_ratio)
                .filter(|ratio| ratio.is_finite())
                .map_or(DOCK_RATIO, |ratio| ratio.clamp(MIN_DOCK, 1.0 - MIN_DOCK)),
            terminal_hidden: layout.is_some_and(|l| l.terminal_hidden),
            dock_dragging: false,
            dock_bounds: Rc::default(),
            renaming: None,
            beside_dragging: None,
        };
        if let Some(layout) = layout {
            this.restore(layout, window, cx);
        }
        if this.tabs.is_empty() {
            this.add_tab(window, cx);
        } else {
            this.active = layout.map_or(0, |l| l.active).min(this.tabs.len() - 1);
            this.note_active();
            this.focus_active(window, cx);
        }
        this
    }

    /// Files above, terminals in a panel below; otherwise one row of tabs.
    fn docked(cx: &App) -> bool {
        cx.global::<AppState>().settings.terminal_below()
    }

    /// Index of the file tab (or the terminal tab) to show: the one with the
    /// keyboard if it is of that kind, else the one of that kind shown last.
    fn shown(&self, terminals: bool) -> Option<usize> {
        let of_kind = |tab: &Tab| tab.is_terminal() == terminals;
        if self.tabs.get(self.active).is_some_and(of_kind) {
            return Some(self.active);
        }
        let last = if terminals { self.last_terminal } else { self.last_file };
        self.tabs
            .iter()
            .position(|tab| Some(tab.id) == last && of_kind(tab))
            .or_else(|| self.tabs.iter().rposition(of_kind))
    }

    /// Remembers the tab that is showing as the last one of its kind.
    fn note_active(&mut self) {
        if let Some(tab) = self.tabs.get(self.active) {
            if tab.is_terminal() {
                self.last_terminal = Some(tab.id);
                self.terminal_hidden = false;
            } else {
                self.last_file = Some(tab.id);
            }
        }
    }

    /// With the terminal below the files, a command for terminals given while
    /// the keyboard is in a file goes to the terminal that is showing.
    fn turn_to_terminal(&mut self, cx: &App) {
        if Self::docked(cx)
            && !self.tabs.get(self.active).is_some_and(Tab::is_terminal)
            && let Some(index) = self.shown(true)
        {
            self.active = index;
            self.note_active();
        }
    }

    /// Picks the tab to show after the tab at `index` was taken out.
    fn settle(&mut self, index: usize, terminal: bool, cx: &App) {
        let was_active = self.active == index;
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        // Below the files a closed terminal gives way to another terminal,
        // and a closed file to another file, while there is one.
        if was_active && Self::docked(cx) {
            let same = |tab: &Tab| tab.is_terminal() == terminal;
            let before = self.tabs.iter().take(index).rposition(same);
            if let Some(found) = before.or_else(|| self.tabs.iter().position(same)) {
                self.active = found;
            }
        }
        self.note_active();
    }

    /// Shows the terminal panel and puts the keyboard in it; from inside it,
    /// puts it away and goes back to the file.
    fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let on_terminal = self.tabs.get(self.active).is_some_and(Tab::is_terminal);
        match (self.shown(true), self.shown(false)) {
            (None, _) => self.add_tab(window, cx),
            (Some(_), Some(file)) if on_terminal && Self::docked(cx) => {
                self.select_tab(file, window, cx);
                self.terminal_hidden = true;
            }
            (Some(terminal), _) => self.select_tab(terminal, window, cx),
        }
    }

    /// Puts the terminal panel away, wherever the keyboard is.
    fn hide_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(file) = self.shown(false) {
            if self.tabs.get(self.active).is_some_and(Tab::is_terminal) {
                self.select_tab(file, window, cx);
            }
            self.terminal_hidden = true;
            cx.notify();
        }
    }

    /// The open tabs, written down for the next launch.
    pub fn layout(&self, cx: &App) -> Layout {
        // A tab of changes is not kept: by the next launch they are different.
        let kept = |tab: &Tab| !matches!(tab.body, Body::Diff(_) | Body::Changes(_));
        let tabs: Vec<TabLayout> = self
            .tabs
            .iter()
            .filter_map(|tab| match &tab.body {
                Body::Terminals { root, .. } => {
                    Some(TabLayout::Terminals { panes: root.layout(cx), name: tab.name.clone() })
                }
                Body::Editor { first, beside, .. } => Some(TabLayout::File {
                    path: first.read(cx).path().to_path_buf(),
                    beside: beside.as_ref().map(|beside| state::Beside {
                        path: beside.editor.read(cx).path().to_path_buf(),
                        stacked: beside.axis == Axis::Vertical,
                        ratio: beside.ratio,
                    }),
                }),
                Body::Diff(_) | Body::Changes(_) => None,
            })
            .collect();
        let before = self.tabs.iter().take(self.active).filter(|tab| kept(tab)).count();
        Layout {
            active: before.min(tabs.len().saturating_sub(1)),
            tabs,
            dock_ratio: (self.dock_ratio != DOCK_RATIO).then_some(self.dock_ratio),
            terminal_hidden: self.terminal_hidden,
        }
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
                self.tabs.push(Tab { id, name: None, body: Body::Diff(diff) });
                self.tabs.len() - 1
            }
        };
        self.select_tab(index, window, cx);
    }

    /// The repository or the files changed: tabs of changes read them again.
    pub fn refresh_diffs(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            match &tab.body {
                // What a commit changed stays the same.
                Body::Diff(diff) if !diff.read(cx).is_commit() => diff.update(cx, |diff, cx| diff.reload(cx)),
                Body::Changes(changes) => changes.update(cx, |changes, cx| changes.reload(cx)),
                _ => {}
            }
        }
    }

    /// Shows every change in the repository in a tab (there is one such
    /// tab), on the side of the index asked for.
    pub fn open_changes(&mut self, staged: bool, reveal: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let side = if staged { Side::Staged } else { Side::Unstaged };
        let open = self.tabs.iter().position(|t| matches!(t.body, Body::Changes(_)));
        let index = match open {
            Some(index) => {
                if let Body::Changes(changes) = &self.tabs[index].body {
                    changes.update(cx, |changes, cx| changes.set_side(side, cx));
                }
                index
            }
            None => {
                let root = self.root_dir.clone();
                let changes = cx.new(|cx| ChangesView::new(root, side, cx));
                let subscription = cx.subscribe_in(&changes, window, |this, _, event: &ChangesEvent, window, cx| match event {
                    ChangesEvent::Changed => cx.emit(AreaEvent::GitChanged),
                    ChangesEvent::Open { path, line } => this.open_path(path.clone(), *line, None, window, cx),
                });
                self.subscriptions.insert(changes.entity_id(), subscription);
                let id = self.id();
                self.tabs.push(Tab { id, name: None, body: Body::Changes(changes) });
                self.tabs.len() - 1
            }
        };
        if let (Some(path), Body::Changes(changes)) = (reveal, &self.tabs[index].body) {
            changes.update(cx, |changes, cx| changes.reveal(path, cx));
        }
        self.select_tab(index, window, cx);
    }

    /// Shows what a commit changed in a file, in a tab of its own.
    pub fn open_commit_diff(
        &mut self,
        root: PathBuf,
        path: PathBuf,
        hash: String,
        parent: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.tabs.iter().position(|t| match &t.body {
            Body::Diff(diff) => diff.read(cx).shows_commit(&path, &hash),
            _ => false,
        });
        let index = match open {
            Some(index) => index,
            None => {
                let diff = cx.new(|cx| DiffView::of_commit(root, path, hash, parent, cx));
                let id = self.id();
                self.tabs.push(Tab { id, name: None, body: Body::Diff(diff) });
                self.tabs.len() - 1
            }
        };
        self.select_tab(index, window, cx);
    }

    fn restore(&mut self, layout: &Layout, window: &mut Window, cx: &mut Context<Self>) {
        for tab in &layout.tabs {
            match tab {
                // A file that is gone is simply not reopened.
                TabLayout::File { path, beside } => {
                    let second = beside.as_ref().filter(|b| b.path.is_file() && &b.path != path);
                    let index = match (path.is_file(), second) {
                        (true, _) => self.push_editor(path.clone(), window, cx),
                        (false, Some(second)) => {
                            self.push_editor(second.path.clone(), window, cx);
                            continue;
                        }
                        (false, None) => continue,
                    };
                    if let Some(second) = second {
                        let editor = self.new_editor(second.path.clone(), window, cx);
                        let axis = if second.stacked { Axis::Vertical } else { Axis::Horizontal };
                        let ratio = if second.ratio.is_finite() { second.ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO) } else { 0.5 };
                        if let Body::Editor { beside, .. } = &mut self.tabs[index].body {
                            *beside = Some(Beside { editor, axis, ratio, bounds: Rc::default() });
                        }
                    }
                }
                TabLayout::Terminals { panes, name } => {
                    let mut room = MAX_PANES;
                    let Some(root) = self.restore_node(panes, &mut room, window, cx) else { continue };
                    let mut leaves = Vec::new();
                    root.leaves(&mut leaves);
                    let id = self.id();
                    let name = name.clone();
                    self.tabs.push(Tab { id, name, body: Body::Terminals { root, active: leaves[0].clone() } });
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
            PaneLayout::Terminal { cwd, shell } => {
                *room = room.checked_sub(1)?;
                // The folder may have been deleted since; the project folder is still there.
                let cwd = cwd.clone().filter(|dir| dir.is_dir()).unwrap_or_else(|| self.root_dir.clone());
                Some(Node::Leaf(self.spawn_pane_with(cwd, shell.clone(), window, cx)))
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

    /// Opens `path` in an editor that is not in a tab yet.
    fn new_editor(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Entity<EditorView> {
        let root = self.root_dir.clone();
        let editor = cx.new(|cx| EditorView::new(path, root, window, cx));
        let subscription = cx.subscribe_in(&editor, window, |this, _, event: &EditorEvent, window, cx| match event {
            EditorEvent::Changed => cx.notify(),
            EditorEvent::Open { path, at } => this.open_path(path.clone(), Some(at.0), Some(at.1), window, cx),
        });
        self.subscriptions.insert(editor.entity_id(), subscription);
        editor
    }

    /// Adds a tab with `path` open in the editor and returns its index.
    fn push_editor(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> usize {
        let editor = self.new_editor(path, window, cx);
        self.push_open_editor(editor)
    }

    /// Gives an editor that is already open a tab of its own.
    fn push_open_editor(&mut self, editor: Entity<EditorView>) -> usize {
        let id = self.id();
        self.tabs.push(Tab { id, name: None, body: Body::Editor { first: editor, beside: None, second_active: false } });
        self.tabs.len() - 1
    }

    /// The tab a file is open in, and whether it is the second file there.
    fn find_file(&self, path: &Path, cx: &App) -> Option<(usize, bool)> {
        self.tabs.iter().enumerate().find_map(|(index, tab)| {
            tab.editors().iter().position(|e| e.read(cx).path() == path).map(|at| (index, at == 1))
        })
    }

    /// Shows which of the two files of a tab has the keyboard.
    fn set_second_active(&mut self, index: usize, second: bool) {
        if let Some(Body::Editor { beside, second_active, .. }) = self.tabs.get_mut(index).map(|t| &mut t.body) {
            *second_active = second && beside.is_some();
        }
    }

    /// The file tab another file can go beside: the one showing.
    fn beside_target(&self, cx: &App) -> Option<usize> {
        let index = if Self::docked(cx) { self.shown(false)? } else { self.active };
        matches!(self.tabs.get(index)?.body, Body::Editor { .. }).then_some(index)
    }

    /// Asks the window to choose a file to show beside the one showing; with
    /// no file showing, the file is simply opened.
    fn split_editor(&mut self, axis: Axis, cx: &mut Context<Self>) {
        cx.emit(AreaEvent::PickBeside(axis));
    }

    /// Shows a file beside the file showing, in the same tab. A file that has
    /// a tab of its own moves over; the file that was beside before gets a
    /// tab of its own, so nothing is closed.
    pub fn open_beside(&mut self, path: PathBuf, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        if path.is_dir() {
            return;
        }
        let Some(target) = self.beside_target(cx) else {
            self.open_path(path, None, None, window, cx);
            return;
        };
        let target_id = self.tabs[target].id;
        let editor = match self.find_file(&path, cx) {
            Some((index, second)) if index == target || self.tabs[index].editors().len() > 1 => {
                // Already beside something: shown where it is.
                self.set_second_active(index, second);
                self.select_tab(index, window, cx);
                return;
            }
            Some((index, _)) => {
                let tab = self.tabs.remove(index);
                if self.active > index {
                    self.active -= 1;
                }
                let Body::Editor { first, .. } = tab.body else { return };
                first
            }
            None => self.new_editor(path, window, cx),
        };
        self.put_beside(target_id, editor, axis, window, cx);
    }

    /// Puts an open editor beside the file of tab `target_id`.
    fn put_beside(
        &mut self,
        target_id: usize,
        editor: Entity<EditorView>,
        axis: Axis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.tabs.iter().position(|t| t.id == target_id) else {
            let index = self.push_open_editor(editor);
            self.select_tab(index, window, cx);
            return;
        };
        let Body::Editor { beside, second_active, .. } = &mut self.tabs[target].body else { return };
        let old = beside.replace(Beside { editor, axis, ratio: 0.5, bounds: Rc::default() });
        *second_active = true;
        if let Some(old) = old {
            self.push_open_editor(old.editor);
        }
        let target = self.tabs.iter().position(|t| t.id == target_id).unwrap_or(target);
        self.select_tab(target, window, cx);
    }

    /// A tab dragged onto a file: its file goes beside that one.
    fn drop_beside(&mut self, dragged: usize, target_id: usize, window: &mut Window, cx: &mut Context<Self>) {
        if dragged == target_id {
            return;
        }
        let Some(index) = self.tabs.iter().position(|t| t.id == dragged) else { return };
        if self.tabs[index].editors().len() != 1 {
            return;
        }
        let tab = self.tabs.remove(index);
        if self.active > index {
            self.active -= 1;
        }
        let Body::Editor { first, .. } = tab.body else { return };
        self.put_beside(target_id, first, Axis::Horizontal, window, cx);
    }

    /// Closes the files of an editor tab: both, or with `half` one of two.
    /// Unsaved edits are asked about first.
    fn close_files(&mut self, id: usize, half: Option<bool>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter().find(|t| t.id == id) else { return };
        let all: Vec<Entity<EditorView>> = tab.editors().into_iter().cloned().collect();
        let half = half.filter(|_| all.len() > 1);
        let closing: Vec<_> = match half {
            None => all,
            Some(second) => all.into_iter().skip(usize::from(second)).take(1).collect(),
        };
        let done = move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| match half {
            None => this.remove_tab(id, window, cx),
            Some(second) => this.remove_half(id, second, window, cx),
        };
        let dirty: Vec<_> = closing.into_iter().filter(|e| e.read(cx).is_dirty()).collect();
        if dirty.is_empty() {
            done(self, window, cx);
            return;
        }
        let title = match dirty.as_slice() {
            [one] => tf("Save changes to {name}?", &[("name", &one.read(cx).file_name())]),
            _ => tn(dirty.len() as u64, "Save changes to {n} file?", "Save changes to {n} files?", &[]),
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &title,
            Some(t("Your changes will be lost if you do not save them.")),
            &[t("Save"), t("Don't Save"), t("Cancel")],
            cx,
        );
        let this = cx.entity().downgrade();
        cx.spawn_in(window, async move |_, cx| match answer.await {
            Ok(0) => {
                let left = Rc::new(Cell::new(dirty.len()));
                for editor in dirty {
                    let (left, this) = (left.clone(), this.clone());
                    let _ = editor.update_in(cx, |editor, window, cx| {
                        let saved = move |window: &mut Window, cx: &mut App| {
                            left.set(left.get() - 1);
                            if left.get() == 0 {
                                let _ = this.update(cx, |this, cx| done(this, window, cx));
                            }
                        };
                        editor.save(saved, window, cx);
                    });
                }
            }
            Ok(1) => {
                let _ = this.update_in(cx, |this, window, cx| done(this, window, cx));
            }
            _ => {}
        })
        .detach();
    }

    /// Takes one of the two files out of a tab; the other gets the whole tab.
    fn remove_half(&mut self, id: usize, second: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else { return };
        let Body::Editor { first, beside, second_active } = &mut self.tabs[index].body else { return };
        let Some(other) = beside.take() else {
            self.remove_tab(id, window, cx);
            return;
        };
        let gone = if second { other.editor } else { std::mem::replace(first, other.editor) };
        *second_active = false;
        self.subscriptions.remove(&gone.entity_id());
        if self.active == index {
            self.focus_active(window, cx);
        }
        cx.notify();
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
            self.tabs.iter().flat_map(|t| t.editors()).filter(|e| e.read(cx).is_dirty()).cloned().collect();
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
        self.tabs.iter().flat_map(|t| t.editors()).filter(|e| e.read(cx).is_dirty()).count()
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
        let index = match self.find_file(&path, cx) {
            Some((index, second)) => {
                self.set_second_active(index, second);
                index
            }
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
        let index = match self.shown(true) {
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

    /// The file whose tab is showing. With one row of tabs that is the
    /// active tab; with the terminal below, the file above it.
    pub fn shown_file(&self, cx: &App) -> Option<PathBuf> {
        let index = if Self::docked(cx) { self.shown(false)? } else { self.active };
        self.tabs.get(index)?.editor().map(|editor| editor.read(cx).path().to_path_buf())
    }

    /// Who last changed the line under the cursor of the file that is showing.
    pub fn shown_blame(&self, cx: &App) -> Option<wercode_core::git::Blame> {
        let index = if Self::docked(cx) { self.shown(false)? } else { self.active };
        self.tabs.get(index)?.editor()?.read(cx).blame().cloned()
    }

    /// Opens a new terminal tab and runs `command` in it.
    pub fn run_in_new_tab(&mut self, command: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.add_tab(window, cx);
        if let Some(pane) = self.tabs[self.active].active_pane() {
            pane.read(cx).run(command);
        }
    }

    /// Git has a new commit or another branch: open files count their
    /// changed lines from it.
    pub fn git_changed(&mut self, cx: &mut Context<Self>) {
        for editor in self.tabs.iter().flat_map(|t| t.editors()) {
            editor.update(cx, |editor, cx| editor.git_changed(cx));
        }
    }

    /// A file or folder was renamed: tabs of files under the old name follow it.
    pub fn path_renamed(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        for editor in self.tabs.iter().flat_map(|t| t.editors()) {
            let moved = editor.read(cx).path().strip_prefix(from).ok().map(|rest| {
                if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) }
            });
            if let Some(path) = moved {
                editor.update(cx, |editor, cx| editor.set_path(path, cx));
            }
        }
        cx.notify();
    }

    /// The tab of every change, if it is showing.
    #[cfg(test)]
    pub fn active_changes(&self) -> Option<Entity<ChangesView>> {
        match &self.tabs.get(self.active)?.body {
            Body::Changes(changes) => Some(changes.clone()),
            _ => None,
        }
    }

    /// The tab of changes that is showing, if one is.
    #[cfg(test)]
    pub fn active_diff(&self) -> Option<Entity<DiffView>> {
        match &self.tabs.get(self.active)?.body {
            Body::Diff(diff) => Some(diff.clone()),
            _ => None,
        }
    }

    /// The editor tab of a file, if it is open.
    #[cfg(test)]
    pub fn editor_for(&self, path: &Path, cx: &App) -> Option<Entity<EditorView>> {
        self.tabs.iter().flat_map(|t| t.editors()).find(|e| e.read(cx).path() == path).cloned()
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

    /// A terminal running `shell`, or the shell in the settings.
    fn spawn_pane_with(
        &mut self,
        cwd: PathBuf,
        shell: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Pane {
        let pane = cx.new(|cx| TerminalView::with_shell(Some(cwd), shell, cx));
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
        self.add_tab_with(None, window, cx);
    }

    /// A new terminal tab running `shell`, or the shell in the settings.
    pub fn add_tab_with(&mut self, shell: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        // New tabs start in the project folder; splits follow the pane they came from.
        let pane = self.spawn_pane_with(self.root_dir.clone(), shell, window, cx);
        let id = self.id();
        self.tabs.push(Tab { id, name: None, body: Body::Terminals { root: Node::Leaf(pane.clone()), active: pane } });
        self.active = self.tabs.len() - 1;
        self.note_active();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Puts a field on a terminal tab to give it a name of its own.
    fn start_rename(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter().find(|t| t.id == id && t.is_terminal()) else { return };
        let current = tab.name.clone().unwrap_or_else(|| {
            tab.active_pane().map(|p| p.read(cx).label().to_string()).unwrap_or_default()
        });
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => this.finish_rename(true, window, cx),
            _ => {}
        });
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.renaming = Some(Renaming { tab: id, input, _subscription: subscription });
        cx.notify();
    }

    /// Takes the typed name (an empty one goes back to the automatic name),
    /// or, without `keep`, leaves the name as it was.
    fn finish_rename(&mut self, keep: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else { return };
        if keep {
            let typed = renaming.input.read(cx).value().trim().to_string();
            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == renaming.tab) {
                tab.name = (!typed.is_empty()).then_some(typed);
            }
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Moves a tab to where another one is, keeping the same tab active.
    fn move_tab(&mut self, id: usize, to: usize, cx: &mut Context<Self>) {
        let Some(from) = self.tabs.iter().position(|t| t.id == id) else { return };
        if from == to || to >= self.tabs.len() {
            return;
        }
        let active = self.tabs.get(self.active).map(|t| t.id);
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        if let Some(index) = active.and_then(|id| self.tabs.iter().position(|t| t.id == id)) {
            self.active = index;
        }
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
            self.note_active();
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    fn split_active(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        // With one row of tabs a file splits too: another file goes beside
        // it. Below the files the split keys stay with the terminal.
        if !Self::docked(cx) && self.tabs.get(self.active).is_some_and(|t| t.editor().is_some()) {
            self.split_editor(axis, cx);
            return;
        }
        self.turn_to_terminal(cx);
        let Some(tab) = self.tabs.get(self.active) else { return };
        if tab.panes().len() >= MAX_PANES {
            return;
        }
        let Some(source) = tab.active_pane().cloned() else { return };
        let cwd = source.read(cx).cwd().unwrap_or_else(|| self.root_dir.clone());
        // A split runs the same shell as the terminal it came from.
        let shell = source.read(cx).chosen_shell();
        let pane = self.spawn_pane_with(cwd, shell, window, cx);
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
        // Below the files each kind has its own row, and the keys stay in it.
        let terminal = self.tabs.get(self.active).is_some_and(Tab::is_terminal);
        let docked = Self::docked(cx);
        let row: Vec<usize> =
            (0..self.tabs.len()).filter(|&i| !docked || self.tabs[i].is_terminal() == terminal).collect();
        let Some(current) = row.iter().position(|&i| i == self.active) else { return };
        let next = row[(current as isize + step).rem_euclid(row.len() as isize) as usize];
        self.select_tab(next, window, cx);
    }

    fn cycle_pane(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.turn_to_terminal(cx);
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let panes = tab.panes();
        let Body::Terminals { active, .. } = &mut tab.body else { return };
        let current = panes.iter().position(|p| p == active).unwrap_or(0) as isize;
        let next = (current + step).rem_euclid(panes.len() as isize) as usize;
        *active = panes[next].clone();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Moves the keyboard to the terminal next to the active one: `across`
    /// for left and right, `forward` for right and down. Among several
    /// neighbours on that side, the one most level with this one.
    fn step_pane(&mut self, across: bool, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.turn_to_terminal(cx);
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Body::Terminals { root, active } = &mut tab.body else { return };
        let mut places = Vec::new();
        root.places([0., 0., 1., 1.], &mut places);
        let Some(&(_, here)) = places.iter().find(|(pane, _)| pane == active) else { return };
        // Edges along the way (near, far) and across it (low, high).
        let along = |at: [f32; 4]| if across { (at[0], at[2]) } else { (at[1], at[3]) };
        let beside = |at: [f32; 4]| if across { (at[1], at[3]) } else { (at[0], at[2]) };
        let (near, far) = along(here);
        let (low, high) = beside(here);
        let level = (low + high) / 2.;
        let next = places
            .iter()
            .filter(|(_, at)| {
                let touches = if forward { (along(*at).0 - far).abs() < 1e-4 } else { (along(*at).1 - near).abs() < 1e-4 };
                touches && beside(*at).0 < high - 1e-4 && beside(*at).1 > low + 1e-4
            })
            .min_by(|a, b| {
                let off = |at: [f32; 4]| ((beside(at).0 + beside(at).1) / 2. - level).abs();
                off(a.1).total_cmp(&off(b.1))
            });
        let Some((pane, _)) = next else { return };
        *active = pane.clone();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Closes a pane or a tab, asking first if that would kill a running program.
    fn request_close(&mut self, target: CloseTarget, window: &mut Window, cx: &mut Context<Self>) {
        if let CloseTarget::Tab(id) = target
            && self.tabs.iter().any(|t| t.id == id && t.editor().is_some())
        {
            self.close_files(id, None, window, cx);
            return;
        }
        // Nothing to lose in a tab of changes.
        if let CloseTarget::Tab(id) = target
            && self.tabs.iter().any(|t| t.id == id && matches!(t.body, Body::Diff(_) | Body::Changes(_)))
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

    fn remove_tab(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else { return };
        let tab = self.tabs.remove(index);
        for editor in tab.editors() {
            self.subscriptions.remove(&editor.entity_id());
        }
        if let Body::Changes(changes) = &tab.body {
            self.subscriptions.remove(&changes.entity_id());
        }
        self.settle(index, tab.is_terminal(), cx);
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
        let name = tab.name.clone();
        let Body::Terminals { root, active } = tab.body else { return };
        let was_active = active.entity_id() == id;
        match root.without(id) {
            Some(root) => {
                let mut panes = Vec::new();
                root.leaves(&mut panes);
                let active = if was_active { panes[0].clone() } else { active };
                self.tabs.insert(index, Tab { id: tab.id, name, body: Body::Terminals { root, active } });
            }
            None => self.settle(index, true, cx),
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

    /// Closes the terminal that has the keyboard, or the file that has it.
    fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        if let Body::Editor { second_active, .. } = &tab.body {
            self.close_files(tab.id, Some(*second_active), window, cx);
            return;
        }
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
        if let Some(id) = self.beside_dragging {
            if event.pressed_button != Some(MouseButton::Left) {
                self.beside_dragging = None;
                return;
            }
            let Some(Body::Editor { beside: Some(beside), .. }) = self.tabs.iter_mut().find(|t| t.id == id).map(|t| &mut t.body)
            else {
                return;
            };
            let bounds = beside.bounds.get();
            let ratio = match beside.axis {
                Axis::Horizontal if bounds.size.width > px(0.) => (event.position.x - bounds.origin.x) / bounds.size.width,
                Axis::Vertical if bounds.size.height > px(0.) => (event.position.y - bounds.origin.y) / bounds.size.height,
                _ => return,
            };
            beside.ratio = ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
            cx.notify();
            return;
        }
        if self.dock_dragging {
            let bounds = self.dock_bounds.get();
            if event.pressed_button != Some(MouseButton::Left) {
                self.dock_dragging = false;
            } else if bounds.size.height > px(0.) {
                let ratio = (event.position.y - bounds.origin.y) / bounds.size.height;
                self.dock_ratio = ratio.clamp(MIN_DOCK, 1.0 - MIN_DOCK);
                cx.notify();
            }
            return;
        }
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

    /// `alone` is a terminal that has its tab to itself: the cross on the tab
    /// closes it. Terminals that share a tab each get a cross of their own.
    fn render_node(&self, node: &Node, alone: bool, cx: &mut Context<Self>) -> AnyElement {
        let split = match node {
            Node::Leaf(pane) => {
                let id = pane.entity_id();
                return div()
                    .size_full()
                    .relative()
                    .child(pane.clone())
                    .when(!alone, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top(px(4.))
                                .right(px(16.))
                                .rounded_sm()
                                .bg(cx.theme().background.opacity(0.8))
                                // The press must not start a selection in the terminal underneath.
                                .occlude()
                                .child(
                                    Button::new(("close-terminal", id.as_u64() as usize))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Close)
                                        .tooltip(t("Close this terminal"))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                            this.request_close(CloseTarget::Pane(id), window, cx);
                                        })),
                                ),
                        )
                    })
                    .into_any_element();
            }
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
            .child(self.render_node(&split.first, false, cx));
        let second = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .child(self.render_node(&split.second, false, cx));
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

    fn render_tab_bar(&self, group: Group, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = match group {
            Group::All => Some(self.active),
            Group::Files => self.shown(false),
            Group::Terminals => self.shown(true),
        };
        let shown_id = shown.and_then(|index| self.tabs.get(index)).map(|tab| tab.id);
        let has_files = self.shown(false).is_some();
        let theme = cx.theme();
        let (border, muted, foreground, active_bg, hover_bg) = (
            theme.border,
            theme.muted_foreground,
            theme.foreground,
            theme.tab_active,
            theme.secondary_hover,
        );

        let in_group = |tab: &Tab| match group {
            Group::All => true,
            Group::Files => !tab.is_terminal(),
            Group::Terminals => tab.is_terminal(),
        };
        let tabs = self.tabs.iter().enumerate().filter(|(_, tab)| in_group(tab)).map(|(index, tab)| {
            let tab_id = tab.id;
            let selected = Some(index) == shown;
            let (label, picture, unsaved) = match &tab.body {
                Body::Terminals { active, .. } => {
                    let label = tab.name.clone().map(SharedString::from).unwrap_or_else(|| active.read(cx).label());
                    (label, None, false)
                }
                Body::Editor { first, beside, .. } => {
                    let name = first.read(cx).file_name();
                    let picture = file_icons::for_file(&name).and_then(crate::assets::file_icon);
                    let label = match beside {
                        Some(beside) => format!("{name} | {}", beside.editor.read(cx).file_name()),
                        None => name,
                    };
                    let unsaved = tab.editors().iter().any(|e| e.read(cx).is_dirty());
                    (SharedString::from(label), picture, unsaved)
                }
                Body::Diff(diff) => {
                    let diff = diff.read(cx);
                    let picture = file_icons::for_file(&diff.file_name()).and_then(crate::assets::file_icon);
                    (SharedString::from(diff.label()), picture, false)
                }
                Body::Changes(changes) => {
                    let label = match changes.read(cx).side() {
                        Side::Unstaged => t("All Changes"),
                        Side::Staged => t("All Staged Changes"),
                    };
                    (SharedString::from(label), crate::assets::file_icon("git"), false)
                }
            };
            let panes = tab.panes().len();
            let terminal = tab.is_terminal();
            let renaming = self.renaming.as_ref().filter(|r| r.tab == tab_id).map(|r| r.input.clone());
            let dragged = DraggedTab { id: tab_id, label: label.clone() };
            h_flex()
                .id(("tab", tab_id))
                .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(hover_bg))
                .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| this.move_tab(dragged.id, index, cx)))
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
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    // A double click on a terminal tab gives it a name.
                    if terminal && event.click_count() >= 2 {
                        this.start_rename(tab_id, window, cx);
                    } else {
                        this.select_tab(index, window, cx);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        this.request_close(CloseTarget::Tab(tab_id), window, cx);
                    }),
                )
                .when_some(picture, |d, picture| d.child(img(picture).flex_none().size(px(14.))))
                .child(match renaming {
                    Some(input) => div()
                        .flex_1()
                        .min_w(px(160.))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                this.finish_rename(false, window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .child(Input::new(&input).xsmall())
                        .into_any_element(),
                    None => div().flex_1().min_w_0().truncate().child(label).into_any_element(),
                })
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
        let on_file = match group {
            Group::All => self.tabs.get(self.active).is_some_and(|t| t.active_pane().is_none()),
            Group::Files => true,
            Group::Terminals => false,
        };
        // A file in the editor can have another one beside it.
        let on_editor = on_file && shown.and_then(|index| self.tabs.get(index)).is_some_and(|t| t.editor().is_some());
        let close = if on_file {
            button("close-file", IconName::Close, t("Close this file (Ctrl+Shift+W)"))
        } else {
            button("close-pane", IconName::Close, t("Close this terminal (Ctrl+Shift+W)"))
        };
        let close = close.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| match group {
            Group::All => this.close_active(window, cx),
            // The row's own tab, wherever the keyboard is.
            Group::Files => {
                if let Some(id) = shown_id {
                    this.request_close(CloseTarget::Tab(id), window, cx);
                }
            }
            Group::Terminals => {
                this.turn_to_terminal(cx);
                this.close_active(window, cx);
            }
        }));
        let area = cx.entity().downgrade();
        let new_tab = h_flex()
            .px_2()
            .flex_none()
            .child(
                button("new-tab", IconName::Plus, t("New terminal (Ctrl+Shift+T)"))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.add_tab(window, cx))),
            )
            .child(button("new-tab-shell", IconName::ChevronDown, t("New terminal with another shell")).dropdown_menu(
                move |menu, _, _| {
                    let mut menu = menu.label(t("Open a terminal with"));
                    for found in shell::available() {
                        let (area, setting) = (area.clone(), found.setting.clone());
                        menu = menu.item(PopupMenuItem::new(found.name).on_click(move |_, window, cx| {
                            let _ = area.update(cx, |area, cx| area.add_tab_with(Some(setting.clone()), window, cx));
                        }));
                    }
                    menu
                },
            ));
        h_flex()
            .h(px(TAB_BAR_HEIGHT))
            .flex_none()
            .w_full()
            .border_b_1()
            .border_color(border)
            .child(
                h_flex()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .children(tabs)
                    .when(group != Group::Files, |d| d.child(new_tab)),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .px_2()
                    .child(close)
                    .when(on_editor, |d| {
                        d.child(
                            button("split-editor-right", IconName::PanelRight, t("Open another file beside this one"))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.split_editor(Axis::Horizontal, cx))),
                        )
                        .child(
                            button("split-editor-down", IconName::PanelBottom, t("Open another file below this one"))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.split_editor(Axis::Vertical, cx))),
                        )
                    })
                    .when(!on_file, |d| {
                        d.child(
                            button("split-right", IconName::PanelRight, t("Split right (Ctrl+Shift+D)")).on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.split_active(Axis::Horizontal, window, cx)
                                }),
                            ),
                        )
                        .child(
                            button("split-down", IconName::PanelBottom, t("Split down (Ctrl+Shift+E)")).on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.split_active(Axis::Vertical, window, cx)
                                }),
                            ),
                        )
                    })
                    .when(group == Group::Files, |d| {
                        d.child(
                            button("toggle-terminal", IconName::PanelBottom, t("Show or hide the terminal (Ctrl+Shift+J)"))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    if this.terminal_hidden || this.shown(true).is_none() {
                                        this.toggle_terminal(window, cx);
                                    } else {
                                        this.hide_terminal(window, cx);
                                    }
                                })),
                        )
                    })
                    .when(group == Group::Terminals && has_files, |d| {
                        d.child(
                            button("hide-terminal", IconName::ChevronDown, t("Hide the terminal (Ctrl+Shift+J)")).on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| this.hide_terminal(window, cx)),
                            ),
                        )
                    }),
            )
    }

    /// One file, or two with a line to drag between them. A tab dropped on
    /// it puts its file beside this one.
    fn render_editors(&self, tab: &Tab, cx: &mut Context<Self>) -> AnyElement {
        let Body::Editor { first, beside, second_active } = &tab.body else { return div().into_any_element() };
        let tab_id = tab.id;
        let drop = cx.listener(move |this, dragged: &DraggedTab, window, cx| this.drop_beside(dragged.id, tab_id, window, cx));
        let Some(beside) = beside else {
            return div()
                .id(("editor-body", tab_id))
                .size_full()
                .drag_over::<DraggedTab>(|style, _, _, _| style.opacity(0.6))
                .on_drop(drop)
                .child(first.clone())
                .into_any_element();
        };
        let horizontal = beside.axis == Axis::Horizontal;
        let (ratio, bounds) = (beside.ratio, beside.bounds.clone());
        let theme = cx.theme();
        let (line, accent, background) = (theme.border, theme.primary, theme.background);
        let half = |editor: &Entity<EditorView>, second: bool, cx: &mut Context<Self>| {
            let active = second == *second_active;
            div()
                .relative()
                .size_full()
                .child(editor.clone())
                // A thin line over the file that has the keyboard.
                .when(active, |d| d.child(div().absolute().top_0().left_0().right_0().h(px(2.)).bg(accent)))
                .child(
                    div().absolute().top(px(4.)).right(px(16.)).rounded_sm().bg(background.opacity(0.8)).occlude().child(
                        Button::new(("close-half", tab_id * 2 + usize::from(second)))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip(t("Close this file"))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.close_files(tab_id, Some(second), window, cx)
                            })),
                    ),
                )
        };
        let first = div()
            .flex_none()
            .overflow_hidden()
            .map(|d| if horizontal { d.h_full().w(relative(ratio)) } else { d.w_full().h(relative(ratio)) })
            .child(half(first, false, cx));
        let second = div().flex_1().min_w_0().min_h_0().overflow_hidden().child(half(&beside.editor, true, cx));
        let divider = div()
            .id(("editor-divider", tab_id))
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
            .child(div().bg(line).map(|d| if horizontal { d.h_full().w(px(1.)) } else { d.w_full().h(px(1.)) }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.beside_dragging = Some(tab_id);
                    cx.stop_propagation();
                }),
            );
        div()
            .id(("editor-body", tab_id))
            .size_full()
            .relative()
            .flex()
            .when(!horizontal, |d| d.flex_col())
            .drag_over::<DraggedTab>(|style, _, _, _| style.opacity(0.6))
            .on_drop(drop)
            .child(canvas(move |laid_out, _, _| bounds.set(laid_out), |_, _, _, _| {}).absolute().size_full())
            .child(first)
            .child(divider)
            .child(second)
            .into_any_element()
    }

    /// What the tab at `index` shows under its tab bar.
    fn render_body(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        match self.tabs.get(index).map(|t| &t.body) {
            Some(Body::Terminals { root, .. }) => self.render_node(root, true, cx),
            Some(Body::Editor { .. }) => self.render_editors(&self.tabs[index], cx),
            Some(Body::Diff(diff)) => diff.clone().into_any_element(),
            Some(Body::Changes(changes)) => changes.clone().into_any_element(),
            None => div().into_any_element(),
        }
    }

    /// Files above, the terminal panel below, a line to drag between them.
    /// With no file open the terminals have the whole space.
    fn render_docked(&self, cx: &mut Context<Self>) -> AnyElement {
        let file = self.shown(false);
        let terminal = self.shown(true).filter(|_| file.is_none() || !self.terminal_hidden);
        let both = file.is_some() && terminal.is_some();
        let (ratio, bounds) = (self.dock_ratio, self.dock_bounds.clone());
        let (line, background) = (cx.theme().border, cx.theme().background);

        let files = file.map(|index| {
            v_flex()
                .id("file-dock")
                .test_support()
                .w_full()
                .min_h_0()
                .map(|d| if both { d.flex_none().h(relative(ratio)) } else { d.flex_1() })
                .child(self.render_tab_bar(Group::Files, cx))
                .child(div().flex_1().min_h_0().w_full().child(self.render_body(index, cx)))
        });
        let divider = both.then(|| {
            div()
                .id("dock-divider")
                .test_support()
                .flex_none()
                .w_full()
                .h(px(DIVIDER))
                .flex()
                .flex_col()
                .justify_center()
                .cursor_row_resize()
                .bg(background)
                .child(div().w_full().h(px(1.)).bg(line))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.dock_dragging = true;
                        cx.stop_propagation();
                    }),
                )
        });
        let terminals = terminal.map(|index| {
            v_flex()
                .id("terminal-dock")
                .test_support()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(self.render_tab_bar(Group::Terminals, cx))
                .child(div().flex_1().min_h_0().w_full().child(self.render_body(index, cx)))
        });
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .child(canvas(move |laid_out, _, _| bounds.set(laid_out), |_, _, _, _| {}).absolute().size_full())
            .children(files)
            .children(divider)
            .children(terminals)
            .into_any_element()
    }

    fn render_confirm(&self, busy: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let text = tn(
            busy as u64,
            "A program is still running in this terminal. Closing it will stop the program.",
            "Programs are still running in {n} terminals. Closing them will stop the programs.",
            &[],
        );
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
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(t("Close terminal?")))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(text))
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .child(Button::new("confirm-cancel").small().label(t("Cancel")).on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.answer_confirm(false, window, cx)
                                }),
                            ))
                            .child(Button::new("confirm-close").small().danger().label(t("Close")).on_click(
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
        // And whichever of two files was clicked has the keyboard in its tab.
        for tab in &mut self.tabs {
            if let Body::Editor { first, beside: Some(beside), second_active } = &mut tab.body {
                if beside.editor.focus_handle(cx).contains_focused(window, cx) {
                    *second_active = true;
                } else if first.focus_handle(cx).contains_focused(window, cx) {
                    *second_active = false;
                }
            }
        }

        // With two tabs on screen, the one that was clicked into is the active one.
        let docked = Self::docked(cx);
        if docked {
            for terminals in [false, true] {
                if let Some(index) = self.shown(terminals)
                    && index != self.active
                    && self.tabs[index].has_focus(window, cx)
                {
                    self.active = index;
                }
            }
        }
        self.note_active();

        let content = if docked {
            self.render_docked(cx)
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(self.render_tab_bar(Group::All, cx))
                .child(div().flex_1().min_h_0().w_full().child(self.render_body(self.active, cx)))
                .into_any_element()
        };
        // While a question is on screen the shortcuts below must not act behind it.
        let idle = self.confirm.is_none();

        v_flex()
            .id("terminal-area")
            .test_support()
            .key_context("TerminalArea")
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .on_mouse_move(cx.listener(Self::on_drag))
            // A file dragged here from the tree, or from another program, is
            // typed into the terminal as a path.
            .on_drop(cx.listener(|this, dragged: &DraggedPath, window, cx| {
                this.insert_path(&dragged.0, window, cx);
            }))
            .on_drop(cx.listener(|this, dragged: &ExternalPaths, window, cx| {
                for path in dragged.paths() {
                    this.insert_path(path, window, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| {
                    this.dragging = None;
                    this.dock_dragging = false;
                    this.beside_dragging = None;
                }),
            )
            .on_action(cx.listener(|this, _: &SplitEditorRight, _, cx| this.split_editor(Axis::Horizontal, cx)))
            .on_action(cx.listener(|this, _: &SplitEditorDown, _, cx| this.split_editor(Axis::Vertical, cx)))
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
            .on_action(cx.listener(move |this, _: &PaneLeft, window, cx| {
                if idle {
                    this.step_pane(true, false, window, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &PaneRight, window, cx| {
                if idle {
                    this.step_pane(true, true, window, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &PaneUp, window, cx| {
                if idle {
                    this.step_pane(false, false, window, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &PaneDown, window, cx| {
                if idle {
                    this.step_pane(false, true, window, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &ToggleTerminal, window, cx| {
                if idle {
                    this.toggle_terminal(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &RenameTab, window, cx| {
                this.turn_to_terminal(cx);
                if let Some(tab) = this.tabs.get(this.active).filter(|t| t.is_terminal()) {
                    let id = tab.id;
                    this.start_rename(id, window, cx);
                }
            }))
            // From the palette: the terminal shown in the terminal row.
            .on_action(cx.listener(|this, _: &Find, window, cx| {
                this.turn_to_terminal(cx);
                let pane = this.tabs.get(this.active).and_then(|tab| tab.active_pane().cloned());
                if let Some(pane) = pane {
                    pane.update(cx, |pane, cx| pane.open_find(window, cx));
                }
            }))
            .child(content)
            .when_some(self.confirm, |d, (_, busy)| d.child(self.render_confirm(busy, cx)))
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, ClipboardItem, Entity, Image, ImageFormat, TestAppContext, WindowOptions,
    };
    use wercode_core::config::Settings;

    use super::TerminalArea;

    fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<TerminalArea>) {
        // The terminals start real shells, whose reader threads park.
        cx.executor().allow_parking();
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
        cx.executor().allow_parking();
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

        // The arrows go to the terminal on that side: left | top over bottom.
        let panes = area.read_with(cx, |a, _| a.tabs[0].panes());
        let active = |cx: &mut TestAppContext| area.read_with(cx, |a, _| a.tabs[0].active_pane().unwrap().clone());
        assert_eq!(active(cx), panes[2]);
        press(cx, window, &["ctrl-alt-up"]);
        assert_eq!(active(cx), panes[1]);
        press(cx, window, &["ctrl-alt-up", "ctrl-alt-right"]);
        assert_eq!(active(cx), panes[1], "nothing above or to the right");
        press(cx, window, &["ctrl-alt-left"]);
        assert_eq!(active(cx), panes[0]);
        press(cx, window, &["ctrl-alt-right", "ctrl-alt-down"]);
        assert_eq!(active(cx), panes[2]);

        // Moving between panes changes which one a close removes.
        let before = area.read_with(cx, |a, _| a.tabs[0].active_pane().unwrap().entity_id());
        press(cx, window, &["ctrl-alt-left"]);
        let after = area.read_with(cx, |a, _| a.tabs[0].active_pane().unwrap().entity_id());
        assert_ne!(before, after);
        press(cx, window, &["ctrl-shift-w"]);
        assert_eq!(shape(cx, &area), (1, 0, 2));
        let panes = area.read_with(cx, |a, _| a.tabs[0].panes());
        assert!(panes.iter().all(|p| p.entity_id() != after));

        // Each of several terminals has a cross of its own, whichever has the keyboard.
        press(cx, window, &["ctrl-shift-d"]);
        assert_eq!(shape(cx, &area), (1, 0, 3));
        let panes = area.read_with(cx, |a, _| a.tabs[0].panes());
        let (first, active) = (panes[0].entity_id(), area.read_with(cx, |a, _| a.tabs[0].active_pane().unwrap().entity_id()));
        assert_ne!(first, active);
        cx.update_window(window, |_, window, cx| window.click(("close-terminal", first.as_u64() as usize), cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(shape(cx, &area), (1, 0, 2));
        assert!(area.read_with(cx, |a, _| a.tabs[0].panes()).iter().all(|p| p.entity_id() != first));

        // The centre is never left empty.
        press(cx, window, &["ctrl-shift-q"]);
        assert_eq!(shape(cx, &area), (1, 0, 1));
    }

    #[gpui_kit::test]
    async fn a_pasted_picture_is_typed_as_the_path_of_a_png(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (window, area) = open(cx);
        // A 1×1 bitmap, the kind a screenshot often is on Windows.
        let bitmap: Vec<u8> = [
            &b"BM"[..], &58u32.to_le_bytes(), &[0; 4], &54u32.to_le_bytes(), &40u32.to_le_bytes(),
            &1i32.to_le_bytes(), &1i32.to_le_bytes(), &1u16.to_le_bytes(), &24u16.to_le_bytes(),
            &[0; 24], &[0, 0, 255, 0],
        ]
        .concat();
        cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Bmp, bitmap))));
        press(cx, window, &["ctrl-shift-v"]);

        let screen = |cx: &mut TestAppContext| {
            area.read_with(cx, |a, cx| a.tabs[0].active_pane().unwrap().read(cx).screen_text())
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let path = loop {
            cx.run_until_parked();
            let text = screen(cx);
            // While the path is still being drawn, PowerShell may finish it
            // in grey with one from its history, a picture already deleted:
            // only a file that is there counts.
            if let Some(start) = text.find("wercode-pasted")
                && let Some(end) = text[start..].find(".png")
            {
                let typed = text[..start + end + 4].rsplit(' ').next().unwrap();
                let path = std::path::PathBuf::from(typed.trim_matches('"'));
                if path.is_file() {
                    break path;
                }
            }
            assert!(std::time::Instant::now() < deadline, "the path never showed: {text:?}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let saved = std::fs::read(&path).unwrap();
        assert!(saved.starts_with(b"\x89PNG"), "saved as PNG: {}", path.display());
        let _ = std::fs::remove_file(path);
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
    async fn a_picture_opens_as_a_picture(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (window, area) = open(cx);
        let dir = std::env::temp_dir().join(format!("wercode-picture-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dot.png");
        // A 1x1 PNG.
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 0x1F, 0x15, 0xC4, 0x89, 0, 0, 0, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF,
            0xC0, 0xF0, 0x1F, 0, 0x05, 0x00, 0x01, 0xFF, 0x89, 0x99, 0x3D, 0x1D, 0, 0, 0, 0, 0x49, 0x45, 0x4E, 0x44,
            0xAE, 0x42, 0x60, 0x82,
        ];
        std::fs::write(&path, png).unwrap();
        cx.update_window(window, |_, window, cx| {
            area.update(cx, |a, cx| a.open_path(path.clone(), None, None, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        let editor = area.read_with(cx, |a, _| a.tabs[1].editor().unwrap().clone());
        assert!(editor.read_with(cx, |e, _| e.has_picture()));
        assert!(!editor.read_with(cx, |e, _| e.is_dirty()));
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
        let TabLayout::Terminals { panes, .. } = &layout.tabs[0] else { panic!("{layout:?}") };
        assert_eq!(panes.terminals(), 3);
        // Split right, then the right half split down.
        let PaneLayout::Split { stacked: false, second, .. } = panes else { panic!("{panes:?}") };
        assert!(matches!(**second, PaneLayout::Split { stacked: true, .. }));
        assert_eq!(layout.tabs[2], TabLayout::File { path: file.clone(), beside: None });

        // A layout from the state file: a folder and a file that are gone are
        // left out, a broken ratio is repaired, and no tab gets a fifth terminal.
        let terminal = |cwd: Option<std::path::PathBuf>| Box::new(PaneLayout::Terminal { cwd, shell: None });
        let split = |stacked, ratio, first, second| Box::new(PaneLayout::Split { stacked, ratio, first, second });
        let crowded = split(
            false,
            f32::NAN,
            split(true, 0.25, terminal(Some(dir.join("sub"))), terminal(Some(dir.join("gone")))),
            split(true, 5.0, terminal(None), split(false, 0.5, terminal(None), terminal(None))),
        );
        let saved = Layout {
            dock_ratio: None,
            terminal_hidden: false,
            tabs: vec![
                TabLayout::File { path: dir.join("gone.txt"), beside: None },
                TabLayout::Terminals { panes: *crowded, name: None },
                TabLayout::File { path: file.clone(), beside: None },
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
        assert_eq!(layout.tabs[1], TabLayout::File { path: file.clone(), beside: None });
        let TabLayout::Terminals { panes, .. } = &layout.tabs[0] else { panic!("{layout:?}") };
        assert_eq!(panes.terminals(), 4);
        let PaneLayout::Split { stacked: false, ratio, first, second } = panes else { panic!("{panes:?}") };
        assert_eq!(*ratio, 0.5);
        assert!(matches!(**first, PaneLayout::Split { stacked: true, ratio, .. } if ratio == 0.25));
        assert!(matches!(**second, PaneLayout::Split { stacked: true, ratio, .. } if ratio == 0.9));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    async fn two_files_share_a_tab(cx: &mut TestAppContext) {
        use super::{AreaEvent, Axis, Cell, Context, Rc, Window};
        use wercode_core::state::{Beside as Saved, Layout, TabLayout};
        let (window, area) = open(cx);
        let dir = std::env::temp_dir().join(format!("wercode-beside-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let [a, b, c] = ["a.txt", "b.txt", "c.txt"].map(|name| {
            std::fs::write(dir.join(name), format!("{name}\n")).unwrap();
            dir.join(name)
        });
        let run = |cx: &mut TestAppContext, f: &dyn Fn(&mut TerminalArea, &mut Window, &mut Context<TerminalArea>)| {
            cx.update_window(window, |_, window, cx| area.update(cx, |area, cx| f(area, window, cx))).unwrap();
            cx.run_until_parked();
        };
        let tabs = |cx: &mut TestAppContext| area.read_with(cx, |a, cx| a.layout(cx).tabs);
        let shown = |cx: &mut TestAppContext| area.read_with(cx, |a, cx| a.shown_file(cx));
        let file = |path: &std::path::PathBuf, beside: Option<(&std::path::PathBuf, bool)>| TabLayout::File {
            path: path.clone(),
            beside: beside.map(|(path, stacked)| Saved { path: path.clone(), stacked, ratio: 0.5 }),
        };

        run(cx, &|area, window, cx| {
            area.open_path(b.clone(), None, None, window, cx);
            area.open_path(a.clone(), None, None, window, cx);
        });
        assert_eq!(tabs(cx).len(), 3);
        // A file with a tab of its own moves over beside the one showing, and has the keyboard.
        run(cx, &|area, window, cx| area.open_beside(b.clone(), Axis::Horizontal, window, cx));
        assert_eq!(tabs(cx), [tabs(cx)[0].clone(), file(&a, Some((&b, false)))]);
        assert_eq!(shown(cx), Some(b.clone()));
        // Another file beside takes the place; the one before gets its tab back.
        run(cx, &|area, window, cx| area.open_beside(c.clone(), Axis::Vertical, window, cx));
        assert_eq!(tabs(cx)[1..], [file(&a, Some((&c, true))), file(&b, None)]);
        // Opening a file of the pair shows that half.
        run(cx, &|area, window, cx| area.open_path(a.clone(), None, None, window, cx));
        assert_eq!(shown(cx), Some(a.clone()));
        assert_eq!(area.read_with(cx, |a, _| a.active), 1);

        // The split keys in a file ask the window for a file to put beside it.
        let picked = Rc::new(Cell::new(None));
        let seen = picked.clone();
        let _watch = cx.update(|cx| {
            cx.subscribe(&area, move |_, event: &AreaEvent, _| {
                if let AreaEvent::PickBeside(axis) = event {
                    seen.set(Some(*axis));
                }
            })
        });
        press(cx, window, &["ctrl-shift-e"]);
        assert_eq!(picked.get(), Some(Axis::Vertical));
        press(cx, window, &["ctrl-shift-d"]);
        assert_eq!(picked.get(), Some(Axis::Horizontal));

        // Closing the file with the keyboard leaves the other one the whole tab.
        press(cx, window, &["ctrl-shift-w"]);
        assert_eq!(tabs(cx)[1..], [file(&c, None), file(&b, None)]);
        // A tab dropped on a file goes beside it.
        let (b_tab, c_tab) = area.read_with(cx, |a, _| (a.tabs[2].id, a.tabs[1].id));
        run(cx, &|area, window, cx| area.drop_beside(b_tab, c_tab, window, cx));
        assert_eq!(tabs(cx)[1..], [file(&c, Some((&b, false)))]);

        // Written down and put back the same way; a second file that is gone is left out.
        let saved = Layout { tabs: vec![file(&a, Some((&c, true))), file(&b, Some((&dir.join("gone.txt"), false)))], ..Layout::default() };
        let root = dir.clone();
        let (_, restored) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| TerminalArea::new(root, Some(&saved), window, cx))
            })
            .unwrap()
        });
        assert_eq!(restored.read_with(cx, |a, cx| a.layout(cx).tabs), [file(&a, Some((&c, true))), file(&b, None)]);
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

    #[gpui_kit::test]
    async fn files_sit_above_the_terminal_when_the_settings_say_so(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (window, area) = open(cx);
        cx.update(|cx| crate::theme::change_settings(cx, |s| s.layout = "terminal_below".into()).unwrap());
        let dir = std::env::temp_dir().join(format!("wercode-dock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.txt", "b.txt"] {
            std::fs::write(dir.join(name), "text\n").unwrap();
        }
        let on_screen = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (window.try_find("file-dock").is_some(), window.try_find("terminal-dock").is_some())
            })
            .unwrap()
        };
        let open_file = |cx: &mut TestAppContext, name: &str| {
            cx.update_window(window, |_, window, cx| {
                area.update(cx, |a, cx| a.open_path(dir.join(name), None, None, window, cx));
            })
            .unwrap();
            cx.run_until_parked();
        };
        // No file open: the terminals have the whole space.
        assert_eq!(on_screen(cx), (false, true));

        open_file(cx, "a.txt");
        open_file(cx, "b.txt");
        assert_eq!(on_screen(cx), (true, true));
        assert_eq!(shape(cx, &area), (3, 2, 0));
        // The keys move along the row the keyboard is in, not into the other one.
        press(cx, window, &["ctrl-tab"]);
        assert_eq!(area.read_with(cx, |a, _| a.active), 1);
        press(cx, window, &["ctrl-tab"]);
        assert_eq!(area.read_with(cx, |a, _| a.active), 2);

        // A split asked for from a file happens in the terminal below.
        press(cx, window, &["ctrl-shift-d"]);
        assert_eq!(shape(cx, &area), (3, 0, 2));
        // From the terminal the panel is put away, and comes back with the keyboard in it.
        press(cx, window, &["ctrl-shift-j"]);
        assert_eq!(on_screen(cx), (true, false));
        assert_eq!(area.read_with(cx, |a, _| a.active), 2);
        press(cx, window, &["ctrl-shift-j"]);
        assert_eq!(on_screen(cx), (true, true));
        assert_eq!(area.read_with(cx, |a, _| a.active), 0);

        // A closed file gives way to the other file; with none left the terminal is alone again.
        cx.update_window(window, |_, window, cx| window.click("close-file", cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(on_screen(cx), (true, true));
        assert_eq!(area.read_with(cx, |a, _| a.tabs.len()), 2);
        cx.update_window(window, |_, window, cx| window.click("close-file", cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(on_screen(cx), (false, true));

        // Back to one row of tabs, at once.
        open_file(cx, "a.txt");
        cx.update(|cx| crate::theme::change_settings(cx, |s| s.layout = "tabs".into()).unwrap());
        assert_eq!(on_screen(cx), (false, false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    async fn tabs_are_renamed_moved_and_opened_with_another_shell(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (window, area) = open(cx);
        let names = |cx: &mut TestAppContext| {
            area.read_with(cx, |a, _| a.tabs.iter().map(|t| t.name.clone()).collect::<Vec<_>>())
        };

        // The palette's Rename Tab puts a field on the tab; Enter keeps the name.
        cx.update_window(window, |_, window, cx| {
            area.update(cx, |a, cx| a.focus_active(window, cx));
            window.dispatch_action(Box::new(super::RenameTab), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(area.read_with(cx, |a, _| a.renaming.is_some()));
        let input = area.read_with(cx, |a, _| a.renaming.as_ref().unwrap().input.clone());
        cx.update_window(window, |_, window, cx| {
            input.update(cx, |input, cx| input.set_value("server", window, cx));
        })
        .unwrap();
        press(cx, window, &["enter"]);
        assert_eq!(names(cx), [Some("server".to_string())]);
        assert!(area.read_with(cx, |a, _| a.renaming.is_none()));
        // The name is written down and comes back with the tab.
        let layout = area.read_with(cx, |a, cx| a.layout(cx));
        let wercode_core::state::TabLayout::Terminals { name, .. } = &layout.tabs[0] else { panic!("{layout:?}") };
        assert_eq!(name.as_deref(), Some("server"));

        // A tab with another shell; a split of it runs the same shell.
        let shell = wercode_core::shell::resolve("auto", &[]).program.to_string_lossy().into_owned();
        cx.update_window(window, |_, window, cx| {
            area.update(cx, |a, cx| a.add_tab_with(Some(shell.clone()), window, cx));
        })
        .unwrap();
        press(cx, window, &["ctrl-shift-d"]);
        let chosen = area.read_with(cx, |a, cx| {
            a.tabs[1].panes().iter().map(|p| p.read(cx).chosen_shell()).collect::<Vec<_>>()
        });
        assert_eq!(chosen, [Some(shell.clone()), Some(shell.clone())]);

        // Dragging the second tab before the first keeps it the active one.
        let second = area.read_with(cx, |a, _| a.tabs[1].id);
        area.update(cx, |a, cx| a.move_tab(second, 0, cx));
        assert_eq!(names(cx), [None, Some("server".to_string())]);
        assert_eq!(area.read_with(cx, |a, _| (a.active, a.tabs[0].id)), (0, second));

        // The find field opens over the terminal with the keyboard in it.
        press(cx, window, &["ctrl-shift-s"]);
        let pane = area.read_with(cx, |a, _| a.tabs[0].active_pane().cloned().unwrap());
        assert!(pane.read_with(cx, |p, _| p.find_is_open()));
        press(cx, window, &["escape"]);
        assert!(!pane.read_with(cx, |p, _| p.find_is_open()));
    }

    /// Waits, with real time passing, until `done` holds.
    fn wait_until(cx: &mut TestAppContext, what: &str, mut done: impl FnMut(&mut TestAppContext) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "{what} never happened");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// The Windows console takes Shift+Enter as a whole Ctrl+J press, on
    /// which PowerShell (like Claude Code and Codex) starts a new line of the
    /// same command instead of running it.
    #[cfg(windows)]
    #[gpui_kit::test]
    async fn shift_enter_continues_the_command_on_a_new_line(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (window, area) = open(cx);
        let pane = area.read_with(cx, |a, _| a.tabs[0].active_pane().unwrap().clone());
        let screen = |cx: &mut TestAppContext| pane.read_with(cx, |p, _| p.screen_text());
        wait_until(cx, "the shell asking for the key presses of the console", |cx| {
            pane.read_with(cx, |p, _| p.win32_input())
        });
        wait_until(cx, "the prompt", |cx| screen(cx).contains("PS "));
        cx.update_window(window, |_, window, cx| window.input("echo one", cx)).unwrap();
        press(cx, window, &["shift-enter"]);
        cx.update_window(window, |_, window, cx| window.input("echo two", cx)).unwrap();
        wait_until(cx, "a second line of the same command", |cx| screen(cx).contains(">> echo two"));
        let shown = screen(cx);
        assert!(shown.find("echo one").unwrap() < shown.find(">> echo two").unwrap(), "in order: {shown:?}");
        assert_eq!(shown.matches("PS ").count(), 1, "nothing ran yet: {shown:?}");
    }

    /// Cmd keys belong to the app, as in every Mac terminal: Cmd+V pastes
    /// and no Cmd key is typed into the shell.
    #[gpui_kit::test]
    async fn cmd_v_pastes_and_cmd_keys_are_not_typed(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (window, area) = open(cx);
        let pane = area.read_with(cx, |a, _| a.tabs[0].active_pane().unwrap().clone());
        let screen = |cx: &mut TestAppContext| pane.read_with(cx, |p, _| p.screen_text());
        cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("wercode-cmd-paste".into())));
        press(cx, window, &["cmd-k", "cmd-v"]);
        wait_until(cx, "the paste", |cx| screen(cx).contains("wercode-cmd-paste"));
        assert!(!screen(cx).contains("kwercode"), "Cmd+K typed nothing: {:?}", screen(cx));
    }
}
