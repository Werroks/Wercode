pub mod area;
pub mod input;
pub mod mouse;
pub mod view;

use gpui_kit::{App, KeyBinding, actions};

pub use area::TerminalArea;

actions!(
    terminal,
    [
        /// Tab goes to the shell (completion), not to the next focusable control.
        SendTab,
        SendBackTab,
        NewTab,
        CloseTab,
        ClosePane,
        SplitRight,
        SplitDown,
        NextTab,
        PrevTab,
        NextPane,
        PrevPane,
    ]
);

/// Registers terminal shortcuts. They follow VS Code and Windows Terminal
/// where those agree, and stay on Ctrl+Shift so plain Ctrl keys reach the shell.
pub fn init(cx: &mut App) {
    let terminal = Some("Terminal");
    let area = Some("TerminalArea");
    cx.bind_keys([
        KeyBinding::new("tab", SendTab, terminal),
        KeyBinding::new("shift-tab", SendBackTab, terminal),
        KeyBinding::new("ctrl-shift-t", NewTab, area),
        KeyBinding::new("ctrl-shift-w", ClosePane, area),
        KeyBinding::new("ctrl-shift-q", CloseTab, area),
        KeyBinding::new("ctrl-shift-d", SplitRight, area),
        KeyBinding::new("ctrl-shift-e", SplitDown, area),
        KeyBinding::new("ctrl-tab", NextTab, area),
        KeyBinding::new("ctrl-shift-tab", PrevTab, area),
        KeyBinding::new("ctrl-pagedown", NextTab, area),
        KeyBinding::new("ctrl-pageup", PrevTab, area),
        KeyBinding::new("ctrl-alt-right", NextPane, area),
        KeyBinding::new("ctrl-alt-down", NextPane, area),
        KeyBinding::new("ctrl-alt-left", PrevPane, area),
        KeyBinding::new("ctrl-alt-up", PrevPane, area),
    ]);
}
