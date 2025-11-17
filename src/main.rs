#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rustos::test_runner)]
#![reexport_test_harness_main = "test_main"]

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
    use x86_64::{structures::paging::Translate, VirtAddr};

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
    let mapper = unsafe { memory::init(phys_mem_offset) };
    let _frame_allocator = unsafe {
        memory::BootInfoFrameAllocator::init(&boot_info.memory_map)
    };

    println!("Kernel initialized successfully!");
    println!();

    // Demonstrate address translation
    println!("Paging demonstration:");

    // Translate some addresses
    let addresses = [
        // VGA buffer
        0xb8000,
        // Some code address
        0x201008,
        // Some stack address
        0x0100_0020_1a10,
        // Virtual address mapped to physical address 0
        boot_info.physical_memory_offset,
    ];

    for &address in &addresses {
        let virt = VirtAddr::new(address);
        let phys = mapper.translate_addr(virt);
        println!("  {:?} -> {:?}", virt, phys);
    }

    println!();
    println!("Type something on your keyboard:");
    println!();

    #[cfg(test)]
    test_main();

    rustos::hlt_loop();
}

#[test_case]
fn trivial_assertion() {
    assert_eq!(1, 1);
}
