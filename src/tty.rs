/// TTY (Teletype) subsystem
///
/// Provides terminal abstraction with line discipline for input processing.
/// Supports:
/// - Line-buffered (canonical) and raw modes
/// - Echo control
/// - Signal generation (Ctrl+C → SIGINT, Ctrl+Z → SIGTSTP)
/// - Terminal size tracking (rows, columns)
/// - Input/output buffering

use alloc::collections::VecDeque;
use alloc::string::String;
use spin::Mutex;

/// Terminal settings (simplified termios)
#[derive(Debug, Clone, Copy)]
pub struct Termios {
    /// Canonical mode: line-buffered input with editing
    pub canonical: bool,
    /// Echo input characters
    pub echo: bool,
    /// Generate signals (Ctrl+C, Ctrl+Z)
    pub signals: bool,
    /// Process CR/LF translation
    pub cr_translate: bool,
}

impl Default for Termios {
    fn default() -> Self {
        Termios {
            canonical: true,
            echo: true,
            signals: true,
            cr_translate: true,
        }
    }
}

/// Terminal window size
#[derive(Debug, Clone, Copy)]
pub struct WinSize {
    pub rows: u16,
    pub cols: u16,
    pub xpixel: u16,
    pub ypixel: u16,
}

impl Default for WinSize {
    fn default() -> Self {
        WinSize {
            rows: 25,
            cols: 80,
            xpixel: 0,
            ypixel: 0,
        }
    }
}

/// TTY device
pub struct Tty {
    /// Terminal name
    pub name: &'static str,
    /// Terminal settings
    pub termios: Termios,
    /// Window size
    pub winsize: WinSize,
    /// Input buffer (bytes waiting to be read)
    input_buf: VecDeque<u8>,
    /// Line editing buffer (canonical mode)
    line_buf: String,
    /// Foreground process group ID
    pub fg_pgid: usize,
    /// Session ID
    pub session_id: usize,
    /// Whether the terminal is connected
    pub active: bool,
}

impl Tty {
    pub const fn new(name: &'static str) -> Self {
        Tty {
            name,
            termios: Termios {
                canonical: true,
                echo: true,
                signals: true,
                cr_translate: true,
            },
            winsize: WinSize {
                rows: 25,
                cols: 80,
                xpixel: 0,
                ypixel: 0,
            },
            input_buf: VecDeque::new(),
            line_buf: String::new(),
            fg_pgid: 0,
            session_id: 0,
            active: true,
        }
    }

    /// Process an input byte from the keyboard/serial driver.
    /// Returns a signal to send if applicable (e.g., SIGINT from Ctrl+C).
    pub fn input_byte(&mut self, byte: u8) -> Option<TtySignal> {
        if !self.active {
            return None;
        }

        // Signal generation
        if self.termios.signals {
            match byte {
                0x03 => return Some(TtySignal::Interrupt),  // Ctrl+C → SIGINT
                0x1A => return Some(TtySignal::Suspend),    // Ctrl+Z → SIGTSTP
                0x1C => return Some(TtySignal::Quit),       // Ctrl+\ → SIGQUIT
                0x04 => return Some(TtySignal::Eof),        // Ctrl+D → EOF
                _ => {}
            }
        }

        if self.termios.canonical {
            // Canonical mode: buffer lines
            match byte {
                b'\n' | b'\r' => {
                    self.line_buf.push('\n');
                    // Move completed line to input buffer
                    for b in self.line_buf.bytes() {
                        self.input_buf.push_back(b);
                    }
                    self.line_buf.clear();
                    if self.termios.echo {
                        crate::print!("\n");
                    }
                }
                0x7F | 0x08 => {
                    // Backspace
                    if !self.line_buf.is_empty() {
                        self.line_buf.pop();
                        if self.termios.echo {
                            crate::print!("\x08 \x08");
                        }
                    }
                }
                _ => {
                    self.line_buf.push(byte as char);
                    if self.termios.echo {
                        crate::print!("{}", byte as char);
                    }
                }
            }
        } else {
            // Raw mode: pass through immediately
            self.input_buf.push_back(byte);
            if self.termios.echo {
                crate::print!("{}", byte as char);
            }
        }

        None
    }

    /// Read bytes from the TTY input buffer.
    /// Returns the number of bytes actually read.
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        let mut count = 0;
        for slot in buf.iter_mut() {
            if let Some(byte) = self.input_buf.pop_front() {
                *slot = byte;
                count += 1;
            } else {
                break;
            }
        }
        count
    }

    /// Write bytes to the TTY output (goes to VGA + serial).
    pub fn write(&self, data: &[u8]) {
        if let Ok(s) = core::str::from_utf8(data) {
            crate::print!("{}", s);
        } else {
            for &byte in data {
                crate::print!("{}", byte as char);
            }
        }
    }

    /// Check how many bytes are available to read
    pub fn bytes_available(&self) -> usize {
        self.input_buf.len()
    }

    /// Set raw mode (disable canonical processing)
    pub fn set_raw(&mut self) {
        self.termios.canonical = false;
        self.termios.echo = false;
    }

    /// Set cooked/canonical mode
    pub fn set_cooked(&mut self) {
        self.termios.canonical = true;
        self.termios.echo = true;
    }

    /// Flush input buffer
    pub fn flush_input(&mut self) {
        self.input_buf.clear();
        self.line_buf.clear();
    }
}

/// Signals that the TTY can generate
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtySignal {
    Interrupt,  // SIGINT (Ctrl+C)
    Suspend,    // SIGTSTP (Ctrl+Z)
    Quit,       // SIGQUIT (Ctrl+\)
    Eof,        // EOF (Ctrl+D)
}

/// Global TTY instances
/// tty0: the main console (VGA + serial)
pub static TTY0: Mutex<Tty> = Mutex::new(Tty::new("tty0"));

/// Get the controlling TTY for the current process
pub fn current_tty() -> &'static Mutex<Tty> {
    &TTY0
}

/// Add a new PTY device node to /dev
pub fn add_dev_entries() -> &'static [&'static str] {
    &["tty0", "ptmx"]
}
