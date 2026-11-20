//! Going to a named thing in code (a function, a type, a field): one in the
//! file that is showing, or one anywhere in the project, picked from a list
//! as its name is typed. The language servers say what there is.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::channel::oneshot;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex};
use gpui_kit::*;
use wercode_core::i18n::t;
use wercode_core::lsp::{Servers, Symbol};

actions!(
    symbols,
    [
        /// Lists the named things of the file that is showing.
        GoToSymbol,
        /// Finds a named thing anywhere in the project by its name.
        GoToProjectSymbol
    ]
);

/// A search across the project waits this long after the last key.
const TYPING_PAUSE: Duration = Duration::from_millis(150);
/// At most this many things are listed.
const MAX_ROWS: usize = 200;

pub enum SymbolsEvent {
    /// Go to this place: a 1-based line and column.
    Open { path: PathBuf, line: u32, column: u32 },
    Close,
}

/// One line of the list.
#[derive(Clone, Debug)]
struct Row {
    name: String,
    /// The signature, or what the thing is inside of.
    detail: Option<String>,
    kind: u32,
    path: PathBuf,
    /// Where its name is: 0-based line and column.
    at: (u32, u32),
    /// How deep it sits inside other things of the file.
    depth: usize,
}

pub struct SymbolPicker {
    state: Entity<CommandState>,
    servers: Servers,
    root: PathBuf,
    /// The file whose things are listed; `None` searches the project.
    file: Option<PathBuf>,
    rows: Vec<Row>,
    /// The rows that fit what is typed, as indexes into `rows`.
    shown: Vec<usize>,
    loading: bool,
    task: Option<Task<()>>,
}

impl EventEmitter<SymbolsEvent> for SymbolPicker {}

impl Focusable for SymbolPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.focus_handle(cx)
    }
}

/// The things of a file in order, each followed by what is inside it.
fn flatten(symbols: &[Symbol], depth: usize, out: &mut Vec<Row>) {
    let mut sorted: Vec<&Symbol> = symbols.iter().collect();
    sorted.sort_by_key(|s| s.start);
    for symbol in sorted {
        out.push(Row {
            name: symbol.name.clone(),
            detail: symbol.detail.clone().or_else(|| symbol.container.clone()),
            kind: symbol.kind,
            path: symbol.path.clone(),
            at: symbol.name_at,
            depth,
        });
        flatten(&symbol.children, depth + 1, out);
    }
}

/// Whether `name` has the letters of `query` in their order, ignoring case.
fn fits(name: &str, query: &str) -> bool {
    let name = name.to_lowercase();
    let mut letters = name.chars();
    query.to_lowercase().chars().filter(|c| !c.is_whitespace()).all(|wanted| letters.any(|c| c == wanted))
}

/// The picture for a kind of thing, by the protocol's `SymbolKind`.
fn icon(kind: u32) -> Lucide {
    match kind {
        6 | 9 | 12 | 24 => Lucide::SquareFunction,
        5 | 10 | 11 | 23 | 26 => Lucide::Box,
        2..=4 => Lucide::Package,
        7 | 8 | 13 | 14 | 22 => Lucide::Variable,
        1 => Lucide::FileCode,
        _ => Lucide::Hash,
    }
}

impl SymbolPicker {
    pub fn new(servers: Servers, root: PathBuf, file: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        state.update(cx, |state, cx| state.focus(window, cx));
        let mut this =
            Self { state, servers, root, file: file.clone(), rows: Vec::new(), shown: Vec::new(), loading: true, task: None };
        match file {
            Some(file) => this.list_file(file, cx),
            None => this.search(String::new(), cx),
        }
        this
    }

    fn list_file(&mut self, file: PathBuf, cx: &mut Context<Self>) {
        let (sender, answer) = oneshot::channel();
        self.servers.symbols(&file, move |found| {
            let _ = sender.send(found);
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let found = answer.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                let mut rows = Vec::new();
                flatten(&found, 0, &mut rows);
                this.rows = rows;
                this.loading = false;
                let query = this.state.read(cx).query(cx).to_string();
                this.filter(&query);
                cx.notify();
            });
        }));
    }

    /// Asks the servers of the project for things whose names fit `query`.
    fn search(&mut self, query: String, cx: &mut Context<Self>) {
        let (servers, root) = (self.servers.clone(), self.root.clone());
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TYPING_PAUSE).await;
            let (sender, answer) = oneshot::channel();
            servers.project_symbols(&root, &query, move |found| {
                let _ = sender.send(found);
            });
            let found = answer.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                let mut rows = Vec::new();
                flatten(&found, 0, &mut rows);
                for row in &mut rows {
                    row.depth = 0;
                }
                // Names that start with what was typed first.
                let lower = query.to_lowercase();
                rows.sort_by_cached_key(|row| (!row.name.to_lowercase().starts_with(&lower), row.name.len(), row.name.clone()));
                rows.truncate(MAX_ROWS);
                this.shown = (0..rows.len()).collect();
                this.rows = rows;
                this.loading = false;
                cx.notify();
            });
        }));
    }

    fn filter(&mut self, query: &str) {
        self.shown = self.rows.iter().enumerate().filter(|(_, row)| fits(&row.name, query)).map(|(i, _)| i).take(MAX_ROWS).collect();
    }

    fn query(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.file.is_some() {
            self.filter(query);
            cx.notify();
        } else {
            self.search(query.to_string(), cx);
        }
    }

    fn confirm(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(row) = self.shown.get(row).and_then(|&i| self.rows.get(i)) else { return cx.emit(SymbolsEvent::Close) };
        cx.emit(SymbolsEvent::Open { path: row.path.clone(), line: row.at.0 + 1, column: row.at.1 + 1 });
    }

    /// Where a file is, written short: relative to the project.
    fn short(&self, path: &Path) -> String {
        path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().replace('\\', "/")
    }
}

impl Render for SymbolPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, popover, foreground, muted) =
            (theme.border, theme.popover, theme.popover_foreground, theme.muted_foreground);
        let project = self.file.is_none();
        let items: Vec<CommandItem> = self
            .shown
            .iter()
            .filter_map(|&index| self.rows.get(index))
            .map(|row| {
                let name = SharedString::from(row.name.clone());
                let note = if project { Some(self.short(&row.path)) } else { row.detail.clone() };
                let (depth, kind) = (row.depth, row.kind);
                CommandItem::new().label(row.name.clone()).child(move |_, _| {
                    h_flex()
                        .w_full()
                        .gap_2()
                        .pl(px(depth as f32 * 14.))
                        .child(Icon::new(icon(kind)).xsmall().text_color(muted))
                        .child(div().flex_none().max_w(relative(0.6)).truncate().text_sm().child(name.clone()))
                        .children(note.clone().map(|note| {
                            div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(note)
                        }))
                })
            })
            .collect();
        let this = cx.entity().downgrade();
        let (on_query, on_confirm) = (this.clone(), this.clone());
        let (placeholder, empty) = if project {
            (t("Type the name of a function, type or variable\u{2026}"), t("Nothing in the project has that name."))
        } else {
            (t("Go to a function, type or variable in this file\u{2026}"), t("The language server lists nothing in this file."))
        };
        let loading = self.loading;
        div()
            .id("symbols")
            .mt(px(72.))
            .w(px(620.))
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
                Command::new(&self.state)
                    .items(items)
                    .filterable(false)
                    .placeholder(placeholder)
                    .bordered(false)
                    .empty(move |_, _, _| {
                        div().px_3().py_2().text_sm().child(if loading { t("Asking the language server\u{2026}") } else { empty })
                    })
                    .on_query(move |query, _, cx| {
                        let _ = on_query.update(cx, |this, cx| this.query(query, cx));
                    })
                    .on_confirm(move |index, _, cx| {
                        let _ = on_confirm.update(cx, |this, cx| this.confirm(index.row, cx));
                    })
                    .on_cancel(move |_, cx| {
                        let _ = this.update(cx, |_, cx| cx.emit(SymbolsEvent::Close));
                    }),
            )
    }
}

#[cfg(test)]
impl SymbolPicker {
    /// The names listed now, top first.
    pub fn listed(&self) -> Vec<String> {
        self.shown.iter().filter_map(|&i| self.rows.get(i)).map(|row| row.name.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Row, fits, flatten};
    use std::path::PathBuf;
    use wercode_core::lsp::Symbol;

    fn symbol(name: &str, line: u32, children: Vec<Symbol>) -> Symbol {
        Symbol {
            name: name.into(),
            detail: None,
            kind: 12,
            path: PathBuf::from("a.rs"),
            start: (line, 0),
            end: (line + 1, 0),
            name_at: (line, 3),
            container: None,
            children,
        }
    }

    #[test]
    fn things_are_listed_in_order_with_what_is_inside_them() {
        let symbols = vec![symbol("later", 9, vec![]), symbol("Outer", 1, vec![symbol("inner", 2, vec![])])];
        let mut rows: Vec<Row> = Vec::new();
        flatten(&symbols, 0, &mut rows);
        let listed: Vec<(&str, usize)> = rows.iter().map(|r| (r.name.as_str(), r.depth)).collect();
        assert_eq!(listed, [("Outer", 0), ("inner", 1), ("later", 0)]);
        assert!(fits("handle_click", "hcl") && fits("Point", "po") && !fits("main", "x"));
    }
}
