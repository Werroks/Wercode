//! The commit history and its graph: which lane each commit sits in and which
//! lines join it to its parents, the way VS Code and gitk draw it.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::git::FileStatus;
use crate::i18n::{t, tf};

/// A branch, remote branch or tag that points at a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// The branch checked out now (or HEAD itself when it is detached).
    Head,
    Branch,
    Remote,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    pub name: String,
    pub kind: RefKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    /// Seconds since 1970.
    pub time: i64,
    pub refs: Vec<Ref>,
    /// The first line of the message.
    pub subject: String,
}

impl Commit {
    pub fn short(&self) -> &str {
        &self.hash[..self.hash.len().min(7)]
    }
}

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).stdin(Stdio::null()).stderr(Stdio::null()).stdout(Stdio::piped());
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: no console flashing up next to a GUI app.
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Fields are split by 0x1F and commits by 0x1E, which never occur in them.
const FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%at%x1f%D%x1f%s%x1e";

/// The history of the current branch, and of the branch it follows when
/// there is one, newest first. Empty outside a repository or before the
/// first commit. Blocks; call it off the UI thread.
pub fn log(dir: &Path, with_upstream: bool, limit: usize) -> Vec<Commit> {
    let mut cmd = git(dir);
    cmd.args(["-c", "log.showSignature=false", "log", "--topo-order", "--decorate=full", "--no-color", FORMAT])
        .arg(format!("--max-count={limit}"))
        .arg("HEAD");
    if with_upstream {
        cmd.arg("@{upstream}");
    }
    cmd.arg("--");
    match cmd.output() {
        Ok(out) if out.status.success() => parse_log(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

/// Parses what [`log`] asks git for.
pub fn parse_log(text: &str) -> Vec<Commit> {
    text.split('\x1e')
        .filter_map(|record| {
            let record = record.trim_start_matches(['\r', '\n']);
            if record.is_empty() {
                return None;
            }
            let mut fields = record.split('\x1f');
            let hash = fields.next()?.to_string();
            let parents = fields.next()?.split_whitespace().map(str::to_string).collect();
            let author = fields.next()?.to_string();
            let time = fields.next()?.trim().parse().unwrap_or(0);
            let refs = parse_refs(fields.next()?);
            let subject = fields.next().unwrap_or_default().trim_end().to_string();
            Some(Commit { hash, parents, author, time, refs, subject })
        })
        .collect()
}

/// `HEAD -> refs/heads/main, refs/remotes/origin/main, tag: refs/tags/v1`.
fn parse_refs(text: &str) -> Vec<Ref> {
    text.split(", ")
        .filter_map(|part| {
            let part = part.trim();
            let (name, kind) = if let Some(branch) = part.strip_prefix("HEAD -> refs/heads/") {
                (branch, RefKind::Head)
            } else if part == "HEAD" {
                ("HEAD", RefKind::Head)
            } else if let Some(tag) = part.strip_prefix("tag: refs/tags/") {
                (tag, RefKind::Tag)
            } else if let Some(branch) = part.strip_prefix("refs/heads/") {
                (branch, RefKind::Branch)
            } else if let Some(remote) = part.strip_prefix("refs/remotes/") {
                // origin/HEAD only repeats where origin's default branch is.
                if remote.ends_with("/HEAD") {
                    return None;
                }
                (remote, RefKind::Remote)
            } else {
                return None;
            };
            Some(Ref { name: name.to_string(), kind })
        })
        .collect()
}

/// A file a commit changed, by its path inside the repository (with `/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFile {
    pub path: String,
    pub status: FileStatus,
}

/// The files a commit changed, compared with its first parent.
pub fn commit_files(dir: &Path, commit: &Commit) -> Result<Vec<CommitFile>, String> {
    let mut cmd = git(dir);
    cmd.args(["-c", "core.quotepath=false"]);
    match commit.parents.first() {
        Some(parent) => cmd.args(["diff", "--no-color", "--name-status", "-z", parent, &commit.hash]),
        None => cmd.args(["diff-tree", "-r", "--root", "--no-commit-id", "--name-status", "-z", &commit.hash]),
    };
    let out = cmd.output().map_err(|e| tf("Could not run git: {error}", &[("error", &e)]))?;
    if !out.status.success() {
        return Err(t("git could not list the files of this commit.").into());
    }
    Ok(parse_name_status(&String::from_utf8_lossy(&out.stdout)))
}

/// `--name-status -z`: a status, then one path, or two for a rename or copy.
pub fn parse_name_status(text: &str) -> Vec<CommitFile> {
    let mut fields = text.split('\0').filter(|f| !f.is_empty());
    let mut files = Vec::new();
    while let Some(code) = fields.next() {
        let status = match code.chars().next() {
            Some('A') => FileStatus::Added,
            Some('D') => FileStatus::Deleted,
            Some('R') | Some('C') => FileStatus::Renamed,
            Some('U') => FileStatus::Conflict,
            _ => FileStatus::Modified,
        };
        if status == FileStatus::Renamed {
            fields.next();
        }
        let Some(path) = fields.next() else { break };
        files.push(CommitFile { path: path.to_string(), status });
    }
    files
}

/// What a commit changed in one file, as a patch, against its first parent.
pub fn commit_diff(dir: &Path, hash: &str, parent: Option<&str>, path: &Path) -> Result<String, String> {
    let mut cmd = git(dir);
    cmd.args(["-c", "core.quotepath=false"]);
    match parent {
        Some(parent) => cmd.args(["diff", "--no-color", "--no-ext-diff", parent, hash]),
        // The first commit: everything in it is new.
        None => cmd.args(["show", "--no-color", "--no-ext-diff", "--format=", hash]),
    };
    let out = cmd.arg("--").arg(path).output().map_err(|e| tf("Could not run git: {error}", &[("error", &e)]))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(t("git could not show this change.").into())
    }
}

/// One row of the graph.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    /// The lane the commit's dot is in, and that lane's colour.
    pub lane: usize,
    pub color: usize,
    /// The commit has more than one parent.
    pub merge: bool,
    /// Lanes that pass by from the top edge to the bottom edge: (lane, colour).
    pub through: Vec<(usize, usize)>,
    /// Lines from the top edge into the dot: (lane they come from, colour).
    pub into: Vec<(usize, usize)>,
    /// Lines from the dot to the bottom edge: (lane they go to, colour).
    pub out: Vec<(usize, usize)>,
}

impl Row {
    /// How many lanes wide the row is.
    pub fn width(&self) -> usize {
        let lanes = self.through.iter().chain(&self.into).chain(&self.out).map(|(lane, _)| *lane);
        lanes.chain([self.lane]).max().unwrap_or(0) + 1
    }
}

/// Lays commits (newest first, parents after children) out in lanes. Each
/// lane waits for one commit; a commit takes the leftmost lane waiting for it
/// and hands that lane on to its first parent. Other parents of a merge get a
/// lane of their own, or join one already waiting for them.
pub fn layout(commits: &[Commit]) -> Vec<Row> {
    let mut lanes: Vec<Option<(&str, usize)>> = Vec::new();
    let mut next_color = 0;
    let mut new_color = || {
        next_color += 1;
        next_color - 1
    };
    let free = |lanes: &mut Vec<Option<(&str, usize)>>| match lanes.iter().position(Option::is_none) {
        Some(index) => index,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    };
    let mut rows = Vec::with_capacity(commits.len());
    for commit in commits {
        let waiting: Vec<usize> = (0..lanes.len()).filter(|&i| lanes[i].is_some_and(|(h, _)| h == commit.hash)).collect();
        let (lane, color) = match waiting.first() {
            Some(&i) => (i, lanes[i].map_or(0, |(_, c)| c)),
            // A commit nothing is waiting for starts a lane: a branch tip.
            None => {
                let i = free(&mut lanes);
                (i, new_color())
            }
        };
        let mut row = Row { lane, color, merge: commit.parents.len() > 1, ..Row::default() };
        for (i, slot) in lanes.iter().enumerate() {
            let Some((_, c)) = slot else { continue };
            if waiting.contains(&i) {
                row.into.push((i, *c));
            } else if i != lane {
                row.through.push((i, *c));
            }
        }
        for &i in &waiting {
            lanes[i] = None;
        }
        if lane < lanes.len() {
            lanes[lane] = None;
        }
        for (n, parent) in commit.parents.iter().enumerate() {
            let joined = (n > 0).then(|| lanes.iter().position(|s| s.is_some_and(|(h, _)| h == parent))).flatten();
            let (i, c) = match joined {
                Some(i) => (i, lanes[i].map_or(0, |(_, c)| c)),
                None if n == 0 => {
                    while lanes.len() <= lane {
                        lanes.push(None);
                    }
                    (lane, color)
                }
                None => (free(&mut lanes), new_color()),
            };
            lanes[i] = Some((parent.as_str(), c));
            row.out.push((i, c));
        }
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        rows.push(row);
    }
    rows
}

/// How long ago something happened, in the largest unit that fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ago {
    JustNow,
    Minutes(u64),
    Hours(u64),
    Days(u64),
    Weeks(u64),
    Months(u64),
    Years(u64),
}

pub fn ago(then: i64, now: i64) -> Ago {
    let seconds = (now - then).max(0) as u64;
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    match () {
        _ if minutes < 1 => Ago::JustNow,
        _ if hours < 1 => Ago::Minutes(minutes),
        _ if days < 1 => Ago::Hours(hours),
        _ if days < 7 => Ago::Days(days),
        _ if days < 30 => Ago::Weeks(days / 7),
        _ if days < 365 => Ago::Months((days / 30).max(1)),
        _ => Ago::Years(days / 365),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(hash: &str, parents: &[&str]) -> Commit {
        Commit {
            hash: hash.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            author: "Test".into(),
            time: 0,
            refs: Vec::new(),
            subject: hash.into(),
        }
    }

    #[test]
    fn log_records_are_read_with_their_branches_and_tags() {
        let text = "aaa\x1fbbb ccc\x1fAda\x1f1700000000\x1fHEAD -> refs/heads/main, refs/remotes/origin/main, refs/remotes/origin/HEAD, tag: refs/tags/v1.0\x1fMerge feature/x\x1e\n\
                    bbb\x1f\x1fAda\x1f1690000000\x1frefs/heads/feature/x\x1fFirst\x1e\n";
        let commits = parse_log(text);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].parents, ["bbb", "ccc"]);
        assert_eq!(commits[0].time, 1_700_000_000);
        let refs: Vec<(&str, RefKind)> = commits[0].refs.iter().map(|r| (r.name.as_str(), r.kind)).collect();
        assert_eq!(refs, [("main", RefKind::Head), ("origin/main", RefKind::Remote), ("v1.0", RefKind::Tag)]);
        assert_eq!(commits[1].refs, [Ref { name: "feature/x".into(), kind: RefKind::Branch }]);
        assert!(commits[1].parents.is_empty());
        assert_eq!(commits[1].subject, "First");
    }

    #[test]
    fn a_straight_history_stays_in_one_lane() {
        let rows = layout(&[commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])]);
        assert!(rows.iter().all(|r| r.lane == 0 && r.color == 0 && r.through.is_empty()));
        assert_eq!(rows[0].into, []);
        assert_eq!(rows[0].out, [(0, 0)]);
        assert_eq!(rows[1].into, [(0, 0)]);
        assert_eq!(rows[2].out, [], "the first commit has nothing below it");
    }

    #[test]
    fn a_merged_branch_gets_a_lane_and_comes_back() {
        // m merges f into the line b; f and b both come from a.
        let rows = layout(&[
            commit("m", &["b", "f"]),
            commit("f", &["a"]),
            commit("b", &["a"]),
            commit("a", &[]),
        ]);
        assert!(rows[0].merge);
        assert_eq!(rows[0].out, [(0, 0), (1, 1)]);
        // f sits in the second lane while the first passes by.
        assert_eq!((rows[1].lane, rows[1].color), (1, 1));
        assert_eq!(rows[1].through, [(0, 0)]);
        assert_eq!((rows[2].lane, rows[2].through.as_slice()), (0, &[(1, 1)][..]));
        // Both lanes end in a.
        assert_eq!(rows[3].lane, 0);
        assert_eq!(rows[3].into, [(0, 0), (1, 1)]);
        assert_eq!(rows.iter().map(Row::width).max(), Some(2));
    }

    #[test]
    fn a_commit_nobody_waits_for_starts_its_own_lane() {
        // Two tips: the upstream is one commit ahead (u), the local branch one (l).
        let rows = layout(&[commit("u", &["a"]), commit("l", &["a"]), commit("a", &[])]);
        assert_eq!(rows[0].lane, 0);
        assert_eq!((rows[1].lane, rows[1].into.as_slice()), (1, &[][..]));
        assert_eq!(rows[2].into, [(0, 0), (1, 1)]);
    }

    #[test]
    fn name_status_reads_renames_by_their_new_name() {
        let files = parse_name_status("M\0src/a.rs\0R087\0old.rs\0new.rs\0A\0b.txt\0D\0c.txt\0");
        let got: Vec<(&str, FileStatus)> = files.iter().map(|f| (f.path.as_str(), f.status)).collect();
        assert_eq!(
            got,
            [
                ("src/a.rs", FileStatus::Modified),
                ("new.rs", FileStatus::Renamed),
                ("b.txt", FileStatus::Added),
                ("c.txt", FileStatus::Deleted)
            ]
        );
    }

    #[test]
    fn time_is_told_in_the_largest_unit() {
        let now = 1_000_000_000;
        assert_eq!(ago(now - 20, now), Ago::JustNow);
        assert_eq!(ago(now - 5 * 60, now), Ago::Minutes(5));
        assert_eq!(ago(now - 3 * 3600, now), Ago::Hours(3));
        assert_eq!(ago(now - 2 * 86400, now), Ago::Days(2));
        assert_eq!(ago(now - 15 * 86400, now), Ago::Weeks(2));
        assert_eq!(ago(now - 70 * 86400, now), Ago::Months(2));
        assert_eq!(ago(now - 800 * 86400, now), Ago::Years(2));
        assert_eq!(ago(now + 50, now), Ago::JustNow, "a clock a little ahead is not the future");
    }

    #[test]
    fn a_real_repository_gives_its_history_and_files() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("wercode-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(&dir).args(args).status().unwrap().success(), "{args:?}");
        };
        run(&["init", "-q", "-b", "trunk"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "First"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "Second"]);

        let commits = log(&dir, false, 50);
        assert_eq!(commits.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(), ["Second", "First"]);
        assert_eq!(commits[0].refs, [Ref { name: "trunk".into(), kind: RefKind::Head }]);
        let files = commit_files(&dir, &commits[0]).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(commit_files(&dir, &commits[1]).unwrap()[0].status, FileStatus::Added);
        let patch = commit_diff(&dir, &commits[0].hash, Some(&commits[1].hash), &dir.join("a.txt")).unwrap();
        assert!(patch.contains("+two"), "{patch}");
        let first = commit_diff(&dir, &commits[1].hash, None, Path::new("a.txt")).unwrap();
        assert!(first.contains("+one"), "{first}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
