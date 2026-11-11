//! Encoding pointer events for programs that asked for them (vim, htop, TUIs).

pub const LEFT: u8 = 0;
pub const MIDDLE: u8 = 1;
pub const RIGHT: u8 = 2;
/// Motion with no button held.
pub const NONE: u8 = 3;
pub const WHEEL_UP: u8 = 64;
pub const WHEEL_DOWN: u8 = 65;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Press,
    Release,
    Motion,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

/// One mouse report. `col` and `row` are zero-based viewport cells. Returns
/// nothing when the legacy encoding cannot express the position.
pub fn encode(button: u8, kind: Kind, col: usize, row: usize, mods: Mods, sgr: bool) -> Option<Vec<u8>> {
    let mut code = button as u32;
    if kind == Kind::Motion {
        code += 32;
    }
    if mods.shift {
        code += 4;
    }
    if mods.alt {
        code += 8;
    }
    if mods.control {
        code += 16;
    }
    if sgr {
        let end = if kind == Kind::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{};{};{}{}", code, col + 1, row + 1, end).into_bytes());
    }
    // Legacy: one byte per field, offset by 32; releases do not say which button.
    if col > 222 || row > 222 {
        return None;
    }
    if kind == Kind::Release {
        code = (code & !0b11) | 3;
    }
    Some(vec![0x1b, b'[', b'M', (32 + code) as u8, (33 + col) as u8, (33 + row) as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr() {
        let m = Mods::default();
        assert_eq!(encode(LEFT, Kind::Press, 0, 0, m, true).unwrap(), b"\x1b[<0;1;1M");
        assert_eq!(encode(LEFT, Kind::Release, 9, 4, m, true).unwrap(), b"\x1b[<0;10;5m");
        assert_eq!(encode(LEFT, Kind::Motion, 2, 3, m, true).unwrap(), b"\x1b[<32;3;4M");
        let ctrl = Mods { control: true, ..m };
        assert_eq!(encode(WHEEL_DOWN, Kind::Press, 0, 0, ctrl, true).unwrap(), b"\x1b[<81;1;1M");
    }

    #[test]
    fn legacy() {
        let m = Mods::default();
        assert_eq!(encode(RIGHT, Kind::Press, 0, 0, m, false).unwrap(), b"\x1b[M\x22\x21\x21");
        assert_eq!(encode(RIGHT, Kind::Release, 1, 1, m, false).unwrap(), b"\x1b[M\x23\x22\x22");
        assert!(encode(LEFT, Kind::Press, 300, 0, m, false).is_none());
    }
}
