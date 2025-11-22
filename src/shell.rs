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
        };

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
        print!("{}", self.prompt);
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

        // Skip trailing whitespace
        while pos > 0 && self.buffer.chars().nth(pos - 1).map_or(false, |c| c.is_whitespace()) {
            pos -= 1;
        }

        // Delete word characters
        while pos > 0 && self.buffer.chars().nth(pos - 1).map_or(false, |c| !c.is_whitespace()) {
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

        if parts.len() == 1 {
            // Autocomplete command name
            let commands = [
                "alias", "cat", "cd", "clear", "cp", "date", "df", "du", "echo",
                "edit", "find", "grep", "head", "hello", "help", "history",
                "hostname", "kill", "less", "ls", "meminfo", "mkdir", "more",
                "mount", "mv", "ps", "pwd", "reboot", "rm", "rmdir", "shutdown",
                "sleep", "tail", "time", "touch", "tree", "umount", "unalias",
                "uptime", "version", "wc", "which", "write",
            ];

            let matches: Vec<&str> = commands
                .iter()
                .filter(|cmd| cmd.starts_with(partial))
                .copied()
                .collect();

            match matches.len() {
                0 => None,
                1 => Some(matches[0].to_string()),
                _ => {
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

        // Get list of files from active filesystem
        let files = VfsContext::list();

        // Find matching filenames
        let matches: Vec<String> = files
            .iter()
            .filter(|f| f.name.starts_with(partial_filename))
            .map(|f| f.name.clone())
            .collect();

        match matches.len() {
            0 => None,
            1 => {
                // Single match - rebuild command with completed filename
                let mut new_parts = parts[..parts.len()-1].to_vec();
                new_parts.push(matches[0].clone());
                Some(new_parts.join(" "))
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
        println!("  date      - Show system uptime as date");
        println!("  time      - Show current timer ticks");
        println!("  meminfo   - Display memory information");
        println!("  version   - Show RustOS version");
        println!("  history   - Show command history");
        println!("  hostname  - Get/set hostname (usage: hostname [name])");
        println!("  shutdown  - Shutdown the system");
        println!("  reboot    - Reboot the system");
        println!("  sleep     - Sleep for N seconds (usage: sleep <seconds>)");
        println!("  usermode  - Test user mode (Ring 3) and system calls");
        println!();
        println!("Process management:");
        println!("  ps        - List all processes");
        println!("  kill      - Terminate a process (usage: kill <pid>)");
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
        println!("  df        - Show disk space usage");
        println!("  du        - Show file sizes (usage: du [file1 file2 ...])");
        println!("  find      - Find files by pattern (usage: find <pattern>)");
        println!("  tree      - Display files as tree");
        println!("  mount     - Mount FAT32 disk (usage: mount fat32)");
        println!("  umount    - Unmount FAT32 disk");
        println!();
        println!("Aliases:");
        println!("  alias     - Create command alias (usage: alias <name> <command>)");
        println!("  unalias   - Remove alias (usage: unalias <name>)");
        println!("  which     - Show command type/location");
        println!();
        println!("Pipes and redirection:");
        println!("  cat <file> | grep <pattern>  - Search in file");
        println!("  cat <file> | wc              - Count lines/words/bytes");
        println!("  ls | grep <pattern>          - Filter file list");
        println!("  echo text > file             - Write to file (overwrite)");
        println!("  echo text >> file            - Append to file");
        println!("  cat file > newfile           - Copy file contents");
        println!("  ls > filelist.txt            - Save file list");
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
        use crate::fs::fat32::FAT32;
        use crate::fs::ramdisk::RAMDISK;
        use crate::fs::vfs::FileSystem;
        use crate::vga_buffer::{WRITER, Color};
        use x86_64::instructions::interrupts;

        // Try FAT32 first if mounted
        let fat32 = FAT32.lock();
        if let Some(ref fs) = *fat32 {
            // List current directory on FAT32
            match fs.list_directory(&self.current_dir) {
                Ok(files) => {
                    drop(fat32);

                    if files.is_empty() {
                        println!("Empty directory");
                        return;
                    }

                    println!("{} ({} items):", self.current_dir, files.len());
                    for file in files {
                        // Print filename in cyan
                        interrupts::without_interrupts(|| {
                            WRITER.lock().set_color(Color::LightCyan, Color::Black);
                        });
                        print!("  {}", file.name);

                        // Print " - " in default color
                        interrupts::without_interrupts(|| {
                            WRITER.lock().reset_color();
                        });
                        print!(" - ");

                        if file.size == 0 && !file.name.contains('.') {
                            // Likely a directory
                            interrupts::without_interrupts(|| {
                                WRITER.lock().set_color(Color::Blue, Color::Black);
                            });
                            print!("<DIR>");
                        } else {
                            // Print size in light green
                            interrupts::without_interrupts(|| {
                                WRITER.lock().set_color(Color::LightGreen, Color::Black);
                            });
                            print!("{} bytes", file.size);
                        }

                        // Reset color
                        interrupts::without_interrupts(|| {
                            WRITER.lock().reset_color();
                        });
                        println!();
                    }
                    return;
                }
                Err(e) => {
                    drop(fat32);
                    println!("ls: {}: {}", self.current_dir, e);
                    println!("Falling back to RAM disk...");
                }
            }
        } else {
            drop(fat32);
        }

        // Fallback to RAM disk
        let ramdisk = RAMDISK.lock();
        let files = ramdisk.list();

        if files.is_empty() {
            println!("No files");
            return;
        }

        println!("RAM disk ({} files, {} bytes used):", files.len(), ramdisk.used_space());
        for file in files {
            // Print filename in cyan
            interrupts::without_interrupts(|| {
                WRITER.lock().set_color(Color::LightCyan, Color::Black);
            });
            print!("  {}", file.name);

            // Print " - " in default color
            interrupts::without_interrupts(|| {
                WRITER.lock().reset_color();
            });
            print!(" - ");

            // Print size in light green
            interrupts::without_interrupts(|| {
                WRITER.lock().set_color(Color::LightGreen, Color::Black);
            });
            print!("{}", file.size);

            // Reset color and print " bytes"
            interrupts::without_interrupts(|| {
                WRITER.lock().reset_color();
            });
            println!(" bytes");
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
            println!("Usage: rm <filename>");
            return;
        }

        let filename = args[0];

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

    /// Execute a pipeline of commands (simplified pipe implementation)
    fn execute_pipeline(&self, command_line: &str) {
        let commands: Vec<&str> = command_line.split('|').map(|s| s.trim()).collect();

        if commands.len() < 2 {
            println!("Error: Invalid pipe syntax");
            return;
        }

        // For now, support specific pipe combinations
        // cat file | grep pattern
        // cat file | wc
        let first_cmd = commands[0].split_whitespace().collect::<Vec<&str>>();
        let second_cmd = commands[1].split_whitespace().collect::<Vec<&str>>();

        if first_cmd.is_empty() || second_cmd.is_empty() {
            println!("Error: Invalid pipe syntax");
            return;
        }

        // Handle: cat file | grep pattern
        if first_cmd[0] == "cat" && second_cmd[0] == "grep" {
            if first_cmd.len() < 2 {
                println!("Usage: cat filename | grep pattern");
                return;
            }

            let filename = first_cmd[1];

            // Get file content
            use crate::fs::ramdisk::RAMDISK;
            use crate::fs::vfs::FileSystem;

            let ramdisk = RAMDISK.lock();
            match ramdisk.read(filename) {
                Ok(content) => {
                    drop(ramdisk);
                    match core::str::from_utf8(&content) {
                        Ok(text) => {
                            // Now grep through the text
                            if second_cmd.len() < 2 {
                                println!("Usage: cat filename | grep pattern");
                                return;
                            }

                            let pattern = second_cmd[1];
                            let mut match_count = 0;

                            for line in text.lines() {
                                if line.contains(pattern) {
                                    match_count += 1;
                                    println!("{}", line);
                                }
                            }

                            if match_count == 0 {
                                println!("No matches found");
                            }
                        }
                        Err(_) => println!("Error: File '{}' is binary", filename),
                    }
                }
                Err(_) => println!("Error: File '{}' not found", filename),
            }
        }
        // Handle: cat file | wc
        else if first_cmd[0] == "cat" && second_cmd[0] == "wc" {
            if first_cmd.len() < 2 {
                println!("Usage: cat filename | wc");
                return;
            }

            let filename = first_cmd[1];

            use crate::fs::ramdisk::RAMDISK;
            use crate::fs::vfs::FileSystem;

            let ramdisk = RAMDISK.lock();
            match ramdisk.read(filename) {
                Ok(content) => {
                    drop(ramdisk);
                    match core::str::from_utf8(&content) {
                        Ok(text) => {
                            let lines = text.lines().count();
                            let words = text.split_whitespace().count();
                            let bytes = content.len();
                            println!("{:8} {:8} {:8}", lines, words, bytes);
                        }
                        Err(_) => println!("Error: File '{}' is binary", filename),
                    }
                }
                Err(_) => println!("Error: File '{}' not found", filename),
            }
        }
        // Handle: ls | grep pattern
        else if first_cmd[0] == "ls" && second_cmd[0] == "grep" {
            if second_cmd.len() < 2 {
                println!("Usage: ls | grep pattern");
                return;
            }

            let pattern = second_cmd[1];

            use crate::fs::ramdisk::RAMDISK;
            use crate::fs::vfs::FileSystem;

            let ramdisk = RAMDISK.lock();
            let files = ramdisk.list();
            drop(ramdisk);

            let mut match_count = 0;

            for file in &files {
                if file.name.contains(pattern) {
                    match_count += 1;
                    println!("{:32} {:8} bytes", file.name, file.size);
                }
            }

            if match_count == 0 {
                println!("No matches found");
            } else {
                println!();
                println!("Total: {} match(es)", match_count);
            }
        }
        else {
            println!("Error: Pipe combination '{}' | '{}' not supported", first_cmd[0], second_cmd[0]);
            println!("Supported pipes:");
            println!("  cat <file> | grep <pattern>");
            println!("  cat <file> | wc");
            println!("  ls | grep <pattern>");
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

    fn cmd_pwd(&self) {
        println!("{}", self.current_dir);
    }

    fn cmd_date(&self) {
        use crate::task::timer::current_ticks;

        let ticks = current_ticks();
        let seconds = ticks / 18;
        let minutes = seconds / 60;
        let hours = minutes / 60;
        let days = hours / 24;

        // Simple uptime-based date (not real date)
        println!("System uptime: {} days, {}:{:02}:{:02}",
            days,
            hours % 24,
            minutes % 60,
            seconds % 60
        );
        println!("(Note: RustOS does not have RTC support yet)");
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

                // Simple busy-wait sleep using timer ticks
                use crate::task::timer::current_ticks;
                let start = current_ticks();
                let target = start + (seconds * 18);  // ~18 ticks per second

                while current_ticks() < target {
                    // Busy wait
                    core::hint::spin_loop();
                }

                println!("Done");
            }
            Err(_) => println!("Error: Invalid number '{}'", args[0]),
        }
    }

    fn cmd_cd(&mut self, args: &[&str]) {
        use crate::fs::fat32::FAT32;

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
            // Stay in current directory
            return;
        }

        if target == ".." {
            // Go to parent directory
            if self.current_dir == "/" {
                // Already at root
                return;
            }

            // Remove last component from path
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
            // Absolute path
            target.to_string()
        } else {
            // Relative path
            if self.current_dir == "/" {
                format!("/{}", target)
            } else {
                format!("{}/{}", self.current_dir, target)
            }
        };

        // Check if directory exists on FAT32
        let fat32 = FAT32.lock();
        if let Some(ref fs) = *fat32 {
            // Verify it's a directory
            match fs.list_directory(&new_path) {
                Ok(_) => {
                    drop(fat32);
                    self.current_dir = new_path;
                    println!("Changed directory to: {}", self.current_dir);
                }
                Err(e) => {
                    drop(fat32);
                    println!("cd: {}: {}", target, e);
                }
            }
        } else {
            drop(fat32);
            println!("cd: FAT32 filesystem not mounted");
        }
    }

    fn cmd_mkdir(&mut self, args: &[&str]) {
        use crate::fs::fat32::FAT32;

        if args.is_empty() {
            println!("Usage: mkdir <directory>");
            return;
        }

        let dirname = args[0];

        // Construct full path
        let path = if dirname.starts_with('/') {
            dirname.to_string()
        } else {
            if self.current_dir == "/" {
                format!("/{}", dirname)
            } else {
                format!("{}/{}", self.current_dir, dirname)
            }
        };

        let mut fat32 = FAT32.lock();
        if let Some(ref mut fs) = *fat32 {
            match fs.create_directory(&path) {
                Ok(_) => println!("Directory '{}' created", dirname),
                Err(e) => println!("mkdir: {}: {}", dirname, e),
            }
        } else {
            println!("mkdir: FAT32 filesystem not mounted");
        }
    }

    fn cmd_rmdir(&mut self, args: &[&str]) {
        use crate::fs::fat32::FAT32;

        if args.is_empty() {
            println!("Usage: rmdir <directory>");
            return;
        }

        let dirname = args[0];

        // Construct full path
        let path = if dirname.starts_with('/') {
            dirname.to_string()
        } else {
            if self.current_dir == "/" {
                format!("/{}", dirname)
            } else {
                format!("{}/{}", self.current_dir, dirname)
            }
        };

        let mut fat32 = FAT32.lock();
        if let Some(ref mut fs) = *fat32 {
            match fs.remove_directory(&path) {
                Ok(_) => println!("Directory '{}' removed", dirname),
                Err(e) => println!("rmdir: {}: {}", dirname, e),
            }
        } else {
            println!("rmdir: FAT32 filesystem not mounted");
        }
    }

    /// Test user mode (Ring 3) and system calls
    fn cmd_usermode(&mut self) {
        println!("Testing user mode (Ring 3) and system calls...");
        println!("Transitioning from kernel mode (Ring 0) to user mode (Ring 3)");
        println!();

        // Jump to user mode and execute demo function
        unsafe {
            crate::userspace::jump_to_usermode(crate::userspace::user_mode_demo as usize);
        }

        // Should never reach here - user_mode_demo calls exit syscall
        println!("ERROR: Returned from user mode unexpectedly!");
    }
}
