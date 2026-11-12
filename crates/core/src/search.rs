//! Text search across a project. Ignored files, binary files and very large
//! files are skipped; results arrive file by file so the list fills as it goes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use regex::{Regex, RegexBuilder};

/// Files larger than this are not searched.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// The search stops after this many matching lines.
pub const MAX_LINES: usize = 5_000;
/// A matching line is shown cut down to about this many characters.
const MAX_SHOWN: usize = 240;
/// How much of a long line is kept in front of its first match.
const LEAD: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// The text is a regular expression rather than plain words.
    pub regex: bool,
}

/// One line with at least one match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// 1-based line number.
    pub number: u32,
    /// 1-based column of the first match, counted in characters.
    pub column: u32,
    /// The line without its indentation, cut down if it is very long.
    pub text: String,
    /// Byte ranges of the matches inside `text`.
    pub ranges: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatches {
    pub path: PathBuf,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    pub files: usize,
    pub lines: usize,
    /// The limit was reached; there may be more matches than were returned.
    pub truncated: bool,
}

/// Turns the query into a pattern. Fails with a readable message when a
/// regular expression is not valid.
pub fn compile(query: &Query) -> Result<Regex, String> {
    let mut pattern = if query.regex { query.text.clone() } else { regex::escape(&query.text) };
    if query.whole_word {
        pattern = format!(r"\b(?:{pattern})\b");
    }
    RegexBuilder::new(&pattern)
        .case_insensitive(!query.case_sensitive)
        .build()
        .map_err(|e| match e {
            regex::Error::Syntax(message) => message.lines().last().unwrap_or("invalid pattern").trim().to_string(),
            other => other.to_string(),
        })
}

fn floor_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Finds the matches in one line, already without its line ending.
fn match_line(pattern: &Regex, number: usize, line: &str) -> Option<Line> {
    let mut found = pattern.find_iter(line).filter(|m| !m.is_empty()).map(|m| (m.start(), m.end())).peekable();
    let &(first, _) = found.peek()?;
    let column = line[..first].chars().count() as u32 + 1;

    // Show the line from its first visible character, or from a little before
    // the match when that is far to the right.
    let indent = line.len() - line.trim_start().len();
    let mut start = indent.min(first);
    if first - start > LEAD * 2 {
        start = floor_boundary(line, first - LEAD);
    }
    let end = floor_boundary(line, start + MAX_SHOWN);
    let ranges = found
        .take_while(|&(from, _)| from < end)
        .map(|(from, to)| (from - start, to.min(end) - start))
        .collect();
    Some(Line { number: number as u32 + 1, column, text: line[start..end].trim_end().to_string(), ranges })
}

/// Searches the text of one file.
pub fn search_text(pattern: &Regex, text: &str, limit: usize) -> Vec<Line> {
    text.lines()
        .enumerate()
        .filter_map(|(number, line)| match_line(pattern, number, line))
        .take(limit)
        .collect()
}

/// Searches every file under `root`, calling `found` for each file that has
/// matches, in the order the files are listed. Setting `cancel` stops it early.
pub fn search(
    root: &Path,
    query: &Query,
    cancel: &AtomicBool,
    mut found: impl FnMut(FileMatches),
) -> Result<Summary, String> {
    let mut summary = Summary::default();
    if query.text.is_empty() {
        return Ok(summary);
    }
    let pattern = compile(query)?;
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();
    for entry in walker.filter_map(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if entry.metadata().is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else { continue };
        // A zero byte near the start means this is not text.
        if bytes.iter().take(8_000).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines = search_text(&pattern, &text, MAX_LINES - summary.lines);
        if lines.is_empty() {
            continue;
        }
        summary.files += 1;
        summary.lines += lines.len();
        found(FileMatches { path: entry.into_path(), lines });
        if summary.lines >= MAX_LINES {
            summary.truncated = true;
            break;
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(text: &str) -> Query {
        Query { text: text.into(), ..Query::default() }
    }

    fn lines(query: &Query, text: &str) -> Vec<Line> {
        search_text(&compile(query).unwrap(), text, usize::MAX)
    }

    #[test]
    fn finds_plain_text_whatever_its_case() {
        let found = lines(&query("hello"), "say Hello\nnothing\n    hello, hello\r\n");
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].number, found[0].column, found[0].text.as_str()), (1, 5, "say Hello"));
        assert_eq!(found[0].ranges, [(4, 9)]);
        // Indentation is not shown, but the column still counts it.
        assert_eq!((found[1].number, found[1].column, found[1].text.as_str()), (3, 5, "hello, hello"));
        assert_eq!(found[1].ranges, [(0, 5), (7, 12)]);
    }

    #[test]
    fn options_narrow_the_search() {
        let text = "Cat\ncat\nconcatenate\na.c\nabc\n";
        let numbers = |q: &Query| lines(q, text).iter().map(|l| l.number).collect::<Vec<_>>();
        assert_eq!(numbers(&query("cat")), [1, 2, 3]);
        assert_eq!(numbers(&Query { case_sensitive: true, ..query("cat") }), [2, 3]);
        assert_eq!(numbers(&Query { whole_word: true, ..query("cat") }), [1, 2]);
        // Without the regex option a dot is only a dot.
        assert_eq!(numbers(&query("a.c")), [4]);
        assert_eq!(numbers(&Query { regex: true, ..query("a.c") }), [4, 5]);
        assert!(compile(&Query { regex: true, ..query("(") }).is_err());
        assert!(compile(&query("(")).is_ok());
    }

    #[test]
    fn a_long_line_is_cut_around_its_match() {
        let line = format!("{}needle{}", "\u{44f}".repeat(500), "x".repeat(500));
        let found = lines(&query("needle"), &line);
        let shown = &found[0];
        assert_eq!(shown.column, 501);
        assert!(shown.text.len() <= MAX_SHOWN, "{}", shown.text.len());
        let (from, to) = shown.ranges[0];
        assert_eq!(&shown.text[from..to], "needle");
    }

    #[test]
    fn walks_a_folder_skipping_ignored_and_binary_files() {
        let root = std::env::temp_dir().join(format!("wercode-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(root.join("src").join("a.rs"), "fn needle() {}\n// needle again\n").unwrap();
        std::fs::write(root.join("b.txt"), "no match\n").unwrap();
        std::fs::write(root.join("target").join("c.txt"), "needle\n").unwrap();
        std::fs::write(root.join(".git").join("d"), "needle\n").unwrap();
        std::fs::write(root.join("e.bin"), b"needle\0needle").unwrap();

        let cancel = AtomicBool::new(false);
        let mut files = Vec::new();
        let summary = search(&root, &query("needle"), &cancel, |file| files.push(file)).unwrap();
        assert_eq!(summary, Summary { files: 1, lines: 2, truncated: false });
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, root.join("src").join("a.rs"));

        // An empty query finds nothing, and a cancelled search stops at once.
        assert_eq!(search(&root, &query(""), &cancel, |_| panic!()).unwrap(), Summary::default());
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(search(&root, &query("needle"), &cancel, |_| panic!()).unwrap(), Summary::default());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
