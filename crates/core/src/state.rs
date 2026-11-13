//! What the app remembers between launches, stored as `state.json` next to
//! the settings. Unlike settings, the user never edits this file by hand.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many recent projects are kept.
const MAX_RECENT: usize = 15;

/// The tabs of one project's window, put back when the project is opened again.
/// What the terminals printed is not kept, only where each shell was.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub tabs: Vec<TabLayout>,
    /// Index of the tab that was showing.
    pub active: usize,
    /// With the terminal below the files: the share of the height the files
    /// take, once the line between them has been dragged.
    pub dock_ratio: Option<f32>,
    /// With the terminal below the files: the terminal is put away.
    pub terminal_hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TabLayout {
    Terminals { panes: PaneLayout },
    File { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PaneLayout {
    Terminal {
        /// The shell's directory, when it was known.
        cwd: Option<PathBuf>,
    },
    Split {
        /// One above the other instead of side by side.
        stacked: bool,
        /// Share of the space taken by `first`.
        ratio: f32,
        first: Box<PaneLayout>,
        second: Box<PaneLayout>,
    },
}

impl PaneLayout {
    /// How many terminals this part of the tab holds.
    pub fn terminals(&self) -> usize {
        match self {
            PaneLayout::Terminal { .. } => 1,
            PaneLayout::Split { first, second, .. } => first.terminals() + second.terminals(),
        }
    }
}

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
    /// Open tabs of each project, by its folder.
    pub layouts: BTreeMap<PathBuf, Layout>,
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
            layouts: BTreeMap::new(),
        }
    }
}

impl State {
    /// Moves `path` to the front of the recent list.
    pub fn touch_recent(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(MAX_RECENT);
        // A project that fell off the list will not be reopened from it.
        let recent = &self.recent;
        self.layouts.retain(|project, _| recent.contains(project));
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

        // Tabs are remembered only for projects that are still on the list.
        s.layouts.insert(PathBuf::from("/p/10"), Layout::default());
        s.layouts.insert(PathBuf::from("/p/0"), Layout::default());
        s.touch_recent(Path::new("/p/19"));
        assert_eq!(s.layouts.keys().collect::<Vec<_>>(), [Path::new("/p/10")]);
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
        let split = PaneLayout::Split {
            stacked: true,
            ratio: 0.3,
            first: Box::new(PaneLayout::Terminal { cwd: Some(dir.join("src")) }),
            second: Box::new(PaneLayout::Terminal { cwd: None }),
        };
        assert_eq!(split.terminals(), 2);
        let tabs = vec![TabLayout::Terminals { panes: split }, TabLayout::File { path: dir.join("a.rs") }];
        s.layouts.insert(dir.clone(), Layout { tabs, active: 1, ..Layout::default() });
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
