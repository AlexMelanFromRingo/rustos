#![no_std]
#![no_main]

use core::panic::PanicInfo;
use rustos::{qemu, serial_print, serial_println};
use bootloader::{entry_point, BootInfo};

entry_point!(test_kernel_main);

fn test_kernel_main(_boot_info: &'static BootInfo) -> ! {
    should_fail();
    serial_println!("[test did not panic]");
    qemu::exit_qemu(qemu::QemuExitCode::Failed);

    loop {}
}

fn should_fail() {
    serial_print!("should_panic::should_fail...\t");
    assert_eq!(0, 1);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    serial_println!("[ok]");
    qemu::exit_qemu(qemu::QemuExitCode::Success);
    loop {}
}
