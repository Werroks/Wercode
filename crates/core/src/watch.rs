//! Watching a project folder for changes made by other programs.
//!
//! Changes inside `.git` and in ignored folders (`target`, `node_modules`)
//! are not reported: a build writes thousands of files there and none of them
//! shows in the file tree.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{EventKind, RecursiveMode, Watcher as _};

/// A burst of changes is reported once, this long after it goes quiet.
const QUIET: Duration = Duration::from_millis(200);
/// A burst that never goes quiet is still reported this often.
const LONGEST_WAIT: Duration = Duration::from_secs(1);

/// The `.gitignore` files of a project, each with the folder it applies to.
pub struct Ignores {
    root: PathBuf,
    rules: Vec<(PathBuf, Gitignore)>,
}

impl Ignores {
    pub fn read(root: &Path) -> Self {
        let mut rules = Vec::new();
        let mut add = |dir: &Path, file: PathBuf| {
            let mut builder = GitignoreBuilder::new(dir);
            builder.add(file);
            if let Ok(rule) = builder.build()
                && !rule.is_empty()
            {
                rules.push((dir.to_path_buf(), rule));
            }
        };
        let exclude = root.join(".git").join("info").join("exclude");
        if exclude.is_file() {
            add(root, exclude);
        }
        // The walk itself skips ignored folders, so their own files are never read.
        let walker = ignore::WalkBuilder::new(root)
            .hidden(false)
            .require_git(false)
            .filter_entry(|e| e.file_name() != ".git")
            .build();
        for entry in walker.filter_map(Result::ok) {
            if entry.file_name() == ".gitignore"
                && let Some(dir) = entry.path().parent()
            {
                add(dir, entry.path().to_path_buf());
            }
        }
        Self { root: root.to_path_buf(), rules }
    }

    /// True for anything in `.git`, outside the project, or matched by a rule.
    pub fn is_ignored(&self, path: &Path) -> bool {
        let Ok(inside) = path.strip_prefix(&self.root) else { return true };
        if inside.components().any(|part| part.as_os_str() == ".git") {
            return true;
        }
        let is_dir = path.is_dir();
        self.rules.iter().any(|(dir, rule)| {
            path.strip_prefix(dir)
                .is_ok_and(|below| rule.matched_path_or_any_parents(below, is_dir).is_ignore())
        })
    }

    /// Folders that are not ignored, the project folder first.
    fn folders(&self) -> Vec<PathBuf> {
        ignore::WalkBuilder::new(&self.root)
            .hidden(false)
            .require_git(false)
            .filter_entry(|e| e.file_name() != ".git")
            .build()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_some_and(|t| t.is_dir()))
            .map(|e| e.into_path())
            .collect()
    }
}

/// How the folder is watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// One watch on the project folder covers everything below it. Cheap on
    /// Windows and macOS, where the system does the recursion.
    Recursive,
    /// One watch per folder that is not ignored. Linux counts every watched
    /// folder against a limit, and `target` alone could use it up.
    PerFolder,
}

impl Mode {
    pub fn for_this_system() -> Self {
        if cfg!(target_os = "linux") { Mode::PerFolder } else { Mode::Recursive }
    }
}

type Backend = Arc<Mutex<Option<notify::RecommendedWatcher>>>;

/// Keeps the watch alive. Dropping it stops the reports.
pub struct Watch {
    backend: Backend,
}

impl Drop for Watch {
    fn drop(&mut self) {
        // The thread below ends when the watcher, and with it the sender, is gone.
        if let Ok(mut backend) = self.backend.lock() {
            backend.take();
        }
    }
}

/// Watches `root` and calls `changed` after files in it were created, written,
/// renamed or deleted. Returns `None` when the system refuses to watch; the
/// caller then has to refresh some other way.
pub fn watch(root: &Path, changed: impl Fn() + Send + 'static) -> Option<Watch> {
    watch_as(root, Mode::for_this_system(), changed)
}

pub fn watch_as(root: &Path, mode: Mode, changed: impl Fn() + Send + 'static) -> Option<Watch> {
    let ignores = Arc::new(Mutex::new(Ignores::read(root)));
    let (sender, receiver) = mpsc::channel::<()>();

    let filter = ignores.clone();
    let handler = move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let Ok(mut ignores) = filter.lock() else { return };
        // New rules change what counts as ignored from here on.
        if event.paths.iter().any(|p| p.file_name().is_some_and(|n| n == ".gitignore")) {
            *ignores = Ignores::read(&ignores.root);
        }
        if event.paths.iter().any(|p| !ignores.is_ignored(p)) {
            let _ = sender.send(());
        }
    };
    let mut watcher = notify::recommended_watcher(handler).ok()?;
    let mut watched = HashSet::new();
    match mode {
        Mode::Recursive => watcher.watch(root, RecursiveMode::Recursive).ok()?,
        Mode::PerFolder => {
            watcher.watch(root, RecursiveMode::NonRecursive).ok()?;
            watched.insert(root.to_path_buf());
        }
    }
    let backend: Backend = Arc::new(Mutex::new(Some(watcher)));

    let for_thread = backend.clone();
    let add_folders = move |watched: &mut HashSet<PathBuf>| {
        if mode != Mode::PerFolder {
            return;
        }
        let folders = match ignores.lock() {
            Ok(ignores) => ignores.folders(),
            Err(_) => return,
        };
        let Ok(mut backend) = for_thread.lock() else { return };
        let Some(watcher) = backend.as_mut() else { return };
        watched.retain(|folder| folder.is_dir());
        for folder in folders {
            // A folder that cannot be watched (the limit is reached) is only
            // skipped: the rest of the tree keeps reporting.
            if !watched.contains(&folder) && watcher.watch(&folder, RecursiveMode::NonRecursive).is_ok() {
                watched.insert(folder);
            }
        }
    };
    add_folders(&mut watched);

    std::thread::Builder::new()
        .name("wercode-watch".into())
        .spawn(move || {
            while receiver.recv().is_ok() {
                let started = Instant::now();
                loop {
                    match receiver.recv_timeout(QUIET) {
                        Ok(()) if started.elapsed() < LONGEST_WAIT => {}
                        Ok(()) | Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                // A new folder has to be watched before anything is created in it.
                add_folders(&mut watched);
                changed();
            }
        })
        .ok()?;
    Some(Watch { backend })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("wercode-watch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src").join("inner")).unwrap();
        std::fs::create_dir_all(root.join("target").join("debug")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::write(root.join("src").join(".gitignore"), "generated/\n").unwrap();
        // Symbolic links in the temp folder must not make the paths differ.
        std::fs::canonicalize(&root).map(strip_verbatim).unwrap_or(root)
    }

    fn strip_verbatim(path: PathBuf) -> PathBuf {
        match path.to_string_lossy().strip_prefix(r"\\?\") {
            Some(rest) => PathBuf::from(rest),
            None => path,
        }
    }

    #[test]
    fn knows_what_is_ignored() {
        let root = project("rules");
        let ignores = Ignores::read(&root);
        assert!(!ignores.is_ignored(&root.join("src").join("main.rs")));
        assert!(!ignores.is_ignored(&root.join("new-folder")));
        assert!(ignores.is_ignored(&root.join("target").join("debug").join("app.exe")));
        assert!(ignores.is_ignored(&root.join("target")));
        assert!(ignores.is_ignored(&root.join("build.log")));
        assert!(ignores.is_ignored(&root.join(".git").join("index")));
        // A rule in a nested file applies below that file only.
        assert!(ignores.is_ignored(&root.join("src").join("generated").join("a.rs")));
        assert!(!ignores.is_ignored(&root.join("generated").join("a.rs")));
        assert!(ignores.is_ignored(&std::env::temp_dir()));

        let folders = ignores.folders();
        assert_eq!(folders[0], root);
        assert!(folders.contains(&root.join("src").join("inner")));
        assert!(!folders.iter().any(|f| f.starts_with(root.join("target")) || f.starts_with(root.join(".git"))));
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn reports_changes(mode: Mode, name: &str) {
        let root = project(name);
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let watch = watch_as(&root, mode, move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("the folder can be watched");
        let wait_for = |wanted: usize| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while count.load(Ordering::SeqCst) < wanted && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            count.load(Ordering::SeqCst)
        };
        // macOS also reports what happened shortly before the watch began (the
        // project being created just now), so let that pass and count from there.
        std::thread::sleep(Duration::from_millis(1500));
        let quiet = count.load(Ordering::SeqCst);

        // Ignored places stay silent, however much is written there.
        std::fs::write(root.join("target").join("debug").join("out.bin"), "x").unwrap();
        std::fs::write(root.join(".git").join("index"), "x").unwrap();
        std::fs::write(root.join("build.log"), "x").unwrap();
        std::thread::sleep(QUIET * 4);
        assert_eq!(count.load(Ordering::SeqCst), quiet, "{mode:?}");

        // A burst in a watched folder is reported once, not once per file. (A
        // folder's own change can arrive late through its parent's watch.)
        for name in ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"] {
            std::fs::write(root.join("src").join("inner").join(name), "x").unwrap();
        }
        assert!(wait_for(quiet + 1) > quiet, "{mode:?}");
        std::thread::sleep(QUIET * 3);
        let burst = count.load(Ordering::SeqCst);
        assert!(burst - quiet <= 2, "{mode:?}: {} reports for one burst", burst - quiet);

        // A folder made after the watch began is watched too.
        std::fs::create_dir(root.join("later")).unwrap();
        assert!(wait_for(burst + 1) > burst, "{mode:?}");
        std::thread::sleep(QUIET * 3);
        let before = count.load(Ordering::SeqCst);
        std::fs::write(root.join("later").join("d.rs"), "x").unwrap();
        assert!(wait_for(before + 1) > before, "{mode:?}");

        drop(watch);
        std::thread::sleep(QUIET * 2);
        let after = count.load(Ordering::SeqCst);
        std::fs::write(root.join("src").join("e.rs"), "x").unwrap();
        std::thread::sleep(QUIET * 4);
        assert_eq!(count.load(Ordering::SeqCst), after, "nothing is reported after the watch is dropped");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reports_changes_with_one_recursive_watch() {
        reports_changes(Mode::Recursive, "recursive");
    }

    #[test]
    fn reports_changes_with_a_watch_per_folder() {
        reports_changes(Mode::PerFolder, "folders");
    }
}
