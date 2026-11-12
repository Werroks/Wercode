//! Repository state read by running the system `git`, the way the user would:
//! their config, credentials and hooks all apply, and nothing is linked in.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What happened to a file, strongest last: a folder shows the strongest
/// status found inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileStatus {
    Renamed,
    Added,
    Untracked,
    Modified,
    Deleted,
    Conflict,
}

impl FileStatus {
    /// The one-letter mark shown next to the file name.
    pub fn letter(self) -> &'static str {
        match self {
            FileStatus::Renamed => "R",
            FileStatus::Added => "A",
            FileStatus::Untracked => "U",
            FileStatus::Modified => "M",
            FileStatus::Deleted => "D",
            FileStatus::Conflict => "!",
        }
    }
}

/// One changed file as the git panel lists it: what is already in the index
/// and what is still only in the working tree. A file can have both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Absolute path.
    pub path: PathBuf,
    pub staged: Option<FileStatus>,
    pub unstaged: Option<FileStatus>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoStatus {
    /// Top folder of the repository; file paths below are absolute.
    pub root: PathBuf,
    /// Branch name, or `None` when HEAD is detached.
    pub branch: Option<String>,
    /// Short commit id, empty in a repository without commits.
    pub head: String,
    pub has_upstream: bool,
    /// Commits not pushed yet.
    pub ahead: u32,
    /// Commits on the remote not pulled yet.
    pub behind: u32,
    pub files: BTreeMap<PathBuf, FileStatus>,
    /// The same files in the order git lists them, split by index and working tree.
    pub changes: Vec<Change>,
    /// Folders containing changed files, with the strongest status inside.
    pub folders: BTreeMap<PathBuf, FileStatus>,
}

impl RepoStatus {
    pub fn changed(&self) -> usize {
        self.files.len()
    }

    pub fn conflicts(&self) -> usize {
        self.files.values().filter(|s| **s == FileStatus::Conflict).count()
    }

    pub fn staged(&self) -> impl Iterator<Item = &Change> {
        self.changes.iter().filter(|c| c.staged.is_some())
    }

    pub fn unstaged(&self) -> impl Iterator<Item = &Change> {
        self.changes.iter().filter(|c| c.unstaged.is_some())
    }

    pub fn file(&self, path: &Path) -> Option<FileStatus> {
        self.files.get(path).copied()
    }

    pub fn folder(&self, path: &Path) -> Option<FileStatus> {
        self.folders.get(path).copied()
    }
}

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).stdin(Stdio::null()).stderr(Stdio::null());
    // Reading status must never take the index lock away from the user's own git.
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: no console flashing up next to a GUI app.
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

fn output(cmd: &mut Command) -> Option<Vec<u8>> {
    let out = cmd.output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// Reads the status of the repository containing `dir`. `None` when `dir` is
/// not inside a repository or git is not installed. Blocks; call it off the
/// UI thread.
pub fn status(dir: &Path) -> Option<RepoStatus> {
    let top = output(git(dir).args(["rev-parse", "--show-toplevel"]))?;
    let root = PathBuf::from(String::from_utf8_lossy(&top).trim_end_matches(['\r', '\n']));
    let raw = output(git(dir).args(["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"]))?;
    Some(parse(&root, &String::from_utf8_lossy(&raw)))
}

fn kind(xy: &str) -> FileStatus {
    if xy.contains('D') {
        FileStatus::Deleted
    } else if xy.contains('M') || xy.contains('T') {
        FileStatus::Modified
    } else if xy.contains('R') || xy.contains('C') {
        FileStatus::Renamed
    } else {
        FileStatus::Added
    }
}

/// One side of the `XY` pair: `X` is the index, `Y` the working tree.
fn side(code: char) -> Option<FileStatus> {
    match code {
        'M' | 'T' => Some(FileStatus::Modified),
        'A' => Some(FileStatus::Added),
        'D' => Some(FileStatus::Deleted),
        'R' | 'C' => Some(FileStatus::Renamed),
        _ => None,
    }
}

fn sides(xy: &str) -> (Option<FileStatus>, Option<FileStatus>) {
    let mut codes = xy.chars();
    (codes.next().and_then(side), codes.next().and_then(side))
}

/// Parses `git status --porcelain=v2 --branch -z`.
pub fn parse(root: &Path, text: &str) -> RepoStatus {
    let mut status = RepoStatus { root: root.to_path_buf(), ..Default::default() };
    let mut records = text.split('\0');
    while let Some(record) = records.next() {
        let (tag, rest) = record.split_once(' ').unwrap_or((record, ""));
        let entry = match tag {
            "#" => {
                let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
                match key {
                    "branch.oid" if value != "(initial)" => status.head = value.chars().take(7).collect(),
                    "branch.head" if value != "(detached)" => status.branch = Some(value.to_string()),
                    "branch.upstream" => status.has_upstream = true,
                    "branch.ab" => {
                        let mut parts = value.split(' ');
                        let num = |s: Option<&str>| s.and_then(|s| s[1..].parse().ok()).unwrap_or(0);
                        status.ahead = num(parts.next());
                        status.behind = num(parts.next());
                    }
                    _ => {}
                }
                continue;
            }
            // "1 XY sub mH mI mW hH hI path"
            "1" => rest.splitn(8, ' ').nth(7).map(|path| (path, kind(&rest[..2]), sides(&rest[..2]))),
            // "2 XY sub mH mI mW hH hI Xscore path", then the old path as its own record.
            "2" => {
                records.next();
                rest.splitn(9, ' ').nth(8).map(|path| (path, kind(&rest[..2]), sides(&rest[..2])))
            }
            // "u XY sub m1 m2 m3 mW h1 h2 h3 path"
            "u" => rest
                .splitn(10, ' ')
                .nth(9)
                .map(|path| (path, FileStatus::Conflict, (None, Some(FileStatus::Conflict)))),
            "?" => Some((rest, FileStatus::Untracked, (None, Some(FileStatus::Untracked)))),
            _ => None,
        };
        let Some((path, file_status, (staged, unstaged))) = entry else { continue };
        let full = root.join(path.trim_end_matches('/'));
        for folder in full.ancestors().skip(1) {
            let strongest = status.folders.entry(folder.to_path_buf()).or_insert(file_status);
            *strongest = (*strongest).max(file_status);
            if folder == root {
                break;
            }
        }
        status.changes.push(Change { path: full.clone(), staged, unstaged });
        status.files.insert(full, file_status);
    }
    status
}

/// Runs a git command that changes the repository and returns what git said
/// when it failed. Blocks; call it off the UI thread.
fn run(dir: &Path, args: &[&str], paths: &[PathBuf]) -> Result<(), String> {
    let mut cmd = git(dir);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    // Never wait for an answer nobody can type: no editor, no password prompt.
    cmd.env("GIT_EDITOR", "true").env("GIT_TERMINAL_PROMPT", "0");
    if !paths.is_empty() {
        cmd.arg("--").args(paths);
    }
    let out = cmd.output().map_err(|e| format!("Could not run git: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let said = |bytes: &[u8]| String::from_utf8_lossy(bytes).trim().to_string();
    let (err, text) = (said(&out.stderr), said(&out.stdout));
    Err(if !err.is_empty() {
        err
    } else if !text.is_empty() {
        text
    } else {
        format!("git {} failed", args.first().copied().unwrap_or_default())
    })
}

fn has_commits(dir: &Path) -> bool {
    output(git(dir).args(["rev-parse", "--verify", "--quiet", "HEAD"])).is_some()
}

/// Puts the files into the index, deletions included. No paths means everything.
pub fn stage(dir: &Path, paths: &[PathBuf]) -> Result<(), String> {
    run(dir, &["add", "--all"], paths)
}

/// Takes the files out of the index, leaving the working tree alone. No paths
/// means everything.
pub fn unstage(dir: &Path, paths: &[PathBuf]) -> Result<(), String> {
    if has_commits(dir) {
        run(dir, &["reset", "--quiet"], paths)
    } else if paths.is_empty() {
        // Before the first commit there is nothing to reset to.
        run(dir, &["rm", "--cached", "--quiet", "-r", "."], &[])
    } else {
        run(dir, &["rm", "--cached", "--quiet"], paths)
    }
}

/// Commits what is in the index. The user's hooks and signing settings apply.
pub fn commit(dir: &Path, message: &str) -> Result<(), String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("Write a commit message first.".into());
    }
    run(dir, &["commit", "--quiet", "-m", message], &[])
}

/// The changes to one file as a patch. `staged` compares the index with the
/// last commit; otherwise the working tree with the index. A file git does
/// not track yet is shown as all new.
pub fn diff(dir: &Path, path: &Path, staged: bool, untracked: bool) -> Result<String, String> {
    let mut cmd = git(dir);
    cmd.args(["-c", "core.quotepath=false", "diff", "--no-color", "--no-ext-diff"]);
    if untracked {
        // Comparing with nothing; git reports "different" through its exit code.
        cmd.arg("--no-index").arg("--").arg(if cfg!(windows) { "NUL" } else { "/dev/null" }).arg(path);
    } else {
        if staged {
            cmd.arg("--cached");
        }
        cmd.arg("--").arg(path);
    }
    let out = cmd.stdout(Stdio::piped()).output().map_err(|e| format!("Could not run git: {e}"))?;
    if out.status.success() || (untracked && out.status.code() == Some(1)) {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err("git could not compare this file.".into())
    }
}

/// Throws away the changes to tracked files in the working tree. What is
/// staged stays staged. This cannot be undone.
pub fn discard(dir: &Path, paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    run(dir, &["checkout", "--quiet"], paths)
}

/// Sends the branch to its remote. A branch that was never pushed goes to
/// `origin` under its own name and starts following it.
pub fn push(dir: &Path, has_upstream: bool) -> Result<(), String> {
    if has_upstream {
        run(dir, &["push", "--quiet"], &[])
    } else {
        run(dir, &["push", "--quiet", "--set-upstream", "origin", "HEAD"], &[])
    }
}

/// Brings in the remote's commits the way the user's git is set up to.
pub fn pull(dir: &Path) -> Result<(), String> {
    run(dir, &["pull", "--quiet"], &[])
}

pub fn fetch(dir: &Path) -> Result<(), String> {
    run(dir, &["fetch", "--quiet", "--all", "--prune"], &[])
}

/// Local branches, by name.
pub fn branches(dir: &Path) -> Vec<String> {
    output(git(dir).args(["for-each-ref", "--format=%(refname:short)", "refs/heads"]))
        .map(|out| String::from_utf8_lossy(&out).lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Checks out an existing local branch. Git refuses when that would
/// overwrite changes, and says which files.
pub fn switch(dir: &Path, branch: &str) -> Result<(), String> {
    run(dir, &["switch", "--quiet", branch], &[])
}

/// Makes a branch at the current commit and switches to it.
pub fn create_branch(dir: &Path, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Write a name for the branch first.".into());
    }
    run(dir, &["switch", "--quiet", "-c", name], &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_and_files() {
        let text = concat!(
            "# branch.oid 7a4ad76f00000000000000000000000000000000\0",
            "# branch.head main\0",
            "# branch.upstream origin/main\0",
            "# branch.ab +2 -1\0",
            "1 .M N... 100644 100644 100644 aaa bbb crates/app/src/main.rs\0",
            "1 A. N... 000000 100644 100644 aaa bbb docs/new file.md\0",
            "2 R. N... 100644 100644 100644 aaa bbb R100 src/b.rs\0src/a.rs\0",
            "1 .D N... 100644 100644 000000 aaa bbb gone.txt\0",
            "u UU N... 100644 100644 100644 100644 aaa bbb ccc crates/app/both.rs\0",
            "? assets/themes/black.toml\0",
        );
        let root = Path::new("/repo");
        let s = parse(root, text);
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.head, "7a4ad76");
        assert!(s.has_upstream);
        assert_eq!((s.ahead, s.behind), (2, 1));
        assert_eq!(s.changed(), 6);
        assert_eq!(s.conflicts(), 1);
        assert_eq!(s.file(&root.join("crates/app/src/main.rs")), Some(FileStatus::Modified));
        assert_eq!(s.file(&root.join("docs/new file.md")), Some(FileStatus::Added));
        assert_eq!(s.file(&root.join("src/b.rs")), Some(FileStatus::Renamed));
        assert_eq!(s.file(&root.join("src/a.rs")), None);
        assert_eq!(s.file(&root.join("gone.txt")), Some(FileStatus::Deleted));
        assert_eq!(s.file(&root.join("assets/themes/black.toml")), Some(FileStatus::Untracked));
        // A folder carries the strongest status of what is inside it.
        assert_eq!(s.folder(&root.join("crates/app")), Some(FileStatus::Conflict));
        assert_eq!(s.folder(&root.join("crates/app/src")), Some(FileStatus::Modified));
        assert_eq!(s.folder(&root.join("assets")), Some(FileStatus::Untracked));
        assert_eq!(s.folder(root), Some(FileStatus::Conflict));
        // The panel's two lists: the index and the working tree.
        let names = |list: Vec<&Change>| -> Vec<String> {
            list.iter().map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned()).collect()
        };
        assert_eq!(names(s.staged().collect()), ["new file.md", "b.rs"]);
        assert_eq!(names(s.unstaged().collect()), ["main.rs", "gone.txt", "both.rs", "black.toml"]);
    }

    #[test]
    fn a_file_can_be_staged_and_changed_again() {
        let s = parse(Path::new("/r"), "1 MM N... 100644 100644 100644 aaa bbb a.rs\0");
        assert_eq!(s.changed(), 1);
        assert_eq!(s.staged().count(), 1);
        assert_eq!(s.unstaged().count(), 1);
    }

    #[test]
    fn detached_head_and_new_repository() {
        let s = parse(Path::new("/r"), "# branch.oid (initial)\0# branch.head (detached)\0");
        assert_eq!(s.branch, None);
        assert_eq!(s.head, "");
        assert!(!s.has_upstream);
        assert_eq!(s.changed(), 0);
    }

    #[test]
    fn reads_a_real_repository() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("wercode-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        assert!(status(&dir).is_none() || status(&dir).is_some_and(|s| s.root != dir));
        assert!(git(&dir).args(["init", "-q", "-b", "trunk"]).status().unwrap().success());
        std::fs::write(dir.join("sub").join("a.txt"), "hi").unwrap();

        let s = status(&dir.join("sub")).expect("inside a repository");
        assert_eq!(s.branch.as_deref(), Some("trunk"));
        assert_eq!(s.changed(), 1);
        assert_eq!(s.files.values().next(), Some(&FileStatus::Untracked));

        // Stage, unstage and commit, before and after the first commit.
        let file = s.changes[0].path.clone();
        for (key, value) in [("user.name", "Test"), ("user.email", "test@example.com"), ("commit.gpgsign", "false")] {
            assert!(git(&dir).args(["config", key, value]).status().unwrap().success());
        }
        for round in 0..2 {
            if round == 1 {
                std::fs::write(&file, "again").unwrap();
            }
            stage(&dir, &[file.clone()]).unwrap();
            let s = status(&dir).unwrap();
            assert_eq!((s.staged().count(), s.unstaged().count()), (1, 0), "round {round}");
            unstage(&dir, &[file.clone()]).unwrap();
            let s = status(&dir).unwrap();
            assert_eq!((s.staged().count(), s.unstaged().count()), (0, 1), "round {round}");
            stage(&dir, &[]).unwrap();
            assert!(commit(&dir, "  ").is_err());
            commit(&dir, &format!("Commit {round}")).unwrap();
            let s = status(&dir).unwrap();
            assert_eq!(s.changed(), 0, "round {round}");
            assert!(!s.head.is_empty());
        }
        // Nothing staged: git refuses, and its words come back.
        assert!(!commit(&dir, "Empty").unwrap_err().is_empty());

        // Patches for a changed, a staged and a new file.
        std::fs::write(&file, "again\nand more\n").unwrap();
        let new = dir.join("new file.txt");
        std::fs::write(&new, "brand new\n").unwrap();
        let patch = diff(&dir, &file, false, false).unwrap();
        assert!(patch.contains("+and more") && patch.contains("a.txt"), "{patch}");
        assert_eq!(diff(&dir, &file, true, false).unwrap(), "");
        let patch = diff(&dir, &new, false, true).unwrap();
        assert!(patch.contains("+brand new"), "{patch}");
        stage(&dir, &[file.clone()]).unwrap();
        assert!(diff(&dir, &file, true, false).unwrap().contains("+and more"));
        assert_eq!(diff(&dir, &file, false, false).unwrap(), "");
        unstage(&dir, &[file.clone()]).unwrap();

        // Discarding puts the committed text back and leaves other files alone.
        discard(&dir, &[file.clone()]).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "again");
        assert!(new.is_file());

        // Branches: list, create, switch; a missing one is an error with git's words.
        assert_eq!(branches(&dir), ["trunk"]);
        create_branch(&dir, " feature/one ").unwrap();
        assert_eq!(status(&dir).unwrap().branch.as_deref(), Some("feature/one"));
        assert_eq!(branches(&dir), ["feature/one", "trunk"]);
        switch(&dir, "trunk").unwrap();
        assert_eq!(status(&dir).unwrap().branch.as_deref(), Some("trunk"));
        assert!(!switch(&dir, "nope").unwrap_err().is_empty());
        assert!(create_branch(&dir, "  ").is_err());
        assert!(!create_branch(&dir, "trunk").unwrap_err().is_empty());

        // Push and fetch against a remote that is a folder next door.
        let remote = dir.with_extension("remote");
        let _ = std::fs::remove_dir_all(&remote);
        assert!(Command::new("git").args(["init", "-q", "--bare"]).arg(&remote).status().unwrap().success());
        assert!(git(&dir).args(["remote", "add", "origin"]).arg(&remote).status().unwrap().success());
        assert!(!status(&dir).unwrap().has_upstream);
        push(&dir, false).unwrap();
        let s = status(&dir).unwrap();
        assert!(s.has_upstream);
        assert_eq!((s.ahead, s.behind), (0, 0));
        stage(&dir, &[]).unwrap();
        commit(&dir, "One more").unwrap();
        assert_eq!(status(&dir).unwrap().ahead, 1);
        push(&dir, true).unwrap();
        assert_eq!(status(&dir).unwrap().ahead, 0);
        fetch(&dir).unwrap();
        pull(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&remote);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
