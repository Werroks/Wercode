//! Telling that a terminal runs a command-line AI agent (Claude Code, Codex
//! and the like), so the prompt box can open for it.

use std::path::Path;

/// Program names of the agents, as the system lists the running programs.
/// Codex ships a binary named after the platform (`codex-x86_64-...`).
const AGENTS: &[&str] = &[
    "claude", "codex", "gemini", "qwen", "aider", "opencode", "cursor-agent", "copilot", "goose", "crush", "amp",
];

/// True when `program` (a name from [`crate::process::descendant_names`]) is
/// one of the agents.
pub fn is_agent(program: &str) -> bool {
    AGENTS.iter().any(|agent| {
        program == *agent || program.strip_prefix(agent).is_some_and(|rest| rest.starts_with(['-', '.', '_']))
    })
}

/// True when a terminal title is the one an agent gives the window as it
/// starts. Agents installed with npm run as `node`, so their program name
/// says nothing; Claude Code then still names itself in the title.
pub fn title_names_agent(title: &str) -> bool {
    let title = title.to_lowercase();
    ["claude code", "codex", "gemini cli"].iter().any(|name| title.contains(name))
}

/// True for the kinds of picture the agents take: PNG, JPEG, GIF and WebP.
pub fn is_picture(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| ["png", "jpg", "jpeg", "gif", "webp"].contains(&extension.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_are_known_by_their_program() {
        for name in ["claude", "codex", "codex-x86_64-pc-windows-msvc", "gemini", "aider", "cursor-agent"] {
            assert!(is_agent(name), "{name}");
        }
        for name in ["node", "pwsh", "cargo", "claudette", "codexed", "git", "ampersand"] {
            assert!(!is_agent(name), "{name}");
        }
    }

    #[test]
    fn titles_that_name_an_agent() {
        assert!(title_names_agent("\u{2733} Claude Code"));
        assert!(title_names_agent("Codex"));
        assert!(!title_names_agent("pwsh \u{b7} weather"));
        assert!(!title_names_agent("C:\\Program Files\\PowerShell\\7\\pwsh.exe"));
    }

    #[test]
    fn pictures_by_extension() {
        assert!(is_picture(Path::new("shot.PNG")));
        assert!(is_picture(Path::new("C:\\Users\\me\\photo.jpeg")));
        assert!(!is_picture(Path::new("notes.txt")));
        assert!(!is_picture(Path::new("png")));
    }
}
