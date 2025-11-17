/// Power management module for shutdown and reboot
use x86_64::instructions::port::Port;

/// Shutdown the system using ACPI
///
/// This uses the QEMU/Bochs power management port
pub fn shutdown() -> ! {
    unsafe {
        // QEMU/Bochs ACPI shutdown port
        let mut port = Port::new(0x604);
        port.write(0x2000u16);
    }

    // If shutdown failed, halt
    loop {
        x86_64::instructions::hlt();
    }
}

/// Reboot the system using keyboard controller
///
/// This uses the PS/2 controller reset method
pub fn reboot() -> ! {
    unsafe {
        // Try using keyboard controller (8042) reset
        let mut port = Port::new(0x64);

        // Wait for controller to be ready
        loop {
            let status: u8 = port.read();
            if (status & 0x02) == 0 {
                break;
            }
        }

        // Send reset command
        port.write(0xFEu8);
    }

    // If reboot failed, halt
    loop {
        x86_64::instructions::hlt();
    }
}
