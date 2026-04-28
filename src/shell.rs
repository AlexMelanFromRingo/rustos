/// Simple command-line shell for RustOS
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;
use crate::{print, println};

const MAX_HISTORY: usize = 50;

/// Cross-task command queue: background tasks (cron, init, etc.) push command
/// strings here.  The shell drains and runs them between user input events.
pub static SHELL_COMMAND_QUEUE: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Submit a command string for the shell to execute when next idle.
/// Returns true if queued.
pub fn enqueue_command(cmd: &str) -> bool {
    SHELL_COMMAND_QUEUE.lock().push_back(cmd.to_string());
    true
}
/// Job status for shell job control
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Stopped,
    Done,
}

/// A shell job (background process)
#[derive(Debug, Clone)]
pub struct Job {
    pub id: usize,
    pub pid: usize,
    pub command: String,
    pub status: JobStatus,
}

impl Job {
    fn status_str(&self) -> &'static str {
        match self.status {
            JobStatus::Running => "Running",
            JobStatus::Stopped => "Stopped",
            JobStatus::Done => "Done",
        }
    }
}

/// Simple glob pattern matching (supports * and ?)
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let (plen, tlen) = (pat.len(), txt.len());
    let (mut pi, mut ti) = (0, 0);
    let (mut star_pi, mut star_ti) = (usize::MAX, 0);

    while ti < tlen {
        if pi < plen && (pat[pi] == '?' || pat[pi] == txt[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < plen && pat[pi] == '*' {
            star_pi = pi;
            star_ti = ti;
            pi += 1;
        } else if star_pi != usize::MAX {
            pi = star_pi + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    while pi < plen && pat[pi] == '*' {
        pi += 1;
    }

    pi == plen
}

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
    jobs: Vec<Job>,  // Background jobs
    next_job_id: usize,  // Next job ID to assign
    last_exit: i32,   // $? — exit status of the most recently completed command
    /// Per-command stdin buffer.  When Some, commands that would otherwise
    /// read from the TTY (cat, wc, grep, head, tail, sort, ...) consume
    /// from this string instead.  Set by here-docs, drained by the
    /// command, then cleared in `execute()` after dispatch.
    stdin_override: Option<String>,
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
            jobs: Vec::new(),
            next_job_id: 1,
            last_exit: 0,
            stdin_override: None,
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
        // PS1-style prompt: user@host:cwd$  (root gets `#`)
        let creds = crate::users::CURRENT_CREDS.lock();
        let user = creds.username.clone();
        let is_root = creds.uid == 0;
        drop(creds);
        let cwd: &str = if self.current_dir.is_empty() { "/" } else { &self.current_dir };
        let mark = if is_root { '#' } else { '$' };
        print!("{}@{}:{}{} ", user, self.hostname, cwd, mark);
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

    /// Get the display length of the prompt (`user@host:cwd$ `)
    pub fn prompt_len(&self) -> usize {
        let creds = crate::users::CURRENT_CREDS.lock();
        let username_len = creds.username.len();
        drop(creds);
        let cwd_len = if self.current_dir.is_empty() { 1 } else { self.current_dir.len() };
        // user + '@' + host + ':' + cwd + '$' or '#' + ' '
        username_len + 1 + self.hostname.len() + 1 + cwd_len + 1 + 1
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
                "alias", "bg", "cat", "cd", "chmod", "chown", "clear", "cp",
                "date", "df", "du", "echo", "dmesg", "edit", "env", "exec",
                "export", "fg", "find", "free", "grep", "head", "hello", "help",
                "history", "hostname", "id", "jobs", "kill", "less", "ln", "ls",
                "meminfo", "mkdir", "more", "mount", "mv", "printenv", "ps",
                "pwd", "readlink", "reboot", "renice", "rm", "rmdir", "shutdown",
                "sleep", "spawn", "stat", "tail", "time", "top", "touch", "tree",
                "umount", "uname", "unalias", "unset", "uptime", "useradd",
                "usermode", "version", "wc", "which", "whoami", "write",
                "passwd", "su", "service", "systemctl", "runlevel", "init",
                "telinit", "slabinfo", "login", "logout", "exit", "motd",
                "polltest", "epolltest", "ifconfig", "ip", "netstat", "ss",
                "ping", "udptest", "tcptest", "httptest", "unixtest",
                "logger", "syslog", "crontab", "nslookup", "host", "getent",
                "vmstat", "vmalloc-test", "ulimit", "dns-cache",
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

    /// Drain the cross-task command queue, running each pending command in
    /// this shell.  Called by the keyboard loop between input events so
    /// background tasks (cron, etc.) can submit work.
    pub fn drain_queued_commands(&mut self) {
        loop {
            let cmd = match SHELL_COMMAND_QUEUE.lock().pop_front() {
                Some(c) => c,
                None => break,
            };
            self.buffer = cmd;
            self.cursor_pos = self.buffer.len();
            self.execute();
        }
    }

    /// True iff there is work pending in the queue.
    pub fn has_queued_commands() -> bool {
        !SHELL_COMMAND_QUEUE.lock().is_empty()
    }

    /// Set the input buffer to `cmd` (used by external dispatchers that
    /// will then call .execute()).
    pub fn set_buffer_for(&mut self, cmd: &str) {
        self.buffer = cmd.to_string();
        self.cursor_pos = self.buffer.len();
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

        // Expand environment variables ($VAR) — but skip for `for` loops
        // (for loop handles its own variable substitution)
        let command = if command.starts_with("for ") || command.starts_with("if ") || command.starts_with("while ") {
            command
        } else {
            self.expand_env_vars(&command)
        };

        // Expand $(...) command substitution before any further parsing.
        let command = self.expand_command_subst(&command);

        // Here-docs: `cat << EOF\nLINE\nLINE\nEOF` — we accept the form
        // `<< MARKER ; LINE ; LINE ; MARKER` on a single typed line because
        // our shell doesn't accept multi-line input mid-command.  The
        // semicolons act as line separators inside the here-doc.
        let command = if let Some(idx) = command.find("<<") {
            self.expand_heredoc(&command, idx)
        } else {
            command
        };

        // Real AST-driven control flow: if/while/for go through the
        // recursive-descent parser in src/shell_parser.rs.  Anything that
        // isn't a control structure falls through to the legacy linear
        // dispatcher below (which also handles pipes, redirection, glob,
        // background jobs, etc).
        if command.starts_with("if ") || command.starts_with("while ") || command.starts_with("for ") {
            match crate::shell_parser::parse(&command) {
                Ok(ast) => {
                    self.execute_ast(&ast);
                    return;
                }
                Err(e) => {
                    println!("syntax error: {}", e);
                    return;
                }
            }
        }

        // Check for semicolon-separated commands
        if command.contains(';') {
            let commands: Vec<&str> = command.split(';').collect();
            for subcmd in commands {
                let subcmd = subcmd.trim();
                if !subcmd.is_empty() {
                    self.buffer = subcmd.to_string();
                    self.cursor_pos = self.buffer.len();
                    self.execute();
                }
            }
            return;
        }

        // `for` is handled by the AST path above; anything reaching here
        // beginning with `for ` would have failed to parse.

        // Check for output redirection
        if command.contains('>') {
            self.execute_with_redirection(&command);
            return;
        }

        // Check for background execution (&)
        let (command, background) = if command.ends_with('&') {
            let cmd = command.trim_end_matches('&').trim().to_string();
            (cmd, true)
        } else {
            (command, false)
        };

        // Check for pipes
        if command.contains('|') {
            self.execute_pipeline(&command);
            return;
        }

        // Parse command and arguments
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return;
        }
        let cmd = parts[0];
        let raw_args = &parts[1..];

        // Expand glob patterns in arguments
        let expanded = self.expand_globs(raw_args);
        let expanded_refs: Vec<&str> = expanded.iter().map(|s| s.as_str()).collect();
        let args = &expanded_refs[..];

        // Handle background execution
        if background {
            self.run_background(&command);
            return;
        }

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
            "ls" => self.cmd_ls(args),
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
            "dmesg" => self.cmd_dmesg(args),
            "hostname" => self.cmd_hostname(args),
            "whoami" => self.cmd_whoami(),
            "id" => self.cmd_id(),
            "su" => self.cmd_su(args),
            "passwd" => self.cmd_passwd(args),
            "useradd" => self.cmd_useradd(args),
            "sudo" => self.cmd_sudo(args),
            "uname" => self.cmd_uname(args),
            "free" => self.cmd_free(args),
            "top" => self.cmd_top(),
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
            "spawn" => self.cmd_spawn(args),
            "export" => self.cmd_export(args),
            "printenv" | "env" => self.cmd_printenv(args),
            "unset" => self.cmd_unset(args),
            "renice" => self.cmd_renice(args),
            "stat" => self.cmd_stat(args),
            "ln" => self.cmd_ln(args),
            "chmod" => self.cmd_chmod(args),
            "chown" => self.cmd_chown(args),
            "readlink" => self.cmd_readlink(args),
            "jobs" => self.cmd_jobs(),
            "fg" => self.cmd_fg(args),
            "bg" => self.cmd_bg(args),
            "exec" => self.cmd_exec(args),
            "seq" => self.cmd_seq(args),
            "service" | "systemctl" => self.cmd_service(args),
            "runlevel" => self.cmd_runlevel(),
            "init" | "telinit" => self.cmd_init(args),
            "slabinfo" => self.cmd_slabinfo(),
            "vmalloc-test" | "vmstat" => self.cmd_vmstat(args),
            "login" => self.cmd_login(args),
            "logout" | "exit" => self.cmd_logout(),
            "motd" => self.cmd_motd(),
            "polltest" => self.cmd_polltest(),
            "epolltest" => self.cmd_epolltest(),
            "ulimit" => self.cmd_ulimit(args),
            "flock" => self.cmd_flock(args),
            "fcntl" => self.cmd_fcntl(args),
            "partprobe" => self.cmd_partprobe(args),
            "pkg" => self.cmd_pkg(args),
            "mknod" => self.cmd_mknod(args),
            "tsc" => self.cmd_tsc(),
            "seccomp" => self.cmd_seccomp(args),
            "getcap" => self.cmd_getcap(args),
            "setcap" => self.cmd_setcap(args),
            "ifconfig" | "ip" => self.cmd_ifconfig(args),
            "dhclient" => self.cmd_dhclient(args),
            "arp" => self.cmd_arp(args),
            "netstat" | "ss" => self.cmd_netstat(args),
            "ping" => self.cmd_ping(args),
            "udptest" => self.cmd_udptest(args),
            "tcptest" => self.cmd_tcptest(args),
            "tcpinfo" => self.cmd_tcpinfo(),
            "inode" => self.cmd_inode(args),
            "sha256" | "sha256sum" => self.cmd_sha256(args),
            "mouse" => self.cmd_mouse(),
            "hpet" => self.cmd_hpet(),
            "futex" => self.cmd_futex(args),
            "panic-test" => self.cmd_panic_test(),
            "ext2" => self.cmd_ext2(args),
            "lspci" => self.cmd_lspci(),
            "nicstat" => self.cmd_nicstat(),
            "httptest" => self.cmd_httptest(args),
            "unixtest" => self.cmd_unixtest(args),
            "wget" | "curl" => self.cmd_wget(args),
            "logger" => self.cmd_logger(args),
            "syslog" => self.cmd_syslog(args),
            "crontab" => self.cmd_crontab(args),
            "nslookup" | "host" => self.cmd_nslookup(args),
            "getent" => self.cmd_getent(args),
            "dns-cache" => self.cmd_dns_cache(args),
            "test" | "[" => self.cmd_test(args),
            "true" => {},
            "false" => println!("false"),
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
                        jobs: self.jobs.clone(),
                        next_job_id: self.next_job_id,
                        last_exit: self.last_exit,
                        stdin_override: self.stdin_override.clone(),
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
        println!("  dmesg     - Display kernel log messages (usage: dmesg [-n N])");
        println!("  time      - Show current timer ticks");
        println!("  meminfo   - Display memory information");
        println!("  version   - Show RustOS version");
        println!("  history   - Show command history");
        println!("  hostname  - Get/set hostname (usage: hostname [name])");
        println!("  shutdown  - Shutdown the system");
        println!("  reboot    - Reboot the system");
        println!("  sleep     - Sleep for N seconds (usage: sleep <seconds>)");
        println!("  whoami    - Print current user name");
        println!("  id        - Print user and group IDs");
        println!("  uname     - Print system information (usage: uname [-a])");
        println!("  free      - Display memory usage (usage: free [-h])");
        println!("  top       - Show process status summary");
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
        println!("  ln        - Create symbolic link (usage: ln -s <target> <link>)");
        println!("  chmod     - Change permissions (usage: chmod <mode> <file>)");
        println!("  chown     - Change ownership (usage: chown <uid[:gid]> <file>)");
        println!("  readlink  - Show symlink target (usage: readlink <link>)");
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
        println!("Job control:");
        println!("  <cmd> &   - Run command in background");
        println!("  jobs      - List background jobs");
        println!("  fg [%N]   - Bring job N to foreground");
        println!("  bg [%N]   - Continue stopped job in background");
        println!();
        println!("Users / authentication:");
        println!("  whoami    - Print current username");
        println!("  id        - Print uid/gid/groups");
        println!("  su [user] - Switch user (default root)");
        println!("  passwd    - Change user password");
        println!("  useradd   - Add a new user (root only)");
        println!();
        println!("Init / services:");
        println!("  service <name> <start|stop|restart|status>");
        println!("  systemctl <verb> <name>      - Same as service");
        println!("  runlevel  - Show current runlevel");
        println!("  init <0..6> / telinit <0..6> - Change runlevel");
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

    fn cmd_ls(&self, args: &[&str]) {
        use crate::fs::vfs::{VfsContext, VfsFileType};
        use crate::vga_buffer::{WRITER, Color};
        use x86_64::instructions::interrupts;

        let mut long_format = false;
        let mut dir = self.current_dir.as_str();
        for arg in args {
            if *arg == "-l" || *arg == "-la" || *arg == "-al" {
                long_format = true;
            } else {
                dir = arg;
            }
        }

        match VfsContext::list_dir(dir) {
            Ok(files) => {
                if files.is_empty() {
                    println!("Empty directory");
                    return;
                }

                for file in &files {
                    if long_format {
                        let mode_str = Self::format_mode(file.file_type, file.mode);
                        print!("{} {:>5} {:>5} {:>8} ", mode_str, file.uid, file.gid, file.size);
                    }

                    match file.file_type {
                        VfsFileType::Directory => {
                            interrupts::without_interrupts(|| {
                                WRITER.lock().set_color(Color::LightBlue, Color::Black);
                            });
                            print!("{}/", file.name);
                        }
                        VfsFileType::Symlink => {
                            interrupts::without_interrupts(|| {
                                WRITER.lock().set_color(Color::LightGreen, Color::Black);
                            });
                            // Show symlink target
                            let full_path = if dir == "/" {
                                format!("/{}", file.name)
                            } else {
                                format!("{}/{}", dir, file.name)
                            };
                            if let Ok(target) = VfsContext::readlink(&full_path) {
                                print!("{} -> {}", file.name, target);
                            } else {
                                print!("{}", file.name);
                            }
                        }
                        _ => {
                            interrupts::without_interrupts(|| {
                                WRITER.lock().set_color(Color::LightCyan, Color::Black);
                            });
                            print!("{}", file.name);
                        }
                    }

                    // Reset color
                    interrupts::without_interrupts(|| {
                        WRITER.lock().reset_color();
                    });

                    if !long_format && file.file_type == VfsFileType::Regular {
                        print!("  {} bytes", file.size);
                    }
                    println!();
                }
            }
            Err(_) => {
                println!("ls: cannot list '{}'", dir);
            }
        }
    }

    fn cmd_cat(&mut self, args: &[&str]) {
        use crate::fs::vfs::VfsContext;

        if args.is_empty() {
            // Read from stdin: heredoc body if set, else nothing.
            if let Some(body) = self.take_stdin() {
                print!("{}", body);
                if !body.ends_with('\n') { println!(); }
                return;
            }
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

    fn cmd_wc(&mut self, args: &[&str]) {
        use crate::fs::vfs::VfsContext;

        if args.is_empty() {
            // Read from stdin (heredoc).
            let body = match self.take_stdin() {
                Some(b) => b,
                None => {
                    println!("Usage: wc <filename>");
                    return;
                }
            };
            let bytes = body.len();
            let lines = body.lines().count();
            let words = body.split_whitespace().count();
            println!("  {} {} {}", lines, words, bytes);
            return;
        }

        let filename = args[0];

        match VfsContext::read(filename) {
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

        println!("  PID  PGID   SID  NI  STATE       STACK");
        println!("  ---  ----  ----  --  -----       -----");

        for process in processes {
            println!("{:5}  {:4}  {:4}  {:3}  {:<11} {} bytes",
                process.pid,
                process.pgid,
                process.sid,
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
    /// Execute a for loop: for VAR in ITEM1 ITEM2 ...; do CMD; done
    /// Also supports: for VAR in ITEMS; do CMD; done (all on one line)
    fn execute_for_loop(&mut self, command: &str) {
        // Parse: for VAR in ITEM1 ITEM2 ...; do CMD; done
        // or:   for VAR in ITEM1 ITEM2 ... do CMD done
        let command = command.trim();

        // Remove "for " prefix
        let rest = &command[4..];

        // Find variable name
        let parts: Vec<&str> = rest.splitn(2, " in ").collect();
        if parts.len() != 2 {
            println!("Syntax error: expected 'for VAR in ITEMS; do CMD; done'");
            return;
        }

        let var_name = parts[0].trim();
        let rest = parts[1].trim();

        // Find "do" keyword
        let (items_str, body_str) = if let Some(do_pos) = rest.find(" do ") {
            (&rest[..do_pos], &rest[do_pos + 4..])
        } else if let Some(do_pos) = rest.find("; do ") {
            (&rest[..do_pos], &rest[do_pos + 5..])
        } else {
            println!("Syntax error: missing 'do' keyword");
            return;
        };

        // Remove trailing "done" or "; done"
        let body = body_str
            .trim_end()
            .trim_end_matches("done")
            .trim_end()
            .trim_end_matches(';')
            .trim();

        if body.is_empty() {
            println!("Syntax error: empty loop body");
            return;
        }

        // Parse items (split by whitespace, expand globs)
        let items: Vec<&str> = items_str.split_whitespace().collect();
        let expanded_items = self.expand_globs(&items);

        // Execute body for each item
        for item in &expanded_items {
            // Substitute $VAR in the body
            let expanded_body = body.replace(&format!("${}", var_name), item)
                .replace(&format!("${{{}}}", var_name), item);

            // Execute the command
            self.buffer = expanded_body;
            self.cursor_pos = self.buffer.len();
            self.execute();
        }
    }

    fn execute_with_redirection(&mut self, command_line: &str) {
        use crate::fs::vfs::VfsContext;

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

        // Parse and capture output using the VGA capture buffer
        let cmd_parts: Vec<&str> = command.split_whitespace().collect();
        if cmd_parts.is_empty() {
            println!("Error: No command specified");
            return;
        }

        let cmd = cmd_parts[0];
        let args = &cmd_parts[1..];

        // Capture the command's println! output
        crate::vga_buffer::start_capture();
        match cmd {
            "echo" => self.cmd_echo(args),
            "cat" => self.cmd_cat(args),
            "ls" => self.cmd_ls(args),
            "help" => self.cmd_help(),
            "ps" => self.cmd_ps(),
            "date" => self.cmd_date(),
            "uptime" => self.cmd_uptime(),
            "dmesg" => self.cmd_dmesg(args),
            "printenv" | "env" => self.cmd_printenv(args),
            "free" => self.cmd_free(args),
            "uname" => self.cmd_uname(args),
            "df" => self.cmd_df(),
            "history" => self.cmd_history(),
            "hostname" => self.cmd_hostname(args),
            "whoami" => self.cmd_whoami(),
            "id" => self.cmd_id(),
            _ => {
                let _ = crate::vga_buffer::stop_capture();
                println!("Error: Command '{}' does not support output redirection", cmd);
                return;
            }
        }
        let content = crate::vga_buffer::stop_capture();

        if content.is_empty() {
            return;
        }

        // Handle append vs overwrite via VFS
        let final_content = if append {
            match VfsContext::read(filename) {
                Ok(existing) => {
                    match core::str::from_utf8(&existing) {
                        Ok(existing_text) => {
                            format!("{}{}", existing_text, content)
                        }
                        Err(_) => {
                            println!("Error: Cannot append to binary file");
                            return;
                        }
                    }
                }
                Err(_) => content,
            }
        } else {
            content
        };

        match VfsContext::write(filename, final_content.as_bytes().to_vec()) {
            Ok(_) => println!("Output written to '{}'", filename),
            Err(_) => println!("Error writing to file '{}'", filename),
        }
    }

    /// Execute a pipeline of commands
    ///
    /// Uses output capture to grab the text output of the first command,
    /// then pipes it as input to the second command (grep, wc, head, tail, sort).
    fn execute_pipeline(&mut self, command_line: &str) {
        let commands: Vec<&str> = command_line.split('|').map(|s| s.trim()).collect();

        if commands.len() < 2 {
            println!("Error: Invalid pipe syntax");
            return;
        }

        let second_cmd: Vec<&str> = commands[1].split_whitespace().collect();

        if commands[0].trim().is_empty() || second_cmd.is_empty() {
            println!("Error: Invalid pipe syntax");
            return;
        }

        // Step 1: Capture output from first command using VGA capture buffer
        crate::vga_buffer::start_capture();

        // Parse and execute the first command normally — output goes to capture buffer
        let first_parts: Vec<&str> = commands[0].split_whitespace().collect();
        if !first_parts.is_empty() {
            let cmd = first_parts[0];
            let args = &first_parts[1..];
            match cmd {
                "help" => self.cmd_help(),
                "echo" => self.cmd_echo(args),
                "uptime" => self.cmd_uptime(),
                "meminfo" => self.cmd_meminfo(),
                "version" => self.cmd_version(),
                "history" => self.cmd_history(),
                "ps" => self.cmd_ps(),
                "ls" => self.cmd_ls(args),
                "cat" => self.cmd_cat(args),
                "date" => self.cmd_date(),
                "dmesg" => self.cmd_dmesg(args),
                "uname" => self.cmd_uname(args),
                "free" => self.cmd_free(args),
                "printenv" | "env" => self.cmd_printenv(args),
                "df" => self.cmd_df(),
                "pwd" => self.cmd_pwd(),
                "hostname" => self.cmd_hostname(args),
                "whoami" => self.cmd_whoami(),
                "id" => self.cmd_id(),
                "top" => self.cmd_top(),
                "kill" => self.cmd_kill(args),
                _ => println!("Pipe: '{}' cannot produce output", cmd),
            }
        }

        let output = crate::vga_buffer::stop_capture();

        if output.is_empty() {
            return;
        }

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
        // No args: list current mounts (Linux behaviour).
        if args.is_empty() {
            for m in crate::fs::vfs::list_mounts() {
                println!("{} on {} type {} ({})",
                    m.source, m.mount_point, m.fs_type, m.options);
            }
            return;
        }

        // mount -t TYPE SOURCE TARGET
        if args[0] == "-t" && args.len() >= 4 {
            let fs_type = args[1];
            let source = args[2];
            let target = args[3];
            crate::fs::vfs::register_mount(crate::fs::vfs::Mount::new(
                source, target, fs_type, "rw",
            ));
            println!("mount: {} mounted on {} ({})", source, target, fs_type);
            return;
        }

        // Legacy: `mount fat32`
        match args[0] {
            "fat32" => {
                use crate::fs::fat32;

                println!("Attempting to mount FAT32 filesystem...");
                match fat32::init() {
                    Ok(_) => {
                        println!("FAT32 filesystem mounted successfully!");
                        crate::fs::vfs::register_mount(crate::fs::vfs::Mount::new(
                            "fat32:/dev/sda1", "/mnt", "fat32", "rw",
                        ));
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
                println!("Supported filesystems: fat32, or `mount -t TYPE SRC TARGET`");
            }
        }
    }

    fn cmd_umount(&self, args: &[&str]) {
        if !args.is_empty() {
            // Specific mount-point unmount.
            let target = args[0];
            if crate::fs::vfs::unregister_mount(target) {
                if target == "/mnt" {
                    use crate::fs::fat32::FAT32;
                    *FAT32.lock() = None;
                }
                println!("Unmounted {}", target);
            } else {
                println!("umount: {}: not mounted", target);
            }
            return;
        }

        // Default: unmount FAT32 (legacy behaviour).
        use crate::fs::fat32::FAT32;
        let mut fat32 = FAT32.lock();
        if fat32.is_some() {
            *fat32 = None;
            crate::fs::vfs::unregister_mount("/mnt");
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
                // $? — exit status of the most recent command
                if bytes[i + 1] == b'?' {
                    result.push_str(&format!("{}", self.last_exit));
                    i += 2;
                    continue;
                }
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

    /// Expand a here-doc by extracting the body and stashing it in
    /// `stdin_override`.  The remaining command line (without `<<MARKER`
    /// ... `MARKER`) is returned for execution; commands that would
    /// otherwise read from stdin will consume from `stdin_override`
    /// instead.  No temp file is created.
    ///
    /// Single-line input form:  `cat << EOF ; line1 ; line2 ; EOF`
    /// (semicolons act as line separators because this shell doesn't
    /// accept multi-line input mid-command yet).
    fn expand_heredoc(&mut self, command: &str, idx: usize) -> String {
        let head = &command[..idx];
        let tail = &command[idx + 2..];
        let tail = tail.trim_start();

        let mut marker_end = 0;
        for (i, ch) in tail.char_indices() {
            if ch.is_whitespace() || ch == ';' {
                marker_end = i;
                break;
            }
            marker_end = i + ch.len_utf8();
        }
        if marker_end == 0 {
            return command.to_string();
        }
        let marker = &tail[..marker_end];
        let rest = &tail[marker_end..];

        let mut body = String::new();
        let mut after_marker_idx: Option<usize> = None;
        let mut acc_offset = 0usize;
        for piece in rest.split(';') {
            acc_offset += piece.len() + 1;
            let trimmed = piece.trim();
            if trimmed == marker {
                after_marker_idx = Some(acc_offset);
                break;
            }
            if !trimmed.is_empty() {
                body.push_str(trimmed);
                body.push('\n');
            }
        }

        // Stash the heredoc body so the command's stdin reads it.
        self.stdin_override = Some(body);

        let after = after_marker_idx
            .and_then(|i| rest.get(i.min(rest.len())..))
            .unwrap_or("");
        format!("{}{}", head.trim_end(), after)
    }

    /// Take the heredoc body if any, leaving `stdin_override` empty for
    /// the next command.
    pub fn take_stdin(&mut self) -> Option<String> {
        self.stdin_override.take()
    }

    /// Expand `$(cmd)` substitutions by capturing each inner command's output.
    /// Trailing newlines are stripped (like POSIX shells).
    fn expand_command_subst(&mut self, input: &str) -> String {
        let bytes = input.as_bytes();
        let mut out = String::new();
        let mut i = 0;
        while i < bytes.len() {
            if i + 1 < bytes.len() && bytes[i] == b'$' && bytes[i + 1] == b'(' {
                // Find the matching close paren (no nested support).
                let mut depth = 1;
                let start = i + 2;
                let mut j = start;
                while j < bytes.len() && depth > 0 {
                    match bytes[j] {
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 { break; }
                    j += 1;
                }
                if depth != 0 {
                    out.push_str(&input[i..]);
                    return out;
                }
                let inner = &input[start..j];
                let captured = self.capture_inner_command(inner);
                let trimmed = captured.trim_end_matches('\n').trim_end_matches('\r');
                out.push_str(trimmed);
                i = j + 1;
            } else {
                out.push(input[i..].chars().next().unwrap());
                i += input[i..].chars().next().unwrap().len_utf8();
            }
        }
        out
    }

    /// Run `cmd` capturing its VGA output and returning the captured text.
    /// Used for command substitution.
    fn capture_inner_command(&mut self, cmd: &str) -> String {
        crate::vga_buffer::start_capture();
        // Save and restore the buffer/cursor so re-entrant execute() doesn't
        // clobber the outer command line.
        let saved_buf = core::mem::take(&mut self.buffer);
        let saved_cursor = self.cursor_pos;
        self.buffer = cmd.to_string();
        self.cursor_pos = self.buffer.len();
        self.execute();
        self.buffer = saved_buf;
        self.cursor_pos = saved_cursor;
        crate::vga_buffer::stop_capture()
    }

    /// Evaluate a parsed AST against this shell.
    fn execute_ast(&mut self, cmd: &crate::shell_parser::Cmd) {
        use crate::shell_parser::Cmd;
        match cmd {
            Cmd::Simple(words) => {
                let line = crate::shell_parser::join_simple(words);
                self.run_subcommand(&line);
            }
            Cmd::Sequence(items) => {
                for it in items {
                    self.execute_ast(it);
                }
            }
            Cmd::If { branches, else_branch } => {
                let mut taken = false;
                for (cond, body) in branches {
                    self.execute_ast(cond);
                    if self.last_exit == 0 {
                        self.execute_ast(body);
                        taken = true;
                        break;
                    }
                }
                if !taken {
                    if let Some(eb) = else_branch {
                        self.execute_ast(eb);
                    }
                }
            }
            Cmd::While { cond, body } => {
                for _ in 0..1000 { // safety cap
                    self.execute_ast(cond);
                    if self.last_exit != 0 { break; }
                    self.execute_ast(body);
                }
            }
            Cmd::For { var, items, body } => {
                for v in items {
                    // Bind VAR=v in env_vars for the body (overwriting prior).
                    self.env_vars.retain(|(k, _)| k != var);
                    self.env_vars.push((var.clone(), v.clone()));
                    self.execute_ast(body);
                }
            }
        }
    }

    /// Legacy ad-hoc `if` parser, kept only as a compatibility fallback for
    /// callers that haven't been migrated to execute_ast yet.
    #[allow(dead_code)]
    fn execute_if(&mut self, command: &str) {
        let rest = command["if ".len()..].trim();
        let parts: Vec<String> = rest.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        self.execute_if_continued(&parts, 0, false);
    }

    fn execute_if_continued(&mut self, parts: &[String], start: usize, mut executed: bool) {
        // A "branch boundary" token is one of: "fi", "else"[+body], "elif "+cond.
        let is_boundary = |s: &str| {
            s == "fi" || s == "else" || s.starts_with("else ")
                || s.starts_with("elif ") || s.starts_with("else\t")
        };

        let mut idx = start;
        while idx < parts.len() {
            let cond = parts[idx].clone();
            idx += 1;
            if idx >= parts.len() { return; }
            // The next part should start with "then".
            let p = &parts[idx];
            let first_body = if p == "then" {
                String::new()
            } else if let Some(rest) = p.strip_prefix("then ") {
                rest.trim().to_string()
            } else if let Some(rest) = p.strip_prefix("then\t") {
                rest.trim().to_string()
            } else {
                println!("if: expected 'then' got '{}'", p);
                return;
            };
            let mut body: Vec<String> = Vec::new();
            if !first_body.is_empty() { body.push(first_body); }
            idx += 1;
            while idx < parts.len() && !is_boundary(&parts[idx]) {
                body.push(parts[idx].clone());
                idx += 1;
            }

            if !executed {
                self.run_subcommand(&cond);
                if self.last_exit == 0 {
                    for b in &body { self.run_subcommand(b); }
                    executed = true;
                }
            }
            if idx >= parts.len() { return; }

            let p = parts[idx].clone();
            if p == "fi" { return; }
            if p == "else" || p.starts_with("else ") || p.starts_with("else\t") {
                let mut else_body: Vec<String> = Vec::new();
                if let Some(rest) = p.strip_prefix("else ").or_else(|| p.strip_prefix("else\t")) {
                    let r = rest.trim();
                    if !r.is_empty() { else_body.push(r.to_string()); }
                }
                idx += 1;
                while idx < parts.len() && parts[idx].as_str() != "fi" {
                    else_body.push(parts[idx].clone());
                    idx += 1;
                }
                if !executed {
                    for b in &else_body { self.run_subcommand(b); }
                }
                return;
            }
            if p.starts_with("elif ") {
                let trimmed = p["elif ".len()..].trim().to_string();
                let mut new_parts: Vec<String> = parts[..idx].to_vec();
                new_parts.push(trimmed);
                new_parts.extend_from_slice(&parts[idx + 1..]);
                return self.execute_if_continued(&new_parts, idx, executed);
            }
            return;
        }
    }

    /// `while COND ; do BODY ; done` with a 1000-iteration safety cap.
    fn execute_while(&mut self, command: &str) {
        let rest = command["while ".len()..].trim();
        let parts: Vec<String> = rest.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            println!("while: missing condition");
            return;
        }
        let cond = parts[0].clone();
        let mut idx = 1;
        if idx >= parts.len() || !parts[idx].starts_with("do") {
            println!("while: missing 'do'");
            return;
        }
        let first_body = parts[idx]["do".len()..].trim().to_string();
        let mut body = Vec::new();
        if !first_body.is_empty() { body.push(first_body); }
        idx += 1;
        while idx < parts.len() && parts[idx].as_str() != "done" {
            body.push(parts[idx].clone());
            idx += 1;
        }

        for _ in 0..1000 {
            self.run_subcommand(&cond);
            if self.last_exit != 0 { break; }
            for b in &body { self.run_subcommand(b); }
        }
    }

    /// Re-enter the dispatcher with a freshly assembled command line.  Used
    /// by the if/while/sudo paths to run a sub-command without disturbing
    /// the outer caller's buffer/cursor too much.
    fn run_subcommand(&mut self, cmd: &str) {
        let saved_buf = core::mem::take(&mut self.buffer);
        let saved_cursor = self.cursor_pos;
        self.buffer = cmd.to_string();
        self.cursor_pos = self.buffer.len();
        self.execute();
        self.buffer = saved_buf;
        self.cursor_pos = saved_cursor;
    }

    /// Expand glob patterns (*, ?) in arguments
    fn expand_globs(&self, args: &[&str]) -> Vec<String> {
        use crate::fs::vfs::VfsContext;

        let mut result = Vec::new();

        for arg in args {
            if !arg.contains('*') && !arg.contains('?') {
                result.push(arg.to_string());
                continue;
            }

            // Determine the directory to list and the pattern to match
            let (dir, pattern) = if let Some(pos) = arg.rfind('/') {
                let dir_part = &arg[..=pos];
                let pat_part = &arg[pos + 1..];
                // Resolve relative paths
                let resolved = if dir_part.starts_with('/') {
                    dir_part.to_string()
                } else if self.current_dir == "/" {
                    format!("/{}", dir_part)
                } else {
                    format!("{}/{}", self.current_dir, dir_part)
                };
                (resolved, pat_part.to_string())
            } else {
                (self.current_dir.clone(), arg.to_string())
            };

            // List directory entries
            let mut matched = Vec::new();
            if let Ok(entries) = VfsContext::list_dir(&dir) {
                for entry in &entries {
                    if glob_match(&pattern, &entry.name) {
                        let full_path = if dir == "/" {
                            format!("/{}", entry.name)
                        } else if arg.contains('/') {
                            // Reconstruct with the prefix
                            if let Some(pos) = arg.rfind('/') {
                                format!("{}{}", &arg[..=pos], entry.name)
                            } else {
                                entry.name.clone()
                            }
                        } else {
                            entry.name.clone()
                        };
                        matched.push(full_path);
                    }
                }
            }

            if matched.is_empty() {
                // No matches — keep the original pattern (like bash)
                result.push(arg.to_string());
            } else {
                matched.sort();
                result.extend(matched);
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

    fn cmd_uname(&self, args: &[&str]) {
        if args.is_empty() {
            println!("RustOS");
            return;
        }

        let show_all = args.contains(&"-a");
        let mut parts = Vec::new();

        if show_all || args.contains(&"-s") { parts.push("RustOS"); }
        if show_all || args.contains(&"-n") { parts.push(&self.hostname); }
        if show_all || args.contains(&"-r") { parts.push(env!("CARGO_PKG_VERSION")); }
        if show_all || args.contains(&"-v") { parts.push("#1 SMP PREEMPT"); }
        if show_all || args.contains(&"-m") { parts.push("x86_64"); }
        if show_all || args.contains(&"-p") { parts.push("x86_64"); }
        if show_all || args.contains(&"-o") { parts.push("RustOS/Rust"); }

        if parts.is_empty() {
            println!("RustOS");
        } else {
            println!("{}", parts.join(" "));
        }
    }

    fn cmd_free(&self, args: &[&str]) {
        let stats = crate::memory::memory_stats();
        let total_kb = stats.0 as u64 * 4;  // frames * 4KiB
        let used_kb = stats.1 as u64 * 4;
        let free_kb = total_kb - used_kb;

        let human = args.contains(&"-h");

        if human {
            let format_size = |kb: u64| -> String {
                if kb >= 1024 * 1024 {
                    format!("{:.1}Gi", kb as f64 / (1024.0 * 1024.0))
                } else if kb >= 1024 {
                    format!("{:.1}Mi", kb as f64 / 1024.0)
                } else {
                    format!("{}Ki", kb)
                }
            };
            println!("{:>14}{:>12}{:>12}{:>12}{:>12}{:>12}",
                "total", "used", "free", "shared", "buff/cache", "available");
            println!("Mem: {:>10}{:>12}{:>12}{:>12}{:>12}{:>12}",
                format_size(total_kb), format_size(used_kb), format_size(free_kb),
                "0B", "0B", format_size(free_kb));
            println!("Swap:{:>10}{:>12}{:>12}",
                "0B", "0B", "0B");
        } else {
            println!("{:>14}{:>12}{:>12}{:>12}{:>12}{:>12}",
                "total", "used", "free", "shared", "buff/cache", "available");
            println!("Mem: {:>10}{:>12}{:>12}{:>12}{:>12}{:>12}",
                total_kb, used_kb, free_kb, 0, 0, free_kb);
            println!("Swap:{:>10}{:>12}{:>12}",
                0, 0, 0);
        }
    }

    fn cmd_top(&self) {
        use crate::process::PROCESS_MANAGER;

        let pm = PROCESS_MANAGER.lock();
        let processes = pm.all_processes();
        let ticks = crate::task::timer::current_ticks();
        let uptime_secs = ticks / 18;

        // Header
        let dt = crate::drivers::rtc::read_datetime();
        let mut buf = [0u8; 32];
        let len = dt.format(&mut buf);
        let time_str = core::str::from_utf8(&buf[..len]).unwrap_or("??:??:??");
        println!("top - {} up {}:{:02}:{:02}, {} tasks",
            time_str,
            uptime_secs / 3600,
            (uptime_secs % 3600) / 60,
            uptime_secs % 60,
            processes.len(),
        );

        // Memory
        let stats = crate::memory::memory_stats();
        let total_kb = stats.0 * 4;
        let used_kb = stats.1 * 4;
        println!("MiB Mem:  {:6} total, {:6} free, {:6} used",
            total_kb / 1024, (total_kb - used_kb) / 1024, used_kb / 1024);
        println!();

        // Process table
        println!("{:>5} {:>3} {:>8} {:>8} {:<11} {}",
            "PID", "NI", "VIRT", "RES", "STATE", "COMMAND");

        for process in processes {
            if process.state == crate::process::ProcessState::Terminated {
                continue;
            }
            let res_kb = process.stack.len() / 1024;
            let virt_kb = process.user_stack_size.unwrap_or(0) / 1024;
            println!("{:>5} {:>3} {:>7}K {:>7}K {:<11} process_{}",
                process.pid,
                process.nice,
                virt_kb,
                res_kb,
                process.state.as_str(),
                process.pid,
            );
        }
    }

    fn cmd_dmesg(&self, args: &[&str]) {
        let entries = crate::klog::read_all();

        if entries.is_empty() {
            println!("(no kernel messages)");
            return;
        }

        // Support -n N to show last N entries
        let show_count = if args.len() >= 2 && args[0] == "-n" {
            args[1].parse::<usize>().unwrap_or(entries.len())
        } else {
            entries.len()
        };

        let start = entries.len().saturating_sub(show_count);
        for entry in &entries[start..] {
            println!("{}", entry);
        }
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

    /// less / more: page through a file one screen at a time.
    /// Press SPACE / Enter for next page, q to quit.  No backwards motion
    /// (matches `more(1)`).  Reads keys directly from the scancode/serial
    /// queues since the shell's main loop is async.
    fn cmd_less(&mut self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: less <filename>");
            return;
        }

        let filename = args[0];
        let content = match crate::fs::vfs::VfsContext::read(filename) {
            Ok(c) => c,
            Err(_) => { println!("Error: File '{}' not found", filename); return; }
        };
        let text = match core::str::from_utf8(&content) {
            Ok(t) => t,
            Err(_) => { println!("Error: File is not valid UTF-8 text"); return; }
        };

        // VGA console height — leave one line for the status prompt.
        const ROWS_PER_PAGE: usize = 24;
        let lines: alloc::vec::Vec<&str> = text.lines().collect();
        let total = lines.len();
        let mut idx = 0usize;

        loop {
            // Print one page.
            let end = (idx + ROWS_PER_PAGE).min(total);
            for line in &lines[idx..end] {
                println!("{}", line);
            }
            idx = end;

            if idx >= total {
                println!("(END)");
                return;
            }

            // Status line — show how far through the file we are.
            let pct = idx * 100 / total.max(1);
            print!(":-- {}% (line {}/{}) — SPACE/Enter: next, q: quit --", pct, idx, total);

            // Read one key.
            match self.read_pager_key() {
                'q' | 'Q' => { println!(); return; }
                _ => { println!(); /* SPACE / ENTER / anything else = next page */ }
            }
        }
    }

    /// Block until a key arrives.  Returns the lower-case character.
    fn read_pager_key(&mut self) -> char {
        use crate::task::keyboard::{SCANCODE_QUEUE, SERIAL_QUEUE};
        use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};

        let mut keyboard = Keyboard::new(
            ScancodeSet1::new(),
            layouts::Us104Key,
            HandleControl::MapLettersToUnicode,
        );

        loop {
            if let Ok(q) = SERIAL_QUEUE.try_get() {
                if let Some(byte) = q.pop() {
                    let c = byte as char;
                    if c.is_ascii() && !c.is_control() { return c.to_ascii_lowercase(); }
                    if byte == b'\r' || byte == b'\n' || byte == b' ' { return ' '; }
                }
            }
            if let Ok(q) = SCANCODE_QUEUE.try_get() {
                if let Some(scancode) = q.pop() {
                    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
                        if let Some(DecodedKey::Unicode(c)) = keyboard.process_keyevent(key_event) {
                            if c.is_ascii() && !c.is_control() { return c.to_ascii_lowercase(); }
                            if c == '\r' || c == '\n' || c == ' ' { return ' '; }
                        }
                    }
                }
            }
            x86_64::instructions::interrupts::enable_and_hlt();
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
            crate::process::set_current_cwd("/");
            return;
        }

        let target = args[0];

        // Handle special cases
        if target == "/" {
            self.current_dir = String::from("/");
            crate::process::set_current_cwd("/");
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
            crate::process::set_current_cwd(&self.current_dir);
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
            self.current_dir = new_path.clone();
            crate::process::set_current_cwd(&new_path);
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

    fn cmd_spawn(&mut self, args: &[&str]) {
        if args.is_empty() || args[0] == "test" {
            // Spawn preemptive multitasking test
            crate::userspace::spawn_preemptive_test();
        } else {
            println!("Usage: spawn test");
            println!("  test  — Run two user processes with preemptive scheduling");
        }
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

        use crate::fs::vfs::{VfsContext, VfsFileType};

        for filename in args {
            let path = if filename.starts_with('/') {
                String::from(*filename)
            } else if self.current_dir == "/" {
                format!("/{}", filename)
            } else {
                format!("{}/{}", self.current_dir, filename)
            };

            let info = match VfsContext::stat(&path) {
                Ok(info) => info,
                Err(_) => {
                    println!("stat: cannot stat '{}': No such file or directory", filename);
                    continue;
                }
            };

            let file_type = match info.file_type {
                VfsFileType::Directory => "directory",
                VfsFileType::Regular => "regular file",
                VfsFileType::Symlink => "symbolic link",
                VfsFileType::CharDevice => "character device",
                VfsFileType::BlockDevice => "block device",
            };

            let mode_str = Self::format_mode(info.file_type, info.mode);
            let blocks = (info.size + 511) / 512;

            println!("  File: {}", filename);
            // Show symlink target if applicable
            if info.file_type == VfsFileType::Symlink {
                if let Ok(target) = VfsContext::readlink(&path) {
                    println!("  File: {} -> {}", filename, target);
                }
            }
            println!("  Size: {:<15} Blocks: {:<10} {}", info.size, blocks, file_type);
            println!("Access: ({:04o}/{})  Uid: ({:>5}/    root)   Gid: ({:>5}/    root)",
                info.mode, mode_str, info.uid, info.gid);

            // Show timestamps
            let dt = crate::drivers::rtc::read_datetime();
            let mut buf = [0u8; 32];
            let len = dt.format(&mut buf);
            let time_str = core::str::from_utf8(&buf[..len]).unwrap_or("unknown");
            if info.mtime > 0 {
                println!("Modify: boot+{}s (RTC: {})", info.mtime, time_str);
            } else {
                println!("Modify: {}", time_str);
            }
        }
    }

    /// Format mode bits as rwxrwxrwx string
    fn format_mode(file_type: crate::fs::vfs::VfsFileType, mode: u16) -> String {
        use crate::fs::vfs::VfsFileType;
        let prefix = match file_type {
            VfsFileType::Directory => 'd',
            VfsFileType::Symlink => 'l',
            VfsFileType::CharDevice => 'c',
            VfsFileType::BlockDevice => 'b',
            VfsFileType::Regular => '-',
        };
        let mut s = String::with_capacity(10);
        s.push(prefix);
        s.push(if mode & 0o400 != 0 { 'r' } else { '-' });
        s.push(if mode & 0o200 != 0 { 'w' } else { '-' });
        s.push(if mode & 0o100 != 0 { 'x' } else { '-' });
        s.push(if mode & 0o040 != 0 { 'r' } else { '-' });
        s.push(if mode & 0o020 != 0 { 'w' } else { '-' });
        s.push(if mode & 0o010 != 0 { 'x' } else { '-' });
        s.push(if mode & 0o004 != 0 { 'r' } else { '-' });
        s.push(if mode & 0o002 != 0 { 'w' } else { '-' });
        s.push(if mode & 0o001 != 0 { 'x' } else { '-' });
        s
    }

    /// ln - create symbolic links
    fn cmd_ln(&self, args: &[&str]) {
        use crate::fs::vfs::VfsContext;

        // Parse -s flag for symbolic link
        if args.len() < 2 || args[0] != "-s" {
            println!("Usage: ln -s <target> <link_name>");
            return;
        }
        if args.len() < 3 {
            println!("Usage: ln -s <target> <link_name>");
            return;
        }

        let target = args[1];
        let link_name = args[2];

        let link_path = if link_name.starts_with('/') {
            String::from(link_name)
        } else if self.current_dir == "/" {
            format!("/{}", link_name)
        } else {
            format!("{}/{}", self.current_dir, link_name)
        };

        match VfsContext::symlink(&link_path, target) {
            Ok(()) => {}
            Err(e) => println!("ln: failed to create symlink: {:?}", e),
        }
    }

    /// chmod - change file mode bits
    fn cmd_chmod(&self, args: &[&str]) {
        use crate::fs::vfs::VfsContext;

        if args.len() < 2 {
            println!("Usage: chmod <mode> <file>");
            return;
        }

        let mode_str = args[0];
        let filename = args[1];

        // Parse octal mode
        let mode = match u16::from_str_radix(mode_str, 8) {
            Ok(m) => m,
            Err(_) => {
                println!("chmod: invalid mode '{}'", mode_str);
                return;
            }
        };

        let path = if filename.starts_with('/') {
            String::from(filename)
        } else if self.current_dir == "/" {
            format!("/{}", filename)
        } else {
            format!("{}/{}", self.current_dir, filename)
        };

        match VfsContext::chmod(&path, mode) {
            Ok(()) => {}
            Err(e) => println!("chmod: cannot change permissions of '{}': {:?}", filename, e),
        }
    }

    /// chown - change file owner and group
    fn cmd_chown(&self, args: &[&str]) {
        use crate::fs::vfs::VfsContext;

        if args.len() < 2 {
            println!("Usage: chown <owner[:group]> <file>");
            return;
        }

        let owner_str = args[0];
        let filename = args[1];

        // Parse owner:group
        let (uid, gid) = if let Some(colon_pos) = owner_str.find(':') {
            let uid: u32 = owner_str[..colon_pos].parse().unwrap_or(0);
            let gid: u32 = owner_str[colon_pos + 1..].parse().unwrap_or(0);
            (uid, gid)
        } else {
            let uid: u32 = owner_str.parse().unwrap_or(0);
            (uid, 0)
        };

        let path = if filename.starts_with('/') {
            String::from(filename)
        } else if self.current_dir == "/" {
            format!("/{}", filename)
        } else {
            format!("{}/{}", self.current_dir, filename)
        };

        match VfsContext::chown(&path, uid, gid) {
            Ok(()) => {}
            Err(e) => println!("chown: cannot change ownership of '{}': {:?}", filename, e),
        }
    }

    /// readlink - print the target of a symbolic link
    fn cmd_readlink(&self, args: &[&str]) {
        use crate::fs::vfs::VfsContext;

        if args.is_empty() {
            println!("Usage: readlink <link>");
            return;
        }

        for filename in args {
            let path = if filename.starts_with('/') {
                String::from(*filename)
            } else if self.current_dir == "/" {
                format!("/{}", filename)
            } else {
                format!("{}/{}", self.current_dir, filename)
            };

            match VfsContext::readlink(&path) {
                Ok(target) => println!("{}", target),
                Err(_) => println!("readlink: {}: Invalid argument", filename),
            }
        }
    }

    /// Run a command in the background
    fn run_background(&mut self, command: &str) {
        use crate::process::PROCESS_MANAGER;

        // Parse the command to get the name for the job
        let cmd_name = command.split_whitespace().next().unwrap_or("unknown");

        // For sleep command: spawn as a real background process
        if cmd_name == "sleep" {
            let parts: Vec<&str> = command.split_whitespace().collect();
            if parts.len() >= 2 {
                let seconds: u64 = parts[1].parse().unwrap_or(1);
                let ticks = seconds * 18; // ~18 ticks per second

                // Create a kernel process for the sleep
                let pid = {
                    let mut pm = PROCESS_MANAGER.lock();
                    let pid = pm.create_process(0, 4096);
                    // Mark it as blocked immediately (it's sleeping)
                    if let Some(proc) = pm.get_process_mut(pid) {
                        proc.state = crate::process::ProcessState::Blocked;
                    }
                    pid
                };

                // Register a wake-up timer
                crate::process::scheduler::SCHEDULER.lock().sleep_pid(pid, ticks);

                let job_id = self.next_job_id;
                self.next_job_id += 1;
                self.jobs.push(Job {
                    id: job_id,
                    pid,
                    command: command.to_string(),
                    status: JobStatus::Running,
                });
                println!("[{}] {}", job_id, pid);
                return;
            }
        }

        // For other commands: run immediately but report as job
        let job_id = self.next_job_id;
        self.next_job_id += 1;

        // Execute command synchronously but wrap in job tracking
        println!("[{}] (running in foreground — true background requires process support)", job_id);

        // Execute the command
        self.buffer = command.to_string();
        self.cursor_pos = self.buffer.len();
        // Create a mini-shell to run the command without the & flag
        let parts: Vec<&str> = command.split_whitespace().collect();
        if !parts.is_empty() {
            let cmd = parts[0];
            let expanded = self.expand_globs(&parts[1..]);
            let expanded_refs: Vec<&str> = expanded.iter().map(|s| s.as_str()).collect();
            let args = &expanded_refs[..];
            // Run the command directly
            match cmd {
                "sleep" => self.cmd_sleep(args),
                _ => {
                    // For unrecognized commands, just note it
                    println!("{}: background execution not yet supported for this command", cmd);
                }
            }
        }

        self.jobs.push(Job {
            id: job_id,
            pid: 0,
            command: command.to_string(),
            status: JobStatus::Done,
        });
        println!("[{}] Done                    {}", job_id, command);
    }

    /// jobs - list background jobs
    fn cmd_jobs(&mut self) {
        // Update job statuses
        self.update_job_statuses();

        if self.jobs.is_empty() {
            return; // No output when no jobs, matching bash behavior
        }

        for job in &self.jobs {
            println!("[{}]  {}                 {}", job.id, job.status_str(), job.command);
        }

        // Clean up done jobs
        self.jobs.retain(|j| j.status != JobStatus::Done);
    }

    /// fg - bring a background job to foreground
    fn cmd_fg(&mut self, args: &[&str]) {
        let job_id = if args.is_empty() {
            // Get the most recent job
            match self.jobs.last() {
                Some(job) => job.id,
                None => {
                    println!("fg: no current job");
                    return;
                }
            }
        } else {
            // Parse job ID (strip % prefix if present)
            let id_str = args[0].trim_start_matches('%');
            match id_str.parse::<usize>() {
                Ok(id) => id,
                Err(_) => {
                    println!("fg: {}: no such job", args[0]);
                    return;
                }
            }
        };

        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            println!("{}", job.command);
            if job.pid > 0 && job.status == JobStatus::Running {
                // Poll until process is done
                loop {
                    if crate::process::scheduler::SCHEDULER.lock().is_pid_done(job.pid) {
                        break;
                    }
                    // Yield to let the scheduler tick
                    x86_64::instructions::hlt();
                }
            }
            job.status = JobStatus::Done;
        } else {
            println!("fg: %{}: no such job", job_id);
        }
    }

    /// bg - continue a stopped job in the background
    fn cmd_bg(&mut self, args: &[&str]) {
        let job_id = if args.is_empty() {
            match self.jobs.last() {
                Some(job) => job.id,
                None => {
                    println!("bg: no current job");
                    return;
                }
            }
        } else {
            let id_str = args[0].trim_start_matches('%');
            match id_str.parse::<usize>() {
                Ok(id) => id,
                Err(_) => {
                    println!("bg: {}: no such job", args[0]);
                    return;
                }
            }
        };

        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            if job.status == JobStatus::Stopped {
                job.status = JobStatus::Running;
                // Resume the process
                if job.pid > 0 {
                    let mut pm = crate::process::PROCESS_MANAGER.lock();
                    if let Some(proc) = pm.get_process_mut(job.pid) {
                        proc.state = crate::process::ProcessState::Ready;
                    }
                }
                println!("[{}]  {} &", job.id, job.command);
            } else {
                println!("bg: job {} already running", job_id);
            }
        } else {
            println!("bg: %{}: no such job", job_id);
        }
    }

    /// Update job statuses from process manager
    fn update_job_statuses(&mut self) {
        let pm = crate::process::PROCESS_MANAGER.lock();
        for job in &mut self.jobs {
            if job.pid > 0 {
                if let Some(proc) = pm.processes().iter().find(|p| p.pid == job.pid) {
                    match proc.state {
                        crate::process::ProcessState::Terminated
                        | crate::process::ProcessState::Zombie => {
                            if job.status != JobStatus::Done {
                                job.status = JobStatus::Done;
                            }
                        }
                        crate::process::ProcessState::Blocked => {
                            // Still running (sleeping)
                        }
                        _ => {}
                    }
                } else {
                    // Process no longer exists
                    job.status = JobStatus::Done;
                }
            }
        }
    }

    /// seq - print a sequence of numbers
    fn cmd_seq(&self, args: &[&str]) {
        let (start, end, step) = match args.len() {
            1 => {
                let end: i64 = args[0].parse().unwrap_or(0);
                (1i64, end, 1i64)
            }
            2 => {
                let start: i64 = args[0].parse().unwrap_or(0);
                let end: i64 = args[1].parse().unwrap_or(0);
                (start, end, 1)
            }
            3 => {
                let start: i64 = args[0].parse().unwrap_or(0);
                let step: i64 = args[1].parse().unwrap_or(1);
                let end: i64 = args[2].parse().unwrap_or(0);
                (start, end, step)
            }
            _ => {
                println!("Usage: seq [start] end");
                return;
            }
        };

        if step == 0 {
            println!("seq: zero step");
            return;
        }

        let mut i = start;
        while (step > 0 && i <= end) || (step < 0 && i >= end) {
            println!("{}", i);
            i += step;
        }
    }

    /// test - evaluate conditional expressions (sets $? = 0 on true, 1 on false)
    fn cmd_test(&mut self, args: &[&str]) {
        use crate::fs::vfs::VfsContext;

        // Strip trailing "]" if invoked as "["
        let args = if !args.is_empty() && args[args.len() - 1] == "]" {
            &args[..args.len() - 1]
        } else {
            args
        };

        let result = match args {
            ["-f", path] => VfsContext::exists(path) && !VfsContext::is_directory(path),
            ["-d", path] => VfsContext::is_directory(path),
            ["-e", path] => VfsContext::exists(path),
            ["-z", s] => s.is_empty(),
            ["-n", s] => !s.is_empty(),
            [a, "=", b] | [a, "==", b] => a == b,
            [a, "!=", b] => a != b,
            [a, "-eq", b] => a.parse::<i64>().ok() == b.parse::<i64>().ok(),
            [a, "-ne", b] => a.parse::<i64>().ok() != b.parse::<i64>().ok(),
            [a, "-lt", b] => a.parse::<i64>().unwrap_or(0) < b.parse::<i64>().unwrap_or(0),
            [a, "-gt", b] => a.parse::<i64>().unwrap_or(0) > b.parse::<i64>().unwrap_or(0),
            _ => {
                println!("test: unrecognized expression");
                false
            }
        };

        self.last_exit = if result { 0 } else { 1 };
    }

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

    /// Print current user name (whoami)
    fn cmd_whoami(&self) {
        let creds = crate::users::CURRENT_CREDS.lock();
        println!("{}", creds.username);
    }

    /// Print uid/gid/groups (id)
    fn cmd_id(&self) {
        let creds = crate::users::CURRENT_CREDS.lock();
        let db = crate::users::USER_DB.lock();
        let uname = creds.username.clone();
        let primary_group_name = db.get_group_by_gid(creds.gid)
            .map(|g| g.name.clone())
            .unwrap_or_else(|| String::from("unknown"));

        // Build supplementary group list
        let mut group_list = String::new();
        let groups = db.user_groups(&uname);
        for (i, g) in groups.iter().enumerate() {
            if i > 0 {
                group_list.push(',');
            }
            group_list.push_str(&format!("{}({})", g.gid, g.name));
        }
        if group_list.is_empty() {
            group_list = format!("{}({})", creds.gid, primary_group_name);
        }

        println!(
            "uid={}({}) gid={}({}) groups={}",
            creds.uid, uname, creds.gid, primary_group_name, group_list
        );
    }

    /// sudo: run a command as another user.
    /// Reads /etc/sudoers; format: `USER ALL=(ALL) [NOPASSWD]`
    /// Becomes target user, dispatches the command, then restores.
    fn cmd_sudo(&mut self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: sudo [-u USER] <command> [args...]");
            return;
        }
        let mut target_user = String::from("root");
        let mut cmd_start = 0usize;
        if args[0] == "-u" {
            if args.len() < 3 { println!("sudo: -u needs USER and CMD"); return; }
            target_user = String::from(args[1]);
            cmd_start = 2;
        }
        let cmd = args[cmd_start];
        let cmd_args = &args[cmd_start + 1..];

        // Sudoers check.
        let sudoers = match crate::fs::vfs::VfsContext::read("/etc/sudoers") {
            Ok(d) => d,
            Err(_) => { println!("sudo: /etc/sudoers missing"); return; }
        };
        let sudoers = core::str::from_utf8(&sudoers).unwrap_or("");
        let caller = crate::users::CURRENT_CREDS.lock().username.clone();
        let mut matched_no_passwd = false;
        let mut matched = false;
        for raw in sudoers.lines() {
            let line = match raw.find('#') { Some(i) => &raw[..i], None => raw };
            let line = line.trim();
            if line.is_empty() { continue; }
            let mut parts = line.split_whitespace();
            let user = match parts.next() { Some(u) => u, None => continue };
            // `user ALL=(ALL) [NOPASSWD]`
            if user != caller && user != "ALL" { continue; }
            matched = true;
            if line.contains("NOPASSWD") { matched_no_passwd = true; }
        }
        if !matched {
            println!("sudo: {} is not in the sudoers file. This incident will be reported.", caller);
            crate::syslog::log(
                crate::syslog::Facility::Authpriv,
                crate::syslog::Severity::Warning,
                "sudo",
                alloc::format!("denied: {} not in sudoers", caller),
            );
            return;
        }
        if !matched_no_passwd {
            print!("[sudo] password for {}: ", caller);
            let pw = self.read_password();
            let db = crate::users::USER_DB.lock();
            if !db.check_password(&caller, &pw) {
                println!("\nSorry, try again.");
                return;
            }
        }

        // Switch creds.
        let target = {
            let db = crate::users::USER_DB.lock();
            match db.get_user(&target_user) {
                Some(u) => u.clone(),
                None => { println!("sudo: unknown user '{}'", target_user); return; }
            }
        };
        let prev = {
            let creds = crate::users::CURRENT_CREDS.lock();
            (creds.uid, creds.gid, creds.euid, creds.egid, creds.username.clone())
        };
        {
            let mut creds = crate::users::CURRENT_CREDS.lock();
            creds.uid = target.uid;
            creds.gid = target.gid;
            creds.euid = target.uid;
            creds.egid = target.gid;
            creds.username = target.name.clone();
        }
        crate::capability::reseed_for_uid(target.uid);
        crate::syslog::log(
            crate::syslog::Facility::Authpriv,
            crate::syslog::Severity::Notice,
            "sudo",
            alloc::format!("{} -> {} : {} {}", caller, target.name, cmd, cmd_args.join(" ")),
        );

        // Reconstruct the command line and re-enter the dispatcher.
        let mut full = String::from(cmd);
        for a in cmd_args { full.push(' '); full.push_str(a); }
        self.buffer = full;
        self.cursor_pos = self.buffer.len();
        self.execute();

        // Restore previous credentials.
        {
            let mut creds = crate::users::CURRENT_CREDS.lock();
            creds.uid = prev.0;
            creds.gid = prev.1;
            creds.euid = prev.2;
            creds.egid = prev.3;
            creds.username = prev.4;
        }
        crate::capability::reseed_for_uid(prev.2);
    }

    /// Switch user (su)
    fn cmd_su(&mut self, args: &[&str]) {
        let target = if args.is_empty() { "root" } else { args[0] };
        let db = crate::users::USER_DB.lock();
        let user = match db.get_user(target) {
            Some(u) => u.clone(),
            None => {
                println!("su: user {} does not exist", target);
                return;
            }
        };
        drop(db);

        // If we're already root, no password needed
        let need_password = {
            let creds = crate::users::CURRENT_CREDS.lock();
            creds.uid != 0
        };

        if need_password {
            // Read password from input (echo off would be nicer, but for now just prompt)
            print!("Password: ");
            // Read a line from stdin via VFS
            let pw = self.read_password();
            let db = crate::users::USER_DB.lock();
            if !db.check_password(target, &pw) {
                println!("\nsu: Authentication failure");
                return;
            }
        }

        // Switch credentials
        let mut creds = crate::users::CURRENT_CREDS.lock();
        creds.uid = user.uid;
        creds.gid = user.gid;
        creds.euid = user.uid;
        creds.egid = user.gid;
        creds.username = user.name.clone();
        drop(creds);
        crate::capability::reseed_for_uid(user.uid);

        // Move to user's home directory if it exists
        if !user.home.is_empty() {
            self.current_dir = user.home.clone();
        }

        println!("Switched to user '{}' (uid={})", user.name, user.uid);
    }

    /// Change password (passwd)
    fn cmd_passwd(&mut self, args: &[&str]) {
        let target = if args.is_empty() {
            crate::users::CURRENT_CREDS.lock().username.clone()
        } else {
            String::from(args[0])
        };

        let creds = crate::users::CURRENT_CREDS.lock();
        let current_user = creds.username.clone();
        let is_root = creds.uid == 0;
        drop(creds);

        // Only root can change other users' passwords
        if target != current_user && !is_root {
            println!("passwd: You may not change passwords for other users.");
            return;
        }

        // Verify the current password if not root
        if !is_root {
            print!("Current password: ");
            let cur = self.read_password();
            let db = crate::users::USER_DB.lock();
            if !db.check_password(&target, &cur) {
                println!("\npasswd: Authentication failure");
                return;
            }
        }

        print!("New password: ");
        let pw1 = self.read_password();
        if pw1.len() < 4 {
            println!("\npasswd: password too short (minimum 4 characters)");
            return;
        }
        print!("Retype new password: ");
        let pw2 = self.read_password();
        if pw1 != pw2 {
            println!("\npasswd: passwords do not match");
            return;
        }

        let mut db = crate::users::USER_DB.lock();
        match db.set_password(&target, &pw1) {
            Ok(_) => {
                // Persist to /etc/shadow style — for now just regenerate /etc/passwd
                println!("\npasswd: password updated successfully");
                let _ = self.persist_user_db(&db);
            }
            Err(e) => println!("\npasswd: {}", e),
        }
    }

    /// Add a new user (useradd)
    fn cmd_useradd(&mut self, args: &[&str]) {
        let creds = crate::users::CURRENT_CREDS.lock();
        if creds.uid != 0 {
            println!("useradd: only root may add users");
            return;
        }
        drop(creds);

        if args.is_empty() {
            println!("Usage: useradd [-u UID] [-g GID] [-d HOME] [-s SHELL] <name>");
            return;
        }

        let mut uid: Option<u32> = None;
        let mut gid: u32 = 100; // default: users group
        let mut home: Option<String> = None;
        let mut shell: String = String::from("/bin/sh");
        let mut name: Option<&str> = None;

        let mut i = 0;
        while i < args.len() {
            match args[i] {
                "-u" if i + 1 < args.len() => {
                    uid = args[i + 1].parse().ok();
                    i += 2;
                }
                "-g" if i + 1 < args.len() => {
                    gid = args[i + 1].parse().unwrap_or(100);
                    i += 2;
                }
                "-d" if i + 1 < args.len() => {
                    home = Some(String::from(args[i + 1]));
                    i += 2;
                }
                "-s" if i + 1 < args.len() => {
                    shell = String::from(args[i + 1]);
                    i += 2;
                }
                arg if !arg.starts_with('-') => {
                    name = Some(arg);
                    i += 1;
                }
                _ => i += 1,
            }
        }

        let name = match name {
            Some(n) => n,
            None => {
                println!("useradd: missing username");
                return;
            }
        };

        // Auto-assign UID if not given
        let mut db = crate::users::USER_DB.lock();
        let uid = uid.unwrap_or_else(|| {
            let mut next = 1000u32;
            for u in db.users() {
                if u.uid >= next && u.uid < 65000 {
                    next = u.uid + 1;
                }
            }
            next
        });

        let home = home.unwrap_or_else(|| format!("/home/{}", name));

        let user = crate::users::User {
            name: String::from(name),
            password_hash: String::from("*"), // locked initially
            uid,
            gid,
            comment: String::new(),
            home: home.clone(),
            shell,
        };

        match db.add_user(user) {
            Ok(_) => {
                println!("useradd: created user '{}' (uid={}, gid={})", name, uid, gid);
                let _ = self.persist_user_db(&db);
                drop(db);
                // Create home directory
                if !home.is_empty() && home != "/" {
                    use crate::fs::vfs::VfsContext;
                    let _ = VfsContext::mkdir(&home);
                }
            }
            Err(e) => println!("useradd: {}", e),
        }
    }

    /// Read a password line from stdin synchronously, without echoing characters.
    ///
    /// Polls the serial and scancode queues directly so this works even though
    /// the rest of the shell input loop is async.
    fn read_password(&mut self) -> String {
        use crate::task::keyboard::{SCANCODE_QUEUE, SERIAL_QUEUE};
        use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};

        let mut keyboard = Keyboard::new(
            ScancodeSet1::new(),
            layouts::Us104Key,
            HandleControl::MapLettersToUnicode,
        );

        let mut buf = String::new();
        loop {
            let mut got_char: Option<char> = None;

            // Serial queue first (already decoded)
            if let Ok(q) = SERIAL_QUEUE.try_get() {
                if let Some(byte) = q.pop() {
                    got_char = match byte {
                        b'\r' | b'\n' => Some('\n'),
                        0x7F | 0x08 => Some('\u{0008}'),
                        b if (0x20..=0x7E).contains(&b) => Some(b as char),
                        _ => None,
                    };
                }
            }

            // Scancode queue
            if got_char.is_none() {
                if let Ok(q) = SCANCODE_QUEUE.try_get() {
                    if let Some(scancode) = q.pop() {
                        if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
                            if let Some(key) = keyboard.process_keyevent(key_event) {
                                if let DecodedKey::Unicode(c) = key {
                                    got_char = Some(c);
                                }
                            }
                        }
                    }
                }
            }

            if let Some(c) = got_char {
                if c == '\n' || c == '\r' {
                    println!();
                    return buf;
                } else if c == '\u{0008}' || c == '\u{007F}' {
                    buf.pop();
                } else if c.is_ascii() && !c.is_control() {
                    buf.push(c);
                    // Echo a star to give feedback without revealing the password
                    print!("*");
                }
            } else {
                x86_64::instructions::interrupts::enable_and_hlt();
            }
        }
    }

    /// Persist user database to /etc/passwd and /etc/group
    fn persist_user_db(&self, db: &crate::users::UserDb) -> Result<(), ()> {
        use crate::fs::vfs::VfsContext;
        let _ = VfsContext::write("/etc/passwd", db.to_passwd_string().as_bytes().to_vec());
        let _ = VfsContext::write("/etc/group", db.to_group_string().as_bytes().to_vec());
        Ok(())
    }

    /// service / systemctl: manage init services
    fn cmd_service(&mut self, args: &[&str]) {
        // service <name> <start|stop|restart|status>
        // systemctl <verb> <name>  — accept either ordering
        if args.is_empty() {
            self.print_service_list();
            return;
        }

        // systemctl verbs come first
        let verbs = ["start", "stop", "restart", "status", "list", "enable", "disable"];
        let (verb, name): (&str, Option<&str>) = if verbs.contains(&args[0]) {
            (args[0], args.get(1).copied())
        } else if args.len() >= 2 && verbs.contains(&args[1]) {
            (args[1], Some(args[0]))
        } else {
            // Bare name -> status
            ("status", Some(args[0]))
        };

        match verb {
            "list" => self.print_service_list(),
            "start" => match name {
                Some(n) => match crate::init::INIT.lock().start(n) {
                    Ok(_) => println!("Started {}", n),
                    Err(e) => println!("service: {}: {}", n, e),
                },
                None => println!("service: missing service name"),
            },
            "stop" => match name {
                Some(n) => match crate::init::INIT.lock().stop(n) {
                    Ok(_) => println!("Stopped {}", n),
                    Err(e) => println!("service: {}: {}", n, e),
                },
                None => println!("service: missing service name"),
            },
            "restart" => match name {
                Some(n) => match crate::init::INIT.lock().restart(n) {
                    Ok(_) => println!("Restarted {}", n),
                    Err(e) => println!("service: {}: {}", n, e),
                },
                None => println!("service: missing service name"),
            },
            "status" => match name {
                Some(n) => self.print_service_status(n),
                None => self.print_service_list(),
            },
            "enable" => match name {
                Some(n) => {
                    let mut init = crate::init::INIT.lock();
                    let level = init.runlevel() as u8;
                    match init.get_mut(n) {
                        Some(svc) => {
                            // Add this runlevel + the standard multi-user
                            // levels so a freshly enabled service comes up
                            // on the next runlevel transition.
                            svc.runlevels |= 1 << level;
                            svc.runlevels |= (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5);
                            println!("Enabled {} (runlevels mask {:#04x})", n, svc.runlevels);
                        }
                        None => println!("service: {}: not found", n),
                    }
                }
                None => println!("service: missing service name"),
            },
            "disable" => match name {
                Some(n) => {
                    let mut init = crate::init::INIT.lock();
                    match init.get_mut(n) {
                        Some(svc) => {
                            svc.runlevels = 0;
                            println!("Disabled {} (runlevels mask 0x00)", n);
                        }
                        None => println!("service: {}: not found", n),
                    }
                    // Stop it now if it's currently running.
                    let _ = init.stop(n);
                }
                None => println!("service: missing service name"),
            },
            _ => println!("service: unknown verb '{}'", verb),
        }
    }

    fn print_service_list(&self) {
        let init = crate::init::INIT.lock();
        let tick = crate::task::timer::current_ticks();
        println!("UNIT          STATE      DESCRIPTION");
        for svc in init.services() {
            println!("{}", svc.status_line(tick));
        }
        println!("---");
        println!("{} of {} services running, runlevel {}",
            init.running_count(), init.total_count(), init.runlevel().as_str());
    }

    fn print_service_status(&self, name: &str) {
        let init = crate::init::INIT.lock();
        let svc = match init.get(name) {
            Some(s) => s,
            None => {
                println!("service: {}: not found", name);
                return;
            }
        };
        let tick = crate::task::timer::current_ticks();
        let dur = tick.saturating_sub(svc.state_since_tick) / 100;
        println!("● {} - {}", svc.name, svc.description);
        println!("   State: {} (since {}s ago)", svc.state.as_str(), dur);
        println!("   Exec: {}", svc.exec);
        if !svc.requires.is_empty() {
            print!("   Requires:");
            for d in &svc.requires {
                print!(" {}", d);
            }
            println!();
        }
        println!("   Restart: {:?}", svc.restart);
        println!("   Starts: {}", svc.start_count);
        if let Some(pid) = svc.pid {
            println!("   PID: {}", pid);
        }
    }

    /// runlevel: print current and previous runlevel
    fn cmd_runlevel(&self) {
        let level = crate::init::INIT.lock().runlevel();
        println!("N {}", level as u8);
        println!("({})", level.as_str());
    }

    /// nslookup / host <name>: resolve a hostname via DNS (with /etc/hosts fallback).
    fn cmd_nslookup(&self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: nslookup <name>");
            return;
        }
        let name = args[0];

        // Show resolver configuration for diagnostics.
        let resolvers = crate::net::dns::read_resolvers();
        if resolvers.is_empty() {
            println!("Server:  /etc/hosts (no nameservers configured)");
            println!("Address: 127.0.0.1#0");
        } else {
            println!("Server:  {}", resolvers[0]);
            println!("Address: {}#{}", resolvers[0], 53);
        }
        println!();

        match crate::net::dns::resolve(name) {
            Some(addr) => {
                println!("Name:    {}", name);
                println!("Address: {}", addr);
            }
            None => {
                println!("** server can't find {}: NXDOMAIN", name);
            }
        }
    }

    /// dns-cache: show / flush the DNS cache.
    ///   dns-cache         : show count
    ///   dns-cache flush   : drop all entries
    fn cmd_dns_cache(&self, args: &[&str]) {
        if args.first() == Some(&"flush") {
            crate::net::dns::flush_cache();
            println!("DNS cache flushed");
            return;
        }
        println!("DNS cache: {} entries", crate::net::dns::cache_size());
    }

    /// getent <database> <key>: query system databases.  Supported: hosts,
    /// passwd, group.
    fn cmd_getent(&self, args: &[&str]) {
        if args.len() < 1 {
            println!("Usage: getent <database> [key]");
            println!("  databases: hosts, passwd, group");
            return;
        }
        match args[0] {
            "hosts" => {
                if args.len() >= 2 {
                    let results = crate::net::resolver::resolve_all(args[1]);
                    if results.is_empty() {
                        // exit 2 in real getent; we just print nothing
                        return;
                    }
                    for (addr, canonical) in results {
                        println!("{:<15} {}", alloc::format!("{}", addr), canonical);
                    }
                } else if let Ok(data) = crate::fs::vfs::VfsContext::read("/etc/hosts") {
                    if let Ok(s) = core::str::from_utf8(&data) {
                        print!("{}", s);
                    }
                }
            }
            "passwd" => {
                let db = crate::users::USER_DB.lock();
                let text = db.to_passwd_string();
                if args.len() >= 2 {
                    for line in text.lines() {
                        if line.starts_with(args[1]) && line.as_bytes().get(args[1].len()) == Some(&b':') {
                            println!("{}", line);
                        }
                    }
                } else {
                    print!("{}", text);
                }
            }
            "group" => {
                let db = crate::users::USER_DB.lock();
                print!("{}", db.to_group_string());
            }
            other => println!("getent: unknown database '{}'", other),
        }
    }

    /// crontab: list, add, or remove cron jobs.
    ///   crontab -l           : list
    ///   crontab -a <secs> <command>
    ///   crontab -r <index>   : remove by 1-based index
    ///   crontab -R           : reload from /etc/crontab
    fn cmd_crontab(&self, args: &[&str]) {
        if args.is_empty() || args[0] == "-l" {
            let table = crate::cron::CRONTAB.lock();
            if table.list().is_empty() {
                println!("(empty crontab)");
                return;
            }
            println!("# IDX  INTERVAL_S  RUNS  COMMAND");
            for (i, job) in table.list().iter().enumerate() {
                println!("  {:<3}  {:<10}  {:<4}  {}",
                    i + 1, job.interval_secs, job.run_count, job.command);
            }
            return;
        }

        match args[0] {
            "-a" if args.len() >= 3 => {
                let secs: u64 = match args[1].parse() {
                    Ok(n) => n,
                    Err(_) => { println!("crontab: invalid interval '{}'", args[1]); return; }
                };
                let cmd = args[2..].join(" ");
                crate::cron::CRONTAB.lock().add(secs, &cmd);
                crate::cron::save();
                println!("crontab: added job (every {}s)", secs);
            }
            "-r" if args.len() >= 2 => {
                let idx: usize = match args[1].parse() {
                    Ok(n) => n,
                    Err(_) => { println!("crontab: invalid index '{}'", args[1]); return; }
                };
                let removed = crate::cron::CRONTAB.lock().remove(idx);
                if removed {
                    crate::cron::save();
                    println!("crontab: removed job {}", idx);
                } else {
                    println!("crontab: no job at index {}", idx);
                }
            }
            "-R" => {
                crate::cron::reload();
                println!("crontab: reloaded from /etc/crontab");
            }
            other => {
                println!("Usage: crontab [-l|-a SECS CMD|-r IDX|-R]");
                println!("crontab: unknown option '{}'", other);
            }
        }
    }

    /// logger: write a syslog entry. Usage: logger [-p facility.severity] [-t tag] message
    fn cmd_logger(&self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: logger [-p fac.sev] [-t tag] <message>");
            return;
        }

        let mut facility = crate::syslog::Facility::User;
        let mut severity = crate::syslog::Severity::Info;
        let mut tag = String::from("user");
        let mut msg_parts: Vec<&str> = Vec::new();

        let mut i = 0;
        while i < args.len() {
            match args[i] {
                "-p" if i + 1 < args.len() => {
                    let spec = args[i + 1];
                    if let Some(dot) = spec.find('.') {
                        if let Some(f) = crate::syslog::Facility::parse(&spec[..dot]) {
                            facility = f;
                        }
                        if let Some(s) = crate::syslog::Severity::parse(&spec[dot + 1..]) {
                            severity = s;
                        }
                    }
                    i += 2;
                }
                "-t" if i + 1 < args.len() => {
                    tag = String::from(args[i + 1]);
                    i += 2;
                }
                other => {
                    msg_parts.push(other);
                    i += 1;
                }
            }
        }

        let message = msg_parts.join(" ");
        crate::syslog::log(facility, severity, &tag, message);
    }

    /// syslog: dump in-memory syslog ring (most recent N entries)
    fn cmd_syslog(&self, args: &[&str]) {
        let count: usize = args.first()
            .and_then(|s| s.parse().ok())
            .unwrap_or(50);
        let entries = crate::syslog::SYSLOG.lock().snapshot();
        let start = entries.len().saturating_sub(count);
        let host = self.hostname.clone();
        for e in entries.iter().skip(start) {
            println!("{}", e.format_line(&host));
        }
        if entries.is_empty() {
            println!("syslog: empty");
        }
    }

    /// getcap: print the running process's effective+permitted set, or
    /// the per-uid seed for a given uid.
    ///   getcap            -> running process effective set
    ///   getcap UID        -> per-uid seed for UID
    fn cmd_getcap(&self, args: &[&str]) {
        if args.is_empty() {
            // Running process's view.
            let pm = crate::process::PROCESS_MANAGER.lock();
            let (eff, perm) = match pm.current_process() {
                Some(p) => (p.cap_effective, p.cap_permitted),
                None => {
                    drop(pm);
                    let raw = crate::capability::current_effective_raw();
                    (raw, raw)
                }
            };
            println!("(current process)");
            println!("  effective:  0x{:016x}", eff);
            println!("  permitted:  0x{:016x}", perm);
            for c in crate::capability::Capability::all() {
                let bit = 1u64 << (*c as u32);
                if eff & bit != 0 {
                    println!("  + {}", c.name());
                }
            }
            return;
        }

        let uid: u32 = match args[0].parse() {
            Ok(n) => n,
            Err(_) => { println!("getcap: bad uid"); return; }
        };
        let caps = crate::capability::CAPS.lock().for_uid(uid);
        println!("uid={} seed=0x{:016x}", uid, caps.raw());
        for c in crate::capability::Capability::all() {
            if caps.has(*c) {
                println!("  + {}", c.name());
            }
        }
    }

    /// setcap: grant or drop a capability for a uid (root only).
    ///   setcap UID +CAP   # grant
    ///   setcap UID -CAP   # drop
    fn cmd_setcap(&self, args: &[&str]) {
        if !crate::capability::check(crate::capability::Capability::Setpcap) {
            println!("setcap: requires CAP_SETPCAP");
            return;
        }
        if args.len() < 2 {
            println!("Usage: setcap UID +CAP|-CAP");
            return;
        }
        let uid: u32 = match args[0].parse() {
            Ok(n) => n,
            Err(_) => { println!("setcap: bad uid"); return; }
        };
        let arg = args[1];
        let (grant, name) = if let Some(s) = arg.strip_prefix('+') {
            (true, s)
        } else if let Some(s) = arg.strip_prefix('-') {
            (false, s)
        } else {
            println!("setcap: must prefix capability with + or -");
            return;
        };
        let cap = match crate::capability::Capability::parse(name) {
            Some(c) => c,
            None => { println!("setcap: unknown capability '{}'", name); return; }
        };
        if grant {
            crate::capability::CAPS.lock().grant(uid, cap);
            println!("setcap: granted {} to uid {} (seed)", cap.name(), uid);
        } else {
            crate::capability::CAPS.lock().revoke(uid, cap);
            println!("setcap: revoked {} from uid {} (seed)", cap.name(), uid);
        }
        // If the running process is this uid, refresh its effective set so the
        // change takes effect immediately rather than on next setuid.
        let cur_euid = crate::users::CURRENT_CREDS.lock().euid;
        if cur_euid == uid {
            crate::capability::reseed_for_uid(uid);
        }
    }

    /// seccomp: install / inspect a per-process BPF syscall filter.
    ///   seccomp                                  : count of installed filters
    ///   seccomp allowlist PID SC1,SC2,... ACTION : install BPF allowlist
    ///   seccomp remove PID                       : drop the filter
    ///   seccomp test PID SC                      : run the filter on (SC, args=0)
    /// Actions: allow, errno, kill, log, trap
    fn cmd_seccomp(&self, args: &[&str]) {
        use crate::seccomp::{action, build_allowlist, check, Decision, FILTERS};

        let parse_action = |s: &str| -> Option<u32> {
            match s {
                "allow" => Some(action::RET_ALLOW),
                "errno" => Some(action::RET_ERRNO | 13), // EACCES
                "kill"  => Some(action::RET_KILL_PROCESS),
                "log"   => Some(action::RET_LOG),
                "trap"  => Some(action::RET_TRAP),
                _ => None,
            }
        };

        if args.is_empty() {
            let n = FILTERS.lock().count();
            println!("seccomp: {} filter(s) installed", n);
            return;
        }
        match args[0] {
            "allowlist" if args.len() >= 4 => {
                let pid: usize = match args[1].parse() { Ok(n) => n, Err(_) => { println!("seccomp: bad pid"); return; }};
                let allowed: alloc::vec::Vec<u32> = args[2].split(',')
                    .filter_map(|s| s.parse().ok())
                    .collect();
                let act = match parse_action(args[3]) { Some(a) => a, None => { println!("seccomp: bad action"); return; }};
                let prog = build_allowlist(&allowed, act);
                if FILTERS.lock().install(pid, prog) {
                    println!("seccomp: BPF allowlist of {} syscalls installed for pid {} (default action 0x{:x})",
                        allowed.len(), pid, act);
                } else {
                    println!("seccomp: filter table full");
                }
            }
            "remove" if args.len() >= 2 => {
                let pid: usize = match args[1].parse() { Ok(n) => n, Err(_) => { println!("seccomp: bad pid"); return; }};
                FILTERS.lock().remove(pid);
                println!("seccomp: removed filter for pid {}", pid);
            }
            "test" if args.len() >= 3 => {
                let pid: usize = args[1].parse().unwrap_or(0);
                let sc: usize = args[2].parse().unwrap_or(0);
                let dec = check(pid, sc, [0u64; 6]);
                let label: &str = match dec {
                    Decision::Allow => "ALLOW",
                    Decision::Log => "LOG",
                    Decision::Trap => "TRAP",
                    Decision::Kill => "KILL",
                    Decision::Errno(_) => "ERRNO",
                };
                println!("seccomp: pid={} sc={} -> {} ({:?})", pid, sc, label, dec);
            }
            _ => println!("Usage: seccomp [allowlist PID SC1,SC2,... ACTION | remove PID | test PID SC]"),
        }
    }

    /// tsc: report TSC frequency and current monotonic time in ns.
    fn cmd_tsc(&self) {
        let freq = crate::tsc::freq_hz();
        let ns = crate::tsc::ns_since_boot();
        if freq == 0 {
            println!("tsc: not calibrated");
            return;
        }
        let mhz = freq / 1_000_000;
        println!("TSC frequency: {} Hz (~{} MHz)", freq, mhz);
        println!("Monotonic since boot: {}.{:09} s", ns / 1_000_000_000, ns % 1_000_000_000);
        println!("rdtsc: {}", crate::tsc::read_tsc());
    }

    /// mknod: register a new device node.
    ///   mknod NAME {c|b} MAJOR MINOR
    fn cmd_mknod(&self, args: &[&str]) {
        if args.len() < 4 {
            println!("Usage: mknod NAME {{c|b}} MAJOR MINOR");
            return;
        }
        let name = args[0];
        let kind = match args[1] {
            "c" => crate::fs::devfs::DevKind::Char,
            "b" => crate::fs::devfs::DevKind::Block,
            other => { println!("mknod: unknown type '{}'", other); return; }
        };
        let major: u32 = match args[2].parse() {
            Ok(n) => n,
            Err(_) => { println!("mknod: invalid major"); return; }
        };
        let minor: u32 = match args[3].parse() {
            Ok(n) => n,
            Err(_) => { println!("mknod: invalid minor"); return; }
        };
        crate::fs::devfs::register(name, kind, major, minor);
        println!("mknod: /dev/{} ({} {}, {})", name, args[1], major, minor);
    }

    /// pkg: tiny package manager.
    ///   pkg list                : list installed packages
    ///   pkg install <pkgfile>   : install from a manifest file
    ///   pkg remove <name>       : uninstall by name
    ///   pkg info <name>         : show manifest details
    ///   pkg sample              : write /tmp/sample.pkg with a 2-file demo
    fn cmd_pkg(&self, args: &[&str]) {
        if args.is_empty() || args[0] == "list" {
            let names = crate::pkg::list_installed();
            if names.is_empty() {
                println!("(no packages installed)");
            } else {
                for n in names {
                    if let Some((nm, ver, desc)) = crate::pkg::info(&n) {
                        println!("{}-{}  {}", nm, ver, desc);
                    } else {
                        println!("{}", n);
                    }
                }
            }
            return;
        }

        match args[0] {
            "install" if args.len() >= 2 => {
                match crate::pkg::install(args[1]) {
                    Ok(m) => println!("pkg: installed {}-{} ({} files)",
                        m.name, m.version, m.files.len()),
                    Err(e) => println!("pkg: {}", e),
                }
            }
            "remove" if args.len() >= 2 => {
                let force = args.contains(&"--force") || args.contains(&"-f");
                match crate::pkg::remove_with(args[1], force) {
                    Ok(n) => println!("pkg: removed {} ({} files unlinked)", args[1], n),
                    Err(e) => println!("pkg: {}", e),
                }
            }
            "info" if args.len() >= 2 => {
                match crate::pkg::info(args[1]) {
                    Some((n, v, d)) => {
                        println!("Name: {}\nVersion: {}\nDescription: {}", n, v, d);
                        let deps = crate::pkg::dependencies_of(args[1]);
                        if !deps.is_empty() {
                            println!("Depends: {}", deps.join(", "));
                        }
                        let rdeps = crate::pkg::reverse_dependencies(args[1]);
                        if !rdeps.is_empty() {
                            println!("Required by: {}", rdeps.join(", "));
                        }
                    }
                    None => println!("pkg: {} not installed", args[1]),
                }
            }
            "deps" if args.len() >= 2 => {
                let deps = crate::pkg::dependencies_of(args[1]);
                let rdeps = crate::pkg::reverse_dependencies(args[1]);
                if deps.is_empty() && rdeps.is_empty() {
                    println!("(no dependency edges)");
                } else {
                    println!("Depends:     {:?}", deps);
                    println!("Required by: {:?}", rdeps);
                }
            }
            "sample" => {
                let mut data = alloc::vec::Vec::new();
                data.extend_from_slice(b"NAME hello\nVERSION 0.1.0\nDESCRIPTION RustOS demo package\n");
                data.extend_from_slice(b"FILE /usr/share/hello/greet.txt 14\n");
                data.extend_from_slice(b"hello, rustos\n");
                data.extend_from_slice(b"FILE /usr/share/hello/version.txt 6\n");
                data.extend_from_slice(b"0.1.0\n");
                let _ = crate::fs::vfs::VfsContext::mkdir("/tmp");
                match crate::fs::vfs::VfsContext::write("/tmp/sample.pkg", data) {
                    Ok(_) => println!("pkg: wrote /tmp/sample.pkg (try `pkg install /tmp/sample.pkg`)"),
                    Err(e) => println!("pkg: write failed: {:?}", e),
                }
            }
            _ => println!("Usage: pkg [list | install FILE | remove NAME | info NAME | sample]"),
        }
    }

    /// partprobe: parse the MBR sector of the first ATA disk (or a synthetic
    /// disk image if none is attached) and print the partition table.
    fn cmd_partprobe(&self, args: &[&str]) {
        let want_gpt = args.first() == Some(&"gpt");

        // Try a real ATA read first.
        let mut sector = [0u8; 512];
        let got_real = {
            let mut drv = crate::drivers::ata::ATA_DRIVE.lock();
            drv.read_sector(0, &mut sector).is_ok()
        };

        if !got_real {
            for b in sector.iter_mut() { *b = 0; }
            sector[510] = 0x55;
            sector[511] = 0xAA;
            if want_gpt {
                // Protective MBR: single 0xEE entry covering the whole disk.
                sector[446 + 4] = 0xEE;
                sector[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
                sector[446 + 12..446 + 16].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
                println!("partprobe: no ATA disk; using synthetic GPT for self-test");
            } else {
                // MBR with two real entries.
                sector[446] = 0x80;
                sector[446 + 4] = 0x83;
                sector[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
                sector[446 + 12..446 + 16].copy_from_slice(&102400u32.to_le_bytes());
                sector[462 + 4] = 0x0C;
                sector[462 + 8..462 + 12].copy_from_slice(&104448u32.to_le_bytes());
                sector[462 + 12..462 + 16].copy_from_slice(&102400u32.to_le_bytes());
                println!("partprobe: no ATA disk; using synthetic MBR for self-test");
            }
        }

        match crate::partition::parse_mbr(&sector) {
            Ok(parts) => {
                if crate::partition::is_gpt_protective(&parts) {
                    println!("partprobe: GPT protective MBR detected — reading GPT header at LBA 1");
                    // Read sector 1 (the GPT header) and the partition
                    // entry array.  Without a real disk we fall back to
                    // a synthetic GPT layout matching what `mkfs.gpt` /
                    // gdisk would produce.
                    let mut hdr_sector = [0u8; 512];
                    let got_real_hdr = {
                        let mut drv = crate::drivers::ata::ATA_DRIVE.lock();
                        drv.read_sector(1, &mut hdr_sector).is_ok()
                    };
                    let mut entries: alloc::vec::Vec<u8> = if got_real_hdr {
                        // Read 32 sectors of partition array (128 entries × 128 bytes = 16 KB).
                        let mut buf = alloc::vec![0u8; 32 * 512];
                        let mut drv = crate::drivers::ata::ATA_DRIVE.lock();
                        for i in 0..32 {
                            let mut s = [0u8; 512];
                            if drv.read_sector(2 + i, &mut s).is_err() { break; }
                            buf[i as usize * 512..i as usize * 512 + 512].copy_from_slice(&s);
                        }
                        buf
                    } else {
                        // Synthesise a GPT with two entries: EFI system + Linux fs.
                        synthesise_demo_gpt(&mut hdr_sector)
                    };
                    let _ = entries.len();
                    match crate::partition::parse_gpt(&hdr_sector, &entries) {
                        Ok((hdr, parts)) => {
                            for line in crate::partition::format_gpt(&hdr, &parts) {
                                println!("{}", line);
                            }
                        }
                        Err(e) => println!("partprobe: GPT parse error: {}", e),
                    }
                    return;
                }
                for line in crate::partition::format_table(&parts) {
                    println!("{}", line);
                }
            }
            Err(e) => println!("partprobe: {}", e),
        }
    }

    /// fcntl: byte-range advisory locks.
    ///   fcntl list                            : show all byte-range locks
    ///   fcntl lock {-s|-x} OWNER PATH START LEN
    ///   fcntl unlock OWNER PATH START LEN
    fn cmd_fcntl(&self, args: &[&str]) {
        if args.is_empty() || args[0] == "list" {
            for (path, locks) in crate::flock::BYTE_LOCKS.lock().snapshot() {
                println!("{}:", path);
                for l in locks {
                    let k = match l.kind {
                        crate::flock::LockKind::Shared => "SH",
                        crate::flock::LockKind::Exclusive => "EX",
                    };
                    let len = if l.len == 0 { "EOF".into() } else { alloc::format!("{}", l.len) };
                    println!("  owner={} {}-{} {}", l.owner, l.start, len, k);
                }
            }
            return;
        }

        match args[0] {
            "lock" if args.len() >= 6 => {
                let kind = match args[1] {
                    "-s" => crate::flock::LockKind::Shared,
                    "-x" => crate::flock::LockKind::Exclusive,
                    _ => { println!("fcntl: -s or -x required"); return; }
                };
                let owner: usize = args[2].parse().unwrap_or(0);
                let path = args[3];
                let start: u64 = args[4].parse().unwrap_or(0);
                let len: u64 = args[5].parse().unwrap_or(0);
                let lock = crate::flock::ByteLock { owner, start, len, kind };
                match crate::flock::BYTE_LOCKS.lock().try_lock(path, lock) {
                    Ok(()) => println!("fcntl: locked {}@{}+{}", path, start, len),
                    Err(e) => println!("fcntl: {}", e),
                }
            }
            "unlock" if args.len() >= 5 => {
                let owner: usize = args[1].parse().unwrap_or(0);
                let path = args[2];
                let start: u64 = args[3].parse().unwrap_or(0);
                let len: u64 = args[4].parse().unwrap_or(0);
                let n = crate::flock::BYTE_LOCKS.lock().unlock(path, owner, start, len);
                println!("fcntl: removed {} lock(s)", n);
            }
            _ => println!("Usage: fcntl [list | lock {{-s|-x}} OWNER PATH START LEN | unlock OWNER PATH START LEN]"),
        }
    }

    /// flock: acquire/release advisory file locks.
    ///   flock list                : show all locks
    ///   flock -s OWNER PATH       : take a shared lock as OWNER
    ///   flock -x OWNER PATH       : take an exclusive lock
    ///   flock -u OWNER PATH       : release
    fn cmd_flock(&self, args: &[&str]) {
        if args.is_empty() || args[0] == "list" {
            for (path, holders) in crate::flock::LOCKS.lock().snapshot() {
                print!("{}: ", path);
                for h in holders {
                    let k = match h.kind {
                        crate::flock::LockKind::Shared => "SH",
                        crate::flock::LockKind::Exclusive => "EX",
                    };
                    print!("[owner={} {}] ", h.owner, k);
                }
                println!();
            }
            return;
        }

        if args.len() < 3 {
            println!("Usage: flock {{-s|-x|-u}} OWNER PATH");
            return;
        }
        let op = match args[0] {
            "-s" => crate::flock::LOCK_SH,
            "-x" => crate::flock::LOCK_EX,
            "-u" => crate::flock::LOCK_UN,
            other => { println!("flock: unknown flag '{}'", other); return; }
        };
        let owner: usize = match args[1].parse() {
            Ok(n) => n,
            Err(_) => { println!("flock: invalid owner '{}'", args[1]); return; }
        };
        let path = args[2];
        match crate::flock::flock(path, owner, op | crate::flock::LOCK_NB) {
            Ok(()) => println!("flock: {} {}", path, args[0]),
            Err(e) => println!("flock: {}: {}", path, e),
        }
    }

    /// ulimit: list/get/set resource limits.
    ///   ulimit          : list all
    ///   ulimit -a       : same
    ///   ulimit -n       : show RLIMIT_NOFILE soft
    ///   ulimit -n 2048  : set RLIMIT_NOFILE soft+hard to 2048
    fn cmd_ulimit(&self, args: &[&str]) {
        use crate::rlimit::{fmt_limit, Resource, Rlimit, LIMITS};

        // No args or -a: list everything.
        if args.is_empty() || args[0] == "-a" {
            let table = LIMITS.lock();
            println!("{:<12} {:<14} {:<14}", "resource", "soft", "hard");
            for (res, lim) in table.iter() {
                println!("{:<12} {:<14} {:<14}",
                    res.as_str(), fmt_limit(lim.soft), fmt_limit(lim.hard));
            }
            return;
        }

        // Map -X flag → Resource.
        let res = match args[0] {
            "-c" => Resource::Core,
            "-d" => Resource::Data,
            "-f" => Resource::Fsize,
            "-l" => Resource::Memlock,
            "-m" => Resource::Rss,
            "-n" => Resource::NoFile,
            "-s" => Resource::Stack,
            "-t" => Resource::Cpu,
            "-u" => Resource::Nproc,
            "-v" => Resource::As,
            other => {
                println!("ulimit: unknown option '{}'", other);
                return;
            }
        };

        // No second arg: print soft.
        if args.len() < 2 {
            let lim = LIMITS.lock().get(res);
            println!("{}", fmt_limit(lim.soft));
            return;
        }

        // Second arg: set new value.  "unlimited" maps to RLIM_INFINITY.
        let new_val: u64 = if args[1] == "unlimited" {
            crate::rlimit::RLIM_INFINITY
        } else {
            match args[1].parse() {
                Ok(n) => n,
                Err(_) => { println!("ulimit: invalid value '{}'", args[1]); return; }
            }
        };
        let is_root = crate::users::CURRENT_CREDS.lock().uid == 0;
        let cur = LIMITS.lock().get(res);
        let new_hard = new_val.max(cur.hard.min(new_val));
        let new = Rlimit { soft: new_val, hard: if is_root { new_val } else { cur.hard.min(new_val).max(new_hard) } };
        match LIMITS.lock().set(res, Rlimit { soft: new_val, hard: if is_root { new_val } else { cur.hard } }, is_root) {
            Ok(()) => println!("ulimit: {} set to {}", res.as_str(), fmt_limit(new.soft)),
            Err(e) => println!("ulimit: {}", e),
        }
    }

    /// epolltest: stand up an epoll instance, register stdin/stdout, poll once.
    fn cmd_epolltest(&self) {
        use crate::syscall::epoll::{
            epoll_create, epoll_ctl, epoll_wait, ops, flags, EpollEvent,
        };

        let ep = epoll_create();
        println!("epoll_create -> {}", ep);

        let stdin_ev = EpollEvent { events: flags::EPOLLIN,  data: 0xA1 };
        let stdout_ev = EpollEvent { events: flags::EPOLLOUT, data: 0xA2 };
        println!("epoll_ctl ADD stdin  -> {}", epoll_ctl(ep, ops::EPOLL_CTL_ADD, 0, stdin_ev));
        println!("epoll_ctl ADD stdout -> {}", epoll_ctl(ep, ops::EPOLL_CTL_ADD, 1, stdout_ev));

        let mut out = [EpollEvent { events: 0, data: 0 }; 8];
        let n = epoll_wait(ep, &mut out, 0);
        println!("epoll_wait(timeout=0) -> {} ready", n);
        for i in 0..(n as usize) {
            let e = out[i].events;
            let d = out[i].data;
            println!("  events=0x{:08X}  data=0x{:X}", e, d);
        }

        // DEL stdin and recheck
        println!("epoll_ctl DEL stdin -> {}", epoll_ctl(ep, ops::EPOLL_CTL_DEL, 0, stdin_ev));
        let n2 = epoll_wait(ep, &mut out, 0);
        println!("epoll_wait after DEL -> {} ready", n2);

        crate::syscall::epoll::epoll_close(ep);
        println!("epolltest: done");
    }

    /// arp: show / manipulate the ARP cache, or self-test the protocol.
    ///   arp                  : show cache
    ///   arp -d IP            : delete an entry
    ///   arp -s IP MAC        : add a static entry (mac as aa:bb:cc:dd:ee:ff)
    ///   arp test             : run a request → reply parse round-trip
    fn cmd_arp(&self, args: &[&str]) {
        if args.is_empty() {
            for (ip, mac) in crate::net::arp::cache_snapshot() {
                println!("{} -> {}", ip, crate::net::eth::fmt_mac(&mac));
            }
            return;
        }
        match args[0] {
            "-d" if args.len() >= 2 => {
                let _ = args[1]; // delete-by-IP not modelled; clear all instead
                crate::net::arp::cache_clear();
                println!("arp: cache cleared");
            }
            "-s" if args.len() >= 3 => {
                let ip = match crate::net::Ipv4Addr::parse(args[1]) {
                    Some(a) => a,
                    None => { println!("arp: bad IP"); return; }
                };
                let mac = match parse_mac(args[2]) {
                    Some(m) => m,
                    None => { println!("arp: bad MAC"); return; }
                };
                crate::net::arp::cache_insert(ip, mac);
                println!("arp: {} -> {}", ip, crate::net::eth::fmt_mac(&mac));
            }
            "test" => {
                let my_mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
                let my_ip = crate::net::Ipv4Addr([10, 0, 2, 15]);
                let peer_mac = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];
                let peer_ip = crate::net::Ipv4Addr([10, 0, 2, 2]);

                // Synthesise a request from the peer asking who-has my_ip.
                let req = crate::net::arp::ArpPacket {
                    htype: crate::net::arp::HTYPE_ETHERNET,
                    ptype: crate::net::arp::PTYPE_IPV4,
                    hlen:  crate::net::arp::HLEN_ETHERNET,
                    plen:  crate::net::arp::PLEN_IPV4,
                    op:    crate::net::arp::op::REQUEST,
                    sender_mac: peer_mac,
                    sender_ip:  peer_ip,
                    target_mac: [0u8; 6],
                    target_ip:  my_ip,
                };
                let req_bytes = req.build();
                println!("arp test: REQUEST {} bytes who-has {}", req_bytes.len(), my_ip);

                // Process it as if it arrived; we should reply.
                match crate::net::arp::input(&req_bytes, my_mac, my_ip) {
                    Some(reply_frame) => {
                        println!("arp test: REPLY frame {} bytes", reply_frame.len());
                        let arp_payload = &reply_frame[crate::net::eth::ETH_HEADER_LEN..];
                        if let Some(reply) = crate::net::arp::ArpPacket::parse(arp_payload) {
                            println!(
                                "arp test: parsed reply: {} is at {}",
                                reply.sender_ip,
                                crate::net::eth::fmt_mac(&reply.sender_mac)
                            );
                        }
                    }
                    None => println!("arp test: no reply generated"),
                }

                // Verify peer is now in our cache.
                if let Some(m) = crate::net::arp::cache_lookup(peer_ip) {
                    println!("arp test: cache has {} -> {}", peer_ip, crate::net::eth::fmt_mac(&m));
                }
            }
            _ => println!("Usage: arp [test | -s IP MAC | -d IP]"),
        }
    }

    /// dhclient: drive a DHCP DISCOVER → OFFER → REQUEST → ACK round-trip.
    /// In this build there's no real DHCP server reachable, so the offer
    /// and ack are forged locally to exercise the parse/serialise paths.
    fn cmd_dhclient(&self, _args: &[&str]) {
        use crate::net::dhcp::*;
        use crate::net::Ipv4Addr;

        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let xid: u32 = (crate::task::timer::current_ticks() as u32) ^ 0xDEADBEEF;
        let server = Ipv4Addr([10, 0, 2, 2]);
        let assigned = Ipv4Addr([10, 0, 2, 15]);

        let discover = build_discover(mac, xid);
        println!("dhclient: DISCOVER xid=0x{:08x} ({} bytes)", xid, discover.len());

        let offer = fake_offer_for(&discover, assigned, server);
        let parsed_offer = DhcpMessage::parse(&offer).expect("offer parse");
        println!("dhclient: OFFER yiaddr={} server={} type={:?} lease={}s",
            parsed_offer.yiaddr, server,
            parsed_offer.message_type(),
            parsed_offer.lease_seconds().unwrap_or(0));

        let request = build_request(mac, xid, parsed_offer.yiaddr, server);
        println!("dhclient: REQUEST xid=0x{:08x} ({} bytes)", xid, request.len());

        let ack = fake_ack_for(&request, parsed_offer.yiaddr, server);
        let parsed_ack = DhcpMessage::parse(&ack).expect("ack parse");
        println!("dhclient: ACK yiaddr={} type={:?} lease={}s",
            parsed_ack.yiaddr,
            parsed_ack.message_type(),
            parsed_ack.lease_seconds().unwrap_or(0));

        crate::syslog::log(
            crate::syslog::Facility::Daemon,
            crate::syslog::Severity::Info,
            "dhclient",
            alloc::format!("bound: {} from {}", parsed_ack.yiaddr, server),
        );
    }

    /// ifconfig / ip: list network interfaces.
    fn cmd_ifconfig(&self, args: &[&str]) {
        // Optional: ifconfig lo up | ifconfig lo down
        if args.len() >= 2 {
            let name = args[0];
            let action = args[1];
            let mut stack = crate::net::NET_STACK.lock();
            if let Some(e) = stack.find_by_name(name) {
                match action {
                    "up" => { e.iface.set_up(true); println!("{} up", name); }
                    "down" => { e.iface.set_up(false); println!("{} down", name); }
                    _ => println!("ifconfig: unknown action '{}'", action),
                }
            } else {
                println!("ifconfig: interface '{}' not found", name);
            }
            return;
        }
        for line in crate::net::ifconfig_lines() {
            println!("{}", line);
            println!();
        }
    }

    /// netstat / ss: list bound UDP sockets and TCP endpoints.
    fn cmd_netstat(&self, _args: &[&str]) {
        use crate::net::socket::SOCKETS;
        println!("Proto Local Address           Foreign Address          State        Recv-Q  H");
        // UDP sockets
        let udp = SOCKETS.lock().enumerate();
        for (h, proto, bound, qlen) in udp {
            let addr = match bound {
                Some(a) => alloc::format!("{}", a),
                None => alloc::string::String::from("*:*"),
            };
            let p = match proto {
                crate::net::socket::SocketProto::Udp => "udp",
            };
            println!("{:<5} {:<24} {:<24} {:<12} {:>6}  {}",
                p, addr, "*:*", "-", qlen, h);
        }
        // TCP endpoints
        for (h, state, local, remote, rxlen) in crate::net::tcp::enumerate() {
            let remote_str = if remote.port == 0 { alloc::string::String::from("*:*") }
                             else { alloc::format!("{}", remote) };
            println!("{:<5} {:<24} {:<24} {:<12} {:>6}  {}",
                "tcp", alloc::format!("{}", local), remote_str, state.as_str(), rxlen, h);
        }
    }

    /// ping: real ICMP echo. Loopback only in this build (no NIC driver yet).
    fn cmd_ping(&self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: ping <host-or-ip> [count]");
            return;
        }
        let target = args[0];
        let addr = match crate::net::dns::resolve(target) {
            Some(a) => {
                if target != alloc::format!("{}", a) {
                    println!("PING {} ({})", target, a);
                }
                a
            }
            None => {
                println!("ping: cannot resolve '{}'", target);
                return;
            }
        };
        if !addr.is_loopback() {
            println!("ping: only loopback addresses are reachable in this build");
            return;
        }
        let count: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);

        // Use the lower 16 bits of timer ticks as a unique identifier so
        // back-to-back pings don't collide with stray queued replies.
        let ident = (crate::task::timer::current_ticks() as u16) | 0x8000;
        let payload: &[u8] = b"abcdefghijklmnopqrstuvwabcdefghi"; // 32 bytes

        println!("PING {}: 32 data bytes", addr);
        let mut received = 0usize;
        for seq in 0..count {
            let send_tick = crate::task::timer::current_ticks();
            match crate::net::icmp::send_echo_request(
                crate::net::Ipv4Addr::LOCALHOST,
                addr,
                ident,
                seq as u16,
                payload,
            ) {
                Ok(_) => {
                    // The reply is dispatched synchronously by lo's TX path.
                    if let Some(reply) = crate::net::icmp::drain_echo_reply(ident, seq as u16) {
                        let dt_ticks = reply.recv_tick.saturating_sub(send_tick);
                        let dt_ms = dt_ticks * 55; // ~PIT period
                        println!(
                            "{} bytes from {}: icmp_seq={} time={}ms",
                            reply.payload.len(),
                            reply.from,
                            seq,
                            dt_ms
                        );
                        received += 1;
                    } else {
                        println!("Request timeout for icmp_seq {}", seq);
                    }
                }
                Err(e) => {
                    println!("ping: send error: {}", e);
                    break;
                }
            }
        }
        let lost = count - received;
        let loss_pct = if count == 0 { 0 } else { lost * 100 / count };
        println!("--- {} ping statistics ---", addr);
        println!("{} packets transmitted, {} received, {}% packet loss",
            count, received, loss_pct);
    }

    /// wget / curl: fetch an HTTP URL over TCP.  Usage:
    ///   wget http://host[:port]/path        -> writes to ./<basename>
    ///   wget -O FILE http://host[:port]/path
    ///   wget -- http://host[:port]/path     -> stdout (curl-style print)
    fn cmd_wget(&self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: wget [-O FILE] <url>");
            return;
        }

        let mut output_file: Option<&str> = None;
        let mut print_to_stdout = false;
        let mut url: Option<&str> = None;

        let mut i = 0;
        while i < args.len() {
            match args[i] {
                "-O" if i + 1 < args.len() => {
                    output_file = Some(args[i + 1]);
                    i += 2;
                }
                "--" => { print_to_stdout = true; i += 1; }
                u if u.starts_with("http://") || u.starts_with("https://") => {
                    url = Some(u);
                    i += 1;
                }
                u => { url = Some(u); i += 1; }
            }
        }

        let url = match url { Some(u) => u, None => { println!("wget: no URL"); return; } };

        // Strip optional scheme.
        let stripped = url.strip_prefix("http://").unwrap_or(url);
        let stripped = stripped.strip_prefix("https://").unwrap_or(stripped);

        // Split into host[:port] and path.
        let (authority, path) = match stripped.find('/') {
            Some(p) => (&stripped[..p], &stripped[p..]),
            None => (stripped, "/"),
        };
        let (host, port) = match authority.rfind(':') {
            Some(p) => {
                let port: u16 = authority[p + 1..].parse().unwrap_or(80);
                (&authority[..p], port)
            }
            None => (authority, 80u16),
        };

        // Resolve the host.
        let addr = match crate::net::dns::resolve(host) {
            Some(a) => a,
            None => {
                println!("wget: cannot resolve {}", host);
                return;
            }
        };

        // Connect.
        use crate::net::tcp::{close, connect, recv, send};
        use crate::net::SocketAddrV4;
        let server_addr = SocketAddrV4 { ip: addr, port };
        let conn = match connect(server_addr) {
            Ok(h) => h,
            Err(e) => { println!("wget: connect to {} failed: {:?}", server_addr, e); return; }
        };

        // Send a tiny HTTP/1.0 GET.
        let req = alloc::format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: rustos-wget/0.1\r\nConnection: close\r\n\r\n",
            path, host
        );
        if let Err(e) = send(conn, req.as_bytes()) {
            println!("wget: send failed: {:?}", e);
            let _ = close(conn);
            return;
        }

        // If the destination is our local httpd, drive a single
        // accept→serve→close round on the kernel-resident listener so the
        // response is queued by the time we recv() below.  When we acquire
        // a real TCP scheduler this becomes unnecessary.
        if addr.is_loopback() && port == 80 {
            if let Some(server_listener) = crate::httpd::global_listener() {
                let _ = crate::httpd::serve_once(server_listener);
            }
        }

        // Read the response.  In our loopback stack the whole reply arrives
        // in the recv queue immediately after the synchronous handshake.
        let mut buf = [0u8; 8192];
        let n = match recv(conn, &mut buf) {
            Ok(n) => n,
            Err(e) => { println!("wget: recv failed: {:?}", e); let _ = close(conn); return; }
        };
        let _ = close(conn);

        let resp = &buf[..n];
        // Split status line and body
        let mut header_end = 0usize;
        let mut found = false;
        while header_end + 3 < resp.len() {
            if &resp[header_end..header_end + 4] == b"\r\n\r\n" {
                found = true;
                break;
            }
            header_end += 1;
        }
        let body_start = if found { header_end + 4 } else { resp.len() };
        let header_text = core::str::from_utf8(&resp[..header_end]).unwrap_or("");
        let status_line = header_text.lines().next().unwrap_or("");
        println!("{}", status_line);

        let body = &resp[body_start..];
        if print_to_stdout {
            if let Ok(s) = core::str::from_utf8(body) {
                println!("{}", s);
            } else {
                println!("[binary, {} bytes]", body.len());
            }
            return;
        }

        let target = match output_file {
            Some(f) => f.to_string(),
            None => {
                // Default name: trailing path component, or "index.html"
                let name = path.rsplit('/').next().unwrap_or("");
                if name.is_empty() { String::from("index.html") } else { String::from(name) }
            }
        };
        match crate::fs::vfs::VfsContext::write(&target, body.to_vec()) {
            Ok(_) => println!("Saved {} bytes to {}", body.len(), target),
            Err(e) => println!("wget: write {}: {:?}", target, e),
        }
    }

    /// unixtest: end-to-end test of AF_UNIX SOCK_STREAM and SOCK_DGRAM.
    fn cmd_unixtest(&self, _args: &[&str]) {
        use crate::net::unix::{
            accept, bind, close, connect, recv, recvfrom, send, sendto, socket, UnixKind,
        };

        // ---- SOCK_STREAM ----
        let server = socket(UnixKind::Stream);
        if let Err(e) = bind(server, "/tmp/.unix-stream-test") {
            println!("unixtest: stream bind error: {:?}", e);
            return;
        }
        if let Err(e) = crate::net::unix::listen(server, 4) {
            println!("unixtest: stream listen error: {:?}", e);
            return;
        }
        let client = socket(UnixKind::Stream);
        if let Err(e) = connect(client, "/tmp/.unix-stream-test") {
            println!("unixtest: stream connect error: {:?}", e);
            return;
        }
        let server_side = match accept(server) {
            Ok(h) => h,
            Err(e) => { println!("unixtest: stream accept error: {:?}", e); return; }
        };
        let payload = b"unix-stream-payload";
        let _ = send(client, payload);
        let mut buf = [0u8; 64];
        match recv(server_side, &mut buf) {
            Ok(n) => {
                println!("unixtest: stream server got {} bytes: '{}'",
                    n, core::str::from_utf8(&buf[..n]).unwrap_or("<binary>"));
                if &buf[..n] == payload {
                    println!("unixtest: stream payload matches");
                }
            }
            Err(e) => println!("unixtest: stream recv error: {:?}", e),
        }
        let _ = send(server_side, b"stream-ack");
        let mut buf2 = [0u8; 64];
        if let Ok(n) = recv(client, &mut buf2) {
            println!("unixtest: stream client got reply: '{}'",
                core::str::from_utf8(&buf2[..n]).unwrap_or("<binary>"));
        }
        close(client);
        close(server_side);
        close(server);

        // ---- SOCK_DGRAM ----
        let dg_srv = socket(UnixKind::Datagram);
        let _ = bind(dg_srv, "/tmp/.unix-dgram-test");
        let dg_cli = socket(UnixKind::Datagram);
        let _ = bind(dg_cli, "/tmp/.unix-dgram-client");
        match sendto(dg_cli, "/tmp/.unix-dgram-test", b"hello-dgram") {
            Ok(n) => println!("unixtest: dgram client sent {} bytes", n),
            Err(e) => println!("unixtest: dgram send error: {:?}", e),
        }
        let mut buf3 = [0u8; 64];
        match recvfrom(dg_srv, &mut buf3) {
            Ok((n, from)) => {
                println!("unixtest: dgram server got {} bytes from '{}': '{}'",
                    n, from, core::str::from_utf8(&buf3[..n]).unwrap_or("<binary>"));
            }
            Err(e) => println!("unixtest: dgram recv error: {:?}", e),
        }
        close(dg_cli);
        close(dg_srv);
        println!("unixtest: done");
    }

    /// httptest: spin up the demo httpd, issue a GET via the kernel TCP
    /// client, print the response.  Validates: TCP + VFS + HTTP parse.
    fn cmd_httptest(&self, args: &[&str]) {
        use crate::net::tcp::{close, connect, listen, recv, send};
        use crate::net::{Ipv4Addr, SocketAddrV4};

        let port: u16 = args.first().and_then(|s| s.parse().ok()).unwrap_or(8000);
        let path: &str = args.get(1).copied().unwrap_or("/");

        let server_addr = SocketAddrV4 { ip: Ipv4Addr::LOCALHOST, port };

        let server = match listen(server_addr, 4) {
            Ok(h) => h,
            Err(e) => { println!("httptest: listen error: {:?}", e); return; }
        };
        println!("httptest: server listening on {}", server_addr);

        // Connect from a client.
        let client = match connect(server_addr) {
            Ok(h) => h,
            Err(e) => {
                println!("httptest: connect error: {:?}", e);
                let _ = close(server);
                return;
            }
        };

        // Send a GET request.
        let req = alloc::format!(
            "GET {} HTTP/1.0\r\nHost: 127.0.0.1\r\nUser-Agent: rustos-shell\r\n\r\n",
            path
        );
        if let Err(e) = send(client, req.as_bytes()) {
            println!("httptest: send error: {:?}", e);
            let _ = close(client); let _ = close(server);
            return;
        }

        // Server side: accept + serve once.
        match crate::httpd::serve_once(server) {
            Ok(_) => {}
            Err(e) => {
                println!("httptest: serve error: {}", e);
                let _ = close(client); let _ = close(server);
                return;
            }
        }

        // Read the response on the client side.
        let mut buf = [0u8; 4096];
        match recv(client, &mut buf) {
            Ok(n) => {
                let resp = core::str::from_utf8(&buf[..n]).unwrap_or("<binary>");
                println!("--- HTTP response ({} bytes) ---", n);
                print!("{}", resp);
                if !resp.ends_with('\n') { println!(); }
                println!("--- end ---");
            }
            Err(e) => println!("httptest: client recv error: {:?}", e),
        }

        let _ = close(client);
        let _ = close(server);
    }

    /// lspci: list every device the PCI scanner found at boot.
    fn cmd_lspci(&self) {
        let devs = crate::drivers::pci::list();
        if devs.is_empty() {
            println!("lspci: no PCI devices enumerated");
            return;
        }
        println!("addr      vendor  device  class  subclass  prog_if  IRQ");
        for d in devs {
            println!("{:02x}:{:02x}.{}   {:#06x}  {:#06x}   {:02x}     {:02x}        {:02x}      {}",
                d.addr.bus, d.addr.dev, d.addr.func,
                d.vendor_id, d.device_id,
                d.class_code, d.subclass, d.prog_if,
                d.interrupt_line);
        }
    }

    /// nicstat: virtio-net status (MAC, RX/TX packet counts).
    fn cmd_nicstat(&self) {
        if !crate::drivers::virtio_net::is_available() {
            println!("virtio-net: not available (no -device virtio-net-pci?)");
            return;
        }
        let g = crate::drivers::virtio_net::VIRTIO_NET.lock();
        if let Some(nic) = g.as_ref() {
            println!("virtio-net:");
            let mac = nic.mac;
            println!("  MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
            println!("  RX packets: {}", nic.rx_packets());
            println!("  TX packets: {}", nic.tx_packets());
        }
    }

    /// ext2: mount the synthetic demo image, list root, read a file.
    ///   ext2 mount             : install the demo image into EXT2 slot
    ///   ext2 ls                : list root directory
    ///   ext2 cat <path>        : read file content
    ///   ext2 super             : print superblock summary
    fn cmd_ext2(&self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: ext2 [mount | ls | cat PATH | super]");
            return;
        }
        match args[0] {
            "mount" => {
                let img = crate::fs::ext2::synthesise_demo_image();
                match crate::fs::ext2::Ext2Fs::mount(img) {
                    Ok(fs) => {
                        let groups = fs.bgdt.len();
                        let bs = fs.sb.block_size();
                        *crate::fs::ext2::EXT2.lock() = Some(fs);
                        println!("ext2: mounted demo image — {} groups, block size {}", groups, bs);
                    }
                    Err(e) => println!("ext2: mount failed: {}", e),
                }
            }
            "super" => {
                let g = crate::fs::ext2::EXT2.lock();
                match g.as_ref() {
                    Some(fs) => {
                        println!("Magic:           {:#06x}", fs.sb.magic);
                        println!("Block size:      {}", fs.sb.block_size());
                        println!("Inode size:      {}", fs.sb.inode_size);
                        println!("Inodes total:    {}", fs.sb.inodes_count);
                        println!("Blocks total:    {}", fs.sb.blocks_count);
                        println!("Free inodes:     {}", fs.sb.free_inodes_count);
                        println!("Free blocks:     {}", fs.sb.free_blocks_count);
                        println!("Blocks/group:    {}", fs.sb.blocks_per_group);
                        println!("Inodes/group:    {}", fs.sb.inodes_per_group);
                        println!("First data:      {}", fs.sb.first_data_block);
                        println!("Groups:          {}", fs.bgdt.len());
                    }
                    None => println!("ext2: not mounted (run `ext2 mount`)"),
                }
            }
            "ls" => {
                let g = crate::fs::ext2::EXT2.lock();
                let fs = match g.as_ref() {
                    Some(f) => f,
                    None => { println!("ext2: not mounted"); return; }
                };
                let root = match fs.read_inode(2) { Ok(i) => i, Err(e) => { println!("ext2: {}", e); return; } };
                let entries = match fs.read_dir(&root) { Ok(e) => e, Err(e) => { println!("ext2: {}", e); return; } };
                println!("ino   type  name");
                for e in entries {
                    let kind = match e.file_type {
                        1 => "f",
                        2 => "d",
                        7 => "l",
                        _ => "?",
                    };
                    println!("{:5}  {}     {}", e.inode, kind, e.name);
                }
            }
            "cat" if args.len() >= 2 => {
                let g = crate::fs::ext2::EXT2.lock();
                let fs = match g.as_ref() {
                    Some(f) => f,
                    None => { println!("ext2: not mounted"); return; }
                };
                match fs.read_file(args[1]) {
                    Ok(d) => {
                        if let Ok(s) = core::str::from_utf8(&d) {
                            print!("{}", s);
                            if !s.ends_with('\n') { println!(); }
                        } else {
                            println!("ext2: {} is not UTF-8 ({} bytes)", args[1], d.len());
                        }
                    }
                    Err(e) => println!("ext2: {}", e),
                }
            }
            _ => println!("Usage: ext2 [mount | ls | cat PATH | super]"),
        }
    }

    /// panic-test: deliberately panic to exercise the backtrace printer.
    fn cmd_panic_test(&self) -> ! {
        panic!("panic-test: this is intentional");
    }

    /// futex: list active wait queues, or run a wake self-test.
    ///   futex            : list addresses with parked waiters
    ///   futex test       : park 0 waiters, wake them — verifies wake counts
    fn cmd_futex(&self, args: &[&str]) {
        if args.first() == Some(&"test") {
            // Run wake on a fresh address; should report 0 woken because
            // no one is parked.  Real wait() needs a separate task; this
            // exercises the wake path.
            let dummy: i32 = 1;
            let woken = crate::futex::wake(&dummy as *const i32, 4);
            println!("futex test: wake on empty queue → {} woken", woken);
            return;
        }
        let snap = crate::futex::snapshot();
        if snap.is_empty() {
            println!("(no active futex wait queues)");
            return;
        }
        println!("addr               waiters");
        for (addr, waiters) in snap {
            println!("{:#018x}  {:?}", addr, waiters);
        }
    }

    /// mouse: print current cursor position and button state.
    fn cmd_mouse(&self) {
        let s = crate::drivers::mouse::snapshot();
        let lb = if s.buttons & 0x01 != 0 { "L" } else { "-" };
        let rb = if s.buttons & 0x02 != 0 { "R" } else { "-" };
        let mb = if s.buttons & 0x04 != 0 { "M" } else { "-" };
        println!("Mouse: x={} y={} buttons={}{}{} packets={}",
            s.x, s.y, lb, rb, mb, s.packets);
    }

    /// hpet: print HPET counter, period, and derived frequency.
    fn cmd_hpet(&self) {
        if !crate::drivers::hpet::is_available() {
            println!("HPET: not available");
            return;
        }
        let period = crate::drivers::hpet::period_fs();
        let freq = if period > 0 { 1_000_000_000_000_000u64 / period } else { 0 };
        let counter1 = crate::drivers::hpet::read_counter();
        let ns1 = crate::drivers::hpet::counter_to_ns(counter1);
        // Take a second sample after a brief delay.
        for _ in 0..1_000_000 { core::hint::spin_loop(); }
        let counter2 = crate::drivers::hpet::read_counter();
        let elapsed = counter2.wrapping_sub(counter1);
        let elapsed_ns = crate::drivers::hpet::counter_to_ns(elapsed);
        println!("HPET: period={} fs (~{} Hz)", period, freq);
        println!("  counter sample 1: {}  (~{} ns since boot)", counter1, ns1);
        println!("  counter sample 2: {}", counter2);
        println!("  ~{} ns elapsed in spin loop", elapsed_ns);
    }

    /// sha256 / sha256sum: hash a file or stdin (heredoc).
    fn cmd_sha256(&mut self, args: &[&str]) {
        if args.is_empty() {
            // Stdin (heredoc) path.
            let body = match self.take_stdin() {
                Some(b) => b,
                None => { println!("Usage: sha256 <file> [file ...]"); return; }
            };
            let d = crate::sha256::hash(body.as_bytes());
            println!("{}  -", crate::sha256::hex(&d));
            return;
        }
        for path in args {
            match crate::fs::vfs::VfsContext::read(path) {
                Ok(data) => {
                    let d = crate::sha256::hash(&data);
                    println!("{}  {}", crate::sha256::hex(&d), path);
                }
                Err(_) => println!("sha256: {}: read error", path),
            }
        }
    }

    /// inode: show the inode number assigned by the backing filesystem to
    /// each path passed.  Looks the path up via DentryCache, which calls
    /// the filesystem's ino() implementation under the hood.
    fn cmd_inode(&self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: inode <path> [path ...]");
            return;
        }
        for path in args {
            match crate::fs::inode::lookup(path) {
                Some(i) => {
                    let kind = match i.file_type {
                        crate::fs::vfs::VfsFileType::Regular => "f",
                        crate::fs::vfs::VfsFileType::Directory => "d",
                        crate::fs::vfs::VfsFileType::Symlink => "l",
                        crate::fs::vfs::VfsFileType::CharDevice => "c",
                        crate::fs::vfs::VfsFileType::BlockDevice => "b",
                    };
                    println!("ino={:#x}  type={}  mode={:04o}  uid={}  gid={}  nlink={}  size={}  {}",
                        i.ino, kind, i.mode, i.uid, i.gid, i.nlink, i.size, i.path);
                }
                None => println!("inode: {}: not found", path),
            }
        }
    }

    /// tcpinfo: detailed view of every TCP endpoint — congestion window,
    /// ssthresh, smoothed RTT, RTO, retransmit queue depth and backoff.
    fn cmd_tcpinfo(&self) {
        for e in crate::net::tcp::enumerate_detailed() {
            println!("[{}] {:<11} local={} remote={}", e.handle, e.state.as_str(), e.local, e.remote);
            println!("    snd_nxt={} snd_una={} rcv_nxt={}", e.snd_nxt, e.snd_una, e.rcv_nxt);
            let ssthresh_str = if e.ssthresh == u32::MAX { String::from("inf") } else { alloc::format!("{}", e.ssthresh) };
            println!("    cwnd={} ssthresh={} srtt={}t rttvar={}t rto={}t",
                e.cwnd, ssthresh_str, e.srtt, e.rttvar, e.rto);
            println!("    retx={} backoff={} rcv_buf={}",
                e.retx_len, e.retx_backoff, e.rcv_buf_len);
        }
    }

    /// tcptest: end-to-end TCP loopback handshake + send/recv + close.
    fn cmd_tcptest(&self, args: &[&str]) {
        use crate::net::tcp::{accept, close, connect, listen, recv, send};
        use crate::net::{Ipv4Addr, SocketAddrV4};

        let port: u16 = args.first().and_then(|s| s.parse().ok()).unwrap_or(8080);
        let payload: &[u8] = args.get(1).map(|s| s.as_bytes()).unwrap_or(b"hello-tcp");

        // Server: listen
        let server_addr = SocketAddrV4 { ip: Ipv4Addr::LOCALHOST, port };
        let server = match listen(server_addr, 4) {
            Ok(h) => h,
            Err(e) => { println!("tcptest: listen error: {:?}", e); return; }
        };
        println!("tcptest: listening on {}", server_addr);

        // Client: connect
        let client = match connect(server_addr) {
            Ok(h) => h,
            Err(e) => {
                println!("tcptest: connect error: {:?}", e);
                let _ = close(server);
                return;
            }
        };
        println!("tcptest: client connected (handle {})", client);

        // Server: accept
        let accepted = match accept(server) {
            Ok(h) => h,
            Err(e) => {
                println!("tcptest: accept error: {:?}", e);
                let _ = close(server);
                let _ = close(client);
                return;
            }
        };
        println!("tcptest: server accepted (handle {})", accepted);

        // Client: send
        match send(client, payload) {
            Ok(n) => println!("tcptest: client sent {} bytes", n),
            Err(e) => println!("tcptest: send error: {:?}", e),
        }

        // Server: recv
        let mut buf = [0u8; 256];
        match recv(accepted, &mut buf) {
            Ok(n) => {
                let s = core::str::from_utf8(&buf[..n]).unwrap_or("<binary>");
                println!("tcptest: server received {} bytes: '{}'", n, s);
                if &buf[..n] == payload {
                    println!("tcptest: payload matches");
                } else {
                    println!("tcptest: payload MISMATCH");
                }
            }
            Err(e) => println!("tcptest: recv error: {:?}", e),
        }

        // Server sends a reply
        let _ = send(accepted, b"server-ack");
        let mut buf2 = [0u8; 64];
        match recv(client, &mut buf2) {
            Ok(n) => {
                let s = core::str::from_utf8(&buf2[..n]).unwrap_or("<binary>");
                println!("tcptest: client received reply: '{}'", s);
            }
            Err(e) => println!("tcptest: client recv error: {:?}", e),
        }

        let _ = close(client);
        let _ = close(accepted);
        let _ = close(server);
        println!("tcptest: closed all endpoints");
    }

    /// udptest: send-recv loopback round-trip with payload validation.
    fn cmd_udptest(&self, args: &[&str]) {
        use crate::net::socket::{bind, recvfrom, sendto, socket, SocketDomain, SocketProto, SocketType};
        use crate::net::{Ipv4Addr, SocketAddrV4};

        let payload: &[u8] = if args.is_empty() {
            b"hello, loopback world"
        } else {
            args[0].as_bytes()
        };

        let port: u16 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(12345);
        let server = socket(SocketDomain::AF_INET, SocketType::Datagram, SocketProto::Udp);
        let server_addr = SocketAddrV4 { ip: Ipv4Addr::LOCALHOST, port };
        if let Err(e) = bind(server, server_addr) {
            println!("udptest: bind error: {:?}", e);
            return;
        }

        let client = socket(SocketDomain::AF_INET, SocketType::Datagram, SocketProto::Udp);
        match sendto(client, payload, server_addr) {
            Ok(n) => println!("udptest: sent {} bytes to {}", n, server_addr),
            Err(e) => {
                println!("udptest: send error: {:?}", e);
                crate::net::socket::close(client);
                crate::net::socket::close(server);
                return;
            }
        }

        let mut buf = [0u8; 1500];
        match recvfrom(server, &mut buf) {
            Ok((n, from)) => {
                println!("udptest: received {} bytes from {}", n, from);
                if &buf[..n] == payload {
                    println!("udptest: payload matches ({} bytes)", n);
                } else {
                    println!("udptest: payload MISMATCH");
                }
            }
            Err(e) => println!("udptest: recv error: {:?}", e),
        }

        crate::net::socket::close(client);
        crate::net::socket::close(server);
    }

    /// polltest: exercise the poll syscall against a pipe and report results.
    /// Useful for verifying that fd_readiness reports correct revents.
    fn cmd_polltest(&self) {
        use crate::syscall::poll::{do_poll, fd_readiness, flags, PollFd};
        use crate::syscall::pipe::{create_pipe, pipe_write};

        let pid = match create_pipe() {
            Some(p) => p,
            None => {
                println!("polltest: cannot create pipe");
                return;
            }
        };

        // Empty pipe — POLLIN on the read end should be 0.
        let mut polls = [
            PollFd { fd: 0,  events: flags::POLLIN,  revents: 0 }, // stdin (TTY)
            PollFd { fd: 1,  events: flags::POLLOUT, revents: 0 }, // stdout
            PollFd { fd: 99, events: flags::POLLIN,  revents: 0 }, // bogus
        ];
        let r = do_poll(&mut polls, 0);
        println!("Empty stdin/stdout/bogus poll => {} ready:", r);
        for p in &polls {
            println!("  fd={} events={:#06x} revents={:#06x}",
                p.fd, p.events as u16, p.revents as u16);
        }

        // Stuff a byte into the pipe directly.
        let _ = pipe_write(pid, b"x");
        // Direct readiness check on the pipe (we don't have a stable fd yet).
        // Emulate: ask kernel to peek the kind via fd_readiness on a synthetic
        // FD installed in the pipe path — fall back to printing the helper.
        let pipe_avail = crate::syscall::pipe::pipe_bytes_available(pid);
        let pipe_writeable = crate::syscall::pipe::pipe_writable(pid);
        println!(
            "Pipe id {}: bytes_available={}, writable={}",
            pid, pipe_avail, pipe_writeable
        );
        // fd_readiness on stdin/stdout reflects TTY/console state.
        let readiness_stdin = fd_readiness(0, flags::POLLIN);
        let readiness_stdout = fd_readiness(1, flags::POLLOUT);
        println!(
            "fd_readiness(stdin, POLLIN)  = {:#06x}",
            readiness_stdin as u16
        );
        println!(
            "fd_readiness(stdout, POLLOUT) = {:#06x}",
            readiness_stdout as u16
        );
    }

    /// motd: display the message of the day from /etc/motd
    fn cmd_motd(&self) {
        match crate::fs::vfs::VfsContext::read("/etc/motd") {
            Ok(data) => {
                if let Ok(s) = core::str::from_utf8(&data) {
                    print!("{}", s);
                } else {
                    println!("/etc/motd: not valid UTF-8");
                }
            }
            Err(_) => println!("/etc/motd not found"),
        }
    }

    /// login: prompt for username/password and switch credentials
    fn cmd_login(&mut self, args: &[&str]) {
        // Print /etc/issue header if present
        if let Ok(data) = crate::fs::vfs::VfsContext::read("/etc/issue") {
            if let Ok(s) = core::str::from_utf8(&data) {
                let kernel = "rustos x86_64";
                let host = self.hostname.clone();
                print!("{}", s.replace("\\n", &host).replace("\\l", kernel));
            }
        }

        let user = if !args.is_empty() {
            String::from(args[0])
        } else {
            print!("login: ");
            self.read_line_echoed()
        };

        let entry = {
            let db = crate::users::USER_DB.lock();
            db.get_user(&user).cloned()
        };

        let user_entry = match entry {
            Some(u) => u,
            None => {
                println!("Login incorrect");
                return;
            }
        };

        // Authenticate
        if !user_entry.password_hash.is_empty()
            && user_entry.password_hash != "*"
            && user_entry.password_hash != "!"
        {
            print!("Password: ");
            let pw = self.read_password();
            let db = crate::users::USER_DB.lock();
            if !db.check_password(&user, &pw) {
                println!("Login incorrect");
                return;
            }
        } else if user_entry.password_hash == "*" || user_entry.password_hash == "!" {
            println!("Account is locked");
            return;
        }

        // Switch credentials and home directory
        {
            let mut creds = crate::users::CURRENT_CREDS.lock();
            creds.uid = user_entry.uid;
            creds.gid = user_entry.gid;
            creds.euid = user_entry.uid;
            creds.egid = user_entry.gid;
            creds.username = user_entry.name.clone();
        }
        crate::capability::reseed_for_uid(user_entry.uid);

        if !user_entry.home.is_empty() {
            self.current_dir = user_entry.home.clone();
        }

        // Update USER env var
        for entry in self.env_vars.iter_mut() {
            if entry.0 == "USER" {
                entry.1 = user_entry.name.clone();
            } else if entry.0 == "HOME" {
                entry.1 = user_entry.home.clone();
            }
        }

        // Show MOTD on successful login
        self.cmd_motd();
        println!("Last login: just now on tty0");
    }

    /// logout / exit: drop privileges and re-show login prompt
    fn cmd_logout(&mut self) {
        println!("logout");
        // Drop to "guest" credentials so the next prompt shows the change.
        // Re-login is required to do anything privileged.
        {
            let mut creds = crate::users::CURRENT_CREDS.lock();
            creds.uid = 65534;
            creds.gid = 65534;
            creds.euid = 65534;
            creds.egid = 65534;
            creds.username = alloc::string::String::from("guest");
        }
        crate::capability::reseed_for_uid(65534);
        self.current_dir = String::from("/");
    }

    /// Read a visible line from input (used for username prompt)
    fn read_line_echoed(&mut self) -> String {
        use crate::task::keyboard::{SCANCODE_QUEUE, SERIAL_QUEUE};
        use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};

        let mut keyboard = Keyboard::new(
            ScancodeSet1::new(),
            layouts::Us104Key,
            HandleControl::MapLettersToUnicode,
        );

        let mut buf = String::new();
        loop {
            let mut got_char: Option<char> = None;

            if let Ok(q) = SERIAL_QUEUE.try_get() {
                if let Some(byte) = q.pop() {
                    got_char = match byte {
                        b'\r' | b'\n' => Some('\n'),
                        0x7F | 0x08 => Some('\u{0008}'),
                        b if (0x20..=0x7E).contains(&b) => Some(b as char),
                        _ => None,
                    };
                }
            }

            if got_char.is_none() {
                if let Ok(q) = SCANCODE_QUEUE.try_get() {
                    if let Some(scancode) = q.pop() {
                        if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
                            if let Some(key) = keyboard.process_keyevent(key_event) {
                                if let DecodedKey::Unicode(c) = key {
                                    got_char = Some(c);
                                }
                            }
                        }
                    }
                }
            }

            if let Some(c) = got_char {
                if c == '\n' || c == '\r' {
                    println!();
                    return buf;
                } else if c == '\u{0008}' || c == '\u{007F}' {
                    if !buf.is_empty() {
                        buf.pop();
                        print!("\x08 \x08");
                    }
                } else if c.is_ascii() && !c.is_control() {
                    buf.push(c);
                    print!("{}", c);
                }
            } else {
                x86_64::instructions::interrupts::enable_and_hlt();
            }
        }
    }

    /// vmstat / vmalloc-test: report vmalloc region usage and exercise it.
    fn cmd_vmstat(&self, args: &[&str]) {
        let (total, used, allocs) = crate::vmalloc::stats();
        println!("vmalloc region: {} KiB total, {} KiB used, {} allocations",
            total / 1024, used / 1024, allocs);

        if args.first() == Some(&"test") {
            let bytes: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(64 * 1024);
            match crate::vmalloc::vmalloc(bytes) {
                Ok(p) => {
                    // Touch first / last byte to confirm it's mapped writable.
                    unsafe {
                        p.write(0xAB);
                        p.add(bytes - 1).write(0xCD);
                        let a = p.read();
                        let b = p.add(bytes - 1).read();
                        println!("vmalloc({}) = {:p}, first=0x{:02X}, last=0x{:02X}",
                            bytes, p, a, b);
                    }
                    let (_, used_after, allocs_after) = crate::vmalloc::stats();
                    println!("after alloc: used={} KiB, allocations={}",
                        used_after / 1024, allocs_after);
                    let _ = crate::vmalloc::vfree(p);
                    let (_, used_final, allocs_final) = crate::vmalloc::stats();
                    println!("after free:  used={} KiB, allocations={}",
                        used_final / 1024, allocs_final);
                }
                Err(e) => println!("vmalloc({}) failed: {:?}", bytes, e),
            }
        }
    }

    /// slabinfo: print kernel slab allocator stats (Linux-style /proc/slabinfo)
    fn cmd_slabinfo(&self) {
        let snapshot = crate::slab::SLAB_REGISTRY.snapshot();
        if snapshot.is_empty() {
            println!("slabinfo: no slab caches registered");
            return;
        }
        println!(
            "{:<16} {:>6} {:>8} {:>10} {:>9} {:>9} {:>5} {:>6} {:>5} {:>4}",
            "name", "size", "per_slab", "total_objs", "used_objs", "free_objs",
            "slabs", "allocs", "frees", "grew",
        );
        for (name, st) in snapshot {
            println!(
                "{:<16} {:>6} {:>8} {:>10} {:>9} {:>9} {:>5} {:>6} {:>5} {:>4}",
                name, st.obj_size, st.objs_per_slab, st.total_objs, st.used_objs,
                st.free_objs, st.total_slabs, st.allocs, st.frees, st.grew,
            );
        }
    }

    /// init / telinit: change the runlevel
    fn cmd_init(&mut self, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: init <0..6>");
            return;
        }
        let level = match args[0].parse::<u8>() {
            Ok(n) => match crate::init::RunLevel::from_u8(n) {
                Some(l) => l,
                None => {
                    println!("init: invalid runlevel '{}'", args[0]);
                    return;
                }
            },
            Err(_) => {
                println!("init: invalid runlevel '{}'", args[0]);
                return;
            }
        };

        // Special handling for halt/reboot
        match level {
            crate::init::RunLevel::Halt => {
                println!("System halt requested. Shutting down all services...");
                let _ = crate::init::INIT.lock().set_runlevel(level);
                println!("System halted. Press the power button to reboot.");
                crate::power::shutdown();
            }
            crate::init::RunLevel::Reboot => {
                println!("Reboot requested. Stopping all services...");
                let _ = crate::init::INIT.lock().set_runlevel(level);
                println!("Rebooting...");
                crate::power::reboot();
            }
            _ => {
                let prev = crate::init::INIT.lock().runlevel();
                match crate::init::INIT.lock().set_runlevel(level) {
                    Ok(_) => println!("Switched runlevel: {} -> {}", prev.as_str(), level.as_str()),
                    Err(e) => println!("init: {}", e),
                }
            }
        }
    }
}

/// Build a self-test GPT header + partition array entirely in memory.
/// Lets the partprobe demo exercise the GPT parser without a real disk.
fn synthesise_demo_gpt(hdr_sector: &mut [u8; 512]) -> alloc::vec::Vec<u8> {
    use crate::partition::GPT_SIGNATURE;
    // Two-entry partition array: EFI System (LBA 2048..) + Linux fs.
    let mut entries: alloc::vec::Vec<u8> = alloc::vec![0u8; 32 * 512];

    // EFI System Partition GUID and a Linux-fs GUID (from UEFI Annex A).
    let efi: [u8; 16] = [0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11,
                          0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b];
    let linux_fs: [u8; 16] = [0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47,
                               0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d, 0xe4];

    // Partition 1: EFI System, LBA 2048..104447 (50 MiB).
    entries[0..16].copy_from_slice(&efi);
    entries[32..40].copy_from_slice(&2048u64.to_le_bytes());
    entries[40..48].copy_from_slice(&104447u64.to_le_bytes());
    let efi_name: &str = "EFI";
    let mut name_off = 56;
    for ch in efi_name.encode_utf16() {
        entries[name_off..name_off + 2].copy_from_slice(&ch.to_le_bytes());
        name_off += 2;
    }

    // Partition 2: Linux fs, LBA 104448..1048575 (~460 MiB).
    let p2 = 128usize;
    entries[p2..p2 + 16].copy_from_slice(&linux_fs);
    entries[p2 + 32..p2 + 40].copy_from_slice(&104448u64.to_le_bytes());
    entries[p2 + 40..p2 + 48].copy_from_slice(&1048575u64.to_le_bytes());
    let lfs_name: &str = "rootfs";
    let mut name_off = p2 + 56;
    for ch in lfs_name.encode_utf16() {
        entries[name_off..name_off + 2].copy_from_slice(&ch.to_le_bytes());
        name_off += 2;
    }

    let entries_crc = crate::partition::crc32(&entries);

    // Now build the header.
    for b in hdr_sector.iter_mut() { *b = 0; }
    hdr_sector[0..8].copy_from_slice(&GPT_SIGNATURE.to_le_bytes());
    hdr_sector[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes()); // rev 1.0
    hdr_sector[12..16].copy_from_slice(&92u32.to_le_bytes());        // header size
    // CRC32 of header — left zero, computed below.
    hdr_sector[24..32].copy_from_slice(&1u64.to_le_bytes());          // current LBA
    hdr_sector[32..40].copy_from_slice(&0xFFFF_FFFF_FFFF_FFFEu64.to_le_bytes()); // backup
    hdr_sector[40..48].copy_from_slice(&34u64.to_le_bytes());         // first usable
    hdr_sector[48..56].copy_from_slice(&1048574u64.to_le_bytes());    // last usable
    // Disk GUID — fixed for the demo.
    let disk_guid: [u8; 16] = *b"\x12\x34\x56\x78\x9a\xbc\xde\xf0RUSTOSG\x01";
    hdr_sector[56..72].copy_from_slice(&disk_guid);
    hdr_sector[72..80].copy_from_slice(&2u64.to_le_bytes());          // partition entry LBA
    hdr_sector[80..84].copy_from_slice(&128u32.to_le_bytes());        // num entries
    hdr_sector[84..88].copy_from_slice(&128u32.to_le_bytes());        // entry size
    hdr_sector[88..92].copy_from_slice(&entries_crc.to_le_bytes());   // entries CRC32

    // Recompute the header CRC32 with the CRC field zeroed.
    let crc = crate::partition::crc32(&hdr_sector[..92]);
    hdr_sector[16..20].copy_from_slice(&crc.to_le_bytes());

    entries
}

/// Parse "aa:bb:cc:dd:ee:ff" into a [u8; 6].
fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut parts = s.split(':');
    for slot in out.iter_mut() {
        let p = parts.next()?;
        if p.len() != 2 { return None; }
        *slot = u8::from_str_radix(p, 16).ok()?;
    }
    if parts.next().is_some() { return None; }
    Some(out)
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
