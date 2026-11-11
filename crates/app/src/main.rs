// No console window next to the app on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod assets;
mod panel;
mod term;
mod theme;
mod workspace;

use std::path::PathBuf;

use gpui_kit::component::TitleBar;
use gpui_kit::*;
use wercode_core::state::State;

use crate::workspace::Workspace;

/// The folder to open: the first argument; without one, the project that was
/// open last time. With neither, the welcome screen.
fn project_root(state: &State) -> Option<PathBuf> {
    let path = std::env::args_os().nth(1).map(PathBuf::from).or_else(|| state.last_project())?;
    Some(workspace::normalize(&path))
}

/// Where the window was last time, unless that place is no longer on any screen.
fn window_bounds(state: &State, cx: &App) -> Option<WindowBounds> {
    let [x, y, width, height] = state.window?;
    let bounds = Bounds::new(point(px(x), px(y)), size(px(width.max(640.)), px(height.max(400.))));
    if !cx.displays().iter().any(|display| display.bounds().intersects(&bounds)) {
        return None;
    }
    Some(if state.maximized { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) })
}

fn main() {
    let state = wercode_core::state::load();
    let root = project_root(&state);
    let (settings, settings_error) = wercode_core::config::load();
    if let Some(e) = settings_error {
        eprintln!("settings ignored: {e}");
    }

    gpui_kit::application().with_assets(assets::AppAssets).run(move |cx| {
        gpui_kit::init(cx);
        theme::apply(settings, cx);
        term::init(cx);
        workspace::init(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let options = WindowOptions {
            window_min_size: Some(size(px(640.), px(400.))),
            window_bounds: window_bounds(&state, cx),
            ..TitleBar::window_options()
        };
        let state_path = wercode_core::state::state_path();
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::new(root, state, state_path, window, cx))
        })
        .expect("failed to open window");
        cx.activate(true);
    });
}
