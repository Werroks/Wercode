//! The git panel: what changed, what is staged, and a field to commit it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wercode_core::git::{self, Change, FileStatus, RepoStatus};
use wercode_core::{file_icons, fs_ops};

use super::files::status_color;
use super::graph::{GraphEvent, GraphView};
use wercode_core::i18n::{self, Language, t, tf};

actions!(
    git,
    [
        /// Brings in the remote's commits.
        Pull,
        /// Sends the branch's commits to its remote.
        Push,
        Fetch,
        /// Commits with the message in the git panel; without one, puts the
        /// keyboard in the message field.
        Commit,
        StageAll,
        /// Shows every change in the repository in one tab.
        ShowAllChanges
    ]
);

pub enum GitPanelEvent {
    /// A command changed the repository; its status should be read again.
    Changed,
    /// The file itself was asked for.
    Open(PathBuf),
    /// A file in one of the lists was clicked: show what changed in it.
    OpenDiff { path: PathBuf, staged: bool, untracked: bool },
    /// A file of a commit in the history was clicked.
    OpenCommitDiff { path: PathBuf, hash: String, parent: Option<String> },
    /// Every change on one side of the index, in one tab.
    OpenAllChanges { staged: bool },
}

/// The commands that talk to the remote.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Remote {
    Fetch,
    Pull,
    Push,
}

/// The last fetch, pull or push, for the status bar: these are often started
/// by a shortcut while the panel is hidden.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RemoteActivity {
    #[default]
    Idle,
    Running(Remote),
    /// Git's own words are in the panel.
    Failed(Remote),
}

impl RemoteActivity {
    /// What the status bar says, if anything.
    pub fn label(self) -> Option<&'static str> {
        Some(match self {
            RemoteActivity::Idle => return None,
            RemoteActivity::Running(Remote::Fetch) => t("Fetching\u{2026}"),
            RemoteActivity::Running(Remote::Pull) => t("Pulling\u{2026}"),
            RemoteActivity::Running(Remote::Push) => t("Pushing\u{2026}"),
            RemoteActivity::Failed(Remote::Fetch) => t("Fetch failed"),
            RemoteActivity::Failed(Remote::Pull) => t("Pull failed"),
            RemoteActivity::Failed(Remote::Push) => t("Push failed"),
        })
    }
}

/// Which of the two lists a row belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum List {
    Staged,
    Changes,
}

pub struct GitPanel {
    root: PathBuf,
    git: Option<Arc<RepoStatus>>,
    message: Entity<TextareaState>,
    /// A git command is running; the buttons wait for it.
    busy: bool,
    error: Option<String>,
    activity: RemoteActivity,
    /// The language the message field's hint is in; it follows the settings.
    language: Language,
    /// The history under the changes.
    graph: Entity<GraphView>,
    _subscriptions: [Subscription; 3],
}

impl EventEmitter<GitPanelEvent> for GitPanel {}

impl GitPanel {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| {
            TextareaState::new(window, cx).auto_grow(1, 8).placeholder(t("Commit message (Ctrl+Enter to commit)"))
        });
        let subscription =
            cx.subscribe_in(&message, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { secondary: true, .. } = event {
                    this.commit(window, cx);
                }
            });
        let graph = cx.new(|_| GraphView::new(root.clone()));
        let opened = cx.subscribe(&graph, |_, _, event: &GraphEvent, cx| match event {
            GraphEvent::OpenDiff { path, hash, parent } => cx.emit(GitPanelEvent::OpenCommitDiff {
                path: path.clone(),
                hash: hash.clone(),
                parent: parent.clone(),
            }),
        });
        // Folding the history away changes how the panel is split.
        let folded = cx.observe(&graph, |_, _, cx| cx.notify());
        Self {
            root,
            git: None,
            message,
            busy: false,
            error: None,
            activity: RemoteActivity::Idle,
            language: i18n::language(),
            graph,
            _subscriptions: [subscription, opened, folded],
        }
    }

    pub fn activity(&self) -> RemoteActivity {
        self.activity
    }

    pub fn has_message(&self, cx: &App) -> bool {
        !self.message.read(cx).value().trim().is_empty()
    }

    pub fn focus_message(&self, window: &mut Window, cx: &mut App) {
        self.message.update(cx, |input, cx| input.focus(window, cx));
    }

    pub fn stage_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.paths(List::Changes);
        self.move_files(List::Changes, paths, window, cx);
    }

    pub fn set_git(&mut self, git: Option<Arc<RepoStatus>>, cx: &mut Context<Self>) {
        self.graph.update(cx, |graph, cx| graph.set_git(git.as_deref(), cx));
        self.git = git;
        cx.notify();
    }

    fn paths(&self, list: List) -> Vec<PathBuf> {
        let Some(git) = &self.git else { return Vec::new() };
        match list {
            List::Staged => git.staged().map(|c| c.path.clone()).collect(),
            List::Changes => git.unstaged().map(|c| c.path.clone()).collect(),
        }
    }

    /// Runs a git command in the background, then asks for a fresh status.
    /// `done` runs in the window when the command succeeded.
    fn run(
        &mut self,
        command: impl FnOnce(&Path) -> Result<(), String> + Send + 'static,
        done: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        self.activity = RemoteActivity::Idle;
        cx.notify();
        let root = self.root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { command(&root) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if let RemoteActivity::Running(command) = this.activity {
                    this.activity =
                        if result.is_ok() { RemoteActivity::Idle } else { RemoteActivity::Failed(command) };
                }
                match result {
                    Ok(()) => done(this, window, cx),
                    Err(error) => this.error = Some(error),
                }
                cx.emit(GitPanelEvent::Changed);
                cx.notify();
            });
        })
        .detach();
    }

    /// Moves files into the index, or out of it when they come from the staged list.
    fn move_files(&mut self, from: List, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        self.run(
            move |root| match from {
                List::Changes => git::stage(root, &paths),
                List::Staged => git::unstage(root, &paths),
            },
            |_, _, _| {},
            window,
            cx,
        );
    }

    /// Fetches, pulls or pushes. Git's own words are shown when it fails
    /// (no remote, rejected push, no way to ask for a password).
    pub fn remote(&mut self, command: Remote, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.git.is_none() {
            return;
        }
        let has_upstream = self.git.as_ref().is_some_and(|git| git.has_upstream);
        self.run(
            move |root| match command {
                Remote::Fetch => git::fetch(root),
                Remote::Pull => git::pull(root),
                Remote::Push => git::push(root, has_upstream),
            },
            |_, _, _| {},
            window,
            cx,
        );
        self.activity = RemoteActivity::Running(command);
    }

    /// Asks, then throws away the changes to a file: a tracked file goes back
    /// to what is in the index, a new one goes to the trash.
    fn discard(&mut self, path: PathBuf, untracked: bool, window: &mut Window, cx: &mut Context<Self>) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (title, detail, button) = if untracked {
            (
                tf("Move {name} to the trash?", &[("name", &name)]),
                t("Git does not know this file yet. You can bring it back from the trash."),
                t("Move to Trash"),
            )
        } else {
            (
                tf("Discard changes in {name}?", &[("name", &name)]),
                t("The file goes back to its last staged or committed text. This cannot be undone."),
                t("Discard"),
            )
        };
        let answer = window.prompt(PromptLevel::Warning, &title, Some(detail), &[button, t("Cancel")], cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.run(
                    move |root| if untracked { fs_ops::trash(&path) } else { git::discard(root, &[path]) },
                    |_, _, _| {},
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    /// Commits the staged files; with nothing staged, everything that changed.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.message.read(cx).value().trim().to_string();
        if message.is_empty() {
            self.error = Some(t("Write a commit message first.").into());
            cx.notify();
            return;
        }
        let Some(git) = &self.git else { return };
        if git.changes.is_empty() {
            self.error = Some(t("There is nothing to commit.").into());
            cx.notify();
            return;
        }
        let stage_all = git.staged().next().is_none();
        self.run(
            move |root| {
                if stage_all {
                    git::stage(root, &[])?;
                }
                git::commit(root, &message)
            },
            |this, window, cx| this.message.update(cx, |input, cx| input.set_value("", window, cx)),
            window,
            cx,
        );
    }

    fn render_section(&self, list: List, cx: &mut Context<Self>) -> Option<AnyElement> {
        let git = self.git.clone()?;
        let changes: Vec<&Change> = match list {
            List::Staged => git.staged().collect(),
            List::Changes => git.unstaged().collect(),
        };
        if changes.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.list_hover);
        let (title, all_id, all_icon, all_tip, row_tip) = match list {
            List::Staged => (t("STAGED CHANGES"), "git-unstage-all", IconName::Minus, t("Unstage all"), t("Unstage")),
            List::Changes => (t("CHANGES"), "git-stage-all", IconName::Plus, t("Stage all"), t("Stage")),
        };
        let (prefix, button) = match list {
            List::Staged => ("git-staged", "git-unstage"),
            List::Changes => ("git-change", "git-stage"),
        };
        let (all_changes_id, staged) = match list {
            List::Staged => ("git-all-staged", true),
            List::Changes => ("git-all-changes", false),
        };
        let busy = self.busy;

        let rows = changes.into_iter().enumerate().map(|(ix, change)| {
            let status = match list {
                List::Staged => change.staged,
                List::Changes => change.unstaged,
            }
            .unwrap_or(FileStatus::Modified);
            let color = status_color(status, cx);
            let name = change.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let folder = change
                .path
                .parent()
                .and_then(|p| p.strip_prefix(&git.root).ok())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let picture = file_icons::for_file(&name).and_then(crate::assets::file_icon);
            let path = change.path.clone();
            let clicked = change.path.clone();
            let (opened, discarded) = (change.path.clone(), change.path.clone());
            let exists = status != FileStatus::Deleted;
            let untracked = status == FileStatus::Untracked;
            let staged = list == List::Staged;
            h_flex()
                .id((prefix, ix))
                .test_support()
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    cx.emit(GitPanelEvent::OpenDiff { path: clicked.clone(), staged, untracked })
                }))
                .w_full()
                .gap_1p5()
                .pl_3()
                .pr_2()
                .py_0p5()
                .text_sm()
                .hover(|s| s.bg(hover))
                .child(match picture {
                    Some(path) => img(path).flex_none().size(px(16.)).into_any_element(),
                    None => Icon::new(IconName::File).small().text_color(muted).into_any_element(),
                })
                .child(
                    div()
                        .flex_none()
                        .max_w(relative(0.7))
                        .truncate()
                        .text_color(color)
                        .when(status == FileStatus::Deleted, |d| d.line_through())
                        .child(name),
                )
                .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(folder))
                .when(exists, |d| {
                    d.child(
                        Button::new((if staged { "git-open-staged" } else { "git-open" }, ix))
                            .ghost()
                            .xsmall()
                            .icon(IconName::File)
                            .tooltip(t("Open the file"))
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                cx.emit(GitPanelEvent::Open(opened.clone()));
                            })),
                    )
                })
                .when(!staged, |d| {
                    d.child(
                        Button::new(("git-discard", ix))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(Lucide::Undo2))
                            .tooltip(t("Discard changes"))
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.discard(discarded.clone(), untracked, window, cx);
                            })),
                    )
                })
                .child(
                    Button::new((button, ix))
                        .ghost()
                        .xsmall()
                        .icon(all_icon.clone())
                        .tooltip(row_tip)
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            // The button is inside the row; the press is not for the row.
                            cx.stop_propagation();
                            this.move_files(list, vec![path.clone()], window, cx);
                        })),
                )
                .child(div().flex_none().w(px(12.)).text_xs().text_color(color).child(status.letter()))
        });
        let rows: Vec<_> = rows.collect();

        Some(
            v_flex()
                .w_full()
                .child(
                    h_flex()
                        .h(px(28.))
                        .pl_3()
                        .pr_2()
                        .gap_1p5()
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(muted)
                                .child(format!("{title}  {}", rows.len())),
                        )
                        .child(
                            Button::new(all_changes_id)
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(Lucide::FileDiff))
                                .tooltip(t("Show all changes in one tab (Ctrl+Shift+A)"))
                                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                    cx.emit(GitPanelEvent::OpenAllChanges { staged })
                                })),
                        )
                        .child(
                            Button::new(all_id)
                                .ghost()
                                .xsmall()
                                .icon(all_icon.clone())
                                .tooltip(all_tip)
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    let paths = this.paths(list);
                                    this.move_files(list, paths, window, cx);
                                })),
                        )
                        // Keeps the button above the row buttons, which are followed by a letter.
                        .child(div().flex_none().w(px(12.))),
                )
                .children(rows)
                .into_any_element(),
        )
    }
}

impl Render for GitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language != i18n::language() {
            self.language = i18n::language();
            let hint = t("Commit message (Ctrl+Enter to commit)");
            self.message.update(cx, |input, cx| input.set_placeholder(hint, window, cx));
        }
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let note = |text: &'static str| div().p_3().text_sm().text_color(muted).child(text);

        let Some(git) = self.git.clone() else {
            return v_flex()
                .size_full()
                .child(note(t("This folder is not a git repository. Run git init in the terminal to start one.")))
                .into_any_element();
        };
        let nothing_staged = git.staged().next().is_none();
        let clean = git.changes.is_empty();
        let label = if nothing_staged && !clean { t("Stage All and Commit") } else { t("Commit") };
        let staged = self.render_section(List::Staged, cx);
        let changes = self.render_section(List::Changes, cx);
        let busy = self.busy;
        let remote = |id: &'static str, icon: Lucide, label: String, tooltip: &'static str, command: Remote| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(Icon::new(icon))
                .label(label)
                .tooltip(tooltip)
                .disabled(busy)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.remote(command, window, cx)))
        };
        let count = |word: &str, n: u32| if n > 0 { format!("{word} {n}") } else { word.to_string() };
        let push_label = if git.has_upstream { count(t("Push"), git.ahead) } else { t("Publish").to_string() };
        let remotes = h_flex()
            .gap_1()
            .child(remote("git-fetch", Lucide::RefreshCw, t("Fetch").into(), t("Fetch from all remotes"), Remote::Fetch))
            .child(remote("git-pull", Lucide::ArrowDownToLine, count(t("Pull"), git.behind), t("Pull"), Remote::Pull))
            .child(remote(
                "git-push",
                Lucide::ArrowUpFromLine,
                push_label,
                if git.has_upstream { t("Push") } else { t("Push this branch to origin for the first time") },
                Remote::Push,
            ));

        v_flex()
            .id("git-panel")
            .size_full()
            .child(
                v_flex()
                    .flex_none()
                    .p_2()
                    .gap_2()
                    .child(remotes)
                    .child(Textarea::new(&self.message))
                    .child(
                        Button::new("git-commit")
                            .primary()
                            .small()
                            .w_full()
                            .label(label)
                            .loading(self.busy)
                            .disabled(self.busy || clean)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.commit(window, cx))),
                    )
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().id("git-error").text_xs().text_color(danger).child(error))
                    }),
            )
            .child(div().flex_1().min_h_0().child(self.render_lists(staged, changes, clean, cx)))
            .into_any_element()
    }
}

impl GitPanel {
    /// The changes above, the history below, with a border to drag between them.
    fn render_lists(
        &self,
        staged: Option<AnyElement>,
        changes: Option<AnyElement>,
        clean: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let lists = v_flex()
            .id("git-changes")
            .size_full()
            .overflow_y_scroll()
            .children(staged)
            .children(changes)
            .when(clean, |d| d.child(div().p_3().text_sm().text_color(muted).child(t("No changes. The working tree is clean."))));
        if self.graph.read(cx).collapsed() {
            return v_flex().size_full().child(div().flex_1().min_h_0().child(lists)).child(self.graph.clone()).into_any_element();
        }
        v_resizable("git-split")
            .child(resizable_panel().child(lists))
            .child(resizable_panel().size(px(300.)).size_range(px(60.)..px(4000.)).child(self.graph.clone()))
            .into_any_element()
    }
}

#[cfg(test)]
impl GitPanel {
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub fn write_message(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.message.update(cx, |input, cx| input.set_value(text.to_string(), window, cx));
    }

    pub fn message_text(&self, cx: &App) -> String {
        self.message.read(cx).value().to_string()
    }

    pub fn graph(&self) -> Entity<GraphView> {
        self.graph.clone()
    }

    pub fn message_focused(&self, window: &Window, cx: &App) -> bool {
        self.message.read(cx).focus_handle(cx).is_focused(window)
    }
}
