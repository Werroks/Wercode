//! Which lines of a text differ from an older version of it: what an editor
//! marks next to the line numbers.

/// What happened to a run of lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The lines are new.
    Added,
    /// The lines stand where other lines stood.
    Modified,
    /// Lines were taken out here; none of the present lines is theirs.
    Deleted,
}

/// A run of lines in the new text, 0-based, `end` not included. For
/// [`Kind::Deleted`] the run is empty and `start` is the line the removed
/// lines stood before (one past the last line when they were at the end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    pub kind: Kind,
    pub start: u32,
    pub end: u32,
}

/// Above this many pairs of lines the middle of a file is called changed as
/// a whole rather than compared line by line.
const MAX_PAIRS: usize = 4_000_000;

/// The lines of a text; one that ends in a line break has no empty line after it.
fn lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)).collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// For each old line of the middle part, whether it is kept, and the same for the new.
fn kept(old: &[&str], new: &[&str]) -> (Vec<bool>, Vec<bool>) {
    let (n, m) = (old.len(), new.len());
    let (mut old_kept, mut new_kept) = (vec![false; n], vec![false; m]);
    if n == 0 || m == 0 || n.saturating_mul(m) > MAX_PAIRS {
        return (old_kept, new_kept);
    }
    // The longest run of lines both have, in order: lengths from each pair on.
    let width = m + 1;
    let mut longest = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            longest[i * width + j] = if old[i] == new[j] {
                longest[(i + 1) * width + j + 1] + 1
            } else {
                longest[(i + 1) * width + j].max(longest[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            (old_kept[i], new_kept[j]) = (true, true);
            (i, j) = (i + 1, j + 1);
        } else if longest[(i + 1) * width + j] >= longest[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (old_kept, new_kept)
}

/// The runs of lines of `new` that are not in `old` as they stand, from the
/// top down.
pub fn changes(old: &str, new: &str) -> Vec<Change> {
    let (old, new) = (lines(old), lines(new));
    // Most of a file is the same at both ends; only the middle is compared.
    let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let tail = old[head..].iter().rev().zip(new[head..].iter().rev()).take_while(|(a, b)| a == b).count();
    let (old_middle, new_middle) = (&old[head..old.len() - tail], &new[head..new.len() - tail]);
    let (old_kept, new_kept) = kept(old_middle, new_middle);

    let mut found = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old_middle.len() || j < new_middle.len() {
        if i < old_middle.len() && j < new_middle.len() && old_kept[i] && new_kept[j] {
            (i, j) = (i + 1, j + 1);
            continue;
        }
        // A stretch between two kept lines: what went, and what came.
        let (gone_from, came_from) = (i, j);
        while i < old_middle.len() && !old_kept[i] {
            i += 1;
        }
        while j < new_middle.len() && !new_kept[j] {
            j += 1;
        }
        let (gone, came) = (i - gone_from, j - came_from);
        let at = |line: usize| (head + line) as u32;
        match (gone, came) {
            (0, 0) => break,
            (_, 0) => found.push(Change { kind: Kind::Deleted, start: at(came_from), end: at(came_from) }),
            (0, _) => found.push(Change { kind: Kind::Added, start: at(came_from), end: at(j) }),
            // As many lines as went are changed ones; any more are new.
            _ => {
                let changed = came_from + gone.min(came);
                found.push(Change { kind: Kind::Modified, start: at(came_from), end: at(changed) });
                if came > gone {
                    found.push(Change { kind: Kind::Added, start: at(changed), end: at(j) });
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(kind: Kind, start: u32, end: u32) -> Change {
        Change { kind, start, end }
    }

    #[test]
    fn added_changed_and_removed_lines_are_told_apart() {
        let old = "one\ntwo\nthree\nfour\nfive\n";
        assert_eq!(changes(old, old), []);
        // Line endings of the other kind are not a change.
        assert_eq!(changes(old, &old.replace('\n', "\r\n")), []);
        assert_eq!(changes(old, "one\ntwo\nnew\nthree\nfour\nfive\n"), [change(Kind::Added, 2, 3)]);
        assert_eq!(changes(old, "one\nTWO\nthree\nfour\nfive\n"), [change(Kind::Modified, 1, 2)]);
        assert_eq!(changes(old, "one\ntwo\nfour\nfive\n"), [change(Kind::Deleted, 2, 2)]);
        assert_eq!(changes(old, "one\ntwo\nthree\nfour\n"), [change(Kind::Deleted, 4, 4)]);
        assert_eq!(changes(old, "two\nthree\nfour\nfive\n"), [change(Kind::Deleted, 0, 0)]);
        // One line became three: one changed, two new.
        assert_eq!(
            changes(old, "one\na\nb\nc\nthree\nfour\nfive\n"),
            [change(Kind::Modified, 1, 2), change(Kind::Added, 2, 4)]
        );
        // Several places at once.
        assert_eq!(
            changes(old, "zero\none\ntwo\nthree\nFOUR\nfive\nsix\n"),
            [change(Kind::Added, 0, 1), change(Kind::Modified, 4, 5), change(Kind::Added, 6, 7)]
        );
        assert_eq!(changes("", "a\nb\n"), [change(Kind::Added, 0, 2)]);
        assert_eq!(changes("a\nb\n", ""), [change(Kind::Deleted, 0, 0)]);
    }
}
