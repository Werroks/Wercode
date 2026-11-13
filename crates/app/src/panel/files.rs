//! The file tree in the side panel, with git marks next to changed files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::tree::{TreeEvent, TreeItem, TreeState, tree};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wercode_core::{file_icons, fs_ops, fs_tree};
use wercode_core::git::{FileStatus, RepoStatus};

/// Upper bound on entries shown at once. Only open folders are read, so this
/// is reached by opening very many of them, or one with thousands of files.
const MAX_ENTRIES: usize = 5_000;

/// What the name field is for. The path that goes with it is the folder to
/// create in, or the entry being renamed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Naming {
    File,
    Folder,
    Rename,
}

pub enum FilesEvent {
    /// A file was clicked.
    Open(PathBuf),
    /// Type this path into the terminal.
    Insert(PathBuf),
    /// Open the file in the program named by `$EDITOR`, in a terminal tab.
    OpenInTerminal(PathBuf),
    /// A file or folder got a new name; files open from it should follow.
    Renamed { from: PathBuf, to: PathBuf },
}

/// A file or folder of the tree on its way to a terminal under the pointer.
#[derive(Clone)]
pub struct DraggedPath(pub PathBuf);

impl Render for DraggedPath {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.0.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        div()
            .px_2()
            .py_0p5()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .text_sm()
            .child(name)
    }
}

/// The program the user's shell tools open files with, if one is set.
fn terminal_editor() -> Option<String> {
    ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

fn reveal_label() -> &'static str {
    if cfg!(windows) {
        "Reveal in File Explorer"
    } else if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Show in File Manager"
    }
}

pub struct FilesPanel {
    root: PathBuf,
    tree: Entity<TreeState>,
    truncated: bool,
    /// Ids of the folders the user has opened; kept across re-reads of the disk.
    expanded: HashSet<SharedString>,
    /// Ids of everything that is a folder, including empty ones.
    folders: Arc<HashSet<SharedString>>,
    git: Option<Arc<RepoStatus>>,
    /// The name field for a new file or folder, while it is showing.
    creating: Option<(Naming, PathBuf)>,
    name_input: Entity<InputState>,
    error: Option<String>,
    /// The entry to select once the folder has been read again.
    select_next: Option<SharedString>,
    /// The folder being read in the background; a newer read replaces it.
    reading: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// Reads the project folder and the folders that are open in the tree.
fn read(root: &Path, expanded: &HashSet<SharedString>) -> (Vec<fs_tree::Node>, bool) {
    let open: HashSet<PathBuf> = expanded.iter().map(|id| PathBuf::from(id.as_ref())).collect();
    let mut budget = MAX_ENTRIES;
    let nodes = fs_tree::read_tree(root, &open, &mut budget);
    (nodes, budget == 0)
}

/// Turns what was read into rows. A closed folder with something inside gets
/// one unseen child: the tree only opens a folder that has children.
fn items(
    nodes: Vec<fs_tree::Node>,
    expanded: &mut HashSet<SharedString>,
    folders: &mut HashSet<SharedString>,
) -> Vec<TreeItem> {
    nodes
        .into_iter()
        .map(|node| {
            let id: SharedString = node.entry.path.to_string_lossy().into_owned().into();
            let mut item = TreeItem::new(id.clone(), node.entry.name);
            if node.entry.is_dir {
                folders.insert(id.clone());
                if node.has_more {
                    item = item.child(TreeItem::new(format!("{id}\u{1}"), ""));
                } else if node.children.is_empty() {
                    // Everything inside is gone (or ignored): nothing to keep open.
                    expanded.remove(&id);
                } else {
                    item = item.children(items(node.children, expanded, folders)).expanded(true);
                }
            }
            item
        })
        .collect()
}

/// The colour a status paints the file name and its mark with.
pub(crate) fn status_color(status: FileStatus, cx: &App) -> Hsla {
    let theme = cx.theme();
    match status {
        FileStatus::Untracked | FileStatus::Added | FileStatus::Renamed => theme.success,
        FileStatus::Modified => theme.warning,
        FileStatus::Deleted | FileStatus::Conflict => theme.danger,
    }
}

impl EventEmitter<FilesEvent> for FilesPanel {}

impl FilesPanel {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut expanded = HashSet::new();
        let mut folders = HashSet::new();
        // Only the top level is read here, so the window does not wait for it.
        let (nodes, truncated) = read(&root, &expanded);
        let items = items(nodes, &mut expanded, &mut folders);
        let tree = cx.new(|cx| TreeState::new(cx).items(items));
        let name_input = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![
            cx.subscribe(&tree, |this, _, event: &TreeEvent, cx| match event {
                // What is inside a folder is read when it is opened.
                TreeEvent::Expanded(id) => {
                    if this.expanded.insert(id.clone()) {
                        this.refresh(cx);
                    }
                }
                TreeEvent::Collapsed(id) => {
                    this.expanded.remove(id);
                }
            }),
            cx.subscribe_in(&name_input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.finish_create(window, cx),
                InputEvent::Blur => this.cancel_create(cx),
                _ => {}
            }),
        ];
        Self {
            root,
            tree,
            truncated,
            expanded,
            folders: Arc::new(folders),
            git: None,
            creating: None,
            name_input,
            error: None,
            select_next: None,
            reading: None,
            _subscriptions: subscriptions,
        }
    }

    /// Reads the folder from disk again, keeping open folders open. The disk
    /// is read in the background; the tree changes when that is done.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let (root, expanded) = (self.root.clone(), self.expanded.clone());
        self.reading = Some(cx.spawn(async move |this, cx| {
            let (nodes, truncated) = cx.background_executor().spawn(async move { read(&root, &expanded) }).await;
            let _ = this.update(cx, |this, cx| this.show(nodes, truncated, cx));
        }));
    }

    fn show(&mut self, nodes: Vec<fs_tree::Node>, truncated: bool, cx: &mut Context<Self>) {
        let mut folders = HashSet::new();
        let selected = self.tree.read(cx).selected_item().map(|item| item.id.clone());
        let items = items(nodes, &mut self.expanded, &mut folders);
        self.folders = Arc::new(folders);
        self.truncated = truncated;
        // A newly made entry is selected; otherwise the selection stays on its file.
        let created = self.select_next.take();
        let select = created.clone().or(selected);
        self.tree.update(cx, |tree, cx| {
            tree.set_items(items, cx);
            let index = select.and_then(|id| tree.index_of(&id));
            tree.set_selected_index(index, cx);
            if let (Some(index), true) = (index, created.is_some()) {
                tree.scroll_to_item(index, ScrollStrategy::Center);
            }
        });
        cx.notify();
    }

    pub fn set_git(&mut self, git: Option<Arc<RepoStatus>>, cx: &mut Context<Self>) {
        self.git = git;
        cx.notify();
    }

    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.expanded.clear();
        self.refresh(cx);
    }

    /// New entries go into the selected folder, or next to the selected file.
    fn target_dir(&self, cx: &App) -> PathBuf {
        let Some(entry) = self.tree.read(cx).selected_entry() else {
            return self.root.clone();
        };
        let path = PathBuf::from(entry.item().id.as_ref());
        if self.folders.contains(&entry.item().id) {
            path
        } else {
            path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone())
        }
    }

    fn start_create(&mut self, kind: Naming, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.target_dir(cx);
        self.start_naming(kind, dir, window, cx);
    }

    /// Shows the name field: empty for something new, or holding the present
    /// name of the entry at `path` when renaming.
    fn start_naming(&mut self, kind: Naming, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = match kind {
            Naming::Rename => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            Naming::File | Naming::Folder => String::new(),
        };
        self.creating = Some((kind, path));
        self.error = None;
        self.name_input.update(cx, |input, cx| {
            input.set_value(name, window, cx);
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }

    /// Asks, then moves the file or folder to the system trash.
    fn delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let title = format!("Move {name} to the trash?");
        let detail = if path.is_dir() {
            "The folder and everything in it will be moved. You can bring it back from the trash."
        } else {
            "You can bring it back from the trash."
        };
        let answer = window.prompt(PromptLevel::Warning, &title, Some(detail), &["Move to Trash", "Cancel"], cx);
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let target = path.clone();
            let result = cx.background_executor().spawn(async move { fs_ops::trash(&target) }).await;
            let _ = this.update(cx, |this, cx| {
                this.error = result.err();
                this.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn copy_path(&self, path: &Path, relative: bool, cx: &mut Context<Self>) {
        let shown = match path.strip_prefix(&self.root) {
            Ok(inside) if relative => inside.to_string_lossy().replace('\\', "/"),
            _ => path.to_string_lossy().into_owned(),
        };
        cx.write_to_clipboard(ClipboardItem::new_string(shown));
    }

    fn finish_rename(&mut self, path: PathBuf, name: &str, cx: &mut Context<Self>) {
        match fs_ops::rename(&path, name) {
            Ok(to) => {
                self.creating = None;
                self.error = None;
                // Folders open below a renamed folder stay open under its new name.
                let moved: Vec<SharedString> =
                    self.expanded.iter().filter(|id| Path::new(id.as_ref()).starts_with(&path)).cloned().collect();
                for id in moved {
                    self.expanded.remove(&id);
                    if let Ok(rest) = Path::new(id.as_ref()).strip_prefix(&path) {
                        self.expanded.insert(to.join(rest).to_string_lossy().into_owned().into());
                    }
                }
                self.select_next = Some(to.to_string_lossy().into_owned().into());
                self.refresh(cx);
                if to != path {
                    cx.emit(FilesEvent::Renamed { from: path, to });
                }
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn cancel_create(&mut self, cx: &mut Context<Self>) {
        if self.creating.take().is_some() {
            cx.notify();
        }
    }

    fn finish_create(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some((kind, dir)) = self.creating.clone() else { return };
        let name = self.name_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.cancel_create(cx);
            return;
        }
        if kind == Naming::Rename {
            self.finish_rename(dir, &name, cx);
            return;
        }
        let path = dir.join(&name);
        // `create_new` refuses to touch a file that is already there.
        let result = match kind {
            Naming::Folder | Naming::Rename => std::fs::create_dir_all(&path),
            Naming::File => path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|_| std::fs::File::create_new(&path).map(|_| ())),
        };
        match result {
            Ok(()) => {
                self.creating = None;
                self.error = None;
                // Open every folder on the way down, so the new entry is visible.
                for folder in path.ancestors().skip(1).take_while(|p| *p != self.root) {
                    self.expanded.insert(folder.to_string_lossy().into_owned().into());
                }
                self.select_next = Some(path.to_string_lossy().into_owned().into());
                self.refresh(cx);
            }
            Err(e) => self.error = Some(format!("Could not create {name}: {e}")),
        }
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.to_string_lossy().into_owned());
        h_flex()
            .h(px(32.))
            .pl_3()
            .pr_1()
            .flex_none()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child(name.to_uppercase()),
            )
            .child(
                Button::new("new-file")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::FilePlus))
                    .tooltip("New file")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_create(Naming::File, window, cx)
                    })),
            )
            .child(
                Button::new("new-folder")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::FolderPlus))
                    .tooltip("New folder")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_create(Naming::Folder, window, cx)
                    })),
            )
            .child(
                Button::new("refresh-files")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::RefreshCw))
                    .tooltip("Refresh")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.refresh(cx))),
            )
            .child(
                Button::new("collapse-files")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::ChevronsDownUp))
                    .tooltip("Collapse all folders")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.collapse_all(cx))),
            )
    }

    fn render_name_field(&self, kind: Naming, dir: &Path, cx: &mut Context<Self>) -> impl IntoElement {
        let place = dir.strip_prefix(&self.root).ok().filter(|p| !p.as_os_str().is_empty());
        let label = match (kind, place) {
            (Naming::File, None) => "New file".to_string(),
            (Naming::Folder, None) => "New folder".to_string(),
            (Naming::File, Some(p)) => format!("New file in {}", p.display()),
            (Naming::Folder, Some(p)) => format!("New folder in {}", p.display()),
            (Naming::Rename, Some(p)) => format!("Rename {}", p.display()),
            (Naming::Rename, None) => "Rename".to_string(),
        };
        v_flex()
            .px_2()
            .pb_2()
            .gap_1()
            .flex_none()
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(label))
            .child(Input::new(&self.name_input).small())
    }
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let git = self.git.clone();
        let folders = self.folders.clone();
        let this = cx.entity().downgrade();
        let (menu_owner, menu_folders) = (this.clone(), folders.clone());
        let editor = terminal_editor();

        v_flex()
            .size_full()
            .child(self.render_header(cx))
            .when_some(self.creating.clone(), |this, (kind, dir)| {
                this.child(self.render_name_field(kind, &dir, cx))
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(div().px_3().pb_2().text_xs().text_color(cx.theme().danger).child(error))
            })
            .child(
                div().flex_1().min_h_0().child(
                    tree(&self.tree, move |ix, entry, _selected, _window, cx| {
                        // An empty folder has no children, so the tree alone cannot tell.
                        let folder = folders.contains(&entry.item().id);
                        let name = entry.item().label.as_ref();
                        let picture = if folder {
                            crate::assets::file_icon(file_icons::for_folder(name))
                        } else {
                            file_icons::for_file(name).and_then(crate::assets::file_icon)
                        };
                        let path = Path::new(entry.item().id.as_ref());
                        let status = git.as_ref().and_then(|g| if folder { g.folder(path) } else { g.file(path) });
                        let color = status.map(|s| status_color(s, cx));
                        let chevron = if entry.is_expanded() { IconName::ChevronDown } else { IconName::ChevronRight };
                        let mark = match status {
                            // A folder only says that something inside changed.
                            Some(_) if folder => div()
                                .size(px(7.))
                                .rounded_full()
                                .bg(color.unwrap_or(muted))
                                .opacity(0.8)
                                .into_any_element(),
                            Some(s) => div().text_xs().child(s.letter()).into_any_element(),
                            None => div().into_any_element(),
                        };
                        ListItem::new(ix).py_0p5().pl(px(4.) + px(12.) * entry.depth() as f32).pr_3().child(
                            h_flex()
                                .id(("file-row", ix))
                                .test_support()
                                .w_full()
                                .gap_1p5()
                                .text_sm()
                                .when_some(color, |d, c| d.text_color(c))
                                // Dropped on a terminal, the path is typed into it.
                                .on_drag(DraggedPath(path.to_path_buf()), |dragged, _, _, cx| {
                                    cx.new(|_| dragged.clone())
                                })
                                // The tree itself selects the row and opens folders.
                                .when(!folder, |d| {
                                    let (this, path) = (this.clone(), path.to_path_buf());
                                    d.on_click(move |event, _, cx| {
                                        // With Alt held the path goes to the terminal instead.
                                        let event = if event.modifiers().alt {
                                            FilesEvent::Insert(path.clone())
                                        } else {
                                            FilesEvent::Open(path.clone())
                                        };
                                        let _ = this.update(cx, |_, cx| cx.emit(event));
                                    })
                                })
                                // A file keeps the width of the chevron, so names line up.
                                .child(div().flex_none().w(px(14.)).when(folder, |d| {
                                    d.child(Icon::new(chevron).xsmall().text_color(muted))
                                }))
                                .child(match picture {
                                    Some(path) => img(path).flex_none().size(px(16.)).into_any_element(),
                                    None => Icon::new(IconName::File).small().text_color(muted).into_any_element(),
                                })
                                .child(div().flex_1().min_w_0().truncate().child(entry.item().label.clone()))
                                .child(div().flex_none().w(px(12.)).flex().justify_center().child(mark)),
                        )
                    })
                    .context_menu(move |_, entry, menu, _, _| {
                        let path = PathBuf::from(entry.item().id.as_ref());
                        let folder = menu_folders.contains(&entry.item().id);
                        // Each line of the menu acts on the panel with the row's path.
                        let item = |label: &'static str, act: fn(&mut Self, PathBuf, &mut Window, &mut Context<Self>)| {
                            let (owner, path) = (menu_owner.clone(), path.clone());
                            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                let _ = owner.update(cx, |this, cx| act(this, path.clone(), window, cx));
                            })
                        };
                        menu.when(folder, |menu| {
                            menu.item(item("New File\u{2026}", |this, path, window, cx| {
                                this.start_naming(Naming::File, path, window, cx)
                            }))
                            .item(item("New Folder\u{2026}", |this, path, window, cx| {
                                this.start_naming(Naming::Folder, path, window, cx)
                            }))
                            .separator()
                        })
                        .item(item("Insert Path into Terminal", |_, path, _, cx| cx.emit(FilesEvent::Insert(path))))
                        .when(!folder && editor.is_some(), |menu| {
                            menu.item(item("Open in Terminal Editor", |_, path, _, cx| {
                                cx.emit(FilesEvent::OpenInTerminal(path))
                            }))
                        })
                        .separator()
                        .item(item("Copy Path", |this, path, _, cx| this.copy_path(&path, false, cx)))
                        .item(item("Copy Relative Path", |this, path, _, cx| this.copy_path(&path, true, cx)))
                        .item(item(reveal_label(), |_, path, _, cx| cx.reveal_path(&path)))
                        .separator()
                        .item(item("Rename\u{2026}", |this, path, window, cx| {
                            this.start_naming(Naming::Rename, path, window, cx)
                        }))
                        .item(item("Move to Trash", |this, path, window, cx| this.delete(path, window, cx)))
                    })
                    .size_full(),
                ),
            )
            .when(self.truncated, |this| {
                this.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("Showing the first {MAX_ENTRIES} entries")),
                )
            })
    }
}

#[cfg(test)]
impl FilesPanel {
    /// Whether the tree has a row for this path right now.
    pub fn shows(&self, path: &Path, cx: &App) -> bool {
        let id: SharedString = path.to_string_lossy().into_owned().into();
        self.tree.read(cx).index_of(&id).is_some()
    }

    /// Renames the way the menu does: show the field, type a name, press Enter.
    pub fn rename_to(&mut self, path: &Path, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.start_naming(Naming::Rename, path.to_path_buf(), window, cx);
        self.name_input.update(cx, |input, cx| input.set_value(name.to_string(), window, cx));
        self.finish_create(window, cx);
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn insert(&mut self, path: &Path, cx: &mut Context<Self>) {
        cx.emit(FilesEvent::Insert(path.to_path_buf()));
    }

    /// Opens a folder the way a click on it does.
    pub fn open_folder(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.expanded.insert(path.to_string_lossy().into_owned().into()) {
            self.refresh(cx);
        }
    }
}
