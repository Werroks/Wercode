//! The file tree in the side panel.

use std::path::{Path, PathBuf};

use gpui_kit::component::list::ListItem;
use gpui_kit::component::tree::{TreeItem, TreeState, tree};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wercode_core::fs_tree;

/// Upper bound on entries read at once, so a huge repository cannot stall the window.
const MAX_ENTRIES: usize = 5_000;

pub struct FilesPanel {
    root: PathBuf,
    tree: Entity<TreeState>,
    truncated: bool,
}

fn build(dir: &Path, budget: &mut usize) -> Vec<TreeItem> {
    let mut items = Vec::new();
    for entry in fs_tree::list_dir(dir) {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let id = entry.path.to_string_lossy().into_owned();
        let mut item = TreeItem::new(id, entry.name);
        if entry.is_dir {
            item = item.children(build(&entry.path, budget));
        }
        items.push(item);
    }
    items
}

impl FilesPanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut budget = MAX_ENTRIES;
        let items = build(&root, &mut budget);
        let tree = cx.new(|cx| TreeState::new(cx).items(items));
        Self { root, tree, truncated: budget == 0 }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let name = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.to_string_lossy().into_owned());

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .border_l_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .h(px(36.))
                    .px_3()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(muted)
                    .child(name.to_uppercase()),
            )
            .child(
                div().flex_1().min_h_0().child(
                    tree(&self.tree, move |ix, entry, _selected, _window, _cx| {
                        let icon = if entry.is_folder() {
                            if entry.is_expanded() { IconName::FolderOpen } else { IconName::Folder }
                        } else {
                            IconName::File
                        };
                        ListItem::new(ix)
                            .pl(px(8.) + px(12.) * entry.depth() as f32)
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_sm()
                                    .child(Icon::new(icon).small().text_color(muted))
                                    .child(entry.item().label.clone()),
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
