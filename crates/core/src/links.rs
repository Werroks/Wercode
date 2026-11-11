//! Finding the URL or file path under the pointer in a line of terminal output.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    Url(String),
    Path { path: PathBuf, line: Option<u32>, col: Option<u32> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// First column of the link.
    pub start: usize,
    /// One past its last column.
    pub end: usize,
    pub target: LinkTarget,
}

const SCHEMES: [&str; 3] = ["https://", "http://", "file://"];
const OPENERS: &[char] = &['"', '\'', '`', '(', '[', '{', '<'];
const CLOSERS: &[char] = &['"', '\'', '`', ']', '}', '>', '.', ',', ';', ':', '!', '?'];

/// The link covering column `col` of `row` (one char per column, `\0` for the
/// second half of a wide character). Paths count only when they exist on
/// disk, resolved against `cwd` when relative.
pub fn link_at(row: &[char], col: usize, cwd: Option<&Path>) -> Option<Link> {
    let blank = |c: char| c == ' ' || c == '\t';
    if col >= row.len() || blank(row[col]) {
        return None;
    }
    let mut start = col;
    while start > 0 && !blank(row[start - 1]) {
        start -= 1;
    }
    let mut end = col + 1;
    while end < row.len() && !blank(row[end]) {
        end += 1;
    }

    if let Some(link) = url_in(row, start, end) {
        return (link.start <= col && col < link.end).then_some(link);
    }
    let link = path_in(row, start, end, cwd)?;
    (link.start <= col && col < link.end).then_some(link)
}

fn text(row: &[char], start: usize, end: usize) -> String {
    row[start..end].iter().filter(|c| **c != '\0').collect()
}

fn url_in(row: &[char], start: usize, end: usize) -> Option<Link> {
    let from = (start..end).find(|&i| {
        SCHEMES.iter().any(|s| {
            let s: Vec<char> = s.chars().collect();
            row[i..end].starts_with(&s)
        })
    })?;
    let mut to = end;
    loop {
        let Some(&last) = row[from..to].last() else { return None };
        let unbalanced = |open: char, close: char| {
            last == close
                && row[from..to].iter().filter(|c| **c == close).count()
                    > row[from..to].iter().filter(|c| **c == open).count()
        };
        if CLOSERS.contains(&last) && last != ']' && last != '}'
            || unbalanced('(', ')')
            || unbalanced('[', ']')
            || unbalanced('{', '}')
        {
            to -= 1;
        } else {
            break;
        }
    }
    let url = text(row, from, to);
    // A bare scheme is not a link.
    if SCHEMES.contains(&url.as_str()) {
        return None;
    }
    Some(Link { start: from, end: to, target: LinkTarget::Url(url) })
}

/// Splits a trailing position off a path: `:12`, `:12:5` or `(12,5)`.
fn split_position(s: &str) -> (&str, Option<u32>, Option<u32>) {
    if let Some(body) = s.strip_suffix(')')
        && let Some(open) = body.rfind('(')
    {
        let inside = &body[open + 1..];
        let mut parts = inside.split(',');
        let line = parts.next().and_then(|p| p.trim().parse().ok());
        let col = parts.next().and_then(|p| p.trim().parse().ok());
        if line.is_some() && parts.next().is_none() && open > 0 {
            return (&s[..open], line, col);
        }
    }
    let number_suffix = |s: &str| -> Option<(usize, u32)> {
        let at = s.rfind(':')?;
        let n = s[at + 1..].parse().ok()?;
        Some((at, n))
    };
    if let Some((at, last)) = number_suffix(s) {
        if let Some((at2, first)) = number_suffix(&s[..at]) {
            return (&s[..at2], Some(first), Some(last));
        }
        return (&s[..at], Some(last), None);
    }
    (s, None, None)
}

fn resolve(path: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let candidate = Path::new(path);
    let full = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        cwd?.join(candidate)
    };
    if full.exists() {
        return Some(full);
    }
    // `git diff` writes paths as a/src/x.rs and b/src/x.rs.
    let stripped = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/"))?;
    let full = cwd?.join(stripped);
    full.exists().then_some(full)
}

fn path_in(row: &[char], mut start: usize, mut end: usize, cwd: Option<&Path>) -> Option<Link> {
    while start < end && OPENERS.contains(&row[start]) {
        start += 1;
    }
    // Try the token as it is, then with closing punctuation peeled off one
    // character at a time: `src/main.rs:12:` and `(src/main.rs)` both work.
    loop {
        if start >= end {
            return None;
        }
        let token = text(row, start, end);
        let (path, line, col) = split_position(&token);
        if let Some(path) = resolve(path, cwd) {
            return Some(Link { start, end, target: LinkTarget::Path { path, line, col } });
        }
        let last = row[end - 1];
        if CLOSERS.contains(&last) || last == ')' {
            end -= 1;
        } else {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn finds_urls_and_trims_punctuation() {
        let r = row("see (https://example.com/a_(b)?q=1), ok");
        let link = link_at(&r, 10, None).unwrap();
        assert_eq!(link.target, LinkTarget::Url("https://example.com/a_(b)?q=1".into()));
        assert_eq!((link.start, link.end), (5, 34));
        assert!(link_at(&r, 0, None).is_none());
        assert!(link_at(&r, 37, None).is_none());
    }

    #[test]
    fn splits_positions() {
        assert_eq!(split_position("a/b.rs:12:5"), ("a/b.rs", Some(12), Some(5)));
        assert_eq!(split_position("a/b.rs:12"), ("a/b.rs", Some(12), None));
        assert_eq!(split_position("a/b.cs(7,3)"), ("a/b.cs", Some(7), Some(3)));
        assert_eq!(split_position("C:\\x\\y.rs:4"), ("C:\\x\\y.rs", Some(4), None));
        assert_eq!(split_position("C:\\x\\y.rs"), ("C:\\x\\y.rs", None, None));
    }

    #[test]
    fn finds_existing_paths_only() {
        let dir = std::env::temp_dir().join(format!("wercode-links-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src").join("main.rs"), "fn main() {}").unwrap();

        let r = row("error at src/main.rs:12:5: oops");
        let link = link_at(&r, 12, Some(&dir)).unwrap();
        assert_eq!(
            link.target,
            LinkTarget::Path {
                path: dir.join("src/main.rs"),
                line: Some(12),
                col: Some(5)
            }
        );
        assert_eq!((link.start, link.end), (9, 25));

        let r = row("--- a/src/main.rs");
        assert!(matches!(
            link_at(&r, 8, Some(&dir)).unwrap().target,
            LinkTarget::Path { line: None, .. }
        ));

        let r = row("no such src/nothing.rs here");
        assert!(link_at(&r, 12, Some(&dir)).is_none());
        // Relative paths need a directory to resolve against.
        let r = row("src/main.rs");
        assert!(link_at(&r, 2, None).is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
