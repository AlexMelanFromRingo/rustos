/// Simple command-line shell for RustOS
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use crate::{print, println};

const MAX_HISTORY: usize = 50;

pub struct Shell {
    buffer: String,
    prompt: &'static str,
    history: Vec<String>,
    history_index: Option<usize>,
    saved_buffer: String,
}

impl Shell {
    pub fn new() -> Self {
        Shell {
            buffer: String::new(),
            prompt: "> ",
            history: Vec::new(),
            history_index: None,
            saved_buffer: String::new(),
        }
    }

    pub fn print_prompt(&self) {
        print!("{}", self.prompt);
    }

    pub fn add_char(&mut self, c: char) {
        self.buffer.push(c);
        self.history_index = None;
    }

    pub fn backspace(&mut self) -> bool {
        if self.buffer.pop().is_some() {
            self.history_index = None;
            true
        } else {
            false
        }
    }

    pub fn get_buffer(&self) -> &str {
        &self.buffer
    }

    pub fn set_buffer(&mut self, s: String) {
        self.buffer = s;
    }

    pub fn clear_buffer(&mut self) {
        self.buffer.clear();
    }

    /// Navigate up in command history
    pub fn history_up(&mut self) -> Option<String> {
        if self.history.is_empty() {
            return None;
        }

        let new_index = match self.history_index {
            None => {
                // Save current buffer before entering history
                self.saved_buffer = self.buffer.clone();
                Some(self.history.len() - 1)
            }
            Some(idx) if idx > 0 => Some(idx - 1),
            Some(_) => return None, // Already at oldest
        };

        self.history_index = new_index;
        new_index.map(|idx| self.history[idx].clone())
    }

    /// Navigate down in command history
    pub fn history_down(&mut self) -> Option<String> {
        match self.history_index {
            None => None,
            Some(idx) if idx < self.history.len() - 1 => {
                self.history_index = Some(idx + 1);
                Some(self.history[idx + 1].clone())
            }
            Some(_) => {
                // Restore saved buffer
                self.history_index = None;
                Some(self.saved_buffer.clone())
            }
        }
    }

    pub fn execute(&mut self) {
        let command = self.buffer.trim().to_string();
        self.buffer.clear();
        self.history_index = None;

        if command.is_empty() {
            return;
        }

        // Add to history
        if self.history.last() != Some(&command) {
            self.history.push(command.clone());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
        }

        // Parse command and arguments
        let parts: Vec<&str> = command.split_whitespace().collect();
        let cmd = parts[0];
        let args = &parts[1..];

        match cmd {
            "help" => self.cmd_help(),
            "clear" => self.cmd_clear(),
            "echo" => self.cmd_echo(args),
            "shutdown" => self.cmd_shutdown(),
            "reboot" => self.cmd_reboot(),
            "hello" => self.cmd_hello(),
            "uptime" => self.cmd_uptime(),
            "meminfo" => self.cmd_meminfo(),
            "version" => self.cmd_version(),
            "time" => self.cmd_time(),
            "history" => self.cmd_history(),
            _ => {
                println!("Unknown command: '{}'. Type 'help' for available commands.", cmd);
            }
        }
    }

    fn cmd_help(&self) {
        println!("Available commands:");
        println!("  help      - Show this help message");
        println!("  clear     - Clear the screen");
        println!("  echo      - Echo the arguments");
        println!("  hello     - Print a greeting");
        println!("  uptime    - Show system uptime");
        println!("  time      - Show current timer ticks");
        println!("  meminfo   - Display memory information");
        println!("  version   - Show RustOS version");
        println!("  history   - Show command history");
        println!("  shutdown  - Shutdown the system");
        println!("  reboot    - Reboot the system");
        println!();
        println!("Use UP/DOWN arrows to navigate command history");
    }

    fn cmd_clear(&self) {
        use crate::vga_buffer::WRITER;
        use x86_64::instructions::interrupts;

        interrupts::without_interrupts(|| {
            WRITER.lock().clear_screen();
        });
    }

    fn cmd_echo(&self, args: &[&str]) {
        if args.is_empty() {
            println!();
        } else {
            println!("{}", args.join(" "));
        }
    }

    fn cmd_shutdown(&self) {
        println!("Shutting down...");
        crate::power::shutdown();
    }

    fn cmd_reboot(&self) {
        println!("Rebooting...");
        crate::power::reboot();
    }

    fn cmd_hello(&self) {
        println!("Hello from RustOS!");
        println!("Welcome to a minimal operating system written in Rust.");
    }

    fn cmd_uptime(&self) {
        use crate::task::timer::current_ticks;

        let ticks = current_ticks();
        // Assuming ~18.2 Hz timer (PIT default frequency)
        let seconds = ticks / 18;
        let minutes = seconds / 60;
        let hours = minutes / 60;

        println!(
            "Uptime: {}h {}m {}s ({} ticks)",
            hours,
            minutes % 60,
            seconds % 60,
            ticks
        );
    }

    fn cmd_meminfo(&self) {
        use crate::allocator::{HEAP_START, HEAP_SIZE};

        println!("Memory Information:");
        println!("  Heap start: 0x{:x}", HEAP_START);
        println!("  Heap size:  {} KB ({} bytes)", HEAP_SIZE / 1024, HEAP_SIZE);
        println!("  Allocator:  Fixed-size block allocator");
        println!("  Block sizes: 8, 16, 32, 64, 128, 256, 512, 1024, 2048 bytes");
    }

    fn cmd_version(&self) {
        println!("RustOS v0.1.0");
        println!("A minimal operating system written in Rust");
        println!();
        println!("Features:");
        println!("  - VGA text mode output");
        println!("  - Hardware interrupts (keyboard, timer)");
        println!("  - Paging and memory management");
        println!("  - Heap allocation (fixed-size block allocator)");
        println!("  - Async/await cooperative multitasking");
        println!("  - Power management (shutdown/reboot)");
        println!("  - Interactive shell with command history");
    }

    fn cmd_time(&self) {
        use crate::task::timer::current_ticks;
        println!("Timer ticks: {}", current_ticks());
    }

    fn cmd_history(&self) {
        if self.history.is_empty() {
            println!("No command history");
            return;
        }

        println!("Command history:");
        for (i, cmd) in self.history.iter().enumerate() {
            println!("  {} {}", i + 1, cmd);
        }
    }
}
