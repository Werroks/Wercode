//! The window's root view: title bar with the project switcher, terminals in
//! the centre, a side panel (files, search, git) and a status bar.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wercode_core::git::{self, RepoStatus};
use wercode_core::state::{self, State};

use crate::panel::{FilesPanel, GitPanel, GitPanelEvent};
use crate::term::{
    AreaEvent, ClosePane, CloseTab, NewTab, NextPane, NextTab, PrevPane, PrevTab, SplitDown, SplitRight,
    TerminalArea,
};
use crate::theme::AppState;

actions!(
    workspace,
    [TogglePanel, OpenFolder, ShowFiles, ShowSearch, ShowGit, SwitchProject, CommandPalette]
);

/// How often the repository status is read again while the window is active.
const GIT_POLL: Duration = Duration::from_secs(2);
/// Resizing produces a burst of changes; the state file is written once after it.
const SAVE_DELAY: Duration = Duration::from_millis(400);

/// Workspace shortcuts sit on Ctrl+Shift: plain Ctrl keys belong to the shell
/// and to the programs running in it.
pub fn init(cx: &mut App) {
    let context = Some("Workspace");
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-b", TogglePanel, context),
        KeyBinding::new("ctrl-shift-o", OpenFolder, context),
        KeyBinding::new("ctrl-shift-f", ShowSearch, context),
        KeyBinding::new("ctrl-shift-g", ShowGit, context),
        KeyBinding::new("ctrl-shift-p", CommandPalette, context),
    ]);
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
}

impl PanelKind {
    fn name(self) -> &'static str {
        match self {
            PanelKind::Files => "files",
            PanelKind::Search => "search",
            PanelKind::Git => "git",
        }
    }

    fn from_name(name: &str) -> Self {
        match name {
            "search" => PanelKind::Search,
            "git" => PanelKind::Git,
            _ => PanelKind::Files,
        }
    }
}

/// Everything in the palette: label and the action it runs. The shortcut shown
/// next to each one comes from the key bindings.
fn palette_commands() -> Vec<(&'static str, Box<dyn Action>)> {
    vec![
        ("Terminal: New Tab", Box::new(NewTab)),
        ("Terminal: Split Right", Box::new(SplitRight)),
        ("Terminal: Split Down", Box::new(SplitDown)),
        ("Terminal: Close Pane", Box::new(ClosePane)),
        ("Terminal: Close Tab", Box::new(CloseTab)),
        ("Terminal: Next Tab", Box::new(NextTab)),
        ("Terminal: Previous Tab", Box::new(PrevTab)),
        ("Terminal: Next Pane", Box::new(NextPane)),
        ("Terminal: Previous Pane", Box::new(PrevPane)),
        ("View: Toggle Side Panel", Box::new(TogglePanel)),
        ("View: Show Files", Box::new(ShowFiles)),
        ("View: Show Search", Box::new(ShowSearch)),
        ("View: Show Git", Box::new(ShowGit)),
        ("Project: Open Folder\u{2026}", Box::new(OpenFolder)),
        ("Project: Switch to Recent\u{2026}", Box::new(SwitchProject)),
    ]
}

/// An open folder with everything that belongs to it.
struct Project {
    root: PathBuf,
    terminals: Entity<TerminalArea>,
    files: Entity<FilesPanel>,
    changes: Entity<GitPanel>,
    /// `None` outside a repository, and until the first read finishes.
    git: Option<Arc<RepoStatus>>,
    _subscriptions: [Subscription; 2],
    _git_poll: Task<()>,
}

pub struct Workspace {
    /// `None` shows the welcome screen.
    project: Option<Project>,
    panel: PanelKind,
    panel_open: bool,
    /// The list of recent projects under the title is showing.
    switcher_open: bool,
    palette: Option<Entity<CommandState>>,
    /// A terminal command is on its way to the terminals.
    forwarding: bool,
    state: State,
    /// Where `state` is saved; `None` keeps it in memory only.
    state_path: Option<PathBuf>,
    save_task: Option<Task<()>>,
    _bounds: Subscription,
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
            Self::open(root, window, cx)
        });
        if project.is_none() {
            window.set_window_title("Wercode");
        }
        let bounds = cx.observe_window_bounds(window, |this, window, cx| this.remember_window(window, cx));
        let this = Self {
            project,
            panel: PanelKind::from_name(&state.panel),
            panel_open: state.panel_open,
            switcher_open: false,
            palette: None,
            forwarding: false,
            state,
            state_path,
            save_task: None,
            _bounds: bounds,
        };
        this.save();
        this
    }

    fn open(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Project {
        let terminals = cx.new(|cx| TerminalArea::new(root.clone(), window, cx));
        let files = cx.new(|cx| FilesPanel::new(root.clone(), window, cx));
        let changes = cx.new(|cx| GitPanel::new(root.clone(), window, cx));
        let subscriptions = [
            cx.subscribe(&terminals, |_, _, event, cx| match event {
                // Until the built-in editor exists, a clicked path opens in
                // whatever the system uses for that file type.
                AreaEvent::OpenPath { path, .. } => cx.open_with_system(path),
            }),
            // Staging or committing shows at once, without waiting for the next poll.
            cx.subscribe(&changes, |this, _, event, cx| match event {
                GitPanelEvent::Changed => this.refresh_git(cx),
            }),
        ];
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

        Project { root, terminals, files, changes, git: None, _subscriptions: subscriptions, _git_poll: git_poll }
    }

    #[cfg(test)]
    fn pane_count(&self, cx: &App) -> usize {
        self.project.as_ref().map_or(0, |p| p.terminals.read(cx).pane_count())
    }

    #[cfg(test)]
    fn root(&self) -> Option<PathBuf> {
        self.project.as_ref().map(|p| p.root.clone())
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
        project.git = status.map(Arc::new);
        let git = project.git.clone();
        project.changes.update(cx, |changes, cx| changes.set_git(git.clone(), cx));
        project.files.update(cx, |files, cx| {
            files.set_git(git, cx);
            if files_changed {
                files.refresh(cx);
            }
        });
        cx.notify();
    }

    fn save(&self) {
        if let Some(path) = &self.state_path
            && let Err(e) = state::save_to(path, &self.state)
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

    fn toggle_panel(&mut self, cx: &mut Context<Self>) {
        self.panel_open = !self.panel_open;
        self.state.panel_open = self.panel_open;
        self.save();
        cx.notify();
    }

    /// Shows a side panel; choosing the one already showing hides it.
    fn show_panel(&mut self, kind: PanelKind, cx: &mut Context<Self>) {
        if self.panel_open && self.panel == kind {
            self.panel_open = false;
        } else {
            self.panel = kind;
            self.panel_open = true;
        }
        self.state.panel = self.panel.name().into();
        self.state.panel_open = self.panel_open;
        self.save();
        cx.notify();
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switcher_open = false;
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
        let busy = self.project.as_ref().map_or(0, |p| p.terminals.read(cx).busy_count(cx));
        if busy == 0 || !cx.global::<AppState>().settings.confirm_close {
            self.open_folder(path, window, cx);
            return;
        }
        let detail = format!(
            "{busy} terminal(s) in {} are still running a program. Switching closes them.",
            current.map(folder_name).unwrap_or_default()
        );
        let answer = window.prompt(
            PromptLevel::Warning,
            "Switch project?",
            Some(&detail),
            &["Switch", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| this.open_folder(path, window, cx));
            }
        })
        .detach();
    }

    fn open_folder(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.state.touch_recent(&path);
        let project = Self::open(path, window, cx);
        project.terminals.focus_handle(cx).focus(window, cx);
        self.project = Some(project);
        self.save();
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
                                .tooltip("Toggle side panel (Ctrl+Shift+B)")
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
            .child("Open folder\u{2026}")
            .child(div().flex_1())
            .child(div().text_xs().text_color(muted).child("Ctrl+Shift+O"))
    }

    /// A dimmed layer over the window that closes whatever floats on it.
    fn backdrop(&self, id: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        div().id(id).absolute().inset_0().occlude().on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                this.switcher_open = false;
                this.close_palette(window, cx);
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
                .child(div().px_3().py_1().text_xs().text_color(muted).child("RECENT PROJECTS"))
                .children(rows)
                .child(div().my_1().h(px(1.)).bg(border))
                .child(self.open_folder_row(cx)),
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
                        .placeholder("Type a command\u{2026}")
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
                        .child("A terminal with your files and git next to it. Open a folder to start."),
                )
                .child(self.open_folder_row(cx))
                .when(has_recent, |d| {
                    d.child(div().my_2().h(px(1.)).bg(border))
                        .child(div().px_3().py_1().text_xs().text_color(muted).child("RECENT PROJECTS"))
                        .children(rows)
                }),
        )
    }

    fn render_panel(&self, project: &Project, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted, selected_bg) = (theme.border, theme.muted_foreground, theme.secondary_hover);
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
        let placeholder = |text: &'static str| {
            div().p_3().text_sm().text_color(muted).child(text).into_any_element()
        };
        let body = match self.panel {
            PanelKind::Files => project.files.clone().into_any_element(),
            PanelKind::Search => placeholder("Search across the project is not built yet."),
            PanelKind::Git => project.changes.clone().into_any_element(),
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
                    .child(tab("panel-files", Icon::new(IconName::Folder), "Files", PanelKind::Files))
                    .child(tab(
                        "panel-search",
                        Icon::new(IconName::Search),
                        "Search (Ctrl+Shift+F)",
                        PanelKind::Search,
                    ))
                    .child(tab("panel-git", Icon::new(Lucide::GitBranch), "Git (Ctrl+Shift+G)", PanelKind::Git)),
            )
            .child(div().flex_1().min_h_0().child(body))
    }

    /// Branch, unpushed commits and changes. The branch takes the colour of
    /// the worst thing true about the working tree.
    fn render_status(&self, project: &Project, cx: &mut Context<Self>) -> impl IntoElement {
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
                    None => format!("detached at {}", git.head),
                };
                let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
                h_flex()
                    .id("status-git")
                    .test_support()
                    .gap_3()
                    .child(
                        item()
                            .text_color(color)
                            .child(Icon::new(Lucide::GitBranch).xsmall())
                            .child(if changed > 0 { format!("{branch}*") } else { branch }),
                    )
                    .when(git.ahead > 0, |d| {
                        d.child(
                            item()
                                .text_color(theme.link)
                                .child(Icon::new(IconName::ArrowUp).xsmall())
                                .child(format!("{} to push", plural(git.ahead as usize, "commit"))),
                        )
                    })
                    .when(git.behind > 0, |d| {
                        d.child(
                            item()
                                .child(Icon::new(IconName::ArrowDown).xsmall())
                                .child(format!("{} to pull", plural(git.behind as usize, "commit"))),
                        )
                    })
                    .when(git.branch.is_some() && !git.has_upstream, |d| d.child(item().child("not published")))
                    .when(changed > 0, |d| {
                        d.child(item().text_color(theme.warning).child(plural(changed, "change")))
                    })
                    .when(conflicts > 0, |d| {
                        d.child(
                            item()
                                .text_color(theme.danger)
                                .child(Icon::new(IconName::TriangleAlert).xsmall())
                                .child(plural(conflicts, "conflict")),
                        )
                    })
                    .when(changed == 0, |d| d.child(item().child("clean")))
                    .into_any_element()
            }
            None => item().child("No git repository").into_any_element(),
        };

        h_flex()
            .id("status-bar")
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
            .child(div().flex_1())
            .child(
                div()
                    .id("status-palette")
                    .px_1()
                    .rounded_sm()
                    .hover(|s| s.bg(theme.secondary_hover))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_palette(window, cx)))
                    .child("Commands  Ctrl+Shift+P"),
            )
    }

    fn render_body(&self, project: &Project, cx: &mut Context<Self>) -> AnyElement {
        if !self.panel_open {
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
        let (body, status) = match &self.project {
            Some(project) => {
                (self.render_body(project, cx), Some(self.render_status(project, cx).into_any_element()))
            }
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
            .on_action(cx.listener(|this, _: &OpenFolder, window, cx| this.pick_folder(window, cx)))
            .on_action(cx.listener(|this, _: &ShowFiles, _, cx| {
                if !(this.panel_open && this.panel == PanelKind::Files) {
                    this.show_panel(PanelKind::Files, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ShowSearch, _, cx| this.show_panel(PanelKind::Search, cx)))
            .on_action(cx.listener(|this, _: &ShowGit, _, cx| this.show_panel(PanelKind::Git, cx)))
            .on_action(cx.listener(|this, _: &SwitchProject, _, cx| {
                this.palette = None;
                this.switcher_open = true;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CommandPalette, window, cx| {
                if this.palette.is_some() { this.close_palette(window, cx) } else { this.open_palette(window, cx) }
            }))
            .on_action(cx.listener(|this, a: &NewTab, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &SplitRight, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &SplitDown, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &ClosePane, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &CloseTab, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &NextTab, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &PrevTab, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &NextPane, window, cx| this.forward(a, window, cx)))
            .on_action(cx.listener(|this, a: &PrevPane, window, cx| this.forward(a, window, cx)))
            .child(self.render_title(cx))
            .child(div().flex_1().min_h_0().child(body))
            .children(status)
            .when(self.switcher_open, |d| d.child(self.render_switcher(cx)))
            .when_some(self.palette.clone(), |d, palette| d.child(self.render_palette(&palette, cx)))
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
}
