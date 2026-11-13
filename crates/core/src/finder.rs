//! Finding a file of the project by a few letters of its name.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// A project with more files than this is listed only this far.
pub const MAX_FILES: usize = 100_000;

/// Every file under `root` that is not ignored, as a path relative to `root`
/// with forward slashes. Setting `cancel` stops the walk early.
pub fn list_files(root: &Path, cancel: &AtomicBool) -> Vec<String> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();
    let mut files = Vec::new();
    for entry in walker.filter_map(Result::ok) {
        if cancel.load(Ordering::Relaxed) || files.len() >= MAX_FILES {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Ok(relative) = entry.path().strip_prefix(root) {
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    files
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// How well the letters of `query` are found, in order, in `text` from
/// `start` on. `None` when some letter is missing.
fn score_from(text: &[char], start: usize, query: &[char]) -> Option<i32> {
    let mut score = 0;
    let mut at = start;
    let mut previous: Option<usize> = None;
    for &wanted in query {
        let found = (at..text.len()).find(|&i| lower(text[i]) == wanted)?;
        score += 1;
        if previous.is_some_and(|p| p + 1 == found) {
            // Letters in a row are what was typed on purpose.
            score += 8;
        }
        let boundary = found == 0
            || matches!(text[found - 1], '/' | '_' | '-' | '.' | ' ')
            || (text[found].is_uppercase() && text[found - 1].is_lowercase());
        if boundary {
            score += 6;
        }
        previous = Some(found);
        at = found + 1;
    }
    Some(score)
}

/// Scores one path against a query that is already lower case and has no
/// spaces. Higher is better.
fn score(path: &str, query: &[char]) -> Option<i32> {
    let text: Vec<char> = path.chars().collect();
    let name = text.iter().rposition(|&c| c == '/').map_or(0, |i| i + 1);
    // A match inside the file name counts for more than one spread over folders.
    let in_name = score_from(&text, name, query).map(|s| s + 30);
    let anywhere = score_from(&text, 0, query);
    let best = in_name.max(anywhere)?;
    // Of two equal matches the shorter path is the likelier one.
    Some(best * 4 - (text.len() as i32) / 4)
}

/// Indexes into `files` of the paths matching `query`, best first, at most
/// `limit` of them. An empty query gives the first files as they are listed.
pub fn matches(files: &[String], query: &str, limit: usize) -> Vec<usize> {
    let query: Vec<char> =
        query.chars().filter(|c| !c.is_whitespace()).map(|c| if c == '\\' { '/' } else { lower(c) }).collect();
    if query.is_empty() {
        return (0..files.len().min(limit)).collect();
    }
    let mut found: Vec<(i32, usize)> =
        files.iter().enumerate().filter_map(|(index, path)| Some((score(path, &query)?, index))).collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    found.truncate(limit);
    found.into_iter().map(|(_, index)| index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found<'a>(files: &'a [String], query: &str) -> Vec<&'a str> {
        matches(files, query, 10).into_iter().map(|i| files[i].as_str()).collect()
    }

    #[test]
    fn the_file_name_counts_for_more_than_the_folders() {
        let files: Vec<String> = [
            "crates/app/src/workspace.rs",
            "crates/core/src/watch.rs",
            "docs/work/space/readme.md",
            "Cargo.toml",
            "crates/app/src/panel/GitPanel.rs",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(found(&files, "workspace")[0], "crates/app/src/workspace.rs");
        assert_eq!(found(&files, "wat"), ["crates/core/src/watch.rs"]);
        // Capitals inside a name and folders can be typed by their first letters.
        assert_eq!(found(&files, "gp")[0], "crates/app/src/panel/GitPanel.rs");
        assert_eq!(found(&files, "core/watch"), ["crates/core/src/watch.rs"]);
        assert_eq!(found(&files, r"core\watch"), ["crates/core/src/watch.rs"]);
        assert_eq!(found(&files, "CARGO"), ["Cargo.toml"]);
        assert!(found(&files, "zzz").is_empty());
        // Nothing typed yet: the list as it is, up to the limit.
        assert_eq!(matches(&files, "  ", 2), [0, 1]);
    }

    #[test]
    fn lists_files_without_ignored_ones() {
        let root = std::env::temp_dir().join(format!("wercode-finder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(root.join("src").join("a.rs"), "").unwrap();
        std::fs::write(root.join("target").join("b"), "").unwrap();
        std::fs::write(root.join(".git").join("c"), "").unwrap();

        let cancel = AtomicBool::new(false);
        assert_eq!(list_files(&root, &cancel), [".gitignore", "src/a.rs"]);
        cancel.store(true, Ordering::Relaxed);
        assert!(list_files(&root, &cancel).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
