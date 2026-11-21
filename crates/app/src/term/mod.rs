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
        /// Lists the tabs, the one used last first, and picks the one before
        /// this; held down with Ctrl, further presses go down the list.
        SwitchTab,
        /// The same list, going up from the bottom.
        SwitchTabBack,
        NextPane,
        PrevPane,
        /// The keyboard goes to the terminal on that side of this one.
        PaneLeft,
        PaneRight,
        PaneUp,
        PaneDown,
        /// Shows the terminal and puts the keyboard in it, or puts it away.
        ToggleTerminal,
        /// Looks for text in the output of the terminal with the keyboard.
        Find,
        /// Gives the terminal tab a name of its own.
        RenameTab,
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
