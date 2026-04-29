//! HID class driver — boot-protocol keyboard and mouse.
//!
//! References:
//!   * USB HID 1.11 (May 27 2001) — §B "Boot Interface Subclass"
//!   * USB HID Usage Tables 1.5 — §10 keyboard usage IDs
//!
//! The boot protocol gives us a fixed report layout:
//!
//!   keyboard (8 bytes):
//!     [0]    modifier mask: LCtrl,LShift,LAlt,LGui,RCtrl,RShift,RAlt,RGui
//!     [1]    reserved (always 0)
//!     [2..7] up to 6 simultaneously-pressed key usages (HID Usage Page 7)
//!
//!   mouse (3 bytes):
//!     [0]    button mask: bit 0 = LMB, bit 1 = RMB, bit 2 = MMB
//!     [1]    int8 X delta
//!     [2]    int8 Y delta
//!
//! Real (non-boot-protocol) HID parses arbitrary report descriptors; we
//! don't yet, so the SET_PROTOCOL(BOOT) request is mandatory for any
//! HID interface we want to talk to.
//!
//! All state machines here are pure functions of bytes — no host-controller
//! coupling — so they're trivially testable against synthetic reports.

// ---------------------------------------------------------------------------
// Modifier-mask bits (HID 1.11 §B.1)
// ---------------------------------------------------------------------------

pub const KMOD_LCTRL:  u8 = 1 << 0;
pub const KMOD_LSHIFT: u8 = 1 << 1;
pub const KMOD_LALT:   u8 = 1 << 2;
pub const KMOD_LGUI:   u8 = 1 << 3;
pub const KMOD_RCTRL:  u8 = 1 << 4;
pub const KMOD_RSHIFT: u8 = 1 << 5;
pub const KMOD_RALT:   u8 = 1 << 6;
pub const KMOD_RGUI:   u8 = 1 << 7;

// ---------------------------------------------------------------------------
// Keyboard report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyboardReport {
    pub modifiers: u8,
    pub keys: [u8; 6],
}

impl KeyboardReport {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 { return None; }
        // Byte 1 is reserved per spec; some devices put garbage there.
        // We tolerate that — only modifiers + keys[2..7] matter.
        Some(Self {
            modifiers: bytes[0],
            keys: [bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]],
        })
    }

    /// Returns true if `usage` (HID Usage Page 7 keycode) is currently
    /// in the report's key-down array.  HID's "rollover" sentinel
    /// 0x01..0x03 (ErrorRollOver / POSTFail / ErrorUndefined) reports
    /// over-saturated input and should be treated as "no keys", per
    /// HID 1.11 §B.1.
    pub fn is_pressed(&self, usage: u8) -> bool {
        if usage == 0 { return false; }
        // Detect the rollover/error sentinels (all six slots = 0x01).
        if self.keys.iter().all(|&k| k == 0x01) { return false; }
        self.keys.iter().any(|&k| k == usage)
    }

    pub fn shift_held(&self) -> bool {
        (self.modifiers & (KMOD_LSHIFT | KMOD_RSHIFT)) != 0
    }
    pub fn ctrl_held(&self) -> bool {
        (self.modifiers & (KMOD_LCTRL | KMOD_RCTRL)) != 0
    }
    pub fn alt_held(&self) -> bool {
        (self.modifiers & (KMOD_LALT | KMOD_RALT)) != 0
    }
}

/// Diff two consecutive keyboard reports and return (newly-pressed,
/// newly-released) key arrays.  The classic HID host approach.
pub fn keyboard_diff(prev: &KeyboardReport, cur: &KeyboardReport) -> (alloc::vec::Vec<u8>, alloc::vec::Vec<u8>) {
    let mut pressed = alloc::vec::Vec::new();
    let mut released = alloc::vec::Vec::new();
    // Skip rollover sentinels.
    let prev_rollover = prev.keys.iter().all(|&k| k == 0x01);
    let cur_rollover  = cur.keys.iter().all(|&k| k == 0x01);
    if prev_rollover || cur_rollover { return (pressed, released); }
    for &k in &cur.keys {
        if k != 0 && !prev.keys.contains(&k) { pressed.push(k); }
    }
    for &k in &prev.keys {
        if k != 0 && !cur.keys.contains(&k) { released.push(k); }
    }
    (pressed, released)
}

/// Translate a HID Usage Page 7 keycode (US layout) to a printable
/// ASCII char, or '\0' for non-printable / unknown.  Honours `shift`.
///
/// Reference: HID Usage Tables 1.5 §10 Keyboard/Keypad Page (0x07).
pub fn usage_to_ascii(usage: u8, shift: bool) -> u8 {
    // 0x04..0x1D: a..z
    if usage >= 0x04 && usage <= 0x1D {
        let lower = b'a' + (usage - 0x04);
        return if shift { lower - b'a' + b'A' } else { lower };
    }
    // 0x1E..0x27: 1 2 3 4 5 6 7 8 9 0  (Note: 0 is at 0x27)
    if usage >= 0x1E && usage <= 0x26 {
        let unshifted = b'1' + (usage - 0x1E);
        return if shift {
            // Shifted top row: !@#$%^&*(
            match usage {
                0x1E => b'!', 0x1F => b'@', 0x20 => b'#', 0x21 => b'$',
                0x22 => b'%', 0x23 => b'^', 0x24 => b'&', 0x25 => b'*',
                0x26 => b'(',
                _ => unshifted,
            }
        } else { unshifted };
    }
    if usage == 0x27 { return if shift { b')' } else { b'0' }; }
    // Selected punctuation / whitespace.
    match usage {
        0x28 => b'\n',  // Enter
        0x29 => 0x1B,   // Esc
        0x2A => 0x08,   // Backspace
        0x2B => b'\t',  // Tab
        0x2C => b' ',   // Space
        0x2D => if shift { b'_' } else { b'-' },
        0x2E => if shift { b'+' } else { b'=' },
        0x2F => if shift { b'{' } else { b'[' },
        0x30 => if shift { b'}' } else { b']' },
        0x31 => if shift { b'|' } else { b'\\' },
        0x33 => if shift { b':' } else { b';' },
        0x34 => if shift { b'"' } else { b'\'' },
        0x35 => if shift { b'~' } else { b'`' },
        0x36 => if shift { b'<' } else { b',' },
        0x37 => if shift { b'>' } else { b'.' },
        0x38 => if shift { b'?' } else { b'/' },
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Mouse report
// ---------------------------------------------------------------------------

pub const MBTN_LEFT:   u8 = 1 << 0;
pub const MBTN_RIGHT:  u8 = 1 << 1;
pub const MBTN_MIDDLE: u8 = 1 << 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseReport {
    pub buttons: u8,
    pub dx: i8,
    pub dy: i8,
}

impl MouseReport {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 3 { return None; }
        Some(Self {
            buttons: bytes[0],
            dx: bytes[1] as i8,
            dy: bytes[2] as i8,
        })
    }

    pub fn left(&self)   -> bool { (self.buttons & MBTN_LEFT)   != 0 }
    pub fn right(&self)  -> bool { (self.buttons & MBTN_RIGHT)  != 0 }
    pub fn middle(&self) -> bool { (self.buttons & MBTN_MIDDLE) != 0 }
}
