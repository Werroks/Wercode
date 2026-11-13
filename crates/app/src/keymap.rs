//! Keyboard shortcuts: the built-in ones, and the user's own from the `[keys]`
//! table of `settings.toml`, which replace them command by command.

use std::rc::Rc;

use gpui_kit::*;

use crate::editor::Save;
use crate::term::{ClosePane, CloseTab, NewTab, NextPane, NextTab, PrevPane, PrevTab, SplitDown, SplitRight};
use crate::theme::AppState;
use crate::workspace::{
    CommandPalette, NewWindow, OpenFolder, OpenSettings, QuickOpen, ShowFiles, ShowGit, ShowSearch, SwitchBranch,
    SwitchProject, TogglePanel,
};

/// A command that can have a shortcut.
pub struct Shortcut {
    /// The name used in `[keys]`.
    pub command: &'static str,
    /// The part of the window the shortcut works in.
    context: &'static str,
    /// The built-in shortcuts. App shortcuts sit on Ctrl+Shift: plain Ctrl
    /// keys belong to the shell and to the programs running in it.
    keys: &'static [&'static str],
    action: fn() -> Box<dyn Action>,
}

macro_rules! shortcut {
    ($command:literal, $context:literal, $action:expr, [$($key:literal),*]) => {
        Shortcut { command: $command, context: $context, keys: &[$($key),*], action: || Box::new($action) }
    };
}

pub fn shortcuts() -> Vec<Shortcut> {
    vec![
        shortcut!("workspace.command_palette", "Workspace", CommandPalette, ["ctrl-shift-p"]),
        shortcut!("workspace.quick_open", "Workspace", QuickOpen, ["ctrl-shift-k"]),
        shortcut!("workspace.find_in_files", "Workspace", ShowSearch, ["ctrl-shift-f"]),
        shortcut!("workspace.toggle_panel", "Workspace", TogglePanel, ["ctrl-shift-b"]),
        shortcut!("workspace.show_files", "Workspace", ShowFiles, []),
        shortcut!("workspace.show_git", "Workspace", ShowGit, ["ctrl-shift-g"]),
        shortcut!("workspace.open_folder", "Workspace", OpenFolder, ["ctrl-shift-o"]),
        shortcut!("workspace.switch_project", "Workspace", SwitchProject, []),
        shortcut!("workspace.switch_branch", "Workspace", SwitchBranch, []),
        shortcut!("workspace.new_window", "Workspace", NewWindow, ["ctrl-shift-n"]),
        shortcut!("workspace.settings", "Workspace", OpenSettings, []),
        shortcut!("terminal.new_tab", "TerminalArea", NewTab, ["ctrl-shift-t"]),
        shortcut!("terminal.close_tab", "TerminalArea", CloseTab, ["ctrl-shift-q"]),
        shortcut!("terminal.close_pane", "TerminalArea", ClosePane, ["ctrl-shift-w"]),
        shortcut!("terminal.split_right", "TerminalArea", SplitRight, ["ctrl-shift-d"]),
        shortcut!("terminal.split_down", "TerminalArea", SplitDown, ["ctrl-shift-e"]),
        shortcut!("terminal.next_tab", "TerminalArea", NextTab, ["ctrl-tab", "ctrl-pagedown"]),
        shortcut!("terminal.previous_tab", "TerminalArea", PrevTab, ["ctrl-shift-tab", "ctrl-pageup"]),
        shortcut!("terminal.next_pane", "TerminalArea", NextPane, ["ctrl-alt-right", "ctrl-alt-down"]),
        shortcut!("terminal.previous_pane", "TerminalArea", PrevPane, ["ctrl-alt-left", "ctrl-alt-up"]),
        // Plain Ctrl+S is safe here: the keyboard is in a text editor, not in a shell.
        shortcut!("editor.save", "EditorView", Save, ["ctrl-s"]),
    ]
}

/// What is bound right now, and what in `[keys]` could not be used.
#[derive(Default)]
struct Applied {
    /// Shortcut, context and command, in the order of [`shortcuts`].
    bound: Vec<(String, &'static str, &'static str)>,
    problems: Vec<String>,
}

impl Global for Applied {}

fn is_shortcut(keys: &str, cx: &App) -> bool {
    !keys.trim().is_empty() && KeyBinding::load(keys, Box::new(NoAction), None, false, None, cx).is_ok()
}

/// A shortcut from the settings in the form the key bindings want: lower
/// case, with `-` between the keys. `Ctrl+Shift+Y` is accepted as well.
fn normalized(keys: &str) -> String {
    let mut out = String::with_capacity(keys.len());
    let mut letters = keys.trim().chars().peekable();
    while let Some(letter) = letters.next() {
        // A plus at the end of a shortcut is the plus key itself.
        let joins = letter == '+' && letters.peek().is_some_and(|next| !next.is_whitespace());
        out.extend(if joins { '-'.to_lowercase() } else { letter.to_lowercase() });
    }
    out
}

/// Binds the shortcuts the settings ask for. Called whenever the settings
/// change; shortcuts that are no longer wanted stop working.
pub fn apply(cx: &mut App) {
    let overrides = cx.global::<AppState>().settings.keys.clone();
    let all = shortcuts();
    let mut problems: Vec<String> = overrides
        .keys()
        .filter(|name| !all.iter().any(|s| s.command == name.as_str()))
        .map(|name| format!("[keys] names a command that does not exist: {name}"))
        .collect();

    let mut wanted: Vec<(String, usize)> = Vec::new();
    for (index, shortcut) in all.iter().enumerate() {
        let keys: Vec<String> = match overrides.get(shortcut.command) {
            Some(keys) => keys.list().into_iter().map(normalized).collect(),
            None => shortcut.keys.iter().map(|key| key.to_string()).collect(),
        };
        for key in keys {
            if is_shortcut(&key, cx) {
                wanted.push((key, index));
            } else {
                problems.push(format!("[keys] has a shortcut that cannot be read: {key} (for {})", shortcut.command));
            }
        }
    }

    let bound: Vec<_> =
        wanted.iter().map(|(key, index)| (key.clone(), all[*index].context, all[*index].command)).collect();
    let previous = cx.try_global::<Applied>().map(|applied| applied.bound.clone());
    if previous.as_ref() != Some(&bound) {
        let within = |context: &str| KeyBindingContextPredicate::parse(context).ok().map(Rc::new);
        let mut bindings = Vec::new();
        // Bindings cannot be taken back one by one. An empty binding on top of
        // an old one switches it off; the new ones go on top of that.
        for (key, context, _) in previous.iter().flatten() {
            bindings.extend(KeyBinding::load(key, Box::new(NoAction), within(context), false, None, cx));
        }
        for (key, index) in &wanted {
            let shortcut = &all[*index];
            bindings.extend(KeyBinding::load(key, (shortcut.action)(), within(shortcut.context), false, None, cx));
        }
        cx.bind_keys(bindings);
    }
    cx.set_global(Applied { bound, problems });
}

/// What in `[keys]` could not be used, as sentences for the settings screen.
pub fn problems(cx: &App) -> Vec<String> {
    cx.try_global::<Applied>().map(|applied| applied.problems.clone()).unwrap_or_default()
}

/// `ctrl-shift-t` the way it is printed on keys: `Ctrl+Shift+T`.
pub fn label(keys: &str) -> String {
    let stroke = |stroke: &str| {
        let parts: Vec<String> = stroke
            .split('-')
            .map(|part| {
                let mut letters = part.chars();
                match letters.next() {
                    Some(first) => first.to_uppercase().chain(letters).collect(),
                    None => String::new(),
                }
            })
            .collect();
        parts.join("+")
    };
    keys.split_whitespace().map(stroke).collect::<Vec<_>>().join(" ")
}

/// Every command with the shortcuts it has now, for the settings screen.
pub fn listing(cx: &App) -> Vec<(&'static str, Vec<String>)> {
    let applied = cx.try_global::<Applied>();
    shortcuts()
        .iter()
        .map(|shortcut| {
            let keys = applied
                .iter()
                .flat_map(|applied| &applied.bound)
                .filter(|(_, _, command)| *command == shortcut.command)
                .map(|(key, _, _)| label(key))
                .collect();
            (shortcut.command, keys)
        })
        .collect()
}

/// The first shortcut of a command, ready to show next to its name.
pub fn label_for(command: &str, cx: &App) -> Option<String> {
    let applied = cx.try_global::<Applied>()?;
    applied.bound.iter().find(|(_, _, name)| *name == command).map(|(key, _, _)| label(key))
}

#[cfg(test)]
mod tests {
    use super::{label, normalized, shortcuts};

    #[test]
    fn shortcuts_are_printed_like_keys() {
        assert_eq!(label("ctrl-shift-t"), "Ctrl+Shift+T");
        assert_eq!(label("ctrl-k ctrl-pagedown"), "Ctrl+K Ctrl+Pagedown");
        assert_eq!(normalized(" Ctrl+Shift+Y "), "ctrl-shift-y");
        assert_eq!(normalized("ctrl-k Ctrl++"), "ctrl-k ctrl-+");
    }

    #[test]
    fn every_command_is_named_once() {
        let all = shortcuts();
        for (index, shortcut) in all.iter().enumerate() {
            assert!(all[..index].iter().all(|other| other.command != shortcut.command), "{}", shortcut.command);
        }
    }
}
