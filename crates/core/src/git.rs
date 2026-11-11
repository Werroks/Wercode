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
            "1" => rest.splitn(8, ' ').nth(7).map(|path| (path, kind(&rest[..2]))),
            // "2 XY sub mH mI mW hH hI Xscore path", then the old path as its own record.
            "2" => {
                records.next();
                rest.splitn(9, ' ').nth(8).map(|path| (path, kind(&rest[..2])))
            }
            // "u XY sub m1 m2 m3 mW h1 h2 h3 path"
            "u" => rest.splitn(10, ' ').nth(9).map(|path| (path, FileStatus::Conflict)),
            "?" => Some((rest, FileStatus::Untracked)),
            _ => None,
        };
        let Some((path, file_status)) = entry else { continue };
        let full = root.join(path.trim_end_matches('/'));
        for folder in full.ancestors().skip(1) {
            let strongest = status.folders.entry(folder.to_path_buf()).or_insert(file_status);
            *strongest = (*strongest).max(file_status);
            if folder == root {
                break;
            }
        }
        status.files.insert(full, file_status);
    }
    status
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
        let _ = std::fs::remove_dir_all(&dir);
    }
}
