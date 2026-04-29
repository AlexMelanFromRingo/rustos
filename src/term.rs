//! VT-220 / xterm terminal state machine — Paul Williams DEC parser.
//!
//! This is a *pure* parser: it consumes a stream of bytes and emits
//! `Event`s.  The caller (VGA buffer, framebuffer renderer, serial
//! tee, anything else) decides how to react.  Splitting parser from
//! sink lets us reuse the same FSM for graphical and text outputs,
//! and lets the test suite assert on Event sequences without rendering
//! anything.
//!
//! ## State model
//!
//! Implements the classic five states from <https://vt100.net/emu/dec_ansi_parser>:
//!
//!     Ground          — normal printable / control characters
//!     Escape          — saw 0x1B, deciding what comes next
//!     CsiEntry        — saw `ESC [`, collecting params
//!     CsiParam        — inside parameter bytes (digits, ';')
//!     CsiIntermediate — saw an intermediate byte (0x20-0x2F)
//!     OscString       — `ESC ]` ... terminated by ST/BEL
//!
//! Anywhere a character outside that state's expected range arrives,
//! we drop the partial sequence and return to Ground (matches
//! xterm's behaviour for malformed input).
//!
//! ## Coverage
//!
//! * SGR (m): reset, bold/dim, fg/bg 30-37/40-47/90-97/100-107,
//!            38;5;n / 48;5;n indexed-256, 39/49 default fg/bg.
//! * Cursor: ESC[<n>A/B/C/D, ESC[<r>;<c>H, ESC[s/u (DEC private),
//!           ESC 7 / ESC 8 (DECSC/DECRC).
//! * Erase:  ESC[2J, ESC[K, ESC[<n>K.
//! * Mode:   ESC[?1049h/l (alternate screen on/off), ESC[?25h/l
//!           (cursor visibility).
//! * OSC:    ESC]0;TITLE BEL — captured as `Event::SetTitle`.
//! * Tabs:   0x09 → `Event::HorizontalTab`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Maximum number of CSI parameters; xterm accepts up to 16 — anything
/// beyond is silently ignored, like a real terminal would.
pub const MAX_PARAMS: usize = 16;

/// Indexed color slot in xterm's 256-color palette.  The renderer is
/// free to map these to whatever the underlying display can show.
pub type Color256 = u8;

/// Decoded event the parser hands to the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A printable character (after escape filtering).  May be
    /// extended ASCII / Latin-1; UTF-8 is consumed byte-by-byte.
    Print(u8),
    /// Control byte not absorbed by the state machine: backspace,
    /// newline, CR, tab, bell.
    Newline,
    CarriageReturn,
    Backspace,
    HorizontalTab,
    Bell,

    /// Cursor positioning (1-based, top-left = (1,1)).
    CursorTo { row: u16, col: u16 },
    CursorUp(u16),
    CursorDown(u16),
    CursorForward(u16),
    CursorBack(u16),
    SaveCursor,
    RestoreCursor,
    ShowCursor(bool),

    /// Erase in display: 0 = cursor → end, 1 = start → cursor, 2 = all,
    /// 3 = saved-lines.
    EraseDisplay(u8),
    /// Erase in line: 0/1/2.
    EraseLine(u8),
    /// Scroll up `n` lines (the contents below the top scroll into view).
    ScrollUp(u16),
    /// Scroll down `n` lines.
    ScrollDown(u16),

    /// Switch alternate screen buffer (xterm DECSET 1049).
    AlternateScreen(bool),

    /// Reset all SGR attrs.
    ResetAttrs,
    SetBold(bool),
    SetUnderline(bool),
    SetReverse(bool),
    SetForeground(Color256),
    SetBackground(Color256),
    SetDefaultFg,
    SetDefaultBg,

    /// OSC 0/2;TITLE — terminal window title.
    SetTitle(String),

    /// Anything we couldn't decode — caller can ignore.
    Unhandled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    CsiEntry,
    CsiParam,
    /// CsiIntermediate — saw an intermediate byte in 0x20..=0x2F.
    /// xterm just ignores intermediates for our subset, but the state
    /// must exist so we don't mis-interpret the final byte.
    CsiIntermediate,
    OscString,
}

/// A complete VT parser.  Feed it bytes via `feed`; it returns a
/// (possibly empty) vector of `Event`s for the caller to act on.
pub struct Parser {
    state: State,
    /// Whether the current CSI sequence opened with `?` (DEC private).
    private: bool,
    /// Decoded CSI parameter list (up to MAX_PARAMS).
    params: [u32; MAX_PARAMS],
    n_params: usize,
    /// Currently-accumulating numeric parameter.
    cur_param: u32,
    have_digit: bool,
    /// OSC payload accumulator.
    osc: String,
}

impl Parser {
    pub const fn new() -> Self {
        Parser {
            state: State::Ground,
            private: false,
            params: [0; MAX_PARAMS],
            n_params: 0,
            cur_param: 0,
            have_digit: false,
            osc: String::new(),
        }
    }

    fn flush_param(&mut self) {
        if self.have_digit && self.n_params < MAX_PARAMS {
            self.params[self.n_params] = self.cur_param;
            self.n_params += 1;
        }
        self.cur_param = 0;
        self.have_digit = false;
    }

    fn reset(&mut self) {
        self.state = State::Ground;
        self.private = false;
        self.n_params = 0;
        self.cur_param = 0;
        self.have_digit = false;
        self.osc.clear();
    }

    /// Convenience helper: feed one byte, return all the events it
    /// generated.  Most byte feeds yield 0 or 1 events; CSI/OSC
    /// finalisation can yield several.
    pub fn feed(&mut self, byte: u8) -> Vec<Event> {
        let mut out = Vec::new();
        self.step(byte, &mut out);
        out
    }

    fn step(&mut self, b: u8, out: &mut Vec<Event>) {
        // Universal: ESC always starts a fresh escape sequence and
        // CAN can always abort.
        if b == 0x1B {
            self.reset();
            self.state = State::Escape;
            return;
        }
        if b == 0x18 || b == 0x1A {
            self.reset();
            return;
        }

        match self.state {
            State::Ground => match b {
                0x07 => out.push(Event::Bell),
                0x08 => out.push(Event::Backspace),
                0x09 => out.push(Event::HorizontalTab),
                0x0A | 0x0B | 0x0C => out.push(Event::Newline),
                0x0D => out.push(Event::CarriageReturn),
                0x20..=0xFF => out.push(Event::Print(b)),
                _ => {} // ignore other C0
            },

            State::Escape => match b {
                b'[' => {
                    self.state = State::CsiEntry;
                }
                b']' => {
                    self.state = State::OscString;
                    self.osc.clear();
                }
                b'7' => { out.push(Event::SaveCursor); self.reset(); }
                b'8' => { out.push(Event::RestoreCursor); self.reset(); }
                b'M' => { out.push(Event::ScrollDown(1)); self.reset(); }
                b'D' => { out.push(Event::ScrollUp(1)); self.reset(); }
                b'c' => {
                    // RIS — full reset; collapse to ResetAttrs +
                    // EraseDisplay(2) + CursorTo(1,1).
                    out.push(Event::ResetAttrs);
                    out.push(Event::EraseDisplay(2));
                    out.push(Event::CursorTo { row: 1, col: 1 });
                    self.reset();
                }
                _ => self.reset(),
            },

            State::CsiEntry => {
                if b == b'?' {
                    self.private = true;
                    self.state = State::CsiParam;
                    return;
                }
                self.state = State::CsiParam;
                self.handle_csi_byte(b, out);
            }

            State::CsiParam => self.handle_csi_byte(b, out),

            State::CsiIntermediate => {
                // We accept the intermediate but treat the next final
                // byte as the dispatch.
                if (0x40..=0x7E).contains(&b) {
                    self.flush_param();
                    self.dispatch_csi(b, out);
                    self.reset();
                } else if (0x20..=0x2F).contains(&b) {
                    // Multiple intermediates allowed — keep state.
                } else {
                    self.reset();
                }
            }

            State::OscString => match b {
                0x07 => { self.dispatch_osc(out); self.reset(); }
                0x9C => { self.dispatch_osc(out); self.reset(); }
                _ => {
                    if self.osc.len() < 256 {
                        self.osc.push(b as char);
                    }
                }
            },
        }
    }

    fn handle_csi_byte(&mut self, b: u8, out: &mut Vec<Event>) {
        match b {
            b'0'..=b'9' => {
                self.cur_param = self.cur_param.saturating_mul(10)
                    + (b - b'0') as u32;
                self.have_digit = true;
            }
            b';' => self.flush_param(),
            0x20..=0x2F => self.state = State::CsiIntermediate,
            0x40..=0x7E => {
                self.flush_param();
                self.dispatch_csi(b, out);
                self.reset();
            }
            _ => self.reset(),
        }
    }

    fn p(&self, idx: usize, default: u32) -> u32 {
        if idx < self.n_params { self.params[idx] } else { default }
    }

    fn dispatch_csi(&mut self, final_byte: u8, out: &mut Vec<Event>) {
        match (self.private, final_byte) {
            (false, b'A') => out.push(Event::CursorUp(self.p(0, 1) as u16)),
            (false, b'B') => out.push(Event::CursorDown(self.p(0, 1) as u16)),
            (false, b'C') => out.push(Event::CursorForward(self.p(0, 1) as u16)),
            (false, b'D') => out.push(Event::CursorBack(self.p(0, 1) as u16)),
            (false, b'H') | (false, b'f') => {
                let row = self.p(0, 1).max(1) as u16;
                let col = self.p(1, 1).max(1) as u16;
                out.push(Event::CursorTo { row, col });
            }
            (false, b's') => out.push(Event::SaveCursor),
            (false, b'u') => out.push(Event::RestoreCursor),
            (false, b'J') => out.push(Event::EraseDisplay(self.p(0, 0) as u8)),
            (false, b'K') => out.push(Event::EraseLine(self.p(0, 0) as u8)),
            (false, b'S') => out.push(Event::ScrollUp(self.p(0, 1) as u16)),
            (false, b'T') => out.push(Event::ScrollDown(self.p(0, 1) as u16)),
            (false, b'm') => self.dispatch_sgr(out),
            (true,  b'h') => self.dispatch_dec_set(true, out),
            (true,  b'l') => self.dispatch_dec_set(false, out),
            _ => out.push(Event::Unhandled),
        }
    }

    fn dispatch_sgr(&mut self, out: &mut Vec<Event>) {
        if self.n_params == 0 {
            out.push(Event::ResetAttrs);
            return;
        }
        let mut i = 0;
        while i < self.n_params {
            let p = self.params[i];
            match p {
                0 => out.push(Event::ResetAttrs),
                1 => out.push(Event::SetBold(true)),
                4 => out.push(Event::SetUnderline(true)),
                7 => out.push(Event::SetReverse(true)),
                22 => out.push(Event::SetBold(false)),
                24 => out.push(Event::SetUnderline(false)),
                27 => out.push(Event::SetReverse(false)),
                30..=37 => out.push(Event::SetForeground(ansi_to_256(p as u8 - 30))),
                39 => out.push(Event::SetDefaultFg),
                40..=47 => out.push(Event::SetBackground(ansi_to_256(p as u8 - 40))),
                49 => out.push(Event::SetDefaultBg),
                90..=97 => out.push(Event::SetForeground(ansi_to_256(p as u8 - 90 + 8))),
                100..=107 => out.push(Event::SetBackground(ansi_to_256(p as u8 - 100 + 8))),
                38 => {
                    // 38;5;n — indexed 256-color foreground.
                    if i + 2 < self.n_params && self.params[i + 1] == 5 {
                        out.push(Event::SetForeground(self.params[i + 2] as u8));
                        i += 2;
                    }
                }
                48 => {
                    if i + 2 < self.n_params && self.params[i + 1] == 5 {
                        out.push(Event::SetBackground(self.params[i + 2] as u8));
                        i += 2;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn dispatch_dec_set(&mut self, on: bool, out: &mut Vec<Event>) {
        for i in 0..self.n_params {
            match self.params[i] {
                25 => out.push(Event::ShowCursor(on)),
                1049 => out.push(Event::AlternateScreen(on)),
                _ => {}
            }
        }
    }

    fn dispatch_osc(&mut self, out: &mut Vec<Event>) {
        // OSC payload format: "<id>;<text>".
        let text = core::mem::take(&mut self.osc);
        if let Some((id_str, body)) = text.split_once(';') {
            if id_str == "0" || id_str == "2" {
                out.push(Event::SetTitle(body.to_string()));
                return;
            }
        }
        out.push(Event::Unhandled);
    }
}

/// Convert standard ANSI-30..37 / -90..97 indices to xterm 256-palette
/// indices.  ANSI 0 maps to 256-palette 0 (black), ANSI 7 to 7 (white),
/// ANSI 8 to 8 (bright black = grey), etc.  Our indices already match
/// — this is just a documenting cast.
fn ansi_to_256(idx: u8) -> Color256 { idx }
