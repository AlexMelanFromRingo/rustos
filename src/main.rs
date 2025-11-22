#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rustos::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use core::panic::PanicInfo;
use bootloader::{BootInfo, entry_point};

// Import VGA macros
#[macro_use]
extern crate rustos;

entry_point!(kernel_main);

/// This function is called on panic.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    rustos::hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rustos::test_panic_handler(info)
}

/// Entry point for our kernel.
/// The bootloader will call this function.
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    use rustos::memory;
    use x86_64::VirtAddr;

    println!("RustOS - A minimal operating system written in Rust");
    println!("Based on Phil Opp's excellent tutorials");
    println!("  https://os.phil-opp.com/");
    println!();

    // Initialize GDT, IDT and PIC
    rustos::init();

    // Initialize memory management
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe {
        memory::BootInfoFrameAllocator::init(&boot_info.memory_map)
    };

    // Initialize heap
    rustos::allocator::init_heap(&mut mapper, &mut frame_allocator)
        .expect("heap initialization failed");

    // Initialize ATA driver AFTER heap is ready
    rustos::drivers::ata::init();

    // Initialize syscall support (after heap and GDT)
    rustos::init_syscall();

    println!("Kernel initialized successfully!");
    println!();

    #[cfg(test)]
    test_main();

    println!("Starting RustOS Shell...");
    println!("Type 'help' for available commands");
    println!();

    let mut executor = rustos::task::executor::Executor::new();
    executor.spawn(rustos::task::Task::new(keyboard_task()));
    executor.spawn(rustos::task::Task::new(status_task()));
    executor.run();
}

async fn status_task() {
    use rustos::task::timer::Timer;

    let mut counter = 0u64;

    loop {
        // Wait ~3 seconds (assuming ~18.2 Hz timer)
        Timer::new(54).await;

        counter += 1;
        if counter % 5 == 0 {
            println!("[Info] System running... ({} updates)", counter);
        }
    }
}

/// Try to mount FAT32 filesystem automatically at startup
fn try_mount_fat32() -> Result<(), &'static str> {
    use rustos::fs::fat32::{Fat32, FAT32};

    // Try to create FAT32 instance (this reads boot sector and validates)
    let fat32_fs = Fat32::new()?;

    // Store in global singleton
    let mut fat32 = FAT32.lock();
    *fat32 = Some(fat32_fs);
    drop(fat32);

    Ok(())
}

async fn keyboard_task() {
    use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
    use futures_util::stream::StreamExt;
    use rustos::task::keyboard::ScancodeStream;
    use rustos::shell::Shell;
    use rustos::vga_buffer::WRITER;
    use x86_64::instructions::interrupts;

    // Try to auto-mount FAT32 at startup
    match try_mount_fat32() {
        Ok(()) => {
            println!("FAT32 filesystem mounted successfully!");
            println!("Files and command history will persist across reboots.");
        }
        Err(e) => {
            println!("Could not mount FAT32: {}", e);
            println!("Using RAM disk (data will be lost on reboot).");
        }
    }
    println!();
    println!("Active filesystem: {}", rustos::fs::vfs::VfsContext::filesystem_name());
    println!();

    let mut scancodes = ScancodeStream::new();
    let mut keyboard = Keyboard::new(
        ScancodeSet1::new(),
        layouts::Us104Key,
        HandleControl::MapLettersToUnicode,
    );

    let mut shell = Shell::new();
    shell.print_prompt();

    while let Some(scancode) = scancodes.next().await {
        if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
            if let Some(key) = keyboard.process_keyevent(key_event) {
                match key {
                    DecodedKey::Unicode(character) => {
                        if character == '\n' {
                            println!();
                            shell.execute();
                            shell.print_prompt();
                        } else if character == '\u{0001}' {
                            // Ctrl+A - Move to beginning of line
                            shell.move_cursor_home();
                            interrupts::without_interrupts(|| {
                                let mut writer = WRITER.lock();
                                writer.set_cursor_column(2);
                            });
                        } else if character == '\u{0005}' {
                            // Ctrl+E - Move to end of line
                            let buffer_len = shell.buffer_len();
                            shell.move_cursor_end();
                            interrupts::without_interrupts(|| {
                                let mut writer = WRITER.lock();
                                writer.set_cursor_column(2 + buffer_len);
                            });
                        } else if character == '\u{0015}' {
                            // Ctrl+U - Delete from cursor to beginning
                            let old_len = shell.buffer_len();
                            if shell.delete_to_beginning() {
                                let (_clear_len, new_text) = shell.redraw_line();
                                let new_len = new_text.len();

                                interrupts::without_interrupts(|| {
                                    let mut writer = WRITER.lock();
                                    // Move cursor to start of buffer (after prompt "> ")
                                    writer.set_cursor_column(2);
                                    drop(writer);

                                    // Print new text
                                    print!("{}", new_text);
                                    // Clear remaining old characters
                                    for _ in 0..(old_len - new_len) {
                                        print!(" ");
                                    }

                                    // Set cursor to beginning (after prompt)
                                    writer = WRITER.lock();
                                    writer.set_cursor_column(2);
                                });
                            }
                        } else if character == '\u{0017}' {
                            // Ctrl+W - Delete word backward
                            let old_len = shell.buffer_len();
                            if shell.delete_word_backward() {
                                let (_clear_len, new_text) = shell.redraw_line();
                                let new_len = new_text.len();
                                let cursor_pos = shell.get_cursor_pos();

                                interrupts::without_interrupts(|| {
                                    let mut writer = WRITER.lock();
                                    // Move cursor to start of buffer (after prompt "> ")
                                    writer.set_cursor_column(2);
                                    drop(writer);

                                    // Print new text
                                    print!("{}", new_text);
                                    // Clear remaining old characters
                                    for _ in 0..(old_len - new_len) {
                                        print!(" ");
                                    }

                                    // Set cursor to correct position
                                    writer = WRITER.lock();
                                    writer.set_cursor_column(2 + cursor_pos);
                                });
                            }
                        } else if character == '\u{0008}' {
                            // Backspace as Unicode character - use absolute positioning
                            let old_len = shell.buffer_len();
                            if shell.backspace() {
                                let (_clear_len, new_text) = shell.redraw_line();
                                let new_len = new_text.len();
                                let cursor_pos = shell.get_cursor_pos();

                                interrupts::without_interrupts(|| {
                                    let mut writer = WRITER.lock();
                                    // Move cursor to start of buffer (after prompt "> ")
                                    writer.set_cursor_column(2);
                                    drop(writer);

                                    // Print new text
                                    print!("{}", new_text);
                                    // Clear remaining old characters
                                    for _ in 0..(old_len - new_len) {
                                        print!(" ");
                                    }

                                    // Set cursor to correct position
                                    writer = WRITER.lock();
                                    writer.set_cursor_column(2 + cursor_pos);
                                });
                            }
                        } else if character == '\t' {
                            // Tab for autocomplete - use absolute positioning
                            let old_len = shell.buffer_len();
                            if let Some(completed) = shell.autocomplete() {
                                let new_len = completed.len();

                                interrupts::without_interrupts(|| {
                                    let mut writer = WRITER.lock();
                                    // Move cursor to start of buffer (after prompt "> ")
                                    writer.set_cursor_column(2);
                                    drop(writer);

                                    // Print completed command
                                    print!("{}", completed);
                                    // Clear remaining old characters if new is shorter
                                    if new_len < old_len {
                                        for _ in 0..(old_len - new_len) {
                                            print!(" ");
                                        }
                                    }

                                    // Set cursor to end of completed text
                                    writer = WRITER.lock();
                                    writer.set_cursor_column(2 + new_len);
                                });
                                shell.set_buffer(completed);
                            }
                        } else if character >= ' ' && character <= '~' {
                            // Only printable ASCII - insert at cursor position
                            let old_len = shell.buffer_len();
                            shell.add_char(character);
                            let cursor_pos = shell.get_cursor_pos();

                            // Redraw entire buffer
                            let (_clear_len, new_text) = shell.redraw_line();
                            let new_len = new_text.len();

                            interrupts::without_interrupts(|| {
                                let mut writer = WRITER.lock();
                                // Move cursor to start of buffer (after prompt "> ")
                                writer.set_cursor_column(2);
                                drop(writer);

                                // Print new text
                                print!("{}", new_text);
                                // Clear remaining old characters if any
                                if old_len > new_len {
                                    for _ in 0..(old_len - new_len) {
                                        print!(" ");
                                    }
                                }

                                // Set cursor to correct position (prompt + cursor_pos)
                                writer = WRITER.lock();
                                writer.set_cursor_column(2 + cursor_pos);
                            });
                        }
                        // Ignore other control characters
                    }
                    DecodedKey::RawKey(key_code) => {
                        use pc_keyboard::KeyCode;
                        match key_code {
                            KeyCode::Backspace => {
                                // Backspace at cursor position
                                let old_len = shell.buffer_len();
                                if shell.backspace() {
                                    let (_clear_len, new_text) = shell.redraw_line();
                                    let new_len = new_text.len();
                                    let cursor_pos = shell.get_cursor_pos();

                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Move cursor to start of buffer (after prompt "> ")
                                        writer.set_cursor_column(2);
                                        drop(writer);

                                        // Print new text
                                        print!("{}", new_text);
                                        // Clear remaining old characters
                                        for _ in 0..(old_len - new_len) {
                                            print!(" ");
                                        }

                                        // Set cursor to correct position
                                        writer = WRITER.lock();
                                        writer.set_cursor_column(2 + cursor_pos);
                                    });
                                }
                            }
                            KeyCode::Delete => {
                                // Delete character at cursor
                                let old_len = shell.buffer_len();
                                if shell.delete_char() {
                                    let (_clear_len, new_text) = shell.redraw_line();
                                    let new_len = new_text.len();
                                    let cursor_pos = shell.get_cursor_pos();

                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Move cursor to start of buffer (after prompt "> ")
                                        writer.set_cursor_column(2);
                                        drop(writer);

                                        // Print new text
                                        print!("{}", new_text);
                                        // Clear remaining old characters
                                        for _ in 0..(old_len - new_len) {
                                            print!(" ");
                                        }

                                        // Set cursor to correct position
                                        writer = WRITER.lock();
                                        writer.set_cursor_column(2 + cursor_pos);
                                    });
                                }
                            }
                            KeyCode::ArrowLeft => {
                                // Move cursor left using absolute positioning
                                if shell.move_cursor_left() {
                                    let cursor_pos = shell.get_cursor_pos();
                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Set cursor to absolute position (prompt + cursor_pos)
                                        writer.set_cursor_column(2 + cursor_pos);
                                    });
                                }
                            }
                            KeyCode::ArrowRight => {
                                // Move cursor right using absolute positioning
                                if shell.move_cursor_right() {
                                    let cursor_pos = shell.get_cursor_pos();
                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Set cursor to absolute position (prompt + cursor_pos)
                                        writer.set_cursor_column(2 + cursor_pos);
                                    });
                                }
                            }
                            KeyCode::Home => {
                                // Move to start of line
                                shell.move_cursor_home();
                                // Set VGA cursor to prompt position (2 chars for "> ")
                                interrupts::without_interrupts(|| {
                                    let mut writer = WRITER.lock();
                                    writer.set_cursor_column(2);
                                });
                            }
                            KeyCode::End => {
                                // Move to end of line
                                let buffer_len = shell.buffer_len();
                                shell.move_cursor_end();
                                // Set VGA cursor to prompt + buffer length
                                interrupts::without_interrupts(|| {
                                    let mut writer = WRITER.lock();
                                    writer.set_cursor_column(2 + buffer_len);
                                });
                            }
                            KeyCode::ArrowUp => {
                                let old_len = shell.buffer_len();
                                if let Some(cmd) = shell.history_up() {
                                    let new_len = cmd.len();

                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Move cursor to start of buffer (after prompt "> ")
                                        writer.set_cursor_column(2);
                                        drop(writer);

                                        // Print new command
                                        print!("{}", cmd);
                                        // Clear remaining old characters if new is shorter
                                        if new_len < old_len {
                                            for _ in 0..(old_len - new_len) {
                                                print!(" ");
                                            }
                                        }

                                        // Set cursor to end of new command
                                        writer = WRITER.lock();
                                        writer.set_cursor_column(2 + new_len);
                                    });
                                    shell.set_buffer(cmd);
                                }
                            }
                            KeyCode::ArrowDown => {
                                let old_len = shell.buffer_len();
                                if let Some(cmd) = shell.history_down() {
                                    let new_len = cmd.len();

                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Move cursor to start of buffer (after prompt "> ")
                                        writer.set_cursor_column(2);
                                        drop(writer);

                                        // Print new command
                                        print!("{}", cmd);
                                        // Clear remaining old characters if new is shorter
                                        if new_len < old_len {
                                            for _ in 0..(old_len - new_len) {
                                                print!(" ");
                                            }
                                        }

                                        // Set cursor to end of new command
                                        writer = WRITER.lock();
                                        writer.set_cursor_column(2 + new_len);
                                    });
                                    shell.set_buffer(cmd);
                                }
                            }
                            KeyCode::Tab => {
                                // Autocomplete command or filename
                                let old_len = shell.buffer_len();
                                if let Some(completed) = shell.autocomplete() {
                                    let new_len = completed.len();

                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Move cursor to start of buffer (after prompt "> ")
                                        writer.set_cursor_column(2);
                                        drop(writer);

                                        // Print completed command
                                        print!("{}", completed);
                                        // Clear remaining old characters if new is shorter
                                        if new_len < old_len {
                                            for _ in 0..(old_len - new_len) {
                                                print!(" ");
                                            }
                                        }

                                        // Set cursor to end of completed text
                                        writer = WRITER.lock();
                                        writer.set_cursor_column(2 + new_len);
                                    });
                                    shell.set_buffer(completed);
                                }
                            }
                            _ => {} // Ignore other special keys
                        }
                    }
                }
            }
        }
    }
}

#[test_case]
fn trivial_assertion() {
    assert_eq!(1, 1);
}
