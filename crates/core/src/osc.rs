//! Picks the shell's working directory out of the raw PTY stream.
//!
//! Shells announce it with `OSC 7 ; file://host/path` (most Unix shells, when
//! set up) or `OSC 9 ; 9 ; path` (the Windows convention). The emulator ignores
//! both, so the bytes are scanned here on their way to it.

use std::path::PathBuf;

/// Longest OSC payload worth keeping; anything longer is not a directory.
const MAX_PAYLOAD: usize = 4096;

#[derive(Default)]
enum State {
    #[default]
    Ground,
    /// Saw ESC.
    Escape,
    /// Inside `ESC ]` ... collecting the payload.
    Osc,
    /// Inside an OSC and saw ESC (expecting `\` to finish it).
    OscEscape,
}

#[derive(Default)]
pub struct CwdScanner {
    state: State,
    payload: Vec<u8>,
}

impl CwdScanner {
    /// Feeds a chunk of output. Returns the last directory announced in it.
    /// Sequences may be split across chunks.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<PathBuf> {
        let mut found = None;
        for &b in bytes {
            match self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.state = State::Escape;
                    }
                }
                State::Escape => {
                    self.state = match b {
                        b']' => {
                            self.payload.clear();
                            State::Osc
                        }
                        0x1b => State::Escape,
                        _ => State::Ground,
                    };
                }
                State::Osc => match b {
                    0x07 => {
                        found = parse(&self.payload).or(found);
                        self.state = State::Ground;
                    }
                    0x1b => self.state = State::OscEscape,
                    _ => {
                        if self.payload.len() < MAX_PAYLOAD {
                            self.payload.push(b);
                        }
                    }
                },
                State::OscEscape => {
                    if b == b'\\' {
                        found = parse(&self.payload).or(found);
                    }
                    self.state = State::Ground;
                }
            }
        }
        found
    }
}

fn parse(payload: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(payload).ok()?;
    if let Some(path) = text.strip_prefix("9;9;") {
        let path = path.trim_matches('"');
        return (!path.is_empty()).then(|| PathBuf::from(path));
    }
    let url = text.strip_prefix("7;")?;
    let rest = url.strip_prefix("file://")?;
    // Skip the host name: the path starts at the first slash.
    let path = &rest[rest.find('/')?..];
    let decoded = percent_decode(path);
    // `file:///C:/Users` on Windows: drop the slash before the drive letter.
    let bytes = decoded.as_bytes();
    let decoded = if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' {
        decoded[1..].to_string()
    } else {
        decoded
    };
    (!decoded.is_empty()).then(|| PathBuf::from(decoded))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_style() {
        let mut s = CwdScanner::default();
        let got = s.feed(b"text\x1b]9;9;C:\\Users\\me\\proj\x07PS> ");
        assert_eq!(got, Some(PathBuf::from("C:\\Users\\me\\proj")));
    }

    #[test]
    fn osc7_with_escapes_and_st_terminator() {
        let mut s = CwdScanner::default();
        let got = s.feed(b"\x1b]7;file://host/home/me/my%20dir\x1b\\$ ");
        assert_eq!(got, Some(PathBuf::from("/home/me/my dir")));
        let got = s.feed(b"\x1b]7;file:///C:/Users/me\x07");
        assert_eq!(got, Some(PathBuf::from("C:/Users/me")));
    }

    #[test]
    fn split_across_chunks_and_other_osc_ignored() {
        let mut s = CwdScanner::default();
        assert_eq!(s.feed(b"\x1b]0;window title\x07\x1b]9;9;D:\\wo"), None);
        assert_eq!(s.feed(b"rk\x07"), Some(PathBuf::from("D:\\work")));
        assert_eq!(s.feed(b"\x1b[31mred\x1b[0m"), None);
    }
}
