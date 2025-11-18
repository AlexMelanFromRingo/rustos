/// Simple line-based text editor
use alloc::vec::Vec;
use alloc::string::{String, ToString};
use crate::println;
use crate::fs::ramdisk::RAMDISK;
use crate::fs::vfs::{FileSystem, VfsError};

pub struct Editor {
    lines: Vec<String>,
    filename: String,
    modified: bool,
}

impl Editor {
    pub fn new(filename: &str) -> Self {
        let mut lines = Vec::new();

        // Try to load existing file
        let ramdisk = RAMDISK.lock();
        if let Ok(content) = ramdisk.read(filename) {
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

        drop(ramdisk);

        Editor {
            lines,
            filename: filename.to_string(),
            modified: false,
        }
    }

    pub fn run(&mut self) {
        println!("RustOS Editor - File: {}", self.filename);
        println!("Commands: (i)nsert, (a)ppend, (d)elete, (p)rint, (w)rite, (q)uit, (h)elp");
        println!("Type line number to edit that line");
        println!();

        self.print_all();

        println!("\nEditor ready. Type 'h' for help, 'q' to quit.");
        println!("Note: This is a demonstration. In real shell, use interactive commands.");
        println!("For now, use: write <file> <content>");
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
        let mut ramdisk = RAMDISK.lock();
        ramdisk.write(&self.filename, bytes)?;
        drop(ramdisk);

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
    println!("  q          - Quit editor");
    println!();
    println!("Examples:");
    println!("  a Hello World    - Add 'Hello World' at end");
    println!("  i 1 First line   - Insert 'First line' at beginning");
    println!("  e 2 New text     - Replace line 2 with 'New text'");
    println!("  d 3              - Delete line 3");
}
