//! Checking code with language servers: the programs editors talk to for
//! errors and warnings (`rust-analyzer`, `pyright`, `gopls` and the like).
//!
//! Wercode does not bundle them. A server installed on the computer is
//! found on `PATH` and started for the files of its language when one of them
//! is opened; what it reports is kept here by file. Each server runs in its
//! own process with a thread here that talks to it, so nothing waits for a
//! server on the thread that draws the window.

mod client;
#[doc(hidden)]
pub mod fake;
mod rpc;
mod specs;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use specs::{Program, Spec, builtin, find_program, language_id, language_name, root_for};

use client::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

/// One error, warning or note a server reported for a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Where it starts and ends: 0-based lines, columns in characters.
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub severity: Severity,
    pub message: String,
    /// Which tool said it: `rustc`, `clippy`, `Pyright`, `eslint`.
    pub source: Option<String>,
    /// The tool's own name for the rule, such as `E0308` or `unused_imports`.
    pub code: Option<String>,
}

/// How a server is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Being looked for, started, or waiting for its first answer.
    Starting,
    /// Running and working on something it described, such as indexing.
    Busy(String),
    Ready,
    /// Not installed on this computer.
    Missing,
    /// It could not start, or stopped by itself; why.
    Failed(String),
}

/// What changed. Read the new state from [`Servers`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// What the servers say about this file changed.
    Problems(PathBuf),
    /// A server started, stopped, or is busy with something else.
    Status,
}

/// What checks a file: its language and each server for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The language, as people write it.
    pub language: &'static str,
    /// The servers for it, by name.
    pub servers: Vec<(String, Status)>,
    /// When no server for the language is installed: a command that installs one.
    pub install: Option<String>,
}

/// An open file.
struct Doc {
    project: PathBuf,
    language: &'static str,
    /// The text as last given, for a server that starts later.
    text: String,
    /// The servers it was sent to, by id.
    servers: Vec<usize>,
    /// How many editors have it open.
    opened: usize,
}

struct Server {
    id: usize,
    /// Index into the list of specs.
    spec: usize,
    root: PathBuf,
    project: PathBuf,
    name: String,
    status: Status,
    commands: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct State {
    enabled: bool,
    next_id: usize,
    servers: Vec<Server>,
    docs: HashMap<PathBuf, Doc>,
    /// What each server reported for each file, by server id.
    problems: HashMap<PathBuf, BTreeMap<usize, Vec<Problem>>>,
    /// Open projects, with how many windows show each.
    projects: HashMap<PathBuf, usize>,
}

struct Shared {
    specs: Vec<Spec>,
    notify: Box<dyn Fn(Event) + Send + Sync>,
    state: Mutex<State>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Called by a server's thread.
    fn set_status(&self, id: usize, status: Status, name: Option<String>) {
        {
            let mut state = self.lock();
            let Some(server) = state.servers.iter_mut().find(|s| s.id == id) else { return };
            if server.status == status && name.is_none() {
                return;
            }
            server.status = status;
            if let Some(name) = name {
                server.name = name;
            }
        }
        (self.notify)(Event::Status);
    }

    /// Called by a server's thread with everything it now says about one file.
    fn set_problems(&self, id: usize, path: PathBuf, problems: Vec<Problem>) {
        {
            let mut state = self.lock();
            if !state.servers.iter().any(|s| s.id == id) {
                return;
            }
            let by_server = state.problems.entry(path.clone()).or_default();
            let changed = if problems.is_empty() {
                by_server.remove(&id).is_some()
            } else {
                by_server.insert(id, problems.clone()).as_ref() != Some(&problems)
            };
            if by_server.is_empty() {
                state.problems.remove(&path);
            }
            if !changed {
                return;
            }
        }
        (self.notify)(Event::Problems(path));
    }

    /// Called by a server's thread when the server stopped by itself.
    fn server_ended(&self, id: usize, status: Status) {
        let forgotten = {
            let mut state = self.lock();
            let Some(server) = state.servers.iter_mut().find(|s| s.id == id) else { return };
            server.status = status;
            self.forget(&mut state, id)
        };
        self.report_forgotten(forgotten);
        (self.notify)(Event::Status);
    }

    /// Forgets what a server said, once it is gone.
    fn forget(&self, state: &mut State, id: usize) -> Vec<PathBuf> {
        let mut changed = Vec::new();
        state.problems.retain(|path, by_server| {
            if by_server.remove(&id).is_some() {
                changed.push(path.clone());
            }
            !by_server.is_empty()
        });
        changed
    }

    fn report_forgotten(&self, paths: Vec<PathBuf>) {
        for path in paths {
            (self.notify)(Event::Problems(path));
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        for server in &self.state.get_mut().map(|s| std::mem::take(&mut s.servers)).unwrap_or_default() {
            let _ = server.commands.send(Command::Stop);
        }
    }
}

/// Keeps a project's servers running; they stop when the last guard for it
/// is dropped.
pub struct ProjectGuard {
    shared: Arc<Shared>,
    root: PathBuf,
}

impl Drop for ProjectGuard {
    fn drop(&mut self) {
        let forgotten = {
            let mut state = self.shared.lock();
            let Some(count) = state.projects.get_mut(&self.root) else { return };
            *count -= 1;
            if *count > 0 {
                return;
            }
            state.projects.remove(&self.root);
            state.docs.retain(|_, doc| doc.project != self.root);
            let (gone, kept): (Vec<_>, Vec<_>) =
                std::mem::take(&mut state.servers).into_iter().partition(|s| s.project == self.root);
            state.servers = kept;
            let mut forgotten = Vec::new();
            for server in gone {
                let _ = server.commands.send(Command::Stop);
                forgotten.extend(self.shared.forget(&mut state, server.id));
            }
            forgotten
        };
        self.shared.report_forgotten(forgotten);
        (self.shared.notify)(Event::Status);
    }
}

/// The language servers of every window. Cloning gives another handle to
/// the same servers.
#[derive(Clone)]
pub struct Servers {
    shared: Arc<Shared>,
}

impl Servers {
    /// `notify` is called from other threads whenever something changes.
    pub fn new(specs: Vec<Spec>, notify: impl Fn(Event) + Send + Sync + 'static) -> Self {
        let state = State { enabled: true, ..State::default() };
        Self { shared: Arc::new(Shared { specs, notify: Box::new(notify), state: Mutex::new(state) }) }
    }

    pub fn enabled(&self) -> bool {
        self.shared.lock().enabled
    }

    /// Off stops every server and forgets what they said; on starts them
    /// again for the files that are open.
    pub fn set_enabled(&self, on: bool) {
        if self.shared.lock().enabled == on {
            return;
        }
        self.shared.lock().enabled = on;
        if on {
            self.restart();
        } else {
            self.stop_servers();
        }
    }

    /// Marks a project as open in a window.
    pub fn project(&self, root: &Path) -> ProjectGuard {
        *self.shared.lock().projects.entry(root.to_path_buf()).or_default() += 1;
        ProjectGuard { shared: self.shared.clone(), root: root.to_path_buf() }
    }

    /// A file was opened in `project` with this text. Each open needs its
    /// own [`close`](Self::close): two windows may show the same file.
    pub fn open(&self, project: &Path, path: &Path, text: String) {
        let Some(language) = language_id(path) else { return };
        {
            let mut state = self.shared.lock();
            match state.docs.get_mut(path) {
                Some(doc) => {
                    doc.opened += 1;
                    drop(state);
                    self.change(path, text);
                    state = self.shared.lock();
                }
                None => {
                    let doc = Doc { project: project.to_path_buf(), language, text, servers: Vec::new(), opened: 1 };
                    state.docs.insert(path.to_path_buf(), doc);
                }
            }
            self.attach(&mut state, path);
        }
        (self.shared.notify)(Event::Status);
    }

    /// The text of an open file changed.
    pub fn change(&self, path: &Path, text: String) {
        let mut state = self.shared.lock();
        let Some(doc) = state.docs.get_mut(path) else { return };
        if doc.text == text {
            return;
        }
        doc.text = text.clone();
        let ids = doc.servers.clone();
        Self::send(&state, &ids, |_| Command::Change { path: path.to_path_buf(), text: text.clone() });
    }

    /// An open file was written to disk.
    pub fn saved(&self, path: &Path) {
        let state = self.shared.lock();
        let Some(doc) = state.docs.get(path) else { return };
        Self::send(&state, &doc.servers, |_| Command::Save { path: path.to_path_buf() });
    }

    /// A file is no longer open.
    pub fn close(&self, path: &Path) {
        let mut state = self.shared.lock();
        let Some(doc) = state.docs.get_mut(path) else { return };
        doc.opened -= 1;
        if doc.opened > 0 {
            return;
        }
        let Some(doc) = state.docs.remove(path) else { return };
        Self::send(&state, &doc.servers, |_| Command::Close { path: path.to_path_buf() });
    }

    /// Everything the servers say about a file, from the top of the file down.
    pub fn problems(&self, path: &Path) -> Vec<Problem> {
        let state = self.shared.lock();
        let mut all: Vec<Problem> =
            state.problems.get(path).map(|by_server| by_server.values().flatten().cloned().collect()).unwrap_or_default();
        all.sort_by_key(|p| (p.line, p.column, p.severity));
        all
    }

    /// The files under `root` that have problems, by path, with their problems.
    pub fn all_problems(&self, root: &Path) -> Vec<(PathBuf, Vec<Problem>)> {
        let paths: Vec<PathBuf> = {
            let state = self.shared.lock();
            state.problems.keys().filter(|p| p.starts_with(root)).cloned().collect()
        };
        let mut all: Vec<_> = paths
            .into_iter()
            .map(|path| {
                let problems = self.problems(&path);
                (path, problems)
            })
            .collect();
        all.retain(|(_, problems)| !problems.is_empty());
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    /// Errors and warnings in the files under `root`.
    pub fn counts(&self, root: &Path) -> (usize, usize) {
        let state = self.shared.lock();
        let mut counts = (0, 0);
        for problem in state.problems.iter().filter(|(p, _)| p.starts_with(root)).flat_map(|(_, s)| s.values().flatten())
        {
            match problem.severity {
                Severity::Error => counts.0 += 1,
                Severity::Warning => counts.1 += 1,
                Severity::Info | Severity::Hint => {}
            }
        }
        counts
    }

    /// Whether some server is running, or starting, for files under `root`.
    pub fn is_checking(&self, root: &Path) -> bool {
        let state = self.shared.lock();
        state.servers.iter().any(|s| s.project.starts_with(root) && !matches!(s.status, Status::Missing | Status::Failed(_)))
    }

    /// What checks a file. `None` for a file no server Wercode knows checks,
    /// and while checking is off.
    pub fn report(&self, path: &Path) -> Option<Report> {
        let state = self.shared.lock();
        if !state.enabled {
            return None;
        }
        let language = language_id(path)?;
        let doc = state.docs.get(path);
        let mut servers = Vec::new();
        let mut install = None;
        let mut working = false;
        for (index, spec) in self.shared.specs.iter().enumerate().filter(|(_, s)| s.languages.contains(&language)) {
            let server =
                doc.and_then(|doc| state.servers.iter().find(|s| s.spec == index && doc.servers.contains(&s.id)));
            match server {
                Some(server) if server.status == Status::Missing => {
                    if spec.offer && install.is_none() {
                        install = spec.install.clone();
                    }
                }
                Some(server) => {
                    working |= !matches!(server.status, Status::Failed(_));
                    servers.push((server.name.clone(), server.status.clone()));
                }
                None => {}
            }
        }
        let language = language_name(language);
        if servers.is_empty() && install.is_none() && doc.is_none() {
            return None;
        }
        Some(Report { language, servers, install: if working { None } else { install } })
    }

    /// Looks again for servers that were not installed, for the files that
    /// are open. Cheap when nothing is missing.
    pub fn retry_missing(&self) {
        let forgotten = {
            let mut state = self.shared.lock();
            if !state.enabled || !state.servers.iter().any(|s| s.status == Status::Missing) {
                return;
            }
            let (missing, kept): (Vec<_>, Vec<_>) =
                std::mem::take(&mut state.servers).into_iter().partition(|s| s.status == Status::Missing);
            state.servers = kept;
            let ids: Vec<usize> = missing.iter().map(|s| s.id).collect();
            let mut forgotten = Vec::new();
            for id in &ids {
                forgotten.extend(self.shared.forget(&mut state, *id));
            }
            let paths: Vec<PathBuf> = state
                .docs
                .iter_mut()
                .filter_map(|(path, doc)| {
                    let before = doc.servers.len();
                    doc.servers.retain(|id| !ids.contains(id));
                    (doc.servers.len() != before).then(|| path.clone())
                })
                .collect();
            for path in paths {
                self.attach(&mut state, &path);
            }
            forgotten
        };
        self.shared.report_forgotten(forgotten);
        (self.shared.notify)(Event::Status);
    }

    /// Stops every server and starts them again for the open files.
    pub fn restart(&self) {
        self.stop_servers();
        {
            let mut state = self.shared.lock();
            if !state.enabled {
                return;
            }
            let paths: Vec<PathBuf> = state.docs.keys().cloned().collect();
            for path in paths {
                self.attach(&mut state, &path);
            }
        }
        (self.shared.notify)(Event::Status);
    }

    /// Asks every server to stop and waits a moment for them to go, as
    /// Wercode quits.
    pub fn stop_all(&self, wait: Duration) {
        let threads = self.stop_servers();
        let deadline = Instant::now() + wait;
        for thread in threads {
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// Asks every server to stop and forgets what they said; the open files
    /// stay known. Returns the threads that talk to them.
    fn stop_servers(&self) -> Vec<JoinHandle<()>> {
        let (threads, forgotten) = {
            let mut state = self.shared.lock();
            let servers = std::mem::take(&mut state.servers);
            for doc in state.docs.values_mut() {
                doc.servers.clear();
            }
            let mut forgotten = Vec::new();
            let mut threads = Vec::new();
            for mut server in servers {
                let _ = server.commands.send(Command::Stop);
                forgotten.extend(self.shared.forget(&mut state, server.id));
                threads.extend(server.thread.take());
            }
            (threads, forgotten)
        };
        self.shared.report_forgotten(forgotten);
        (self.shared.notify)(Event::Status);
        threads
    }

    fn send(state: &State, ids: &[usize], command: impl Fn(&Server) -> Command) {
        for server in state.servers.iter().filter(|s| ids.contains(&s.id)) {
            let _ = server.commands.send(command(server));
        }
    }

    /// Sends an open file to each server for its language, starting the
    /// servers that are not running yet.
    fn attach(&self, state: &mut State, path: &Path) {
        if !state.enabled {
            return;
        }
        let Some(doc) = state.docs.get(path) else { return };
        let (project, language, text) = (doc.project.clone(), doc.language, doc.text.clone());
        let mut added = Vec::new();
        for (index, spec) in self.shared.specs.iter().enumerate() {
            if !spec.languages.contains(&language) {
                continue;
            }
            let doc = &state.docs[path];
            if state.servers.iter().any(|s| s.spec == index && doc.servers.contains(&s.id)) {
                continue;
            }
            let root = root_for(path, &project, &spec.markers);
            let existing = state.servers.iter().position(|s| s.spec == index && s.root == root && s.project == project);
            let position = match existing {
                Some(position) => position,
                None => {
                    let id = state.next_id;
                    state.next_id += 1;
                    let (commands, receiver) = mpsc::channel();
                    let thread = client::start(
                        id,
                        spec.clone(),
                        root.clone(),
                        commands.clone(),
                        receiver,
                        Arc::downgrade(&self.shared),
                    );
                    state.servers.push(Server {
                        id,
                        spec: index,
                        root,
                        project: project.clone(),
                        name: spec.name.clone(),
                        status: Status::Starting,
                        commands,
                        thread: Some(thread),
                    });
                    state.servers.len() - 1
                }
            };
            let server = &state.servers[position];
            let _ = server.commands.send(Command::Open { path: path.to_path_buf(), language, text: text.clone() });
            added.push(server.id);
        }
        if let Some(doc) = state.docs.get_mut(path) {
            doc.servers.extend(added);
        }
    }
}

/// The `file:` address of a path, as servers want it.
pub fn uri_from_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if let Some(share) = text.strip_prefix("//") {
        // A Windows share: \\server\folder is file://server/folder.
        out.push_str(&encode(share));
        return out;
    }
    if !text.starts_with('/') {
        out.push('/');
    }
    out.push_str(&encode(&text));
    out
}

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The path of a `file:` address; `None` for any other kind.
pub fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = rest.get(i + 1..i + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            decoded.push(byte);
            i += 3;
            continue;
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    let text = String::from_utf8(decoded).ok()?;
    if cfg!(windows) {
        let text = match text.strip_prefix('/') {
            // /C:/Users -> C:/Users, with the drive letter as Windows writes it.
            Some(rest) if rest.as_bytes().get(1) == Some(&b':') => {
                let mut chars = rest.chars();
                let drive = chars.next()?.to_ascii_uppercase();
                format!("{drive}{}", chars.as_str())
            }
            Some(rest) => rest.to_string(),
            // file://server/folder
            None => format!("//{text}"),
        };
        return Some(PathBuf::from(text.replace('/', "\\")));
    }
    if text.starts_with('/') { Some(PathBuf::from(text)) } else { Some(PathBuf::from(format!("//{text}"))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_addresses_convert_both_ways() {
        if cfg!(windows) {
            let path = Path::new(r"C:\Users\me\my project\main.rs");
            assert_eq!(uri_from_path(path), "file:///C:/Users/me/my%20project/main.rs");
            assert_eq!(path_from_uri("file:///C:/Users/me/my%20project/main.rs").unwrap(), path);
            // Servers that write the drive in lower case or encode the colon.
            assert_eq!(path_from_uri("file:///c%3A/Users/me/my%20project/main.rs").unwrap(), path);
            let share = Path::new(r"\\server\share\a.py");
            assert_eq!(uri_from_path(share), "file://server/share/a.py");
            assert_eq!(path_from_uri("file://server/share/a.py").unwrap(), share);
        } else {
            let path = Path::new("/home/me/my project/\u{0434}.rs");
            let uri = uri_from_path(path);
            assert_eq!(uri, "file:///home/me/my%20project/%D0%B4.rs");
            assert_eq!(path_from_uri(&uri).unwrap(), path);
        }
        assert_eq!(path_from_uri("untitled:Untitled-1"), None);
    }

    /// The test binary plays the server: started with this variable set,
    /// the test below talks the protocol on its standard streams.
    const FAKE: &str = "WERCODE_FAKE_LANGUAGE_SERVER";

    #[test]
    fn fake_language_server() {
        if std::env::var_os(FAKE).is_some() {
            fake::serve();
        }
    }

    fn fake_spec() -> Spec {
        let exe = std::env::current_exe().unwrap();
        Spec {
            name: "fake".into(),
            programs: vec![Program {
                program: exe.to_string_lossy().into_owned(),
                args: fake::test_args("lsp::tests::fake_language_server"),
                env: vec![(FAKE.into(), "1".into())],
            }],
            probe: None,
            languages: vec!["rust"],
            markers: vec!["Cargo.toml"],
            install: None,
            offer: true,
        }
    }

    fn missing_spec() -> Spec {
        Spec {
            name: "missing".into(),
            programs: vec![Program { program: "wercode-no-such-server".into(), args: vec![], env: vec![] }],
            install: Some("install-it".into()),
            languages: vec!["python"],
            ..fake_spec()
        }
    }

    /// Waits for `done`, re-checking whenever the servers report something.
    fn wait(events: &mpsc::Receiver<Event>, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            let _ = events.recv_timeout(Duration::from_millis(100));
        }
    }

    #[test]
    fn a_server_reports_problems_that_follow_the_text() {
        let dir = std::env::temp_dir().join(format!("wercode-lsp-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.rs");
        let (sender, events) = mpsc::channel();
        let servers = Servers::new(vec![fake_spec(), missing_spec()], move |event| {
            let _ = sender.send(event);
        });
        let guard = servers.project(&dir);

        // The emoji takes two UTF-16 units and one character.
        servers.open(&dir, &file, "fn main() {}\nlet \u{1F600} = bad;\nmeh\n".into());
        wait(&events, "the first problems", || servers.problems(&file).len() == 2);
        let problems = servers.problems(&file);
        assert_eq!((problems[0].line, problems[0].column, problems[0].end_column), (1, 8, 11));
        assert_eq!(problems[0].severity, Severity::Error);
        assert_eq!(problems[0].message, "bad word");
        assert_eq!(problems[0].source.as_deref(), Some("fake"));
        assert_eq!((problems[1].line, problems[1].severity), (2, Severity::Warning));
        assert_eq!(servers.counts(&dir), (1, 1));
        assert_eq!(servers.all_problems(&dir), vec![(file.clone(), problems.clone())]);
        wait(&events, "the server to be ready", || {
            servers.report(&file).is_some_and(|r| r.servers.first().is_some_and(|s| s.1 == Status::Ready))
        });
        assert!(servers.is_checking(&dir));

        servers.change(&file, "all good\n".into());
        wait(&events, "the problems to go", || servers.problems(&file).is_empty());

        // A language whose server is not installed offers the install.
        let script = dir.join("tool.py");
        servers.open(&dir, &script, "print()".into());
        wait(&events, "the missing server", || {
            servers.report(&script).is_some_and(|r| r.install.as_deref() == Some("install-it"))
        });
        assert_eq!(servers.report(&script).unwrap().language, "Python");
        servers.retry_missing();
        wait(&events, "the server to be missing again", || {
            servers.report(&script).is_some_and(|r| r.install.is_some())
        });

        // Turning checking off forgets everything; on brings it back.
        servers.change(&file, "bad again\n".into());
        wait(&events, "problems after the change", || servers.problems(&file).len() == 1);
        servers.set_enabled(false);
        assert!(servers.problems(&file).is_empty());
        assert_eq!(servers.report(&file), None);
        servers.set_enabled(true);
        wait(&events, "problems after turning it on", || servers.problems(&file).len() == 1);

        // Closing the project stops its servers.
        drop(guard);
        assert!(servers.problems(&file).is_empty());
        assert!(!servers.is_checking(&dir));
        servers.stop_all(Duration::from_secs(5));
        // The server works in the folder until its process has gone.
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::fs::remove_dir_all(&dir).is_err() {
            assert!(Instant::now() < deadline, "the server did not stop");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
