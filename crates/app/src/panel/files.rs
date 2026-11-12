//! The file tree in the side panel, with git marks next to changed files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::tree::{TreeEvent, TreeItem, TreeState, tree};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wercode_core::{file_icons, fs_tree};
use wercode_core::git::{FileStatus, RepoStatus};

/// Upper bound on entries read at once, so a huge repository cannot stall the window.
const MAX_ENTRIES: usize = 5_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Creating {
    File,
    Folder,
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
    creating: Option<(Creating, PathBuf)>,
    name_input: Entity<InputState>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

fn build(
    dir: &Path,
    budget: &mut usize,
    expanded: &HashSet<SharedString>,
    folders: &mut HashSet<SharedString>,
) -> Vec<TreeItem> {
    let mut items = Vec::new();
    for entry in fs_tree::list_dir(dir) {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let id: SharedString = entry.path.to_string_lossy().into_owned().into();
        let mut item = TreeItem::new(id.clone(), entry.name);
        if entry.is_dir {
            folders.insert(id.clone());
            item = item.children(build(&entry.path, budget, expanded, folders)).expanded(expanded.contains(&id));
        }
        items.push(item);
    }
    items
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

impl FilesPanel {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let expanded = HashSet::new();
        let mut budget = MAX_ENTRIES;
        let mut folders = HashSet::new();
        let items = build(&root, &mut budget, &expanded, &mut folders);
        let tree = cx.new(|cx| TreeState::new(cx).items(items));
        let name_input = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![
            cx.subscribe(&tree, |this, _, event: &TreeEvent, _| match event {
                TreeEvent::Expanded(id) => {
                    this.expanded.insert(id.clone());
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
            truncated: budget == 0,
            expanded,
            folders: Arc::new(folders),
            git: None,
            creating: None,
            name_input,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    /// Reads the folder from disk again, keeping open folders open.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let mut budget = MAX_ENTRIES;
        let mut folders = HashSet::new();
        let items = build(&self.root, &mut budget, &self.expanded, &mut folders);
        self.folders = Arc::new(folders);
        self.truncated = budget == 0;
        self.tree.update(cx, |tree, cx| tree.set_items(items, cx));
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

    fn start_create(&mut self, kind: Creating, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.target_dir(cx);
        self.creating = Some((kind, dir));
        self.error = None;
        self.name_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
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
        let path = dir.join(&name);
        // `create_new` refuses to touch a file that is already there.
        let result = match kind {
            Creating::Folder => std::fs::create_dir_all(&path),
            Creating::File => path
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
                self.refresh(cx);
                let id: SharedString = path.to_string_lossy().into_owned().into();
                self.tree.update(cx, |tree, cx| {
                    let ix = tree.index_of(&id);
                    tree.set_selected_index(ix, cx);
                    if let Some(ix) = ix {
                        tree.scroll_to_item(ix, ScrollStrategy::Center);
                    }
                });
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
                        this.start_create(Creating::File, window, cx)
                    })),
            )
            .child(
                Button::new("new-folder")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::FolderPlus))
                    .tooltip("New folder")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_create(Creating::Folder, window, cx)
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

    fn render_name_field(&self, kind: Creating, dir: &Path, cx: &mut Context<Self>) -> impl IntoElement {
        let place = dir.strip_prefix(&self.root).ok().filter(|p| !p.as_os_str().is_empty());
        let label = match (kind, place) {
            (Creating::File, None) => "New file".to_string(),
            (Creating::Folder, None) => "New folder".to_string(),
            (Creating::File, Some(p)) => format!("New file in {}", p.display()),
            (Creating::Folder, Some(p)) => format!("New folder in {}", p.display()),
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
                                .w_full()
                                .gap_1p5()
                                .text_sm()
                                .when_some(color, |d, c| d.text_color(c))
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
