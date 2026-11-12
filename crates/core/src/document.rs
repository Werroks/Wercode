//! Reading a text file for the editor and writing it back the way it was
//! found: same line endings, same byte-order mark.

use std::path::Path;
use std::time::SystemTime;

/// Files above this size are not opened in the editor.
pub const MAX_SIZE: u64 = 20 * 1024 * 1024;

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// What has to be put back when the text is saved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Format {
    /// Lines end with CR LF on disk; in memory they always end with LF.
    pub crlf: bool,
    /// The file starts with a UTF-8 byte-order mark.
    pub bom: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// The text with LF line endings and without the byte-order mark.
    pub text: String,
    pub format: Format,
    /// When the file was last written, to notice changes made by other programs.
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    TooLarge,
    /// Not UTF-8 text: an image, an archive, another encoding.
    NotText,
    Io(String),
}

impl LoadError {
    /// The sentence shown in place of the editor.
    pub fn message(&self) -> String {
        match self {
            LoadError::TooLarge => "This file is larger than 20 MB and is not opened in the editor.".into(),
            LoadError::NotText => "This file is not UTF-8 text, so it cannot be shown here.".into(),
            LoadError::Io(e) => format!("Could not read the file: {e}"),
        }
    }
}

pub fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Reads a file for editing. Blocks; call it off the UI thread.
pub fn load(path: &Path) -> Result<Document, LoadError> {
    let meta = std::fs::metadata(path).map_err(|e| LoadError::Io(e.to_string()))?;
    if meta.len() > MAX_SIZE {
        return Err(LoadError::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(|e| LoadError::Io(e.to_string()))?;
    let (text, format) = decode(&bytes)?;
    Ok(Document { text, format, modified: meta.modified().ok() })
}

pub fn decode(bytes: &[u8]) -> Result<(String, Format), LoadError> {
    let bom = bytes.starts_with(BOM);
    let body = if bom { &bytes[BOM.len()..] } else { bytes };
    if body.contains(&0) {
        return Err(LoadError::NotText);
    }
    let text = std::str::from_utf8(body).map_err(|_| LoadError::NotText)?;
    let crlf = text.contains("\r\n");
    let text = if crlf { text.replace("\r\n", "\n") } else { text.to_string() };
    Ok((text, Format { crlf, bom }))
}

pub fn encode(text: &str, format: Format) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 3);
    if format.bom {
        out.extend_from_slice(BOM);
    }
    if format.crlf {
        // Text pasted with CR LF must not end up with CR CR LF.
        out.extend_from_slice(text.replace("\r\n", "\n").replace('\n', "\r\n").as_bytes());
    } else {
        out.extend_from_slice(text.as_bytes());
    }
    out
}

/// Writes the text and returns the file's new modification time. Blocks.
pub fn save(path: &Path, text: &str, format: Format) -> Result<Option<SystemTime>, String> {
    std::fs::write(path, encode(text, format)).map_err(|e| e.to_string())?;
    Ok(modified(path))
}

/// The syntax highlighting name for a file, by its name or extension.
pub fn language(path: &Path) -> &'static str {
    let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    match name.as_str() {
        "makefile" | "gnumakefile" => return "make",
        "cmakelists.txt" => return "cmake",
        "cargo.lock" => return "toml",
        ".bashrc" | ".zshrc" | ".profile" | ".bash_profile" => return "bash",
        _ => {}
    }
    match name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or_default() {
        "rs" => "rust",
        "toml" => "toml",
        "json" | "jsonc" => "json",
        "md" | "markdown" => "markdown",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "py" | "pyi" => "python",
        "go" => "go",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "sh" | "bash" | "zsh" => "bash",
        "yml" | "yaml" => "yaml",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "sql" => "sql",
        "diff" | "patch" => "diff",
        _ => "text",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings_and_bom_survive_a_round_trip() {
        let original = b"\xEF\xBB\xBFone\r\ntwo\r\n";
        let (text, format) = decode(original).unwrap();
        assert_eq!(text, "one\ntwo\n");
        assert_eq!(format, Format { crlf: true, bom: true });
        assert_eq!(encode(&text, format), original);
        // An edit that brought its own CR LF does not double the CR.
        assert_eq!(encode("a\r\nb\n", format), b"\xEF\xBB\xBFa\r\nb\r\n");

        let (text, format) = decode(b"plain\n").unwrap();
        assert_eq!((text.as_str(), format), ("plain\n", Format::default()));
        assert_eq!(encode(&text, format), b"plain\n");
    }

    #[test]
    fn binary_and_other_encodings_are_refused() {
        assert_eq!(decode(b"PNG\0\x01"), Err(LoadError::NotText));
        assert_eq!(decode(b"caf\xE9"), Err(LoadError::NotText));
        assert!(decode(b"").is_ok());
    }

    #[test]
    fn loads_and_saves_a_file() {
        let path = std::env::temp_dir().join(format!("wercode-doc-{}.txt", std::process::id()));
        std::fs::write(&path, "a\r\nb").unwrap();
        let doc = load(&path).unwrap();
        assert_eq!(doc.text, "a\nb");
        assert!(doc.modified.is_some());
        save(&path, "a\nb\nc", doc.format).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"a\r\nb\r\nc");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(load(&path), Err(LoadError::Io(_))));
    }

    #[test]
    fn picks_a_language_by_name() {
        assert_eq!(language(Path::new("src/main.rs")), "rust");
        assert_eq!(language(Path::new("Cargo.lock")), "toml");
        assert_eq!(language(Path::new("README.MD")), "markdown");
        assert_eq!(language(Path::new("notes")), "text");
    }
}
