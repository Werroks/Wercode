//! User settings, stored as `settings.toml` in the platform config directory.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub font_family: String,
    pub font_size: f32,
    pub line_height: f32,
    /// "auto", a program name, or a path to an executable.
    pub shell: String,
    pub shell_args: Vec<String>,
    /// Let the shell report its working directory, so splits open where you
    /// are and relative paths in output can be clicked.
    pub shell_integration: bool,
    /// Ask before closing a terminal that is still running a program.
    pub confirm_close: bool,
    pub cursor_blink: bool,
    pub scrollback: usize,
    pub copy_on_select: bool,
    /// Width of the side panel on first launch; later the dragged width is remembered.
    pub panel_width: f32,
    /// Which side the files/search/git panel sits on: "left" or "right".
    pub panel_side: String,
}

impl Settings {
    /// Anything other than "right" means left, so a typo cannot hide the panel.
    pub fn panel_on_right(&self) -> bool {
        self.panel_side.eq_ignore_ascii_case("right")
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "black".into(),
            font_family: default_font().into(),
            font_size: 14.0,
            line_height: 1.2,
            shell: "auto".into(),
            shell_args: Vec::new(),
            shell_integration: true,
            confirm_close: true,
            cursor_blink: true,
            scrollback: 10_000,
            copy_on_select: false,
            panel_width: 300.0,
            panel_side: "left".into(),
        }
    }
}

fn default_font() -> &'static str {
    if cfg!(windows) {
        "Cascadia Mono"
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "DejaVu Sans Mono"
    }
}

pub fn config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "wercode").map(|d| d.config_dir().to_path_buf())
}

pub fn settings_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("settings.toml"))
}

/// Loads settings. A missing file gives defaults; a broken file gives defaults
/// plus a message for the user, and never stops the app from starting.
pub fn load() -> (Settings, Option<String>) {
    let Some(path) = settings_path() else {
        return (Settings::default(), None);
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<Settings>(&text) {
            Ok(s) => (s, None),
            Err(e) => (
                Settings::default(),
                Some(format!("{}: {}", path.display(), e)),
            ),
        },
        Err(_) => (Settings::default(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_keeps_defaults() {
        let s: Settings = toml::from_str("font_size = 16").unwrap();
        assert_eq!(s.font_size, 16.0);
        assert_eq!(s.theme, "black");
        assert_eq!(s.scrollback, 10_000);
        assert!(!s.panel_on_right());
        let s: Settings = toml::from_str("panel_side = \"Right\"").unwrap();
        assert!(s.panel_on_right());
    }
}
