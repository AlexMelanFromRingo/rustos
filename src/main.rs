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
                        } else if character >= ' ' && character <= '~' {
                            // Only accept printable ASCII characters
                            print!("{}", character);
                            shell.add_char(character);
                        }
                        // Silently ignore control characters and escape sequences
                    }
                    DecodedKey::RawKey(key_code) => {
                        use pc_keyboard::KeyCode;
                        match key_code {
                            KeyCode::Backspace => {
                                interrupts::without_interrupts(|| {
                                    WRITER.lock().write_byte(0x08);
                                });
                                shell.backspace();
                            }
                            KeyCode::ArrowUp => {
                                if let Some(cmd) = shell.history_up() {
                                    // Clear current line
                                    let buffer_len = shell.get_buffer().len();
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
                                    let buffer_len = shell.get_buffer().len();
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
