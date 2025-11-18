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

    println!("Hello World from RustOS!");
    println!("This is a minimal kernel written in Rust.");
    println!();
    println!("Based on Phil Opp's excellent tutorials:");
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

    println!("Kernel initialized successfully!");
    println!("Power management: shutdown and reboot available");
    println!();

    // Demonstrate heap allocation
    println!("Heap allocation tests:");

    // Test Box
    use alloc::boxed::Box;
    let heap_value = Box::new(41);
    println!("  Box test: heap value at {:p} = {}", heap_value, *heap_value);

    // Test Vec
    use alloc::vec::Vec;
    let mut vec = Vec::new();
    for i in 0..10 {
        vec.push(i);
    }
    println!("  Vec test: created vector with {} elements", vec.len());

    // Test String
    use alloc::string::String;
    let mut string = String::from("Hello from the heap!");
    string.push_str(" Heap allocation works!");
    println!("  String test: {}", string);

    println!();
    println!("All heap allocations successful!");
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

async fn keyboard_task() {
    use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
    use futures_util::stream::StreamExt;
    use rustos::task::keyboard::ScancodeStream;
    use rustos::shell::Shell;
    use rustos::vga_buffer::WRITER;
    use x86_64::instructions::interrupts;

    let mut scancodes = ScancodeStream::new();
    let mut keyboard = Keyboard::new(
        ScancodeSet1::new(),
        layouts::Us104Key,
        HandleControl::Ignore,
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
                        } else if character == '\u{0008}' {
                            // Backspace as Unicode character
                            if shell.backspace() {
                                // Redraw line
                                let (_clear_len, new_text) = shell.redraw_line();
                                // Clear old line
                                for _ in 0..new_text.len() + 1 {
                                    interrupts::without_interrupts(|| {
                                        WRITER.lock().write_byte(0x08);
                                    });
                                }
                                // Print new text and position cursor
                                print!("{}", new_text);
                                let cursor_pos = shell.get_cursor_pos();
                                let chars_to_left = new_text.len() - cursor_pos;
                                for _ in 0..chars_to_left {
                                    interrupts::without_interrupts(|| {
                                        WRITER.lock().write_byte(0x08);
                                    });
                                }
                            }
                        } else if character == '\t' {
                            // Tab for autocomplete
                            if let Some(completed) = shell.autocomplete() {
                                // Clear current buffer visually
                                let old_len = shell.buffer_len();
                                for _ in 0..old_len {
                                    interrupts::without_interrupts(|| {
                                        WRITER.lock().write_byte(0x08);
                                    });
                                }
                                // Print completed command
                                print!("{}", completed);
                                shell.set_buffer(completed);
                            }
                        } else if character >= ' ' && character <= '~' {
                            // Only printable ASCII - insert at cursor position
                            let old_len = shell.buffer_len();
                            shell.add_char(character);
                            let cursor_pos = shell.get_cursor_pos();

                            // Redraw from cursor position
                            let (_clear_len, new_text) = shell.redraw_line();

                            interrupts::without_interrupts(|| {
                                let mut writer = WRITER.lock();
                                // Clear old content
                                for _ in 0..old_len {
                                    writer.move_cursor_left();
                                }
                                // Print new text
                                drop(writer);
                                print!("{}", new_text);

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
                                if shell.backspace() {
                                    let (_clear_len, new_text) = shell.redraw_line();
                                    let cursor_pos = shell.get_cursor_pos();

                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Move back to start
                                        for _ in 0..new_text.len() + 1 {
                                            writer.move_cursor_left();
                                        }
                                        drop(writer);

                                        // Print new text + space to clear last char
                                        print!("{} ", new_text);

                                        // Set cursor to correct position
                                        writer = WRITER.lock();
                                        writer.set_cursor_column(2 + cursor_pos);
                                    });
                                }
                            }
                            KeyCode::Delete => {
                                // Delete character at cursor
                                if shell.delete_char() {
                                    let (_clear_len, new_text) = shell.redraw_line();
                                    let cursor_pos = shell.get_cursor_pos();

                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Move back to start
                                        for _ in 0..new_text.len() + 1 {
                                            writer.move_cursor_left();
                                        }
                                        drop(writer);

                                        // Print new text + space to clear last char
                                        print!("{} ", new_text);

                                        // Set cursor to correct position
                                        writer = WRITER.lock();
                                        writer.set_cursor_column(2 + cursor_pos);
                                    });
                                }
                            }
                            KeyCode::ArrowLeft => {
                                // Move cursor left without erasing
                                if shell.move_cursor_left() {
                                    interrupts::without_interrupts(|| {
                                        WRITER.lock().move_cursor_left();
                                    });
                                }
                            }
                            KeyCode::ArrowRight => {
                                // Move cursor right without writing
                                if shell.move_cursor_right() {
                                    interrupts::without_interrupts(|| {
                                        WRITER.lock().move_cursor_right();
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
                                if let Some(cmd) = shell.history_up() {
                                    // Clear current line
                                    let buffer_len = shell.buffer_len();
                                    for _ in 0..buffer_len {
                                        interrupts::without_interrupts(|| {
                                            WRITER.lock().write_byte(0x08);
                                        });
                                    }
                                    // Print new command
                                    print!("{}", cmd);
                                    shell.set_buffer(cmd);
                                }
                            }
                            KeyCode::ArrowDown => {
                                if let Some(cmd) = shell.history_down() {
                                    // Clear current line
                                    let buffer_len = shell.buffer_len();
                                    for _ in 0..buffer_len {
                                        interrupts::without_interrupts(|| {
                                            WRITER.lock().write_byte(0x08);
                                        });
                                    }
                                    // Print new command
                                    print!("{}", cmd);
                                    shell.set_buffer(cmd);
                                }
                            }
                            KeyCode::Tab => {
                                // Autocomplete command
                                if let Some(completed) = shell.autocomplete() {
                                    // Clear current buffer visually
                                    let old_len = shell.buffer_len();
                                    for _ in 0..old_len {
                                        interrupts::without_interrupts(|| {
                                            WRITER.lock().write_byte(0x08);
                                        });
                                    }
                                    // Print completed command
                                    print!("{}", completed);
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
