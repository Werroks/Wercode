//! What the app remembers between launches, stored as `state.json` next to
//! the settings. Unlike settings, the user never edits this file by hand.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many recent projects are kept.
const MAX_RECENT: usize = 15;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Opened folders, most recent first.
    pub recent: Vec<PathBuf>,
    /// Whether the side panel is shown.
    pub panel_open: bool,
    /// Which side panel is selected: "files", "search" or "git".
    pub panel: String,
    /// Width the side panel was dragged to; `None` until the user resizes it.
    pub panel_width: Option<f32>,
    /// Window position and size when not maximized: x, y, width, height.
    pub window: Option<[f32; 4]>,
    pub maximized: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            recent: Vec::new(),
            panel_open: true,
            panel: "files".into(),
            panel_width: None,
            window: None,
            maximized: false,
        }
    }
}

impl State {
    /// Moves `path` to the front of the recent list.
    pub fn touch_recent(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(MAX_RECENT);
    }

    /// The most recent project that still exists on disk.
    pub fn last_project(&self) -> Option<PathBuf> {
        self.recent.iter().find(|p| p.is_dir()).cloned()
    }
}

pub fn state_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("state.json"))
}

/// Loads the state. A missing or damaged file gives the defaults: losing the
/// recent list must never stop the app from starting.
pub fn load_from(path: &Path) -> State {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Writes the state through a temporary file, so a crash mid-write cannot
/// leave a half-written file behind.
pub fn save_to(path: &Path, state: &State) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    std::fs::rename(&tmp, path)
}

pub fn load() -> State {
    state_path().map(|p| load_from(&p)).unwrap_or_default()
}

pub fn save(state: &State) {
    if let Some(path) = state_path()
        && let Err(e) = save_to(&path, state)
    {
        eprintln!("could not save {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_list_dedupes_orders_and_caps() {
        let mut s = State::default();
        for i in 0..20 {
            s.touch_recent(Path::new(&format!("/p/{i}")));
        }
        assert_eq!(s.recent.len(), MAX_RECENT);
        assert_eq!(s.recent[0], PathBuf::from("/p/19"));
        s.touch_recent(Path::new("/p/10"));
        assert_eq!(s.recent[0], PathBuf::from("/p/10"));
        assert_eq!(s.recent.iter().filter(|p| **p == PathBuf::from("/p/10")).count(), 1);
    }

    #[test]
    fn round_trips_and_survives_a_damaged_file() {
        let dir = std::env::temp_dir().join(format!("wercode-state-{}", std::process::id()));
        let path = dir.join("state.json");
        let mut s = State::default();
        s.touch_recent(&dir);
        s.panel_open = false;
        s.panel = "git".into();
        s.panel_width = Some(412.0);
        s.window = Some([10.0, 20.0, 1200.0, 800.0]);
        save_to(&path, &s).unwrap();
        assert_eq!(load_from(&path), s);
        assert_eq!(s.last_project(), Some(dir.clone()));

        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_from(&path), State::default());
        // Unknown and missing fields are fine: the file outlives versions.
        std::fs::write(&path, r#"{"panel":"search","future":1}"#).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.panel, "search");
        assert!(loaded.panel_open);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
