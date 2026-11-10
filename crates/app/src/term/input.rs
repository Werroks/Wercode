//! Turning key presses into the bytes a terminal program expects.
//!
//! Behaviour follows xterm conventions, the same ones VS Code's terminal uses.

use gpui_kit::Keystroke;

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_param(ks: &Keystroke) -> u8 {
    let m = &ks.modifiers;
    1 + (m.shift as u8) + 2 * (m.alt as u8) + 4 * (m.control as u8)
}

/// Returns the bytes to send for a key press, or `None` if the key means
/// nothing to a terminal (a lone modifier, for example).
pub fn encode(ks: &Keystroke, app_cursor: bool) -> Option<Vec<u8>> {
    let m = &ks.modifiers;
    let mods = modifier_param(ks);
    let plain = mods == 1;
    let key = ks.key.as_str();

    let csi_letter = |letter: char| -> Vec<u8> {
        if plain {
            if app_cursor {
                format!("\x1bO{letter}").into_bytes()
            } else {
                format!("\x1b[{letter}").into_bytes()
            }
        } else {
            format!("\x1b[1;{mods}{letter}").into_bytes()
        }
    };
    let csi_tilde = |n: u8| -> Vec<u8> {
        if plain {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{mods}~").into_bytes()
        }
    };
    let with_alt = |mut bytes: Vec<u8>| -> Vec<u8> {
        if m.alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };

    let bytes = match key {
        "enter" => with_alt(vec![b'\r']),
        "tab" if m.shift => b"\x1b[Z".to_vec(),
        "tab" => vec![b'\t'],
        "escape" => vec![0x1b],
        "backspace" if m.control => {
            // ConPTY reads 0x08 as Ctrl+Backspace; Unix shells expect ^W.
            vec![if cfg!(windows) { 0x08 } else { 0x17 }]
        }
        "backspace" => with_alt(vec![0x7f]),
        "up" => csi_letter('A'),
        "down" => csi_letter('B'),
        "right" => csi_letter('C'),
        "left" => csi_letter('D'),
        "home" => csi_letter('H'),
        "end" => csi_letter('F'),
        "insert" => csi_tilde(2),
        "delete" => csi_tilde(3),
        "pageup" => csi_tilde(5),
        "pagedown" => csi_tilde(6),
        "f1" | "f2" | "f3" | "f4" => {
            let letter = (b'P' + key.as_bytes()[1] - b'1') as char;
            if plain {
                format!("\x1bO{letter}").into_bytes()
            } else {
                format!("\x1b[1;{mods}{letter}").into_bytes()
            }
        }
        "f5" => csi_tilde(15),
        "f6" => csi_tilde(17),
        "f7" => csi_tilde(18),
        "f8" => csi_tilde(19),
        "f9" => csi_tilde(20),
        "f10" => csi_tilde(21),
        "f11" => csi_tilde(23),
        "f12" => csi_tilde(24),
        _ => {
            // Ctrl+Alt together is AltGr on many layouts: treat it as typing.
            let altgr = m.control && m.alt;
            if m.control && !altgr {
                let b = control_byte(key)?;
                return Some(vec![b]);
            }
            let text = match (&ks.key_char, key) {
                (Some(c), _) if !c.is_empty() => c.clone(),
                (_, "space") => " ".to_string(),
                (None, k) if m.alt && k.chars().count() == 1 => k.to_string(),
                _ => return None,
            };
            let mut bytes = text.into_bytes();
            if m.alt && !altgr {
                bytes.insert(0, 0x1b);
            }
            bytes
        }
    };
    Some(bytes)
}

/// The control character for Ctrl+key.
fn control_byte(key: &str) -> Option<u8> {
    match key {
        "space" | "2" | "@" => Some(0x00),
        "[" | "3" => Some(0x1b),
        "\\" | "4" => Some(0x1c),
        "]" | "5" => Some(0x1d),
        "6" | "^" => Some(0x1e),
        "/" | "7" | "_" | "-" => Some(0x1f),
        "8" | "?" => Some(0x7f),
        k if k.len() == 1 && k.as_bytes()[0].is_ascii_alphabetic() => {
            Some(k.as_bytes()[0].to_ascii_lowercase() & 0x1f)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::Modifiers;

    fn ks(key: &str, ch: Option<&str>, control: bool, alt: bool, shift: bool) -> Keystroke {
        Keystroke {
            modifiers: Modifiers { control, alt, shift, ..Default::default() },
            key: key.into(),
            key_char: ch.map(Into::into),
        }
    }

    #[test]
    fn typing_and_control() {
        assert_eq!(encode(&ks("a", Some("a"), false, false, false), false).unwrap(), b"a");
        assert_eq!(encode(&ks("ф", Some("ф"), false, false, false), false).unwrap(), "ф".as_bytes());
        assert_eq!(encode(&ks("c", None, true, false, false), false).unwrap(), [0x03]);
        assert_eq!(encode(&ks("enter", None, false, false, false), false).unwrap(), b"\r");
        assert_eq!(encode(&ks("b", None, false, true, false), false).unwrap(), b"\x1bb");
    }

    #[test]
    fn arrows_and_modes() {
        assert_eq!(encode(&ks("up", None, false, false, false), false).unwrap(), b"\x1b[A");
        assert_eq!(encode(&ks("up", None, false, false, false), true).unwrap(), b"\x1bOA");
        assert_eq!(encode(&ks("right", None, true, false, false), true).unwrap(), b"\x1b[1;5C");
        assert_eq!(encode(&ks("delete", None, false, false, false), false).unwrap(), b"\x1b[3~");
        assert_eq!(encode(&ks("f1", None, false, false, false), false).unwrap(), b"\x1bOP");
        assert_eq!(encode(&ks("f5", None, false, false, true), false).unwrap(), b"\x1b[15;2~");
        assert_eq!(encode(&ks("tab", None, false, false, true), false).unwrap(), b"\x1b[Z");
    }

    #[test]
    fn lone_modifier_sends_nothing() {
        assert!(encode(&ks("shift", None, false, false, true), false).is_none());
    }
}
