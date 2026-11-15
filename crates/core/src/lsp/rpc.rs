//! The wire format of the language server protocol: a `Content-Length`
//! header, an empty line, then that many bytes of JSON.

use std::io::{self, BufRead, Write};

use serde_json::Value;

/// A message larger than this is taken as a broken stream, not read.
const LARGEST: usize = 256 * 1024 * 1024;

pub fn write(out: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(&body)?;
    out.flush()
}

/// Reads the next message; `None` once the stream has ended. Lines that are
/// not headers are skipped: some servers print notes of their own to the
/// same stream before they start talking.
pub fn read(input: &mut impl BufRead) -> io::Result<Option<Value>> {
    loop {
        let mut length = None;
        let mut line = Vec::new();
        loop {
            line.clear();
            if input.read_until(b'\n', &mut line)? == 0 {
                return Ok(None);
            }
            let text = String::from_utf8_lossy(&line);
            let text = text.trim_end_matches(['\r', '\n']);
            if text.is_empty() {
                if length.is_some() {
                    break;
                }
                continue;
            }
            // Found anywhere on the line: a note printed without a line end
            // runs into the header that follows it.
            const HEADER: &str = "content-length:";
            if let Some(at) = text.to_ascii_lowercase().find(HEADER) {
                length = text[at + HEADER.len()..].trim().parse::<usize>().ok();
            }
        }
        let length = length.unwrap_or_default();
        if length > LARGEST {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
        }
        let mut body = vec![0; length];
        input.read_exact(&mut body)?;
        if let Ok(message) = serde_json::from_slice(&body) {
            return Ok(Some(message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_survive_the_round_trip_and_stray_lines_are_skipped() {
        let mut stream = Vec::new();
        stream.extend_from_slice(b"\nrunning 1 test\ntest x ... ");
        write(&mut stream, &json!({"id": 1, "method": "initialize"})).unwrap();
        stream.extend_from_slice(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n");
        write(&mut stream, &json!({"text": "\u{1F600} done"})).unwrap();

        let mut input = io::Cursor::new(stream);
        assert_eq!(read(&mut input).unwrap(), Some(json!({"id": 1, "method": "initialize"})));
        assert_eq!(read(&mut input).unwrap(), Some(json!({"text": "\u{1F600} done"})));
        assert_eq!(read(&mut input).unwrap(), None);
    }
}
