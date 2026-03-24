/// Simple line-based text editor
use alloc::vec::Vec;
use alloc::string::{String, ToString};
use crate::{print, println};
use crate::fs::vfs::{VfsContext, VfsError};
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
use spin::Mutex;

static KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> = Mutex::new(
    Keyboard::new(ScancodeSet1::new(), layouts::Us104Key, HandleControl::Ignore)
);

pub struct Editor {
    lines: Vec<String>,
    filename: String,
    modified: bool,
}

impl Editor {
    pub fn new(filename: &str) -> Self {
        let mut lines = Vec::new();

        // Try to load existing file via VFS (FAT32 or RAMDISK)
        if let Ok(content) = VfsContext::read(filename) {
            if let Ok(text) = core::str::from_utf8(&content) {
                for line in text.lines() {
                    lines.push(line.to_string());
                }
                if lines.is_empty() {
                    lines.push(String::new());
                }
            } else {
                println!("Warning: File is binary, starting with empty buffer");
                lines.push(String::new());
            }
        } else {
            // New file
            lines.push(String::new());
        }

        Editor {
            lines,
            filename: filename.to_string(),
            modified: false,
        }
    }

    pub fn run(&mut self) {
        println!("RustOS Editor - File: {}", self.filename);
        println!("Commands: (i)nsert, (a)ppend, (d)elete, (p)rint, (w)rite, (q)uit, (h)elp");
        println!();

        self.print_all();
        println!();

        // Main editor loop
        loop {
            print!(": ");

            let command = match self.read_line() {
                Some(line) => line,
                None => continue,
            };

            let command = command.trim();
            if command.is_empty() {
                continue;
            }

            // Parse command
            let parts: Vec<&str> = command.splitn(3, ' ').collect();
            let cmd = parts[0];

            match cmd {
                "h" | "help" => print_help(),
                "q" | "quit" => {
                    if self.modified {
                        println!("Warning: File modified but not saved!");
                        println!("Use 'w' to save, or 'q!' to quit without saving");
                    } else {
                        break;
                    }
                }
                "q!" => break,
                "p" | "print" => {
                    if parts.len() > 1 {
                        if let Ok(line_num) = parts[1].parse::<usize>() {
                            self.print_line(line_num);
                        } else {
                            println!("Error: Invalid line number");
                        }
                    } else {
                        self.print_all();
                    }
                }
                "a" | "append" => {
                    if parts.len() > 1 {
                        let text = parts[1..].join(" ");
                        self.append_line(text);
                        println!("Line appended");
                    } else {
                        println!("Usage: a <text>");
                    }
                }
                "i" | "insert" => {
                    if parts.len() > 2 {
                        if let Ok(line_num) = parts[1].parse::<usize>() {
                            let text = parts[2..].join(" ");
                            self.insert_line(line_num - 1, text);
                            println!("Line inserted");
                        } else {
                            println!("Error: Invalid line number");
                        }
                    } else {
                        println!("Usage: i <line> <text>");
                    }
                }
                "e" | "edit" => {
                    if parts.len() > 2 {
                        if let Ok(line_num) = parts[1].parse::<usize>() {
                            let text = parts[2..].join(" ");
                            self.replace_line(line_num, text);
                            println!("Line replaced");
                        } else {
                            println!("Error: Invalid line number");
                        }
                    } else {
                        println!("Usage: e <line> <text>");
                    }
                }
                "d" | "delete" => {
                    if parts.len() > 1 {
                        if let Ok(line_num) = parts[1].parse::<usize>() {
                            self.delete_line(line_num);
                        } else {
                            println!("Error: Invalid line number");
                        }
                    } else {
                        println!("Usage: d <line>");
                    }
                }
                "w" | "write" | "save" => {
                    match self.save() {
                        Ok(_) => {},
                        Err(e) => println!("Error saving: {:?}", e),
                    }
                }
                _ => {
                    println!("Unknown command: '{}'. Type 'h' for help.", cmd);
                }
            }
        }

        println!("Editor closed.");
    }

    /// Read a line of input from keyboard
    fn read_line(&self) -> Option<String> {
        use crate::task::keyboard::SCANCODE_QUEUE;

        let mut line = String::new();

        loop {
            // Poll scancode queue
            if let Ok(queue) = SCANCODE_QUEUE.try_get() {
                if let Some(scancode) = queue.pop() {
                    let mut keyboard = KEYBOARD.lock();

                    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
                        if let Some(key) = keyboard.process_keyevent(key_event) {
                            match key {
                                DecodedKey::Unicode(character) => {
                                    if character == '\n' {
                                        println!();
                                        return Some(line);
                                    } else if character == '\x08' {
                                        // Backspace
                                        if !line.is_empty() {
                                            line.pop();
                                            print!("\x08 \x08");
                                        }
                                    } else if character >= ' ' && character <= '~' {
                                        line.push(character);
                                        print!("{}", character);
                                    }
                                }
                                DecodedKey::RawKey(_) => {}
                            }
                        }
                    }

                    drop(keyboard);
                }
            }

            // Yield CPU to prevent busy-waiting
            x86_64::instructions::hlt();
        }
    }

    pub fn print_all(&self) {
        for (i, line) in self.lines.iter().enumerate() {
            println!("{:3}: {}", i + 1, line);
        }
        if self.lines.is_empty() {
            println!("(empty file)");
        }
    }

    pub fn print_line(&self, line_num: usize) {
        if line_num > 0 && line_num <= self.lines.len() {
            println!("{:3}: {}", line_num, self.lines[line_num - 1]);
        } else {
            println!("Error: Invalid line number");
        }
    }

    pub fn insert_line(&mut self, line_num: usize, content: String) {
        if line_num == 0 {
            self.lines.insert(0, content);
        } else if line_num <= self.lines.len() {
            self.lines.insert(line_num, content);
        } else {
            self.lines.push(content);
        }
        self.modified = true;
    }

    pub fn append_line(&mut self, content: String) {
        self.lines.push(content);
        self.modified = true;
    }

    pub fn delete_line(&mut self, line_num: usize) {
        if line_num > 0 && line_num <= self.lines.len() {
            self.lines.remove(line_num - 1);
            self.modified = true;
            println!("Deleted line {}", line_num);
        } else {
            println!("Error: Invalid line number");
        }
    }

    pub fn replace_line(&mut self, line_num: usize, content: String) {
        if line_num > 0 && line_num <= self.lines.len() {
            self.lines[line_num - 1] = content;
            self.modified = true;
        } else {
            println!("Error: Invalid line number");
        }
    }

    pub fn save(&mut self) -> Result<(), VfsError> {
        let mut content = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            content.push_str(line);
            if i < self.lines.len() - 1 {
                content.push('\n');
            }
        }

        let bytes = content.as_bytes().to_vec();
        VfsContext::write(&self.filename, bytes)?;

        self.modified = false;
        println!("Saved {} lines to '{}'", self.lines.len(), self.filename);
        Ok(())
    }

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
}

pub fn print_help() {
    println!("Editor Commands:");
    println!("  h          - Show this help");
    println!("  p          - Print all lines");
    println!("  p N        - Print line N");
    println!("  i N <text> - Insert <text> before line N");
    println!("  a <text>   - Append <text> at end");
    println!("  d N        - Delete line N");
    println!("  e N <text> - Edit (replace) line N with <text>");
    println!("  w          - Write (save) file");
    println!("  q          - Quit editor (warns if unsaved)");
    println!("  q!         - Quit without saving");
    println!();
    println!("Examples:");
    println!("  a Hello World    - Add 'Hello World' at end");
    println!("  i 1 First line   - Insert 'First line' at beginning");
    println!("  e 2 New text     - Replace line 2 with 'New text'");
    println!("  d 3              - Delete line 3");
}
