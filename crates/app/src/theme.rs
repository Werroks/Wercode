//! The active colour scheme, shared by every view.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Global, Hsla, rgb};
use wercode_core::config::Settings;
use wercode_core::theme::{Appearance, Rgb, Theme as Scheme};

pub struct AppState {
    pub scheme: Scheme,
    pub settings: Settings,
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
        c.popover = hsla(ui.surface);
        c.popover_foreground = hsla(ui.text);
        c.list = hsla(ui.bg);
        c.list_hover = hsla(ui.surface);
        c.list_active = hsla(ui.surface_2);
        c.list_active_border = hsla(ui.surface_2);
        c.list_even = hsla(ui.bg);
        c.title_bar = hsla(ui.bg);
        c.title_bar_border = hsla(ui.border);
        c.tab_bar = hsla(ui.bg);
        c.tab = hsla(ui.bg);
        c.tab_active = hsla(ui.surface);
        c.tab_foreground = hsla(ui.text_muted);
        c.tab_active_foreground = hsla(ui.text);
        c.sidebar = hsla(ui.bg);
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

    cx.set_global(AppState { scheme, settings });
}
