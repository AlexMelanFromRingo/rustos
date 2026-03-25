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
}

impl Writer {
    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            0x08 => self.backspace(), // Backspace
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
            match byte {
                // printable ASCII byte, newline, or backspace
                0x20..=0x7e | b'\n' | 0x08 => self.write_byte(byte),
                // Extended ASCII (box drawing, symbols, etc.)
                0x80..=0xff => self.write_byte(byte),
                // not part of printable range
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
