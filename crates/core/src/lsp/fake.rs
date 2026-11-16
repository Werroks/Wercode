//! A tiny language server for tests. A test binary runs itself as one: the
//! test that calls [`serve`] is started again in a child process, with the
//! arguments from [`test_args`], and talks the protocol on its own streams.
//!
//! It calls the word `bad` an error and `meh` a warning, wherever they are.
//! Asked about a word, it says which word it is, and that the word is
//! defined where the file first has it.

use std::collections::HashMap;
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

/// The word at a position counted in UTF-16 units: its line, and where the
/// word starts and ends there in bytes.
fn word_at<'a>(text: &'a str, position: &Value) -> Option<(&'a str, usize, usize)> {
    let line = text.split('\n').nth(position["line"].as_u64()? as usize)?;
    let units = position["character"].as_u64()? as usize;
    let mut at = line.len();
    let mut used = 0;
    for (index, c) in line.char_indices() {
        if used >= units {
            at = index;
            break;
        }
        used += c.len_utf16();
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = line[..at].char_indices().rev().take_while(|(_, c)| is_word(*c)).last().map_or(at, |(i, _)| i);
    let end = at + line[at..].chars().take_while(|c| is_word(*c)).map(char::len_utf8).sum::<usize>();
    (start < end).then_some((line, start, end))
}

/// A range within one line, from byte offsets to UTF-16 units.
fn range(number: usize, line: &str, start: usize, end: usize) -> Value {
    let units = |to: usize| line[..to].encode_utf16().count();
    json!({"start": {"line": number, "character": units(start)}, "end": {"line": number, "character": units(end)}})
}

/// Answers on standard input and output until told to exit.
pub fn serve() -> ! {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut out = io::stdout().lock();
    let mut texts: HashMap<String, String> = HashMap::new();
    while let Ok(Some(message)) = rpc::read(&mut input) {
        let id = message.get("id").cloned();
        let params = &message["params"];
        match message["method"].as_str() {
            Some("initialize") => {
                let result = json!({"capabilities": {
                    "textDocumentSync": 1,
                    "hoverProvider": true,
                    "definitionProvider": {"workDoneProgress": false},
                }});
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
                let text = doc["text"].as_str().unwrap_or_default();
                texts.insert(doc["uri"].as_str().unwrap_or_default().into(), text.into());
                diagnose(&mut out, &doc["uri"], text);
            }
            Some("textDocument/didChange") => {
                let text = params["contentChanges"].as_array().and_then(|c| c.last()).map(|c| &c["text"]);
                let text = text.and_then(Value::as_str).unwrap_or_default();
                texts.insert(params["textDocument"]["uri"].as_str().unwrap_or_default().into(), text.into());
                diagnose(&mut out, &params["textDocument"]["uri"], text);
            }
            Some(method @ ("textDocument/hover" | "textDocument/definition")) => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                let text = texts.get(uri).map(String::as_str).unwrap_or_default();
                let number = params["position"]["line"].as_u64().unwrap_or_default() as usize;
                let result = match word_at(text, &params["position"]) {
                    Some((line, start, end)) if method.ends_with("hover") => json!({
                        "contents": {"kind": "markdown", "value": format!("the word `{}`", &line[start..end])},
                        "range": range(number, line, start, end),
                    }),
                    Some((line, start, end)) => {
                        let word = &line[start..end];
                        let first = text.split('\n').enumerate().find_map(|(n, l)| l.find(word).map(|at| (n, l, at)));
                        match first {
                            Some((n, l, at)) => json!([{"uri": uri, "range": range(n, l, at, at + word.len())}]),
                            None => Value::Null,
                        }
                    }
                    None => Value::Null,
                };
                send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": result}));
            }
            Some("textDocument/didClose") => diagnose(&mut out, &params["textDocument"]["uri"], ""),
            Some("exit") => std::process::exit(0),
            Some(_) if id.is_some() => send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": null})),
            _ => {}
        }
    }
    std::process::exit(0)
}
