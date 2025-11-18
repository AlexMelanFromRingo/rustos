/// Simple command-line shell for RustOS
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use crate::{print, println};

const MAX_HISTORY: usize = 50;

pub struct Shell {
    buffer: String,
    cursor_pos: usize,  // Position in buffer (0 = start, buffer.len() = end)
    prompt: &'static str,
    history: Vec<String>,
    history_index: Option<usize>,
    saved_buffer: String,
}

impl Shell {
    pub fn new() -> Self {
        Shell {
            buffer: String::new(),
            cursor_pos: 0,
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
        self.buffer.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
        self.history_index = None;
    }

    pub fn backspace(&mut self) -> bool {
        if self.cursor_pos == 0 {
            return false;
        }
        // Find character boundary before cursor
        let mut pos = self.cursor_pos - 1;
        while !self.buffer.is_char_boundary(pos) && pos > 0 {
            pos -= 1;
        }
        self.buffer.remove(pos);
        self.cursor_pos = pos;
        self.history_index = None;
        true
    }

    pub fn delete_char(&mut self) -> bool {
        if self.cursor_pos >= self.buffer.len() {
            return false;
        }
        self.buffer.remove(self.cursor_pos);
        self.history_index = None;
        true
    }

    pub fn move_cursor_left(&mut self) -> bool {
        if self.cursor_pos == 0 {
            return false;
        }
        // Move to previous character boundary
        let mut pos = self.cursor_pos - 1;
        while !self.buffer.is_char_boundary(pos) && pos > 0 {
            pos -= 1;
        }
        self.cursor_pos = pos;
        true
    }

    pub fn move_cursor_right(&mut self) -> bool {
        if self.cursor_pos >= self.buffer.len() {
            return false;
        }
        // Move to next character boundary
        let mut pos = self.cursor_pos + 1;
        while !self.buffer.is_char_boundary(pos) && pos < self.buffer.len() {
            pos += 1;
        }
        self.cursor_pos = pos;
        true
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor_pos = self.buffer.len();
    }

    pub fn get_cursor_pos(&self) -> usize {
        self.cursor_pos
    }

    pub fn get_buffer(&self) -> &str {
        &self.buffer
    }

    pub fn set_buffer(&mut self, s: String) {
        self.cursor_pos = s.len();
        self.buffer = s;
    }

    pub fn clear_buffer(&mut self) {
        self.buffer.clear();
        self.cursor_pos = 0;
    }

    pub fn buffer_len(&self) -> usize {
        self.buffer.len()
    }

    /// Redraw the current line (for cursor movement)
    /// Returns (chars_to_backspace, new_text)
    pub fn redraw_line(&self) -> (usize, &str) {
        (self.buffer.len(), &self.buffer)
    }

    /// Try to autocomplete the current buffer
    /// Returns Some(completed) if single match, None otherwise
    /// Prints all matches if multiple matches found
    pub fn autocomplete(&mut self) -> Option<String> {
        let partial = self.buffer.trim();
        if partial.is_empty() {
            return None;
        }

        // List of available commands
        let commands = [
            "cat", "clear", "cp", "echo", "edit", "grep", "head",
            "hello", "help", "history", "kill", "ls", "meminfo",
            "mv", "ps", "reboot", "rm", "shutdown", "tail", "time",
            "touch", "uptime", "version", "wc", "write",
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
                // Multiple matches - show all options
                println!();
                for m in &matches {
                    print!("{} ", m);
                }
                println!();
                self.print_prompt();
                print!("{}", self.buffer);
                None
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
            "ps" => self.cmd_ps(),
            "kill" => self.cmd_kill(args),
            "grep" => self.cmd_grep(args),
            "ls" => self.cmd_ls(),
            "cat" => self.cmd_cat(args),
            "write" => self.cmd_write(args),
            "rm" => self.cmd_rm(args),
            "touch" => self.cmd_touch(args),
            "cp" => self.cmd_cp(args),
            "mv" => self.cmd_mv(args),
            "head" => self.cmd_head(args),
            "tail" => self.cmd_tail(args),
            "wc" => self.cmd_wc(args),
            "edit" => self.cmd_edit(args),
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
        println!("Process management:");
        println!("  ps        - List all processes");
        println!("  kill      - Terminate a process (usage: kill <pid>)");
        println!();
        println!("File system commands:");
        println!("  ls        - List files in RAM disk");
        println!("  cat       - Display file contents");
        println!("  write     - Create/write file (usage: write filename content)");
        println!("  rm        - Remove file");
        println!("  touch     - Create empty file");
        println!("  cp        - Copy file (usage: cp source dest)");
        println!("  mv        - Move/rename file (usage: mv source dest)");
        println!("  head      - Show first N lines (usage: head [-n N] filename)");
        println!("  tail      - Show last N lines (usage: tail [-n N] filename)");
        println!("  wc        - Count lines/words/bytes (usage: wc filename)");
        println!("  grep      - Search for pattern in file (usage: grep [-i] [-n] pattern file)");
        println!("  edit      - Simple text editor (usage: edit filename)");
        println!();
        println!("Keyboard shortcuts:");
        println!("  LEFT/RIGHT - Move cursor left/right");
        println!("  HOME/END   - Jump to start/end of line");
        println!("  UP/DOWN    - Navigate command history");
        println!("  TAB        - Autocomplete command");
        println!("  Backspace  - Delete previous character");
        println!("  Delete     - Delete character at cursor");
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

    fn cmd_touch(&self, args: &[&str]) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        if args.is_empty() {
            println!("Usage: touch <filename>");
            return;
        }

        let filename = args[0];
        let mut ramdisk = RAMDISK.lock();

        // Create empty file
        match ramdisk.write(filename, Vec::new()) {
            Ok(_) => println!("File '{}' created", filename),
            Err(e) => println!("Error: {:?}", e),
        }
    }

    fn cmd_cp(&self, args: &[&str]) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        if args.len() < 2 {
            println!("Usage: cp <source> <destination>");
            return;
        }

        let source = args[0];
        let dest = args[1];
        let mut ramdisk = RAMDISK.lock();

        // Read source file
        match ramdisk.read(source) {
            Ok(content) => {
                // Write to destination
                match ramdisk.write(dest, content) {
                    Ok(_) => println!("Copied '{}' to '{}'", source, dest),
                    Err(e) => println!("Error writing destination: {:?}", e),
                }
            }
            Err(_) => println!("Error: Source file '{}' not found", source),
        }
    }

    fn cmd_mv(&self, args: &[&str]) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        if args.len() < 2 {
            println!("Usage: mv <source> <destination>");
            return;
        }

        let source = args[0];
        let dest = args[1];
        let mut ramdisk = RAMDISK.lock();

        // Read source file
        match ramdisk.read(source) {
            Ok(content) => {
                // Write to destination
                match ramdisk.write(dest, content) {
                    Ok(_) => {
                        // Delete source
                        match ramdisk.delete(source) {
                            Ok(_) => println!("Moved '{}' to '{}'", source, dest),
                            Err(e) => println!("Error deleting source: {:?}", e),
                        }
                    }
                    Err(e) => println!("Error writing destination: {:?}", e),
                }
            }
            Err(_) => println!("Error: Source file '{}' not found", source),
        }
    }

    fn cmd_head(&self, args: &[&str]) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        let num_lines;
        let filename;

        // Parse arguments
        if args.is_empty() {
            println!("Usage: head [-n N] <filename>");
            return;
        }

        if args[0] == "-n" {
            if args.len() < 3 {
                println!("Usage: head -n <number> <filename>");
                return;
            }
            num_lines = args[1].parse().unwrap_or(10);
            filename = args[2];
        } else {
            num_lines = 10;
            filename = args[0];
        }

        let ramdisk = RAMDISK.lock();
        match ramdisk.read(filename) {
            Ok(content) => {
                match core::str::from_utf8(&content) {
                    Ok(text) => {
                        let lines: Vec<&str> = text.lines().collect();
                        for line in lines.iter().take(num_lines) {
                            println!("{}", line);
                        }
                    }
                    Err(_) => println!("Error: File is not valid UTF-8 text"),
                }
            }
            Err(_) => println!("Error: File '{}' not found", filename),
        }
    }

    fn cmd_tail(&self, args: &[&str]) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        let num_lines;
        let filename;

        // Parse arguments
        if args.is_empty() {
            println!("Usage: tail [-n N] <filename>");
            return;
        }

        if args[0] == "-n" {
            if args.len() < 3 {
                println!("Usage: tail -n <number> <filename>");
                return;
            }
            num_lines = args[1].parse().unwrap_or(10);
            filename = args[2];
        } else {
            num_lines = 10;
            filename = args[0];
        }

        let ramdisk = RAMDISK.lock();
        match ramdisk.read(filename) {
            Ok(content) => {
                match core::str::from_utf8(&content) {
                    Ok(text) => {
                        let lines: Vec<&str> = text.lines().collect();
                        let start = if lines.len() > num_lines {
                            lines.len() - num_lines
                        } else {
                            0
                        };
                        for line in &lines[start..] {
                            println!("{}", line);
                        }
                    }
                    Err(_) => println!("Error: File is not valid UTF-8 text"),
                }
            }
            Err(_) => println!("Error: File '{}' not found", filename),
        }
    }

    fn cmd_wc(&self, args: &[&str]) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        if args.is_empty() {
            println!("Usage: wc <filename>");
            return;
        }

        let filename = args[0];
        let ramdisk = RAMDISK.lock();

        match ramdisk.read(filename) {
            Ok(content) => {
                let bytes = content.len();
                match core::str::from_utf8(&content) {
                    Ok(text) => {
                        let lines = text.lines().count();
                        let words = text.split_whitespace().count();
                        println!("  {} {} {} {}", lines, words, bytes, filename);
                    }
                    Err(_) => {
                        println!("  0 0 {} {} (binary)", bytes, filename);
                    }
                }
            }
            Err(_) => println!("Error: File '{}' not found", filename),
        }
    }

    fn cmd_edit(&self, args: &[&str]) {
        use crate::editor::Editor;

        if args.is_empty() {
            println!("Usage: edit <filename>");
            return;
        }

        let filename = args[0];
        let mut editor = Editor::new(filename);
        editor.run();
    }

    fn cmd_ps(&self) {
        use crate::process::PROCESS_MANAGER;

        let pm = PROCESS_MANAGER.lock();
        let processes = pm.all_processes();

        println!("PID    STATE       STACK_SIZE");
        println!("---    -----       ----------");

        for process in processes {
            let state_str = match process.state {
                crate::process::ProcessState::Ready => "Ready    ",
                crate::process::ProcessState::Running => "Running  ",
                crate::process::ProcessState::Blocked => "Blocked  ",
                crate::process::ProcessState::Terminated => "Terminated",
            };

            println!("{:<6} {:<11} {} bytes",
                process.pid,
                state_str,
                process.stack.len()
            );
        }

        println!();
        println!("Total: {} processes", processes.len());

        if let Some(current_pid) = pm.current_pid {
            println!("Current: PID {}", current_pid);
        } else {
            println!("Current: None");
        }
    }

    fn cmd_kill(&self, args: &[&str]) {
        use crate::process::scheduler;

        if args.is_empty() {
            println!("Usage: kill <pid>");
            return;
        }

        let pid_str = args[0];
        match pid_str.parse::<usize>() {
            Ok(pid) => {
                scheduler::terminate(pid);
                println!("Process {} terminated", pid);
            }
            Err(_) => {
                println!("Error: Invalid PID '{}'", pid_str);
            }
        }
    }

    fn cmd_grep(&self, args: &[&str]) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        if args.is_empty() {
            println!("Usage: grep [-i] [-n] pattern filename");
            println!("  -i  Case insensitive search");
            println!("  -n  Show line numbers");
            return;
        }

        let mut case_insensitive = false;
        let mut show_line_numbers = false;
        let mut arg_idx = 0;

        // Parse flags
        while arg_idx < args.len() && args[arg_idx].starts_with('-') {
            match args[arg_idx] {
                "-i" => case_insensitive = true,
                "-n" => show_line_numbers = true,
                "-in" | "-ni" => {
                    case_insensitive = true;
                    show_line_numbers = true;
                }
                _ => {
                    println!("Error: Unknown option '{}'", args[arg_idx]);
                    return;
                }
            }
            arg_idx += 1;
        }

        // Need at least pattern and filename
        if arg_idx + 2 > args.len() {
            println!("Usage: grep [-i] [-n] pattern filename");
            return;
        }

        let pattern = args[arg_idx];
        let filename = args[arg_idx + 1];

        // Read file
        let ramdisk = RAMDISK.lock();
        match ramdisk.read(filename) {
            Ok(content) => {
                drop(ramdisk);

                // Convert to UTF-8
                match core::str::from_utf8(&content) {
                    Ok(text) => {
                        let mut match_count = 0;

                        // Prepare pattern for comparison
                        let search_pattern = if case_insensitive {
                            pattern.to_lowercase()
                        } else {
                            pattern.to_string()
                        };

                        // Search through lines
                        for (line_num, line) in text.lines().enumerate() {
                            let search_line = if case_insensitive {
                                line.to_lowercase()
                            } else {
                                line.to_string()
                            };

                            if search_line.contains(&search_pattern) {
                                match_count += 1;
                                if show_line_numbers {
                                    println!("{}:{}", line_num + 1, line);
                                } else {
                                    println!("{}", line);
                                }
                            }
                        }

                        if match_count == 0 {
                            println!("No matches found");
                        } else {
                            println!();
                            println!("{} match(es) found", match_count);
                        }
                    }
                    Err(_) => {
                        println!("Error: File '{}' is binary, cannot search", filename);
                    }
                }
            }
            Err(_) => {
                println!("Error: File '{}' not found", filename);
            }
        }
    }
}
