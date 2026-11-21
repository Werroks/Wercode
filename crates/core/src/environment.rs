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

/// What the login shell prints around its `PATH`, so that whatever its
/// startup files print as well can be told apart.
#[cfg_attr(not(unix), allow(dead_code))]
const PATH_MARK: &str = "_WERCODE_PATH_";

/// The `PATH` between the marks in what the login shell printed.
#[cfg_attr(not(unix), allow(dead_code))]
fn path_between_marks(output: &str) -> Option<OsString> {
    let mut parts = output.split(PATH_MARK);
    parts.next()?;
    let path = parts.next()?;
    parts.next()?;
    (!path.trim().is_empty()).then(|| OsString::from(path.trim()))
}

/// The `PATH` the user's login shell sets up, for finding programs such as
/// language servers. A window started from the Dock on macOS (or from a
/// desktop menu on Linux) gets only the system folders: `~/.cargo/bin`,
/// Homebrew and nvm are added by the shell's startup files, which only a
/// terminal runs. Asked once, off the UI thread, and only when Wercode was
/// not started from a terminal (whose `PATH` it already has). `None` on
/// Windows, where programs register their folders for every process.
pub fn login_path() -> Option<&'static std::ffi::OsStr> {
    static PATH: std::sync::OnceLock<Option<OsString>> = std::sync::OnceLock::new();
    PATH.get_or_init(ask_login_shell).as_deref()
}

#[cfg(unix)]
fn ask_login_shell() -> Option<OsString> {
    use std::io::{IsTerminal as _, Read as _};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    if std::io::stdin().is_terminal() {
        return None;
    }
    let shell = std::env::var_os("SHELL").filter(|shell| !shell.is_empty())?;
    // `-i` too: nvm and many others are set up in `.zshrc` and `.bashrc`, which
    // only an interactive shell reads. fish joins a quoted `$PATH` with colons.
    let script = format!("printf '{PATH_MARK}%s{PATH_MARK}' \"$PATH\"");
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        let _ = stdout.read_to_string(&mut output);
        let _ = sender.send(output);
    });
    // A startup file that waits for something must not hold the servers up for long.
    let output = receiver.recv_timeout(Duration::from_secs(5));
    let _ = child.kill();
    let _ = child.wait();
    path_between_marks(&output.ok()?)
}

#[cfg(not(unix))]
fn ask_login_shell() -> Option<OsString> {
    None
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

    #[test]
    fn the_login_shells_path_is_read_between_the_marks() {
        let printed = "Welcome back!\n_WERCODE_PATH_/opt/homebrew/bin:/usr/bin_WERCODE_PATH_";
        assert_eq!(path_between_marks(printed), Some(OsString::from("/opt/homebrew/bin:/usr/bin")));
        assert_eq!(path_between_marks("no marks at all"), None);
        assert_eq!(path_between_marks("_WERCODE_PATH_/usr/bin"), None);
        assert_eq!(path_between_marks("_WERCODE_PATH__WERCODE_PATH_"), None);
    }

    #[cfg(windows)]
    #[test]
    fn names_are_matched_without_case_on_windows() {
        assert_eq!(left(&["ClaudeCode", "No_Color"]), ["ClaudeCode", "No_Color"]);
    }
}
