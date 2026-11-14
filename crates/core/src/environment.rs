//! Environment variables Wercode drops when it starts.
//!
//! Wercode is often started from inside another program: the shell of an AI
//! agent such as Claude Code, or the terminal built into an editor. Those
//! mark the processes they start, and every shell and git command Wercode
//! runs would inherit the marks. A tab in Wercode belongs to the person at
//! the keyboard, so the marks go before anything else starts.

use std::ffi::OsString;

/// What Claude Code puts on every command it runs. Inherited, they make a
/// `claude` started in a Wercode tab take itself for a child of that session:
/// it stops saving its transcript and reports to the wrong parent.
const AGENT_MARKS: &[&str] = &[
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_INVOKED_SKILLS",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_EFFORT",
    "CLAUDE_PID",
];

/// What an agent's shell adds so that a program, not a person, reads the
/// output: no colours, no password prompts, no editor for commit messages.
/// Dropped only next to the agent's marks; anywhere else they are the user's
/// own choice.
const AGENT_SHELL_SETTINGS: &[&str] =
    &["GIT_EDITOR", "GIT_TERMINAL_PROMPT", "NO_COLOR", "PYTHONIOENCODING", "TRACEPARENT"];

/// How the terminal of VS Code or Cursor answers git's password questions:
/// through a pipe into the editor, which closes when the editor does.
const EDITOR_GIT_PROMPT: &[&str] = &[
    "GIT_ASKPASS",
    "VSCODE_GIT_ASKPASS_EXTRA_ARGS",
    "VSCODE_GIT_ASKPASS_MAIN",
    "VSCODE_GIT_ASKPASS_NODE",
    "VSCODE_GIT_IPC_AUTH_TOKEN",
    "VSCODE_GIT_IPC_HANDLE",
];

/// Names of variables are not case-sensitive on Windows.
fn same(name: &str, wanted: &str) -> bool {
    if cfg!(windows) { name.eq_ignore_ascii_case(wanted) } else { name == wanted }
}

/// Which of `names` were left by the program that started Wercode.
pub fn leftovers(names: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let names: Vec<OsString> = names.into_iter().collect();
    let has = |wanted: &str| names.iter().any(|name| name.to_str().is_some_and(|name| same(name, wanted)));
    let from_agent = has("CLAUDECODE") || has("CLAUDE_CODE_CHILD_SESSION");
    let from_editor = has("VSCODE_GIT_ASKPASS_MAIN");
    names
        .into_iter()
        .filter(|name| {
            let Some(name) = name.to_str() else { return false };
            let listed = |list: &[&str]| list.iter().any(|wanted| same(name, wanted));
            listed(AGENT_MARKS)
                || (from_agent && listed(AGENT_SHELL_SETTINGS))
                || (from_editor && listed(EDITOR_GIT_PROMPT))
        })
        .collect()
}

/// Removes the [`leftovers`] from this process, so nothing it starts sees them.
///
/// # Safety
///
/// Changes the environment of the whole process. Call it before any other
/// thread has started.
pub unsafe fn drop_leftovers() {
    for name in leftovers(std::env::vars_os().map(|(name, _)| name)) {
        // SAFETY: the caller promises that no other thread is running yet.
        unsafe { std::env::remove_var(name) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn left(names: &[&str]) -> Vec<String> {
        leftovers(names.iter().map(OsString::from)).into_iter().map(|n| n.into_string().unwrap()).collect()
    }

    #[test]
    fn claude_code_marks_go_and_take_its_shell_settings_with_them() {
        let started_by_agent = [
            "PATH",
            "CLAUDECODE",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ID",
            "AI_AGENT",
            "NO_COLOR",
            "GIT_EDITOR",
            "GIT_TERMINAL_PROMPT",
            "COLORTERM",
            "CLAUDE_CONFIG_DIR",
        ];
        assert_eq!(
            left(&started_by_agent),
            [
                "CLAUDECODE",
                "CLAUDE_CODE_CHILD_SESSION",
                "CLAUDE_CODE_SESSION_ID",
                "AI_AGENT",
                "NO_COLOR",
                "GIT_EDITOR",
                "GIT_TERMINAL_PROMPT"
            ]
        );
    }

    #[test]
    fn the_users_own_choices_stay_when_no_agent_started_wercode() {
        assert!(left(&["PATH", "NO_COLOR", "GIT_EDITOR", "GIT_ASKPASS", "CLAUDE_CONFIG_DIR"]).is_empty());
    }

    #[test]
    fn an_editors_password_pipe_goes() {
        let from_cursor = ["PATH", "GIT_ASKPASS", "VSCODE_GIT_ASKPASS_MAIN", "VSCODE_GIT_IPC_HANDLE", "NO_COLOR"];
        assert_eq!(left(&from_cursor), ["GIT_ASKPASS", "VSCODE_GIT_ASKPASS_MAIN", "VSCODE_GIT_IPC_HANDLE"]);
    }

    #[cfg(windows)]
    #[test]
    fn names_are_matched_without_case_on_windows() {
        assert_eq!(left(&["ClaudeCode", "No_Color"]), ["ClaudeCode", "No_Color"]);
    }
}
