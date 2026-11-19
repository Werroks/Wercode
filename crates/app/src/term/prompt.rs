//! The prompt box: a text field under a terminal for writing to the agent
//! running in it (Claude Code, Codex and the like). It edits like any text
//! field, keeps pasted pictures as thumbnails, and sends everything to the
//! program at once on Enter.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Paste, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wercode_core::i18n::{t, tf};
use wercode_core::{agent, fs_ops, pasted};

/// How tall the box grows with its text before it scrolls.
const MAX_ROWS: usize = 12;

pub enum PromptEvent {
    /// Hand this to the program and press Enter.
    Send { text: String, pictures: Vec<PathBuf> },
    /// A key for the program itself: one that means nothing to an empty box,
    /// such as the arrows and Enter that answer an agent's question.
    Key(Keystroke),
    /// The user closed the box.
    Close,
}

pub struct PromptBox {
    text: Entity<TextareaState>,
    focus: FocusHandle,
    /// Pictures waiting to be sent, saved as files.
    pictures: Vec<PathBuf>,
    /// The box opened by itself when an agent started, rather than at the user's word.
    pub(super) for_agent: bool,
    error: Option<String>,
    _keys: Subscription,
}

impl EventEmitter<PromptEvent> for PromptBox {}

impl PromptBox {
    pub fn new(for_agent: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = cx.new(|cx| {
            TextareaState::new(window, cx).auto_grow(1, MAX_ROWS).placeholder(t(
                "Write to the program here. Enter sends, Shift+Enter starts a new line, Ctrl+V pastes a picture.",
            ))
        });
        let focus = text.read(cx).focus_handle(cx);
        // The text field binds Enter, the arrows, Escape and the clipboard
        // keys itself, so the box looks at each key before the field does.
        let this = cx.entity().downgrade();
        let watched = focus.clone();
        let keys = cx.intercept_keystrokes(move |event, window, cx| {
            if !watched.is_focused(window) {
                return;
            }
            let Some(this) = this.upgrade() else { return };
            if this.update(cx, |this, cx| this.on_key(&event.keystroke, window, cx)) {
                cx.stop_propagation();
            }
        });
        Self { text, focus, pictures: Vec::new(), for_agent, error: None, _keys: keys }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.text.update(cx, |text, cx| text.focus(window, cx));
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// Nothing written and no picture waiting.
    pub fn is_empty(&self, cx: &App) -> bool {
        self.pictures.is_empty() && self.text.read(cx).value().trim().is_empty()
    }

    /// Returns true when the key was used here and must go no further.
    fn on_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let m = &keystroke.modifiers;
        let plain = !m.control && !m.alt && !m.shift && !m.platform;
        let empty = self.is_empty(cx);
        match keystroke.key.as_str() {
            "enter" if plain => {
                if empty {
                    cx.emit(PromptEvent::Key(keystroke.clone()));
                } else {
                    self.submit(window, cx);
                }
                true
            }
            // A new line, as in the terminal. Ctrl+Shift+Enter stays the commit shortcut.
            "enter" if (m.shift != m.control) && !m.alt => {
                self.text.update(cx, |text, cx| text.replace("\n", window, cx));
                true
            }
            // Escape stops the agent, whatever is written here.
            "escape" if plain => {
                cx.emit(PromptEvent::Key(keystroke.clone()));
                true
            }
            "up" | "down" | "tab" if empty && !m.control && !m.alt && !m.platform => {
                cx.emit(PromptEvent::Key(keystroke.clone()));
                true
            }
            "c" if empty && m.control && !m.shift && !m.alt => {
                cx.emit(PromptEvent::Key(keystroke.clone()));
                true
            }
            // The terminal's other paste keys, which the field does not know.
            // The field's own paste then takes pictures through `paste_item`.
            "v" if m.control && m.shift && !m.alt => {
                window.dispatch_action(Box::new(Paste), cx);
                true
            }
            "insert" if m.shift && !m.control && !m.alt => {
                window.dispatch_action(Box::new(Paste), cx);
                true
            }
            _ => false,
        }
    }

    /// A paste into the field, by key or from its menu: a picture or copied
    /// files become attachments. Returns false for text, which the field
    /// inserts itself.
    fn paste_item(&mut self, item: &ClipboardItem, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let has_text = item.entries().iter().any(|entry| match entry {
            ClipboardEntry::String(string) => !string.text().is_empty(),
            _ => false,
        });
        if has_text {
            return false;
        }
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(picture) => {
                    self.save_picture(picture.bytes.clone(), picture.format.extension(), cx);
                    return true;
                }
                ClipboardEntry::ExternalPaths(files) => {
                    for path in files.paths() {
                        self.add_path(path, None, window, cx);
                    }
                    return true;
                }
                ClipboardEntry::String(_) => {}
            }
        }
        false
    }

    /// A file for the message: a picture waits as a thumbnail, anything else
    /// is written as its path, relative to `cwd` when it is inside it.
    pub fn add_path(&mut self, path: &Path, cwd: Option<&Path>, window: &mut Window, cx: &mut Context<Self>) {
        if agent::is_picture(path) {
            self.pictures.push(path.to_path_buf());
            self.error = None;
            cx.notify();
        } else {
            let text = format!("{} ", fs_ops::for_shell(path, cwd));
            self.text.update(cx, |field, cx| field.replace(text, window, cx));
        }
    }

    /// Saves a pasted picture away from the window's thread and shows it.
    fn save_picture(&mut self, bytes: Vec<u8>, extension: &'static str, cx: &mut Context<Self>) {
        let saving = cx.background_executor().spawn(async move { pasted::save(&bytes, extension, &pasted::folder()) });
        cx.spawn(async move |this, cx| {
            let saved = saving.await;
            let _ = this.update(cx, |this, cx| {
                match saved {
                    Ok(path) => {
                        this.pictures.push(path);
                        this.error = None;
                    }
                    Err(error) => {
                        this.error =
                            Some(tf("Could not save the pasted picture: {error}", &[("error", &error)]));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text.read(cx).value().trim().to_string();
        if text.is_empty() && self.pictures.is_empty() {
            return;
        }
        let pictures = std::mem::take(&mut self.pictures);
        self.text.update(cx, |field, cx| field.set_value("", window, cx));
        self.error = None;
        cx.emit(PromptEvent::Send { text, pictures });
        cx.notify();
    }

    fn pick_pictures(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked =
            cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    for path in &paths {
                        this.add_path(path, None, window, cx);
                    }
                    this.focus(window, cx);
                });
            }
        })
        .detach();
    }

    fn render_pictures(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        h_flex().flex_wrap().gap_1p5().children(self.pictures.iter().enumerate().map(|(index, path)| {
            div()
                .id(("prompt-picture", index))
                .relative()
                .size(px(52.))
                .flex_none()
                .rounded_md()
                .overflow_hidden()
                .border_1()
                .border_color(border)
                .child(img(path.clone()).size_full().object_fit(ObjectFit::Cover))
                .child(
                    div().absolute().top(px(2.)).right(px(2.)).child(
                        Button::new(("prompt-picture-remove", index))
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip(t("Remove this picture"))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if index < this.pictures.len() {
                                    this.pictures.remove(index);
                                }
                                cx.notify();
                            })),
                    ),
                )
        }))
    }
}

impl Render for PromptBox {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, background, danger) = (theme.border, theme.background, theme.danger);
        let empty = self.is_empty(cx);
        let this = cx.entity().downgrade();
        let close_label = match crate::keymap::label_for("terminal.prompt_box", cx) {
            Some(keys) => tf("Close the prompt box ({keys})", &[("keys", &keys)]),
            None => t("Close the prompt box").to_string(),
        };
        v_flex()
            .id("prompt-box")
            .test_support()
            .flex_none()
            .w_full()
            .gap_1p5()
            .px_2()
            .py_1p5()
            .border_t_1()
            .border_color(border)
            .bg(background)
            .when(!self.pictures.is_empty(), |d| d.child(self.render_pictures(cx)))
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .items_end()
                    .child(div().flex_1().min_w_0().child(Textarea::new(&self.text).on_paste(move |item, window, cx| {
                        this.update(cx, |this, cx| this.paste_item(item, window, cx)).unwrap_or(false)
                    })))
                    .child(
                        Button::new("prompt-attach")
                            .ghost()
                            .small()
                            .icon(Lucide::Paperclip)
                            .tooltip(t("Attach pictures"))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.pick_pictures(window, cx))),
                    )
                    .child(
                        Button::new("prompt-send")
                            .primary()
                            .small()
                            .icon(IconName::ArrowUp)
                            .disabled(empty)
                            .tooltip(t("Send (Enter)"))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.submit(window, cx))),
                    )
                    .child(
                        Button::new("prompt-close")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .tooltip(close_label)
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(PromptEvent::Close))),
                    ),
            )
            .when_some(self.error.clone(), |d, error| d.child(div().text_xs().text_color(danger).child(error)))
    }
}

#[cfg(test)]
impl PromptBox {
    pub fn text(&self, cx: &App) -> String {
        self.text.read(cx).value().to_string()
    }

    pub fn pictures(&self) -> &[PathBuf] {
        &self.pictures
    }
}
