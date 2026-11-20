//! Checking code with language servers: the programs editors talk to for
//! errors and warnings (`rust-analyzer`, `pyright`, `gopls` and the like).
//!
//! Wercode does not bundle them. A server installed on the computer is
//! found on `PATH`, or one Wercode fetched into its own folder
//! ([`download`]), and started for the files of its language when one of them
//! is opened; what it reports is kept here by file. Each server runs in its
//! own process with a thread here that talks to it, so nothing waits for a
//! server on the thread that draws the window.

mod client;
pub mod download;
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

pub use serde_json::Value;
pub use specs::{Program, Spec, builtin, find_program, language_id, language_name, root_for};

use client::{Command, Question};

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

/// What a server says about the code under the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hover {
    /// The type, the signature, the documentation: Markdown.
    pub text: String,
    /// The piece of code it is about, when the server says: where it starts
    /// and ends, as 0-based lines and columns in characters.
    pub range: Option<((u32, u32), (u32, u32))>,
}

/// A place in a file, such as where a name is defined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    /// Where the name starts and ends: 0-based lines, columns in characters.
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// A change to the text of a file: what stands between two places is
/// replaced. Places are 0-based lines and columns in characters, all of them
/// in the text as it was before any of the changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub start: (u32, u32),
    pub end: (u32, u32),
    pub text: String,
}

/// Changes a server wants made to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdits {
    pub path: PathBuf,
    pub edits: Vec<Edit>,
}

/// Something a server offers to do at a place: a quick fix ("add the
/// missing import"), a refactoring, or a change to the whole file.
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub title: String,
    /// `quickfix`, `refactor.extract`, `source.organizeImports` and the like.
    pub kind: Option<String>,
    /// The server thinks this is the fix to take.
    pub preferred: bool,
    /// The changes, when the server sent them right away.
    pub edits: Vec<FileEdits>,
    /// The server that offered it and the file it was asked about.
    server: usize,
    path: PathBuf,
    /// The action as the server wrote it, for asking the rest of it later.
    raw: Value,
}

/// A named thing in code: a function, a type, a field, a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    /// More about it, such as a signature.
    pub detail: Option<String>,
    /// The protocol's `SymbolKind`: 5 class, 6 method, 12 function, 13 variable...
    pub kind: u32,
    pub path: PathBuf,
    /// Where the whole of it starts and ends, and where its name is: 0-based
    /// lines, columns in characters.
    pub start: (u32, u32),
    pub end: (u32, u32),
    pub name_at: (u32, u32),
    /// What it is inside of, for a symbol found across the project.
    pub container: Option<String>,
    pub children: Vec<Symbol>,
}

/// A note a server wants shown in the code, such as the type of a variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    /// Where it goes: a 0-based line and a column in characters.
    pub line: u32,
    pub column: u32,
    pub label: String,
    /// The protocol's kind: 1 a type, 2 a parameter name; `None` for others.
    pub kind: Option<u32>,
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
    /// A server asked for these changes to files, after a command it ran.
    Edit(Vec<FileEdits>),
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
    /// When no server for the language is installed: the one Wercode can
    /// fetch into its own folder, by name.
    pub download: Option<String>,
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

    /// Asks what the servers know about the code at a place in an open file:
    /// a 0-based line and a column in characters. `reply` is called from
    /// another thread with the first answer that says something, or with
    /// `None`. It is dropped uncalled if a server stops before answering.
    pub fn hover(&self, path: &Path, line: u32, column: u32, reply: impl FnOnce(Option<Hover>) + Send + 'static) {
        self.ask(path, line, column, reply, |answer| answer.is_none(), |reply| Question::Hover(reply));
    }

    /// Asks where the name at a place in an open file is defined. `reply`
    /// is called like the one of [`hover`](Self::hover); no places means
    /// the servers do not know.
    pub fn definition(&self, path: &Path, line: u32, column: u32, reply: impl FnOnce(Vec<Target>) + Send + 'static) {
        self.ask(path, line, column, reply, |answer| answer.is_empty(), |reply| Question::Definition(reply));
    }

    /// Asks what could be typed at a place in an open file. The answer is
    /// the server's own (a `CompletionList` or a list of `CompletionItem`
    /// of the protocol), with columns counted in characters; `Null` when
    /// no server has anything.
    pub fn completion(&self, path: &Path, line: u32, column: u32, reply: impl FnOnce(Value) + Send + 'static) {
        let empty = |answer: &Value| {
            let items = answer.as_array().or_else(|| answer["items"].as_array());
            items.is_none_or(Vec::is_empty)
        };
        self.ask(path, line, column, reply, empty, |reply| Question::Completion(reply));
    }

    /// Asks how an open file should be laid out, indenting by `tab_size`
    /// spaces where the language leaves it open. No edits means it is fine
    /// as it is, or that no server formats this language.
    pub fn format(&self, path: &Path, tab_size: u32, reply: impl FnOnce(Vec<Edit>) + Send + 'static) {
        self.ask(path, 0, 0, reply, |answer| answer.is_empty(), move |reply| Question::Format { tab_size, reply });
    }

    /// Asks what the servers offer to do with the code between two places
    /// in an open file: quick fixes for the problems there, refactorings.
    /// Every server's offers come back together.
    pub fn code_actions(
        &self,
        path: &Path,
        start: (u32, u32),
        end: (u32, u32),
        reply: impl FnOnce(Vec<Action>) + Send + 'static,
    ) {
        self.gather(path, start, reply, move |reply| Question::CodeActions { end, reply });
    }

    /// Gets the changes of an action, asking its server for them if it sent
    /// none yet, and has the server run its command if it has one (the
    /// server then sends the changes it makes as [`Event::Edit`]).
    pub fn perform(&self, action: &Action, reply: impl FnOnce(Result<Vec<FileEdits>, String>) + Send + 'static) {
        let state = self.shared.lock();
        let Some(server) = state.servers.iter().find(|s| s.id == action.server) else {
            drop(state);
            return reply(Err(crate::i18n::t("The language server that offered this has stopped.").into()));
        };
        let commands = server.commands.clone();
        drop(state);
        let path = action.path.clone();
        let execute = move |commands: &mpsc::Sender<Command>, raw: &Value| {
            // A command of its own, or an action that carries one.
            let command = match &raw["command"] {
                Value::String(_) => raw.clone(),
                Value::Object(_) => raw["command"].clone(),
                _ => return,
            };
            let question = Question::Execute(command);
            let _ = commands.send(Command::Ask { path: path.clone(), line: 0, column: 0, question });
        };
        let resolvable =
            action.edits.is_empty() && action.raw.get("data").is_some() && !action.raw["command"].is_string();
        if !resolvable {
            execute(&commands, &action.raw);
            return reply(Ok(action.edits.clone()));
        }
        let asking = commands.clone();
        let resolved: Box<dyn FnOnce(Option<Action>) + Send> = Box::new(move |resolved| {
            let Some(resolved) = resolved else { return reply(Ok(Vec::new())) };
            execute(&commands, &resolved.raw);
            reply(Ok(resolved.edits));
        });
        let question = Question::Resolve { action: action.raw.clone(), reply: resolved };
        let _ = asking.send(Command::Ask { path: action.path.clone(), line: 0, column: 0, question });
    }

    /// Asks the servers of an open file to rename what is at a place, in
    /// every file that uses it. The changes come back; nothing is changed yet.
    pub fn rename(
        &self,
        path: &Path,
        line: u32,
        column: u32,
        new_name: &str,
        reply: impl FnOnce(Result<Vec<FileEdits>, String>) + Send + 'static,
    ) {
        /// An answer to a rename; no answer means nothing to change.
        struct Renaming(Result<Vec<FileEdits>, String>);
        impl Default for Renaming {
            fn default() -> Self {
                Renaming(Ok(Vec::new()))
            }
        }
        let new_name = new_name.to_string();
        let empty = |answer: &Renaming| answer.0.as_ref().is_ok_and(Vec::is_empty);
        let reply = move |answer: Renaming| reply(answer.0);
        self.ask(path, line, column, reply, empty, move |reply| Question::Rename {
            new_name: new_name.clone(),
            reply: Box::new(move |answer| reply(Renaming(answer))),
        });
    }

    /// The named things in an open file (functions, types, fields), the way
    /// they nest.
    pub fn symbols(&self, path: &Path, reply: impl FnOnce(Vec<Symbol>) + Send + 'static) {
        self.gather(path, (0, 0), reply, Question::Symbols);
    }

    /// The notes the servers want shown in lines `first..=last` of an open
    /// file, such as the types of variables.
    pub fn hints(&self, path: &Path, first: u32, last: u32, reply: impl FnOnce(Vec<Hint>) + Send + 'static) {
        self.gather(path, (first, 0), reply, move |reply| Question::Hints { last, reply });
    }

    /// Named things across the files of a project whose names fit `query`.
    pub fn project_symbols(&self, root: &Path, query: &str, reply: impl FnOnce(Vec<Symbol>) + Send + 'static) {
        let state = self.shared.lock();
        let servers: Vec<&Server> =
            state.servers.iter().filter(|s| s.project == root || s.root.starts_with(root)).collect();
        let parts = Gathering::new(servers.len(), reply);
        for (server, part) in servers.into_iter().zip(parts) {
            let question = Question::ProjectSymbols { query: query.to_string(), reply: part };
            let _ = server.commands.send(Command::Ask { path: root.to_path_buf(), line: 0, column: 0, question });
        }
    }

    /// Whether an editor has the file open.
    pub fn is_open(&self, path: &Path) -> bool {
        self.shared.lock().docs.contains_key(path)
    }

    /// Asks each server of a file and puts every answer together.
    fn gather<T: Send + 'static>(
        &self,
        path: &Path,
        at: (u32, u32),
        reply: impl FnOnce(Vec<T>) + Send + 'static,
        question: impl Fn(Box<dyn FnOnce(Vec<T>) + Send>) -> Question,
    ) {
        let state = self.shared.lock();
        let servers: Vec<&Server> = match state.docs.get(path) {
            Some(doc) => state.servers.iter().filter(|s| doc.servers.contains(&s.id)).collect(),
            None => Vec::new(),
        };
        let parts = Gathering::new(servers.len(), reply);
        for (server, part) in servers.into_iter().zip(parts) {
            let ask = Command::Ask { path: path.to_path_buf(), line: at.0, column: at.1, question: question(part) };
            let _ = server.commands.send(ask);
        }
    }

    /// Asks each server of a file; the first answer that is not empty wins.
    fn ask<T: Default + Send + 'static>(
        &self,
        path: &Path,
        line: u32,
        column: u32,
        reply: impl FnOnce(T) + Send + 'static,
        empty: fn(&T) -> bool,
        question: impl Fn(Box<dyn FnOnce(T) + Send>) -> Question,
    ) {
        let state = self.shared.lock();
        let servers: Vec<&Server> = match state.docs.get(path) {
            Some(doc) => state.servers.iter().filter(|s| doc.servers.contains(&s.id)).collect(),
            None => Vec::new(),
        };
        // How many servers have yet to answer, and the reply until it is used.
        let reply: Box<dyn FnOnce(T) + Send> = Box::new(reply);
        let waiting = Arc::new(Mutex::new((servers.len(), Some(reply))));
        for server in &servers {
            let waiting = waiting.clone();
            let one: Box<dyn FnOnce(T) + Send> = Box::new(move |answer| {
                let reply = {
                    let mut waiting = waiting.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    waiting.0 -= 1;
                    if empty(&answer) && waiting.0 > 0 { None } else { waiting.1.take() }
                };
                if let Some(reply) = reply {
                    reply(answer);
                }
            });
            // A server that has gone takes the question with it, unanswered.
            let _ = server.commands.send(Command::Ask { path: path.to_path_buf(), line, column, question: question(one) });
        }
        let nobody = servers.is_empty();
        drop(state);
        if nobody && let Some(reply) = waiting.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).1.take() {
            reply(T::default());
        }
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
        let mut download = None;
        let mut working = false;
        for (index, spec) in self.shared.specs.iter().enumerate().filter(|(_, s)| s.languages.contains(&language)) {
            let server =
                doc.and_then(|doc| state.servers.iter().find(|s| s.spec == index && doc.servers.contains(&s.id)));
            match server {
                Some(server) if server.status == Status::Missing => {
                    if spec.offer && install.is_none() && download.is_none() {
                        install = spec.install.clone();
                        download = spec.download.as_ref().map(|_| spec.name.clone());
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
        if servers.is_empty() && install.is_none() && download.is_none() && doc.is_none() {
            return None;
        }
        if working {
            (install, download) = (None, None);
        }
        Some(Report { language, servers, install, download })
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
    /// How Wercode can fetch the server named `name`, if it can.
    pub fn download_of(&self, name: &str) -> Option<download::Download> {
        self.shared.specs.iter().find(|spec| spec.name == name)?.download.clone()
    }

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

/// Answers from several servers put together: the reply goes out once each
/// has answered or gone away.
struct Gathering<T> {
    left: usize,
    items: Vec<T>,
    reply: Option<Box<dyn FnOnce(Vec<T>) + Send>>,
}

/// One server's share of a [`Gathering`]: dropped unanswered, it counts as
/// an answer with nothing in it.
struct Part<T: Send + 'static> {
    gathering: Arc<Mutex<Gathering<T>>>,
}

impl<T: Send + 'static> Gathering<T> {
    /// The replies to hand to `count` servers. With no servers, `reply` is
    /// called at once with nothing.
    fn new(count: usize, reply: impl FnOnce(Vec<T>) + Send + 'static) -> Vec<Box<dyn FnOnce(Vec<T>) + Send>> {
        if count == 0 {
            reply(Vec::new());
            return Vec::new();
        }
        let gathering =
            Arc::new(Mutex::new(Gathering { left: count, items: Vec::new(), reply: Some(Box::new(reply)) }));
        (0..count)
            .map(|_| {
                let part = Part { gathering: gathering.clone() };
                Box::new(move |items: Vec<T>| part.give(items)) as Box<dyn FnOnce(Vec<T>) + Send>
            })
            .collect()
    }
}

impl<T: Send + 'static> Part<T> {
    fn give(self, items: Vec<T>) {
        self.gathering.lock().unwrap_or_else(|p| p.into_inner()).items.extend(items);
        // Dropping the part counts the answer.
    }
}

impl<T: Send + 'static> Drop for Part<T> {
    fn drop(&mut self) {
        let done = {
            let mut gathering = self.gathering.lock().unwrap_or_else(|p| p.into_inner());
            gathering.left -= 1;
            if gathering.left > 0 {
                None
            } else {
                let items = std::mem::take(&mut gathering.items);
                gathering.reply.take().map(|reply| (reply, items))
            }
        };
        if let Some((reply, items)) = done {
            reply(items);
        }
    }
}

/// Makes the changes in a text with LF line endings. Places are 0-based
/// lines and columns in characters, in the text as it was.
pub fn apply_edits(text: &str, edits: &[Edit]) -> String {
    let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let offset = |(line, column): (u32, u32)| -> usize {
        let Some(&start) = starts.get(line as usize) else { return text.len() };
        let end = starts.get(line as usize + 1).map_or(text.len(), |next| next - 1);
        let line_text = &text[start..end];
        start + line_text.char_indices().nth(column as usize).map_or(line_text.len(), |(i, _)| i)
    };
    let mut sorted: Vec<&Edit> = edits.iter().collect();
    // From the end of the text up, so one change does not move the places of the next.
    sorted.sort_by(|a, b| b.start.cmp(&a.start));
    let mut out = text.to_string();
    for edit in sorted {
        let (from, to) = (offset(edit.start), offset(edit.end));
        if from <= to && to <= out.len() {
            out.replace_range(from..to, &edit.text);
        }
    }
    out
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
            download: None,
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
    fn edits_are_made_from_the_end_up() {
        let edit = |start: (u32, u32), end: (u32, u32), text: &str| Edit { start, end, text: text.into() };
        let text = "let \u{1F600} = bad;\nbad bad\n";
        let edits = [edit((0, 8), (0, 11), "good"), edit((1, 0), (1, 3), "x"), edit((1, 4), (1, 7), "y"), edit((2, 0), (2, 0), "end\n")];
        assert_eq!(apply_edits(text, &edits), "let \u{1F600} = good;\nx y\nend\n");
    }

    #[test]
    fn a_server_fixes_renames_lists_and_notes() {
        let dir = std::env::temp_dir().join(format!("wercode-lsp-smart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (file, other) = (dir.join("main.rs"), dir.join("other.rs"));
        let (sender, events) = mpsc::channel();
        let servers = Servers::new(vec![fake_spec()], move |event| {
            let _ = sender.send(event);
        });
        let guard = servers.project(&dir);
        let text = "struct Point\n  fn len(\nfn main() {\n    let \u{1F600}x = bad;\n}\n";
        servers.open(&dir, &file, text.into());
        servers.open(&dir, &other, "fn helper() { bad }\n".into());
        wait(&events, "the problems", || servers.problems(&file).len() == 1);
        fn recv<T>(r: &mpsc::Receiver<T>) -> T {
            r.recv_timeout(Duration::from_secs(30)).unwrap()
        }

        // Quick fixes for the problem on the line, from every server together.
        let (asked, answers) = mpsc::channel();
        servers.code_actions(&file, (3, 0), (3, 0), move |found| asked.send(found).unwrap());
        let actions: Vec<Action> = recv(&answers);
        let titles: Vec<&str> = actions.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(titles, ["Replace bad with good", "Shout the line", "Say hi"]);
        assert!(actions[0].preferred);
        // The emoji is two UTF-16 units for the server and one character here.
        let good = Edit { start: (3, 13), end: (3, 16), text: "good".into() };
        assert_eq!(actions[0].edits, vec![FileEdits { path: file.clone(), edits: vec![good.clone()] }]);

        // Its changes as they are; the next one's asked for; the last one's
        // command makes the server send its change by itself.
        let perform = |action: &Action| {
            let (asked, answers) = mpsc::channel();
            servers.perform(action, move |found| asked.send(found).unwrap());
            recv(&answers).unwrap()
        };
        assert_eq!(perform(&actions[0]), actions[0].edits);
        let shout = Edit { start: (3, 0), end: (3, 17), text: "    LET \u{1F600}X = BAD;".into() };
        assert_eq!(perform(&actions[1]), vec![FileEdits { path: file.clone(), edits: vec![shout] }]);
        assert_eq!(perform(&actions[2]), Vec::new());
        let deadline = Instant::now() + Duration::from_secs(30);
        let sent = loop {
            assert!(Instant::now() < deadline, "timed out waiting for the command's edit");
            if let Ok(Event::Edit(files)) = events.recv_timeout(Duration::from_millis(100)) {
                break Some(files);
            }
        };
        let hi = Edit { start: (0, 0), end: (0, 0), text: "hi\n".into() };
        assert_eq!(sent.unwrap(), vec![FileEdits { path: file.clone(), edits: vec![hi] }]);

        // A rename reaches every open file that has the name.
        let (asked, answers) = mpsc::channel();
        servers.rename(&file, 3, 14, "fine", move |found| asked.send(found).unwrap());
        let mut renamed = recv(&answers).unwrap();
        renamed.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(renamed.iter().map(|f| f.path.clone()).collect::<Vec<_>>(), [file.clone(), other.clone()]);
        assert_eq!(renamed[0].edits, vec![Edit { text: "fine".into(), ..good }]);
        assert_eq!(apply_edits("fn helper() { bad }\n", &renamed[1].edits), "fn helper() { fine }\n");

        // The things in the file, the way they nest, and across the project.
        let (asked, answers) = mpsc::channel();
        servers.symbols(&file, move |found| asked.send(found).unwrap());
        let symbols: Vec<Symbol> = recv(&answers);
        assert_eq!(symbols.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Point", "main"]);
        assert_eq!((symbols[0].kind, symbols[0].children[0].name.as_str()), (23, "len"));
        assert_eq!((symbols[0].start, symbols[0].end, symbols[0].name_at), ((0, 0), (1, 9), (0, 7)));
        let (asked, answers) = mpsc::channel();
        servers.project_symbols(&dir, "help", move |found| asked.send(found).unwrap());
        let found: Vec<Symbol> = recv(&answers);
        assert_eq!(found.iter().map(|s| (s.name.as_str(), s.path.clone())).collect::<Vec<_>>(), [("helper", other.clone())]);

        // Notes in the lines asked about.
        let (asked, answers) = mpsc::channel();
        servers.hints(&file, 0, 4, move |found| asked.send(found).unwrap());
        let hints: Vec<Hint> = recv(&answers);
        assert_eq!(hints, vec![Hint { line: 3, column: 10, label: ": i32".into(), kind: Some(1) }]);
        assert!(servers.is_open(&file) && !servers.is_open(&dir.join("nope.rs")));

        drop(guard);
        servers.stop_all(Duration::from_secs(5));
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::fs::remove_dir_all(&dir).is_err() {
            assert!(Instant::now() < deadline, "the server did not stop");
            std::thread::sleep(Duration::from_millis(50));
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

        // What is under the pointer, and where a name is defined.
        servers.change(&file, "let \u{1F600} = name;\nuse name;\n".into());
        let (asked, answers) = mpsc::channel();
        let answer = asked.clone();
        servers.hover(&file, 1, 5, move |hover| answer.send(hover).unwrap());
        let hover = answers.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
        assert_eq!(hover, Hover { text: "the word `name`".into(), range: Some(((1, 4), (1, 8))) });
        let answer = asked.clone();
        servers.hover(&file, 0, 4, move |hover| answer.send(hover).unwrap());
        assert_eq!(answers.recv_timeout(Duration::from_secs(30)).unwrap(), None);
        let (asked, places) = mpsc::channel();
        servers.definition(&file, 1, 6, move |found| asked.send(found).unwrap());
        let found = places.recv_timeout(Duration::from_secs(30)).unwrap();
        // The emoji is two units for the server and one character here.
        let first = Target { path: file.clone(), line: 0, column: 8, end_line: 0, end_column: 12 };
        assert_eq!(found, vec![first]);
        // What could be typed, and how the file should be laid out.
        let (asked, lists) = mpsc::channel();
        servers.completion(&file, 1, 4, move |list| asked.send(list).unwrap());
        let list = lists.recv_timeout(Duration::from_secs(30)).unwrap();
        let labels: Vec<&str> = list["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["let", "name", "use"]);
        servers.change(&file, "let \u{1F600} = name;  \nuse name;\t\n".into());
        let (asked, edits) = mpsc::channel();
        servers.format(&file, 4, move |found| asked.send(found).unwrap());
        let found = edits.recv_timeout(Duration::from_secs(30)).unwrap();
        let tidy = |line, start, end| Edit { start: (line, start), end: (line, end), text: String::new() };
        assert_eq!(found, vec![tidy(0, 13, 15), tidy(1, 9, 10)]);
        // A file that is not open has nobody to ask.
        let (asked, places) = mpsc::channel();
        servers.definition(&dir.join("other.rs"), 0, 0, move |found| asked.send(found).unwrap());
        assert_eq!(places.recv_timeout(Duration::from_secs(30)).unwrap(), Vec::new());

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
