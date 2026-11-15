//! The thread that runs one language server and talks to it.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::specs::{self, Program, Spec};
use super::{Problem, Severity, Shared, Status, path_from_uri, rpc, uri_from_path};

/// How long a server may take to finish after it was asked to stop.
const STOP_WAIT: Duration = Duration::from_secs(2);
/// How long the check that a program really is the server may take.
const PROBE_WAIT: Duration = Duration::from_secs(10);

pub(super) enum Command {
    Open { path: PathBuf, language: &'static str, text: String },
    Change { path: PathBuf, text: String },
    Save { path: PathBuf },
    Close { path: PathBuf },
    Stop,
    /// A message from the server.
    Incoming(Value),
    /// The server's output has ended: it exited.
    Ended,
}

pub(super) fn start(
    id: usize,
    spec: Spec,
    root: PathBuf,
    sender: Sender<Command>,
    receiver: Receiver<Command>,
    shared: Weak<Shared>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("lsp {}", spec.name))
        .spawn(move || run(id, spec, root, sender, receiver, shared))
        .expect("could not start a thread for a language server")
}

#[cfg(windows)]
fn hide_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;
    // CREATE_NO_WINDOW: no console flashing up next to a GUI app.
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn hide_window(_: &mut std::process::Command) {}

/// Whether the program answers `args` (usually `--version`) without failing.
fn probe(path: &Path, args: &[String]) -> bool {
    let mut command = std::process::Command::new(path);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(search) = specs::server_path() {
        command.env("PATH", search);
    }
    hide_window(&mut command);
    let Ok(mut child) = command.spawn() else { return false };
    let deadline = Instant::now() + PROBE_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                return false;
            }
        }
    }
}

/// The first of the spec's programs found on this computer.
fn find(spec: &Spec) -> Option<(Program, PathBuf)> {
    spec.programs.iter().find_map(|program| {
        let path = specs::find_program(&program.program)?;
        if let Some(args) = &spec.probe
            && !probe(&path, args)
        {
            return None;
        }
        Some((program.clone(), path))
    })
}

fn spawn(program: &Program, path: &Path, root: &Path) -> std::io::Result<Child> {
    let mut command = std::process::Command::new(path);
    command
        .args(&program.args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(search) = specs::server_path() {
        command.env("PATH", search);
    }
    for (name, value) in &program.env {
        command.env(name, value);
    }
    hide_window(&mut command);
    command.spawn()
}

/// How the server counts columns.
#[derive(Clone, Copy)]
enum Encoding {
    Utf8,
    Utf16,
    Utf32,
}

/// The column in characters of a column the server counted in its own units.
fn column(line: &str, units: u32, encoding: Encoding) -> u32 {
    let width = |c: char| match encoding {
        Encoding::Utf8 => c.len_utf8() as u32,
        Encoding::Utf16 => c.len_utf16() as u32,
        Encoding::Utf32 => 1,
    };
    let (mut used, mut chars) = (0, 0);
    for c in line.chars() {
        if used >= units {
            return chars;
        }
        used += width(c);
        chars += 1;
    }
    chars + units.saturating_sub(used)
}

fn problem(diagnostic: &Value, lines: Option<&[&str]>, encoding: Encoding) -> Option<Problem> {
    let range = diagnostic.get("range")?;
    let position = |at: &Value| -> Option<(u32, u32)> {
        let line = at.get("line")?.as_u64()? as u32;
        let units = at.get("character")?.as_u64()? as u32;
        let column = match lines.and_then(|lines| lines.get(line as usize)) {
            Some(text) => column(text, units, encoding),
            None => units,
        };
        Some((line, column))
    };
    let (line, column) = position(range.get("start")?)?;
    let (end_line, end_column) = position(range.get("end")?)?;
    Some(Problem {
        line,
        column,
        end_line,
        end_column,
        // The protocol leaves a missing severity to the editor; errors are the safe guess.
        severity: match diagnostic.get("severity").and_then(Value::as_u64) {
            Some(2) => Severity::Warning,
            Some(3) => Severity::Info,
            Some(4) => Severity::Hint,
            _ => Severity::Error,
        },
        message: diagnostic.get("message")?.as_str()?.trim().to_string(),
        source: diagnostic.get("source").and_then(Value::as_str).map(String::from),
        code: match diagnostic.get("code") {
            Some(Value::String(code)) => Some(code.clone()),
            Some(Value::Number(code)) => Some(code.to_string()),
            _ => None,
        },
    })
}

/// A file the server has open.
struct OpenDoc {
    uri: String,
    version: i64,
    text: String,
}

/// One server's side of the conversation.
struct Session {
    id: usize,
    root: PathBuf,
    shared: Weak<Shared>,
    stdin: ChildStdin,
    next_request: i64,
    initialize: i64,
    shutdown: Option<i64>,
    initialized: bool,
    /// File commands that came before the server was ready.
    queue: Vec<Command>,
    docs: HashMap<PathBuf, OpenDoc>,
    encoding: Encoding,
    /// The server wants the text with each save.
    save_text: bool,
    /// Work the server described as going on, by its token: its title, and
    /// the title with how far it has got.
    progress: HashMap<String, (String, String)>,
    /// Why the server failed, when it answered its start with an error.
    failure: Option<String>,
}

impl Session {
    fn shared(&self) -> Option<Arc<Shared>> {
        self.shared.upgrade()
    }

    fn write(&mut self, message: Value) {
        // A server that went away is noticed by the reader, which sees its output end.
        let _ = rpc::write(&mut self.stdin, &message);
    }

    fn request(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_request;
        self.next_request += 1;
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.write(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn initialize(&mut self) {
        let root = uri_from_path(&self.root);
        let name = self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let params = json!({
            // The server exits by itself if Wercode dies without stopping it.
            "processId": std::process::id(),
            "clientInfo": {"name": "Wercode", "version": env!("CARGO_PKG_VERSION")},
            "rootUri": root,
            "rootPath": self.root.to_string_lossy(),
            "workspaceFolders": [{"uri": root, "name": name}],
            "capabilities": {
                "general": {"positionEncodings": ["utf-32", "utf-16"]},
                "textDocument": {
                    "synchronization": {"didSave": true, "dynamicRegistration": false},
                    "publishDiagnostics": {"relatedInformation": false, "codeDescriptionSupport": true},
                },
                "workspace": {"workspaceFolders": true, "configuration": false},
                "window": {"workDoneProgress": true},
            },
        });
        self.initialize = self.request("initialize", params);
    }

    fn set_status(&self, status: Status) {
        if let Some(shared) = self.shared() {
            shared.set_status(self.id, status, None);
        }
    }

    /// Ready, or busy with the work the server last described.
    fn report_progress(&self) {
        if !self.initialized {
            return;
        }
        match self.progress.values().next() {
            Some((_, work)) => self.set_status(Status::Busy(work.clone())),
            None => self.set_status(Status::Ready),
        }
    }

    /// Handles a message from the server. False once the server has
    /// agreed to stop or refused to start.
    fn incoming(&mut self, message: Value) -> bool {
        let method = message.get("method").and_then(Value::as_str).map(String::from);
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match (method, message.get("id").cloned()) {
            (Some(method), Some(id)) => self.answer(&method, id, &params),
            (Some(method), None) => self.notification(&method, &params),
            (None, Some(id)) => {
                if id.as_i64() == Some(self.initialize) {
                    return self.started(&message);
                }
                if self.shutdown.is_some() && id.as_i64() == self.shutdown {
                    return false;
                }
            }
            (None, None) => {}
        }
        true
    }

    /// The answer to `initialize`.
    fn started(&mut self, message: &Value) -> bool {
        if let Some(error) = message.get("error") {
            self.failure = Some(error.get("message").and_then(Value::as_str).unwrap_or_default().to_string());
            return false;
        }
        let capabilities = &message["result"]["capabilities"];
        self.encoding = match capabilities["positionEncoding"].as_str() {
            Some("utf-32") => Encoding::Utf32,
            Some("utf-8") => Encoding::Utf8,
            _ => Encoding::Utf16,
        };
        let save = &capabilities["textDocumentSync"]["save"];
        self.save_text = save["includeText"].as_bool().unwrap_or(false);
        self.notify("initialized", json!({}));
        self.initialized = true;
        self.report_progress();
        for command in std::mem::take(&mut self.queue) {
            self.document(command);
        }
        true
    }

    /// Answers something the server asked.
    fn answer(&mut self, method: &str, id: Value, params: &Value) {
        let result = match method {
            // Nothing configured: each server uses its own defaults.
            "workspace/configuration" => {
                let count = params["items"].as_array().map_or(0, Vec::len);
                Value::Array(vec![Value::Null; count])
            }
            "workspace/workspaceFolders" => {
                let name = self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                json!([{"uri": uri_from_path(&self.root), "name": name}])
            }
            "window/workDoneProgress/create"
            | "client/registerCapability"
            | "client/unregisterCapability"
            | "window/showMessageRequest" => Value::Null,
            "window/showDocument" => json!({"success": false}),
            "workspace/applyEdit" => json!({"applied": false}),
            _ => {
                self.write(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": format!("{method} is not supported")},
                }));
                return;
            }
        };
        self.write(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn notification(&mut self, method: &str, params: &Value) {
        match method {
            "textDocument/publishDiagnostics" => self.diagnostics(params),
            "$/progress" => {
                let token = match &params["token"] {
                    Value::String(token) => token.clone(),
                    other => other.to_string(),
                };
                let value = &params["value"];
                match value["kind"].as_str() {
                    Some("begin") | Some("report") => {
                        // Only the beginning has a title; reports carry the rest.
                        let title = match value["title"].as_str() {
                            Some(title) => title.to_string(),
                            None => self.progress.get(&token).map(|(title, _)| title.clone()).unwrap_or_default(),
                        };
                        let detail = value["message"]
                            .as_str()
                            .map(String::from)
                            .or_else(|| value["percentage"].as_u64().map(|p| format!("{p}%")));
                        let work = match detail {
                            Some(detail) if !title.is_empty() => format!("{title} \u{2014} {detail}"),
                            Some(detail) => detail,
                            None => title.clone(),
                        };
                        self.progress.insert(token, (title, work));
                    }
                    Some("end") => {
                        self.progress.remove(&token);
                    }
                    _ => return,
                }
                self.report_progress();
            }
            _ => {}
        }
    }

    fn diagnostics(&mut self, params: &Value) {
        let Some(uri) = params["uri"].as_str() else { return };
        let open = self.docs.iter().find(|(_, doc)| doc.uri == uri);
        let Some(path) = open.map(|(path, _)| path.clone()).or_else(|| path_from_uri(uri)) else { return };
        let lines: Option<Vec<&str>> = open.map(|(_, doc)| doc.text.split('\n').collect());
        let problems: Vec<Problem> = params["diagnostics"]
            .as_array()
            .map(|all| all.iter().filter_map(|d| problem(d, lines.as_deref(), self.encoding)).collect())
            .unwrap_or_default();
        if let Some(shared) = self.shared() {
            shared.set_problems(self.id, path, problems);
        }
    }

    /// Sends a change to an open file, or holds it until the server is ready.
    fn document(&mut self, command: Command) {
        if !self.initialized {
            self.queue.push(command);
            return;
        }
        match command {
            Command::Open { path, language, text } => {
                if self.docs.contains_key(&path) {
                    return self.document(Command::Change { path, text });
                }
                let uri = uri_from_path(&path);
                self.notify(
                    "textDocument/didOpen",
                    json!({"textDocument": {"uri": uri, "languageId": language, "version": 1, "text": text}}),
                );
                self.docs.insert(path, OpenDoc { uri, version: 1, text });
            }
            Command::Change { path, text } => {
                let Some(doc) = self.docs.get_mut(&path) else { return };
                doc.version += 1;
                doc.text = text;
                let params = json!({
                    "textDocument": {"uri": doc.uri, "version": doc.version},
                    "contentChanges": [{"text": doc.text}],
                });
                self.notify("textDocument/didChange", params);
            }
            Command::Save { path } => {
                let Some(doc) = self.docs.get(&path) else { return };
                let mut params = json!({"textDocument": {"uri": doc.uri}});
                if self.save_text {
                    params["text"] = Value::String(doc.text.clone());
                }
                self.notify("textDocument/didSave", params);
            }
            Command::Close { path } => {
                let Some(doc) = self.docs.remove(&path) else { return };
                self.notify("textDocument/didClose", json!({"textDocument": {"uri": doc.uri}}));
            }
            Command::Stop | Command::Incoming(_) | Command::Ended => {}
        }
    }
}

fn run(id: usize, spec: Spec, root: PathBuf, sender: Sender<Command>, receiver: Receiver<Command>, shared: Weak<Shared>) {
    let report = |status: Status, name: Option<String>| {
        if let Some(shared) = shared.upgrade() {
            shared.set_status(id, status, name);
        }
    };
    let Some((program, path)) = find(&spec) else {
        report(Status::Missing, None);
        return;
    };
    let name = Path::new(&program.program).file_stem().map(|n| n.to_string_lossy().into_owned());
    report(Status::Starting, name);
    let mut child = match spawn(&program, &path, &root) {
        Ok(child) => child,
        Err(error) => {
            report(Status::Failed(error.to_string()), None);
            return;
        }
    };
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.kill();
        return;
    };

    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(message)) = rpc::read(&mut reader) {
            if sender.send(Command::Incoming(message)).is_err() {
                return;
            }
        }
        let _ = sender.send(Command::Ended);
    });
    // What the server says on its error stream, for when it fails.
    let said: Arc<Mutex<VecDeque<String>>> = Arc::default();
    let kept = said.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            let mut kept = kept.lock().unwrap_or_else(|p| p.into_inner());
            kept.push_back(line);
            if kept.len() > 20 {
                kept.pop_front();
            }
        }
    });

    let mut session = Session {
        id,
        root,
        shared: shared.clone(),
        stdin,
        next_request: 1,
        initialize: 0,
        shutdown: None,
        initialized: false,
        queue: Vec::new(),
        docs: HashMap::new(),
        encoding: Encoding::Utf16,
        save_text: false,
        progress: HashMap::new(),
        failure: None,
    };
    session.initialize();

    let mut stopping: Option<Instant> = None;
    let mut ended = false;
    loop {
        let command = match stopping {
            Some(deadline) => match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(command) => command,
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            },
            None => match receiver.recv() {
                Ok(command) => command,
                Err(_) => break,
            },
        };
        match command {
            Command::Stop => {
                if stopping.is_some() {
                    continue;
                }
                if !session.initialized {
                    break;
                }
                session.shutdown = Some(session.request("shutdown", Value::Null));
                stopping = Some(Instant::now() + STOP_WAIT);
            }
            Command::Ended => {
                ended = true;
                break;
            }
            Command::Incoming(message) => {
                if !session.incoming(message) {
                    break;
                }
            }
            command => {
                if stopping.is_none() {
                    session.document(command);
                }
            }
        }
    }

    session.notify("exit", Value::Null);
    let failure = session.failure.take();
    drop(session);
    let deadline = Instant::now() + STOP_WAIT;
    while !matches!(child.try_wait(), Ok(Some(_))) {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Stopped without being asked: say why, as far as the server said.
    if (ended && stopping.is_none()) || failure.is_some() {
        let detail = failure
            .filter(|f| !f.is_empty())
            .or_else(|| said.lock().unwrap_or_else(|p| p.into_inner()).back().cloned())
            .unwrap_or_default();
        if let Some(shared) = shared.upgrade() {
            shared.server_ended(id, Status::Failed(detail));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_are_counted_in_characters() {
        let line = "let \u{1F600} = \u{0434}a;";
        // After the emoji: 6 UTF-16 units, 9 UTF-8 bytes, 5 characters.
        assert_eq!(column(line, 6, Encoding::Utf16), 5);
        assert_eq!(column(line, 8, Encoding::Utf8), 5);
        assert_eq!(column(line, 5, Encoding::Utf32), 5);
        // Past the end of the line, the extra counts as it is.
        assert_eq!(column("ab", 4, Encoding::Utf16), 4);
    }

    #[test]
    fn diagnostics_become_problems() {
        let lines = ["fn main() {", "    let \u{1F600} = x;", "}"];
        let diagnostic = json!({
            "range": {"start": {"line": 1, "character": 13}, "end": {"line": 1, "character": 14}},
            "severity": 1,
            "code": "E0425",
            "source": "rustc",
            "message": "cannot find value `x` in this scope\n",
        });
        let found = problem(&diagnostic, Some(&lines), Encoding::Utf16).unwrap();
        assert_eq!((found.line, found.column, found.end_line, found.end_column), (1, 12, 1, 13));
        assert_eq!(found.severity, Severity::Error);
        assert_eq!(found.code.as_deref(), Some("E0425"));
        assert_eq!(found.message, "cannot find value `x` in this scope");
        let numbered = json!({
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 2}},
            "severity": 2, "code": 6133, "message": "unused",
        });
        let found = problem(&numbered, None, Encoding::Utf16).unwrap();
        assert_eq!((found.severity, found.code.as_deref(), found.source), (Severity::Warning, Some("6133"), None));
    }
}
