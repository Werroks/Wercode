// No console window next to the app on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod panel;
mod term;
mod theme;
mod workspace;

use std::path::PathBuf;

use gpui_kit::component::TitleBar;
use gpui_kit::*;

use crate::workspace::Workspace;

/// The folder to open: the first argument, or the current directory.
fn project_root() -> PathBuf {
    let arg = std::env::args_os().nth(1).map(PathBuf::from);
    let path = arg
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::canonicalize(&path)
        .map(|p| {
            // Drop the Windows `\\?\` prefix: shells and git do not expect it.
            let s = p.to_string_lossy();
            match s.strip_prefix(r"\\?\") {
                Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
                _ => p.clone(),
            }
        })
        .unwrap_or(path)
}

fn main() {
    let root = project_root();
    let (settings, settings_error) = wercode_core::config::load();
    if let Some(e) = settings_error {
        eprintln!("settings ignored: {e}");
    }

    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        theme::apply(settings, cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let options = WindowOptions {
            window_min_size: Some(size(px(640.), px(400.))),
            ..TitleBar::window_options()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::new(root, window, cx))
        })
        .expect("failed to open window");
        cx.activate(true);
    });
}
