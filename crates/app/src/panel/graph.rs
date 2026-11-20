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
use wercode_core::history::{self, Ago, Commit, CommitFile, Ref, RefKind, Row};

use super::files::status_color;
use wercode_core::i18n::{t, tn};

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
            .child(div().flex_1().child(t("GRAPH")))
            .when(!collapsed, |d| {
                d.child(
                    Button::new("graph-refresh")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(Lucide::RefreshCw))
                        .tooltip(t("Refresh"))
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
        // One badge, so the message keeps its room; the rest are counted, and the tooltip names them all.
        let badges = badges(&commit.refs);
        let more = badges.len().saturating_sub(1);
        let first = badges.into_iter().next().map(|badge| {
            let color = match badge.kind {
                RefKind::Head | RefKind::Branch => lane_color(0),
                RefKind::Remote => lane_color(4),
                RefKind::Tag => lane_color(1),
            };
            // A long name is cut at a fixed width: a badge left to shrink with the row
            // collapses to its ellipsis.
            h_flex()
                .flex_none()
                .gap_0p5()
                .px_1p5()
                .rounded(px(4.))
                .border_1()
                .border_color(color)
                .text_xs()
                .text_color(color)
                .when(badge.kind == RefKind::Head, |d| d.bg(color.opacity(0.18)))
                .child(div().flex_none().max_w(px(110.)).truncate().child(badge.name))
                .when(badge.remote, |d| d.child(Icon::new(Lucide::Cloud).xsmall().flex_none()))
        });
        let refs = h_flex()
            .flex_none()
            .gap_1()
            .children(first)
            .when(more > 0, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px_1()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(muted.opacity(0.5))
                        .text_xs()
                        .text_color(muted)
                        .child(format!("+{more}")),
                )
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
            // The message gives way first (the tooltip has it whole); the one badge stays whole.
            .child(div().flex_1().min_w(px(40.)).truncate().child(commit.subject.clone()))
            .child(refs)
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
            None => return vec![note(t("Reading\u{2026}").into(), muted)],
            Some(Err(error)) => return vec![note(error.clone(), danger)],
            Some(Ok(files)) if files.is_empty() => return vec![note(t("No files changed.").into(), muted)],
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
            items.push(div().p_3().text_sm().text_color(muted).child(t("No commits yet.")).into_any_element());
        }
        if commits.len() >= self.limit {
            items.push(
                div()
                    .p_2()
                    .child(
                        Button::new("graph-more")
                            .ghost()
                            .small()
                            .label(t("Show More Commits"))
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
    match ago {
        Ago::JustNow => t("just now").to_string(),
        Ago::Minutes(n) => tn(n, "{n} minute ago", "{n} minutes ago", &[]),
        Ago::Hours(n) => tn(n, "{n} hour ago", "{n} hours ago", &[]),
        Ago::Days(n) => tn(n, "{n} day ago", "{n} days ago", &[]),
        Ago::Weeks(n) => tn(n, "{n} week ago", "{n} weeks ago", &[]),
        Ago::Months(n) => tn(n, "{n} month ago", "{n} months ago", &[]),
        Ago::Years(n) => tn(n, "{n} year ago", "{n} years ago", &[]),
    }
}

/// The card shown when the pointer rests on a commit.
#[derive(Clone)]
struct CommitTip {
    subject: SharedString,
    details: SharedString,
    /// Every branch and tag on the commit, since the row shows only one.
    refs: Option<SharedString>,
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
            refs: (!commit.refs.is_empty())
                .then(|| commit.refs.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join("  \u{b7}  ").into()),
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
            .when_some(self.refs.clone(), |d, refs| d.child(div().text_xs().text_color(lane_color(0)).child(refs)))
    }
}

/// A branch or tag as the graph shows it.
#[derive(Debug, Clone, PartialEq)]
struct Badge {
    name: SharedString,
    kind: RefKind,
    /// A remote branch of the same name points at the same commit.
    remote: bool,
}

/// The badges of a commit, most telling first: the branch checked out, other
/// branches, tags, then remote branches no local one stands for. A local
/// branch takes in the remote ones of its name on the same commit, so `main`
/// and `origin/main` make one badge with a cloud.
fn badges(refs: &[Ref]) -> Vec<Badge> {
    let mut taken = vec![false; refs.len()];
    let mut out = Vec::new();
    for (i, r) in refs.iter().enumerate() {
        if !matches!(r.kind, RefKind::Head | RefKind::Branch) {
            continue;
        }
        let mut remote = false;
        for (j, other) in refs.iter().enumerate() {
            if other.kind == RefKind::Remote && other.name.split_once('/').is_some_and(|(_, branch)| branch == r.name) {
                taken[j] = true;
                remote = true;
            }
        }
        taken[i] = true;
        out.push(Badge { name: r.name.clone().into(), kind: r.kind, remote });
    }
    out.extend(
        refs.iter()
            .zip(&taken)
            .filter(|(_, taken)| !**taken)
            .map(|(r, _)| Badge { name: r.name.clone().into(), kind: r.kind, remote: false }),
    );
    out.sort_by_key(|b| match b.kind {
        RefKind::Head => 0,
        RefKind::Branch => 1,
        RefKind::Tag => 2,
        RefKind::Remote => 3,
    });
    out
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

#[cfg(test)]
mod badge_tests {
    use super::{Badge, badges};
    use wercode_core::history::{Ref, RefKind};

    fn r(name: &str, kind: RefKind) -> Ref {
        Ref { name: name.to_string(), kind }
    }

    fn shown(refs: &[Ref]) -> Vec<(String, RefKind, bool)> {
        badges(refs).into_iter().map(|Badge { name, kind, remote }| (name.to_string(), kind, remote)).collect()
    }

    #[test]
    fn a_branch_takes_in_its_remote_on_the_same_commit() {
        let refs = [r("origin/main", RefKind::Remote), r("main", RefKind::Head)];
        assert_eq!(shown(&refs), [("main".to_string(), RefKind::Head, true)]);
    }

    #[test]
    fn the_checked_out_branch_comes_first_and_lone_remotes_last() {
        let refs = [
            r("upstream/feature", RefKind::Remote),
            r("v1.0", RefKind::Tag),
            r("staging", RefKind::Branch),
            r("origin/staging", RefKind::Remote),
            r("main", RefKind::Head),
        ];
        assert_eq!(
            shown(&refs),
            [
                ("main".to_string(), RefKind::Head, false),
                ("staging".to_string(), RefKind::Branch, true),
                ("v1.0".to_string(), RefKind::Tag, false),
                ("upstream/feature".to_string(), RefKind::Remote, false),
            ]
        );
    }

    #[test]
    fn a_remote_branch_of_another_name_stays_apart() {
        let refs = [r("main", RefKind::Branch), r("origin/mainline", RefKind::Remote)];
        assert_eq!(shown(&refs).len(), 2);
    }
}
