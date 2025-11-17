#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rustos::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;

// Import VGA macros
#[macro_use]
extern crate rustos;

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
#[no_mangle]
pub extern "C" fn _start() -> ! {
    use rustos::serial_println;

    serial_println!("[DEBUG] Starting kernel...");
    println!("Hello World from RustOS!");
    println!("This is a minimal kernel written in Rust.");
    println!();
    println!("Based on Phil Opp's excellent tutorials:");
    println!("  https://os.phil-opp.com/");
    println!();

    serial_println!("[DEBUG] About to initialize interrupts...");
    // Initialize interrupts and PIC
    rustos::init();
    serial_println!("[DEBUG] Interrupts initialized!");

    println!("IDT and PIC initialized");
    println!("Hardware interrupts enabled");
    println!();
    println!("Kernel is running successfully!");
    println!("Timer ticks: . and keyboard input should appear below");
    println!();

    serial_println!("[DEBUG] Entering hlt loop...");

    #[cfg(test)]
    test_main();

    rustos::hlt_loop();
}

#[test_case]
fn trivial_assertion() {
    assert_eq!(1, 1);
}
