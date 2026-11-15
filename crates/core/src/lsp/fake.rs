//! A tiny language server for tests. A test binary runs itself as one: the
//! test that calls [`serve`] is started again in a child process, with the
//! arguments from [`test_args`], and talks the protocol on its own streams.
//!
//! It calls the word `bad` an error and `meh` a warning, wherever they are.

use std::io;

use serde_json::{Value, json};

use super::rpc;

/// Arguments that make a test binary run only the test called `name`, and
/// print as little as it can around it.
pub fn test_args(name: &str) -> Vec<String> {
    [name, "--exact", "--nocapture", "--test-threads=1", "--quiet"].map(String::from).to_vec()
}

fn send(out: &mut impl io::Write, message: Value) {
    let _ = rpc::write(out, &message);
}

fn diagnose(out: &mut impl io::Write, uri: &Value, text: &str) {
    let mut diagnostics = Vec::new();
    for (number, line) in text.split('\n').enumerate() {
        for (word, severity, message) in [("bad", 1, "bad word"), ("meh", 2, "weak word")] {
            if let Some(at) = line.find(word) {
                // Columns in UTF-16 units, as the protocol counts them by default.
                let start = line[..at].encode_utf16().count();
                diagnostics.push(json!({
                    "range": {
                        "start": {"line": number, "character": start},
                        "end": {"line": number, "character": start + word.len()},
                    },
                    "severity": severity,
                    "source": "fake",
                    "message": message,
                }));
            }
        }
    }
    let params = json!({"uri": uri, "diagnostics": diagnostics});
    send(out, json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": params}));
}

/// Answers on standard input and output until told to exit.
pub fn serve() -> ! {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut out = io::stdout().lock();
    while let Ok(Some(message)) = rpc::read(&mut input) {
        let id = message.get("id").cloned();
        let params = &message["params"];
        match message["method"].as_str() {
            Some("initialize") => {
                let result = json!({"capabilities": {"textDocumentSync": 1}});
                send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": result}));
            }
            Some("initialized") => {
                // A server asks its editor things too, and reports work in progress.
                let create = json!({"token": "load"});
                send(&mut out, json!({"jsonrpc": "2.0", "id": "c1", "method": "window/workDoneProgress/create", "params": create}));
                let begin = json!({"token": "load", "value": {"kind": "begin", "title": "Loading"}});
                send(&mut out, json!({"jsonrpc": "2.0", "method": "$/progress", "params": begin}));
                let end = json!({"token": "load", "value": {"kind": "end"}});
                send(&mut out, json!({"jsonrpc": "2.0", "method": "$/progress", "params": end}));
            }
            Some("textDocument/didOpen") => {
                let doc = &params["textDocument"];
                diagnose(&mut out, &doc["uri"], doc["text"].as_str().unwrap_or_default());
            }
            Some("textDocument/didChange") => {
                let text = params["contentChanges"].as_array().and_then(|c| c.last()).map(|c| &c["text"]);
                let text = text.and_then(Value::as_str).unwrap_or_default();
                diagnose(&mut out, &params["textDocument"]["uri"], text);
            }
            Some("textDocument/didClose") => diagnose(&mut out, &params["textDocument"]["uri"], ""),
            Some("exit") => std::process::exit(0),
            Some(_) if id.is_some() => send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": null})),
            _ => {}
        }
    }
    std::process::exit(0)
}
