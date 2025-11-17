#![no_std]
#![no_main]

use core::panic::PanicInfo;

mod vga_buffer;

/// This function is called on panic.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    loop {}
}

/// Entry point for our kernel.
/// The bootloader will call this function.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!("Hello World from RustOS!");
    println!("This is a minimal kernel written in Rust.");
    println!();
    println!("Based on Phil Opp's excellent tutorials:");
    println!("  https://os.phil-opp.com/");
    println!();
    println!("Kernel is running successfully!");

    loop {}
}
