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
        /// Shows the terminal and puts the keyboard in it, or puts it away.
        ToggleTerminal,
    ]
);

/// Keeps Tab for the shell. The shortcuts for tabs and splits are bound with
/// the others, in `keymap`, because the user can change them.
pub fn init(cx: &mut App) {
    let terminal = Some("Terminal");
    cx.bind_keys([
        KeyBinding::new("tab", SendTab, terminal),
        KeyBinding::new("shift-tab", SendBackTab, terminal),
    ]);
}
