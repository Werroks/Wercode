//! Colour schemes. Built-in ones are compiled in from `assets/themes`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};

use crate::i18n::tf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn parse(s: &str) -> Option<Rgb> {
        let h = s.strip_prefix('#')?;
        if h.len() != 6 || !h.is_ascii() {
            return None;
        }
        let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        Some(Rgb(p(0)?, p(2)?, p(4)?))
    }

    pub fn dim(self) -> Rgb {
        let d = |c: u8| (c as u16 * 2 / 3) as u8;
        Rgb(d(self.0), d(self.1), d(self.2))
    }

    /// This colour moved `amount` (0 to 1) of the way towards `other`.
    pub fn mix(self, other: Rgb, amount: f32) -> Rgb {
        let amount = amount.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }

    pub fn as_u32(self) -> u32 {
        (self.0 as u32) << 16 | (self.1 as u32) << 8 | self.2 as u32
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgb::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("bad colour: {s}")))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Theme {
    pub name: String,
    pub appearance: Appearance,
    pub ui: Ui,
    pub terminal: TerminalColors,
    #[serde(default)]
    pub syntax: BTreeMap<String, Rgb>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Dark,
    Light,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Ui {
    pub bg: Rgb,
    pub surface: Rgb,
    pub surface_2: Rgb,
    pub border: Rgb,
    pub border_input: Rgb,
    pub text: Rgb,
    pub text_muted: Rgb,
    pub accent: Rgb,
    pub on_accent: Rgb,
    pub danger: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub selection: Rgb,
}

impl Ui {
    /// The background of the row under the pointer. Lists sit on `surface`
    /// (the side panel) and on `surface_2` (menus and pop-ups), so it is a
    /// step past both, towards the text.
    pub fn hover(&self) -> Rgb {
        self.surface_2.mix(self.text, 0.10)
    }

    /// The background of the chosen row: one step further than [`Ui::hover`].
    pub fn chosen(&self) -> Rgb {
        self.surface_2.mix(self.text, 0.17)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalColors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub ansi: [Rgb; 16],
}

impl TerminalColors {
    /// xterm 256-colour palette entry.
    pub fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                let i = i - 16;
                let step = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
                Rgb(step(i / 36), step(i / 6 % 6), step(i % 6))
            }
            _ => {
                let v = 8 + 10 * (i - 232);
                Rgb(v, v, v)
            }
        }
    }
}

const BUILTIN: &[(&str, &str)] = &[
    ("black", include_str!("../../../assets/themes/black.toml")),
    ("graphite", include_str!("../../../assets/themes/graphite.toml")),
    ("midnight", include_str!("../../../assets/themes/midnight.toml")),
    ("forest", include_str!("../../../assets/themes/forest.toml")),
    ("paper", include_str!("../../../assets/themes/paper.toml")),
];

pub fn builtin_ids() -> impl Iterator<Item = &'static str> {
    BUILTIN.iter().map(|(id, _)| *id)
}

fn builtin_text(id: &str) -> Option<&'static str> {
    BUILTIN.iter().find(|(k, _)| *k == id).map(|(_, t)| *t)
}

/// Where the user's own schemes are: one `<id>.toml` each, next to `settings.toml`.
pub fn user_dir() -> Option<PathBuf> {
    crate::config::config_dir().map(|dir| dir.join("themes"))
}

/// Every scheme to choose from, built-in ones first, as the value for the
/// setting and the name to show.
pub fn available() -> Vec<(String, String)> {
    available_in(user_dir().as_deref())
}

pub fn available_in(dir: Option<&Path>) -> Vec<(String, String)> {
    let mut all: Vec<(String, String)> = builtin_ids().map(|id| (id.to_string(), load(id).name)).collect();
    let mut own: Vec<(String, String)> = dir
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let id = path.file_stem()?.to_string_lossy().into_owned();
            if path.extension()? != "toml" || builtin_text(&id).is_some() {
                return None;
            }
            let name = load_user(&path, &id).map(|t| t.name).unwrap_or_else(|_| id.clone());
            Some((id, name))
        })
        .collect();
    own.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    all.extend(own);
    all
}

/// Returns the named scheme: a built-in one or the user's own, or the
/// default one if there is none by that name or it cannot be read.
pub fn load(id: &str) -> Theme {
    load_checked(id).unwrap_or_else(|_| parse_builtin(BUILTIN[0].1))
}

/// Like [`load`], but says why the user's own scheme could not be used.
pub fn load_checked(id: &str) -> Result<Theme, String> {
    load_checked_in(user_dir().as_deref(), id)
}

pub fn load_checked_in(dir: Option<&Path>, id: &str) -> Result<Theme, String> {
    if let Some(text) = builtin_text(id) {
        return Ok(parse_builtin(text));
    }
    let Some(dir) = dir else { return Ok(parse_builtin(BUILTIN[0].1)) };
    let path = dir.join(format!("{id}.toml"));
    if !path.is_file() {
        return Ok(parse_builtin(BUILTIN[0].1));
    }
    load_user(&path, id)
}

fn parse_builtin(text: &str) -> Theme {
    toml::from_str(text).expect("built-in theme is valid")
}

/// A scheme of the user's: whatever it leaves out comes from the built-in
/// scheme it names in `based_on` (or Black, or Paper for a light one).
fn load_user(path: &Path, id: &str) -> Result<Theme, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let own: toml::Table = toml::from_str(&text).map_err(|e| e.message().to_string())?;
    let light = own.get("appearance").and_then(toml::Value::as_str) == Some("light");
    let base_id = own.get("based_on").and_then(toml::Value::as_str).unwrap_or(if light { "paper" } else { "black" });
    let base_text = builtin_text(base_id).ok_or_else(|| tf("There is no scheme named {name}.", &[("name", &base_id)]))?;
    let mut merged: toml::Table = toml::from_str(base_text).expect("built-in theme is valid");
    merged.insert("name".into(), toml::Value::String(id.to_string()));
    merge(&mut merged, own);
    merged.remove("based_on");
    toml::Value::Table(merged).try_into().map_err(|e: toml::de::Error| e.message().to_string())
}

/// Puts `over` on top of `base`, table by table.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(inner)), toml::Value::Table(value)) => merge(inner, value),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// The text of a new scheme of the user's, starting from `base`: its name
/// and a few colours to change.
pub fn starter(name: &str, base: &str) -> String {
    let theme = load(base);
    let hex = |c: Rgb| format!("#{:06x}", c.as_u32());
    let mut out = format!("name = {name:?}
based_on = {base:?}

[ui]
");
    out.push_str(&format!("accent = \"{}\"
bg = \"{}\"
surface = \"{}\"
", hex(theme.ui.accent), hex(theme.ui.bg), hex(theme.ui.surface)));
    out.push_str("
[syntax]
");
    for key in ["keyword", "string", "comment", "function"] {
        if let Some(colour) = theme.syntax.get(key) {
            out.push_str(&format!("{key} = \"{}\"
", hex(*colour)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtin_themes_parse() {
        for id in builtin_ids() {
            let t = load(id);
            assert!(!t.name.is_empty(), "{id}");
        }
    }

    #[test]
    fn hover_and_chosen_rows_show_on_every_background() {
        // How far apart two colours are, by the channel that differs most.
        let apart = |a: Rgb, b: Rgb| [a.0.abs_diff(b.0), a.1.abs_diff(b.1), a.2.abs_diff(b.2)].into_iter().max().unwrap();
        for id in builtin_ids() {
            let ui = load(id).ui;
            for background in [ui.bg, ui.surface, ui.surface_2] {
                assert!(apart(ui.hover(), background) >= 12, "{id}: hover");
                assert!(apart(ui.chosen(), background) >= 12, "{id}: chosen");
            }
            assert!(apart(ui.hover(), ui.chosen()) >= 6, "{id}: hover and chosen");
        }
        assert_eq!(Rgb(0, 100, 200).mix(Rgb(100, 100, 0), 0.5), Rgb(50, 100, 100));
    }

    #[test]
    fn unknown_theme_falls_back() {
        assert_eq!(load("nope").name, "Black");
    }

    #[test]
    fn a_users_scheme_changes_only_what_it_names() {
        let dir = std::env::temp_dir().join(format!("wercode-themes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sunset.toml"), "name = \"Sunset\"
based_on = \"midnight\"
[ui]
accent = \"#ff8800\"
").unwrap();
        std::fs::write(dir.join("bare.toml"), "appearance = \"light\"
").unwrap();
        std::fs::write(dir.join("broken.toml"), "[ui
").unwrap();
        std::fs::write(dir.join("black.toml"), "name = \"Not this one\"
").unwrap();

        let sunset = load_checked_in(Some(&dir), "sunset").unwrap();
        assert_eq!(sunset.name, "Sunset");
        assert_eq!(sunset.ui.accent, Rgb(0xff, 0x88, 0x00));
        assert_eq!(sunset.ui.bg, load("midnight").ui.bg);
        // Nothing but the appearance: the light built-in scheme, named after the file.
        let bare = load_checked_in(Some(&dir), "bare").unwrap();
        assert_eq!((bare.name.as_str(), bare.ui.bg), ("bare", load("paper").ui.bg));
        assert!(load_checked_in(Some(&dir), "broken").is_err());
        // A file cannot take the place of a built-in scheme.
        assert_eq!(load_checked_in(Some(&dir), "black").unwrap().name, "Black");

        let ids: Vec<String> = available_in(Some(&dir)).into_iter().map(|(id, _)| id).collect();
        assert_eq!(&ids[..5], ["black", "graphite", "midnight", "forest", "paper"]);
        assert_eq!(&ids[5..], ["bare", "broken", "sunset"]);

        // A new scheme starts as a copy that reads back the same.
        std::fs::write(dir.join("mine.toml"), starter("Mine", "forest")).unwrap();
        let mine = load_checked_in(Some(&dir), "mine").unwrap();
        assert_eq!((mine.name.as_str(), mine.ui.accent), ("Mine", load("forest").ui.accent));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn colour_cube() {
        let t = load("black").terminal;
        assert_eq!(t.indexed(16), Rgb(0, 0, 0));
        assert_eq!(t.indexed(231), Rgb(255, 255, 255));
        assert_eq!(t.indexed(232), Rgb(8, 8, 8));
        assert_eq!(t.indexed(255), Rgb(238, 238, 238));
    }
}
