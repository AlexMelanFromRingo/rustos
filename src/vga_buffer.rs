use core::fmt;
use lazy_static::lazy_static;
use spin::Mutex;
use volatile::Volatile;
use x86_64::instructions::port::Port;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

impl Color {
    /// Convert a u8 to Color safely, defaulting to White for invalid values
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Color::Black,
            1 => Color::Blue,
            2 => Color::Green,
            3 => Color::Cyan,
            4 => Color::Red,
            5 => Color::Magenta,
            6 => Color::Brown,
            7 => Color::LightGray,
            8 => Color::DarkGray,
            9 => Color::LightBlue,
            10 => Color::LightGreen,
            11 => Color::LightCyan,
            12 => Color::LightRed,
            13 => Color::Pink,
            14 => Color::Yellow,
            15 => Color::White,
            _ => Color::White,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColorCode(u8);

impl ColorCode {
    fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColorCode,
}

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;

#[repr(transparent)]
struct Buffer {
    chars: [[Volatile<ScreenChar>; BUFFER_WIDTH]; BUFFER_HEIGHT],
}

pub struct Writer {
    column_position: usize,
    color_code: ColorCode,
    buffer: &'static mut Buffer,
    /// ANSI escape parser state: when we hit ESC (0x1B) we transition
    /// from None → SeenEsc → InCsi(buf) and accumulate parameters into
    /// `csi_buf` until a final byte (0x40..=0x7E) terminates the
    /// sequence.  See ECMA-48 §5.4 and `console_codes(4)`.
    ansi_state: AnsiState,
    csi_buf: [u8; 16],
    csi_len: usize,
    /// Saved fg/bg so a single SGR sequence can change one without
    /// destroying the other.
    fg: Color,
    bg: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnsiState {
    None,
    SeenEsc,
    InCsi,
}

impl Writer {
    pub fn write_byte(&mut self, byte: u8) {
        // ----- ANSI / VT100 escape state machine -----------------------
        // We accept the SGR + cursor + erase subset of ECMA-48 so callers
        // can emit standard `\x1b[...m` color codes and they show up in
        // VGA hardware colors instead of as literal "[31m" gibberish.
        match self.ansi_state {
            AnsiState::SeenEsc => {
                if byte == b'[' {
                    self.ansi_state = AnsiState::InCsi;
                    self.csi_len = 0;
                    return;
                } else {
                    // Unknown ESC sequence — drop the escape, fall back
                    // to printing the next byte literally.
                    self.ansi_state = AnsiState::None;
                    // intentional fall-through to default handling below
                }
            }
            AnsiState::InCsi => {
                // Final bytes are 0x40..=0x7E (`@`..`~`).  Parameters
                // are digits and `;`.  Anything else aborts the escape.
                let final_byte = (0x40..=0x7E).contains(&byte);
                if !final_byte {
                    if self.csi_len < self.csi_buf.len() {
                        self.csi_buf[self.csi_len] = byte;
                        self.csi_len += 1;
                    }
                    return;
                }
                self.ansi_state = AnsiState::None;
                self.handle_csi(byte);
                return;
            }
            AnsiState::None => {}
        }
        if byte == 0x1B {
            self.ansi_state = AnsiState::SeenEsc;
            return;
        }

        match byte {
            b'\n' => self.new_line(),
            0x08 => self.backspace(), // Backspace
            b'\r' => { self.column_position = 0; self.update_cursor(); }
            byte => {
                if self.column_position >= BUFFER_WIDTH {
                    self.new_line();
                }

                let row = BUFFER_HEIGHT - 1;
                let col = self.column_position;

                let color_code = self.color_code;
                self.buffer.chars[row][col].write(ScreenChar {
                    ascii_character: byte,
                    color_code,
                });
                self.column_position += 1;
                self.update_cursor();
            }
        }
    }

    /// Handle a complete CSI sequence (ESC [ params final_byte).
    /// Implements:
    ///   * `<n>;<m>m`   — SGR (Select Graphic Rendition): colors + reset
    ///   * `2J` / `0J`  — Erase in Display (clear screen)
    ///   * `K`          — Erase in Line (rest of current row)
    ///   * `<n>;<m>H`   — Cursor Position (1-based)
    ///   * `<n>A/B/C/D` — Cursor up/down/forward/back
    fn handle_csi(&mut self, final_byte: u8) {
        // Parse `;`-separated decimal parameters out of csi_buf.
        let mut params = [0u32; 8];
        let mut np = 0usize;
        let mut cur = 0u32;
        let mut have_digit = false;
        for &b in &self.csi_buf[..self.csi_len] {
            if b.is_ascii_digit() {
                cur = cur * 10 + (b - b'0') as u32;
                have_digit = true;
            } else if b == b';' {
                if np < 8 { params[np] = cur; np += 1; }
                cur = 0;
                have_digit = false;
            }
            // Other intermediate bytes (e.g. '?') are ignored — we
            // don't yet implement private DEC modes.
        }
        if have_digit && np < 8 { params[np] = cur; np += 1; }

        match final_byte {
            b'm' => {
                // SGR.  No params == reset.
                if np == 0 { self.sgr_reset(); return; }
                for i in 0..np { self.apply_sgr(params[i]); }
            }
            b'J' => {
                // Erase in Display.  We honour 2 (entire screen) and
                // 0/missing (cursor → end).  1 (start → cursor) is
                // approximated as 2.
                let mode = if np == 0 { 0 } else { params[0] };
                self.erase_in_display(mode);
            }
            b'K' => {
                // Erase in Line: 0 = cursor → end of line.
                let row = BUFFER_HEIGHT - 1;
                let blank = ScreenChar { ascii_character: b' ',
                                         color_code: self.color_code };
                for col in self.column_position..BUFFER_WIDTH {
                    self.buffer.chars[row][col].write(blank);
                }
            }
            b'H' | b'f' => {
                // CUP — only the column matters in our 1-row writer.
                let col = if np >= 2 { params[1] } else { 1 };
                let target = (col.saturating_sub(1) as usize).min(BUFFER_WIDTH - 1);
                self.column_position = target;
                self.update_cursor();
            }
            b'C' => {
                let n = if np == 0 { 1 } else { params[0] as usize };
                self.column_position = (self.column_position + n).min(BUFFER_WIDTH - 1);
                self.update_cursor();
            }
            b'D' => {
                let n = if np == 0 { 1 } else { params[0] as usize };
                self.column_position = self.column_position.saturating_sub(n);
                self.update_cursor();
            }
            // 'A' (cursor up) / 'B' (cursor down) are no-ops: the only
            // writable row is the bottom one.
            _ => {}
        }
    }

    fn sgr_reset(&mut self) {
        self.fg = Color::Yellow;
        self.bg = Color::Black;
        self.color_code = ColorCode::new(self.fg, self.bg);
    }

    /// Apply one SGR parameter.  Intentionally minimal — we cover the
    /// 30-37 / 40-47 normal-intensity range plus 90-97 / 100-107
    /// bright variants and the 0 reset.  Bold (1) maps to bright fg
    /// because our hardware colors already encode brightness.
    fn apply_sgr(&mut self, p: u32) {
        match p {
            0 => self.sgr_reset(),
            1 => {
                // Bold ⇒ bright fg.  Convert the existing fg to its
                // bright variant (8..15).
                let v = self.fg as u8;
                if v < 8 { self.fg = Color::from_u8(v + 8); }
                self.color_code = ColorCode::new(self.fg, self.bg);
            }
            22 => {
                let v = self.fg as u8;
                if v >= 8 { self.fg = Color::from_u8(v - 8); }
                self.color_code = ColorCode::new(self.fg, self.bg);
            }
            30..=37 => {
                self.fg = ansi_fg(p - 30);
                self.color_code = ColorCode::new(self.fg, self.bg);
            }
            39 => {
                self.fg = Color::Yellow; // default
                self.color_code = ColorCode::new(self.fg, self.bg);
            }
            40..=47 => {
                self.bg = ansi_fg(p - 40);
                self.color_code = ColorCode::new(self.fg, self.bg);
            }
            49 => {
                self.bg = Color::Black;
                self.color_code = ColorCode::new(self.fg, self.bg);
            }
            90..=97 => {
                self.fg = ansi_bright(p - 90);
                self.color_code = ColorCode::new(self.fg, self.bg);
            }
            100..=107 => {
                self.bg = ansi_bright(p - 100);
                self.color_code = ColorCode::new(self.fg, self.bg);
            }
            _ => {}
        }
    }

    fn erase_in_display(&mut self, mode: u32) {
        let blank = ScreenChar { ascii_character: b' ', color_code: self.color_code };
        match mode {
            2 | 1 => {
                for r in 0..BUFFER_HEIGHT {
                    for c in 0..BUFFER_WIDTH {
                        self.buffer.chars[r][c].write(blank);
                    }
                }
                self.column_position = 0;
                self.update_cursor();
            }
            _ => {
                // 0 — current line cursor → end + every line below
                let row = BUFFER_HEIGHT - 1;
                for col in self.column_position..BUFFER_WIDTH {
                    self.buffer.chars[row][col].write(blank);
                }
            }
        }
    }

    pub fn backspace(&mut self) {
        if self.column_position > 0 {
            self.column_position -= 1;
            let row = BUFFER_HEIGHT - 1;
            let col = self.column_position;

            let blank = ScreenChar {
                ascii_character: b' ',
                color_code: self.color_code,
            };
            self.buffer.chars[row][col].write(blank);
            self.update_cursor();
        }
    }

    /// Move cursor left without erasing character
    pub fn move_cursor_left(&mut self) {
        if self.column_position > 0 {
            self.column_position -= 1;
            self.update_cursor();
        }
    }

    /// Move cursor right without writing character
    pub fn move_cursor_right(&mut self) {
        if self.column_position < BUFFER_WIDTH - 1 {
            self.column_position += 1;
            self.update_cursor();
        }
    }

    /// Set cursor to specific column position
    pub fn set_cursor_column(&mut self, col: usize) {
        if col < BUFFER_WIDTH {
            self.column_position = col;
            self.update_cursor();
        }
    }

    /// Get current cursor column position
    pub fn get_cursor_column(&self) -> usize {
        self.column_position
    }

    fn update_cursor(&self) {
        let pos = (BUFFER_HEIGHT - 1) * BUFFER_WIDTH + self.column_position;

        unsafe {
            let mut port_cmd = Port::<u8>::new(0x3D4);
            let mut port_data = Port::<u8>::new(0x3D5);

            // Set cursor location high byte
            port_cmd.write(0x0E);
            port_data.write((pos >> 8) as u8);

            // Set cursor location low byte
            port_cmd.write(0x0F);
            port_data.write(pos as u8);
        }
    }

    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            // ESC (0x1B) and CR (0x0D) need to reach the state machine
            // directly; they used to be replaced with the placeholder
            // glyph, which broke ANSI colors and bare \r prompts.
            match byte {
                0x20..=0x7e | b'\n' | 0x08 | 0x1B | b'\r' => self.write_byte(byte),
                0x80..=0xff => self.write_byte(byte),
                _ => self.write_byte(0xfe),
            }
        }
    }

    fn new_line(&mut self) {
        for row in 1..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                let character = self.buffer.chars[row][col].read();
                self.buffer.chars[row - 1][col].write(character);
            }
        }
        self.clear_row(BUFFER_HEIGHT - 1);
        self.column_position = 0;
        self.update_cursor();
    }

    fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        for col in 0..BUFFER_WIDTH {
            self.buffer.chars[row][col].write(blank);
        }
    }

    pub fn clear_screen(&mut self) {
        for row in 0..BUFFER_HEIGHT {
            self.clear_row(row);
        }
        self.column_position = 0;
        self.update_cursor();
    }

    /// Set foreground color for text output
    pub fn set_color(&mut self, foreground: Color, background: Color) {
        self.color_code = ColorCode::new(foreground, background);
    }

    /// Get current foreground and background colors
    pub fn get_color(&self) -> (Color, Color) {
        let code = self.color_code.0;
        let foreground = code & 0x0F;
        let background = (code >> 4) & 0x0F;
        (
            Color::from_u8(foreground),
            Color::from_u8(background),
        )
    }

    /// Reset to default colors (Yellow on Black)
    pub fn reset_color(&mut self) {
        self.color_code = ColorCode::new(Color::Yellow, Color::Black);
        self.fg = Color::Yellow;
        self.bg = Color::Black;
    }
}

/// Map ANSI 30-37 / 40-47 indices to our hardware Color enum.
fn ansi_fg(idx: u32) -> Color {
    match idx {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Brown,        // ANSI yellow on dim is actually brown on VGA
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::LightGray,
        _ => Color::White,
    }
}

/// Map ANSI 90-97 / 100-107 to bright variants.
fn ansi_bright(idx: u32) -> Color {
    match idx {
        0 => Color::DarkGray,
        1 => Color::LightRed,
        2 => Color::LightGreen,
        3 => Color::Yellow,
        4 => Color::LightBlue,
        5 => Color::Pink,
        6 => Color::LightCyan,
        7 => Color::White,
        _ => Color::White,
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

lazy_static! {
    pub static ref WRITER: Mutex<Writer> = {
        let writer = Writer {
            column_position: 0,
            color_code: ColorCode::new(Color::Yellow, Color::Black),
            buffer: unsafe { &mut *(0xb8000 as *mut Buffer) },
            ansi_state: AnsiState::None,
            csi_buf: [0u8; 16],
            csi_len: 0,
            fg: Color::Yellow,
            bg: Color::Black,
        };
        writer.update_cursor();
        Mutex::new(writer)
    };
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::vga_buffer::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// Global output capture buffer for shell pipelines.
/// When active, _print() writes to this buffer instead of VGA/serial.
static CAPTURE_BUFFER: spin::Mutex<Option<alloc::string::String>> = spin::Mutex::new(None);

/// Start capturing print output into a string buffer.
pub fn start_capture() {
    *CAPTURE_BUFFER.lock() = Some(alloc::string::String::new());
}

/// Stop capturing and return the captured output.
pub fn stop_capture() -> alloc::string::String {
    CAPTURE_BUFFER.lock().take().unwrap_or_default()
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        let mut capture = CAPTURE_BUFFER.lock();
        if let Some(ref mut buf) = *capture {
            let _ = buf.write_fmt(args);
        } else {
            drop(capture);
            WRITER.lock().write_fmt(args).unwrap();
            // Mirror output to serial port for testing/debugging
            crate::serial::_print(args);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_println_simple() {
        println!("test_println_simple output");
    }

    #[test_case]
    fn test_println_many() {
        for _ in 0..200 {
            println!("test_println_many output");
        }
    }

    #[test_case]
    fn test_println_output() {
        use core::fmt::Write;
        use x86_64::instructions::interrupts;

        let s = "Some test string that fits on a single line";
        interrupts::without_interrupts(|| {
            let mut writer = WRITER.lock();
            writeln!(writer, "\n{}", s).expect("writeln failed");
            for (i, c) in s.chars().enumerate() {
                let screen_char = writer.buffer.chars[BUFFER_HEIGHT - 2][i].read();
                assert_eq!(char::from(screen_char.ascii_character), c);
            }
        });
    }
}
