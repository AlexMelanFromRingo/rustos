/// Simple command-line shell for RustOS
use alloc::format;
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
    aliases: Vec<(String, String)>,  // Command aliases (alias, command)
    current_dir: String,  // Current working directory
    hostname: String,  // System hostname
    env_vars: Vec<(String, String)>,  // Environment variables
}

impl Shell {
    pub fn new() -> Self {
        let mut shell = Shell {
            buffer: String::new(),
            cursor_pos: 0,
            prompt: "> ",
            history: Vec::new(),
            history_index: None,
            saved_buffer: String::new(),
            aliases: Vec::new(),
            current_dir: String::from("/"),
            hostname: String::from("rustos"),
            env_vars: Vec::new(),
        };

        // Set default environment variables
        shell.env_vars.push((String::from("HOME"), String::from("/")));
        shell.env_vars.push((String::from("PATH"), String::from("/bin")));
        shell.env_vars.push((String::from("SHELL"), String::from("/bin/sh")));
        shell.env_vars.push((String::from("USER"), String::from("root")));
        shell.env_vars.push((String::from("TERM"), String::from("vga")));

        // Load command history from file
        shell.load_history();

        shell
    }

    /// Load command history from persistent storage (FAT32 if mounted, else RAM disk)
    fn load_history(&mut self) {
        use crate::fs::vfs::VfsContext;

        const HISTORY_FILE: &str = "HISTORY.TXT";

        // Try loading history file using unified VFS
        if let Ok(data) = VfsContext::read(HISTORY_FILE) {
            // Parse history file (one command per line)
            let history_text = core::str::from_utf8(&data).unwrap_or("");

            for line in history_text.lines() {
                if !line.is_empty() && self.history.len() < MAX_HISTORY {
                    self.history.push(line.to_string());
                }
            }
        }
        // If file doesn't exist, start with empty history
    }

    /// Save command history to persistent storage (FAT32 if mounted, else RAM disk)
    fn save_history(&self) {
        use crate::fs::vfs::VfsContext;

        const HISTORY_FILE: &str = "HISTORY.TXT";

        // Create history file content (one command per line)
        let mut content = String::new();
        for cmd in &self.history {
            content.push_str(cmd);
            content.push('\n');
        }

        let data = content.as_bytes().to_vec();

        // Save using unified VFS (FAT32 if mounted, else RAMDISK)
        let _ = VfsContext::write(HISTORY_FILE, data);
    }

    pub fn print_prompt(&self) {
        // Print current directory and prompt
        print!("{}{}", self.current_dir, self.prompt);
    }

    pub fn add_char(&mut self, c: char) {
        // Ensure cursor_pos is at a valid UTF-8 boundary
        if self.cursor_pos > self.buffer.len() {
            self.cursor_pos = self.buffer.len();
        }
        while self.cursor_pos > 0 && !self.buffer.is_char_boundary(self.cursor_pos) {
            self.cursor_pos -= 1;
        }

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

    /// Get the display length of the prompt (current_dir + "> ")
    pub fn prompt_len(&self) -> usize {
        self.current_dir.len() + self.prompt.len()
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

    /// Delete from cursor to beginning of line (Ctrl+U)
    pub fn delete_to_beginning(&mut self) -> bool {
        if self.cursor_pos == 0 {
            return false;
        }
        self.buffer.drain(..self.cursor_pos);
        self.cursor_pos = 0;
        self.history_index = None;
        true
    }

    /// Delete word backward from cursor (Ctrl+W)
    pub fn delete_word_backward(&mut self) -> bool {
        if self.cursor_pos == 0 {
            return false;
        }

        let mut pos = self.cursor_pos;
        let bytes = self.buffer.as_bytes();

        // Skip trailing whitespace (working on bytes since shell input is ASCII)
        while pos > 0 && bytes[pos - 1].is_ascii_whitespace() {
            pos -= 1;
        }

        // Delete word characters
        while pos > 0 && !bytes[pos - 1].is_ascii_whitespace() {
            pos -= 1;
        }

        if pos < self.cursor_pos {
            self.buffer.drain(pos..self.cursor_pos);
            self.cursor_pos = pos;
            self.history_index = None;
            true
        } else {
            false
        }
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

        // Check if we're completing a command or a filename argument
        let parts: Vec<String> = partial.split_whitespace().map(|s| s.to_string()).collect();

        if parts.len() == 1 && !partial.contains(' ') {
            // Autocomplete command name
            let commands = [
                "alias", "cat", "cd", "clear", "cp", "date", "df", "du", "echo",
                "edit", "env", "exec", "export", "find", "grep", "head", "hello",
                "help", "history", "hostname", "kill", "less", "ls", "meminfo",
                "mkdir", "more", "mount", "mv", "printenv", "ps", "pwd", "reboot",
                "renice", "rm", "rmdir", "shutdown", "sleep", "stat", "tail",
                "time", "touch", "tree", "umount", "unalias", "unset", "uptime",
                "usermode", "version", "wc", "which", "write",
            ];

            let matches: Vec<&str> = commands
                .iter()
                .filter(|cmd| cmd.starts_with(partial))
                .copied()
                .collect();

            match matches.len() {
                0 => {
                    // Try filename completion even for first word
                    self.autocomplete_filename(parts)
                }
                1 => Some(format!("{} ", matches[0])), // Add trailing space
                _ => {
                    // Find longest common prefix
                    let prefix = longest_common_prefix(&matches);
                    if prefix.len() > partial.len() {
                        return Some(prefix);
                    }
                    println!();
                    for m in &matches {
                        print!("{}  ", m);
                    }
                    println!();
                    self.print_prompt();
                    print!("{}", self.buffer);
                    None
                }
            }
        } else {
            // Autocomplete filename argument
            self.autocomplete_filename(parts)
        }
    }

    /// Autocomplete filename arguments
    fn autocomplete_filename(&mut self, parts: Vec<String>) -> Option<String> {
        use crate::fs::vfs::VfsContext;

        // Get the partial filename (last part)
        let partial_filename = parts.last().map(|s| s.as_str()).unwrap_or("");

        // Determine the directory to list and the prefix to match
        let (dir_path, name_prefix) = if partial_filename.contains('/') {
            let last_slash = partial_filename.rfind('/').unwrap();
            let dir = if last_slash == 0 { "/" } else { &partial_filename[..last_slash] };
            (dir.to_string(), &partial_filename[last_slash + 1..])
        } else {
            (self.current_dir.clone(), partial_filename)
        };

        // Get directory listing
        let entries = match VfsContext::list_dir(&dir_path) {
            Ok(e) => e,
            Err(_) => return None,
        };

        // Find matching filenames
        let matches: Vec<String> = entries
            .iter()
            .filter(|f| {
                let basename = f.name.rsplit('/').next().unwrap_or(&f.name);
                basename.starts_with(name_prefix)
            })
            .map(|f| {
                let basename = f.name.rsplit('/').next().unwrap_or(&f.name);
                if f.is_directory {
                    format!("{}/", basename)
                } else {
                    basename.to_string()
                }
            })
            .collect();

        match matches.len() {
            0 => None,
            1 => {
                // Single match - rebuild command with completed filename
                let completed = if partial_filename.contains('/') {
                    let last_slash = partial_filename.rfind('/').unwrap();
                    format!("{}/{}", &partial_filename[..last_slash], matches[0])
                } else {
                    matches[0].clone()
                };
                let mut new_parts = parts[..parts.len()-1].to_vec();
                new_parts.push(completed);
                let result = new_parts.join(" ");
                // Add trailing space if not a directory (directory gets /)
                if !matches[0].ends_with('/') {
                    Some(format!("{} ", result))
                } else {
                    Some(result)
                }
            }
            _ => {
                // Find longest common prefix among matches
                let match_refs: Vec<&str> = matches.iter().map(|s| s.as_str()).collect();
                let prefix = longest_common_prefix(&match_refs);

                if prefix.len() > name_prefix.len() {
                    // Complete to common prefix
                    let completed = if partial_filename.contains('/') {
                        let last_slash = partial_filename.rfind('/').unwrap();
                        format!("{}/{}", &partial_filename[..last_slash], prefix)
                    } else {
                        prefix
                    };
                    let mut new_parts = parts[..parts.len()-1].to_vec();
                    new_parts.push(completed);
                    return Some(new_parts.join(" "));
                }

                // Show all options
                println!();
                for m in &matches {
                    print!("{}  ", m);
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
        self.cursor_pos = 0;  // Reset cursor position after clearing buffer
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
            // Save history to disk after adding new command
            self.save_history();
        }

        // Expand environment variables ($VAR)
        let command = self.expand_env_vars(&command);

        // Check for output redirection
        if command.contains('>') {
            self.execute_with_redirection(&command);
            return;
        }

        // Check for pipes
        if command.contains('|') {
            self.execute_pipeline(&command);
            return;
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
            "mount" => self.cmd_mount(args),
            "umount" => self.cmd_umount(args),
            "df" => self.cmd_df(),
            "pwd" => self.cmd_pwd(),
            "date" => self.cmd_date(),
            "hostname" => self.cmd_hostname(args),
            "du" => self.cmd_du(args),
            "find" => self.cmd_find(args),
            "tree" => self.cmd_tree(),
            "less" => self.cmd_less(args),
            "more" => self.cmd_less(args),  // Alias for less
            "alias" => self.cmd_alias(args),
            "unalias" => self.cmd_unalias(args),
            "which" => self.cmd_which(args),
            "sleep" => self.cmd_sleep(args),
            "cd" => self.cmd_cd(args),
            "mkdir" => self.cmd_mkdir(args),
            "rmdir" => self.cmd_rmdir(args),
            "usermode" => self.cmd_usermode(),
            "export" => self.cmd_export(args),
            "printenv" | "env" => self.cmd_printenv(args),
            "unset" => self.cmd_unset(args),
            "renice" => self.cmd_renice(args),
            "stat" => self.cmd_stat(args),
            "exec" => self.cmd_exec(args),
            _ => {
                // Check if it's an alias
                if let Some(expanded) = self.expand_alias(cmd) {
                    // Re-execute with expanded command
                    let full_command = if args.is_empty() {
                        expanded
                    } else {
                        format!("{} {}", expanded, args.join(" "))
                    };
                    // Create a temporary shell to avoid mutable borrow issues
                    let mut temp_shell = Shell {
                        buffer: full_command.clone(),
                        cursor_pos: full_command.len(),
                        prompt: self.prompt,
                        history: self.history.clone(),
                        history_index: None,
                        saved_buffer: String::new(),
                        aliases: self.aliases.clone(),
                        current_dir: self.current_dir.clone(),
                        hostname: self.hostname.clone(),
                        env_vars: self.env_vars.clone(),
                    };
                    temp_shell.execute();
                    // Update history from temp shell
                    self.history = temp_shell.history;
                } else {
                    println!("Unknown command: '{}'. Type 'help' for available commands.", cmd);
                }
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
        println!("  date      - Show current date and time (from RTC)");
        println!("  time      - Show current timer ticks");
        println!("  meminfo   - Display memory information");
        println!("  version   - Show RustOS version");
        println!("  history   - Show command history");
        println!("  hostname  - Get/set hostname (usage: hostname [name])");
        println!("  shutdown  - Shutdown the system");
        println!("  reboot    - Reboot the system");
        println!("  sleep     - Sleep for N seconds (usage: sleep <seconds>)");
        println!("  usermode  - Test user mode (Ring 3) and system calls");
        println!("  exec      - Execute ELF binary (usage: exec <filename>)");
        println!();
        println!("Process management:");
        println!("  ps        - List all processes");
        println!("  kill      - Terminate a process (usage: kill <pid>)");
        println!("  renice    - Change process priority (usage: renice <nice> <pid>)");
        println!();
        println!("File system commands:");
        println!("  pwd       - Print working directory");
        println!("  cd        - Change directory (usage: cd <dir>)");
        println!("  ls        - List files in current/specified directory");
        println!("  mkdir     - Create directory (usage: mkdir <dir>)");
        println!("  rmdir     - Remove empty directory (usage: rmdir <dir>)");
        println!("  cat       - Display file contents");
        println!("  less/more - Page through file contents");
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
        println!("  stat      - Show file status (usage: stat <file>)");
        println!("  df        - Show disk space usage");
        println!("  du        - Show file sizes (usage: du [file1 file2 ...])");
        println!("  find      - Find files by pattern (usage: find <pattern>)");
        println!("  tree      - Display files as tree");
        println!("  mount     - Mount FAT32 disk (usage: mount fat32)");
        println!("  umount    - Unmount FAT32 disk");
        println!();
        println!("Environment:");
        println!("  export    - Set environment variable (usage: export VAR=value)");
        println!("  printenv  - Print environment variables (usage: printenv [VAR])");
        println!("  env       - Same as printenv");
        println!("  unset     - Remove environment variable (usage: unset VAR)");
        println!();
        println!("Aliases:");
        println!("  alias     - Create command alias (usage: alias <name> <command>)");
        println!("  unalias   - Remove alias (usage: unalias <name>)");
        println!("  which     - Show command type/location");
        println!();
        println!("Pipes and redirection:");
        println!("  <cmd> | grep <pattern>  - Search output for pattern");
        println!("  <cmd> | wc [-l|-w|-c]   - Count lines/words/bytes");
        println!("  <cmd> | head [-n N]     - Show first N lines");
        println!("  <cmd> | tail [-n N]     - Show last N lines");
        println!("  <cmd> | sort [-r]       - Sort output");
        println!("  echo text > file        - Write to file (overwrite)");
        println!("  echo text >> file       - Append to file");
        println!();
        println!("Keyboard shortcuts:");
        println!("  LEFT/RIGHT - Move cursor left/right");
        println!("  HOME/END   - Jump to start/end of line");
        println!("  Ctrl+A     - Jump to beginning of line");
        println!("  Ctrl+E     - Jump to end of line");
        println!("  Ctrl+U     - Delete from cursor to beginning");
        println!("  Ctrl+W     - Delete word backward");
        println!("  UP/DOWN    - Navigate command history");
        println!("  TAB        - Autocomplete command or filename");
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

        let dt = crate::drivers::rtc::read_datetime();
        println!(
            " {:02}:{:02}:{:02} up {}h {}m {}s",
            dt.hour, dt.minute, dt.second,
            hours,
            minutes % 60,
            seconds % 60,
        );
    }

    fn cmd_meminfo(&self) {
        use crate::allocator::{HEAP_START, HEAP_SIZE};
        use crate::memory;

        let (total_frames, allocated_frames, free_frames) = memory::memory_stats();
        let total_phys = total_frames as u64 * 4096;
        let allocated_phys = allocated_frames as u64 * 4096;
        let free_phys = free_frames as u64 * 4096;

        println!("Memory Information:");
        println!();
        println!("  Physical Memory:");
        println!("    Total:     {} MiB ({} frames)", total_phys / (1024 * 1024), total_frames);
        println!("    Used:      {} MiB ({} frames)", allocated_phys / (1024 * 1024), allocated_frames);
        println!("    Free:      {} MiB ({} frames)", free_phys / (1024 * 1024), free_frames);
        println!("    Allocator: Bitmap frame allocator (O(1) alloc/dealloc)");
        println!();
        println!("  Kernel Heap:");
        println!("    Start:     0x{:x}", HEAP_START);
        println!("    Size:      {} MiB ({} bytes)", HEAP_SIZE / (1024 * 1024), HEAP_SIZE);
        println!("    Allocator: Fixed-size block allocator");
        println!("    Blocks:    8, 16, 32, 64, 128, 256, 512, 1024, 2048 bytes");
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
        use crate::fs::vfs::VfsContext;
        use crate::vga_buffer::{WRITER, Color};
        use x86_64::instructions::interrupts;

        match VfsContext::list_dir(&self.current_dir) {
            Ok(files) => {
                if files.is_empty() {
                    println!("Empty directory");
                    return;
                }

                for file in &files {
                    if file.is_directory {
                        // Directories in blue
                        interrupts::without_interrupts(|| {
                            WRITER.lock().set_color(Color::LightBlue, Color::Black);
                        });
                        print!("  {}/", file.name);
                    } else {
                        // Files in cyan
                        interrupts::without_interrupts(|| {
                            WRITER.lock().set_color(Color::LightCyan, Color::Black);
                        });
                        print!("  {}", file.name);
                    }

                    // Reset color and show size
                    interrupts::without_interrupts(|| {
                        WRITER.lock().reset_color();
                    });

                    if !file.is_directory {
                        print!("  {} bytes", file.size);
                    }
                    println!();
                }
            }
            Err(_) => {
                println!("ls: cannot list '{}'", self.current_dir);
            }
        }
    }

    fn cmd_cat(&self, args: &[&str]) {
        use crate::fs::vfs::VfsContext;

        if args.is_empty() {
            println!("Usage: cat <filename>");
            return;
        }

        let filename = args[0];

        match VfsContext::read(filename) {
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
        use crate::fs::vfs::{VfsContext, VfsError};

        if args.len() < 2 {
            println!("Usage: write <filename> <content>");
            return;
        }

        let filename = args[0];
        let content = args[1..].join(" ");
        let bytes = content.as_bytes().to_vec();

        match VfsContext::write(filename, bytes) {
            Ok(_) => println!("File '{}' written ({} bytes) [{}]",
                            filename, content.len(), VfsContext::filesystem_name()),
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
        use crate::fs::vfs::{VfsContext, VfsError};

        if args.is_empty() {
            println!("Usage: rm [-rf] <filename>");
            return;
        }

        // Parse flags and filename
        let mut filename = None;
        let mut _recursive = false;
        let mut _force = false;

        for arg in args {
            if arg.starts_with('-') {
                // Parse flags
                for c in arg.chars().skip(1) {
                    match c {
                        'r' => _recursive = true,
                        'f' => _force = true,
                        _ => {
                            println!("rm: invalid option -- '{}'", c);
                            return;
                        }
                    }
                }
            } else {
                // First non-flag argument is the filename
                if filename.is_none() {
                    filename = Some(*arg);
                }
            }
        }

        let filename = match filename {
            Some(f) => f,
            None => {
                println!("Usage: rm [-rf] <filename>");
                return;
            }
        };

        match VfsContext::delete(filename) {
            Ok(_) => println!("File '{}' deleted [{}]", filename, VfsContext::filesystem_name()),
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

        println!("  PID  NI  STATE       STACK");
        println!("  ---  --  -----       -----");

        for process in processes {
            println!("{:5}  {:3}  {:<11} {} bytes",
                process.pid,
                process.nice,
                process.state.as_str(),
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

    fn cmd_renice(&self, args: &[&str]) {
        use crate::process::scheduler;

        if args.len() < 2 {
            println!("Usage: renice <nice> <pid>");
            println!("  nice: -20 (highest priority) to 19 (lowest)");
            return;
        }

        let nice: i8 = match args[0].parse() {
            Ok(n) => n,
            Err(_) => {
                println!("Error: Invalid nice value '{}'", args[0]);
                return;
            }
        };

        let pid: usize = match args[1].parse() {
            Ok(p) => p,
            Err(_) => {
                println!("Error: Invalid PID '{}'", args[1]);
                return;
            }
        };

        match scheduler::set_nice(pid, nice) {
            Some(old) => println!("PID {}: nice {} -> {}", pid, old, nice.clamp(-20, 19)),
            None => println!("Error: Process {} not found", pid),
        }
    }

    fn cmd_kill(&self, args: &[&str]) {
        use crate::process::{PROCESS_MANAGER, signal};

        if args.is_empty() {
            println!("Usage: kill [-signal] <pid>");
            println!("  -l          List signal names");
            println!("  -9 <pid>    Send SIGKILL");
            println!("  -15 <pid>   Send SIGTERM (default)");
            return;
        }

        // Handle -l flag
        if args[0] == "-l" {
            for sig in 1..=20u32 {
                let name = signal::name(sig);
                if name != "UNKNOWN" {
                    println!("{:2}) {}", sig, name);
                }
            }
            return;
        }

        let (sig, pid_str) = if args[0].starts_with('-') {
            // Parse signal number from -N
            let sig_str = &args[0][1..];
            let sig = match sig_str.parse::<u32>() {
                Ok(s) => s,
                Err(_) => {
                    // Try signal name (e.g., -TERM, -KILL)
                    match sig_str.to_uppercase().as_str() {
                        "HUP" => signal::SIGHUP,
                        "INT" => signal::SIGINT,
                        "QUIT" => signal::SIGQUIT,
                        "KILL" => signal::SIGKILL,
                        "TERM" => signal::SIGTERM,
                        "STOP" => signal::SIGSTOP,
                        "CONT" => signal::SIGCONT,
                        _ => {
                            println!("Error: Unknown signal '{}'", sig_str);
                            return;
                        }
                    }
                }
            };
            if args.len() < 2 {
                println!("Usage: kill [-signal] <pid>");
                return;
            }
            (sig, args[1])
        } else {
            (signal::SIGTERM, args[0])
        };

        match pid_str.parse::<usize>() {
            Ok(pid) => {
                let mut pm = PROCESS_MANAGER.lock();
                match pm.send_signal(pid, sig) {
                    Ok(_) => println!("Sent {} to process {}", signal::name(sig), pid),
                    Err(e) => println!("Error: {}", e),
                }
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

                                // Print with colored highlighting
                                use crate::vga_buffer::{WRITER, Color};
                                use x86_64::instructions::interrupts;

                                if show_line_numbers {
                                    // Print line number in green
                                    interrupts::without_interrupts(|| {
                                        WRITER.lock().set_color(Color::LightGreen, Color::Black);
                                    });
                                    print!("{}:", line_num + 1);
                                    interrupts::without_interrupts(|| {
                                        WRITER.lock().reset_color();
                                    });
                                }

                                // Highlight matches in red
                                self.print_highlighted(line, pattern, case_insensitive);
                                println!();
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

    /// Execute command with output redirection (>, >>)
    fn execute_with_redirection(&self, command_line: &str) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        // Determine redirect type (> or >>)
        let (append, parts) = if command_line.contains(">>") {
            (true, command_line.split(">>").collect::<Vec<&str>>())
        } else {
            (false, command_line.split('>').collect::<Vec<&str>>())
        };

        if parts.len() != 2 {
            println!("Error: Invalid redirection syntax");
            return;
        }

        let command = parts[0].trim();
        let filename = parts[1].trim();

        if filename.is_empty() {
            println!("Error: No output file specified");
            return;
        }

        // Parse command and arguments
        let cmd_parts: Vec<&str> = command.split_whitespace().collect();
        if cmd_parts.is_empty() {
            println!("Error: No command specified");
            return;
        }

        let cmd = cmd_parts[0];
        let args = &cmd_parts[1..];

        // Capture output based on command
        let output = match cmd {
            "echo" => {
                // Echo command - join args with spaces
                Some(args.join(" "))
            }
            "cat" => {
                // Cat command - read file contents
                if args.is_empty() {
                    println!("Usage: cat <filename>");
                    return;
                }
                let ramdisk = RAMDISK.lock();
                match ramdisk.read(args[0]) {
                    Ok(content) => {
                        match core::str::from_utf8(&content) {
                            Ok(text) => Some(text.to_string()),
                            Err(_) => {
                                println!("Error: File '{}' is binary", args[0]);
                                return;
                            }
                        }
                    }
                    Err(_) => {
                        println!("Error: File '{}' not found", args[0]);
                        return;
                    }
                }
            }
            "ls" => {
                // List files
                let ramdisk = RAMDISK.lock();
                let files = ramdisk.list();
                let mut output = String::new();
                for file in files {
                    output.push_str(&format!("{}  {} bytes\n", file.name, file.size));
                }
                Some(output)
            }
            _ => {
                println!("Error: Command '{}' does not support output redirection", cmd);
                println!("Supported: echo, cat, ls");
                return;
            }
        };

        if let Some(content) = output {
            let mut ramdisk = RAMDISK.lock();

            // Handle append vs overwrite
            let final_content = if append {
                // Read existing content and append
                match ramdisk.read(filename) {
                    Ok(existing) => {
                        match core::str::from_utf8(&existing) {
                            Ok(existing_text) => {
                                format!("{}{}\n", existing_text, content)
                            }
                            Err(_) => {
                                println!("Error: Cannot append to binary file");
                                return;
                            }
                        }
                    }
                    Err(_) => {
                        // File doesn't exist, just write new content
                        format!("{}\n", content)
                    }
                }
            } else {
                // Overwrite
                format!("{}\n", content)
            };

            match ramdisk.write(filename, final_content.as_bytes().to_vec()) {
                Ok(_) => println!("Output written to '{}'", filename),
                Err(_) => println!("Error writing to file '{}'", filename),
            }
        }
    }

    /// Execute a pipeline of commands
    ///
    /// Captures the text output of the first command as a string,
    /// then pipes it as input to the second command (grep, wc, head, tail).
    fn execute_pipeline(&self, command_line: &str) {
        use crate::fs::vfs::VfsContext;

        let commands: Vec<&str> = command_line.split('|').map(|s| s.trim()).collect();

        if commands.len() < 2 {
            println!("Error: Invalid pipe syntax");
            return;
        }

        let first_cmd: Vec<&str> = commands[0].split_whitespace().collect();
        let second_cmd: Vec<&str> = commands[1].split_whitespace().collect();

        if first_cmd.is_empty() || second_cmd.is_empty() {
            println!("Error: Invalid pipe syntax");
            return;
        }

        // Step 1: Capture output from first command
        let output = match first_cmd[0] {
            "cat" => {
                if first_cmd.len() < 2 {
                    println!("cat: missing filename");
                    return;
                }
                match VfsContext::read(first_cmd[1]) {
                    Ok(data) => match core::str::from_utf8(&data) {
                        Ok(s) => String::from(s),
                        Err(_) => { println!("Error: binary file"); return; }
                    },
                    Err(_) => { println!("cat: {}: not found", first_cmd[1]); return; }
                }
            }
            "ls" => {
                match VfsContext::list_dir(&self.current_dir) {
                    Ok(files) => {
                        let mut out = String::new();
                        for f in &files {
                            if f.is_directory {
                                out.push_str(&f.name);
                                out.push('/');
                            } else {
                                out.push_str(&f.name);
                            }
                            out.push('\n');
                        }
                        out
                    }
                    Err(_) => { println!("ls: error"); return; }
                }
            }
            "echo" => {
                let mut out = first_cmd[1..].join(" ");
                out.push('\n');
                out
            }
            "ps" => {
                use crate::process::PROCESS_MANAGER;
                use core::fmt::Write;
                let pm = PROCESS_MANAGER.lock();
                let mut out = String::from("  PID STATE\n");
                for proc in pm.processes() {
                    let _ = writeln!(out, "{:5} {}", proc.pid, proc.state.as_str());
                }
                out
            }
            _ => {
                println!("Pipe: '{}' cannot produce output", first_cmd[0]);
                return;
            }
        };

        // Step 2: Feed output through second command
        match second_cmd[0] {
            "grep" => {
                if second_cmd.len() < 2 {
                    println!("grep: missing pattern");
                    return;
                }
                let pattern = second_cmd[1];
                let case_insensitive = second_cmd.contains(&"-i");

                for line in output.lines() {
                    let matches = if case_insensitive {
                        line.to_ascii_lowercase().contains(&pattern.to_ascii_lowercase())
                    } else {
                        line.contains(pattern)
                    };
                    if matches {
                        println!("{}", line);
                    }
                }
            }
            "wc" => {
                let lines = output.lines().count();
                let words = output.split_whitespace().count();
                let bytes = output.len();

                if second_cmd.contains(&"-l") {
                    println!("{}", lines);
                } else if second_cmd.contains(&"-w") {
                    println!("{}", words);
                } else if second_cmd.contains(&"-c") {
                    println!("{}", bytes);
                } else {
                    println!("{:8} {:8} {:8}", lines, words, bytes);
                }
            }
            "head" => {
                let n: usize = if second_cmd.len() >= 3 && second_cmd[1] == "-n" {
                    second_cmd[2].parse().unwrap_or(10)
                } else {
                    10
                };
                for line in output.lines().take(n) {
                    println!("{}", line);
                }
            }
            "tail" => {
                let n: usize = if second_cmd.len() >= 3 && second_cmd[1] == "-n" {
                    second_cmd[2].parse().unwrap_or(10)
                } else {
                    10
                };
                let lines: Vec<&str> = output.lines().collect();
                let start = lines.len().saturating_sub(n);
                for line in &lines[start..] {
                    println!("{}", line);
                }
            }
            "sort" => {
                let mut lines: Vec<&str> = output.lines().collect();
                lines.sort();
                if second_cmd.contains(&"-r") {
                    lines.reverse();
                }
                for line in lines {
                    println!("{}", line);
                }
            }
            _ => {
                println!("Pipe: '{}' cannot receive input", second_cmd[0]);
            }
        }
    }

    fn cmd_mount(&self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: mount <filesystem>");
            println!("Supported filesystems: fat32");
            return;
        }

        match args[0] {
            "fat32" => {
                use crate::fs::fat32;

                println!("Attempting to mount FAT32 filesystem...");
                match fat32::init() {
                    Ok(_) => {
                        println!("FAT32 filesystem mounted successfully!");
                        println!("Use 'ls' to list files from the disk.");
                    }
                    Err(e) => {
                        println!("Failed to mount FAT32 filesystem: {}", e);
                        println!("Make sure a FAT32-formatted disk is attached.");
                    }
                }
            }
            _ => {
                println!("Unknown filesystem: '{}'", args[0]);
                println!("Supported filesystems: fat32");
            }
        }
    }

    fn cmd_umount(&self, _args: &[&str]) {
        use crate::fs::fat32::FAT32;

        let mut fat32 = FAT32.lock();
        if fat32.is_some() {
            *fat32 = None;
            println!("FAT32 filesystem unmounted");
        } else {
            println!("No FAT32 filesystem is mounted");
        }
    }

    /// Print text with highlighted pattern
    fn print_highlighted(&self, text: &str, pattern: &str, case_insensitive: bool) {
        use crate::vga_buffer::{WRITER, Color};
        use x86_64::instructions::interrupts;

        let search_text = if case_insensitive {
            text.to_lowercase()
        } else {
            text.to_string()
        };

        let search_pattern = if case_insensitive {
            pattern.to_lowercase()
        } else {
            pattern.to_string()
        };

        let mut last_end = 0;

        while let Some(pos) = search_text[last_end..].find(&search_pattern) {
            let start = last_end + pos;
            let end = start + pattern.len();

            // Print text before match (normal color)
            if start > last_end {
                print!("{}", &text[last_end..start]);
            }

            // Print match (red color)
            interrupts::without_interrupts(|| {
                WRITER.lock().set_color(Color::LightRed, Color::Black);
            });
            print!("{}", &text[start..end]);
            interrupts::without_interrupts(|| {
                WRITER.lock().reset_color();
            });

            last_end = end;
        }

        // Print remaining text
        if last_end < text.len() {
            print!("{}", &text[last_end..]);
        }
    }

    fn cmd_df(&self) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::fat32::FAT32;
        use crate::fs::vfs::FileSystem;

        println!("Filesystem            Size      Used     Avail  Use%  Mounted on");
        println!("---------------------------------------------------------------");

        // Show RAM disk usage
        let ramdisk = RAMDISK.lock();
        let total = ramdisk.total_space();
        let used = ramdisk.used_space();
        let avail = ramdisk.free_space();
        let use_percent = if total > 0 {
            (used * 100) / total
        } else {
            0
        };
        drop(ramdisk);

        println!(
            "{:<20}  {:>6}K  {:>6}K  {:>6}K   {:>3}%  /",
            "ramdisk",
            total / 1024,
            used / 1024,
            avail / 1024,
            use_percent
        );

        // Show FAT32 usage if mounted
        let fat32 = FAT32.lock();
        if let Some(ref fs) = *fat32 {
            let total = fs.total_space();
            let used = fs.used_space();
            let avail = fs.free_space();
            let use_percent = if total > 0 {
                (used * 100) / total
            } else {
                0
            };

            println!(
                "{:<20}  {:>6}K  {:>6}K  {:>6}K   {:>3}%  /mnt/fat32",
                "fat32",
                total / 1024,
                used / 1024,
                avail / 1024,
                use_percent
            );
        }
        drop(fat32);
    }

    /// Helper function to expand aliases
    fn expand_alias(&self, cmd: &str) -> Option<String> {
        for (alias, command) in &self.aliases {
            if alias == cmd {
                return Some(command.clone());
            }
        }
        None
    }

    /// Expand $VAR references in a command string
    fn expand_env_vars(&self, input: &str) -> String {
        let mut result = String::with_capacity(input.len());
        let bytes = input.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' && i + 1 < bytes.len() {
                i += 1;
                // Collect variable name (alphanumeric + underscore)
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                if start < i {
                    let var_name = &input[start..i];
                    if let Some((_, value)) = self.env_vars.iter().find(|(k, _)| k == var_name) {
                        result.push_str(value);
                    }
                    // If variable not found, expand to empty string (like sh)
                } else {
                    result.push('$');
                }
            } else {
                result.push(input[i..].chars().next().unwrap());
                i += input[i..].chars().next().unwrap().len_utf8();
            }
        }
        result
    }

    fn cmd_pwd(&self) {
        println!("{}", self.current_dir);
    }

    fn cmd_date(&self) {
        let dt = crate::drivers::rtc::read_datetime();
        println!("{} {} {:2} {:02}:{:02}:{:02} UTC {}",
            dt.day_name(),
            dt.month_name(),
            dt.day,
            dt.hour,
            dt.minute,
            dt.second,
            dt.year,
        );
    }

    fn cmd_hostname(&mut self, args: &[&str]) {
        if args.is_empty() {
            println!("{}", self.hostname);
        } else {
            self.hostname = args[0].to_string();
            println!("Hostname set to: {}", self.hostname);
        }
    }

    fn cmd_du(&self, args: &[&str]) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        let ramdisk = RAMDISK.lock();
        let files = ramdisk.list();

        if args.is_empty() {
            // Show all files
            let mut total = 0;
            for file in &files {
                let kb = (file.size + 1023) / 1024;  // Round up to KB
                println!("{:>6}K  {}", kb, file.name);
                total += file.size;
            }
            let total_kb = (total + 1023) / 1024;
            println!("{:>6}K  total", total_kb);
        } else {
            // Show specific files
            for filename in args {
                if let Some(file) = files.iter().find(|f| f.name == *filename) {
                    let kb = (file.size + 1023) / 1024;
                    println!("{:>6}K  {}", kb, file.name);
                } else {
                    println!("du: cannot access '{}': No such file", filename);
                }
            }
        }
    }

    fn cmd_find(&self, args: &[&str]) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        if args.is_empty() {
            println!("Usage: find <pattern>");
            println!("  Searches for files matching the pattern");
            return;
        }

        let pattern = args[0];
        let ramdisk = RAMDISK.lock();
        let files = ramdisk.list();

        let mut found = 0;
        for file in &files {
            if file.name.contains(pattern) {
                println!("{}", file.name);
                found += 1;
            }
        }

        if found == 0 {
            println!("No files matching '{}' found", pattern);
        } else {
            println!("\n{} file(s) found", found);
        }
    }

    fn cmd_tree(&self) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        let ramdisk = RAMDISK.lock();
        let files = ramdisk.list();

        println!("/");
        for (i, file) in files.iter().enumerate() {
            let is_last = i == files.len() - 1;
            let prefix = if is_last { "└──" } else { "├──" };
            println!("{} {}", prefix, file.name);
        }

        println!("\n{} files", files.len());
    }

    fn cmd_less(&self, args: &[&str]) {
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;

        if args.is_empty() {
            println!("Usage: less <filename>");
            return;
        }

        let filename = args[0];
        let ramdisk = RAMDISK.lock();

        match ramdisk.read(filename) {
            Ok(content) => {
                match core::str::from_utf8(&content) {
                    Ok(text) => {
                        // Simple pager - just display all content for now
                        // In a real implementation, this would paginate
                        println!("{}", text);
                        println!("\n(END)");
                    }
                    Err(_) => println!("Error: File is not valid UTF-8 text"),
                }
            }
            Err(_) => println!("Error: File '{}' not found", filename),
        }
    }

    fn cmd_alias(&mut self, args: &[&str]) {
        if args.is_empty() {
            // List all aliases
            if self.aliases.is_empty() {
                println!("No aliases defined");
            } else {
                println!("Aliases:");
                for (alias, command) in &self.aliases {
                    println!("  {} = {}", alias, command);
                }
            }
        } else if args.len() < 2 {
            println!("Usage: alias <name> <command>");
            println!("       alias           (list all aliases)");
        } else {
            let alias = args[0].to_string();
            let command = args[1..].join(" ");

            // Remove existing alias if present
            self.aliases.retain(|(a, _)| a != &alias);

            // Add new alias
            self.aliases.push((alias.clone(), command.clone()));
            println!("Alias '{}' set to '{}'", alias, command);
        }
    }

    fn cmd_unalias(&mut self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: unalias <name>");
            return;
        }

        let alias = args[0];
        let initial_len = self.aliases.len();
        self.aliases.retain(|(a, _)| a != alias);

        if self.aliases.len() < initial_len {
            println!("Alias '{}' removed", alias);
        } else {
            println!("Alias '{}' not found", alias);
        }
    }

    fn cmd_which(&self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: which <command>");
            return;
        }

        let cmd = args[0];

        // Check if it's an alias first
        if let Some(expanded) = self.expand_alias(cmd) {
            println!("{}: aliased to '{}'", cmd, expanded);
            return;
        }

        // List of built-in commands
        let builtins = [
            "alias", "cat", "cd", "clear", "cp", "date", "df", "du", "echo",
            "edit", "find", "grep", "head", "hello", "help", "history",
            "hostname", "kill", "less", "ls", "meminfo", "mkdir", "more",
            "mount", "mv", "ps", "pwd", "reboot", "rm", "rmdir", "shutdown",
            "sleep", "tail", "time", "touch", "tree", "umount", "unalias",
            "uptime", "version", "wc", "which", "write"
        ];

        if builtins.contains(&cmd) {
            println!("{}: shell built-in command", cmd);
        } else {
            println!("{}: command not found", cmd);
        }
    }

    fn cmd_sleep(&self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: sleep <seconds>");
            return;
        }

        match args[0].parse::<u64>() {
            Ok(seconds) => {
                println!("Sleeping for {} second(s)...", seconds);

                // Use timer ticks with hlt instruction to save CPU
                use crate::task::timer::current_ticks;
                let target = current_ticks() + (seconds * 18); // ~18.2 Hz PIT

                while current_ticks() < target {
                    x86_64::instructions::hlt(); // Sleep until next interrupt
                }

                println!("Done");
            }
            Err(_) => println!("Error: Invalid number '{}'", args[0]),
        }
    }

    fn cmd_cd(&mut self, args: &[&str]) {
        use crate::fs::vfs::VfsContext;

        if args.is_empty() {
            // cd without arguments goes to root
            self.current_dir = String::from("/");
            return;
        }

        let target = args[0];

        // Handle special cases
        if target == "/" {
            self.current_dir = String::from("/");
            return;
        }

        if target == "." {
            return;
        }

        if target == ".." {
            if self.current_dir == "/" {
                return;
            }
            if let Some(pos) = self.current_dir.rfind('/') {
                if pos == 0 {
                    self.current_dir = String::from("/");
                } else {
                    self.current_dir.truncate(pos);
                }
            }
            return;
        }

        // Construct new path
        let new_path = if target.starts_with('/') {
            target.to_string()
        } else if self.current_dir == "/" {
            format!("/{}", target)
        } else {
            format!("{}/{}", self.current_dir, target)
        };

        // Check if directory exists
        if VfsContext::is_directory(&new_path) {
            self.current_dir = new_path;
        } else if VfsContext::exists(&new_path) {
            println!("cd: {}: Not a directory", target);
        } else {
            println!("cd: {}: No such file or directory", target);
        }
    }

    fn cmd_mkdir(&mut self, args: &[&str]) {
        use crate::fs::vfs::VfsContext;

        if args.is_empty() {
            println!("Usage: mkdir <directory>");
            return;
        }

        let dirname = args[0];

        // Construct full path
        let path = if dirname.starts_with('/') {
            dirname.to_string()
        } else if self.current_dir == "/" {
            format!("/{}", dirname)
        } else {
            format!("{}/{}", self.current_dir, dirname)
        };

        match VfsContext::mkdir(&path) {
            Ok(_) => println!("Directory '{}' created", dirname),
            Err(e) => println!("mkdir: {}: {:?}", dirname, e),
        }
    }

    fn cmd_rmdir(&mut self, args: &[&str]) {
        use crate::fs::vfs::VfsContext;

        if args.is_empty() {
            println!("Usage: rmdir <directory>");
            return;
        }

        let dirname = args[0];

        // Construct full path
        let path = if dirname.starts_with('/') {
            dirname.to_string()
        } else if self.current_dir == "/" {
            format!("/{}", dirname)
        } else {
            format!("{}/{}", self.current_dir, dirname)
        };

        match VfsContext::rmdir(&path) {
            Ok(_) => println!("Directory '{}' removed", dirname),
            Err(e) => println!("rmdir: {}: {:?}", dirname, e),
        }
    }

    /// Test user mode (Ring 3) and system calls
    #[allow(unreachable_code)]
    fn cmd_usermode(&mut self) {
        println!("Testing Ring 3 user mode with real privilege separation!");
        println!();

        // Get function pointer and size
        let fn_ptr = crate::userspace::user_mode_demo as *const () as usize;
        let fn_size = crate::userspace::get_demo_size();

        println!("Kernel -> User Mode Transition:");
        println!("  Function: 0x{:016X} (kernel space)", fn_ptr);
        println!("  Size: {} bytes", fn_size);
        println!("  Copying to identity-mapped user space...");
        println!("  Switching to Ring 3...");
        println!();

        // Jump to user mode - this will execute the demo and exit via syscall
        unsafe {
            crate::userspace::jump_to_usermode(fn_ptr, fn_size);
        }

        // Should never reach here - user_mode_demo calls sys_exit
        println!("ERROR: Returned from user mode unexpectedly!");
    }

    /// Set or display environment variables
    fn cmd_export(&mut self, args: &[&str]) {
        if args.is_empty() {
            // Display all environment variables
            for (key, value) in &self.env_vars {
                println!("{}={}", key, value);
            }
            return;
        }

        for arg in args {
            if let Some(eq_pos) = arg.find('=') {
                let key = &arg[..eq_pos];
                let value = &arg[eq_pos + 1..];
                if key.is_empty() {
                    println!("export: invalid variable name");
                    continue;
                }
                // Update existing or insert new
                if let Some(entry) = self.env_vars.iter_mut().find(|(k, _)| k == key) {
                    entry.1 = String::from(value);
                } else {
                    self.env_vars.push((String::from(key), String::from(value)));
                }
            } else {
                // Just a name without value — check if it exists
                if self.env_vars.iter().any(|(k, _)| k == *arg) {
                    // Already exported, nothing to do
                } else {
                    // Set empty value
                    self.env_vars.push((String::from(*arg), String::new()));
                }
            }
        }
    }

    /// Print environment variables
    fn cmd_printenv(&self, args: &[&str]) {
        if args.is_empty() {
            for (key, value) in &self.env_vars {
                println!("{}={}", key, value);
            }
        } else {
            for name in args {
                if let Some((_, value)) = self.env_vars.iter().find(|(k, _)| k == *name) {
                    println!("{}", value);
                }
            }
        }
    }

    /// Remove environment variables
    fn cmd_unset(&mut self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: unset <variable> [variable ...]");
            return;
        }
        for name in args {
            self.env_vars.retain(|(k, _)| k != *name);
        }
    }

    /// Display file status information
    fn cmd_stat(&self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: stat <file> [file ...]");
            return;
        }

        use crate::fs::vfs::VfsContext;
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::fat32::FAT32;
        use crate::fs::vfs::FileSystem;

        for filename in args {
            let path = if filename.starts_with('/') {
                String::from(*filename)
            } else if self.current_dir == "/" {
                format!("/{}", filename)
            } else {
                format!("{}/{}", self.current_dir, filename)
            };

            let is_dir = VfsContext::is_directory(&path);
            let file_size = if is_dir {
                0usize
            } else {
                let data = {
                    if let Some(ref fs) = *FAT32.lock() {
                        fs.read(&path).ok()
                    } else {
                        RAMDISK.lock().read(&path).ok()
                    }
                };
                match data {
                    Some(d) => d.len(),
                    None => {
                        if !is_dir {
                            println!("stat: cannot stat '{}': No such file or directory", filename);
                            continue;
                        }
                        0
                    }
                }
            };

            let file_type = if is_dir { "directory" } else { "regular file" };
            let mode_str = if is_dir { "drwxr-xr-x" } else { "-rw-r--r--" };
            let mode_oct = if is_dir { "0755" } else { "0644" };
            let blocks = (file_size + 511) / 512;

            println!("  File: {}", filename);
            println!("  Size: {:<15} Blocks: {:<10} {}", file_size, blocks, file_type);
            println!("Access: ({}/{})  Uid: (    0/    root)   Gid: (    0/    root)", mode_oct, mode_str);

            // Show timestamps from RTC
            let dt = crate::drivers::rtc::read_datetime();
            let mut buf = [0u8; 32];
            let len = dt.format(&mut buf);
            let time_str = core::str::from_utf8(&buf[..len]).unwrap_or("unknown");
            println!("Modify: {}", time_str);
        }
    }

    /// Execute ELF binary in user mode
    fn cmd_exec(&mut self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: exec <filename>");
            println!("Execute an ELF binary in user mode (Ring 3)");
            return;
        }

        let filename = args[0];
        println!("Loading ELF binary: {}", filename);

        // Load and execute ELF
        // Execution magically continues here after user program calls exit()
        // and restore_kernel_context_and_return() restores our stack
        crate::elf::load_and_exec(filename);

        // We return here after user program exits!
        println!("\nProgram exited, returned to shell.");
    }
}

/// Find the longest common prefix among a list of strings
fn longest_common_prefix(strings: &[&str]) -> String {
    if strings.is_empty() {
        return String::new();
    }
    if strings.len() == 1 {
        return strings[0].to_string();
    }

    let first = strings[0].as_bytes();
    let mut prefix_len = first.len();

    for s in &strings[1..] {
        let bytes = s.as_bytes();
        prefix_len = prefix_len.min(bytes.len());
        for i in 0..prefix_len {
            if first[i] != bytes[i] {
                prefix_len = i;
                break;
            }
        }
    }

    strings[0][..prefix_len].to_string()
}
