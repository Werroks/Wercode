//! The commit history under the changes in the git panel: a graph of lanes,
//! each commit with its message, branches and author. A click on a commit
//! lists its files; a click on a file shows what the commit changed in it.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wercode_core::file_icons;
use wercode_core::git::{FileStatus, RepoStatus};
use wercode_core::history::{self, Ago, Commit, CommitFile, RefKind, Row};

use super::files::status_color;

/// How many commits are read at first, and how many more each time.
const PAGE: usize = 200;
const ROW_HEIGHT: f32 = 24.;
const LANE_WIDTH: f32 = 14.;
/// Room left of the first lane, so its dots are not cut by the panel's edge.
const GRAPH_PAD: f32 = 8.;
/// Wider graphs are cut off on the right; the messages matter more.
const MAX_LANES: usize = 8;

/// Lane colours, the first for the line of the current branch.
const LANE_COLORS: [u32; 8] = [0x4b9ef9, 0xffb000, 0xdc267f, 0x40b0a6, 0xb66dff, 0xe5774b, 0x8fc34a, 0x9aa0a6];

pub enum GraphEvent {
    /// Show what a commit changed in a file.
    OpenDiff { path: PathBuf, hash: String, parent: Option<String> },
}

/// What the history was read for. When any of it changes, it is read again.
#[derive(Clone, PartialEq, Eq)]
struct Key {
    head: String,
    branch: Option<String>,
    has_upstream: bool,
    ahead: u32,
    behind: u32,
    limit: usize,
}

/// The files of the commit that is open.
struct Open {
    hash: String,
    files: Option<Result<Vec<CommitFile>, String>>,
}

pub struct GraphView {
    /// The project folder, where git is run.
    root: PathBuf,
    /// The repository's top folder: commit paths are relative to it.
    repo: Option<PathBuf>,
    key: Option<Key>,
    commits: Arc<Vec<Commit>>,
    rows: Arc<Vec<Row>>,
    limit: usize,
    loading: bool,
    open: Option<Open>,
    collapsed: bool,
    task: Option<Task<()>>,
    files_task: Option<Task<()>>,
}

impl EventEmitter<GraphEvent> for GraphView {}

impl GraphView {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            repo: None,
            key: None,
            commits: Arc::default(),
            rows: Arc::default(),
            limit: PAGE,
            loading: false,
            open: None,
            collapsed: false,
            task: None,
            files_task: None,
        }
    }

    pub fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// Reads the history again when the repository moved on.
    pub fn set_git(&mut self, git: Option<&RepoStatus>, cx: &mut Context<Self>) {
        let Some(git) = git else {
            self.repo = None;
            self.key = None;
            self.commits = Arc::default();
            self.rows = Arc::default();
            cx.notify();
            return;
        };
        self.repo = Some(git.root.clone());
        let key = Key {
            head: git.head.clone(),
            branch: git.branch.clone(),
            has_upstream: git.has_upstream,
            ahead: git.ahead,
            behind: git.behind,
            limit: self.limit,
        };
        if self.key.as_ref() != Some(&key) {
            self.key = Some(key);
            self.reload(cx);
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else { return };
        let root = self.root.clone();
        self.loading = true;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let (commits, rows) = cx
                .background_executor()
                .spawn(async move {
                    let commits = history::log(&root, key.has_upstream, key.limit);
                    let rows = history::layout(&commits);
                    (commits, rows)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.commits = Arc::new(commits);
                this.rows = Arc::new(rows);
                cx.notify();
            });
        }));
    }

    fn show_more(&mut self, cx: &mut Context<Self>) {
        self.limit += PAGE;
        if let Some(key) = &mut self.key {
            key.limit = self.limit;
        }
        self.reload(cx);
    }

    /// Opens a commit's list of files, or closes it when it is open.
    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(commit) = self.commits.get(index).cloned() else { return };
        if self.open.as_ref().is_some_and(|open| open.hash == commit.hash) {
            self.open = None;
            cx.notify();
            return;
        }
        self.open = Some(Open { hash: commit.hash.clone(), files: None });
        cx.notify();
        let (root, hash) = (self.root.clone(), commit.hash.clone());
        self.files_task = Some(cx.spawn(async move |this, cx| {
            let files = cx.background_executor().spawn(async move { history::commit_files(&root, &commit) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(open) = this.open.as_mut().filter(|open| open.hash == hash) {
                    open.files = Some(files);
                    cx.notify();
                }
            });
        }));
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let collapsed = self.collapsed;
        h_flex()
            .id("graph-header")
            .test_support()
            .flex_none()
            .h(px(28.))
            .pl_1()
            .pr_2()
            .gap_1()
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.muted_foreground)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.collapsed = !this.collapsed;
                cx.notify();
            }))
            .child(Icon::new(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown }).xsmall())
            .child(div().flex_1().child("GRAPH"))
            .when(!collapsed, |d| {
                d.child(
                    Button::new("graph-refresh")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(Lucide::RefreshCw))
                        .tooltip("Refresh")
                        .loading(self.loading)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.reload(cx);
                        })),
                )
            })
    }

    fn render_commit(&self, index: usize, commit: &Commit, row: &Row, lanes: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.list_hover);
        let is_open = self.open.as_ref().is_some_and(|open| open.hash == commit.hash);
        let is_head = commit.refs.iter().any(|r| r.kind == RefKind::Head);
        let tip = CommitTip::new(commit);
        let refs = commit.refs.iter().map(|r| {
            let color = match r.kind {
                RefKind::Head => lane_color(0),
                RefKind::Branch => lane_color(0),
                RefKind::Remote => lane_color(4),
                RefKind::Tag => lane_color(1),
            };
            let pill = div()
                .flex_none()
                .px_1p5()
                .rounded(px(4.))
                .border_1()
                .border_color(color)
                .text_xs()
                .text_color(color)
                .child(r.name.clone());
            if r.kind == RefKind::Head { pill.bg(color.opacity(0.18)) } else { pill }
        });
        h_flex()
            .id(("graph-commit", index))
            .test_support()
            .h(px(ROW_HEIGHT))
            .w_full()
            .pr_2()
            .gap_1p5()
            .text_sm()
            .when(is_open, |d| d.bg(theme.list_active))
            .hover(|s| s.bg(hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle(index, cx)))
            .tooltip(move |_, cx| cx.new(|_| tip.clone()).into())
            .child(lanes_canvas(row.clone(), lanes, theme.background, is_head))
            .child(div().flex_1().min_w_0().truncate().child(commit.subject.clone()))
            .children(refs)
            .child(div().flex_none().max_w(px(120.)).truncate().text_xs().text_color(muted).child(commit.author.clone()))
            .into_any_element()
    }

    /// The files of the open commit, under it.
    fn render_files(&self, commit: &Commit, row: &Row, lanes: usize, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(open) = self.open.as_ref().filter(|open| open.hash == commit.hash) else { return Vec::new() };
        let theme = cx.theme();
        let (muted, hover, danger) = (theme.muted_foreground, theme.list_hover, theme.danger);
        // The lanes that go on below this commit continue past its files.
        let below = Row {
            lane: usize::MAX,
            through: row.through.iter().chain(&row.out).copied().collect(),
            ..Row::default()
        };
        let note = |text: String, color: Hsla| {
            h_flex()
                .h(px(ROW_HEIGHT))
                .gap_1p5()
                .text_xs()
                .text_color(color)
                .child(lanes_canvas(below.clone(), lanes, theme.background, false))
                .child(text)
                .into_any_element()
        };
        let files = match &open.files {
            None => return vec![note("Reading\u{2026}".into(), muted)],
            Some(Err(error)) => return vec![note(error.clone(), danger)],
            Some(Ok(files)) if files.is_empty() => return vec![note("No files changed.".into(), muted)],
            Some(Ok(files)) => files,
        };
        let repo = self.repo.clone().unwrap_or_else(|| self.root.clone());
        let parent = commit.parents.first().cloned();
        files
            .iter()
            .enumerate()
            .map(|(ix, file)| {
                let (folder, name) = match file.path.rsplit_once('/') {
                    Some((folder, name)) => (folder.to_string(), name.to_string()),
                    None => (String::new(), file.path.clone()),
                };
                let color = status_color(file.status, cx);
                let picture = file_icons::for_file(&name).and_then(crate::assets::file_icon);
                let path = repo.join(&file.path);
                let (hash, parent) = (commit.hash.clone(), parent.clone());
                h_flex()
                    .id(("graph-file", ix))
                    .test_support()
                    .h(px(ROW_HEIGHT))
                    .w_full()
                    .pr_2()
                    .gap_1p5()
                    .text_sm()
                    .hover(|s| s.bg(hover))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        cx.emit(GraphEvent::OpenDiff { path: path.clone(), hash: hash.clone(), parent: parent.clone() })
                    }))
                    .child(lanes_canvas(below.clone(), lanes, theme.background, false))
                    .child(match picture {
                        Some(path) => img(path).flex_none().size(px(16.)).into_any_element(),
                        None => Icon::new(IconName::File).small().text_color(muted).into_any_element(),
                    })
                    .child(
                        div()
                            .flex_none()
                            .max_w(relative(0.6))
                            .truncate()
                            .text_color(color)
                            .when(file.status == FileStatus::Deleted, |d| d.line_through())
                            .child(name),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(folder))
                    .child(div().flex_none().w(px(12.)).text_xs().text_color(color).child(file.status.letter()))
                    .into_any_element()
            })
            .collect()
    }
}

impl Render for GraphView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        if self.collapsed {
            return v_flex().id("graph").w_full().flex_none().child(header).into_any_element();
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let (commits, rows) = (self.commits.clone(), self.rows.clone());
        let lanes = rows.iter().map(Row::width).max().unwrap_or(1).min(MAX_LANES);
        let mut items: Vec<AnyElement> = Vec::with_capacity(commits.len() + 1);
        for (index, (commit, row)) in commits.iter().zip(rows.iter()).enumerate() {
            items.push(self.render_commit(index, commit, row, lanes, cx));
            items.extend(self.render_files(commit, row, lanes, cx));
        }
        if commits.is_empty() && !self.loading {
            items.push(div().p_3().text_sm().text_color(muted).child("No commits yet.").into_any_element());
        }
        if commits.len() >= self.limit {
            items.push(
                div()
                    .p_2()
                    .child(
                        Button::new("graph-more")
                            .ghost()
                            .small()
                            .label("Show More Commits")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.show_more(cx))),
                    )
                    .into_any_element(),
            );
        }
        v_flex()
            .id("graph")
            .size_full()
            .child(header)
            .child(v_flex().id("graph-commits").flex_1().min_h_0().overflow_y_scroll().children(items))
            .into_any_element()
    }
}

fn lane_color(index: usize) -> Hsla {
    rgb(LANE_COLORS[index % LANE_COLORS.len()]).into()
}

/// The lanes of one row: lines passing by, lines into and out of the dot,
/// and the dot. A row whose `lane` is past the edge draws only lines. The
/// commit checked out (`head`) gets a ring around its dot.
fn lanes_canvas(row: Row, lanes: usize, background: Hsla, head: bool) -> impl IntoElement {
    let width = GRAPH_PAD + LANE_WIDTH * lanes as f32 + 2.;
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let x = |lane: usize| bounds.origin.x + px(GRAPH_PAD + LANE_WIDTH * lane as f32 + LANE_WIDTH / 2.);
            let top = bounds.origin.y;
            let middle = top + bounds.size.height / 2.;
            let bottom = top + bounds.size.height;
            let visible = |lane: usize| lane < lanes;
            let mut stroke = |from: Point<Pixels>, to: Point<Pixels>, bend: Option<Point<Pixels>>, color: usize| {
                let mut path = PathBuilder::stroke(px(1.5));
                path.move_to(from);
                match bend {
                    Some(corner) if from.x != to.x => path.curve_to(to, corner),
                    _ => path.line_to(to),
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, lane_color(color));
                }
            };
            for &(lane, color) in &row.through {
                if visible(lane) {
                    stroke(point(x(lane), top), point(x(lane), bottom), None, color);
                }
            }
            if !visible(row.lane) {
                return;
            }
            let dot = point(x(row.lane), middle);
            for &(lane, color) in &row.into {
                // Down from the lane above, turning into the dot.
                let from = point(x(lane.min(lanes - 1)), top);
                stroke(from, dot, Some(point(from.x, middle)), color);
            }
            for &(lane, color) in &row.out {
                // Out of the dot, turning down into the lane below.
                let to = point(x(lane.min(lanes - 1)), bottom);
                stroke(dot, to, Some(point(to.x, middle)), color);
            }
            let circle = |radius: f32| Bounds::new(point(dot.x - px(radius), dot.y - px(radius)), size(px(radius * 2.), px(radius * 2.)));
            let color = lane_color(row.color);
            if head {
                window.paint_quad(fill(circle(5.5), color).corner_radii(px(5.5)));
                window.paint_quad(fill(circle(3.5), background).corner_radii(px(3.5)));
                window.paint_quad(fill(circle(2.), color).corner_radii(px(2.)));
            } else {
                window.paint_quad(fill(circle(4.), color).corner_radii(px(4.)));
                if row.merge {
                    window.paint_quad(fill(circle(2.), background).corner_radii(px(2.)));
                }
            }
        },
    )
    .flex_none()
    .w(px(width))
    .h(px(ROW_HEIGHT))
}

/// "5 minutes ago" and the like.
pub fn ago_text(ago: Ago) -> String {
    let count = |n: u64, one: &str, many: &str| if n == 1 { one.to_string() } else { many.replace("{n}", &n.to_string()) };
    match ago {
        Ago::JustNow => "just now".into(),
        Ago::Minutes(n) => count(n, "1 minute ago", "{n} minutes ago"),
        Ago::Hours(n) => count(n, "1 hour ago", "{n} hours ago"),
        Ago::Days(n) => count(n, "1 day ago", "{n} days ago"),
        Ago::Weeks(n) => count(n, "1 week ago", "{n} weeks ago"),
        Ago::Months(n) => count(n, "1 month ago", "{n} months ago"),
        Ago::Years(n) => count(n, "1 year ago", "{n} years ago"),
    }
}

/// The card shown when the pointer rests on a commit.
#[derive(Clone)]
struct CommitTip {
    subject: SharedString,
    details: SharedString,
}

impl CommitTip {
    fn new(commit: &Commit) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(commit.time);
        let when = ago_text(history::ago(commit.time, now));
        Self {
            subject: commit.subject.clone().into(),
            details: format!("{}  \u{b7}  {}  \u{b7}  {when}", commit.short(), commit.author).into(),
        }
    }
}

impl Render for CommitTip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .max_w(px(420.))
            .gap_1()
            .px_2()
            .py_1p5()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_md()
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(self.subject.clone()))
            .child(div().text_xs().text_color(theme.muted_foreground).child(self.details.clone()))
    }
}

#[cfg(test)]
impl GraphView {
    pub fn subjects(&self) -> Vec<String> {
        self.commits.iter().map(|c| c.subject.clone()).collect()
    }

    pub fn rows(&self) -> Arc<Vec<Row>> {
        self.rows.clone()
    }

    /// How many files the open commit lists, once they are read.
    pub fn open_files(&self) -> Option<usize> {
        self.open.as_ref()?.files.as_ref()?.as_ref().ok().map(Vec::len)
    }
}
