//! The settings screen. Every change is applied to all windows at once and
//! written to `settings.toml`.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wercode_core::config::Settings;
use wercode_core::shell::{self, Found};

use crate::theme::{self, AppState};
use crate::update;
use wercode_core::i18n::{Language, t, tf};

const FONT_SIZES: std::ops::RangeInclusive<f32> = 8.0..=32.0;

/// Where people who want to support Wercode can send USDT (TRC-20).
pub const DONATION_ADDRESS: &str = "TVb5SbxLo7kWetAw3Y7n4bdArfsfD1wLRX";
/// How long the copy button says the address was copied.
const COPIED_FOR: std::time::Duration = std::time::Duration::from_secs(3);

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
    /// The donation address was just copied; the button says so for a moment.
    copied: Option<Task<()>>,
    _subscription: Subscription,
    _updates: Subscription,
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
            copied: None,
            _subscription: subscription,
            _updates: cx.observe_global::<update::Updater>(|_, cx| cx.notify()),
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

    /// Automatic updates, a button to look now, and what the last look found.
    fn render_updates(&self, settings: &Settings, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let status = update::status(cx);
        let off = status == update::Status::Off;
        let busy = matches!(status, update::Status::Checking | update::Status::Downloading(_));
        let this = cx.entity().downgrade();
        let switch = Switch::new("settings-auto-update").checked(settings.auto_update).disabled(off).on_click(
            move |on: &bool, _, cx| {
                let on = *on;
                let _ = this.update(cx, |this, cx| this.change(cx, |s| s.auto_update = on));
            },
        );
        let (said, failed) = match &status {
            update::Status::Off => {
                (t("Updates come with installed releases. This copy was built from source.").to_string(), false)
            }
            update::Status::Idle => (String::new(), false),
            update::Status::Checking => (t("Checking\u{2026}").to_string(), false),
            update::Status::UpToDate => (t("Wercode is up to date.").to_string(), false),
            update::Status::Downloading(version) => (tf("Downloading version {version}\u{2026}", &[("version", version)]), false),
            update::Status::Ready(version) => {
                (tf("Version {version} is ready. It is installed when Wercode restarts.", &[("version", version)]), false)
            }
            update::Status::Failed(error) => (tf("Could not update: {error}", &[("error", error)]), true),
        };
        let ready = matches!(status, update::Status::Ready(_));
        let control = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(switch)
                    .child(
                        Button::new("settings-check-updates")
                            .small()
                            .outline()
                            .label(t("Check Now"))
                            .disabled(off || busy || ready)
                            .on_click(|_: &ClickEvent, _, cx| update::check(true, cx)),
                    )
                    .when(ready, |d| {
                        d.child(
                            Button::new("settings-restart-update")
                                .small()
                                .primary()
                                .label(t("Restart to Update"))
                                .on_click(|_: &ClickEvent, _, cx| update::restart(cx)),
                        )
                    })
                    // A copy built from source never updates itself: the
                    // installed release does, so point to it.
                    .when(off, |d| {
                        d.child(
                            Button::new("settings-get-installer")
                                .small()
                                .outline()
                                .label(t("Get the Installer"))
                                .on_click(|_: &ClickEvent, _, cx| cx.open_url(wercode_core::update::RELEASES_URL)),
                        )
                    }),
            )
            .when(!said.is_empty(), |d| {
                d.child(div().id("settings-update-status").text_xs().text_color(if failed { danger } else { muted }).child(said))
            });
        let note = tf(
            "Wercode {version}. New versions download in the background and are installed when Wercode restarts.",
            &[("version", &wercode_core::update::VERSION)],
        );
        h_flex()
            .w_full()
            .gap_4()
            .items_start()
            .child(
                v_flex()
                    .w(px(190.))
                    .flex_none()
                    .child(div().text_sm().child(t("Automatic updates")))
                    .child(div().text_xs().text_color(muted).child(note)),
            )
            .child(div().flex_1().min_w_0().child(control))
    }

    fn copy_address(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(DONATION_ADDRESS.to_string()));
        self.copied = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            let _ = this.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// How to support Wercode: the address for USDT and a button to copy it.
    fn render_support(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let (muted, border, mono) = (theme.muted_foreground, theme.border, theme.mono_font_family.clone());
        let copied = self.copied.is_some();
        v_flex()
            .id("settings-support")
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::Heart).small().text_color(theme.danger))
                    .child(div().text_sm().child(t("Support Wercode"))),
            )
            .child(div().text_xs().text_color(muted).child(t(
                "Wercode is free and open source. If it saves you time and you want to help it grow, you can send USDT on the TRON network (TRC-20) to this address:",
            )))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("settings-donation-address")
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(border)
                            .font_family(mono)
                            .text_xs()
                            .truncate()
                            .child(DONATION_ADDRESS),
                    )
                    .child(
                        Button::new("settings-copy-address")
                            .small()
                            .outline()
                            .icon(if copied { IconName::Check } else { IconName::Copy })
                            .label(if copied { t("Copied") } else { t("Copy") })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy_address(cx))),
                    ),
            )
            .child(div().text_xs().text_color(muted).child(t(
                "Send only USDT on TRC-20 to this address; coins sent on another network are lost.",
            )))
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
        let updates = self.render_updates(&settings, cx);
        let support = self.render_support(cx);
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
            .child(Self::choice("settings-shell-auto", t("Automatic"), auto).on_click(cx.listener(
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
            .child(side("settings-side-left", t("Left"), "left", !on_right))
            .child(side("settings-side-right", t("Right"), "right", on_right));

        // Each language by its own name, so it can be found whatever is shown now.
        let chosen = Language::from_code(&settings.language);
        let languages = h_flex()
            .flex_wrap()
            .gap_1()
            .child(Self::choice("settings-language-auto", t("Automatic"), chosen.is_none()).on_click(cx.listener(
                |this, _: &ClickEvent, _, cx| this.change(cx, |s| s.language = "auto".into()),
            )))
            .children(Language::ALL.into_iter().enumerate().map(|(index, language)| {
                Self::choice(("settings-language", index), language.native_name(), chosen == Some(language)).on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.change(cx, |s| s.language = language.code().into())
                    }),
                )
            }));

        let below = settings.terminal_below();
        let layout = |id: &'static str, label: &'static str, value: &'static str, chosen: bool| {
            Self::choice(id, label, chosen)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.change(cx, |s| s.layout = value.into())))
        };
        let layouts = h_flex()
            .gap_1()
            .child(layout("settings-layout-tabs", t("One row of tabs"), "tabs", !below))
            .child(layout("settings-layout-below", t("Terminal below files"), "terminal_below", below));

        let this = cx.entity().downgrade();
        let switch = |id: &'static str, on: bool, set: fn(&mut Settings, bool)| {
            let this = this.clone();
            Switch::new(id).checked(on).on_click(move |on: &bool, _, cx| {
                let on = *on;
                let _ = this.update(cx, |this, cx| this.change(cx, |s| set(s, on)));
            })
        };

        // Shortcuts are changed in the file; here they are listed with their names.
        let mono = theme.mono_font_family.clone();
        let keys = v_flex().id("settings-keys").gap_0p5().children(crate::keymap::listing(cx).into_iter().map(
            |(command, keys)| {
                h_flex()
                    .gap_3()
                    .text_xs()
                    .child(div().flex_1().min_w_0().truncate().font_family(mono.clone()).child(command))
                    .child(div().flex_none().text_color(muted).child(keys.join(",  ")))
            },
        ));
        let key_problems = crate::keymap::problems(cx);

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
                    .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(t("Settings")))
                    .child(
                        Button::new("settings-close")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip(t("Close (Esc)"))
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
                    .child(Self::row(
                        t("Language"),
                        t("Automatic follows the language of this computer."),
                        languages,
                        cx,
                    ))
                    .child(Self::row(t("Colour scheme"), "", schemes, cx))
                    .child(Self::row(t("Font size"), t("Text in the terminal and in the editor."), font_size, cx))
                    .child(Self::row(
                        t("Font"),
                        t("A monospaced font installed on this computer. Press Enter to apply."),
                        Input::new(&self.font).small(),
                        cx,
                    ))
                    .child(Self::row(t("Shell"), t("Used by terminals opened from now on."), shells, cx))
                    .child(Self::row(t("Side panel"), t("Where files, search and git sit."), sides, cx))
                    .child(Self::row(
                        t("Layout"),
                        t("Files and terminals as tabs of one row, or files above a terminal panel."),
                        layouts,
                        cx,
                    ))
                    .child(Self::row(
                        t("Blinking cursor"),
                        "",
                        switch("settings-blink", settings.cursor_blink, |s, on| s.cursor_blink = on),
                        cx,
                    ))
                    .child(Self::row(
                        t("Confirm closing"),
                        t("Ask before closing a terminal that is still running a program."),
                        switch("settings-confirm", settings.confirm_close, |s, on| s.confirm_close = on),
                        cx,
                    ))
                    .child(Self::row(
                        t("Copy on select"),
                        t("Selecting text in a terminal copies it."),
                        switch("settings-copy", settings.copy_on_select, |s, on| s.copy_on_select = on),
                        cx,
                    ))
                    .child(Self::row(
                        t("Check code"),
                        t("Underline errors and warnings in open files, using the language servers installed on this computer."),
                        switch("settings-language-servers", settings.language_servers, |s, on| s.language_servers = on),
                        cx,
                    ))
                    .child(Self::row(
                        t("Format on save"),
                        t("Lay out the code by the rules of its language when a file is saved, if its language server can."),
                        switch("settings-format-on-save", settings.format_on_save, |s, on| s.format_on_save = on),
                        cx,
                    ))
                    .child(updates)
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().id("settings-error").text_xs().text_color(danger).child(error))
                    })
                    .child(div().flex_none().h(px(1.)).bg(border))
                    .child(support)
                    .child(div().flex_none().h(px(1.)).bg(border))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_sm().child(t("Keyboard shortcuts")))
                            .child(div().text_xs().text_color(muted).child(
                                t("To change one, add its name to the [keys] table of settings.toml, for example \"terminal.new_tab\" = \"ctrl-shift-y\". An empty value removes the shortcut."),
                            )),
                    )
                    .children(
                        key_problems
                            .into_iter()
                            .map(|problem| div().text_xs().text_color(danger).child(problem)),
                    )
                    .child(keys),
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
                        .child(Button::new("settings-open-file").small().outline().label(t("Edit settings.toml")).on_click(
                            cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(SettingsEvent::OpenFile(file.clone()))),
                        )),
                )
            })
    }
}
