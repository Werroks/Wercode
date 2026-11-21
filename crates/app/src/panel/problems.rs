//! The problems panel: the errors and warnings the language servers report
//! for the files of the project, by file.

use std::collections::HashSet;
use std::path::PathBuf;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wercode_core::file_icons;
use wercode_core::i18n::{t, tn};
use wercode_core::lsp::{Problem, Severity};

use crate::lsp::Checker;

const ROW_HEIGHT: f32 = 22.0;

pub enum ProblemsEvent {
    /// A problem was clicked: the file, and the 1-based line and column.
    Open { path: PathBuf, line: u32, column: u32 },
}

/// One line of the list.
#[derive(Clone, Copy)]
enum Row {
    File(usize),
    Problem(usize, usize),
}

pub struct ProblemsPanel {
    root: PathBuf,
    files: Vec<(PathBuf, Vec<Problem>)>,
    /// Files whose problems are hidden.
    collapsed: HashSet<PathBuf>,
    rows: Vec<Row>,
    /// The servers said something new; the list is read again when drawn.
    stale: bool,
    checker: Option<Entity<Checker>>,
    _observe: Option<Subscription>,
}

impl EventEmitter<ProblemsEvent> for ProblemsPanel {}

/// The icon and colour of a severity.
pub fn severity_icon(severity: Severity, cx: &App) -> Icon {
    let theme = cx.theme();
    match severity {
        Severity::Error => Icon::new(Lucide::CircleX).text_color(theme.danger),
        Severity::Warning => Icon::new(IconName::TriangleAlert).text_color(theme.warning),
        Severity::Info | Severity::Hint => Icon::new(Lucide::Info).text_color(theme.link),
    }
}

impl ProblemsPanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let checker = crate::lsp::checker(cx);
        let observe = checker.as_ref().map(|checker| {
            cx.observe(checker, |this, _, cx| {
                this.stale = true;
                cx.notify();
            })
        });
        Self {
            root,
            files: Vec::new(),
            collapsed: HashSet::new(),
            rows: Vec::new(),
            stale: true,
            checker,
            _observe: observe,
        }
    }

    fn refresh(&mut self, cx: &App) {
        self.stale = false;
        self.files =
            self.checker.as_ref().map(|checker| checker.read(cx).servers().all_problems(&self.root)).unwrap_or_default();
        // Files with errors before files with only warnings, each group by path.
        self.files.sort_by_key(|(path, problems)| {
            (problems.iter().map(|p| p.severity).min().unwrap_or(Severity::Hint), path.clone())
        });
        self.collapsed.retain(|path| self.files.iter().any(|(p, _)| p == path));
        self.rebuild_rows();
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (index, (path, problems)) in self.files.iter().enumerate() {
            self.rows.push(Row::File(index));
            if !self.collapsed.contains(path) {
                self.rows.extend((0..problems.len()).map(|problem| Row::Problem(index, problem)));
            }
        }
    }

    fn click_row(&mut self, index: usize, cx: &mut Context<Self>) {
        match self.rows.get(index).copied() {
            Some(Row::File(file)) => {
                let path = self.files[file].0.clone();
                if !self.collapsed.remove(&path) {
                    self.collapsed.insert(path);
                }
                self.rebuild_rows();
                cx.notify();
            }
            Some(Row::Problem(file, problem)) => {
                let (path, problems) = &self.files[file];
                let problem = &problems[problem];
                cx.emit(ProblemsEvent::Open { path: path.clone(), line: problem.line + 1, column: problem.column + 1 });
            }
            None => {}
        }
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.list_hover);
        let row = h_flex()
            .id(("problems-row", index))
            .test_support()
            .h(px(ROW_HEIGHT))
            .w_full()
            .pr_2()
            .gap_1p5()
            .text_sm()
            .hover(|s| s.bg(hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.click_row(index, cx)));
        match self.rows[index] {
            Row::File(file) => {
                let (path, problems) = &self.files[file];
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let folder = path
                    .parent()
                    .and_then(|p| p.strip_prefix(&self.root).ok())
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                let chevron =
                    if self.collapsed.contains(path) { IconName::ChevronRight } else { IconName::ChevronDown };
                let picture = file_icons::for_file(&name).and_then(crate::assets::file_icon);
                row.pl_2()
                    .child(Icon::new(chevron).xsmall().text_color(muted))
                    .child(match picture {
                        Some(path) => img(path).flex_none().size(px(16.)).into_any_element(),
                        None => Icon::new(IconName::File).small().text_color(muted).into_any_element(),
                    })
                    .child(div().flex_none().max_w(relative(0.6)).truncate().child(name))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(folder))
                    .child(div().flex_none().text_xs().text_color(muted).child(problems.len().to_string()))
                    .into_any_element()
            }
            Row::Problem(file, problem) => {
                let problem = &self.files[file].1[problem];
                // Long messages go on over several lines; the first says what it is.
                let message = problem.message.lines().next().unwrap_or_default().to_string();
                let origin = match (&problem.source, &problem.code) {
                    (Some(source), Some(code)) => format!("{source}({code})"),
                    (Some(source), None) => source.clone(),
                    (None, Some(code)) => code.clone(),
                    (None, None) => String::new(),
                };
                let place = format!("{}:{}", problem.line + 1, problem.column + 1);
                let tip = ProblemTip {
                    message: problem.message.trim().to_string().into(),
                    origin: format!("{origin}  {place}").trim().to_string().into(),
                };
                // The message comes first: when the row is short of room the source
                // gives way, down to nothing, before the message is cut (it took up
                // to 40% before). The place stays whole; the tooltip has everything.
                row.pl_6()
                    .tooltip(move |_, cx| cx.new(|_| tip.clone()).into())
                    .child(severity_icon(problem.severity, cx).xsmall().flex_none())
                    .child(div().flex_auto().min_w(px(60.)).truncate().child(message))
                    .when(!origin.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_grow_0()
                                .flex_shrink(1000.)
                                .min_w_0()
                                .max_w(px(180.))
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(origin),
                        )
                    })
                    .child(div().flex_none().text_xs().text_color(muted).child(place))
                    .into_any_element()
            }
        }
    }
}

/// Characters a line of the tooltip holds before it wraps.
const TIP_LINE: usize = 60;

/// The whole of a problem, shown when the pointer rests on its row.
#[derive(Clone)]
struct ProblemTip {
    message: SharedString,
    origin: SharedString,
}

impl Render for ProblemTip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        // A tooltip is measured before it has a width, so text that wraps
        // inside one ran over the lines below: long messages get a width of
        // their own, short ones stay on their lines.
        let wide = self.message.lines().any(|line| line.chars().count() > TIP_LINE);
        v_flex()
            .when(wide, |d| d.w(px(480.)))
            .gap_1()
            .px_2()
            .py_1p5()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_md()
            // One element a line: a line break inside one text is not counted in its height either.
            .children(self.message.lines().map(|line| {
                div().text_sm().when(wide, |d| d.w_full()).child(SharedString::from(line.to_string()))
            }))
            .when(!self.origin.is_empty(), |d| {
                d.child(div().text_xs().text_color(theme.muted_foreground).child(self.origin.clone()))
            })
    }
}

impl Render for ProblemsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.stale {
            self.refresh(cx);
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let (errors, warnings) =
            self.checker.as_ref().map(|checker| checker.read(cx).servers().counts(&self.root)).unwrap_or_default();
        let checking = self.checker.as_ref().is_some_and(|checker| checker.read(cx).servers().is_checking(&self.root));
        let enabled = self.checker.as_ref().is_some_and(|checker| checker.read(cx).servers().enabled());
        let count = |severity: Severity, text: String| {
            h_flex().gap_1().items_center().child(severity_icon(severity, cx).xsmall()).child(text)
        };
        let summary = if !self.files.is_empty() {
            h_flex()
                .gap_3()
                .child(count(Severity::Error, tn(errors as u64, "{n} error", "{n} errors", &[])))
                .child(count(Severity::Warning, tn(warnings as u64, "{n} warning", "{n} warnings", &[])))
                .into_any_element()
        } else if !enabled {
            div().child(t("Checking code is turned off in the settings.")).into_any_element()
        } else if checking {
            div().child(t("No problems found.")).into_any_element()
        } else {
            div()
                .child(t("Errors and warnings appear here once a language server checks a file you open."))
                .into_any_element()
        };
        let count = self.rows.len();

        v_flex()
            .id("problems-panel")
            .test_support()
            .size_full()
            .child(div().id("problems-summary").test_support().flex_none().p_2().text_xs().text_color(muted).child(summary))
            .child(
                uniform_list(
                    "problems-list",
                    count,
                    cx.processor(|this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                        range.map(|index| this.render_row(index, cx)).collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .min_h_0(),
            )
    }
}

#[cfg(test)]
impl ProblemsPanel {
    /// Files and problems listed, once drawn.
    pub fn listed(&self) -> (usize, usize) {
        (self.files.len(), self.files.iter().map(|(_, problems)| problems.len()).sum())
    }
}
