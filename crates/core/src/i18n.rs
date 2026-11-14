//! The languages of the interface.
//!
//! The English text in the code is the key. Each other language has a file
//! in `crates/core/locales/` that maps it to a translation:
//!
//! ```toml
//! "Settings" = "Настройки"
//! "Move {name} to the trash?" = "Переместить {name} в корзину?"
//! "{n} changes" = { one = "{n} изменение", few = "{n} изменения", many = "{n} изменений" }
//! ```
//!
//! A count is looked up by the English text for many; its table has the
//! forms that language uses (`one`, `few`, `many`, `other`). A text with no
//! translation is shown in English. The tests below find every text the code
//! passes to [`t`], [`tf`] and [`tn`] and check that each file has it.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    Russian,
    German,
    Spanish,
    Chinese,
}

impl Language {
    pub const ALL: [Language; 5] =
        [Language::English, Language::Russian, Language::German, Language::Spanish, Language::Chinese];

    /// The value in `settings.toml`.
    pub fn code(self) -> &'static str {
        match self {
            Language::English => "en",
            Language::Russian => "ru",
            Language::German => "de",
            Language::Spanish => "es",
            Language::Chinese => "zh",
        }
    }

    /// `ru`, `ru-RU`, `zh_CN.UTF-8` and the like.
    pub fn from_code(code: &str) -> Option<Language> {
        let primary = code.trim().split(['-', '_', '.']).next()?.to_ascii_lowercase();
        Language::ALL.into_iter().find(|l| l.code() == primary)
    }

    /// The name of the language in that language, the way a list of
    /// languages shows it.
    pub fn native_name(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::Russian => "Русский",
            Language::German => "Deutsch",
            Language::Spanish => "Español",
            Language::Chinese => "中文",
        }
    }

    /// The name the component library uses for its own few texts; it has
    /// no German or Spanish, which then stay English.
    pub fn library_locale(self) -> &'static str {
        match self {
            Language::Russian => "ru",
            Language::Chinese => "zh-CN",
            _ => "en",
        }
    }

    fn source(self) -> Option<&'static str> {
        match self {
            Language::English => None,
            Language::Russian => Some(include_str!("../locales/ru.toml")),
            Language::German => Some(include_str!("../locales/de.toml")),
            Language::Spanish => Some(include_str!("../locales/es.toml")),
            Language::Chinese => Some(include_str!("../locales/zh.toml")),
        }
    }
}

/// The language of this computer, if it is one of ours.
pub fn system() -> Language {
    sys_locale::get_locale().and_then(|code| Language::from_code(&code)).unwrap_or(Language::English)
}

/// The language a `language` setting asks for: "auto" (or anything unknown
/// to it) follows the computer.
pub fn resolve(setting: &str) -> Language {
    Language::from_code(setting).unwrap_or_else(system)
}

#[cfg(not(test))]
static CURRENT: AtomicU8 = AtomicU8::new(0);

// The core's own tests run side by side; each keeps its own language, so the
// one that switches to Russian cannot change what the others read.
#[cfg(test)]
thread_local! {
    static CURRENT: AtomicU8 = const { AtomicU8::new(0) };
}

fn current() -> u8 {
    #[cfg(not(test))]
    return CURRENT.load(Ordering::Relaxed);
    #[cfg(test)]
    return CURRENT.with(|c| c.load(Ordering::Relaxed));
}

pub fn set_language(language: Language) {
    let index = Language::ALL.iter().position(|l| *l == language).unwrap_or(0) as u8;
    #[cfg(not(test))]
    CURRENT.store(index, Ordering::Relaxed);
    #[cfg(test)]
    CURRENT.with(|c| c.store(index, Ordering::Relaxed));
}

pub fn language() -> Language {
    Language::ALL[current() as usize % Language::ALL.len()]
}

#[derive(Debug)]
enum Entry {
    Text(String),
    Count { one: Option<String>, few: Option<String>, many: Option<String>, other: Option<String> },
}

type Catalog = HashMap<String, Entry>;

fn parse(source: &str) -> Result<Catalog, String> {
    let table: toml::Table = toml::from_str(source).map_err(|e| e.to_string())?;
    let mut catalog = Catalog::new();
    for (key, value) in table {
        let entry = match value {
            toml::Value::String(text) => Entry::Text(text),
            toml::Value::Table(forms) => {
                let form = |name: &str| forms.get(name).and_then(|v| v.as_str()).map(str::to_string);
                Entry::Count { one: form("one"), few: form("few"), many: form("many"), other: form("other") }
            }
            _ => return Err(format!("{key}: a translation is a text or a table of forms")),
        };
        catalog.insert(key, entry);
    }
    Ok(catalog)
}

/// Every language's translations, read once. A file with a mistake gives
/// none, so the interface falls back to English rather than breaking.
fn catalog(language: Language) -> Option<&'static Catalog> {
    static ALL: OnceLock<Vec<Option<Catalog>>> = OnceLock::new();
    let all = ALL.get_or_init(|| Language::ALL.iter().map(|l| l.source().and_then(|s| parse(s).ok())).collect());
    let index = Language::ALL.iter().position(|l| *l == language)?;
    all[index].as_ref()
}

/// A text in the current language.
pub fn t(text: &'static str) -> &'static str {
    match catalog(language()).and_then(|c| c.get(text)) {
        Some(Entry::Text(translated)) => translated,
        _ => text,
    }
}

fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = template.to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), &value.to_string());
    }
    out
}

/// A text with `{name}` holes, filled in after translating.
pub fn tf(text: &'static str, args: &[(&str, &dyn Display)]) -> String {
    fill(t(text), args)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    One,
    Few,
    Many,
    Other,
}

/// Which form of a word goes with `n` (the CLDR rules for whole numbers).
fn form(language: Language, n: u64) -> Form {
    match language {
        Language::Russian => {
            let (tens, ones) = (n % 100, n % 10);
            if ones == 1 && tens != 11 {
                Form::One
            } else if (2..=4).contains(&ones) && !(12..=14).contains(&tens) {
                Form::Few
            } else {
                Form::Many
            }
        }
        Language::Chinese => Form::Other,
        _ if n == 1 => Form::One,
        _ => Form::Other,
    }
}

/// A count: `one` is the English text for 1, `many` for any other number.
/// Both may hold `{n}` and the holes named in `args`.
pub fn tn(n: u64, one: &'static str, many: &'static str, args: &[(&str, &dyn Display)]) -> String {
    let language = language();
    let english = if n == 1 { one } else { many };
    let template: &str = match catalog(language).and_then(|c| c.get(many)) {
        Some(Entry::Text(text)) => text,
        Some(Entry::Count { one: f1, few, many: fm, other }) => {
            let wanted = match form(language, n) {
                Form::One => [f1, other, fm, few],
                Form::Few => [few, other, fm, f1],
                Form::Many => [fm, other, few, f1],
                Form::Other => [other, fm, f1, few],
            };
            wanted.into_iter().flatten().next().map(String::as_str).unwrap_or(english)
        }
        None => english,
    };
    let mut all: Vec<(&str, &dyn Display)> = vec![("n", &n)];
    all.extend_from_slice(args);
    fill(template, &all)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;

    #[test]
    fn russian_numbers_take_three_forms() {
        let ru = |n| form(Language::Russian, n);
        assert_eq!([ru(1), ru(21), ru(101)], [Form::One; 3]);
        assert_eq!([ru(2), ru(4), ru(23)], [Form::Few; 3]);
        assert_eq!([ru(0), ru(5), ru(11), ru(12), ru(14), ru(111)], [Form::Many; 6]);
        assert_eq!(form(Language::German, 1), Form::One);
        assert_eq!(form(Language::Spanish, 3), Form::Other);
        assert_eq!(form(Language::Chinese, 1), Form::Other);
    }

    #[test]
    fn language_codes_are_read_loosely() {
        assert_eq!(Language::from_code("ru-RU"), Some(Language::Russian));
        assert_eq!(Language::from_code("zh_CN.UTF-8"), Some(Language::Chinese));
        assert_eq!(Language::from_code("DE"), Some(Language::German));
        assert_eq!(Language::from_code("auto"), None);
        assert_eq!(Language::from_code("fr-FR"), None);
        assert_eq!(resolve("es"), Language::Spanish);
    }

    /// Turns the inside of a Rust string literal into its text.
    fn unescape(literal: &str) -> String {
        let mut out = String::new();
        let mut chars = literal.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('u') => {
                    let hex: String = chars.by_ref().skip(1).take_while(|c| *c != '}').collect();
                    out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                }
                Some(other) => out.push(other),
                None => {}
            }
        }
        out
    }

    /// Every text the code translates: (key, needs a table of forms).
    fn keys_in_code() -> BTreeSet<(String, bool)> {
        let lit = r#""((?:[^"\\]|\\.)*)""#;
        let single = regex::Regex::new(&format!(r"\b(?:t|tf)\(\s*{lit}")).unwrap();
        let count = regex::Regex::new(&format!(r"\btn\([^;]*?,\s*{lit}\s*,\s*{lit}")).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut keys = BTreeSet::new();
        for dir in ["core/src", "app/src"] {
            for entry in ignore::Walk::new(root.join(dir)).flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "rs") || path.ends_with("i18n.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(path).unwrap();
                for found in single.captures_iter(&text) {
                    keys.insert((unescape(&found[1]), false));
                }
                for found in count.captures_iter(&text) {
                    keys.insert((unescape(&found[2]), true));
                }
            }
        }
        keys
    }

    fn holes(text: &str) -> BTreeSet<String> {
        regex::Regex::new(r"\{[a-z_]+\}").unwrap().find_iter(text).map(|m| m.as_str().to_string()).collect()
    }

    #[test]
    fn every_language_translates_every_text_with_the_same_holes() {
        let keys = keys_in_code();
        assert!(keys.len() > 100, "the scan found only {} texts", keys.len());
        for language in Language::ALL.into_iter().skip(1) {
            let catalog = parse(language.source().unwrap()).unwrap_or_else(|e| panic!("{language:?}: {e}"));
            let mut missing = Vec::new();
            for (key, counted) in &keys {
                let wanted = holes(key);
                match catalog.get(key) {
                    None => missing.push(key.clone()),
                    Some(Entry::Text(text)) => {
                        assert!(!counted || language == Language::Chinese, "{language:?}: {key:?} needs a table of forms");
                        assert_eq!(holes(text), wanted, "{language:?}: {key:?} -> {text:?}");
                    }
                    Some(Entry::Count { one, few, many, other }) => {
                        assert!(counted, "{language:?}: {key:?} is not a count");
                        for text in [one, few, many, other].into_iter().flatten() {
                            let mut have = holes(text);
                            // The form for one may say "one" instead of a number.
                            have.insert("{n}".into());
                            let mut want = wanted.clone();
                            want.insert("{n}".into());
                            assert_eq!(have, want, "{language:?}: {key:?} -> {text:?}");
                        }
                    }
                }
            }
            assert!(missing.is_empty(), "{language:?} has no translation for:\n{}", missing.join("\n"));
            let used: BTreeSet<&String> = keys.iter().map(|(k, _)| k).collect();
            let stale: Vec<&String> = catalog.keys().filter(|k| !used.contains(k)).collect();
            assert!(stale.is_empty(), "{language:?} translates texts the code no longer has: {stale:?}");
        }
    }

    #[test]
    fn texts_and_counts_follow_the_language() {
        set_language(Language::Russian);
        assert_eq!(t("Settings"), "Настройки");
        assert_eq!(tn(3, "{n} change", "{n} changes", &[]), "3 изменения");
        assert_eq!(tn(5, "{n} change", "{n} changes", &[]), "5 изменений");
        assert_eq!(t("A text nobody translated"), "A text nobody translated");
        set_language(Language::English);
        assert_eq!(tn(1, "{n} change", "{n} changes", &[]), "1 change");
        assert_eq!(tf("Restart to update to {version}", &[("version", &"0.2.0")]), "Restart to update to 0.2.0");
    }
}
