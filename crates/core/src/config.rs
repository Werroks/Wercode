//! User settings, stored as `settings.toml` in the platform config directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The shortcuts of one command: a single one, or several that do the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Keys {
    One(String),
    Many(Vec<String>),
}

impl Keys {
    /// The shortcuts as written, without empty ones. An empty list means the
    /// command has no shortcut at all.
    pub fn list(&self) -> Vec<&str> {
        let all: Vec<&str> = match self {
            Keys::One(key) => vec![key.as_str()],
            Keys::Many(keys) => keys.iter().map(String::as_str).collect(),
        };
        all.into_iter().map(str::trim).filter(|key| !key.is_empty()).collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub font_family: String,
    pub font_size: f32,
    /// Weight of the terminal text: 400 is regular, 700 bold. Fonts without
    /// that weight fall back to the nearest one they have.
    pub font_weight: f32,
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
    /// How files and terminals share the window: "tabs" puts them in one row
    /// of tabs, "terminal_below" puts files above a terminal panel.
    pub layout: String,
    /// Shortcuts that replace the built-in ones, by command name:
    /// `"terminal.new_tab" = "ctrl-shift-y"`. Written by hand in the `[keys]`
    /// table; the settings screen lists the names.
    pub keys: BTreeMap<String, Keys>,
}

impl Settings {
    /// Anything other than "terminal_below" means one row of tabs.
    pub fn terminal_below(&self) -> bool {
        self.layout.eq_ignore_ascii_case("terminal_below")
    }

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
            font_weight: 500.0,
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
            layout: "tabs".into(),
            keys: BTreeMap::new(),
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
    match settings_path() {
        Some(path) => load_from(&path),
        None => (Settings::default(), None),
    }
}

pub fn load_from(path: &Path) -> (Settings, Option<String>) {
    match std::fs::read_to_string(path) {
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

fn item(setting: &toml::Value) -> Option<toml_edit::Item> {
    use toml_edit::value;
    Some(match setting {
        toml::Value::String(s) => value(s.as_str()),
        toml::Value::Integer(i) => value(*i),
        toml::Value::Float(f) => value(*f),
        toml::Value::Boolean(b) => value(*b),
        toml::Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for entry in items {
                array.push(entry.as_str()?);
            }
            value(array)
        }
        _ => return None,
    })
}

/// Writes the settings into the file, changing only what differs from it.
/// Comments and the order of the lines stay as the user left them, and a
/// setting that still has its default value is not added.
pub fn save_to(path: &Path, settings: &Settings) -> Result<(), String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    // A file that does not parse is the user's to fix; rewriting it would lose their work.
    let mut doc: toml_edit::DocumentMut =
        text.parse().map_err(|e| format!("{} has a mistake and was not changed: {e}", path.display()))?;
    let on_disk: Settings = toml::from_str(&text).unwrap_or_default();
    let table = |s: &Settings| match toml::Value::try_from(s) {
        Ok(toml::Value::Table(table)) => table,
        _ => Default::default(),
    };
    let (wanted, current) = (table(settings), table(&on_disk));
    for (key, value) in &wanted {
        if current.get(key) != Some(value)
            && let Some(mut item) = item(value)
        {
            // A comment at the end of the line belongs to the setting, not to its old value.
            if let (Some(old), Some(new)) = (doc.get(key).and_then(|i| i.as_value()), item.as_value_mut()) {
                *new.decor_mut() = old.decor().clone();
            }
            doc[key.as_str()] = item;
        }
    }
    let failed = |e: std::io::Error| format!("Could not write {}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    std::fs::write(path, doc.to_string()).map_err(failed)
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

    #[test]
    fn saving_keeps_comments_and_writes_only_what_changed() {
        let dir = std::env::temp_dir().join(format!("wercode-config-{}", std::process::id()));
        let path = dir.join("settings.toml");
        let _ = std::fs::remove_dir_all(&dir);

        // No file yet: only the changed setting is written.
        let mut settings = Settings { theme: "paper".into(), ..Settings::default() };
        save_to(&path, &settings).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "theme = \"paper\"");

        std::fs::write(&path, "# my notes
font_size = 16 # big
theme = \"paper\"
").unwrap();
        let (loaded, error) = load_from(&path);
        assert!(error.is_none());
        settings = loaded;
        settings.font_size = 18.0;
        settings.shell = "cmd.exe".into();
        settings.shell_args = vec!["/K".into()];
        settings.cursor_blink = false;
        save_to(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# my notes
font_size = 18.0 # big
theme = \"paper\"
"), "{text}");
        let (again, error) = load_from(&path);
        assert!(error.is_none());
        assert_eq!((again.font_size, again.shell.as_str(), again.cursor_blink), (18.0, "cmd.exe", false));
        assert_eq!(again.shell_args, ["/K"]);
        assert!(!text.contains("scrollback"), "defaults are not written: {text}");

        // Shortcuts are written by hand; saving other settings leaves them alone.
        std::fs::write(&path, "theme = \"paper\"

[keys]
\"terminal.new_tab\" = \"ctrl-shift-y\" # mine
\"terminal.next_tab\" = [\"ctrl-tab\", \"\"]
\"workspace.new_window\" = \"\"
").unwrap();
        let (mut with_keys, error) = load_from(&path);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(with_keys.keys["terminal.new_tab"].list(), ["ctrl-shift-y"]);
        assert_eq!(with_keys.keys["terminal.next_tab"].list(), ["ctrl-tab"]);
        assert!(with_keys.keys["workspace.new_window"].list().is_empty());
        with_keys.font_size = 20.0;
        save_to(&path, &with_keys).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"terminal.new_tab\" = \"ctrl-shift-y\" # mine"), "{text}");
        let (again, error) = load_from(&path);
        assert!(error.is_none(), "{error:?}: {text}");
        assert_eq!((again.font_size, again.keys.len()), (20.0, 3));

        // A broken file is reported and left exactly as it was.
        std::fs::write(&path, "theme = ").unwrap();
        assert!(save_to(&path, &settings).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theme = ");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
