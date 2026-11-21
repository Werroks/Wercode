//! The window's root view: title bar with the project switcher, terminals in
//! the centre, a side panel (files, search, git) and a status bar.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::input::{GoToDefinition, Input, InputEvent, InputState, ToggleCodeActions};
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wercode_core::finder;
use wercode_core::fs_ops;
use wercode_core::git::{self, RepoStatus};
use wercode_core::state::{self, Layout, State};
use wercode_core::tasks::{self, Task as ProjectTask};
use wercode_core::watch::{self, Watch};

use crate::editor::{RenameSymbol, Save, SplitEditorDown, SplitEditorRight};
use crate::symbols::{GoToProjectSymbol, GoToSymbol, SymbolPicker, SymbolsEvent};
use crate::lsp::RestartLanguageServers;
use crate::panel::git as git_panel;
use crate::panel::problems::severity_icon;
use crate::panel::{
    FilesEvent, FilesPanel, GitPanel, GitPanelEvent, ProblemsEvent, ProblemsPanel, Remote, RemoteActivity,
    SearchEvent, SearchPanel,
};
use crate::settings::{SettingsEvent, SettingsView};
use crate::term::{
    ClosePane, CloseTab, Find, NewTab, RenameTab, NextPane, NextTab, PrevPane, PrevTab, SplitDown, SplitRight,
    SwitchTab, SwitchTabBack, TerminalArea, ToggleTerminal,
};
use crate::term::area::AreaEvent;
use crate::theme::AppState;
use crate::update::{self, CheckForUpdates, CloseForUpdate};
use wercode_core::i18n::{t, tf, tn};
use wercode_core::lsp::{ProjectGuard, Severity, Status};

actions!(
    workspace,
    [
        TogglePanel,
        OpenFolder,
        ShowFiles,
        ShowSearch,
        ShowGit,
        SwitchProject,
        CommandPalette,
        NewWindow,
        OpenSettings,
        SwitchBranch,
        /// Lists the folders checked out from the repository, to switch to one or make one.
        SwitchWorktree,
        QuickOpen,
        ShowProblems,
        /// Shows the commands the project describes, to run one in a terminal tab.
        RunTask,
        /// Runs again the task that was run last.
        RerunTask,
        /// Hides the title bar, the side panel and the status bar and fills
        /// the screen with the tabs (on Linux the window keeps its size), or
        /// brings everything back.
        ToggleZen
    ]
);

/// How long the note on how to leave zen mode stays up.
const ZEN_NOTE: Duration = Duration::from_secs(4);

/// Zen mode, while it is on.
struct Zen {
    /// Wercode made the window fill the screen, and puts it back after.
    fullscreen: bool,
    /// Takes the note on how to leave away after a while.
    note: Option<Task<()>>,
}

/// How often the repository status is read again while the window is active.
const GIT_POLL: Duration = Duration::from_secs(2);
/// Resizing produces a burst of changes; the state file is written once after it.
const SAVE_DELAY: Duration = Duration::from_millis(400);

/// How many files the list for opening a file by name shows at once.
const FINDER_ROWS: usize = 50;

/// The workspace shortcuts are bound with the others, in `keymap`.
pub fn init(_: &mut App) {}

/// Opens a window on `root`, or on the welcome screen without one. Every
/// window keeps its own project, terminals and files.
pub fn open_window(
    root: Option<PathBuf>,
    state: State,
    state_path: Option<PathBuf>,
    bounds: Option<WindowBounds>,
    cx: &mut App,
) {
    let options = WindowOptions {
        window_min_size: Some(size(px(640.), px(400.))),
        window_bounds: bounds,
        ..TitleBar::window_options()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Workspace::new(root, state, state_path, window, cx))
    })
    .expect("failed to open window");
}

/// Makes a path absolute and drops the Windows `\\?\` prefix, which shells
/// and git do not expect.
pub fn normalize(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(full) => {
            let text = full.to_string_lossy();
            match text.strip_prefix(r"\\?\") {
                Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
                _ => full.clone(),
            }
        }
        Err(_) => path.to_path_buf(),
    }
}

/// Where a new window opens: four fifths of the size of `current`, a step
/// right of and below its centre, so the window under it still shows.
fn beside(current: Bounds<Pixels>) -> Bounds<Pixels> {
    let width = (current.size.width * 0.8).max(px(640.));
    let height = (current.size.height * 0.8).max(px(400.));
    let step = px(32.);
    let x = current.origin.x + (current.size.width - width) / 2. + step;
    let y = current.origin.y + (current.size.height - height) / 2. + step;
    Bounds::new(point(x, y), size(width, height))
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelKind {
    Files,
    Search,
    Git,
    Problems,
}

impl PanelKind {
    fn name(self) -> &'static str {
        match self {
            PanelKind::Files => "files",
            PanelKind::Search => "search",
            PanelKind::Git => "git",
            PanelKind::Problems => "problems",
        }
    }

    fn from_name(name: &str) -> Self {
        match name {
            "search" => PanelKind::Search,
            "git" => PanelKind::Git,
            "problems" => PanelKind::Problems,
            _ => PanelKind::Files,
        }
    }
}

/// Everything in the palette: label and the action it runs. The shortcut shown
/// next to each one comes from the key bindings.
fn palette_commands() -> Vec<(&'static str, Box<dyn Action>)> {
    vec![
        (t("Terminal: Show or Hide"), Box::new(ToggleTerminal)),
        (t("Terminal: New Tab"), Box::new(NewTab)),
        (t("Terminal: Split Right"), Box::new(SplitRight)),
        (t("Terminal: Split Down"), Box::new(SplitDown)),
        (t("Terminal: Close Pane"), Box::new(ClosePane)),
        (t("Terminal: Close Tab"), Box::new(CloseTab)),
        (t("Terminal: Next Tab"), Box::new(NextTab)),
        (t("Terminal: Previous Tab"), Box::new(PrevTab)),
        (t("View: Switch Tab\u{2026}"), Box::new(SwitchTab)),
        (t("Terminal: Next Pane"), Box::new(NextPane)),
        (t("Terminal: Previous Pane"), Box::new(PrevPane)),
        (t("Terminal: Find\u{2026}"), Box::new(Find)),
        (t("Terminal: Rename Tab\u{2026}"), Box::new(RenameTab)),
        (t("File: Go to File\u{2026}"), Box::new(QuickOpen)),
        (t("File: Save"), Box::new(Save)),
        (t("Editor: Go to Definition"), Box::new(GoToDefinition)),
        (t("Editor: Quick Fix\u{2026}"), Box::new(ToggleCodeActions)),
        (t("Editor: Rename Symbol\u{2026}"), Box::new(RenameSymbol)),
        (t("Editor: Open a File to the Side\u{2026}"), Box::new(SplitEditorRight)),
        (t("Editor: Open a File Below\u{2026}"), Box::new(SplitEditorDown)),
        (t("Go to Symbol in File\u{2026}"), Box::new(GoToSymbol)),
        (t("Go to Symbol in Project\u{2026}"), Box::new(GoToProjectSymbol)),
        (t("View: Toggle Side Panel"), Box::new(TogglePanel)),
        (t("View: Zen Mode"), Box::new(ToggleZen)),
        (t("View: Show Files"), Box::new(ShowFiles)),
        (t("Search: Find in Files"), Box::new(ShowSearch)),
        (t("View: Show Git"), Box::new(ShowGit)),
        (t("View: Show Problems"), Box::new(ShowProblems)),
        (t("Language Servers: Restart"), Box::new(RestartLanguageServers)),
        (t("Tasks: Run\u{2026}"), Box::new(RunTask)),
        (t("Tasks: Run the Last One Again"), Box::new(RerunTask)),
        (t("Window: New Window"), Box::new(NewWindow)),
        (t("Project: Open Folder\u{2026}"), Box::new(OpenFolder)),
        (t("Project: Switch to Recent\u{2026}"), Box::new(SwitchProject)),
        (t("Preferences: Settings"), Box::new(OpenSettings)),
        (t("Git: Switch or Create Branch\u{2026}"), Box::new(SwitchBranch)),
        (t("Git: Switch or Create Worktree\u{2026}"), Box::new(SwitchWorktree)),
        (t("Git: Pull"), Box::new(git_panel::Pull)),
        (t("Git: Push"), Box::new(git_panel::Push)),
        (t("Git: Fetch"), Box::new(git_panel::Fetch)),
        (t("Git: Commit"), Box::new(git_panel::Commit)),
        (t("Git: Stage All Changes"), Box::new(git_panel::StageAll)),
        (t("Git: Show All Changes"), Box::new(git_panel::ShowAllChanges)),
        (t("Help: Check for Updates"), Box::new(CheckForUpdates)),
    ]
}

/// An open folder with everything that belongs to it.
struct Project {
    root: PathBuf,
    terminals: Entity<TerminalArea>,
    files: Entity<FilesPanel>,
    search: Entity<SearchPanel>,
    changes: Entity<GitPanel>,
    problems: Entity<ProblemsPanel>,
    /// `None` outside a repository, and until the first read finishes.
    git: Option<Arc<RepoStatus>>,
    /// Keeps the language servers of the project running.
    _servers: Option<ProjectGuard>,
    _subscriptions: [Subscription; 7],
    _git_poll: Task<()>,
    /// Reports files changed by other programs. `None` when the system would
    /// not watch the folder; the git poll then still notices new files.
    _watch: Option<Watch>,
    _watch_task: Task<()>,
}

/// The list of branches over the status bar.
struct BranchMenu {
    /// Local branches; empty until git has listed them.
    names: Vec<String>,
    /// Branches only a remote has, as `origin/name`.
    remote: Vec<String>,
    /// The name for a new branch.
    input: Entity<InputState>,
    error: Option<String>,
    /// Git is switching; the list waits.
    busy: bool,
    _subscription: Subscription,
}

/// The list of the repository's worktrees over the status bar.
struct WorktreeMenu {
    /// `None` until git has listed them.
    trees: Option<Vec<git::Worktree>>,
    /// The branch for a new worktree.
    input: Entity<InputState>,
    error: Option<String>,
    /// Git is making or removing one; the list waits.
    busy: bool,
    _subscription: Subscription,
}

/// The list for opening a file by a few letters of its name.
struct FileFinder {
    state: Entity<CommandState>,
    /// The chosen file goes beside the file showing, this way.
    beside: Option<Axis>,
    /// Paths inside the project, with forward slashes. `None` until the
    /// folder has been read.
    files: Option<Arc<Vec<String>>>,
    /// The files that match what is typed, best first, as indexes into `files`.
    shown: Vec<usize>,
    /// Stops the reading of the folder when the list is closed early.
    cancel: Arc<AtomicBool>,
}

/// The list of what the project can run.
struct TaskList {
    state: Entity<CommandState>,
    /// `None` until the project's files have been read.
    tasks: Option<Vec<ProjectTask>>,
    /// The tasks that match what is typed, as indexes into `tasks`.
    shown: Vec<usize>,
}

pub struct Workspace {
    /// `None` shows the welcome screen.
    project: Option<Project>,
    panel: PanelKind,
    panel_open: bool,
    /// The list of recent projects under the title is showing.
    switcher_open: bool,
    palette: Option<Entity<CommandState>>,
    finder: Option<FileFinder>,
    tasks: Option<TaskList>,
    /// The command of the task that was run last in this window.
    last_task: Option<String>,
    branches: Option<BranchMenu>,
    worktrees: Option<WorktreeMenu>,
    /// The list of named things in code to go to, while it is showing.
    symbols: Option<(Entity<SymbolPicker>, Subscription)>,
    /// The settings screen, while it is showing.
    settings: Option<(Entity<SettingsView>, Subscription)>,
    /// The user has agreed to close the window; do not ask again.
    closing: bool,
    /// A terminal command is on its way to the terminals.
    forwarding: bool,
    /// Zen mode: nothing but the tabs on screen.
    zen: Option<Zen>,
    state: State,
    /// Where `state` is saved; `None` keeps it in memory only.
    state_path: Option<PathBuf>,
    save_task: Option<Task<()>>,
    _bounds: Subscription,
    /// A download or a waiting update shows in the status bar.
    _updates: Subscription,
    /// What the language servers report shows in the status bar.
    _checks: Option<Subscription>,
}

impl Workspace {
    pub fn new(
        root: Option<PathBuf>,
        mut state: State,
        state_path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let project = root.map(|root| {
            state.touch_recent(&root);
            let layout = state.layouts.get(&root).cloned();
            Self::open(root, layout.as_ref(), window, cx)
        });
        if project.is_none() {
            // Deferred: a title set while the window is being created is lost.
            window.defer(cx, |window, _| window.set_window_title("Wercode"));
        }
        let bounds = cx.observe_window_bounds(window, |this, window, cx| this.remember_window(window, cx));
        let updates = cx.observe_global::<update::Updater>(|_, cx| cx.notify());
        let checks = crate::lsp::checker(cx).map(|checker| cx.observe(&checker, |_, _, cx| cx.notify()));
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.should_close(window, cx)).unwrap_or(true)
        });
        let this = Self {
            project,
            panel: PanelKind::from_name(&state.panel),
            panel_open: state.panel_open,
            switcher_open: false,
            palette: None,
            finder: None,
            tasks: None,
            last_task: None,
            branches: None,
            worktrees: None,
            symbols: None,
            settings: None,
            closing: false,
            forwarding: false,
            zen: None,
            state,
            state_path,
            save_task: None,
            _bounds: bounds,
            _updates: updates,
            _checks: checks,
        };
        this.save_recent();
        this
    }

    /// Opens a folder, with the tabs it had when it was last open.
    fn open(root: PathBuf, layout: Option<&Layout>, window: &mut Window, cx: &mut Context<Self>) -> Project {
        let terminals = cx.new(|cx| TerminalArea::new(root.clone(), layout, window, cx));
        let files = cx.new(|cx| FilesPanel::new(root.clone(), window, cx));
        let search = cx.new(|cx| SearchPanel::new(root.clone(), window, cx));
        let changes = cx.new(|cx| GitPanel::new(root.clone(), window, cx));
        let problems = cx.new(|cx| ProblemsPanel::new(root.clone(), cx));
        let servers = crate::lsp::project(&root, cx);
        let subscriptions = [
            cx.subscribe_in(&files, window, |this, _, event, window, cx| {
                let Some(project) = &this.project else { return };
                let (terminals, root) = (project.terminals.clone(), project.root.clone());
                match event {
                    FilesEvent::Open(path) => this.open_file(path.clone(), None, window, cx),
                    FilesEvent::OpenBeside(path) => {
                        terminals.update(cx, |area, cx| area.open_beside(path.clone(), Axis::Horizontal, window, cx));
                    }
                    FilesEvent::Insert(path) => {
                        terminals.update(cx, |area, cx| area.insert_path(path, window, cx));
                    }
                    FilesEvent::OpenInTerminal(path) => {
                        let editor = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_default();
                        let command = format!("{} {}", editor.trim(), fs_ops::for_shell(path, Some(&root)));
                        terminals.update(cx, |area, cx| area.run_in_new_tab(&command, window, cx));
                    }
                    FilesEvent::Renamed { from, to } => {
                        terminals.update(cx, |area, cx| area.path_renamed(from, to, cx));
                    }
                }
            }),
            cx.subscribe_in(&search, window, |this, _, event, window, cx| match event {
                SearchEvent::Open { path, line, column } => {
                    this.open_file(path.clone(), Some((*line, *column)), window, cx)
                }
                // Git has new changes to show.
                SearchEvent::Replaced => this.refresh_git(cx),
            }),
            cx.subscribe_in(&problems, window, |this, _, event, window, cx| match event {
                ProblemsEvent::Open { path, line, column } => {
                    this.open_file(path.clone(), Some((*line, *column)), window, cx)
                }
            }),
            cx.subscribe_in(&changes, window, |this, _, event, window, cx| match event {
                // Staging or committing shows at once, without waiting for the next poll.
                GitPanelEvent::Changed => this.refresh_git(cx),
                GitPanelEvent::Open(path) => this.open_file(path.clone(), None, window, cx),
                GitPanelEvent::OpenDiff { path, staged, untracked } => {
                    let Some(project) = &this.project else { return };
                    let (root, path, staged, untracked) = (project.root.clone(), path.clone(), *staged, *untracked);
                    project
                        .terminals
                        .update(cx, |area, cx| area.open_diff(root, path, staged, untracked, window, cx));
                }
                GitPanelEvent::OpenAllChanges { staged, reveal } => {
                    this.open_all_changes(*staged, reveal.clone(), window, cx)
                }
                GitPanelEvent::OpenCommitDiff { path, hash, parent } => {
                    let Some(project) = &this.project else { return };
                    let (root, path, hash, parent) = (project.root.clone(), path.clone(), hash.clone(), parent.clone());
                    project
                        .terminals
                        .update(cx, |area, cx| area.open_commit_diff(root, path, hash, parent, window, cx));
                }
            }),
            // A fetch, pull or push shows in the status bar while it runs.
            cx.observe(&changes, |_, _, cx| cx.notify()),
            // The tab of all changes staged or threw something away.
            cx.subscribe_in(&terminals, window, |this, _, event: &AreaEvent, window, cx| match event {
                AreaEvent::GitChanged => this.refresh_git(cx),
                AreaEvent::PickBeside(axis) => this.open_finder_beside(Some(*axis), window, cx),
            }),
            // Tabs opened, closed or split are written down for the next launch.
            cx.observe(&terminals, |this, _, cx| {
                this.remember_layout(cx);
                // The status bar says things about the file that is showing.
                cx.notify();
            }),
        ];

        // The watcher reports from its own thread; this brings the news here.
        let (changed, mut changes_seen) = mpsc::unbounded::<()>();
        let watch = watch::watch(&root, move || {
            let _ = changed.unbounded_send(());
        });
        let watched = root.clone();
        let watch_task = cx.spawn(async move |this, cx| {
            while changes_seen.next().await.is_some() {
                if this.update(cx, |this, cx| this.disk_changed(&watched, cx)).is_err() {
                    break;
                }
            }
        });
        window.set_window_title(&format!("{} - Wercode", folder_name(&root)));

        let dir = root.clone();
        let git_poll = cx.spawn_in(window, async move |this, cx| {
            let mut first = true;
            loop {
                // A window in the background does not need fresh marks.
                let Ok(active) = this.update_in(cx, |_, window, _| window.is_window_active()) else { break };
                if active || first {
                    let path = dir.clone();
                    let status = cx.background_executor().spawn(async move { git::status(&path) }).await;
                    if this.update(cx, |this, cx| this.set_git(&dir, status, cx)).is_err() {
                        break;
                    }
                    first = false;
                }
                cx.background_executor().timer(GIT_POLL).await;
            }
        });

        Project {
            root,
            terminals,
            files,
            search,
            changes,
            problems,
            git: None,
            _servers: servers,
            _subscriptions: subscriptions,
            _git_poll: git_poll,
            _watch: watch,
            _watch_task: watch_task,
        }
    }

    /// Another program created, changed or removed files in the project.
    fn disk_changed(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref().filter(|p| p.root == root) else { return };
        project.files.update(cx, |files, cx| files.refresh(cx));
        project.terminals.update(cx, |area, cx| area.refresh_diffs(cx));
        self.refresh_git(cx);
    }

    /// Shows the branches of the repository, to switch to one or start a new one.
    fn open_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switcher_open = false;
        self.palette = None;
        if !self.project.as_ref().is_some_and(|p| p.git.is_some()) {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(t("New branch name, then Enter")));
        let subscription = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let name = input.read(cx).value().to_string();
                this.change_branch(name, true, window, cx);
            }
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.branches =
            Some(BranchMenu {
                names: Vec::new(),
                remote: Vec::new(),
                input,
                error: None,
                busy: false,
                _subscription: subscription,
            });
        self.list_branches(cx);
        cx.notify();
    }

    /// Reads the branches for the list that is showing.
    fn list_branches(&mut self, cx: &mut Context<Self>) {
        let Some(project) = &self.project else { return };
        let root = project.root.clone();
        cx.spawn(async move |this, cx| {
            let (names, remote) = cx
                .background_executor()
                .spawn(async move { (git::branches(&root), git::remote_branches(&root)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(menu) = &mut this.branches {
                    (menu.names, menu.remote) = (names, remote);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Deletes a local branch; the list stays open and shows what is left,
    /// or what git said against it.
    fn delete_branch(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = &self.project else { return };
        let Some(menu) = &mut self.branches else { return };
        if menu.busy {
            return;
        }
        menu.busy = true;
        menu.error = None;
        let root = project.root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { git::delete_branch(&root, &name) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(menu) = &mut this.branches {
                    menu.busy = false;
                    menu.error = result.err();
                }
                this.list_branches(cx);
                // The graph draws branch names too.
                this.refresh_git(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn close_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.branches.take().is_some() {
            self.focus_terminals(window, cx);
            cx.notify();
        }
    }

    /// Shows the folders checked out from the repository, to switch to one,
    /// open one in another window, or make one for a branch.
    fn open_worktrees(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switcher_open = false;
        self.palette = None;
        self.branches = None;
        if !self.project.as_ref().is_some_and(|p| p.git.is_some()) {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(t("New worktree: a branch name, then Enter")));
        let subscription = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let name = input.read(cx).value().to_string();
                this.add_worktree(name, window, cx);
            }
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.worktrees = Some(WorktreeMenu { trees: None, input, error: None, busy: false, _subscription: subscription });
        self.list_worktrees(cx);
        cx.notify();
    }

    fn list_worktrees(&mut self, cx: &mut Context<Self>) {
        let Some(project) = &self.project else { return };
        let root = project.root.clone();
        cx.spawn(async move |this, cx| {
            let trees = cx.background_executor().spawn(async move { git::worktrees(&root) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(menu) = &mut this.worktrees {
                    menu.trees = Some(trees);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Makes a worktree for a branch next to the repository's own folder and
    /// switches this window to it.
    fn add_worktree(&mut self, branch: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = &self.project else { return };
        let Some(menu) = &mut self.worktrees else { return };
        if menu.busy {
            return;
        }
        let main = menu.trees.as_ref().and_then(|trees| trees.first()).map(|tree| tree.path.clone());
        let main = main.unwrap_or_else(|| project.root.clone());
        menu.busy = true;
        menu.error = None;
        let root = project.root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let folder = git::worktree_folder(&main, &branch);
                    git::add_worktree(&root, &folder, &branch).map(|()| folder)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(folder) => {
                        this.worktrees = None;
                        this.switch_to(folder, window, cx);
                    }
                    Err(error) => {
                        if let Some(menu) = &mut this.worktrees {
                            menu.busy = false;
                            menu.error = Some(error);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Removes a worktree's folder; the list shows what is left, or what git said against it.
    fn remove_worktree(&mut self, tree: git::Worktree, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = &self.project else { return };
        let Some(menu) = &mut self.worktrees else { return };
        if menu.busy {
            return;
        }
        menu.busy = true;
        menu.error = None;
        let root = project.root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { git::remove_worktree(&root, &tree) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(menu) = &mut this.worktrees {
                    menu.busy = false;
                    menu.error = result.err();
                }
                this.list_worktrees(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Opens a worktree in a window of its own, beside this one.
    fn open_worktree_window(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.worktrees = None;
        cx.notify();
        let state = self.fresh_state();
        let state_path = self.state_path.clone();
        let bounds = Some(WindowBounds::Windowed(beside(window.bounds())));
        let path = normalize(&path);
        cx.defer(move |cx| open_window(Some(path), state, state_path, bounds, cx));
    }

    fn close_worktrees(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.worktrees.take().is_some() {
            self.focus_terminals(window, cx);
            cx.notify();
        }
    }

    fn render_worktrees(&self, menu: &WorktreeMenu, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border, hover, danger) = (theme.muted_foreground, theme.border, theme.list_hover, theme.danger);
        let (popover, popover_text) = (theme.popover, theme.popover_foreground);
        let root = self.project.as_ref().map(|p| p.root.clone());
        let trees = menu.trees.clone().unwrap_or_default();
        let busy = menu.busy;
        let rows: Vec<_> = trees
            .into_iter()
            .enumerate()
            .map(|(index, tree)| {
                let current = root.as_deref() == Some(normalize(&tree.path).as_path());
                let usable = !current && !tree.prunable;
                let what = match (&tree.branch, tree.prunable) {
                    (_, true) => t("its folder is gone").to_string(),
                    (Some(branch), _) => branch.clone(),
                    (None, _) => tf("detached at {commit}", &[("commit", &tree.head)]),
                };
                let (target, other, gone) = (tree.path.clone(), tree.path.clone(), tree.clone());
                h_flex()
                    .id(("worktree", index))
                    .test_support()
                    .w_full()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .hover(|s| s.bg(hover))
                    .when(usable, |d| {
                        d.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.worktrees = None;
                            this.switch_to(target.clone(), window, cx);
                        }))
                    })
                    .child(Icon::new(Lucide::FolderGit2).xsmall().text_color(muted))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(folder_name(&tree.path)))
                            .child(div().text_xs().text_color(muted).truncate().child(what)),
                    )
                    .when(current, |d| d.child(Icon::new(IconName::Check).xsmall().text_color(muted)))
                    .when(usable, |d| {
                        d.child(
                            Button::new(("worktree-window", index))
                                .ghost()
                                .xsmall()
                                .icon(Lucide::AppWindow)
                                .tooltip(t("Open in a New Window"))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.open_worktree_window(other.clone(), window, cx);
                                })),
                        )
                    })
                    .when(!tree.main && !current, |d| {
                        d.child(
                            Button::new(("worktree-remove", index))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip(t("Remove Worktree"))
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    // The row under the button would switch to it.
                                    cx.stop_propagation();
                                    this.remove_worktree(gone.clone(), window, cx);
                                })),
                        )
                    })
            })
            .collect();
        self.backdrop("worktrees-backdrop", cx).child(
            v_flex()
                .id("worktrees")
                .absolute()
                .bottom(px(28.))
                .left(px(8.))
                .w(px(380.))
                .max_h(px(420.))
                .p_1()
                .gap_1()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .bg(popover)
                .text_color(popover_text)
                .shadow_lg()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.close_worktrees(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(div().px_3().py_1().text_xs().text_color(muted).child(t("WORKTREES")))
                .child(v_flex().id("worktree-list").flex_1().min_h_0().overflow_y_scroll().children(rows))
                .child(div().h(px(1.)).bg(border))
                .child(div().p_1().child(Input::new(&menu.input).small()))
                .child(div().px_3().pb_1().text_xs().text_color(muted).child(t(
                    "Each worktree is a folder of its own with another branch checked out, so several agents can work at once without getting in each other's way.",
                )))
                .when_some(menu.error.clone(), |d, error| {
                    d.child(div().id("worktree-error").px_3().pb_1().text_xs().text_color(danger).child(error))
                }),
        )
    }

    /// Lists the named things of the file that is showing, or searches the
    /// whole project for one, to go to it.
    fn open_symbols(&mut self, project_wide: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        let Some(project) = &self.project else { return };
        let Some(servers) = crate::lsp::checker(cx).map(|checker| checker.read(cx).servers().clone()) else { return };
        let root = project.root.clone();
        let file = project.terminals.read(cx).shown_file(cx);
        // Without a file showing there is only the project to search.
        let file = if project_wide { None } else { file };
        let picker = cx.new(|cx| SymbolPicker::new(servers, root, file, window, cx));
        let subscription = cx.subscribe_in(&picker, window, |this, _, event: &SymbolsEvent, window, cx| match event {
            SymbolsEvent::Open { path, line, column } => {
                this.symbols = None;
                this.open_file(path.clone(), Some((*line, *column)), window, cx);
                cx.notify();
            }
            SymbolsEvent::Close => this.close_symbols(window, cx),
        });
        self.symbols = Some((picker, subscription));
        cx.notify();
    }

    fn close_symbols(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.symbols.take().is_some() {
            self.focus_terminals(window, cx);
            cx.notify();
        }
    }

    /// Switches to a branch, or makes it first. When git refuses (the switch
    /// would overwrite changes), what it said stays in the list.
    fn change_branch(&mut self, name: String, create: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = &self.project else { return };
        let Some(menu) = &mut self.branches else { return };
        if menu.busy {
            return;
        }
        menu.busy = true;
        menu.error = None;
        // A branch of a remote gets a local one that follows it.
        let follow = !create && menu.remote.contains(&name);
        let root = project.root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let dir = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    if create {
                        git::create_branch(&dir, &name)
                    } else if follow {
                        git::track(&dir, &name)
                    } else {
                        git::switch(&dir, &name)
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => {
                        this.close_branches(window, cx);
                        // Another branch has other files.
                        this.disk_changed(&root, cx);
                    }
                    Err(error) => {
                        if let Some(menu) = &mut this.branches {
                            menu.busy = false;
                            menu.error = Some(error);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_branches(&self, menu: &BranchMenu, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border, hover, danger) = (theme.muted_foreground, theme.border, theme.list_hover, theme.danger);
        let (popover, popover_text) = (theme.popover, theme.popover_foreground);
        let current = self.project.as_ref().and_then(|p| p.git.as_ref()).and_then(|git| git.branch.clone());
        let rows = menu.names.iter().enumerate().map(|(index, name)| {
            let is_current = current.as_deref() == Some(name.as_str());
            let target = name.clone();
            h_flex()
                .id(("branch", index))
                .test_support()
                .w_full()
                .gap_2()
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .hover(|s| s.bg(hover))
                .when(!is_current, |d| {
                    d.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.change_branch(target.clone(), false, window, cx);
                    }))
                })
                .child(Icon::new(Lucide::GitBranch).xsmall().text_color(muted))
                .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                .when(is_current, |d| d.child(Icon::new(IconName::Check).xsmall().text_color(muted)))
                .when(!is_current, |d| {
                    let gone = name.clone();
                    d.child(
                        Button::new(("branch-delete", index))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip(t("Delete Branch"))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                // The row under the button would switch to the branch.
                                cx.stop_propagation();
                                this.delete_branch(gone.clone(), window, cx);
                            })),
                    )
                })
        });
        let mut rows: Vec<_> = rows.collect();
        // After the local ones, what only a remote has: a click brings it here.
        rows.extend(menu.remote.iter().enumerate().map(|(index, name)| {
            let target = name.clone();
            h_flex()
                .id(("remote-branch", index))
                .test_support()
                .w_full()
                .gap_2()
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .text_color(muted)
                .hover(|s| s.bg(hover))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.change_branch(target.clone(), false, window, cx);
                }))
                .child(Icon::new(Lucide::GitBranch).xsmall().text_color(muted))
                .child(div().flex_1().min_w_0().truncate().child(name.clone()))
        }));
        self.backdrop("branches-backdrop", cx).child(
            v_flex()
                .id("branches")
                .absolute()
                .bottom(px(28.))
                .left(px(8.))
                .w(px(340.))
                .max_h(px(420.))
                .p_1()
                .gap_1()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .bg(popover)
                .text_color(popover_text)
                .shadow_lg()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.close_branches(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(div().px_3().py_1().text_xs().text_color(muted).child(t("BRANCHES")))
                .child(v_flex().id("branch-list").flex_1().min_h_0().overflow_y_scroll().children(rows))
                .child(div().h(px(1.)).bg(border))
                .child(div().p_1().child(Input::new(&menu.input).small()))
                .when_some(menu.error.clone(), |d, error| {
                    d.child(div().id("branch-error").px_3().pb_1().text_xs().text_color(danger).child(error))
                }),
        )
    }

    /// Notes which tabs are open, and saves that a moment later.
    fn remember_layout(&mut self, cx: &mut Context<Self>) {
        let Some(project) = &self.project else { return };
        let layout = project.terminals.read(cx).layout(cx);
        if self.state.layouts.get(&project.root) != Some(&layout) {
            self.state.layouts.insert(project.root.clone(), layout);
            self.save_later(cx);
        }
    }

    /// Called when the user closes the window. Returns false to keep it open
    /// while a question about unsaved files or running programs is answered.
    fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.closing {
            return true;
        }
        self.remember_layout(cx);
        self.save();
        let Some(project) = &self.project else { return true };
        let area = project.terminals.read(cx);
        let (busy, unsaved) = (area.busy_count(cx), area.unsaved_count(cx));
        let terminals = project.terminals.clone();
        if unsaved > 0 {
            let title = tn(
                unsaved as u64,
                "Save changes to {n} file before closing?",
                "Save changes to {n} files before closing?",
                &[],
            );
            let answer = window.prompt(
                PromptLevel::Warning,
                &title,
                Some(t("Your changes will be lost if you do not save them.")),
                &[t("Save All"), t("Don't Save"), t("Cancel")],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| match answer.await {
                Ok(0) => {
                    let _ = terminals.update_in(cx, |area, window, cx| {
                        let saved = move |window: &mut Window, cx: &mut App| {
                            let _ = this.update(cx, |this, _| this.closing = true);
                            window.remove_window();
                        };
                        area.save_all(saved, window, cx);
                    });
                }
                Ok(1) => {
                    let _ = this.update_in(cx, |this, window, _| {
                        this.closing = true;
                        window.remove_window();
                    });
                }
                _ => {
                    let _ = cx.update(|_, cx| update::restart_cancelled(cx));
                }
            })
            .detach();
            return false;
        }
        if busy > 0 && cx.global::<AppState>().settings.confirm_close {
            let detail = tn(
                busy as u64,
                "A program is still running in a terminal. Closing the window will stop it.",
                "Programs are still running in {n} terminals. Closing the window will stop them.",
                &[],
            );
            let answer = window.prompt(PromptLevel::Warning, t("Close window?"), Some(&detail), &[t("Close"), t("Cancel")], cx);
            cx.spawn_in(window, async move |this, cx| {
                if answer.await == Ok(0) {
                    let _ = this.update_in(cx, |this, window, _| {
                        this.closing = true;
                        window.remove_window();
                    });
                } else {
                    let _ = cx.update(|_, cx| update::restart_cancelled(cx));
                }
            })
            .detach();
            return false;
        }
        true
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switcher_open = false;
        self.palette = None;
        // What was typed into the file by hand since the last look counts too.
        crate::theme::reload_settings(cx);
        let view = cx.new(|cx| SettingsView::new(window, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
            SettingsEvent::Close => this.close_settings(window, cx),
            SettingsEvent::OpenFile(path) => {
                this.close_settings(window, cx);
                this.open_settings_file(path.clone(), window, cx);
            }
        });
        view.focus_handle(cx).focus(window, cx);
        self.settings = Some((view, subscription));
        cx.notify();
    }

    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.take().is_some() {
            self.focus_terminals(window, cx);
            cx.notify();
        }
    }

    /// Shows `settings.toml` itself, creating it first if it was never written.
    fn open_settings_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if !path.exists() {
            let made = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&path, ""));
            if let Err(e) = made {
                eprintln!("could not create {}: {e}", path.display());
                return;
            }
        }
        if self.project.is_some() {
            self.open_file(path, None, window, cx);
        } else {
            // No project, no editor tabs: hand the file to the system.
            cx.open_with_system(&path);
        }
    }

    #[cfg(test)]
    fn pane_count(&self, cx: &App) -> usize {
        self.project.as_ref().map_or(0, |p| p.terminals.read(cx).pane_count())
    }

    #[cfg(test)]
    fn root(&self) -> Option<PathBuf> {
        self.project.as_ref().map(|p| p.root.clone())
    }

    /// Shows a file in an editor tab, at a 1-based line and column if given.
    fn open_file(&mut self, path: PathBuf, at: Option<(u32, u32)>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(project) = &self.project {
            let (line, column) = (at.map(|at| at.0), at.map(|at| at.1));
            project.terminals.update(cx, |area, cx| area.open_path(path, line, column, window, cx));
        }
    }

    /// Reads the repository status now instead of at the next poll.
    fn refresh_git(&mut self, cx: &mut Context<Self>) {
        let Some(project) = &self.project else { return };
        let dir = project.root.clone();
        cx.spawn(async move |this, cx| {
            let path = dir.clone();
            let status = cx.background_executor().spawn(async move { git::status(&path) }).await;
            let _ = this.update(cx, |this, cx| this.set_git(&dir, status, cx));
        })
        .detach();
    }

    #[cfg(test)]
    fn changes(&self) -> Option<Entity<GitPanel>> {
        self.project.as_ref().map(|p| p.changes.clone())
    }

    #[cfg(test)]
    fn files(&self) -> Entity<FilesPanel> {
        self.project.as_ref().unwrap().files.clone()
    }

    #[cfg(test)]
    fn search(&self) -> Entity<SearchPanel> {
        self.project.as_ref().unwrap().search.clone()
    }

    #[cfg(test)]
    fn problems(&self) -> Entity<ProblemsPanel> {
        self.project.as_ref().unwrap().problems.clone()
    }

    #[cfg(test)]
    fn terminals(&self) -> Entity<TerminalArea> {
        self.project.as_ref().unwrap().terminals.clone()
    }

    #[cfg(test)]
    fn git(&self) -> Option<Arc<RepoStatus>> {
        self.project.as_ref().and_then(|p| p.git.clone())
    }

    fn set_git(&mut self, root: &Path, status: Option<RepoStatus>, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_mut().filter(|p| p.root == root) else { return };
        if project.git.as_deref() == status.as_ref() {
            return;
        }
        // New or vanished files mean the folder itself changed, so read it again.
        let files_changed = match (&project.git, &status) {
            (Some(old), Some(new)) => !old.files.keys().eq(new.files.keys()),
            _ => false,
        };
        let moved = match (&project.git, &status) {
            (Some(old), Some(new)) => old.head != new.head || old.branch != new.branch,
            (None, None) => false,
            _ => true,
        };
        project.git = status.map(Arc::new);
        let git = project.git.clone();
        if moved {
            project.terminals.update(cx, |area, cx| area.git_changed(cx));
        }
        project.changes.update(cx, |changes, cx| changes.set_git(git.clone(), cx));
        project.files.update(cx, |files, cx| {
            files.set_git(git, cx);
            if files_changed {
                files.refresh(cx);
            }
        });
        // Staging and committing move changes from one side to the other.
        project.terminals.update(cx, |area, cx| area.refresh_diffs(cx));
        cx.notify();
    }

    /// Saves this window's layout. The list of recent projects is taken from
    /// disk, so a window never drops a project another window has opened.
    fn save(&self) {
        self.write(&self.fresh_state());
    }

    /// Saves everything, the list of recent projects included.
    fn save_recent(&self) {
        self.write(&self.state);
    }

    fn write(&self, state: &State) {
        if let Some(path) = &self.state_path
            && let Err(e) = state::save_to(path, state)
        {
            eprintln!("could not save {}: {e}", path.display());
        }
    }

    fn save_later(&mut self, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let _ = this.update(cx, |this, _| this.save());
        }));
    }

    fn remember_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (bounds, maximized) = match window.window_bounds() {
            WindowBounds::Windowed(bounds) => (bounds, false),
            WindowBounds::Maximized(restore) => (restore, true),
            WindowBounds::Fullscreen(_) => return,
        };
        let rect: [f32; 4] =
            [bounds.origin.x.into(), bounds.origin.y.into(), bounds.size.width.into(), bounds.size.height.into()];
        if self.state.window != Some(rect) || self.state.maximized != maximized {
            self.state.window = Some(rect);
            self.state.maximized = maximized;
            self.save_later(cx);
        }
    }

    fn remember_panel_width(&mut self, width: f32, cx: &mut Context<Self>) {
        if self.state.panel_width != Some(width) {
            self.state.panel_width = Some(width);
            self.save_later(cx);
        }
    }

    fn focus_terminals(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(project) = &self.project {
            project.terminals.focus_handle(cx).focus(window, cx);
        }
    }

    /// Enters zen mode, filling the screen, or leaves it and puts the window
    /// back the way it was.
    fn toggle_zen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        if self.zen.is_some() {
            self.leave_zen(window, cx);
            return;
        }
        if self.project.is_none() {
            return;
        }
        // Not on Linux: there GPUI learns of full screen only from the compositor,
        // and one that does not say (WSLg's) left the window stuck full screen,
        // since the second toggle asked for full screen again.
        let fullscreen = cfg!(not(target_os = "linux")) && !window.is_fullscreen();
        if fullscreen {
            window.toggle_fullscreen();
        }
        let note = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ZEN_NOTE).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(zen) = &mut this.zen {
                    zen.note = None;
                    cx.notify();
                }
            });
        });
        self.zen = Some(Zen { fullscreen, note: Some(note) });
        self.focus_terminals(window, cx);
        cx.notify();
    }

    fn leave_zen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(zen) = self.zen.take() else { return };
        if zen.fullscreen && window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        cx.notify();
    }

    /// The note on how to leave zen mode, while it shows.
    fn render_zen_note(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let text = match crate::keymap::label_for("workspace.toggle_zen", cx) {
            Some(keys) => tf("Zen mode. {keys} brings everything back.", &[("keys", &keys)]),
            None => t("Zen mode. \"View: Zen Mode\" in the command palette brings everything back.").to_string(),
        };
        div().absolute().top(px(12.)).left_0().w_full().flex().justify_center().child(
            div()
                .id("zen-note")
                .test_support()
                .px_3()
                .py_1p5()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .text_sm()
                .shadow_md()
                .child(text),
        )
    }

    fn toggle_panel(&mut self, cx: &mut Context<Self>) {
        if self.zen.take().is_some() {
            // The panel is asked for: zen mode ends and the panel shows.
            self.panel_open = true;
            self.state.panel_open = true;
            self.save();
            cx.notify();
            return;
        }
        self.panel_open = !self.panel_open;
        self.state.panel_open = self.panel_open;
        self.save();
        cx.notify();
    }

    /// Shows a side panel; choosing the one already showing hides it.
    fn show_panel(&mut self, kind: PanelKind, window: &mut Window, cx: &mut Context<Self>) {
        // A panel asked for in zen mode ends it.
        if self.zen.is_some() {
            self.leave_zen(window, cx);
            self.panel_open = false;
        }
        if self.panel_open && self.panel == kind {
            self.panel_open = false;
            self.focus_terminals(window, cx);
        } else {
            self.panel = kind;
            self.panel_open = true;
            // Asking for the search is asking to type a query.
            if kind == PanelKind::Search
                && let Some(project) = &self.project
            {
                let search = project.search.clone();
                search.update(cx, |search, cx| search.focus_input(window, cx));
            }
        }
        self.state.panel = self.panel.name().into();
        self.state.panel_open = self.panel_open;
        self.save();
        cx.notify();
    }

    /// Fetches, pulls or pushes from a shortcut or the palette. The panel
    /// does the work; the status bar shows how it goes.
    fn git_remote(&mut self, command: Remote, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        cx.notify();
        let Some(panel) = self.project.as_ref().map(|p| p.changes.clone()) else { return };
        panel.update(cx, |panel, cx| panel.remote(command, window, cx));
    }

    /// Shows every change in the repository in one tab.
    fn open_all_changes(&mut self, staged: bool, reveal: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        cx.notify();
        let Some(terminals) = self.project.as_ref().map(|p| p.terminals.clone()) else { return };
        terminals.update(cx, |area, cx| area.open_changes(staged, reveal, window, cx));
    }

    fn git_stage_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        cx.notify();
        let Some(panel) = self.project.as_ref().map(|p| p.changes.clone()) else { return };
        panel.update(cx, |panel, cx| panel.stage_all(window, cx));
    }

    /// Commits the message written in the git panel. Without one, puts the
    /// keyboard in the message field. Either way the panel shows, since it
    /// is where the field and any complaint from git are.
    fn git_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        cx.notify();
        let Some(panel) = self.project.as_ref().map(|p| p.changes.clone()) else { return };
        if !(self.panel_open && self.panel == PanelKind::Git) {
            self.show_panel(PanelKind::Git, window, cx);
        }
        if panel.read(cx).has_message(cx) {
            panel.update(cx, |panel, cx| panel.commit(window, cx));
        } else {
            panel.update(cx, |panel, cx| panel.focus_message(window, cx));
        }
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switcher_open = false;
        self.close_finder_quietly();
        self.tasks = None;
        let palette = cx.new(|cx| CommandState::new(window, cx));
        palette.update(cx, |palette, cx| palette.focus(window, cx));
        self.palette = Some(palette);
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            self.focus_terminals(window, cx);
            cx.notify();
        }
    }

    /// Shows the list of the project's files. The folder is read in the
    /// background; what is typed meanwhile is applied when it arrives.
    fn open_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_finder_beside(None, window, cx);
    }

    /// The list for opening a file by name; with `beside`, the file chosen
    /// is shown next to the one showing, in the same tab.
    fn open_finder_beside(&mut self, beside: Option<Axis>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = &self.project else { return };
        let root = project.root.clone();
        self.switcher_open = false;
        self.palette = None;
        self.close_finder_quietly();
        self.tasks = None;
        let state = cx.new(|cx| CommandState::new(window, cx));
        state.update(cx, |state, cx| {
            state.set_loading(true, window, cx);
            state.focus(window, cx);
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let (flag, mine) = (cancel.clone(), cancel.clone());
        cx.spawn_in(window, async move |this, cx| {
            let files = cx.background_executor().spawn(async move { finder::list_files(&root, &flag) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                // The list may have been closed, or closed and opened again.
                let Some(finder) = this.finder.as_mut().filter(|f| Arc::ptr_eq(&f.cancel, &mine)) else { return };
                let query = finder.state.read(cx).query(cx);
                finder.shown = finder::matches(&files, &query, FINDER_ROWS);
                finder.files = Some(Arc::new(files));
                finder.state.clone().update(cx, |state, cx| state.set_loading(false, window, cx));
                cx.notify();
            });
        })
        .detach();
        self.finder = Some(FileFinder { state, beside, files: None, shown: Vec::new(), cancel });
        cx.notify();
    }

    fn close_finder_quietly(&mut self) -> bool {
        match self.finder.take() {
            Some(finder) => {
                finder.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    fn close_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_finder_quietly() {
            self.focus_terminals(window, cx);
            cx.notify();
        }
    }

    /// Shows what the project can run. Its files are read in the background.
    fn open_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = &self.project else { return };
        let root = project.root.clone();
        self.switcher_open = false;
        self.palette = None;
        self.close_finder_quietly();
        self.tasks = None;
        let state = cx.new(|cx| CommandState::new(window, cx));
        state.update(cx, |state, cx| {
            state.set_loading(true, window, cx);
            state.focus(window, cx);
        });
        let mine = state.entity_id();
        cx.spawn_in(window, async move |this, cx| {
            let found = cx.background_executor().spawn(async move { tasks::discover(&root) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                // The list may have been closed, or closed and opened again.
                let Some(list) = this.tasks.as_mut().filter(|list| list.state.entity_id() == mine) else { return };
                let query = list.state.read(cx).query(cx);
                list.shown = tasks::matches(&found, &query);
                list.tasks = Some(found);
                list.state.clone().update(cx, |state, cx| state.set_loading(false, window, cx));
                cx.notify();
            });
        })
        .detach();
        self.tasks = Some(TaskList { state, tasks: None, shown: Vec::new() });
        cx.notify();
    }

    fn close_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tasks.take().is_some() {
            self.focus_terminals(window, cx);
            cx.notify();
        }
    }

    fn tasks_query(&mut self, query: &str, cx: &mut Context<Self>) {
        let Some(list) = &mut self.tasks else { return };
        if let Some(found) = &list.tasks {
            list.shown = tasks::matches(found, query);
            cx.notify();
        }
    }

    /// Runs the task in row `row` of the list in a new terminal tab.
    fn tasks_confirm(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(list) = self.tasks.take() else { return };
        let command = list
            .tasks
            .as_ref()
            .and_then(|found| found.get(*list.shown.get(row)?))
            .map(|task| task.command.clone());
        cx.notify();
        match command {
            Some(command) => self.run_task(command, window, cx),
            None => self.focus_terminals(window, cx),
        }
    }

    fn run_task(&mut self, command: String, window: &mut Window, cx: &mut Context<Self>) {
        self.run_in_terminal(&command, window, cx);
        self.last_task = Some(command);
    }

    /// Runs the last task again; before any was run, shows the list.
    fn rerun_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        match self.last_task.clone() {
            Some(command) => self.run_task(command, window, cx),
            None => self.open_tasks(window, cx),
        }
    }

    fn finder_query(&mut self, query: &str, cx: &mut Context<Self>) {
        let Some(finder) = &mut self.finder else { return };
        if let Some(files) = &finder.files {
            finder.shown = finder::matches(files, query, FINDER_ROWS);
            cx.notify();
        }
    }

    /// Opens the file in row `row` of the list and closes the list.
    fn finder_confirm(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(finder), Some(project)) = (&self.finder, &self.project) else { return };
        let (beside, terminals) = (finder.beside, project.terminals.clone());
        let path = finder
            .files
            .as_ref()
            .and_then(|files| files.get(*finder.shown.get(row)?))
            .map(|relative| project.root.join(relative));
        self.close_finder_quietly();
        self.tasks = None;
        cx.notify();
        match (path, beside) {
            (Some(path), Some(axis)) => terminals.update(cx, |area, cx| area.open_beside(path, axis, window, cx)),
            (Some(path), None) => self.open_file(path, None, window, cx),
            (None, _) => self.focus_terminals(window, cx),
        }
    }

    /// A terminal command that arrived here was sent while the focus was
    /// outside the terminals (the palette, the file tree). Hand it over.
    fn forward(&mut self, action: &dyn Action, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        cx.notify();
        // If the terminals let the command through, it comes back here; the
        // flag keeps that from going round forever.
        if self.forwarding {
            return;
        }
        let Some(project) = &self.project else { return };
        let terminals = project.terminals.focus_handle(cx);
        terminals.focus(window, cx);
        self.forwarding = true;
        let action = action.boxed_clone();
        let this = cx.entity().downgrade();
        // Deferred: the terminals must not be asked while this view is busy.
        window.defer(cx, move |window, cx| {
            terminals.dispatch_action(action.as_ref(), window, cx);
            let _ = this.update(cx, |this, _| this.forwarding = false);
        });
    }

    /// Replaces the project in this window. Terminals of the old project are
    /// closed, so this asks first when one of them is still running something.
    fn switch_to(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.switcher_open = false;
        cx.notify();
        let path = normalize(&path);
        let current = self.project.as_ref().map(|p| p.root.as_path());
        if current == Some(path.as_path()) || !path.is_dir() {
            return;
        }
        let (busy, unsaved) = self.project.as_ref().map_or((0, 0), |p| {
            let area = p.terminals.read(cx);
            (area.busy_count(cx), area.unsaved_count(cx))
        });
        let ask_busy = busy > 0 && cx.global::<AppState>().settings.confirm_close;
        if !ask_busy && unsaved == 0 {
            self.open_folder(path, window, cx);
            return;
        }
        let name = current.map(folder_name).unwrap_or_default();
        // Losing edits is the worse of the two, so it is the one that gets said.
        let detail = if unsaved > 0 {
            tn(
                unsaved as u64,
                "{n} file in {name} has unsaved changes. Switching discards them.",
                "{n} files in {name} have unsaved changes. Switching discards them.",
                &[("name", &name)],
            )
        } else {
            tn(
                busy as u64,
                "{n} terminal in {name} is still running a program. Switching closes it.",
                "{n} terminals in {name} are still running a program. Switching closes them.",
                &[("name", &name)],
            )
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            t("Switch project?"),
            Some(&detail),
            &[t("Switch"), t("Cancel")],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| this.open_folder(path, window, cx));
            }
        })
        .detach();
    }

    /// A second window for another project. It starts on the welcome screen,
    /// smaller than this one and over its middle, so both can be seen.
    fn new_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switcher_open = false;
        self.palette = None;
        cx.notify();
        let state = self.fresh_state();
        // The new window saves where this one does (tests keep that off the real file).
        let state_path = self.state_path.clone();
        let bounds = Some(WindowBounds::Windowed(beside(window.bounds())));
        cx.defer(move |cx| open_window(None, state, state_path, bounds, cx));
    }

    /// The saved state with the list of recent projects, and the tabs of the
    /// other projects, as they are on disk now: another window may have
    /// changed them since this one started.
    fn fresh_state(&self) -> State {
        let mut state = self.state.clone();
        if let Some(path) = &self.state_path {
            let on_disk = state::load_from(path);
            state.recent = on_disk.recent;
            let mine = self.project.as_ref().map(|p| &p.root);
            for (project, layout) in on_disk.layouts {
                if Some(&project) != mine {
                    state.layouts.insert(project, layout);
                }
            }
        }
        state
    }

    fn open_folder(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // The tabs of the project being left are kept for when it is opened again.
        self.remember_layout(cx);
        self.state = self.fresh_state();
        self.state.touch_recent(&path);
        let layout = self.state.layouts.get(&path).cloned();
        let project = Self::open(path, layout.as_ref(), window, cx);
        project.terminals.focus_handle(cx).focus(window, cx);
        self.project = Some(project);
        self.save_recent();
        cx.notify();
    }

    fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switcher_open = false;
        self.palette = None;
        cx.notify();
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |this, window, cx| this.switch_to(path, window, cx));
            }
        })
        .detach();
    }

    fn render_title(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let hover = theme.secondary_hover;
        let on_right = cx.global::<AppState>().settings.panel_on_right();
        let name = self.project.as_ref().map_or_else(|| "Wercode".to_string(), |p| folder_name(&p.root));
        TitleBar::new()
            .child(
                h_flex()
                    .id("project-switcher")
                    .test_support()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .text_color(theme.foreground)
                    .hover(|s| s.bg(hover))
                    // The title bar drags the window; a press here is for the menu.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.switcher_open = !this.switcher_open;
                        cx.notify();
                    }))
                    .child(img(crate::assets::LOGO).flex_none().size(px(16.)))
                    .child(name)
                    .child(Icon::new(IconName::ChevronDown).xsmall().text_color(theme.muted_foreground)),
            )
            .child(div().flex_1())
            .child(
                div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                    Button::new("title-new-window")
                        .ghost()
                        .xsmall()
                        .icon(Lucide::AppWindow)
                        .tooltip(match crate::keymap::label_for("workspace.new_window", cx) {
                            Some(keys) => tf("New window ({keys})", &[("keys", &keys)]),
                            None => t("New window").to_string(),
                        })
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.new_window(window, cx))),
                ),
            )
            .child(
                div().pr_1().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                    Button::new("open-settings")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Settings)
                        .tooltip(t("Settings"))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_settings(window, cx))),
                ),
            )
            .when(self.project.is_some(), |bar| {
                bar.child(
                    div()
                        .pr_2()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            Button::new("toggle-panel")
                                .ghost()
                                .xsmall()
                                .icon(if on_right { IconName::PanelRight } else { IconName::PanelLeft })
                                .tooltip(t("Toggle side panel (Ctrl+Shift+B)"))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_panel(cx))),
                        ),
                )
            })
    }

    /// Rows of the recent projects, shared by the switcher and the welcome screen.
    fn recent_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.list_hover);
        let current = self.project.as_ref().map(|p| p.root.clone());
        let recent: Vec<PathBuf> = self.state.recent.iter().filter(|p| p.is_dir()).cloned().collect();
        recent
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                let is_current = current.as_ref() == Some(&path);
                let target = path.clone();
                h_flex()
                    .id(("recent", index))
                    .test_support()
                    .w_full()
                    .gap_3()
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .hover(|s| s.bg(hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.switch_to(target.clone(), window, cx);
                    }))
                    .child(div().flex_none().text_sm().child(folder_name(&path)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(path.to_string_lossy().into_owned()),
                    )
                    .when(is_current, |d| d.child(Icon::new(IconName::Check).xsmall().text_color(muted)))
                    .into_any_element()
            })
            .collect()
    }

    fn open_folder_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.list_hover);
        h_flex()
            .id("open-folder")
            .test_support()
            .w_full()
            .gap_2()
            .px_3()
            .py_1p5()
            .rounded_md()
            .text_sm()
            .hover(|s| s.bg(hover))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.pick_folder(window, cx)))
            .child(Icon::new(IconName::FolderOpen).small().text_color(muted))
            .child(t("Open folder\u{2026}"))
            .child(div().flex_1())
            .child(div().text_xs().text_color(muted).child("Ctrl+Shift+O"))
    }

    fn new_window_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.list_hover);
        h_flex()
            .id("new-window")
            .test_support()
            .w_full()
            .gap_2()
            .px_3()
            .py_1p5()
            .rounded_md()
            .text_sm()
            .hover(|s| s.bg(hover))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.new_window(window, cx)))
            .child(Icon::new(IconName::Plus).small().text_color(muted))
            .child(t("New window"))
            .child(div().flex_1())
            .children(
                crate::keymap::label_for("workspace.new_window", cx)
                    .map(|keys| div().text_xs().text_color(muted).child(keys)),
            )
    }

    /// A dimmed layer over the window that closes whatever floats on it.
    fn backdrop(&self, id: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        div().id(id).absolute().inset_0().occlude().on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                this.switcher_open = false;
                this.close_palette(window, cx);
                this.close_finder(window, cx);
                this.close_settings(window, cx);
                this.close_branches(window, cx);
                this.close_worktrees(window, cx);
                this.close_symbols(window, cx);
                cx.notify();
            }),
        )
    }

    fn render_switcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border, popover, foreground) =
            (theme.muted_foreground, theme.border, theme.popover, theme.popover_foreground);
        let rows = self.recent_rows(cx);
        self.backdrop("switcher-backdrop", cx).child(
            v_flex()
                .id("switcher")
                .absolute()
                .top(px(36.))
                .left(px(8.))
                .w(px(520.))
                .max_h(px(480.))
                .overflow_y_scroll()
                .p_1()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .bg(popover)
                .text_color(foreground)
                .shadow_lg()
                // Presses inside the list must not reach the backdrop, which closes it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(div().px_3().py_1().text_xs().text_color(muted).child(t("RECENT PROJECTS")))
                .children(rows)
                .child(div().my_1().h(px(1.)).bg(border))
                .child(self.open_folder_row(cx))
                .child(self.new_window_row(cx)),
        )
    }

    fn render_palette(&self, palette: &Entity<CommandState>, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, popover, foreground) = (theme.border, theme.popover, theme.popover_foreground);
        let this = cx.entity().downgrade();
        let on_confirm = this.clone();
        let items = palette_commands().into_iter().map(|(label, action)| CommandItem::new().label(label).action(action));
        self.backdrop("palette-backdrop", cx).flex().justify_center().items_start().child(
            div()
                .id("palette")
                .mt(px(72.))
                .w(px(560.))
                .max_w(relative(0.9))
                .h_auto()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .bg(popover)
                .text_color(foreground)
                .shadow_lg()
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Command::new(palette)
                        .items(items)
                        .placeholder(t("Type a command\u{2026}"))
                        .bordered(false)
                        // The item's action has been dispatched by now; all
                        // that is left is to get out of the way.
                        .on_confirm(move |_, window, cx| {
                            let _ = on_confirm.update(cx, |this, cx| this.close_palette(window, cx));
                        })
                        .on_cancel(move |window, cx| {
                            let _ = this.update(cx, |this, cx| this.close_palette(window, cx));
                        }),
                ),
        )
    }

    fn render_finder(&self, finder: &FileFinder, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, popover, foreground, muted) =
            (theme.border, theme.popover, theme.popover_foreground, theme.muted_foreground);
        let this = cx.entity().downgrade();
        let (on_query, on_confirm) = (this.clone(), this.clone());
        let files = finder.files.clone().unwrap_or_default();
        let items: Vec<CommandItem> = finder
            .shown
            .iter()
            .filter_map(|&index| files.get(index))
            .map(|path| {
                // The name first, then the folder it is in, quieter.
                let (folder, name) = match path.rsplit_once('/') {
                    Some((folder, name)) => (SharedString::from(folder.to_string()), SharedString::from(name.to_string())),
                    None => (SharedString::default(), SharedString::from(path.clone())),
                };
                CommandItem::new().label(path.clone()).child(move |_, _| {
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(div().flex_none().text_sm().child(name.clone()))
                        .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(folder.clone()))
                })
            })
            .collect();
        self.backdrop("finder-backdrop", cx).flex().justify_center().items_start().child(
            div()
                .id("finder")
                .mt(px(72.))
                .w(px(640.))
                .max_w(relative(0.9))
                .h_auto()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .bg(popover)
                .text_color(foreground)
                .shadow_lg()
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when_some(finder.beside, |d, axis| {
                    d.child(div().id("finder-beside").px_3().pt_2().text_xs().text_color(muted).child(match axis {
                        Axis::Horizontal => t("Choose a file to show to the right of the one showing."),
                        Axis::Vertical => t("Choose a file to show below the one showing."),
                    }))
                })
                .child(
                    Command::new(&finder.state)
                        .items(items)
                        // The list is already the answer to what was typed.
                        .filterable(false)
                        .placeholder(t("Type a file name\u{2026}"))
                        .bordered(false)
                        .empty(|_, _, _| div().px_3().py_2().text_sm().child(t("No file matches")))
                        .on_query(move |query, _, cx| {
                            let _ = on_query.update(cx, |this, cx| this.finder_query(query, cx));
                        })
                        .on_confirm(move |index, window, cx| {
                            let _ = on_confirm.update(cx, |this, cx| this.finder_confirm(index.row, window, cx));
                        })
                        .on_cancel(move |window, cx| {
                            let _ = this.update(cx, |this, cx| this.close_finder(window, cx));
                        }),
                ),
        )
    }

    fn render_tasks(&self, list: &TaskList, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, popover, foreground, muted) =
            (theme.border, theme.popover, theme.popover_foreground, theme.muted_foreground);
        let this = cx.entity().downgrade();
        let (on_query, on_confirm) = (this.clone(), this.clone());
        let found = list.tasks.as_deref().unwrap_or_default();
        let items: Vec<CommandItem> = list
            .shown
            .iter()
            .filter_map(|&index| found.get(index))
            .map(|task| {
                // The command, then the file that describes it, quieter.
                let (command, source) = (SharedString::from(task.command.clone()), task.source);
                CommandItem::new().label(task.command.clone()).child(move |_, _| {
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(div().flex_1().min_w_0().truncate().text_sm().child(command.clone()))
                        .child(div().flex_none().text_xs().text_color(muted).child(source))
                })
            })
            .collect();
        self.backdrop("tasks-backdrop", cx).flex().justify_center().items_start().child(
            div()
                .id("tasks")
                .mt(px(72.))
                .w(px(560.))
                .max_w(relative(0.9))
                .h_auto()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .bg(popover)
                .text_color(foreground)
                .shadow_lg()
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Command::new(&list.state)
                        .items(items)
                        // The list is already the answer to what was typed.
                        .filterable(false)
                        .placeholder(t("Type a task name\u{2026}"))
                        .bordered(false)
                        .empty(|_, _, _| div().px_3().py_2().text_sm().child(t("No tasks found in this project")))
                        .on_query(move |query, _, cx| {
                            let _ = on_query.update(cx, |this, cx| this.tasks_query(query, cx));
                        })
                        .on_confirm(move |index, window, cx| {
                            let _ = on_confirm.update(cx, |this, cx| this.tasks_confirm(index.row, window, cx));
                        })
                        .on_cancel(move |window, cx| {
                            let _ = this.update(cx, |this, cx| this.close_tasks(window, cx));
                        }),
                ),
        )
    }

    fn render_settings(&self, view: &Entity<SettingsView>, cx: &mut Context<Self>) -> impl IntoElement {
        self.backdrop("settings-backdrop", cx)
            .flex()
            .justify_center()
            .items_center()
            .bg(gpui_kit::black().opacity(0.4))
            .child(view.clone())
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let rows = self.recent_rows(cx);
        let has_recent = !rows.is_empty();
        v_flex().id("welcome").test_support().size_full().items_center().justify_center().child(
            v_flex()
                .w(px(520.))
                .max_w(relative(0.9))
                .gap_1()
                .child(
                    h_flex()
                        .px_3()
                        .gap_3()
                        .child(img(crate::assets::LOGO).flex_none().size(px(40.)))
                        .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).child("Wercode")),
                )
                .child(
                    div()
                        .px_3()
                        .pb_4()
                        .text_sm()
                        .text_color(muted)
                        .child(t("A terminal with your files and git next to it. Open a folder to start.")),
                )
                .child(self.open_folder_row(cx))
                .when(has_recent, |d| {
                    d.child(div().my_2().h(px(1.)).bg(border))
                        .child(div().px_3().py_1().text_xs().text_color(muted).child(t("RECENT PROJECTS")))
                        .children(rows)
                }),
        )
    }

    fn render_panel(&self, project: &Project, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, selected_bg) = (theme.border, theme.secondary_hover);
        let tab = |id: &'static str, icon: Icon, tooltip: &'static str, kind: PanelKind| {
            let selected = self.panel == kind;
            div().rounded_md().when(selected, |d| d.bg(selected_bg)).child(
                Button::new(id).ghost().small().icon(icon).tooltip(tooltip).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        // Unlike the shortcut, a click never hides the panel under the pointer.
                        this.panel = kind;
                        this.state.panel = kind.name().into();
                        this.save();
                        cx.notify();
                    },
                )),
            )
        };
        let body = match self.panel {
            PanelKind::Files => project.files.clone().into_any_element(),
            PanelKind::Search => project.search.clone().into_any_element(),
            PanelKind::Git => project.changes.clone().into_any_element(),
            PanelKind::Problems => project.problems.clone().into_any_element(),
        };

        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .h(px(36.))
                    .flex_none()
                    .px_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(border)
                    .child(tab("panel-files", Icon::new(IconName::Folder), t("Files"), PanelKind::Files))
                    .child(tab(
                        "panel-search",
                        Icon::new(IconName::Search),
                        t("Search (Ctrl+Shift+F)"),
                        PanelKind::Search,
                    ))
                    .child(tab("panel-git", Icon::new(Lucide::GitBranch), t("Git (Ctrl+Shift+G)"), PanelKind::Git))
                    .child(tab(
                        "panel-problems",
                        Icon::new(IconName::TriangleAlert),
                        t("Problems (Ctrl+Shift+M)"),
                        PanelKind::Problems,
                    )),
            )
            .child(div().flex_1().min_h_0().child(body))
    }

    /// Errors and warnings in the project, while a language server checks
    /// it or something is reported. A click shows the list.
    fn render_checks(&self, project: &Project, cx: &mut Context<Self>) -> Option<AnyElement> {
        let checker = crate::lsp::checker(cx)?;
        let servers = checker.read(cx).servers().clone();
        let (errors, warnings) = servers.counts(&project.root);
        if errors + warnings == 0 && !servers.is_checking(&project.root) {
            return None;
        }
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.secondary_hover);
        let count = |severity: Severity, n: usize| {
            let icon = if n > 0 { severity_icon(severity, cx) } else { severity_icon(severity, cx).text_color(muted) };
            h_flex().gap_1().items_center().child(icon.xsmall()).child(n.to_string())
        };
        Some(
            h_flex()
                .id("status-problems")
                .test_support()
                .flex_none()
                .gap_2()
                .px_1()
                .rounded_sm()
                .hover(|s| s.bg(hover))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    if !(this.panel_open && this.panel == PanelKind::Problems) {
                        this.show_panel(PanelKind::Problems, window, cx);
                    }
                }))
                .child(count(Severity::Error, errors))
                .child(count(Severity::Warning, warnings))
                .into_any_element(),
        )
    }

    /// The language server of the file that is showing: what it is doing,
    /// or how to install one when there is none.
    fn render_server(&self, project: &Project, cx: &mut Context<Self>) -> Option<AnyElement> {
        let checker = crate::lsp::checker(cx)?;
        let path = project.terminals.read(cx).shown_file(cx)?;
        let report = checker.read(cx).servers().report(&path)?;
        let theme = cx.theme();
        let (hover, link, danger) = (theme.secondary_hover, theme.link, theme.danger);
        let item = h_flex().id("status-server").test_support().flex_none().gap_1().items_center().px_1();
        let action = |id: &'static str, label: &'static str| {
            div().id(id).test_support().px_1().rounded_sm().text_color(link).hover(|s| s.bg(hover)).child(label)
        };
        let text = |text: String| div().max_w(px(420.)).truncate().child(text);

        // A server Wercode can fetch itself: the button does that, and what
        // happens shows here until the server starts.
        if let Some(name) = report.download.clone() {
            let fetching = checker.read(cx).fetching(&name).cloned();
            let fetch = {
                let (checker, name) = (checker.clone(), name.clone());
                action("status-server-install", t("Install")).on_click(move |_: &ClickEvent, _, cx| {
                    checker.update(cx, |checker, cx| checker.fetch(name.clone(), cx));
                })
            };
            return Some(match fetching {
                Some(crate::lsp::Fetching::Running) => item
                    .child(Icon::new(Lucide::LoaderCircle).xsmall())
                    .child(text(tf("Installing {name}\u{2026}", &[("name", &name)])))
                    .into_any_element(),
                Some(crate::lsp::Fetching::Failed(error)) => item
                    .text_color(danger)
                    .child(Icon::new(IconName::TriangleAlert).xsmall())
                    .child(text(tf("Could not install {name}: {error}", &[("name", &name), ("error", &error)])))
                    .child(fetch.tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(t("Try again")).build(window, cx)
                    }))
                    .into_any_element(),
                None => item
                    .child(Icon::new(Lucide::Info).xsmall())
                    .child(text(tf("No language server for {language}", &[("language", &report.language)])))
                    .child(fetch.tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tf("Download {name} into Wercode's own folder", &[("name", &name)]))
                            .build(window, cx)
                    }))
                    .into_any_element(),
            });
        }
        if let Some(command) = report.install {
            let install = action("status-server-install", t("Install")).on_click(cx.listener(
                move |this, _: &ClickEvent, window, cx| this.run_in_terminal(&command, window, cx),
            ));
            return Some(
                item.child(Icon::new(Lucide::Info).xsmall())
                    .child(text(tf("No language server for {language}", &[("language", &report.language)])))
                    .child(install)
                    .into_any_element(),
            );
        }
        // The one worth telling about: a failure, then work, then the rest.
        let rank = |status: &Status| match status {
            Status::Failed(_) => 0,
            Status::Busy(_) => 1,
            Status::Starting => 2,
            Status::Ready => 3,
            Status::Missing => 4,
        };
        let Some((name, status)) = report.servers.iter().min_by_key(|(_, status)| rank(status)) else {
            let none = tf("No language server for {language}", &[("language", &report.language)]);
            return Some(item.child(Icon::new(Lucide::Info).xsmall()).child(text(none)).into_any_element());
        };
        let spinner = || Icon::new(Lucide::LoaderCircle).xsmall();
        Some(match status {
            Status::Starting => {
                item.child(spinner()).child(text(tf("{name}: starting", &[("name", name)]))).into_any_element()
            }
            Status::Busy(work) => item
                .child(spinner())
                .child(text(tf("{name}: {activity}", &[("name", name), ("activity", work)])))
                .into_any_element(),
            Status::Ready | Status::Missing => {
                item.child(Icon::new(Lucide::CircleCheck).xsmall()).child(text(name.clone())).into_any_element()
            }
            Status::Failed(detail) => {
                let said = if detail.is_empty() {
                    tf("{name} stopped", &[("name", name)])
                } else {
                    tf("{name} stopped: {error}", &[("name", name), ("error", detail)])
                };
                let restart = action("status-server-restart", t("Restart")).on_click(cx.listener(
                    |_, _: &ClickEvent, _, cx| {
                        if let Some(checker) = crate::lsp::checker(cx) {
                            checker.read(cx).servers().restart();
                        }
                    },
                ));
                item.text_color(danger)
                    .child(Icon::new(IconName::TriangleAlert).xsmall())
                    .child(text(said))
                    .child(restart)
                    .into_any_element()
            }
        })
    }

    /// Runs a command, such as installing a language server, in a new terminal tab.
    fn run_in_terminal(&mut self, command: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(terminals) = self.project.as_ref().map(|p| p.terminals.clone()) else { return };
        terminals.update(cx, |area, cx| area.run_in_new_tab(command, window, cx));
        cx.notify();
    }

    /// Branch, unpushed commits and changes. The branch takes the colour of
    /// the worst thing true about the working tree.
    fn render_status(&self, project: &Project, cx: &mut Context<Self>) -> impl IntoElement {
        let activity = project.changes.read(cx).activity();
        let update = update::status_bar(cx).map(|status| self.render_update(status, cx).into_any_element());
        let checks = self.render_checks(project, cx);
        let server = self.render_server(project, cx);
        // Who last changed the line under the cursor, and when.
        let blame = project.terminals.read(cx).shown_blame(cx).map(|blame| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs() as i64);
            let when = crate::panel::graph::ago_text(wercode_core::history::ago(blame.time, now));
            let said = tf(
                "{author}, {when} \u{00b7} {summary}",
                &[("author", &blame.author), ("when", &when), ("summary", &blame.summary)],
            );
            div().id("status-blame").test_support().min_w_0().max_w(px(520.)).truncate().child(said)
        });
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let item = || h_flex().flex_none().gap_1().items_center();

        let git = match project.git.as_deref() {
            Some(git) => {
                let (changed, conflicts) = (git.changed(), git.conflicts());
                let color = if conflicts > 0 {
                    theme.danger
                } else if changed > 0 {
                    theme.warning
                } else if git.ahead > 0 {
                    theme.link
                } else {
                    theme.success
                };
                let branch = match &git.branch {
                    Some(name) => name.clone(),
                    None => tf("detached at {commit}", &[("commit", &git.head)]),
                };
                h_flex()
                    .id("status-git")
                    .test_support()
                    .gap_3()
                    .child(
                        item()
                            .id("status-branch")
                            .test_support()
                            .px_1()
                            .rounded_sm()
                            .hover(|s| s.bg(theme.secondary_hover))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_branches(window, cx)))
                            .text_color(color)
                            .child(Icon::new(Lucide::GitBranch).xsmall())
                            .child(if changed > 0 { format!("{branch}*") } else { branch }),
                    )
                    .child(
                        item()
                            .id("status-worktrees")
                            .test_support()
                            .px_1()
                            .rounded_sm()
                            .hover(|s| s.bg(theme.secondary_hover))
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(t("Worktrees: a folder for each branch")).build(window, cx)
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_worktrees(window, cx)))
                            .child(Icon::new(Lucide::FolderGit2).xsmall()),
                    )
                    .when(git.ahead > 0, |d| {
                        d.child(
                            item()
                                .text_color(theme.link)
                                .child(Icon::new(IconName::ArrowUp).xsmall())
                                .child(tn(git.ahead as u64, "{n} commit to push", "{n} commits to push", &[])),
                        )
                    })
                    .when(git.behind > 0, |d| {
                        d.child(
                            item()
                                .child(Icon::new(IconName::ArrowDown).xsmall())
                                .child(tn(git.behind as u64, "{n} commit to pull", "{n} commits to pull", &[])),
                        )
                    })
                    .when(git.branch.is_some() && !git.has_upstream, |d| d.child(item().child(t("not published"))))
                    .when(changed > 0, |d| {
                        d.child(item().text_color(theme.warning).child(tn(changed as u64, "{n} change", "{n} changes", &[])))
                    })
                    .when(conflicts > 0, |d| {
                        d.child(
                            item()
                                .id("status-conflicts")
                                .test_support()
                                .px_1()
                                .rounded_sm()
                                .hover(|s| s.bg(theme.secondary_hover))
                                .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(t("Resolve the conflicts in the list of all changes")).build(window, cx))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_all_changes(false, None, window, cx)))
                                .text_color(theme.danger)
                                .child(Icon::new(IconName::TriangleAlert).xsmall())
                                .child(tn(conflicts as u64, "{n} conflict", "{n} conflicts", &[])),
                        )
                    })
                    .when(changed == 0, |d| d.child(item().child(t("clean"))))
                    .into_any_element()
            }
            None => item().child(t("No git repository")).into_any_element(),
        };

        h_flex()
            .id("status-bar")
            .test_support()
            .h(px(24.))
            .flex_none()
            .px_3()
            .gap_3()
            .border_t_1()
            .border_color(border)
            .bg(theme.sidebar)
            .text_xs()
            .text_color(muted)
            .child(git)
            .when_some(activity.label(), |d, label| {
                let failed = matches!(activity, RemoteActivity::Failed(_));
                d.child(
                    item()
                        .id("status-git-activity")
                        .test_support()
                        .px_1()
                        .rounded_sm()
                        .child(if failed {
                            Icon::new(IconName::TriangleAlert).xsmall()
                        } else {
                            Icon::new(Lucide::RefreshCw).xsmall()
                        })
                        .child(label)
                        // What git said is in the panel.
                        .when(failed, |d| {
                            d.text_color(theme.danger)
                                .hover(|s| s.bg(theme.secondary_hover))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    if !(this.panel_open && this.panel == PanelKind::Git) {
                                        this.show_panel(PanelKind::Git, window, cx);
                                    }
                                }))
                        }),
                )
            })
            .children(checks)
            .child(div().flex_1())
            .children(blame)
            .children(server)
            .children(update)
            .child(
                div()
                    .id("status-palette")
                    .px_1()
                    .rounded_sm()
                    .hover(|s| s.bg(theme.secondary_hover))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_palette(window, cx)))
                    .child(t("Commands  Ctrl+Shift+P")),
            )
    }

    /// A download in progress, an update waiting for a restart, or the answer
    /// to a check asked for by hand.
    fn render_update(&self, status: update::Status, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let item = h_flex().id("status-update").test_support().flex_none().gap_1().items_center().px_1().rounded_sm();
        match status {
            update::Status::Ready(version) => item
                .text_color(theme.link)
                .hover(|s| s.bg(theme.secondary_hover))
                .child(Icon::new(IconName::ArrowDown).xsmall())
                .child(tf("Restart to update to {version}", &[("version", &version)]))
                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| update::restart(cx))),
            update::Status::Downloading(version) => item
                .child(Icon::new(Lucide::RefreshCw).xsmall())
                .child(tf("Downloading Wercode {version}\u{2026}", &[("version", &version)])),
            update::Status::Checking => item.child(t("Checking for updates\u{2026}")),
            update::Status::UpToDate => item.child(t("Wercode is up to date")),
            update::Status::Failed(_) => item
                .text_color(theme.danger)
                .child(Icon::new(IconName::TriangleAlert).xsmall())
                .child(t("Could not check for updates")),
            update::Status::Idle | update::Status::Off => item,
        }
    }

    fn render_body(&self, project: &Project, cx: &mut Context<Self>) -> AnyElement {
        if !self.panel_open || self.zen.is_some() {
            return project.terminals.clone().into_any_element();
        }
        let settings = &cx.global::<AppState>().settings;
        let on_right = settings.panel_on_right();
        let width = self.state.panel_width.unwrap_or(settings.panel_width).clamp(200., 800.);
        let panel = resizable_panel().size(px(width)).size_range(px(200.)..px(800.)).child(self.render_panel(project, cx));
        let terminals = resizable_panel().child(project.terminals.clone());
        let this = cx.entity().downgrade();
        let panel_index = if on_right { 1 } else { 0 };
        // The two sides are different groups: each keeps its own divider position.
        h_resizable(if on_right { "workspace-right" } else { "workspace-left" })
            .map(|group| if on_right { group.child(terminals).child(panel) } else { group.child(panel).child(terminals) })
            .on_resize(move |sizes, _, cx| {
                let Some(width) = sizes.read(cx).sizes().get(panel_index).copied() else { return };
                let _ = this.update(cx, |this, cx| this.remember_panel_width(width.into(), cx));
            })
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let zen = self.zen.as_ref().map(|zen| zen.note.is_some());
        let (body, status) = match &self.project {
            Some(project) => (
                self.render_body(project, cx),
                zen.is_none().then(|| self.render_status(project, cx).into_any_element()),
            ),
            None => (self.render_welcome(cx).into_any_element(), None),
        };

        v_flex()
            .id("workspace")
            .key_context("Workspace")
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_weight(FontWeight::MEDIUM)
            .on_action(cx.listener(|this, _: &TogglePanel, _, cx| this.toggle_panel(cx)))
            .on_action(cx.listener(|this, _: &ToggleZen, window, cx| this.toggle_zen(window, cx)))
            .on_action(cx.listener(|this, _: &OpenFolder, window, cx| this.pick_folder(window, cx)))
            .on_action(cx.listener(|this, _: &ShowFiles, window, cx| {
                if !(this.panel_open && this.panel == PanelKind::Files) {
                    this.show_panel(PanelKind::Files, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ShowSearch, window, cx| {
                this.palette = None;
                this.show_panel(PanelKind::Search, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowGit, window, cx| this.show_panel(PanelKind::Git, window, cx)))
            .on_action(cx.listener(|this, _: &ShowProblems, window, cx| {
                this.palette = None;
                this.show_panel(PanelKind::Problems, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RestartLanguageServers, _, cx| {
                this.palette = None;
                if let Some(checker) = crate::lsp::checker(cx) {
                    checker.read(cx).servers().restart();
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)))
            .on_action(cx.listener(|this, _: &GoToSymbol, window, cx| this.open_symbols(false, window, cx)))
            .on_action(cx.listener(|this, _: &GoToProjectSymbol, window, cx| this.open_symbols(true, window, cx)))
            .on_action(cx.listener(|this, _: &SwitchBranch, window, cx| this.open_branches(window, cx)))
            .on_action(cx.listener(|this, _: &SwitchWorktree, window, cx| this.open_worktrees(window, cx)))
            .on_action(cx.listener(|this, _: &git_panel::Pull, window, cx| this.git_remote(Remote::Pull, window, cx)))
            .on_action(cx.listener(|this, _: &git_panel::Push, window, cx| this.git_remote(Remote::Push, window, cx)))
            .on_action(
                cx.listener(|this, _: &git_panel::Fetch, window, cx| this.git_remote(Remote::Fetch, window, cx)),
            )
            .on_action(cx.listener(|this, _: &git_panel::Commit, window, cx| this.git_commit(window, cx)))
            .on_action(cx.listener(|this, _: &git_panel::StageAll, window, cx| this.git_stage_all(window, cx)))
            .on_action(cx.listener(|this, _: &git_panel::ShowAllChanges, window, cx| {
                this.open_all_changes(false, None, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CheckForUpdates, _, cx| {
                this.palette = None;
                update::check(true, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CloseForUpdate, window, cx| {
                if this.should_close(window, cx) {
                    this.closing = true;
                    window.remove_window();
                }
            }))
            .on_action(cx.listener(|this, _: &NewWindow, window, cx| this.new_window(window, cx)))
            .on_action(cx.listener(|this, _: &SwitchProject, _, cx| {
                this.palette = None;
                this.switcher_open = true;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &QuickOpen, window, cx| {
                if this.finder.is_some() { this.close_finder(window, cx) } else { this.open_finder(window, cx) }
            }))
            .on_action(cx.listener(|this, _: &CommandPalette, window, cx| {
                if this.palette.is_some() { this.close_palette(window, cx) } else { this.open_palette(window, cx) }
            }))
            .on_action(cx.listener(|this, _: &RunTask, window, cx| {
                if this.tasks.is_some() { this.close_tasks(window, cx) } else { this.open_tasks(window, cx) }
            }))
            .on_action(cx.listener(|this, _: &RerunTask, window, cx| this.rerun_task(window, cx)))
            .on_action(cx.listener(|this, a: &Save, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &GoToDefinition, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &ToggleTerminal, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &NewTab, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &SplitRight, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &SplitDown, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &ClosePane, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &CloseTab, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &NextTab, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &PrevTab, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &SwitchTab, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &SwitchTabBack, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &NextPane, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &PrevPane, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &Find, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &RenameTab, window, cx| this.forward(a, window, cx)))
            .when(zen.is_none(), |d| d.child(self.render_title(cx)))
            .child(div().flex_1().min_h_0().child(body))
            .children(status)
            .when(zen == Some(true), |d| d.child(self.render_zen_note(cx)))
            .when(self.switcher_open, |d| d.child(self.render_switcher(cx)))
            .when_some(self.palette.clone(), |d, palette| d.child(self.render_palette(&palette, cx)))
            .when_some(self.finder.as_ref(), |d, finder| d.child(self.render_finder(finder, cx)))
            .when_some(self.tasks.as_ref(), |d, list| d.child(self.render_tasks(list, cx)))
            .when_some(self.symbols.as_ref().map(|(picker, _)| picker.clone()), |d, picker| {
                d.child(self.backdrop("symbols-backdrop", cx).flex().justify_center().items_start().child(picker))
            })
            .when_some(self.branches.as_ref(), |d, menu| d.child(self.render_branches(menu, cx)))
            .when_some(self.worktrees.as_ref(), |d, menu| d.child(self.render_worktrees(menu, cx)))
            .when_some(self.settings.as_ref().map(|(view, _)| view.clone()), |d, view| {
                d.child(self.render_settings(&view, cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, WindowOptions};
    use wercode_core::config::Settings;
    use wercode_core::state::{self, State};

    use super::{PanelKind, Workspace, normalize};

    struct Fixture {
        window: AnyWindowHandle,
        workspace: Entity<Workspace>,
        dir: std::path::PathBuf,
        state_path: std::path::PathBuf,
    }

    fn open_with(cx: &mut TestAppContext, name: &str, project: bool) -> Fixture {
        // Real shells run in these tests, and their reader threads wake the
        // window whenever they like; the scheduler must not call that a bug.
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("wercode-ws-{name}-{}", std::process::id()));
        for project in ["one", "two"] {
            std::fs::create_dir_all(dir.join(project)).unwrap();
        }
        let state_path = dir.join("state.json");
        let root = project.then(|| normalize(&dir.join("one")));
        let (window, workspace) = cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::apply(Settings::default(), cx);
            crate::term::init(cx);
            crate::editor::init(cx);
            super::init(cx);
            let path = state_path.clone();
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| Workspace::new(root, State::default(), Some(path), window, cx))
            })
            .unwrap()
        });
        Fixture { window, workspace, dir, state_path }
    }

    fn open(cx: &mut TestAppContext, name: &str) -> Fixture {
        open_with(cx, name, true)
    }

    fn press(cx: &mut TestAppContext, window: AnyWindowHandle, keys: &[&str]) {
        cx.update_window(window, |_, window, cx| {
            for key in keys {
                window.press(key, cx);
            }
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn zen_mode_leaves_only_the_tabs(cx: &mut TestAppContext) {
        let f = open(cx, "zen");
        let shown = |cx: &mut TestAppContext, id: &'static str| {
            cx.update_window(f.window, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(id).is_some()
            })
            .unwrap()
        };
        let panel = |cx: &mut TestAppContext| f.workspace.read_with(cx, |w, _| (w.zen.is_some(), w.panel_open, w.panel));
        assert!(shown(cx, "project-switcher") && shown(cx, "status-bar"));
        assert_eq!(panel(cx), (false, true, PanelKind::Files));

        press(cx, f.window, &["ctrl-shift-z"]);
        assert_eq!(panel(cx), (true, true, PanelKind::Files));
        assert!(!shown(cx, "project-switcher") && !shown(cx, "status-bar") && !shown(cx, "panel-files"));
        assert!(shown(cx, "terminal-area") && shown(cx, "zen-note"));
        // The note on how to leave goes after a while.
        cx.executor().advance_clock(std::time::Duration::from_secs(5));
        cx.run_until_parked();
        assert!(!shown(cx, "zen-note") && !shown(cx, "status-bar"));
        // The same keys bring everything back, the panel as it was.
        press(cx, f.window, &["ctrl-shift-z"]);
        assert_eq!(panel(cx), (false, true, PanelKind::Files));
        assert!(shown(cx, "project-switcher") && shown(cx, "status-bar") && shown(cx, "panel-files"));

        // Asking for a panel ends zen mode and shows it.
        press(cx, f.window, &["ctrl-shift-z", "ctrl-shift-g"]);
        assert_eq!(panel(cx), (false, true, PanelKind::Git));
        press(cx, f.window, &["ctrl-shift-b", "ctrl-shift-z", "ctrl-shift-b"]);
        assert_eq!(panel(cx), (false, true, PanelKind::Git));
        // Terminal keys go on working in zen mode.
        press(cx, f.window, &["ctrl-shift-z", "ctrl-shift-t"]);
        let tabs = f.workspace.read_with(cx, |w, cx| w.terminals().read(cx).tab_count());
        assert_eq!(tabs, 2);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn panel_shortcuts_are_remembered(cx: &mut TestAppContext) {
        let f = open(cx, "panel");
        let panel = |cx: &mut TestAppContext| f.workspace.read_with(cx, |w, _| (w.panel_open, w.panel));
        assert_eq!(panel(cx), (true, PanelKind::Files));

        press(cx, f.window, &["ctrl-shift-b"]);
        assert_eq!(panel(cx), (false, PanelKind::Files));
        assert!(!state::load_from(&f.state_path).panel_open);

        // Asking for a panel opens the side bar; asking again for the same one hides it.
        press(cx, f.window, &["ctrl-shift-g"]);
        assert_eq!(panel(cx), (true, PanelKind::Git));
        press(cx, f.window, &["ctrl-shift-f"]);
        assert_eq!(panel(cx), (true, PanelKind::Search));
        let saved = state::load_from(&f.state_path);
        assert_eq!((saved.panel_open, saved.panel.as_str()), (true, "search"));
        press(cx, f.window, &["ctrl-shift-f"]);
        assert_eq!(panel(cx), (false, PanelKind::Search));

        // Terminal shortcuts still work with the workspace around them.
        press(cx, f.window, &["ctrl-shift-t"]);
        // The shells are still running inside the folder, so this may not succeed.
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn switching_project_updates_the_recent_list(cx: &mut TestAppContext) {
        let f = open(cx, "switch");
        let one = normalize(&f.dir.join("one"));
        let two = normalize(&f.dir.join("two"));
        assert_eq!(state::load_from(&f.state_path).recent, vec![one.clone()]);

        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.switch_to(two.clone(), window, cx));
        })
        .unwrap();
        assert_eq!(f.workspace.read_with(cx, |w, _| w.root()), Some(two.clone()));
        let saved = state::load_from(&f.state_path);
        assert_eq!(saved.recent, vec![two.clone(), one.clone()]);
        assert_eq!(saved.last_project(), Some(two.clone()));

        // The switcher lists both; clicking the older one goes back to it.
        cx.update_window(f.window, |_, window, cx| {
            window.click("project-switcher", cx);
        })
        .unwrap();
        assert!(f.workspace.read_with(cx, |w, _| w.switcher_open));
        cx.update_window(f.window, |_, window, cx| {
            window.click(("recent", 1usize), cx);
        })
        .unwrap();
        assert_eq!(f.workspace.read_with(cx, |w, _| (w.root(), w.switcher_open)), (Some(one.clone()), false));
        assert_eq!(state::load_from(&f.state_path).recent, vec![one, two]);
        // The shells are still running inside the folder, so this may not succeed.
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn palette_opens_runs_a_command_and_closes(cx: &mut TestAppContext) {
        let f = open(cx, "palette");
        let palette = |cx: &mut TestAppContext| f.workspace.read_with(cx, |w, _| w.palette.is_some());
        press(cx, f.window, &["ctrl-shift-p"]);
        assert!(palette(cx));
        press(cx, f.window, &["escape"]);
        assert!(!palette(cx));

        // Typing narrows the list to one command; Enter runs it.
        press(cx, f.window, &["ctrl-shift-p"]);
        cx.update_window(f.window, |_, window, cx| window.input("toggle side", cx)).unwrap();
        press(cx, f.window, &["enter"]);
        cx.run_until_parked();
        assert!(!palette(cx));
        assert!(!f.workspace.read_with(cx, |w, _| w.panel_open));

        // A terminal command chosen here runs in the terminals, exactly once.
        assert_eq!(f.workspace.read_with(cx, |w, cx| w.pane_count(cx)), 1);
        press(cx, f.window, &["ctrl-shift-p"]);
        cx.update_window(f.window, |_, window, cx| window.input("split right", cx)).unwrap();
        press(cx, f.window, &["enter"]);
        cx.run_until_parked();
        assert!(!palette(cx));
        assert_eq!(f.workspace.read_with(cx, |w, cx| w.pane_count(cx)), 2);

        // Splitting stops at four terminals in a tab, however often it is asked for.
        press(cx, f.window, &["ctrl-shift-d", "ctrl-shift-e", "ctrl-shift-d", "ctrl-shift-d", "ctrl-shift-e"]);
        assert_eq!(f.workspace.read_with(cx, |w, cx| w.pane_count(cx)), 4);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn git_panel_stages_unstages_and_commits(cx: &mut TestAppContext) {
        use std::process::Command;
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        // The repository has to exist before the window opens the folder.
        let repo = std::env::temp_dir().join(format!("wercode-ws-gitpanel-{}", std::process::id())).join("one");
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success(), "{args:?}");
        };
        git(&["init", "-q", "-b", "trunk"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        std::fs::write(repo.join("b.txt"), "b").unwrap();

        let f = open(cx, "gitpanel");
        press(cx, f.window, &["ctrl-shift-g"]);
        cx.run_until_parked();
        let counts = |cx: &mut TestAppContext| {
            cx.run_until_parked();
            f.workspace.read_with(cx, |w, _| {
                let git = w.git().expect("a repository");
                (git.staged().count(), git.unstaged().count())
            })
        };
        assert_eq!(counts(cx), (0, 2));
        let panel = f.workspace.read_with(cx, |w, _| w.changes()).unwrap();

        // The plus on a row stages that file; the minus takes it back out.
        cx.update_window(f.window, |_, window, cx| window.click(("git-stage", 0usize), cx)).unwrap();
        assert_eq!(counts(cx), (1, 1));
        cx.update_window(f.window, |_, window, cx| window.click(("git-unstage", 0usize), cx)).unwrap();
        assert_eq!(counts(cx), (0, 2));
        cx.update_window(f.window, |_, window, cx| window.click("git-stage-all", cx)).unwrap();
        assert_eq!(counts(cx), (2, 0));
        cx.update_window(f.window, |_, window, cx| window.click("git-unstage-all", cx)).unwrap();
        assert_eq!(counts(cx), (0, 2));

        // No message, no commit.
        cx.update_window(f.window, |_, window, cx| window.click("git-commit", cx)).unwrap();
        assert_eq!(counts(cx), (0, 2));
        assert!(panel.read_with(cx, |p, _| p.error().is_some()));

        // Only the staged file goes into the commit.
        cx.update_window(f.window, |_, window, cx| window.click(("git-stage", 0usize), cx)).unwrap();
        assert_eq!(counts(cx), (1, 1));
        cx.update_window(f.window, |_, window, cx| {
            panel.update(cx, |p, cx| p.write_message("First", window, cx));
        })
        .unwrap();
        cx.update_window(f.window, |_, window, cx| window.click("git-commit", cx)).unwrap();
        assert_eq!(counts(cx), (0, 1));
        assert_eq!(panel.read_with(cx, |p, cx| (p.error().map(str::to_string), p.message_text(cx), p.is_busy())), (None, String::new(), false));

        // With nothing staged the button takes everything.
        cx.update_window(f.window, |_, window, cx| {
            panel.update(cx, |p, cx| p.write_message("Second", window, cx));
        })
        .unwrap();
        cx.update_window(f.window, |_, window, cx| window.click("git-commit", cx)).unwrap();
        assert_eq!(counts(cx), (0, 0));
        let log = Command::new("git").arg("-C").arg(&repo).args(["log", "--format=%s"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).lines().collect::<Vec<_>>(), ["Second", "First"]);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn git_panel_shows_changes_discards_pushes_and_switches_branch(cx: &mut TestAppContext) {
        use std::process::Command;
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let repo = project_dir("gitmore");
        let remote = repo.parent().unwrap().join("remote.git");
        let git = |args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success(), "{args:?}");
        };
        git(&["init", "-q", "-b", "trunk"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "First"]);
        assert!(Command::new("git").args(["init", "-q", "--bare"]).arg(&remote).status().unwrap().success());
        git(&["remote", "add", "origin", &remote.to_string_lossy()]);
        std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(repo.join("new.txt"), "fresh\n").unwrap();

        let f = open(cx, "gitmore");
        press(cx, f.window, &["ctrl-shift-g"]);
        let status = |cx: &mut TestAppContext| {
            cx.run_until_parked();
            f.workspace.read_with(cx, |w, _| w.git().expect("a repository"))
        };
        assert_eq!(status(cx).unstaged().count(), 2);
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        let click = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
            cx.update_window(f.window, |_, window, cx| window.click(id, cx)).unwrap();
            cx.run_until_parked();
        };

        // A click on a changed file shows what changed: one line added.
        click(cx, ("git-change", 0usize).into());
        let diff = area.read_with(cx, |a, _| a.active_diff()).expect("a tab of changes");
        assert_eq!(diff.read_with(cx, |d, cx| d.counts(cx)), Some((1, 0)));
        // The same file again is the same tab; a new file is all additions.
        click(cx, ("git-change", 0usize).into());
        assert_eq!(area.read_with(cx, |a, _| a.tab_count()), 2);
        click(cx, ("git-change", 1usize).into());
        let fresh = area.read_with(cx, |a, _| a.active_diff()).unwrap();
        assert_eq!(fresh.read_with(cx, |d, cx| d.counts(cx)), Some((1, 0)));
        // Tabs of changes are not written down for the next launch.
        assert_eq!(area.read_with(cx, |a, cx| a.layout(cx).tabs.len()), 1);

        // Discard asks first; Cancel keeps the edit, Discard puts the file back.
        click(cx, ("git-discard", 0usize).into());
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
        assert_eq!(status(cx).unstaged().count(), 2);
        click(cx, ("git-discard", 0usize).into());
        cx.simulate_prompt_answer("Discard");
        assert_eq!(status(cx).unstaged().count(), 1);
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap().replace("\r\n", "\n"), "one\ntwo\n");
        // The tab that showed those changes has nothing left to show.
        assert_eq!(diff.read_with(cx, |d, _| d.note().map(str::to_string)), Some("No changes in this file.".into()));

        // The first push publishes the branch.
        assert!(!status(cx).has_upstream);
        click(cx, "git-push".into());
        assert!(status(cx).has_upstream);
        let panel = f.workspace.read_with(cx, |w, _| w.changes()).unwrap();
        assert_eq!(panel.read_with(cx, |p, _| p.error().map(str::to_string)), None);
        click(cx, "git-fetch".into());
        click(cx, "git-pull".into());
        assert_eq!(panel.read_with(cx, |p, _| p.error().map(str::to_string)), None);

        // The branch in the status bar opens the list; a typed name makes a branch.
        click(cx, "status-branch".into());
        assert!(f.workspace.read_with(cx, |w, _| w.branches.is_some()));
        cx.update_window(f.window, |_, window, cx| window.input("feature/one", cx)).unwrap();
        press(cx, f.window, &["enter"]);
        assert_eq!(status(cx).branch.as_deref(), Some("feature/one"));
        assert!(f.workspace.read_with(cx, |w, _| w.branches.is_none()));
        // Clicking a branch in the list switches to it.
        click(cx, "status-branch".into());
        let names = f.workspace.read_with(cx, |w, _| w.branches.as_ref().unwrap().names.clone());
        assert_eq!(names, ["feature/one", "trunk"]);
        click(cx, ("branch", 1usize).into());
        assert_eq!(status(cx).branch.as_deref(), Some("trunk"));
        // A name git will not take is reported in the list, which stays open.
        click(cx, "status-branch".into());
        cx.update_window(f.window, |_, window, cx| window.input("trunk", cx)).unwrap();
        press(cx, f.window, &["enter"]);
        cx.run_until_parked();
        assert!(f.workspace.read_with(cx, |w, _| w.branches.as_ref().is_some_and(|m| m.error.is_some())));
        press(cx, f.window, &["escape"]);
        assert!(f.workspace.read_with(cx, |w, _| w.branches.is_none()));

        // A branch that is on the remote can be deleted here; the list then
        // offers the remote's, and a click on that brings it back.
        click(cx, "status-branch".into());
        click(cx, ("branch", 0usize).into());
        assert_eq!(status(cx).branch.as_deref(), Some("feature/one"));
        click(cx, "git-push".into());
        click(cx, "status-branch".into());
        click(cx, ("branch", 1usize).into());
        assert_eq!(status(cx).branch.as_deref(), Some("trunk"));
        click(cx, "status-branch".into());
        let listed = |cx: &mut TestAppContext| {
            f.workspace.read_with(cx, |w, _| w.branches.as_ref().map(|m| (m.names.clone(), m.remote.clone())).unwrap())
        };
        assert_eq!(listed(cx), (vec!["feature/one".to_string(), "trunk".to_string()], vec![]));
        click(cx, ("branch-delete", 0usize).into());
        wait_until(cx, "the branch to go", |cx| listed(cx).0 == ["trunk"]);
        assert_eq!(listed(cx).1, ["origin/feature/one"]);
        click(cx, ("remote-branch", 0usize).into());
        assert_eq!(status(cx).branch.as_deref(), Some("feature/one"));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn git_shortcuts_commit_push_pull_and_report_in_the_status_bar(cx: &mut TestAppContext) {
        use std::process::Command;

        use crate::panel::{Remote, RemoteActivity};
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let repo = project_dir("gitkeys");
        let remote = repo.parent().unwrap().join("remote.git");
        let git = |args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success(), "{args:?}");
        };
        git(&["init", "-q", "-b", "trunk"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "First"]);
        assert!(Command::new("git").args(["init", "-q", "--bare"]).arg(&remote).status().unwrap().success());
        git(&["remote", "add", "origin", &remote.to_string_lossy()]);
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();

        let f = open(cx, "gitkeys");
        let status = |cx: &mut TestAppContext| {
            cx.run_until_parked();
            f.workspace.read_with(cx, |w, _| w.git().expect("a repository"))
        };
        assert_eq!(status(cx).changed(), 1);
        let panel = f.workspace.read_with(cx, |w, _| w.changes()).unwrap();
        let shown =
            |cx: &mut TestAppContext| f.workspace.read_with(cx, |w, _| w.panel_open && w.panel == PanelKind::Git);
        let activity = |cx: &mut TestAppContext| panel.read_with(cx, |p, _| p.activity());

        // Commit with no message yet: the git panel opens with the keyboard in its field.
        assert!(!shown(cx));
        press(cx, f.window, &["ctrl-shift-enter"]);
        assert!(shown(cx));
        let focused =
            cx.update_window(f.window, |_, window, cx| panel.read(cx).message_focused(window, cx)).unwrap();
        assert!(focused);
        // With a message, the same keys commit everything that changed.
        cx.update_window(f.window, |_, window, cx| panel.update(cx, |p, cx| p.write_message("Second", window, cx)))
            .unwrap();
        press(cx, f.window, &["ctrl-shift-enter"]);
        assert_eq!(status(cx).changed(), 0);
        assert_eq!(panel.read_with(cx, |p, cx| p.message_text(cx)), "");

        // Up pushes, and the first push publishes the branch. The status bar
        // says what is running, since the panel may be hidden.
        assert!(!status(cx).has_upstream);
        press(cx, f.window, &["ctrl-shift-up"]);
        assert_eq!(activity(cx), RemoteActivity::Running(Remote::Push));
        assert!(status(cx).has_upstream);
        assert_eq!(activity(cx), RemoteActivity::Idle);
        press(cx, f.window, &["ctrl-shift-down"]);
        assert_eq!(activity(cx), RemoteActivity::Running(Remote::Pull));
        cx.run_until_parked();
        press(cx, f.window, &["ctrl-shift-r"]);
        cx.run_until_parked();
        assert_eq!(activity(cx), RemoteActivity::Idle);
        assert_eq!(panel.read_with(cx, |p, _| p.error().map(str::to_string)), None);

        // A failure stays in the status bar with the panel hidden; a click on
        // it shows the panel with git's own words.
        git(&["remote", "set-url", "origin", &repo.parent().unwrap().join("gone.git").to_string_lossy()]);
        press(cx, f.window, &["ctrl-shift-g"]);
        assert!(!shown(cx));
        press(cx, f.window, &["ctrl-shift-r"]);
        cx.run_until_parked();
        assert_eq!(activity(cx), RemoteActivity::Failed(Remote::Fetch));
        assert!(panel.read_with(cx, |p, _| p.error().is_some()));
        cx.update_window(f.window, |_, window, cx| window.click("status-git-activity", cx)).unwrap();
        assert!(shown(cx));

        // Stage All is in the palette.
        std::fs::write(repo.join("b.txt"), "new\n").unwrap();
        f.workspace.update(cx, |w, cx| w.refresh_git(cx));
        assert_eq!(status(cx).unstaged().count(), 1);
        press(cx, f.window, &["ctrl-shift-p"]);
        cx.update_window(f.window, |_, window, cx| window.input("stage all", cx)).unwrap();
        press(cx, f.window, &["enter"]);
        assert_eq!((status(cx).staged().count(), status(cx).unstaged().count()), (1, 0));
        assert_eq!(activity(cx), RemoteActivity::Idle);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn the_history_lists_commits_and_shows_what_one_changed(cx: &mut TestAppContext) {
        use std::process::Command;
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let repo = project_dir("history");
        let git = |args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success(), "{args:?}");
        };
        git(&["init", "-q", "-b", "trunk"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "First"]);
        // A side branch merged back makes a second lane.
        git(&["switch", "-q", "-c", "side"]);
        std::fs::write(repo.join("b.txt"), "side\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "Side work"]);
        git(&["switch", "-q", "trunk"]);
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "Second"]);
        git(&["merge", "-q", "--no-ff", "-m", "Merge side", "side"]);

        let f = open(cx, "history");
        press(cx, f.window, &["ctrl-shift-g"]);
        let panel = f.workspace.read_with(cx, |w, _| w.changes()).unwrap();
        let graph = panel.read_with(cx, |p, _| p.graph());
        let click = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
            cx.update_window(f.window, |_, window, cx| window.click(id, cx)).unwrap();
            cx.run_until_parked();
        };
        wait_until(cx, "the history", |cx| graph.read_with(cx, |g, _| g.subjects().len() == 4));
        let subjects = graph.read_with(cx, |g, _| g.subjects());
        assert_eq!(subjects[0], "Merge side");
        assert_eq!(subjects.last().map(String::as_str), Some("First"));
        assert_eq!(graph.read_with(cx, |g, _| g.rows().iter().map(|r| r.width()).max()), Some(2));

        // "Second" changed a.txt: its file opens as a tab of what changed.
        let second = subjects.iter().position(|s| s == "Second").unwrap();
        click(cx, ("graph-commit", second).into());
        wait_until(cx, "the commit's files", |cx| graph.read_with(cx, |g, _| g.open_files() == Some(1)));
        click(cx, ("graph-file", 0usize).into());
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        let diff = area.read_with(cx, |a, _| a.active_diff()).expect("a tab of what the commit changed");
        wait_until(cx, "the patch", |cx| diff.read_with(cx, |d, cx| d.counts(cx)) == Some((1, 0)));
        assert!(diff.read_with(cx, |d, _| d.label()).starts_with("a.txt ("));
        // The same file of the same commit is the same tab.
        click(cx, ("graph-file", 0usize).into());
        assert_eq!(area.read_with(cx, |a, _| a.tab_count()), 2);

        // A new commit shows at the top without asking.
        std::fs::write(repo.join("c.txt"), "three\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "Third"]);
        f.workspace.update(cx, |w, cx| w.refresh_git(cx));
        wait_until(cx, "the new commit", |cx| graph.read_with(cx, |g, _| g.subjects().first().map(String::as_str) == Some("Third")));

        // The header folds the history away and back.
        click(cx, "graph-header".into());
        assert!(graph.read_with(cx, |g, _| g.collapsed()));
        click(cx, "graph-header".into());
        assert!(!graph.read_with(cx, |g, _| g.collapsed()));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn a_second_window_opens_on_the_welcome_screen(cx: &mut TestAppContext) {
        let f = open(cx, "windows");
        assert_eq!(cx.update(|cx| cx.windows().len()), 1);
        press(cx, f.window, &["ctrl-shift-n"]);
        cx.run_until_parked();
        let windows = cx.update(|cx| cx.windows());
        assert_eq!(windows.len(), 2);
        // The first window keeps its project.
        assert!(f.workspace.read_with(cx, |w, _| w.root()).is_some());
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn all_changes_stage_discard_and_unstage_single_hunks(cx: &mut TestAppContext) {
        use std::process::Command;
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let repo = project_dir("allchanges");
        let git = |args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success(), "{args:?}");
        };
        let staged = || {
            let out = Command::new("git").arg("-C").arg(&repo).args(["diff", "--cached"]).output().unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        let lines: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        std::fs::write(repo.join("a.txt"), &lines).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "First"]);
        // Two places far apart in one file, and a new file.
        let changed = lines.replace("line 2\n", "line two\n").replace("line 28\n", "line 28\nadded\n");
        std::fs::write(repo.join("a.txt"), &changed).unwrap();
        std::fs::write(repo.join("new.txt"), "fresh\n").unwrap();
        let a = || std::fs::read_to_string(repo.join("a.txt")).unwrap().replace("\r\n", "\n");

        let f = open(cx, "allchanges");
        press(cx, f.window, &["ctrl-shift-a"]);
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        let view = area.read_with(cx, |a, _| a.active_changes()).expect("the tab of all changes");
        let settled = |cx: &mut TestAppContext| {
            wait_until(cx, "the changes to be read", |cx| view.read_with(cx, |v, _| !v.is_busy()));
        };
        let click = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
            cx.update_window(f.window, |_, window, cx| window.click(id, cx)).unwrap();
            settled(cx);
        };
        settled(cx);
        assert_eq!(view.read_with(cx, |v, _| v.names()), ["a.txt", "new.txt"]);
        assert_eq!(view.read_with(cx, |v, _| v.hunks()), [2, 1]);
        // The same shortcut again shows the same tab.
        press(cx, f.window, &["ctrl-shift-a"]);
        assert_eq!(area.read_with(cx, |a, _| a.tab_count()), 2);
        // It is not written down for the next launch.
        assert_eq!(area.read_with(cx, |a, cx| a.layout(cx).tabs.len()), 1);

        // Staging the second piece of a.txt leaves the first one out of the index.
        click(cx, ("changes-stage", 1usize).into());
        assert!(staged().contains("+added") && !staged().contains("line two"), "{}", staged());
        assert_eq!(view.read_with(cx, |v, _| v.hunks()), [1, 1]);
        assert_eq!(view.read_with(cx, |v, _| v.error().map(str::to_string)), None);

        // Throwing the first piece away changes the file; Undo brings it back.
        click(cx, ("changes-discard", 0usize).into());
        assert!(!a().contains("line two"));
        assert!(view.read_with(cx, |v, _| v.can_undo()));
        click(cx, "changes-undo".into());
        assert_eq!(a(), changed);
        assert!(!view.read_with(cx, |v, _| v.can_undo()));

        // The staged side has the piece that went in; it can come back out.
        click(cx, "changes-staged".into());
        assert_eq!(view.read_with(cx, |v, _| v.names()), ["a.txt"]);
        click(cx, ("changes-unstage", 0usize).into());
        assert_eq!(staged(), "");
        assert_eq!(view.read_with(cx, |v, _| v.note().map(str::to_string)), Some("Nothing is staged.".into()));

        // Stage all from the other side puts in the new file too.
        click(cx, "changes-unstaged".into());
        click(cx, "changes-all".into());
        assert!(staged().contains("+fresh"));
        assert_eq!(view.read_with(cx, |v, _| v.names()), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn worktrees_are_made_switched_to_and_removed(cx: &mut TestAppContext) {
        use std::process::Command;
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let repo = project_dir("worktrees");
        let git = |args: &[&str]| Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success();
        assert!(git(&["init", "-q", "-b", "main"]));
        for (key, value) in [("user.name", "Test"), ("user.email", "test@example.com"), ("commit.gpgsign", "false")] {
            assert!(git(&["config", key, value]));
        }
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        assert!(git(&["add", "-A"]) && git(&["commit", "-q", "-m", "First"]));
        let folder = repo.parent().unwrap().join("one-agent-1");

        let f = open(cx, "worktrees");
        let click = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
            cx.update_window(f.window, |_, window, cx| window.click(id, cx)).unwrap();
            cx.run_until_parked();
        };
        let root = |cx: &mut TestAppContext| f.workspace.read_with(cx, |w, _| w.root());
        let listed = |cx: &mut TestAppContext| {
            wait_until(cx, "the worktrees", |cx| {
                f.workspace.read_with(cx, |w, _| w.worktrees.as_ref().is_some_and(|m| m.trees.is_some() && !m.busy))
            });
            f.workspace.read_with(cx, |w, _| w.worktrees.as_ref().unwrap().trees.clone().unwrap())
        };
        wait_until(cx, "the repository", |cx| f.workspace.read_with(cx, |w, _| w.git().is_some()));

        // The button in the status bar lists the one folder; a branch name makes another, and the window goes there.
        click(cx, "status-worktrees".into());
        assert_eq!(listed(cx).len(), 1);
        cx.update_window(f.window, |_, window, cx| window.input("agent-1", cx)).unwrap();
        press(cx, f.window, &["enter"]);
        wait_until(cx, "the new worktree", |cx| root(cx) == Some(normalize(&folder)));
        assert!(folder.join("a.txt").exists());
        assert!(f.workspace.read_with(cx, |w, _| w.worktrees.is_none()));

        // From there the list has both; a click goes back.
        wait_until(cx, "the repository", |cx| f.workspace.read_with(cx, |w, _| w.git().is_some()));
        click(cx, "status-worktrees".into());
        let trees = listed(cx);
        assert_eq!(trees.len(), 2);
        assert_eq!(trees[1].branch.as_deref(), Some("agent-1"));
        click(cx, ("worktree", 0usize).into());
        wait_until(cx, "the first folder", |cx| root(cx) == Some(repo.clone()));

        // The other one is removed from the list; its branch stays.
        wait_until(cx, "the repository", |cx| f.workspace.read_with(cx, |w, _| w.git().is_some()));
        click(cx, "status-worktrees".into());
        assert_eq!(listed(cx).len(), 2);
        click(cx, ("worktree-remove", 1usize).into());
        assert_eq!(listed(cx).len(), 1);
        assert!(!folder.exists());
        let _ = std::fs::remove_dir_all(&f.dir);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[gpui_kit::test]
    async fn merge_conflicts_are_resolved_with_buttons(cx: &mut TestAppContext) {
        use std::process::Command;
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let repo = project_dir("conflicts");
        let git = |args: &[&str]| Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success();
        assert!(git(&["init", "-q", "-b", "main"]));
        for (key, value) in [("user.name", "Test"), ("user.email", "test@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
            assert!(git(&["config", key, value]));
        }
        // A line far wider than the window must not push the buttons out of sight.
        let wide = format!("line 1 {}\n", "wide ".repeat(120));
        let lines: String = std::iter::once(wide).chain((2..=12).map(|n| format!("line {n}\n"))).collect();
        let a = repo.join("a.txt");
        std::fs::write(&a, &lines).unwrap();
        assert!(git(&["add", "-A"]) && git(&["commit", "-q", "-m", "First"]));
        assert!(git(&["checkout", "-q", "-b", "feature"]));
        std::fs::write(&a, lines.replace("line 2\n", "feature 2\n").replace("line 11\n", "feature 11\n")).unwrap();
        assert!(git(&["commit", "-q", "-am", "Feature"]));
        assert!(git(&["checkout", "-q", "main"]));
        std::fs::write(&a, lines.replace("line 2\n", "main 2\n").replace("line 11\n", "main 11\n")).unwrap();
        assert!(git(&["commit", "-q", "-am", "Main"]));
        assert!(!git(&["merge", "-q", "feature"]));
        let text = || std::fs::read_to_string(&a).unwrap();
        let unmerged = || {
            let out = Command::new("git").arg("-C").arg(&repo).args(["diff", "--name-only", "--diff-filter=U"]).output().unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        let f = open(cx, "conflicts");
        press(cx, f.window, &["ctrl-shift-a"]);
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        let view = area.read_with(cx, |a, _| a.active_changes()).expect("the tab of all changes");
        let settled = |cx: &mut TestAppContext| {
            wait_until(cx, "the changes to be read", |cx| view.read_with(cx, |v, _| !v.is_busy()));
        };
        let click = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
            cx.update_window(f.window, |_, window, cx| window.click(id, cx)).unwrap();
            settled(cx);
        };
        settled(cx);
        assert_eq!(view.read_with(cx, |v, _| v.conflicts()), [("a.txt".to_string(), 2)]);
        assert_eq!(view.read_with(cx, |v, _| v.note().map(str::to_string)), None);

        // The first conflict takes the incoming side; the file stays unmerged.
        click(cx, ("accept-incoming", 0usize).into());
        assert!(text().contains("feature 2\n") && !text().contains("main 2\n"), "{}", text());
        assert_eq!(view.read_with(cx, |v, _| v.conflicts()), [("a.txt".to_string(), 1)]);
        assert_eq!(unmerged(), "a.txt");

        // The last one keeps both; the file goes into the index and leaves the list.
        click(cx, ("accept-both", 0usize).into());
        assert!(text().contains("main 11\nfeature 11\n") && !text().contains("<<<<<<<"), "{}", text());
        assert_eq!(unmerged(), "");
        assert!(view.read_with(cx, |v, _| v.conflicts()).is_empty());
        assert_eq!(view.read_with(cx, |v, _| v.error().map(str::to_string)), None);

        // Undo makes it a conflict again.
        click(cx, "changes-undo".into());
        assert_eq!(unmerged(), "a.txt");
        assert_eq!(view.read_with(cx, |v, _| v.conflicts()), [("a.txt".to_string(), 1)]);

        // Markers taken out by hand: the file is marked resolved with a button.
        std::fs::write(&a, lines.replace("line 2\n", "feature 2\n")).unwrap();
        view.update(cx, |v, cx| v.reload(cx));
        settled(cx);
        assert_eq!(view.read_with(cx, |v, _| v.conflicts()), [("a.txt".to_string(), 0)]);
        click(cx, ("changes-mark-resolved", 0usize).into());
        assert_eq!(unmerged(), "");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// Lets real threads (the file watcher) and the test clock both move
    /// until `ready` holds.
    fn wait_until(cx: &mut TestAppContext, what: &str, mut ready: impl FnMut(&mut TestAppContext) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            if ready(cx) {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// The folder `open` will use as the project, made ahead of time so a
    /// test can put files in it first.
    fn project_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wercode-ws-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("one")).unwrap();
        normalize(&dir.join("one"))
    }

    /// The test binary plays a language server: started again with this
    /// variable set, the test below talks the protocol on its streams.
    const FAKE_SERVER: &str = "WERCODE_FAKE_LANGUAGE_SERVER";

    #[test]
    fn fake_language_server() {
        if std::env::var_os(FAKE_SERVER).is_some() {
            wercode_core::lsp::fake::serve();
        }
    }

    /// Opens a window on `project_dir(name)` with checking set up: a server
    /// for Rust that calls `bad` an error and `meh` a warning, a server for
    /// Python that is not installed, and one for Go that Wercode would fetch
    /// but has no download for this computer.
    fn open_checked(cx: &mut TestAppContext, name: &str) -> Fixture {
        use wercode_core::lsp::{Program, Spec, fake};
        cx.executor().allow_parking();
        let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        let fake = Spec {
            name: "fake".into(),
            programs: vec![Program {
                program: exe,
                args: fake::test_args("workspace::tests::fake_language_server"),
                env: vec![(FAKE_SERVER.into(), "1".into())],
            }],
            probe: None,
            languages: vec!["rust"],
            markers: vec![],
            install: None,
            download: None,
            offer: true,
        };
        let missing = Spec {
            name: "missing".into(),
            programs: vec![Program { program: "wercode-no-such-server".into(), args: vec![], env: vec![] }],
            languages: vec!["python"],
            install: Some("echo install-it".into()),
            ..fake.clone()
        };
        let fetchable = Spec {
            name: "fetchable".into(),
            languages: vec!["go"],
            install: None,
            download: Some(wercode_core::lsp::download::Download::GitHub {
                repo: "example/none",
                asset: |_, _| None,
                binary: "none",
            }),
            ..missing.clone()
        };
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::apply(Settings::default(), cx);
            crate::lsp::init_with(vec![fake, missing, fetchable], cx);
        });
        open(cx, name)
    }

    #[gpui_kit::test]
    async fn language_servers_underline_count_and_list_problems(cx: &mut TestAppContext) {
        let root = project_dir("lsp");
        let file = root.join("main.rs");
        let script = root.join("tool.py");
        std::fs::write(&file, "fn main() {}\nlet x = bad;\nmeh\n").unwrap();
        std::fs::write(&script, "print()\n").unwrap();
        let f = open_checked(cx, "lsp");
        let checker = cx.update(|cx| crate::lsp::checker(cx).unwrap());
        let servers = checker.read_with(cx, |c, _| c.servers().clone());

        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.open_file(file.clone(), None, window, cx));
        })
        .unwrap();
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        let editor = area.read_with(cx, |a, cx| a.editor_for(&file, cx)).unwrap();
        wait_until(cx, "the underlines", |cx| editor.read_with(cx, |e, cx| e.underlined(cx)).len() == 2);
        assert_eq!(editor.read_with(cx, |e, cx| e.underlined(cx)), ["bad word", "weak word"]);
        assert_eq!(servers.counts(&root), (1, 1));

        // The counts in the status bar open the list; a problem in it opens its line.
        cx.update_window(f.window, |_, window, cx| window.click("status-problems", cx)).unwrap();
        assert_eq!(f.workspace.read_with(cx, |w, _| (w.panel_open, w.panel)), (true, PanelKind::Problems));
        cx.run_until_parked();
        let problems = f.workspace.read_with(cx, |w, _| w.problems());
        assert_eq!(problems.read_with(cx, |p, _| p.listed()), (1, 2));
        cx.update_window(f.window, |_, window, cx| window.click(("problems-row", 1usize), cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(area.read_with(cx, |a, cx| a.shown_file(cx)), Some(file.clone()));

        // Fixing the text clears the underline once typing pauses.
        cx.update_window(f.window, |_, window, cx| {
            editor.update(cx, |e, cx| e.replace_all("bad", "good", window, cx));
        })
        .unwrap();
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        wait_until(cx, "the error to go", |cx| editor.read_with(cx, |e, cx| e.underlined(cx)) == ["weak word"]);
        wait_until(cx, "the list to follow", |cx| problems.read_with(cx, |p, _| p.listed()) == (1, 1));

        // A language without a server offers to install one in a terminal.
        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.open_file(script.clone(), None, window, cx));
        })
        .unwrap();
        wait_until(cx, "the offer to install", |cx| {
            cx.update(|_| servers.report(&script)).is_some_and(|r| r.install.is_some())
        });
        let tabs = area.read_with(cx, |a, _| a.tab_count());
        cx.update_window(f.window, |_, window, cx| window.click("status-server-install", cx)).unwrap();
        assert_eq!(area.read_with(cx, |a, _| a.tab_count()), tabs + 1);

        // One Wercode can fetch is fetched by the same button; what went wrong is said there.
        let code = root.join("main.go");
        std::fs::write(&code, "package main\n").unwrap();
        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.open_file(code.clone(), None, window, cx));
        })
        .unwrap();
        wait_until(cx, "the offer to fetch", |cx| {
            cx.update(|_| servers.report(&code)).is_some_and(|r| r.download.as_deref() == Some("fetchable"))
        });
        let tabs = area.read_with(cx, |a, _| a.tab_count());
        cx.update_window(f.window, |_, window, cx| window.click("status-server-install", cx)).unwrap();
        wait_until(cx, "the fetch to fail", |cx| {
            checker.read_with(cx, |c, _| matches!(c.fetching("fetchable"), Some(crate::lsp::Fetching::Failed(_))))
        });
        assert_eq!(area.read_with(cx, |a, _| a.tab_count()), tabs, "nothing runs in a terminal");

        // Turned off in the settings, nothing is underlined or counted.
        cx.update(|cx| crate::theme::change_settings(cx, |s| s.language_servers = false)).unwrap();
        wait_until(cx, "the underlines to go", |cx| editor.read_with(cx, |e, cx| e.underlined(cx)).is_empty());
        assert_eq!(servers.counts(&root), (0, 0));
        servers.stop_all(std::time::Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn language_servers_describe_code_and_find_definitions(cx: &mut TestAppContext) {
        use wercode_core::lsp::Status;
        let root = project_dir("lsp-ask");
        let file = root.join("main.rs");
        let other = root.join("other.rs");
        std::fs::write(&file, "fn name() {}

use name;
").unwrap();
        std::fs::write(&other, "one
two
three
").unwrap();
        let f = open_checked(cx, "lsp-ask");
        let checker = cx.update(|cx| crate::lsp::checker(cx).unwrap());
        let servers = checker.read_with(cx, |c, _| c.servers().clone());
        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.open_file(file.clone(), None, window, cx));
        })
        .unwrap();
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        let editor = area.read_with(cx, |a, cx| a.editor_for(&file, cx)).unwrap();
        wait_until(cx, "the server", |cx| {
            cx.update(|_| servers.report(&file)).is_some_and(|r| r.servers.first().is_some_and(|s| s.1 == Status::Ready))
        });

        // The pointer resting on a name asks the server about it.
        let asked = cx
            .update_window(f.window, |_, window, cx| editor.update(cx, |e, cx| e.hover_at(2, 5, window, cx)))
            .unwrap();
        let said = std::rc::Rc::new(std::cell::RefCell::new(None));
        let heard = said.clone();
        cx.update(|cx| cx.foreground_executor().spawn(async move { *heard.borrow_mut() = Some(asked.await) }).detach());
        wait_until(cx, "what the server says", |_| said.borrow().is_some());
        assert_eq!(said.borrow_mut().take().unwrap().as_deref(), Some("the word `name`"));

        // F12 on a name moves the cursor to where the file defines it.
        cx.update_window(f.window, |_, window, cx| editor.update(cx, |e, cx| e.go_to(3, Some(6), window, cx))).unwrap();
        cx.run_until_parked();
        assert_eq!(editor.read_with(cx, |e, cx| e.cursor(cx)), (2, 5));
        press(cx, f.window, &["f12"]);
        wait_until(cx, "the cursor on the definition", |cx| editor.read_with(cx, |e, cx| e.cursor(cx)).0 == 0);

        // The server offers names to complete with.
        let asked = cx
            .update_window(f.window, |_, window, cx| editor.update(cx, |e, cx| e.completions_at(2, 6, window, cx)))
            .unwrap();
        let offered = std::rc::Rc::new(std::cell::RefCell::new(None));
        let heard = offered.clone();
        cx.update(|cx| cx.foreground_executor().spawn(async move { *heard.borrow_mut() = Some(asked.await) }).detach());
        wait_until(cx, "the names to complete with", |_| offered.borrow().is_some());
        // The cursor is after "na": of fn, name and use only one fits.
        assert_eq!(offered.borrow_mut().take().unwrap(), ["name"]);

        // With formatting on, saving lays the file out first: here, no spaces at line ends.
        cx.update(|cx| crate::theme::change_settings(cx, |s| s.format_on_save = true)).unwrap();
        cx.update_window(f.window, |_, window, cx| {
            editor.update(cx, |e, cx| {
                e.replace_all("use name;", "use name;   ", window, cx);
                e.save(|_, _| {}, window, cx);
            });
        })
        .unwrap();
        wait_until(cx, "the formatted file on disk", |_| {
            std::fs::read_to_string(&file).is_ok_and(|text| text.replace("\r\n", "\n") == "fn name() {}\n\nuse name;\n")
        });
        assert_eq!(editor.read_with(cx, |e, cx| e.text(cx)), "fn name() {}\n\nuse name;\n");

        // A definition in another file opens that file in a tab.
        let shown = cx
            .update_window(f.window, |_, window, cx| crate::editor::EditorView::follow(&editor, &other, 1, 0, window, cx))
            .unwrap();
        assert!(shown);
        cx.run_until_parked();
        assert_eq!(area.read_with(cx, |a, cx| a.shown_file(cx)), Some(other.clone()));
        servers.stop_all(std::time::Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn language_servers_fix_rename_outline_and_note(cx: &mut TestAppContext) {
        use wercode_core::lsp::Status;
        let root = project_dir("lsp-smart");
        let file = root.join("main.rs");
        std::fs::write(&file, "struct Point\n  fn len(\nfn main() {\n    let x = bad;\n    helper();\n}\n").unwrap();
        let f = open_checked(cx, "lsp-smart");
        let servers = cx.update(|cx| crate::lsp::checker(cx).unwrap()).read_with(cx, |c, _| c.servers().clone());
        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.open_file(file.clone(), Some((2, 4)), window, cx));
        })
        .unwrap();
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        let editor = area.read_with(cx, |a, cx| a.editor_for(&file, cx)).unwrap();
        wait_until(cx, "the server", |cx| {
            cx.update(|_| servers.report(&file)).is_some_and(|r| r.servers.first().is_some_and(|s| s.1 == Status::Ready))
        });
        let text = |cx: &mut TestAppContext| editor.read_with(cx, |e, cx| e.text(cx));
        // The outline is asked for a moment after the server speaks; the test clock moves by hand.
        let later = |cx: &mut TestAppContext| cx.executor().advance_clock(std::time::Duration::from_millis(300));

        // Above the code, the things the cursor is inside; at line ends, the notes.
        wait_until(cx, "the path to the cursor", |cx| {
            later(cx);
            editor.read_with(cx, |e, cx| e.crumbs(cx)) == ["Point", "len"]
        });
        wait_until(cx, "the notes", |cx| editor.read_with(cx, |e, _| e.hint_labels()) == [": i32"]);

        // A quick fix for the problem on the line changes the text.
        cx.update_window(f.window, |_, window, cx| editor.update(cx, |e, cx| e.go_to(4, Some(13), window, cx))).unwrap();
        cx.run_until_parked();
        let done = cx.update_window(f.window, |_, window, cx| editor.update(cx, |e, cx| e.fix("Replace bad with good", window, cx))).unwrap();
        let fixed = std::rc::Rc::new(std::cell::Cell::new(false));
        let heard = fixed.clone();
        cx.update(|cx| cx.foreground_executor().spawn(async move { heard.set(done.await) }).detach());
        wait_until(cx, "the fix", |cx| text(cx).contains("let x = good;"));
        assert!(fixed.get());

        // F2 renames the name under the cursor through the server.
        cx.update_window(f.window, |_, window, cx| editor.update(cx, |e, cx| e.go_to(5, Some(6), window, cx))).unwrap();
        cx.run_until_parked();
        press(cx, f.window, &["f2"]);
        cx.run_until_parked();
        assert!(editor.read_with(cx, |e, _| e.is_renaming()));
        cx.update_window(f.window, |_, window, cx| window.input("assist", cx)).unwrap();
        press(cx, f.window, &["enter"]);
        wait_until(cx, "the rename", |cx| text(cx).contains("    assist();"));
        assert!(!editor.read_with(cx, |e, _| e.is_renaming()));
        // Escape leaves the name as it was.
        press(cx, f.window, &["f2"]);
        cx.run_until_parked();
        press(cx, f.window, &["escape"]);
        cx.run_until_parked();
        assert!(!editor.read_with(cx, |e, _| e.is_renaming()));

        // Ctrl+R lists the things of the file; picking one goes to it.
        press(cx, f.window, &["ctrl-r"]);
        let picker = f.workspace.read_with(cx, |w, _| w.symbols.as_ref().map(|(p, _)| p.clone())).expect("the list");
        wait_until(cx, "the list of things", |cx| picker.read_with(cx, |p, _| p.listed()) == ["Point", "len", "main"]);
        cx.update_window(f.window, |_, window, cx| window.input("ma", cx)).unwrap();
        wait_until(cx, "the list to follow", |cx| picker.read_with(cx, |p, _| p.listed()) == ["main"]);
        press(cx, f.window, &["enter"]);
        cx.run_until_parked();
        assert!(f.workspace.read_with(cx, |w, _| w.symbols.is_none()));
        assert_eq!(editor.read_with(cx, |e, cx| e.cursor(cx)), (2, 3));
        servers.stop_all(std::time::Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn search_finds_text_and_opens_the_match(cx: &mut TestAppContext) {
        use wercode_core::state::TabLayout;
        let root = project_dir("search");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("a.rs"), "fn main() {}\n\n// the needle is here\n").unwrap();
        std::fs::write(root.join("b.txt"), "Needle one\nnothing\nneedle two\n").unwrap();
        std::fs::write(root.join("c.txt"), "nothing at all\n").unwrap();

        let f = open(cx, "search");
        press(cx, f.window, &["ctrl-shift-f"]);
        assert_eq!(f.workspace.read_with(cx, |w, _| (w.panel_open, w.panel)), (true, PanelKind::Search));
        // The shortcut puts the keyboard in the field: typing is the query.
        cx.update_window(f.window, |_, window, cx| window.input("needle", cx)).unwrap();
        press(cx, f.window, &["enter"]);
        let search = f.workspace.read_with(cx, |w, _| w.search());
        wait_until(cx, "the search", |cx| search.read_with(cx, |s, _| s.found().is_some()));
        assert_eq!(search.read_with(cx, |s, _| s.found()), Some((2, 3)));
        assert_eq!(search.read_with(cx, |s, _| s.status_text()), Some("3 results in 2 files".into()));
        // Two file rows and three lines; the files come in name order.
        assert_eq!(search.read_with(cx, |s, _| s.row_count()), 5);

        // Clicking a file row folds its lines away, clicking again brings them back.
        cx.update_window(f.window, |_, window, cx| window.click(("search-row", 0usize), cx)).unwrap();
        assert_eq!(search.read_with(cx, |s, _| s.row_count()), 3);
        cx.update_window(f.window, |_, window, cx| window.click(("search-row", 0usize), cx)).unwrap();
        assert_eq!(search.read_with(cx, |s, _| s.row_count()), 5);

        // The last row is the match in src/a.rs, on its third line.
        cx.update_window(f.window, |_, window, cx| window.click(("search-row", 4usize), cx)).unwrap();
        cx.run_until_parked();
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        let layout = area.read_with(cx, |a, cx| a.layout(cx));
        assert_eq!(layout.tabs.last(), Some(&TabLayout::File { path: root.join("src").join("a.rs"), beside: None }));
        assert_eq!(layout.active, 1);

        // Options narrow it down; a pattern that is not valid says so.
        cx.update_window(f.window, |_, window, cx| window.click("search-case", cx)).unwrap();
        wait_until(cx, "the case-sensitive search", |cx| search.read_with(cx, |s, _| s.found().is_some()));
        assert_eq!(search.read_with(cx, |s, _| s.found()), Some((2, 2)));
        cx.update_window(f.window, |_, window, cx| window.click("search-regex", cx)).unwrap();
        cx.update_window(f.window, |_, window, cx| {
            search.update(cx, |s, cx| s.write_query("(", window, cx));
        })
        .unwrap();
        wait_until(cx, "the error", |cx| search.read_with(cx, |s, _| s.status_text().is_some()));
        assert!(search.read_with(cx, |s, _| s.status_text()).unwrap().starts_with("Not a valid pattern"));

        // Replace All asks first, then rewrites the files that were found.
        cx.update_window(f.window, |_, window, cx| window.click("search-regex", cx)).unwrap();
        cx.update_window(f.window, |_, window, cx| {
            search.update(cx, |s, cx| {
                s.write_replacement("pin", window, cx);
                s.write_query("needle", window, cx);
            });
        })
        .unwrap();
        wait_until(cx, "the search before replacing", |cx| search.read_with(cx, |s, _| s.found().is_some()));
        let (files, lines) = search.read_with(cx, |s, _| s.found()).unwrap();
        cx.update_window(f.window, |_, window, cx| window.click("search-replace-all", cx)).unwrap();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Replace");
        let said = format!("Replaced {lines} results in {files} files.");
        wait_until(cx, "the replacement", |cx| search.read_with(cx, |s, _| s.status_text()) == Some(said.clone()));
        let text = std::fs::read_to_string(root.join("src").join("a.rs")).unwrap();
        assert!(text.contains("// the pin is here") && !text.contains("needle"), "{text}");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn the_tree_reads_folders_when_opened_and_follows_the_disk(cx: &mut TestAppContext) {
        let root = project_dir("watch");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("inner.rs"), "").unwrap();
        std::fs::write(root.join("top.txt"), "").unwrap();

        let f = open(cx, "watch");
        let files = f.workspace.read_with(cx, |w, _| w.files());
        let shows = |cx: &mut TestAppContext, path: &std::path::Path| files.read_with(cx, |p, cx| p.shows(path, cx));
        assert!(shows(cx, &root.join("top.txt")) && shows(cx, &root.join("src")));
        // A closed folder has not been read yet.
        assert!(!shows(cx, &root.join("src").join("inner.rs")));
        files.update(cx, |p, cx| p.open_folder(&root.join("src"), cx));
        wait_until(cx, "the folder to open", |cx| shows(cx, &root.join("src").join("inner.rs")));

        // A file made by another program appears by itself, in an open folder or at the top.
        std::fs::write(root.join("src").join("made-outside.rs"), "").unwrap();
        std::fs::write(root.join("also-new.txt"), "").unwrap();
        wait_until(cx, "the new files", |cx| {
            shows(cx, &root.join("src").join("made-outside.rs")) && shows(cx, &root.join("also-new.txt"))
        });
        std::fs::remove_file(root.join("top.txt")).unwrap();
        wait_until(cx, "the deleted file to go", |cx| !shows(cx, &root.join("top.txt")));
        assert!(shows(cx, &root.join("src").join("inner.rs")), "open folders stay open");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn renaming_a_file_moves_its_tab_and_paths_go_to_the_terminal(cx: &mut TestAppContext) {
        use wercode_core::state::TabLayout;
        let root = project_dir("rename");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("old.rs"), "fn old() {}\n").unwrap();
        std::fs::write(root.join("taken.txt"), "").unwrap();
        let f = open(cx, "rename");
        let files = f.workspace.read_with(cx, |w, _| w.files());
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        let old = root.join("src").join("old.rs");
        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.open_file(old.clone(), None, window, cx));
        })
        .unwrap();
        cx.run_until_parked();

        // Renaming the folder takes the open file's tab along.
        let rename = |cx: &mut TestAppContext, path: &std::path::Path, name: &str| {
            cx.update_window(f.window, |_, window, cx| {
                files.update(cx, |p, cx| p.rename_to(path, name, window, cx));
            })
            .unwrap();
            cx.run_until_parked();
        };
        rename(cx, &root.join("src"), "lib");
        let moved = root.join("lib").join("old.rs");
        assert!(moved.is_file() && !old.exists());
        let tab = |cx: &mut TestAppContext| area.read_with(cx, |a, cx| a.layout(cx).tabs.last().cloned());
        assert_eq!(tab(cx), Some(TabLayout::File { path: moved.clone(), beside: None }));
        rename(cx, &moved, "new.rs");
        assert_eq!(tab(cx), Some(TabLayout::File { path: root.join("lib").join("new.rs"), beside: None }));
        wait_until(cx, "the renamed folder in the tree", |cx| files.read_with(cx, |p, cx| p.shows(&root.join("lib"), cx)));

        // A name that is taken is refused and said so; nothing moves.
        rename(cx, &root.join("lib"), "taken.txt");
        assert!(files.read_with(cx, |p, _| p.error().is_some()));
        assert!(root.join("lib").is_dir() && root.join("taken.txt").is_file());

        // Inserting a path while a file is showing goes to the terminal tab and shows it.
        assert_eq!(area.read_with(cx, |a, cx| a.layout(cx).active), 1);
        cx.update_window(f.window, |_, _, cx| {
            files.update(cx, |p, cx| p.insert(&root.join("taken.txt"), cx));
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(area.read_with(cx, |a, cx| a.layout(cx).active), 0);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn settings_apply_at_once_and_are_written_to_the_file(cx: &mut TestAppContext) {
        use crate::theme::AppState;
        let f = open(cx, "settings");
        let settings_file = f.dir.join("settings.toml");
        std::fs::write(&settings_file, "# mine\nscrollback = 500\n").unwrap();
        cx.update(|cx| crate::theme::set_settings_path(Some(settings_file.clone()), cx));
        let settings = |cx: &mut TestAppContext| cx.update(|cx| cx.global::<AppState>().settings.clone());
        assert_eq!(settings(cx).scrollback, 10_000);

        // Opening the screen reads the file, so what was typed there by hand counts.
        cx.update_window(f.window, |_, window, cx| window.click("open-settings", cx)).unwrap();
        assert!(f.workspace.read_with(cx, |w, _| w.settings.is_some()));
        assert_eq!(settings(cx).scrollback, 500);
        let before = settings(cx);
        assert_eq!((before.theme.as_str(), before.font_size, before.cursor_blink), ("black", 14.0, true));

        let click = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
            cx.update_window(f.window, |_, window, cx| window.click(id, cx)).unwrap();
        };
        // The second scheme in the list is Graphite.
        click(cx, ("settings-scheme", 1usize).into());
        click(cx, "settings-font-larger".into());
        click(cx, "settings-font-larger".into());
        click(cx, "settings-side-right".into());
        click(cx, "settings-blink".into());
        let after = settings(cx);
        assert_eq!((after.theme.as_str(), after.font_size, after.cursor_blink), ("graphite", 16.0, false));
        assert!(after.panel_on_right());
        assert_eq!(cx.update(|cx| cx.global::<AppState>().scheme.name.clone()), "Graphite");

        let text = std::fs::read_to_string(&settings_file).unwrap();
        assert!(text.starts_with("# mine\nscrollback = 500\n"), "{text}");
        let (saved, error) = wercode_core::config::load_from(&settings_file);
        assert!(error.is_none(), "{error:?}");
        assert_eq!((saved.theme.as_str(), saved.font_size, saved.cursor_blink), ("graphite", 16.0, false));
        assert!(!text.contains("confirm_close"), "untouched settings are not written: {text}");

        // Escape closes the screen and gives the keyboard back to the terminal.
        press(cx, f.window, &["escape"]);
        assert!(f.workspace.read_with(cx, |w, _| w.settings.is_none()));
        press(cx, f.window, &["ctrl-shift-d"]);
        assert_eq!(f.workspace.read_with(cx, |w, cx| w.pane_count(cx)), 2);

        // The file edited in a tab counts as soon as it is saved.
        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.open_file(settings_file.clone(), None, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        wait_until(cx, "the settings file in a tab", |cx| area.read_with(cx, |a, cx| a.editor_for(&settings_file, cx)).is_some());
        let editor = area.read_with(cx, |a, cx| a.editor_for(&settings_file, cx)).unwrap();
        wait_until(cx, "its text", |cx| editor.read_with(cx, |e, cx| e.text(cx)).contains("scrollback"));
        cx.update_window(f.window, |_, window, cx| {
            editor.update(cx, |e, cx| {
                e.replace_all("scrollback = 500", "scrollback = 700", window, cx);
                e.save(|_, _| {}, window, cx);
            });
        })
        .unwrap();
        wait_until(cx, "the saved setting to count", |cx| settings(cx).scrollback == 700);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn closing_the_window_asks_about_unsaved_files(cx: &mut TestAppContext) {
        let root = project_dir("close");
        let path = root.join("notes.txt");
        std::fs::write(&path, "one\n").unwrap();
        let f = open(cx, "close");
        let close = |cx: &mut TestAppContext| {
            cx.update_window(f.window, |_, window, cx| f.workspace.update(cx, |w, cx| w.should_close(window, cx)))
                .unwrap()
        };
        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.open_file(path.clone(), None, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(f.window, |_, window, cx| window.input("zero ", cx)).unwrap();
        cx.run_until_parked();

        // Cancel keeps the window and the edits.
        assert!(!close(cx), "the window must wait for the answer");
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\n");
        assert_eq!(f.workspace.read_with(cx, |w, cx| w.terminals().read(cx).unsaved_count(cx)), 1);

        // Save All writes the file and then closes the window.
        assert!(!close(cx));
        cx.simulate_prompt_answer("Save All");
        cx.run_until_parked();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "zero one\n");
        assert_eq!(cx.update(|cx| cx.windows().len()), 0);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn closing_without_saving_leaves_the_file_alone(cx: &mut TestAppContext) {
        let root = project_dir("discard");
        let path = root.join("notes.txt");
        std::fs::write(&path, "one\n").unwrap();
        let f = open(cx, "discard");
        let close = |cx: &mut TestAppContext| {
            cx.update_window(f.window, |_, window, cx| f.workspace.update(cx, |w, cx| w.should_close(window, cx)))
                .unwrap()
        };
        // Nothing unsaved and nothing running: the window just closes.
        assert!(close(cx));
        assert!(!cx.has_pending_prompt());

        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.open_file(path.clone(), None, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(f.window, |_, window, cx| window.input("zero ", cx)).unwrap();
        cx.run_until_parked();
        assert!(!close(cx));
        cx.simulate_prompt_answer("Don't Save");
        cx.run_until_parked();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\n");
        assert_eq!(cx.update(|cx| cx.windows().len()), 0);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn a_project_reopens_with_the_tabs_it_had(cx: &mut TestAppContext) {
        use wercode_core::state::TabLayout;
        let root = project_dir("reopen");
        let path = root.join("notes.txt");
        std::fs::write(&path, "one\n").unwrap();
        let f = open(cx, "reopen");
        press(cx, f.window, &["ctrl-shift-d", "ctrl-shift-t"]);
        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.open_file(path.clone(), None, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        press(cx, f.window, &["ctrl-pageup"]);
        let closed = cx
            .update_window(f.window, |_, window, cx| f.workspace.update(cx, |w, cx| w.should_close(window, cx)))
            .unwrap();
        assert!(closed);

        // What a new launch reads from the state file.
        let saved = state::load_from(&f.state_path);
        let shape = |layout: &wercode_core::state::Layout| {
            let tabs: Vec<usize> = layout
                .tabs
                .iter()
                .map(|tab| match tab {
                    TabLayout::Terminals { panes, .. } => panes.terminals(),
                    TabLayout::File { .. } => 0,
                })
                .collect();
            (tabs, layout.active)
        };
        let layout = saved.layouts.get(&root).expect("the tabs were saved");
        assert_eq!(shape(layout), (vec![2, 1, 0], 1));
        assert_eq!(layout.tabs[2], TabLayout::File { path: path.clone(), beside: None });

        let state_path = f.state_path.clone();
        let (_, reopened) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| Workspace::new(Some(root.clone()), saved, Some(state_path), window, cx))
            })
            .unwrap()
        });
        let layout = reopened.read_with(cx, |w, cx| w.terminals().read(cx).layout(cx));
        assert_eq!(shape(&layout), (vec![2, 1, 0], 1));

        // Switching away and back keeps the tabs too.
        let two = normalize(&f.dir.join("two"));
        for target in [two, root.clone()] {
            cx.update_window(f.window, |_, window, cx| {
                f.workspace.update(cx, |w, cx| w.switch_to(target.clone(), window, cx));
            })
            .unwrap();
        }
        let layout = f.workspace.read_with(cx, |w, cx| w.terminals().read(cx).layout(cx));
        assert_eq!(shape(&layout), (vec![2, 1, 0], 1));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn without_a_project_the_welcome_screen_opens_one(cx: &mut TestAppContext) {
        let f = open_with(cx, "welcome", false);
        assert_eq!(f.workspace.read_with(cx, |w, _| w.root()), None);
        assert!(state::load_from(&f.state_path).recent.is_empty());

        let two = normalize(&f.dir.join("two"));
        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.switch_to(two.clone(), window, cx));
        })
        .unwrap();
        assert_eq!(f.workspace.read_with(cx, |w, _| w.root()), Some(two.clone()));
        assert_eq!(state::load_from(&f.state_path).recent, vec![two]);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn changed_lines_are_marked_next_to_their_numbers(cx: &mut TestAppContext) {
        use wercode_core::linediff::Kind;
        let root = project_dir("gutter");
        let file = root.join("notes.txt");
        std::fs::write(&file, "one
two
three
").unwrap();
        let git = |args: &[&str]| {
            let done = std::process::Command::new("git").arg("-C").arg(&root).args(args).output().unwrap();
            assert!(done.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&done.stderr));
        };
        git(&["init", "-q", "-b", "trunk"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@example.com", "add", "-A"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", "First"]);
        // Since the commit: a line changed and a line added.
        std::fs::write(&file, "one
TWO
three
four
").unwrap();
        let f = open(cx, "gutter");
        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.open_file(file.clone(), None, window, cx));
        })
        .unwrap();
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        wait_until(cx, "the editor", |cx| area.read_with(cx, |a, cx| a.editor_for(&file, cx)).is_some());
        let editor = area.read_with(cx, |a, cx| a.editor_for(&file, cx)).unwrap();
        let marked = |cx: &mut TestAppContext| editor.read_with(cx, |e, _| e.marked());
        wait_until(cx, "the marks", |cx| marked(cx).len() == 2);
        assert_eq!(marked(cx), [(Kind::Modified, 1, 2), (Kind::Added, 3, 4)]);

        // The status bar says who last changed the line under the cursor.
        cx.update_window(f.window, |_, window, cx| editor.update(cx, |e, cx| e.go_to(1, None, window, cx))).unwrap();
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        wait_until(cx, "who wrote the line", |cx| editor.read_with(cx, |e, _| e.blame().is_some()));
        let blame = editor.read_with(cx, |e, _| e.blame().cloned()).unwrap();
        assert_eq!((blame.author.as_str(), blame.summary.as_str()), ("t", "First"));
        // A line that is not committed yet has nobody.
        cx.update_window(f.window, |_, window, cx| editor.update(cx, |e, cx| e.go_to(4, None, window, cx))).unwrap();
        cx.run_until_parked();
        assert_eq!(editor.read_with(cx, |e, _| e.blame().cloned()), None);

        // Typing moves them once it pauses.
        cx.update_window(f.window, |_, window, cx| {
            editor.update(cx, |e, cx| e.replace_all("one
", "", window, cx));
        })
        .unwrap();
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        // Two lines became one changed line; the new line is one higher.
        wait_until(cx, "the marks after typing", |cx| marked(cx) == [(Kind::Modified, 0, 1), (Kind::Added, 2, 3)]);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn a_task_of_the_project_runs_in_a_terminal_tab(cx: &mut TestAppContext) {
        let root = project_dir("tasks");
        std::fs::write(root.join("package.json"), r#"{"scripts": {"dev": "vite", "lint": "eslint ."}}"#).unwrap();
        std::fs::write(root.join("Makefile"), "docs:\n\ttrue\n").unwrap();
        let f = open(cx, "tasks");
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        let listed = |cx: &mut TestAppContext| {
            f.workspace.read_with(cx, |w, _| {
                let list = w.tasks.as_ref()?;
                let found = list.tasks.as_ref()?;
                Some(list.shown.iter().map(|&i| found[i].command.clone()).collect::<Vec<_>>())
            })
        };

        press(cx, f.window, &["ctrl-shift-x"]);
        wait_until(cx, "the list of tasks", |cx| listed(cx).is_some());
        assert_eq!(listed(cx).unwrap(), ["npm run dev", "npm run lint", "make docs"]);
        cx.update_window(f.window, |_, window, cx| window.input("lint", cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(listed(cx).unwrap(), ["npm run lint"]);
        let tabs = area.read_with(cx, |a, _| a.tab_count());
        press(cx, f.window, &["enter"]);
        cx.run_until_parked();
        assert!(f.workspace.read_with(cx, |w, _| w.tasks.is_none()));
        assert_eq!(area.read_with(cx, |a, _| a.tab_count()), tabs + 1);
        assert_eq!(f.workspace.read_with(cx, |w, _| w.last_task.clone()).as_deref(), Some("npm run lint"));

        // The last task runs again without the list.
        press(cx, f.window, &["ctrl-shift-l"]);
        cx.run_until_parked();
        assert!(f.workspace.read_with(cx, |w, _| w.tasks.is_none()));
        assert_eq!(area.read_with(cx, |a, _| a.tab_count()), tabs + 2);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn a_file_is_opened_by_a_few_letters_of_its_name(cx: &mut TestAppContext) {
        use wercode_core::state::TabLayout;
        let root = project_dir("finder");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(root.join("src").join("alpha.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(root.join("src").join("beta.rs"), "fn beta() {}\n").unwrap();
        std::fs::write(root.join("target").join("alpha.o"), "").unwrap();
        let f = open(cx, "finder");
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        let listed = |cx: &mut TestAppContext| {
            f.workspace.read_with(cx, |w, _| {
                let finder = w.finder.as_ref()?;
                let files = finder.files.as_ref()?;
                Some(finder.shown.iter().map(|&i| files[i].clone()).collect::<Vec<_>>())
            })
        };

        press(cx, f.window, &["ctrl-shift-k"]);
        wait_until(cx, "the list of files", |cx| listed(cx).is_some());
        // Ignored files are not offered.
        assert_eq!(listed(cx).unwrap(), [".gitignore", "src/alpha.rs", "src/beta.rs"]);
        press(cx, f.window, &["escape"]);
        assert!(f.workspace.read_with(cx, |w, _| w.finder.is_none()));

        press(cx, f.window, &["ctrl-shift-k"]);
        wait_until(cx, "the list of files", |cx| listed(cx).is_some());
        cx.update_window(f.window, |_, window, cx| window.input("bet", cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(listed(cx).unwrap(), ["src/beta.rs"]);
        press(cx, f.window, &["enter"]);
        cx.run_until_parked();
        assert!(f.workspace.read_with(cx, |w, _| w.finder.is_none()));
        let last = area.read_with(cx, |a, cx| a.layout(cx).tabs.last().cloned());
        assert_eq!(last, Some(TabLayout::File { path: root.join("src").join("beta.rs"), beside: None }));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn shortcuts_follow_the_settings(cx: &mut TestAppContext) {
        use wercode_core::config::Keys;
        let f = open(cx, "keys");
        let open_now = |cx: &mut TestAppContext| f.workspace.read_with(cx, |w, _| w.panel_open);
        let set = |cx: &mut TestAppContext, name: &str, keys: Option<&str>| {
            cx.update(|cx| {
                crate::theme::change_settings(cx, |s| match keys {
                    Some(keys) => drop(s.keys.insert(name.into(), Keys::One(keys.into()))),
                    None => drop(s.keys.remove(name)),
                })
                .unwrap()
            });
        };
        assert!(open_now(cx));

        // The new shortcut works and the built-in one no longer does.
        set(cx, "workspace.toggle_panel", Some("Ctrl+Shift+Y"));
        press(cx, f.window, &["ctrl-shift-b"]);
        assert!(open_now(cx));
        press(cx, f.window, &["ctrl-shift-y"]);
        assert!(!open_now(cx));
        assert_eq!(cx.update(|cx| crate::keymap::label_for("workspace.toggle_panel", cx)).as_deref(), Some("Ctrl+Shift+Y"));

        // An empty value leaves the command without a shortcut.
        set(cx, "workspace.toggle_panel", Some(""));
        press(cx, f.window, &["ctrl-shift-y", "ctrl-shift-b"]);
        assert!(!open_now(cx));

        // What cannot be used is said, and the rest still works.
        set(cx, "workspace.toggle_panel", Some("y-ctrl"));
        set(cx, "workspace.no_such_thing", Some("ctrl-shift-y"));
        assert_eq!(cx.update(|cx| crate::keymap::problems(cx)).len(), 2);
        press(cx, f.window, &["ctrl-shift-g"]);
        assert!(open_now(cx));

        // Without the setting the built-in shortcut is back.
        set(cx, "workspace.toggle_panel", None);
        set(cx, "workspace.no_such_thing", None);
        assert!(cx.update(|cx| crate::keymap::problems(cx)).is_empty());
        press(cx, f.window, &["ctrl-shift-y"]);
        assert!(open_now(cx));
        press(cx, f.window, &["ctrl-shift-b"]);
        assert!(!open_now(cx));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn shortcuts_are_recorded_on_the_settings_screen(cx: &mut TestAppContext) {
        let f = open(cx, "keyeditor");
        let settings_file = f.dir.join("settings.toml");
        std::fs::write(&settings_file, "# mine\nscrollback = 500\n").unwrap();
        cx.update(|cx| crate::theme::set_settings_path(Some(settings_file.clone()), cx));
        // The shortcuts are at the end of the screen: scroll there first.
        let click = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
            cx.update_window(f.window, |_, window, cx| {
                window.scroll("settings-body", gpui_kit::ScrollDelta::Pixels(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(-20000.))), cx)
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(f.window, |_, window, cx| window.click(id, cx)).unwrap();
            cx.run_until_parked();
        };
        let label = |cx: &mut TestAppContext| cx.update(|cx| crate::keymap::label_for("workspace.toggle_panel", cx));
        let row = cx.update(|cx| crate::keymap::listing(cx)).iter().position(|l| l.command == "workspace.toggle_panel").unwrap();
        cx.update_window(f.window, |_, window, cx| window.click("open-settings", cx)).unwrap();
        let view = f.workspace.read_with(cx, |w, _| w.settings.as_ref().map(|(view, _)| view.clone())).unwrap();
        cx.update_window(f.window, |_, window, cx| view.update(cx, |v, cx| v.filter_keys("side panel", window, cx))).unwrap();
        cx.run_until_parked();

        // The next key press is the new shortcut; it does not run the old meaning of the keys.
        click(cx, ("settings-key", row).into());
        press(cx, f.window, &["ctrl-shift-g"]);
        assert_eq!(label(cx).as_deref(), Some("Ctrl+Shift+G"));
        assert!(f.workspace.read_with(cx, |w, _| w.settings.is_some()));
        assert_eq!(f.workspace.read_with(cx, |w, _| w.panel), PanelKind::Files);
        let text = std::fs::read_to_string(&settings_file).unwrap();
        assert!(text.starts_with("# mine\nscrollback = 500\n") && text.contains("\"workspace.toggle_panel\" = \"ctrl-shift-g\""), "{text}");

        // A letter alone is refused, Escape stops without a change.
        click(cx, ("settings-key", row).into());
        press(cx, f.window, &["y"]);
        assert_eq!(label(cx).as_deref(), Some("Ctrl+Shift+G"));
        press(cx, f.window, &["escape"]);
        assert!(f.workspace.read_with(cx, |w, _| w.settings.is_some()), "Escape only stopped the recording");
        assert_eq!(label(cx).as_deref(), Some("Ctrl+Shift+G"));

        // Removed, then back to the built-in one, which leaves the file as it was.
        click(cx, ("settings-key-clear", row).into());
        assert_eq!(label(cx), None);
        click(cx, ("settings-key-reset", row).into());
        assert_eq!(label(cx).as_deref(), Some("Ctrl+Shift+B"));
        let (saved, _) = wercode_core::config::load_from(&settings_file);
        assert!(saved.keys.is_empty());
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[gpui_kit::test]
    async fn a_file_dragged_onto_the_terminal_is_typed_as_a_path(cx: &mut TestAppContext) {
        let root = project_dir("drag");
        std::fs::write(root.join("only.txt"), "text\n").unwrap();
        let f = open(cx, "drag");
        let files = f.workspace.read_with(cx, |w, _| w.files());
        let area = f.workspace.read_with(cx, |w, _| w.terminals());
        wait_until(cx, "the file in the tree", |cx| files.read_with(cx, |p, cx| p.shows(&root.join("only.txt"), cx)));
        cx.update_window(f.window, |_, window, cx| {
            f.workspace.update(cx, |w, cx| w.open_file(root.join("only.txt"), None, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(area.read_with(cx, |a, cx| a.layout(cx).active), 1);

        // The file is showing; the drop goes to the terminal tab and shows it.
        cx.update_window(f.window, |_, window, cx| window.drag_to(("file-row", 0usize), "terminal-area", cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(area.read_with(cx, |a, cx| a.layout(cx).active), 0);
        let _ = std::fs::remove_dir_all(&f.dir);
    }
}
