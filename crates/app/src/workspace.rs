//! The window's root view: title bar, terminal in the centre, side panel on the right.

use std::path::PathBuf;

use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme as _, TitleBar, v_flex};
use gpui_kit::*;

use crate::panel::FilesPanel;
use crate::term::TerminalView;
use crate::theme::AppState;

pub struct Workspace {
    root: PathBuf,
    terminal: Entity<TerminalView>,
    files: Entity<FilesPanel>,
}

impl Workspace {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let terminal = cx.new(|cx| TerminalView::new(Some(root.clone()), cx));
        let files = cx.new(|cx| FilesPanel::new(root.clone(), cx));
        let handle = terminal.focus_handle(cx);
        window.focus(&handle, cx);
        Self { root, terminal, files }
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel_width = cx.global::<AppState>().settings.panel_width;
        let project = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.to_string_lossy().into_owned());

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new().child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(project),
                ),
            )
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("workspace")
                        .child(resizable_panel().child(self.terminal.clone()))
                        .child(
                            resizable_panel()
                                .size(px(panel_width))
                                .size_range(px(200.)..px(800.))
                                .child(self.files.clone()),
                        ),
                ),
            )
    }
}
