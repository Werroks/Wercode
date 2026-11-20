//! A tiny language server for tests. A test binary runs itself as one: the
//! test that calls [`serve`] is started again in a child process, with the
//! arguments from [`test_args`], and talks the protocol on its own streams.
//!
//! It calls the word `bad` an error and `meh` a warning, wherever they are.
//! Asked about a word, it says which word it is, and that the word is
//! defined where the file first has it. It offers the words of the file to
//! complete with, and formats by cutting the spaces at the ends of lines.
//!
//! It offers to replace `bad` with `good` (a quick fix with its change), to
//! shout the line (a change sent only when asked for) and to say hi (a
//! command, after which it asks the editor to put `hi` at the top). It
//! renames a word wherever the open files have it, lists `fn` and `struct`
//! lines as symbols, and notes `: i32` after the name in a `let` line.

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

/// `fn` and `struct` lines as symbols; `fn` lines indented under a
/// `struct` line are inside it.
fn symbols(text: &str) -> Value {
    let mut top: Vec<Value> = Vec::new();
    for (number, line) in text.split('\n').enumerate() {
        let trimmed = line.trim_start();
        let (kind, rest) = match (trimmed.strip_prefix("fn "), trimmed.strip_prefix("struct ")) {
            (Some(rest), _) => (12, rest),
            (_, Some(rest)) => (23, rest),
            _ => continue,
        };
        let name = rest.split(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or_default();
        let at = line.find(name).unwrap_or(0);
        let symbol = json!({
            "name": name,
            "kind": kind,
            "range": range(number, line, 0, line.len()),
            "selectionRange": range(number, line, at, at + name.len()),
            "children": [],
        });
        let nested = line.starts_with(' ') && top.last().is_some_and(|last| last["kind"] == 23);
        match top.last_mut() {
            Some(parent) if nested => {
                parent["children"].as_array_mut().unwrap().push(symbol);
                parent["range"]["end"] = json!({"line": number, "character": line.encode_utf16().count()});
            }
            _ => top.push(symbol),
        }
    }
    Value::Array(top)
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
                    "completionProvider": {"triggerCharacters": ["."]},
                    "documentFormattingProvider": true,
                    "codeActionProvider": {"resolveProvider": true},
                    "executeCommandProvider": {"commands": ["fake.hi"]},
                    "renameProvider": true,
                    "documentSymbolProvider": true,
                    "workspaceSymbolProvider": true,
                    "inlayHintProvider": true,
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
            Some("textDocument/completion") => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                let text = texts.get(uri).map(String::as_str).unwrap_or_default();
                let mut words: Vec<&str> = text.split(|c: char| !c.is_alphanumeric() && c != '_').collect();
                words.retain(|word| !word.is_empty());
                words.sort();
                words.dedup();
                let items: Vec<Value> = words.iter().map(|word| json!({"label": word, "kind": 1})).collect();
                let result = json!({"isIncomplete": false, "items": items});
                send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": result}));
            }
            Some("textDocument/formatting") => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                let text = texts.get(uri).map(String::as_str).unwrap_or_default();
                let mut edits = Vec::new();
                for (number, line) in text.split('\n').enumerate() {
                    let kept = line.trim_end().len();
                    if kept < line.len() {
                        edits.push(json!({"range": range(number, line, kept, line.len()), "newText": ""}));
                    }
                }
                send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": edits}));
            }
            Some("textDocument/codeAction") => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                let mut actions = Vec::new();
                for diagnostic in params["context"]["diagnostics"].as_array().map(Vec::as_slice).unwrap_or_default() {
                    if diagnostic["message"] == "bad word" {
                        let edit = json!({"changes": {uri: [{"range": diagnostic["range"], "newText": "good"}]}});
                        actions.push(json!({"title": "Replace bad with good", "kind": "quickfix", "isPreferred": true, "edit": edit}));
                    }
                }
                let line = params["range"]["start"]["line"].clone();
                actions.push(json!({"title": "Shout the line", "kind": "refactor", "data": {"uri": uri, "line": line}}));
                actions.push(json!({"title": "Say hi", "command": "fake.hi", "arguments": [uri]}));
                send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": actions}));
            }
            Some("codeAction/resolve") => {
                let uri = params["data"]["uri"].as_str().unwrap_or_default();
                let number = params["data"]["line"].as_u64().unwrap_or_default() as usize;
                let text = texts.get(uri).map(String::as_str).unwrap_or_default();
                let line = text.split('\n').nth(number).unwrap_or_default();
                let mut action = params.clone();
                let change = json!({"range": range(number, line, 0, line.len()), "newText": line.to_uppercase()});
                action["edit"] = json!({"documentChanges": [{"textDocument": {"uri": uri, "version": null}, "edits": [change]}]});
                send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": action}));
            }
            Some("workspace/executeCommand") => {
                send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": null}));
                let uri = params["arguments"][0].clone();
                let top = json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}});
                let edit = json!({"changes": {uri.as_str().unwrap_or_default(): [{"range": top, "newText": "hi\n"}]}});
                send(&mut out, json!({"jsonrpc": "2.0", "id": "edit", "method": "workspace/applyEdit", "params": {"edit": edit}}));
            }
            Some("textDocument/rename") => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                let text = texts.get(uri).map(String::as_str).unwrap_or_default();
                let new_name = params["newName"].as_str().unwrap_or_default();
                let result = match word_at(text, &params["position"]) {
                    Some((line, start, end)) => {
                        let word = &line[start..end];
                        let mut changes = serde_json::Map::new();
                        for (doc, text) in &texts {
                            let mut edits = Vec::new();
                            for (number, line) in text.split('\n').enumerate() {
                                for (at, _) in line.match_indices(word) {
                                    edits.push(json!({"range": range(number, line, at, at + word.len()), "newText": new_name}));
                                }
                            }
                            if !edits.is_empty() {
                                changes.insert(doc.clone(), Value::Array(edits));
                            }
                        }
                        json!({"changes": changes})
                    }
                    None => Value::Null,
                };
                send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": result}));
            }
            Some("textDocument/documentSymbol") => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                let text = texts.get(uri).map(String::as_str).unwrap_or_default();
                send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": symbols(text)}));
            }
            Some("workspace/symbol") => {
                let query = params["query"].as_str().unwrap_or_default();
                let mut found = Vec::new();
                for (uri, text) in &texts {
                    for (number, line) in text.split('\n').enumerate() {
                        if let Some(name) = line.trim_start().strip_prefix("fn ").map(|rest| rest.split('(').next().unwrap_or(rest))
                            && name.contains(query)
                        {
                            let at = line.find(name).unwrap_or(0);
                            let location = json!({"uri": uri, "range": range(number, line, at, at + name.len())});
                            found.push(json!({"name": name, "kind": 12, "location": location}));
                        }
                    }
                }
                send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": found}));
            }
            Some("textDocument/inlayHint") => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                let text = texts.get(uri).map(String::as_str).unwrap_or_default();
                let mut hints = Vec::new();
                for (number, line) in text.split('\n').enumerate() {
                    if let Some(rest) = line.trim_start().strip_prefix("let ") {
                        let name = rest.split([' ', '=']).next().unwrap_or_default();
                        let end = line.find(name).unwrap_or(0) + name.len();
                        let units = line[..end].encode_utf16().count();
                        hints.push(json!({"position": {"line": number, "character": units}, "label": [{"value": ": i32"}], "kind": 1}));
                    }
                }
                send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": hints}));
            }
            Some("exit") => std::process::exit(0),
            Some(_) if id.is_some() => send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": null})),
            _ => {}
        }
    }
    std::process::exit(0)
}
