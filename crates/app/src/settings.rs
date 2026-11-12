//! The settings screen. Every change is applied to all windows at once and
//! written to `settings.toml`.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wercode_core::config::Settings;
use wercode_core::shell::{self, Found};

use crate::theme::{self, AppState};

const FONT_SIZES: std::ops::RangeInclusive<f32> = 8.0..=32.0;

pub enum SettingsEvent {
    Close,
    /// Show the settings file itself in the editor.
    OpenFile(PathBuf),
}

pub struct SettingsView {
    focus: FocusHandle,
    font: Entity<InputState>,
    /// Colour schemes: the value for the setting, and the name to show.
    schemes: Vec<(&'static str, String)>,
    shells: Vec<Found>,
    error: Option<String>,
    _subscription: Subscription,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let family = cx.global::<AppState>().settings.font_family.clone();
        let font = cx.new(|cx| InputState::new(window, cx).default_value(family));
        let subscription = cx.subscribe(&font, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.apply_font(cx);
            }
        });
        let schemes = wercode_core::theme::builtin_ids()
            .map(|id| (id, wercode_core::theme::load(id).name))
            .collect();
        Self {
            focus: cx.focus_handle(),
            font,
            schemes,
            shells: shell::available(),
            error: None,
            _subscription: subscription,
        }
    }

    fn change(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Settings)) {
        self.error = theme::change_settings(cx, change).err();
        cx.notify();
    }

    fn apply_font(&mut self, cx: &mut Context<Self>) {
        let family = self.font.read(cx).value().trim().to_string();
        if !family.is_empty() && family != cx.global::<AppState>().settings.font_family {
            self.change(cx, |s| s.font_family = family);
        }
    }

    fn resize_font(&mut self, step: f32, cx: &mut Context<Self>) {
        self.change(cx, |s| s.font_size = (s.font_size + step).clamp(*FONT_SIZES.start(), *FONT_SIZES.end()));
    }

    /// One setting: its name and a note on the left, the control on the right.
    fn row(name: &'static str, note: &'static str, control: impl IntoElement, cx: &App) -> Div {
        h_flex()
            .w_full()
            .gap_4()
            .items_start()
            .child(
                v_flex()
                    .w(px(190.))
                    .flex_none()
                    .child(div().text_sm().child(name))
                    .when(!note.is_empty(), |d| {
                        d.child(div().text_xs().text_color(cx.theme().muted_foreground).child(note))
                    }),
            )
            .child(div().flex_1().min_w_0().child(control))
    }

    /// A button of a group in which exactly one is chosen.
    fn choice(id: impl Into<ElementId>, label: impl Into<SharedString>, chosen: bool) -> Button {
        let button = Button::new(id).small().label(label);
        if chosen { button.primary() } else { button.outline() }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = cx.global::<AppState>();
        let settings = state.settings.clone();
        let path = state.settings_path.clone();
        let theme = cx.theme();
        let (muted, border, danger) = (theme.muted_foreground, theme.border, theme.danger);

        let schemes = h_flex().flex_wrap().gap_1().children(self.schemes.iter().enumerate().map(|(index, (id, name))| {
            let id = *id;
            Self::choice(("settings-scheme", index), name.clone(), settings.theme == id)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.change(cx, |s| s.theme = id.into())))
        }));

        let font_size = h_flex()
            .gap_2()
            .child(
                Button::new("settings-font-smaller")
                    .small()
                    .outline()
                    .icon(IconName::Minus)
                    .disabled(settings.font_size <= *FONT_SIZES.start())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.resize_font(-1.0, cx))),
            )
            .child(div().id("settings-font-size").w(px(28.)).text_sm().text_center().child(format!("{}", settings.font_size)))
            .child(
                Button::new("settings-font-larger")
                    .small()
                    .outline()
                    .icon(IconName::Plus)
                    .disabled(settings.font_size >= *FONT_SIZES.end())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.resize_font(1.0, cx))),
            );

        let auto = settings.shell.is_empty() || settings.shell == "auto";
        let shells = h_flex()
            .flex_wrap()
            .gap_1()
            .child(Self::choice("settings-shell-auto", "Automatic", auto).on_click(cx.listener(
                |this, _: &ClickEvent, _, cx| {
                    this.change(cx, |s| {
                        s.shell = "auto".into();
                        s.shell_args.clear();
                    })
                },
            )))
            .children(self.shells.iter().enumerate().map(|(index, found)| {
                let setting = found.setting.clone();
                Self::choice(("settings-shell", index), found.name, settings.shell == found.setting).on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let setting = setting.clone();
                        // Arguments written for one shell mean nothing to another.
                        this.change(cx, |s| {
                            s.shell = setting;
                            s.shell_args.clear();
                        })
                    }),
                )
            }));

        let on_right = settings.panel_on_right();
        let side = |id: &'static str, label: &'static str, value: &'static str, chosen: bool| {
            Self::choice(id, label, chosen)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.change(cx, |s| s.panel_side = value.into())))
        };
        let sides = h_flex()
            .gap_1()
            .child(side("settings-side-left", "Left", "left", !on_right))
            .child(side("settings-side-right", "Right", "right", on_right));

        let this = cx.entity().downgrade();
        let switch = |id: &'static str, on: bool, set: fn(&mut Settings, bool)| {
            let this = this.clone();
            Switch::new(id).checked(on).on_click(move |on: &bool, _, cx| {
                let on = *on;
                let _ = this.update(cx, |this, cx| this.change(cx, |s| set(s, on)));
            })
        };

        let file = path.clone();
        v_flex()
            .id("settings")
            .test_support()
            .track_focus(&self.focus)
            // A press inside the card must not reach the backdrop, which closes it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(SettingsEvent::Close);
                    cx.stop_propagation();
                }
            }))
            .w(px(620.))
            .max_w(relative(0.94))
            .max_h(relative(0.9))
            .rounded_lg()
            .border_1()
            .border_color(border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_lg()
            .child(
                h_flex()
                    .flex_none()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(border)
                    .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child("Settings"))
                    .child(
                        Button::new("settings-close")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("Close (Esc)")
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(SettingsEvent::Close))),
                    ),
            )
            .child(
                v_flex()
                    .id("settings-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .gap_4()
                    .child(Self::row("Colour scheme", "", schemes, cx))
                    .child(Self::row("Font size", "Text in the terminal.", font_size, cx))
                    .child(Self::row(
                        "Font",
                        "A monospaced font installed on this computer. Press Enter to apply.",
                        Input::new(&self.font).small(),
                        cx,
                    ))
                    .child(Self::row("Shell", "Used by terminals opened from now on.", shells, cx))
                    .child(Self::row("Side panel", "Where files, search and git sit.", sides, cx))
                    .child(Self::row(
                        "Blinking cursor",
                        "",
                        switch("settings-blink", settings.cursor_blink, |s, on| s.cursor_blink = on),
                        cx,
                    ))
                    .child(Self::row(
                        "Confirm closing",
                        "Ask before closing a terminal that is still running a program.",
                        switch("settings-confirm", settings.confirm_close, |s, on| s.confirm_close = on),
                        cx,
                    ))
                    .child(Self::row(
                        "Copy on select",
                        "Selecting text in a terminal copies it.",
                        switch("settings-copy", settings.copy_on_select, |s, on| s.copy_on_select = on),
                        cx,
                    ))
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().id("settings-error").text_xs().text_color(danger).child(error))
                    }),
            )
            .when_some(file, |d, file| {
                d.child(
                    h_flex()
                        .flex_none()
                        .px_4()
                        .py_2()
                        .gap_3()
                        .border_t_1()
                        .border_color(border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(file.to_string_lossy().into_owned()),
                        )
                        .child(Button::new("settings-open-file").small().outline().label("Edit settings.toml").on_click(
                            cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(SettingsEvent::OpenFile(file.clone()))),
                        )),
                )
            })
    }
}
