//! The active colour scheme, shared by every view.

use std::path::PathBuf;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Global, Hsla, rgb};
use wercode_core::config::Settings;
use wercode_core::theme::{Appearance, Rgb, Theme as Scheme};

pub struct AppState {
    pub scheme: Scheme,
    pub settings: Settings,
    /// Where the settings screen writes; `None` keeps changes in memory only.
    pub settings_path: Option<PathBuf>,
}

impl Global for AppState {}

pub fn hsla(c: Rgb) -> Hsla {
    rgb(c.as_u32()).into()
}

/// Installs the scheme: our own roles for custom drawing, and the matching
/// colours for the component library so both look like one app.
pub fn apply(settings: Settings, cx: &mut App) {
    let scheme = wercode_core::theme::load(&settings.theme);
    let mode = match scheme.appearance {
        Appearance::Dark => ThemeMode::Dark,
        Appearance::Light => ThemeMode::Light,
    };
    Theme::change(mode, None, cx);

    let ui = scheme.ui.clone();
    let mono = settings.font_family.clone();
    Theme::update(cx, move |t| {
        let c = &mut t.colors;
        c.background = hsla(ui.bg);
        c.foreground = hsla(ui.text);
        c.muted = hsla(ui.surface);
        c.muted_foreground = hsla(ui.text_muted);
        c.border = hsla(ui.border);
        c.input = hsla(ui.border_input);
        c.ring = hsla(ui.accent);
        c.primary = hsla(ui.accent);
        c.primary_hover = hsla(ui.accent);
        c.primary_active = hsla(ui.accent);
        c.primary_foreground = hsla(ui.on_accent);
        c.secondary = hsla(ui.surface);
        c.secondary_hover = hsla(ui.surface_2);
        c.secondary_active = hsla(ui.surface_2);
        c.secondary_foreground = hsla(ui.text);
        c.accent = hsla(ui.surface_2);
        c.accent_foreground = hsla(ui.text);
        c.popover = hsla(ui.surface_2);
        c.popover_foreground = hsla(ui.text);
        c.list = hsla(ui.surface);
        c.list_hover = hsla(ui.surface_2);
        c.list_active = hsla(ui.surface_2);
        c.list_active_border = hsla(ui.surface_2);
        c.list_even = hsla(ui.surface);
        c.title_bar = hsla(ui.surface);
        c.title_bar_border = hsla(ui.border);
        c.tab_bar = hsla(ui.surface);
        c.tab = hsla(ui.surface);
        c.tab_active = hsla(ui.bg);
        c.tab_foreground = hsla(ui.text_muted);
        c.tab_active_foreground = hsla(ui.text);
        c.sidebar = hsla(ui.surface);
        c.sidebar_foreground = hsla(ui.text);
        c.sidebar_border = hsla(ui.border);
        c.selection = hsla(ui.selection);
        c.caret = hsla(ui.text);
        c.danger = hsla(ui.danger);
        c.success = hsla(ui.success);
        c.warning = hsla(ui.warning);
        c.link = hsla(ui.accent);
        c.scrollbar_thumb = hsla(ui.border);
        c.scrollbar_thumb_hover = hsla(ui.border_input);
        c.window_border = hsla(ui.border);
        t.mono_font_family = mono.into();
    });

    let settings_path = cx.try_global::<AppState>().and_then(|state| state.settings_path.clone());
    cx.set_global(AppState { scheme, settings, settings_path });
    cx.refresh_windows();
}

/// Says which file the settings live in. Tests leave it unset, so they never
/// touch the user's own settings.
pub fn set_settings_path(path: Option<PathBuf>, cx: &mut App) {
    cx.global_mut::<AppState>().settings_path = path;
}

/// Changes the settings: every window shows the result at once, and the file
/// is updated. An error means the file could not be written; the change still
/// holds until the app is closed.
pub fn change_settings(cx: &mut App, change: impl FnOnce(&mut Settings)) -> Result<(), String> {
    let state = cx.global::<AppState>();
    let (mut settings, path) = (state.settings.clone(), state.settings_path.clone());
    change(&mut settings);
    let saved = match &path {
        Some(path) => wercode_core::config::save_to(path, &settings),
        None => Ok(()),
    };
    apply(settings, cx);
    saved
}

/// Reads the settings file again, picking up what was edited by hand.
pub fn reload_settings(cx: &mut App) {
    let Some(path) = cx.global::<AppState>().settings_path.clone() else { return };
    // A file with a mistake in it keeps the settings that are in use.
    if let (settings, None) = wercode_core::config::load_from(&path) {
        apply(settings, cx);
    }
}
