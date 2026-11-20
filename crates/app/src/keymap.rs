//! Keyboard shortcuts: the built-in ones, and the user's own from the `[keys]`
//! table of `settings.toml`, which replace them command by command.

use std::rc::Rc;

use gpui_kit::component::input::{GoToDefinition, ShowCompletions, ToggleCodeActions};
use gpui_kit::*;

use crate::editor::{RenameSymbol, Save};
use crate::symbols::{GoToProjectSymbol, GoToSymbol};
use crate::lsp::RestartLanguageServers;
use crate::panel::git::{Commit, Fetch, Pull, Push, ShowAllChanges, StageAll};
use crate::term::{
    ClosePane, CloseTab, Find, NewTab, PaneDown, PaneLeft, PaneRight, PaneUp, RenameTab, NextPane, NextTab, PrevPane, PrevTab, SplitDown, SplitRight, ToggleTerminal,
};
use crate::theme::AppState;
use crate::workspace::{
    CommandPalette, NewWindow, OpenFolder, OpenSettings, QuickOpen, RerunTask, RunTask, ShowFiles, ShowGit, ShowProblems, ShowSearch,
    SwitchBranch, SwitchProject, SwitchWorktree, TogglePanel,
};
use wercode_core::i18n::{t, tf};

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
    /// What the command does, in the words of the interface.
    pub title: fn() -> &'static str,
}

macro_rules! shortcut {
    ($command:literal, $context:literal, $action:expr, [$($key:literal),*], $title:expr) => {
        Shortcut { command: $command, context: $context, keys: &[$($key),*], action: || Box::new($action), title: || $title }
    };
}

pub fn shortcuts() -> Vec<Shortcut> {
    vec![
        shortcut!("workspace.command_palette", "Workspace", CommandPalette, ["ctrl-shift-p"], t("View: Command Palette")),
        shortcut!("workspace.quick_open", "Workspace", QuickOpen, ["ctrl-shift-k"], t("File: Go to File\u{2026}")),
        shortcut!("workspace.find_in_files", "Workspace", ShowSearch, ["ctrl-shift-f"], t("Search: Find in Files")),
        shortcut!("workspace.toggle_panel", "Workspace", TogglePanel, ["ctrl-shift-b"], t("View: Toggle Side Panel")),
        shortcut!("workspace.show_files", "Workspace", ShowFiles, [], t("View: Show Files")),
        shortcut!("workspace.show_git", "Workspace", ShowGit, ["ctrl-shift-g"], t("View: Show Git")),
        shortcut!("workspace.show_problems", "Workspace", ShowProblems, ["ctrl-shift-m"], t("View: Show Problems")),
        shortcut!("workspace.restart_language_servers", "Workspace", RestartLanguageServers, [], t("Language Servers: Restart")),
        shortcut!("workspace.open_folder", "Workspace", OpenFolder, ["ctrl-shift-o"], t("Project: Open Folder\u{2026}")),
        shortcut!("workspace.switch_project", "Workspace", SwitchProject, [], t("Project: Switch to Recent\u{2026}")),
        shortcut!("workspace.switch_branch", "Workspace", SwitchBranch, [], t("Git: Switch or Create Branch\u{2026}")),
        shortcut!("workspace.switch_worktree", "Workspace", SwitchWorktree, [], t("Git: Switch or Create Worktree\u{2026}")),
        shortcut!("workspace.new_window", "Workspace", NewWindow, ["ctrl-shift-n"], t("Window: New Window")),
        shortcut!("workspace.settings", "Workspace", OpenSettings, [], t("Preferences: Settings")),
        // X for "execute"; L for the last one again.
        shortcut!("workspace.run_task", "Workspace", RunTask, ["ctrl-shift-x"], t("Tasks: Run\u{2026}")),
        shortcut!("workspace.rerun_task", "Workspace", RerunTask, ["ctrl-shift-l"], t("Tasks: Run the Last One Again")),
        // Up sends commits away, down brings them in, like the arrows on the buttons.
        shortcut!("git.push", "Workspace", Push, ["ctrl-shift-up"], t("Git: Push")),
        shortcut!("git.pull", "Workspace", Pull, ["ctrl-shift-down"], t("Git: Pull")),
        shortcut!("git.fetch", "Workspace", Fetch, ["ctrl-shift-r"], t("Git: Fetch")),
        shortcut!("git.commit", "Workspace", Commit, ["ctrl-shift-enter"], t("Git: Commit")),
        shortcut!("git.stage_all", "Workspace", StageAll, [], t("Git: Stage All Changes")),
        // A for "all changes".
        shortcut!("git.show_all_changes", "Workspace", ShowAllChanges, ["ctrl-shift-a"], t("Git: Show All Changes")),
        shortcut!("terminal.toggle", "TerminalArea", ToggleTerminal, ["ctrl-shift-j"], t("Terminal: Show or Hide")),
        shortcut!("terminal.new_tab", "TerminalArea", NewTab, ["ctrl-shift-t"], t("Terminal: New Tab")),
        shortcut!("terminal.close_tab", "TerminalArea", CloseTab, ["ctrl-shift-q"], t("Terminal: Close Tab")),
        shortcut!("terminal.close_pane", "TerminalArea", ClosePane, ["ctrl-shift-w"], t("Terminal: Close Pane")),
        shortcut!("terminal.split_right", "TerminalArea", SplitRight, ["ctrl-shift-d"], t("Terminal: Split Right")),
        shortcut!("terminal.split_down", "TerminalArea", SplitDown, ["ctrl-shift-e"], t("Terminal: Split Down")),
        shortcut!("terminal.next_tab", "TerminalArea", NextTab, ["ctrl-tab", "ctrl-pagedown"], t("Terminal: Next Tab")),
        shortcut!("terminal.previous_tab", "TerminalArea", PrevTab, ["ctrl-shift-tab", "ctrl-pageup"], t("Terminal: Previous Tab")),
        // The arrows go to the terminal on that side; round the circle is in the palette.
        shortcut!("terminal.pane_left", "TerminalArea", PaneLeft, ["ctrl-alt-left"], t("Terminal: Go to the Pane on the Left")),
        shortcut!("terminal.pane_right", "TerminalArea", PaneRight, ["ctrl-alt-right"], t("Terminal: Go to the Pane on the Right")),
        shortcut!("terminal.pane_up", "TerminalArea", PaneUp, ["ctrl-alt-up"], t("Terminal: Go to the Pane Above")),
        shortcut!("terminal.pane_down", "TerminalArea", PaneDown, ["ctrl-alt-down"], t("Terminal: Go to the Pane Below")),
        shortcut!("terminal.next_pane", "TerminalArea", NextPane, [], t("Terminal: Next Pane")),
        shortcut!("terminal.previous_pane", "TerminalArea", PrevPane, [], t("Terminal: Previous Pane")),
        // Ctrl+Shift+F finds in the project's files; S for "search" here.
        shortcut!("terminal.find", "Terminal", Find, ["ctrl-shift-s"], t("Terminal: Find\u{2026}")),
        shortcut!("terminal.rename_tab", "TerminalArea", RenameTab, [], t("Terminal: Rename Tab\u{2026}")),
        // Plain Ctrl+S is safe here: the keyboard is in a text editor, not in a shell.
        shortcut!("editor.save", "EditorView", Save, ["ctrl-s"], t("File: Save")),
        shortcut!("editor.go_to_definition", "EditorView", GoToDefinition, ["f12"], t("Editor: Go to Definition")),
        shortcut!("editor.show_completions", "EditorView", ShowCompletions, ["ctrl-space"], t("Editor: Show Completions")),
        shortcut!("editor.quick_fix", "EditorView", ToggleCodeActions, ["ctrl-."], t("Editor: Quick Fix\u{2026}")),
        shortcut!("editor.rename", "EditorView", RenameSymbol, ["f2"], t("Editor: Rename Symbol\u{2026}")),
        // R as in Sublime; Ctrl+Shift+O opens a folder here.
        shortcut!("editor.go_to_symbol", "EditorView", GoToSymbol, ["ctrl-r"], t("Go to Symbol in File\u{2026}")),
        shortcut!("workspace.go_to_project_symbol", "Workspace", GoToProjectSymbol, [], t("Go to Symbol in Project\u{2026}")),
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
        .map(|name| tf("[keys] names a command that does not exist: {name}", &[("name", name)]))
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
                problems.push(tf(
                    "[keys] has a shortcut that cannot be read: {key} (for {command})",
                    &[("key", &key), ("command", &shortcut.command)],
                ));
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

/// A key press as a shortcut is written: `ctrl-shift-y`. `None` for a
/// modifier pressed alone.
pub fn stroke(keystroke: &Keystroke) -> Option<String> {
    let key = keystroke.key.to_lowercase();
    if key.is_empty() || matches!(key.as_str(), "shift" | "control" | "ctrl" | "alt" | "platform" | "cmd" | "super" | "win" | "function" | "fn") {
        return None;
    }
    let m = &keystroke.modifiers;
    let mut out = String::new();
    for (on, name) in [(m.control, "ctrl-"), (m.alt, "alt-"), (m.shift, "shift-"), (m.platform, if cfg!(target_os = "macos") { "cmd-" } else { "super-" })] {
        if on {
            out.push_str(name);
        }
    }
    out.push_str(&key);
    Some(out)
}

/// Whether a key press can be a shortcut of the whole window without
/// taking a letter away from typing: it needs Ctrl, Alt or the system
/// key, except for F1 to F24.
pub fn usable(stroke: &str) -> bool {
    let key = stroke.rsplit('-').next().unwrap_or_default();
    let function = key.strip_prefix('f').is_some_and(|n| n.parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)));
    function || ["ctrl-", "alt-", "cmd-", "super-"].iter().any(|m| stroke.contains(m))
}

/// The commands, other than `command`, that a shortcut is bound to now.
pub fn taken_by(stroke: &str, command: &str, cx: &App) -> Vec<&'static str> {
    let all = shortcuts();
    cx.try_global::<Applied>()
        .iter()
        .flat_map(|applied| &applied.bound)
        .filter(|(key, _, name)| key == stroke && *name != command)
        .filter_map(|(_, _, name)| all.iter().find(|s| s.command == *name).map(|s| (s.title)()))
        .collect()
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

/// A command as the settings screen lists it.
pub struct Listed {
    pub command: &'static str,
    pub title: &'static str,
    /// Its shortcuts now, printed like keys.
    pub keys: Vec<String>,
    /// The `[keys]` table changes them.
    pub changed: bool,
}

/// Every command with the shortcuts it has now, for the settings screen.
pub fn listing(cx: &App) -> Vec<Listed> {
    let applied = cx.try_global::<Applied>();
    let overrides = cx.try_global::<AppState>().map(|state| &state.settings.keys);
    shortcuts()
        .iter()
        .map(|shortcut| {
            let keys = applied
                .iter()
                .flat_map(|applied| &applied.bound)
                .filter(|(_, _, command)| *command == shortcut.command)
                .map(|(key, _, _)| label(key))
                .collect();
            let changed = overrides.is_some_and(|keys| keys.contains_key(shortcut.command));
            Listed { command: shortcut.command, title: (shortcut.title)(), keys, changed }
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
    use super::{label, normalized, shortcuts, stroke, usable};
    use gpui_kit::Keystroke;

    #[test]
    fn shortcuts_are_printed_like_keys() {
        assert_eq!(label("ctrl-shift-t"), "Ctrl+Shift+T");
        assert_eq!(label("ctrl-k ctrl-pagedown"), "Ctrl+K Ctrl+Pagedown");
        assert_eq!(normalized(" Ctrl+Shift+Y "), "ctrl-shift-y");
        assert_eq!(normalized("ctrl-k Ctrl++"), "ctrl-k ctrl-+");
    }

    #[test]
    fn a_key_press_is_written_as_a_shortcut() {
        let press = |text: &'static str| stroke(&Keystroke::parse(text).unwrap());
        assert_eq!(press("ctrl-shift-y").as_deref(), Some("ctrl-shift-y"));
        assert_eq!(press("alt-shift-f2").as_deref(), Some("alt-shift-f2"));
        assert_eq!(press("shift-enter").as_deref(), Some("shift-enter"));
        let alone = Keystroke { key: "shift".into(), ..Keystroke::parse("shift-a").unwrap() };
        assert_eq!(stroke(&alone), None);
        assert!(usable("ctrl-shift-y") && usable("f5") && usable("shift-f12") && usable("alt-x"));
        assert!(!usable("y") && !usable("shift-y") && !usable("enter") && !usable("f99"));
    }

    #[test]
    fn every_command_is_named_once() {
        let all = shortcuts();
        for (index, shortcut) in all.iter().enumerate() {
            assert!(all[..index].iter().all(|other| other.command != shortcut.command), "{}", shortcut.command);
        }
    }
}
