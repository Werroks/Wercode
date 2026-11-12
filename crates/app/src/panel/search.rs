//! The search panel: finds text in the files of the project.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wercode_core::file_icons;
use wercode_core::search::{self, FileMatches, Query, Summary};

/// Typing starts a search this long after the last key.
const TYPING_PAUSE: Duration = Duration::from_millis(250);
const ROW_HEIGHT: f32 = 22.0;

pub enum SearchEvent {
    /// A result was clicked: the file, and the 1-based line and column of the match.
    Open { path: PathBuf, line: u32, column: u32 },
}

/// One line of the result list.
#[derive(Clone, Copy)]
enum Row {
    File(usize),
    Line(usize, usize),
}

/// Which of the three search options a button switches.
#[derive(Clone, Copy)]
enum Toggle {
    Case,
    Word,
    Regex,
}

pub struct SearchPanel {
    root: PathBuf,
    input: Entity<InputState>,
    options: Query,
    files: Vec<FileMatches>,
    /// Files whose lines are hidden.
    collapsed: HashSet<usize>,
    rows: Vec<Row>,
    /// What the last finished search found; `None` while one is running or before the first.
    summary: Option<Summary>,
    error: Option<String>,
    running: bool,
    /// Tells the search in the background to stop.
    cancel: Arc<AtomicBool>,
    task: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<SearchEvent> for SearchPanel {}

impl Drop for SearchPanel {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl SearchPanel {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search in files"));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => this.start(TYPING_PAUSE, cx),
            InputEvent::PressEnter { .. } => this.start(Duration::ZERO, cx),
            _ => {}
        });
        Self {
            root,
            input,
            options: Query::default(),
            files: Vec::new(),
            collapsed: HashSet::new(),
            rows: Vec::new(),
            summary: None,
            error: None,
            running: false,
            cancel: Arc::new(AtomicBool::new(false)),
            task: None,
            _subscription: subscription,
        }
    }

    /// Puts the keyboard in the search field and selects what is in it.
    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.input.clone().update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    }

    fn clear(&mut self) {
        self.files.clear();
        self.collapsed.clear();
        self.rows.clear();
        self.summary = None;
        self.error = None;
    }

    /// Searches for what is in the field, after `delay`. A search already
    /// running is stopped: its results are for an older query.
    fn start(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        let query = Query { text: self.input.read(cx).value().to_string(), ..self.options.clone() };
        if query.text.is_empty() {
            self.task = None;
            self.running = false;
            self.clear();
            cx.notify();
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let root = self.root.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            if this.update(cx, |this, cx| {
                this.clear();
                this.running = true;
                cx.notify();
            })
            .is_err()
            {
                return;
            }
            let (sender, mut found) = mpsc::unbounded::<FileMatches>();
            let search = cx.background_executor().spawn(async move {
                search::search(&root, &query, &cancel, |file| {
                    let _ = sender.unbounded_send(file);
                })
            });
            // Results are shown as they come; the channel closes when the search ends.
            while let Some(file) = found.next().await {
                let mut batch = vec![file];
                while let Ok(more) = found.try_recv() {
                    batch.push(more);
                }
                if this.update(cx, |this, cx| this.add(batch, cx)).is_err() {
                    return;
                }
            }
            let result = search.await;
            let _ = this.update(cx, |this, cx| {
                this.running = false;
                match result {
                    Ok(summary) => this.summary = Some(summary),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        }));
    }

    fn add(&mut self, batch: Vec<FileMatches>, cx: &mut Context<Self>) {
        self.files.extend(batch);
        self.rebuild_rows();
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (index, file) in self.files.iter().enumerate() {
            self.rows.push(Row::File(index));
            if !self.collapsed.contains(&index) {
                self.rows.extend((0..file.lines.len()).map(|line| Row::Line(index, line)));
            }
        }
    }

    fn toggle(&mut self, which: Toggle, cx: &mut Context<Self>) {
        let option = match which {
            Toggle::Case => &mut self.options.case_sensitive,
            Toggle::Word => &mut self.options.whole_word,
            Toggle::Regex => &mut self.options.regex,
        };
        *option = !*option;
        self.start(Duration::ZERO, cx);
        cx.notify();
    }

    fn click_row(&mut self, index: usize, cx: &mut Context<Self>) {
        match self.rows.get(index).copied() {
            Some(Row::File(file)) => {
                if !self.collapsed.remove(&file) {
                    self.collapsed.insert(file);
                }
                self.rebuild_rows();
                cx.notify();
            }
            Some(Row::Line(file, line)) => {
                let found = &self.files[file];
                let line = &found.lines[line];
                cx.emit(SearchEvent::Open { path: found.path.clone(), line: line.number, column: line.column });
            }
            None => {}
        }
    }

    /// The line under the field: how the search went.
    fn status(&self) -> Option<String> {
        if let Some(error) = &self.error {
            return Some(format!("Not a valid pattern: {error}"));
        }
        let lines: usize = self.files.iter().map(|f| f.lines.len()).sum();
        let count = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        let found = format!("{} in {}", count(lines, "result"), count(self.files.len(), "file"));
        match self.summary {
            None if self.running && lines == 0 => Some("Searching\u{2026}".into()),
            None if self.running => Some(format!("{found}, still searching\u{2026}")),
            None => None,
            Some(_) if lines == 0 => Some("No results.".into()),
            Some(summary) if summary.truncated => Some(format!("{found}. There are more: only the first are shown.")),
            Some(_) => Some(found),
        }
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, hover, foreground) = (theme.muted_foreground, theme.list_hover, theme.foreground);
        let row = h_flex()
            .id(("search-row", index))
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
                let found = &self.files[file];
                let name = found.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let folder = found
                    .path
                    .parent()
                    .and_then(|p| p.strip_prefix(&self.root).ok())
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                let chevron =
                    if self.collapsed.contains(&file) { IconName::ChevronRight } else { IconName::ChevronDown };
                let picture = file_icons::for_file(&name).and_then(crate::assets::file_icon);
                row.pl_2()
                    .child(Icon::new(chevron).xsmall().text_color(muted))
                    .child(match picture {
                        Some(path) => img(path).flex_none().size(px(16.)).into_any_element(),
                        None => Icon::new(IconName::File).small().text_color(muted).into_any_element(),
                    })
                    .child(div().flex_none().max_w(relative(0.6)).truncate().child(name))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(folder))
                    .child(div().flex_none().text_xs().text_color(muted).child(found.lines.len().to_string()))
                    .into_any_element()
            }
            Row::Line(file, line) => {
                let line = &self.files[file].lines[line];
                let mark = HighlightStyle {
                    color: Some(foreground),
                    background_color: Some(theme.warning.opacity(0.35)),
                    ..Default::default()
                };
                let highlights: Vec<_> = line.ranges.iter().map(|&(from, to)| (from..to, mark)).collect();
                row.pl_6()
                    .child(div().flex_none().w(px(34.)).text_xs().text_color(muted).child(line.number.to_string()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_color(muted)
                            .child(StyledText::new(line.text.clone()).with_highlights(highlights)),
                    )
                    .into_any_element()
            }
        }
    }
}

impl Render for SearchPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let option = |id: &'static str, label: &'static str, tooltip: &'static str, on: bool, which: Toggle| {
            Button::new(id)
                .ghost()
                .xsmall()
                .label(label)
                .tooltip(tooltip)
                .selected(on)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle(which, cx)))
        };
        let options = h_flex()
            .gap_1()
            .child(option("search-case", "Aa", "Match case", self.options.case_sensitive, Toggle::Case))
            .child(option("search-word", "ab", "Match whole word", self.options.whole_word, Toggle::Word))
            .child(option("search-regex", ".*", "Use a regular expression", self.options.regex, Toggle::Regex));
        let status = self.status();
        let failed = self.error.is_some();
        let count = self.rows.len();

        v_flex()
            .id("search-panel")
            .size_full()
            .child(
                v_flex()
                    .flex_none()
                    .p_2()
                    .gap_1p5()
                    .child(Input::new(&self.input).small().cleanable(true))
                    .child(options)
                    .when_some(status, |d, status| {
                        d.child(
                            div()
                                .id("search-status")
                                .text_xs()
                                .text_color(if failed { danger } else { muted })
                                .child(status),
                        )
                    }),
            )
            .child(
                uniform_list(
                    "search-results",
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
impl SearchPanel {
    pub fn write_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_value(text.to_string(), window, cx));
        self.start(Duration::ZERO, cx);
    }

    /// Files found and matching lines in them, once the search has finished.
    pub fn found(&self) -> Option<(usize, usize)> {
        self.summary.map(|_| (self.files.len(), self.files.iter().map(|f| f.lines.len()).sum()))
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn status_text(&self) -> Option<String> {
        self.status()
    }
}
