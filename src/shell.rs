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

    pub fn backspace(&mut self) {
        self.buffer.pop();
        self.history_index = None;
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

    pub fn buffer_len(&self) -> usize {
        self.buffer.len()
    }

    /// Try to autocomplete the current buffer
    pub fn autocomplete(&mut self) -> Option<String> {
        let partial = self.buffer.trim();
        if partial.is_empty() {
            return None;
        }

        // List of available commands
        let commands = [
            "help", "clear", "echo", "hello", "uptime", "time",
            "meminfo", "version", "history", "shutdown", "reboot",
            "ls", "cat", "write", "rm",
        ];

        // Find matching commands
        let matches: Vec<&str> = commands
            .iter()
            .filter(|cmd| cmd.starts_with(partial))
            .copied()
            .collect();

        match matches.len() {
            0 => None,  // No matches
            1 => {
                // Single match - complete it
                Some(matches[0].to_string())
            }
            _ => {
                // Multiple matches - return first for now
                // TODO: cycle through matches on repeated Tab
                Some(matches[0].to_string())
            }
        }
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
            "ls" => self.cmd_ls(),
            "cat" => self.cmd_cat(args),
            "write" => self.cmd_write(args),
            "rm" => self.cmd_rm(args),
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
        println!("File system commands:");
        println!("  ls        - List files in RAM disk");
        println!("  cat       - Display file contents");
        println!("  write     - Create/write file (usage: write filename content)");
        println!("  rm        - Remove file");
        println!();
        println!("Keyboard shortcuts:");
        println!("  UP/DOWN   - Navigate command history");
        println!("  TAB       - Autocomplete command");
        println!("  Backspace - Delete previous character");
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
        println!("  - RAM disk filesystem");
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

    fn cmd_ls(&self) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        let ramdisk = RAMDISK.lock();
        let files = ramdisk.list();

        if files.is_empty() {
            println!("No files");
            return;
        }

        println!("Files ({} files, {} bytes used):", files.len(), ramdisk.used_space());
        for file in files {
            println!("  {} - {} bytes", file.name, file.size);
        }
    }

    fn cmd_cat(&self, args: &[&str]) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        if args.is_empty() {
            println!("Usage: cat <filename>");
            return;
        }

        let filename = args[0];
        let ramdisk = RAMDISK.lock();

        match ramdisk.read(filename) {
            Ok(content) => {
                // Try to display as UTF-8 text
                match core::str::from_utf8(&content) {
                    Ok(text) => println!("{}", text),
                    Err(_) => {
                        // Display as hex if not valid UTF-8
                        println!("Binary file ({} bytes):", content.len());
                        for (i, byte) in content.iter().enumerate() {
                            if i % 16 == 0 {
                                print!("\n{:04x}: ", i);
                            }
                            print!("{:02x} ", byte);
                        }
                        println!();
                    }
                }
            }
            Err(_) => println!("File not found: {}", filename),
        }
    }

    fn cmd_write(&self, args: &[&str]) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::{FileSystem, VfsError};

        if args.len() < 2 {
            println!("Usage: write <filename> <content>");
            return;
        }

        let filename = args[0];
        let content = args[1..].join(" ");
        let bytes = content.as_bytes().to_vec();

        let mut ramdisk = RAMDISK.lock();
        match ramdisk.write(filename, bytes) {
            Ok(_) => println!("File '{}' written ({} bytes)", filename, content.len()),
            Err(e) => {
                let msg = match e {
                    VfsError::InvalidName => "Invalid filename",
                    VfsError::FileTooLarge => "File too large",
                    VfsError::TooManyFiles => "Too many files",
                    VfsError::NoSpace => "No space left",
                    _ => "Error writing file",
                };
                println!("Error: {}", msg);
            }
        }
    }

    fn cmd_rm(&self, args: &[&str]) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::{FileSystem, VfsError};

        if args.is_empty() {
            println!("Usage: rm <filename>");
            return;
        }

        let filename = args[0];
        let mut ramdisk = RAMDISK.lock();

        match ramdisk.delete(filename) {
            Ok(_) => println!("File '{}' deleted", filename),
            Err(e) => {
                let msg = match e {
                    VfsError::FileNotFound => "File not found",
                    _ => "Error deleting file",
                };
                println!("Error: {}", msg);
            }
        }
    }
}
