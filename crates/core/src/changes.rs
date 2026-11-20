//! Every change in a repository at once, cut into the pieces git calls
//! hunks: what one list of all changes shows, and what its buttons do with a
//! single piece (put it in the index, take it out, or throw it away).
//!
//! The pieces come from `git diff` and go back through `git apply`, so line
//! endings, filters and the index are handled by git itself.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::git;
use crate::i18n::{t, tf};

/// Which changes: those still only in the folder, or those in the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Unstaged,
    Staged,
}

/// New files bigger than this are listed without their text.
const MAX_NEW_FILE: u64 = 1 << 20;
/// More new files than this are not read; they are counted instead.
const MAX_NEW_FILES: usize = 500;
/// A new file thrown away goes to the trash; one up to this size can also be
/// brought back with Undo.
const MAX_UNDO_FILE: usize = 16 << 20;

/// A line of a hunk, 1-based, on the side it is on: an unchanged or added
/// line counts on the new side, a removed one on the old side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    pub new: bool,
    pub number: u32,
}

/// One piece of a file's changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// From its `@@` line to its last line, as git wrote it.
    pub text: Vec<u8>,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// The first and the last line it shows.
    pub first: Line,
    pub last: Line,
}

impl Hunk {
    /// Whether the hunk reaches past `line` of the side it is counted on.
    pub fn ends_after(&self, line: Line) -> bool {
        if line.new { self.new_start + self.new_count > line.number } else { self.old_start + self.old_count > line.number }
    }
}

/// The changes to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    /// Absolute.
    pub path: PathBuf,
    /// The lines before the first hunk: `diff --git`, modes, `---` and `+++`.
    pub header: Vec<u8>,
    pub hunks: Vec<Hunk>,
    /// Git does not track the file yet; the patch is made up here, to show it as all new.
    pub untracked: bool,
    /// Git compares it as binary: there are no lines to show.
    pub binary: bool,
    /// A new file too large to show; only its name is listed.
    pub too_large: bool,
}

impl FilePatch {
    /// The whole patch of the file.
    pub fn text(&self) -> Vec<u8> {
        let mut out = self.header.clone();
        for hunk in &self.hunks {
            out.extend_from_slice(&hunk.text);
        }
        out
    }

    /// A patch with one hunk of the file only.
    pub fn hunk_patch(&self, ix: usize) -> Option<Vec<u8>> {
        let hunk = self.hunks.get(ix)?;
        let mut out = self.header.clone();
        out.extend_from_slice(&hunk.text);
        Some(out)
    }

    pub fn additions(&self) -> usize {
        self.count(b'+')
    }

    pub fn deletions(&self) -> usize {
        self.count(b'-')
    }

    fn count(&self, sign: u8) -> usize {
        self.hunks.iter().map(|h| h.text.split(|b| *b == b'\n').skip(1).filter(|l| l.first() == Some(&sign)).count()).sum()
    }
}

/// Every changed file on one side of the index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// The top folder of the repository, where git runs.
    pub top: PathBuf,
    pub files: Vec<FilePatch>,
    /// Files with merge conflicts; they are left out.
    pub conflicts: usize,
    /// New files past the limit; they are left out.
    pub hidden: usize,
}

/// Reads the changes of the repository containing `dir`. Blocks; call it off
/// the UI thread.
pub fn read(dir: &Path, side: Side) -> Result<Changes, String> {
    let top = git::top(dir).ok_or_else(|| t("This folder is not a git repository.").to_string())?;
    let mut cmd = git::git(&top);
    // The user's settings must not change the form of the patch: prefixes,
    // colours, outside programs and conversions all stay as git's own.
    cmd.args(["-c", "core.quotepath=false", "diff", "--no-color", "--no-ext-diff", "--no-textconv", "--no-renames"])
        .args(["--src-prefix=a/", "--dst-prefix=b/"]);
    if side == Side::Staged {
        cmd.arg("--cached");
    }
    let out = cmd.stdout(Stdio::piped()).output().map_err(|e| tf("Could not run git: {error}", &[("error", &e)]))?;
    if !out.status.success() {
        return Err(t("git could not list the changes.").into());
    }
    let (files, conflicts) = parse(&top, &out.stdout);
    let mut changes = Changes { top, files, conflicts, hidden: 0 };
    if side == Side::Unstaged {
        add_untracked(&mut changes);
    }
    Ok(changes)
}

/// The files git does not track yet and does not ignore, shown as all new.
fn add_untracked(changes: &mut Changes) {
    let listed = git::output(git::git(&changes.top).args(["-c", "core.quotepath=false", "ls-files", "--others", "--exclude-standard", "-z"]));
    let names: Vec<String> = listed
        .map(|out| String::from_utf8_lossy(&out).split('\0').filter(|n| !n.is_empty() && !n.ends_with('/')).map(str::to_string).collect())
        .unwrap_or_default();
    changes.hidden = names.len().saturating_sub(MAX_NEW_FILES);
    for name in names.into_iter().take(MAX_NEW_FILES) {
        let path = changes.top.join(&name);
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        let (bytes, too_large) =
            if meta.len() > MAX_NEW_FILE { (None, true) } else { (std::fs::read(&path).ok(), false) };
        let mut file = new_file(&name, bytes.as_deref());
        file.path = path;
        file.too_large = too_large;
        changes.files.push(file);
    }
}

/// The patch that creates a file with these contents; `None` for contents
/// not shown. Git writes the same for `git diff --no-index /dev/null file`.
fn new_file(name: &str, contents: Option<&[u8]>) -> FilePatch {
    // Git ends a name that has a space with a tab, so the name's end is clear.
    let tab = if name.contains(' ') { "\t" } else { "" };
    let mut header = format!("diff --git {} {}\nnew file mode 100644\n", quote(&format!("a/{name}")), quote(&format!("b/{name}")));
    let mut file = FilePatch {
        path: PathBuf::from(name),
        header: Vec::new(),
        hunks: Vec::new(),
        untracked: true,
        binary: false,
        too_large: false,
    };
    let Some(contents) = contents else {
        file.header = header.into_bytes();
        return file;
    };
    if contents.iter().take(8000).any(|b| *b == 0) {
        header.push_str(&format!("Binary files /dev/null and {} differ\n", quote(&format!("b/{name}"))));
        file.header = header.into_bytes();
        file.binary = true;
        return file;
    }
    if !contents.is_empty() {
        header.push_str(&format!("--- /dev/null\n+++ {}{tab}\n", quote(&format!("b/{name}"))));
    }
    file.header = header.into_bytes();
    let lines: Vec<&[u8]> = contents.split_inclusive(|b| *b == b'\n').collect();
    if lines.is_empty() {
        return file;
    }
    let count = lines.len() as u32;
    let mut text = format!("@@ -0,0 +1,{count} @@\n").into_bytes();
    for line in &lines {
        text.push(b'+');
        text.extend_from_slice(line);
    }
    if !contents.ends_with(b"\n") {
        text.extend_from_slice(b"\n\\ No newline at end of file\n");
    }
    file.hunks.push(Hunk {
        text,
        old_start: 0,
        old_count: 0,
        new_start: 1,
        new_count: count,
        first: Line { new: true, number: 1 },
        last: Line { new: true, number: count },
    });
    file
}

/// A name as git writes it in a patch: in double quotes with C escapes when
/// it has characters that would make the line ambiguous.
fn quote(name: &str) -> String {
    if !name.chars().any(|c| matches!(c, '"' | '\\' | '\t' | '\n' | '\r') || c.is_control()) {
        return name.to_string();
    }
    let mut out = String::from("\"");
    for c in name.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\{:03o}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A name from a patch: unquoted if git quoted it.
fn unquote(text: &str) -> String {
    let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) else { return text.to_string() };
    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.bytes().peekable();
    while let Some(b) = chars.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match chars.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'r') => bytes.push(b'\r'),
            Some(b'a') => bytes.push(7),
            Some(b'b') => bytes.push(8),
            Some(b'f') => bytes.push(12),
            Some(b'v') => bytes.push(11),
            Some(d @ b'0'..=b'7') => {
                let mut value = u32::from(d - b'0');
                for _ in 0..2 {
                    match chars.peek() {
                        Some(&d @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(d - b'0');
                            chars.next();
                        }
                        _ => break,
                    }
                }
                bytes.push(value as u8);
            }
            Some(other) => bytes.push(other),
            None => {}
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The file a header names, relative to the top of the repository.
fn header_name(header: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(header);
    let mut minus = None;
    for line in text.lines() {
        // A name with a space ends in a tab.
        let name = |rest: &str| unquote(rest.strip_suffix('\t').unwrap_or(rest));
        if let Some(rest) = line.strip_prefix("+++ ")
            && rest != "/dev/null"
        {
            return name(rest).strip_prefix("b/").map(str::to_string);
        }
        if let Some(rest) = line.strip_prefix("--- ")
            && rest != "/dev/null"
        {
            minus = name(rest).strip_prefix("a/").map(str::to_string);
        }
    }
    if minus.is_some() {
        return minus;
    }
    // No `---` and `+++`: a binary file or a change of mode only. Without
    // renames both names are the same, so the line is `a/NAME b/NAME`.
    let first = text.lines().next()?.strip_prefix("diff --git ")?;
    if first.starts_with('"') {
        let end = first[1..].find("\" ")? + 1;
        return unquote(&first[..=end]).strip_prefix("a/").map(str::to_string);
    }
    let half = first.len().checked_sub(5)? / 2;
    let (a, b) = (first.get(2..2 + half)?, first.get(half + 5..)?);
    (first.starts_with("a/") && a == b).then(|| a.to_string())
}

/// `@@ -a,b +c,d @@`: where the hunk starts on each side and how many lines it has.
fn hunk_range(line: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let text = std::str::from_utf8(line).ok()?;
    let mut parts = text.strip_prefix("@@ -")?.split(' ');
    let side = |part: &str| -> Option<(u32, u32)> {
        match part.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((part.parse().ok()?, 1)),
        }
    };
    let (old_start, old_count) = side(parts.next()?)?;
    let (new_start, new_count) = side(parts.next()?.strip_prefix('+')?)?;
    Some((old_start, old_count, new_start, new_count))
}

/// Cuts a patch from `git diff` into files and hunks. Files with merge
/// conflicts (a combined diff) are left out and counted.
pub fn parse(top: &Path, patch: &[u8]) -> (Vec<FilePatch>, usize) {
    let lines: Vec<&[u8]> = patch.split_inclusive(|b| *b == b'\n').collect();
    let mut sections: Vec<&[&[u8]]> = Vec::new();
    let mut start = None;
    for (ix, line) in lines.iter().enumerate() {
        if line.starts_with(b"diff ") {
            if let Some(from) = start {
                sections.push(&lines[from..ix]);
            }
            start = Some(ix);
        }
    }
    if let Some(from) = start {
        sections.push(&lines[from..]);
    }

    let (mut files, mut conflicts) = (Vec::new(), 0);
    for section in sections {
        if !section[0].starts_with(b"diff --git ") {
            conflicts += 1;
            continue;
        }
        let body = section.iter().position(|l| l.starts_with(b"@@ ")).unwrap_or(section.len());
        let header: Vec<u8> = section[..body].concat();
        let Some(name) = header_name(&header) else { continue };
        let binary = section[..body].iter().any(|l| l.starts_with(b"Binary files ") || l.starts_with(b"GIT binary patch"));
        let mut file = FilePatch {
            path: top.join(&name),
            header,
            hunks: Vec::new(),
            untracked: false,
            binary,
            too_large: false,
        };
        let mut rest = &section[body..];
        while let Some((&first, more)) = rest.split_first() {
            let Some((old_start, old_count, new_start, new_count)) = hunk_range(first) else {
                rest = more;
                continue;
            };
            let (mut old, mut new) = (old_start, new_start);
            let (mut old_left, mut new_left) = (old_count, new_count);
            let mut text = first.to_vec();
            let (mut first_line, mut last_line) = (None, None);
            let mut used = 0;
            for line in more {
                let done = old_left == 0 && new_left == 0;
                let shown = match line.first() {
                    Some(b'\\') => None,
                    _ if done => break,
                    Some(b'+') => {
                        new_left = new_left.saturating_sub(1);
                        new += 1;
                        Some(Line { new: true, number: new - 1 })
                    }
                    Some(b'-') => {
                        old_left = old_left.saturating_sub(1);
                        old += 1;
                        Some(Line { new: false, number: old - 1 })
                    }
                    Some(b' ') | Some(b'\n') | Some(b'\r') => {
                        (old_left, new_left) = (old_left.saturating_sub(1), new_left.saturating_sub(1));
                        (old, new) = (old + 1, new + 1);
                        Some(Line { new: true, number: new - 1 })
                    }
                    _ => break,
                };
                if let Some(shown) = shown {
                    first_line.get_or_insert(shown);
                    last_line = Some(shown);
                }
                text.extend_from_slice(line);
                used += 1;
            }
            rest = &more[used..];
            let fallback = Line { new: true, number: new_start.max(1) };
            file.hunks.push(Hunk {
                text,
                old_start,
                old_count,
                new_start,
                new_count,
                first: first_line.unwrap_or(fallback),
                last: last_line.unwrap_or(fallback),
            });
        }
        files.push(file);
    }
    (files, conflicts)
}

/// Feeds a patch to `git apply`: into the index with `cached`, backwards
/// (taking it out) with `reverse`.
fn apply(top: &Path, patch: &[u8], cached: bool, reverse: bool) -> Result<(), String> {
    let mut cmd = git::git(top);
    cmd.args(["apply", "--whitespace=nowarn"]);
    if cached {
        cmd.arg("--cached");
    }
    if reverse {
        cmd.arg("--reverse");
    }
    cmd.arg("-").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| tf("Could not run git: {error}", &[("error", &e)]))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(patch).map_err(|e| tf("Could not run git: {error}", &[("error", &e)]))?;
    }
    let out = child.wait_with_output().map_err(|e| tf("Could not run git: {error}", &[("error", &e)]))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if said.is_empty() { t("git could not apply the change.").to_string() } else { said })
}

/// What a discard took away, to put it back with Undo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undo {
    /// A patch that was taken out of the folder.
    Patch(Vec<u8>),
    /// A new file that went to the trash, with what was in it.
    File { path: PathBuf, bytes: Vec<u8> },
}

/// Puts one hunk into the index. A new file goes in whole.
pub fn stage_hunk(top: &Path, file: &FilePatch, ix: usize) -> Result<(), String> {
    if file.untracked {
        return git::stage(top, std::slice::from_ref(&file.path));
    }
    apply(top, &file.hunk_patch(ix).ok_or_else(|| t("This change is gone.").to_string())?, true, false)
}

/// Takes one hunk out of the index; the folder keeps it.
pub fn unstage_hunk(top: &Path, file: &FilePatch, ix: usize) -> Result<(), String> {
    apply(top, &file.hunk_patch(ix).ok_or_else(|| t("This change is gone.").to_string())?, true, true)
}

/// Throws one hunk away from the folder. A new file goes to the trash whole.
pub fn discard_hunk(top: &Path, file: &FilePatch, ix: usize) -> Result<Option<Undo>, String> {
    if file.untracked {
        return discard_new_file(file);
    }
    let patch = file.hunk_patch(ix).ok_or_else(|| t("This change is gone.").to_string())?;
    apply(top, &patch, false, true)?;
    Ok(Some(Undo::Patch(patch)))
}

/// Throws away all changes to a file in the folder; what is staged stays.
pub fn discard_file(top: &Path, file: &FilePatch) -> Result<Option<Undo>, String> {
    if file.untracked {
        return discard_new_file(file);
    }
    if file.hunks.is_empty() {
        // No lines to take back one by one: a binary file or a mode.
        git::discard(top, std::slice::from_ref(&file.path))?;
        return Ok(None);
    }
    let patch = file.text();
    apply(top, &patch, false, true)?;
    Ok(Some(Undo::Patch(patch)))
}

fn discard_new_file(file: &FilePatch) -> Result<Option<Undo>, String> {
    let bytes = std::fs::metadata(&file.path)
        .ok()
        .filter(|m| m.len() as usize <= MAX_UNDO_FILE)
        .and_then(|_| std::fs::read(&file.path).ok());
    crate::fs_ops::trash(&file.path)?;
    Ok(bytes.map(|bytes| Undo::File { path: file.path.clone(), bytes }))
}

/// Puts back what a discard took away. Fails if the file changed since in
/// a way that is in the way.
pub fn undo(top: &Path, undo: &Undo) -> Result<(), String> {
    match undo {
        Undo::Patch(patch) => apply(top, patch, false, false),
        Undo::File { path, bytes } => {
            if path.exists() {
                return Err(tf("{name} exists again; it was left as it is.", &[("name", &path.display())]));
            }
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(path, bytes).map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A changed file with two hunks, a deleted file with a space in its
    /// name, a conflict, a binary file and a change of mode.
    const LINES: &[&str] = &[
        "diff --git a/src/one.txt b/src/one.txt",
        "index 1111111..2222222 100644",
        "--- a/src/one.txt",
        "+++ b/src/one.txt",
        "@@ -1,3 +1,3 @@",
        " a",
        "-b",
        "+B",
        " c",
        "@@ -10,2 +10,3 @@ fn scope()",
        " j",
        "+new",
        " k",
        "\\ No newline at end of file",
        "diff --git a/my file.txt b/my file.txt",
        "deleted file mode 100644",
        "--- a/my file.txt\t",
        "+++ /dev/null",
        "@@ -1 +0,0 @@",
        "-gone",
        "diff --cc both.txt",
        "index 1,2..3",
        "--- a/both.txt",
        "+++ b/both.txt",
        "@@@ -1,1 -1,1 +1,5 @@@",
        "++<<<<<<< ours",
        "diff --git a/pic.png b/pic.png",
        "index 3333333..4444444 100644",
        "Binary files a/pic.png and b/pic.png differ",
        r#"diff --git "a/tab\there" "b/tab\there""#,
        "old mode 100644",
        "new mode 100755",
    ];

    #[test]
    fn a_patch_is_cut_into_files_and_hunks() {
        let top = Path::new("/repo");
        let patch: String = LINES.iter().map(|line| format!("{line}\n")).collect();
        let (files, conflicts) = parse(top, patch.as_bytes());
        assert_eq!(conflicts, 1);
        let names: Vec<_> = files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            names,
            ["src/one.txt", "my file.txt", "pic.png", "tab\there"].map(|n| top.join(n)).to_vec()
        );

        let one = &files[0];
        assert_eq!(one.hunks.len(), 2);
        assert_eq!((one.additions(), one.deletions()), (2, 1));
        let first = &one.hunks[0];
        assert_eq!((first.old_start, first.old_count, first.new_start, first.new_count), (1, 3, 1, 3));
        assert_eq!(first.first, Line { new: true, number: 1 });
        assert_eq!(first.last, Line { new: true, number: 3 });
        let second = &one.hunks[1];
        assert!(String::from_utf8_lossy(&second.text).ends_with("\\ No newline at end of file\n"));
        assert_eq!(second.last, Line { new: true, number: 12 });
        // One hunk keeps the header, so git can apply it alone.
        let alone = String::from_utf8(one.hunk_patch(1).unwrap()).unwrap();
        assert!(alone.starts_with("diff --git a/src/one.txt") && alone.contains("+++ b/src/one.txt\n@@ -10,2"));
        assert!(!alone.contains("-b\n"));
        assert_eq!(one.text(), patch.split("diff --git a/my").next().unwrap().as_bytes());

        let gone = &files[1];
        assert_eq!(gone.hunks[0].last, Line { new: false, number: 1 });
        assert!(files[2].binary && files[2].hunks.is_empty());
        assert!(files[3].hunks.is_empty() && !files[3].binary);

        assert!(gone.hunks[0].ends_after(Line { new: false, number: 1 }));
        assert!(!gone.hunks[0].ends_after(Line { new: false, number: 2 }));
    }

    #[test]
    fn a_new_file_reads_like_gits_own_patch() {
        let file = new_file("dir/a b.txt", Some(b"one\r\ntwo"));
        let text = String::from_utf8(file.text()).unwrap();
        assert_eq!(
            text,
            "diff --git a/dir/a b.txt b/dir/a b.txt\nnew file mode 100644\n--- /dev/null\n+++ b/dir/a b.txt\t\n\
             @@ -0,0 +1,2 @@\n+one\r\n+two\n\\ No newline at end of file\n"
        );
        let (parsed, _) = parse(Path::new("/r"), text.as_bytes());
        assert_eq!(parsed[0].path, Path::new("/r").join("dir/a b.txt"));
        assert_eq!(parsed[0].hunks, file.hunks);

        assert!(new_file("x.bin", Some(b"a\0b")).binary);
        assert!(new_file("empty", Some(b"")).hunks.is_empty());
        assert!(new_file("big", None).hunks.is_empty());
        assert_eq!(quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(unquote(&quote("tab\there\u{1}")), "tab\there\u{1}");
        assert_eq!(unquote("\"caf\\303\\251\""), "café");
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git").arg("-C").arg(dir).args(args).status().unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn text(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap().replace("\r\n", "\n")
    }

    #[test]
    fn hunks_are_staged_unstaged_discarded_and_brought_back() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("wercode-changes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        for (key, value) in [("user.name", "Test"), ("user.email", "test@example.com"), ("commit.gpgsign", "false")] {
            git(&dir, &["config", key, value]);
        }
        let lines: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        let file = dir.join("a.txt");
        std::fs::write(&file, &lines).unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "first"]);

        // Two places far apart: two hunks. And a new file.
        let changed = lines.replace("line 2\n", "line two\n").replace("line 28\n", "line 28\nadded\n");
        std::fs::write(&file, &changed).unwrap();
        let new = dir.join("new.txt");
        std::fs::write(&new, "fresh\n").unwrap();

        let changes = read(&dir, Side::Unstaged).unwrap();
        assert_eq!(changes.files.len(), 2);
        let a = &changes.files[0];
        assert_eq!(a.hunks.len(), 2);
        assert!(changes.files[1].untracked);
        assert!(read(&dir, Side::Staged).unwrap().files.is_empty());

        // Staging the second hunk leaves the first one in the folder only.
        stage_hunk(&changes.top, a, 1).unwrap();
        let staged = read(&dir, Side::Staged).unwrap();
        assert_eq!(staged.files.len(), 1);
        assert!(String::from_utf8_lossy(&staged.files[0].text()).contains("+added"));
        let unstaged = read(&dir, Side::Unstaged).unwrap();
        assert_eq!(unstaged.files[0].hunks.len(), 1);
        assert!(String::from_utf8_lossy(&unstaged.files[0].text()).contains("+line two"));

        // And back out of the index.
        unstage_hunk(&staged.top, &staged.files[0], 0).unwrap();
        assert!(read(&dir, Side::Staged).unwrap().files.is_empty());

        // Throwing the first hunk away keeps the second; Undo puts it back.
        let changes = read(&dir, Side::Unstaged).unwrap();
        let undo_patch = discard_hunk(&changes.top, &changes.files[0], 0).unwrap().unwrap();
        assert_eq!(text(&file), lines.replace("line 28\n", "line 28\nadded\n"));
        undo(&changes.top, &undo_patch).unwrap();
        assert_eq!(text(&file), changed);

        // A new file goes into the index whole, and to the trash whole.
        let changes = read(&dir, Side::Unstaged).unwrap();
        stage_hunk(&changes.top, &changes.files[1], 0).unwrap();
        assert_eq!(read(&dir, Side::Staged).unwrap().files.len(), 1);
        git(&dir, &["reset", "-q"]);

        // The whole file: every hunk goes, and comes back.
        let changes = read(&dir, Side::Unstaged).unwrap();
        let undo_file = discard_file(&changes.top, &changes.files[0]).unwrap().unwrap();
        assert_eq!(text(&file), lines);
        undo(&changes.top, &undo_file).unwrap();
        assert_eq!(text(&file), changed);

        // A patch that no longer fits is refused with git's words.
        let changes = read(&dir, Side::Unstaged).unwrap();
        std::fs::write(&file, &lines).unwrap();
        assert!(!discard_hunk(&changes.top, &changes.files[0], 0).unwrap_err().is_empty());

        // Before the first commit, everything staged counts as new.
        let fresh = dir.join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        git(&fresh, &["init", "-q"]);
        std::fs::write(fresh.join("b.txt"), "b\n").unwrap();
        git(&fresh, &["add", "."]);
        let staged = read(&fresh, Side::Staged).unwrap();
        assert_eq!(staged.files.len(), 1);
        unstage_hunk(&staged.top, &staged.files[0], 0).unwrap();
        assert!(read(&fresh, Side::Staged).unwrap().files.is_empty());
        assert!(read(&fresh, Side::Unstaged).unwrap().files[0].untracked);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
