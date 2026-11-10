//! Colour schemes. Built-in ones are compiled in from `assets/themes`.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};

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

/// Returns the named built-in theme, or the default one if the name is unknown.
pub fn load(id: &str) -> Theme {
    let text = BUILTIN
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, t)| *t)
        .unwrap_or(BUILTIN[0].1);
    toml::from_str(text).expect("built-in theme is valid")
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
    fn unknown_theme_falls_back() {
        assert_eq!(load("nope").name, "Black");
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
