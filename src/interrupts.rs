use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};
use lazy_static::lazy_static;
use crate::println;
use pic8259::ChainedPics;
use spin;

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: spin::Mutex<ChainedPics> =
    spin::Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
    // Add more IRQs as needed
    PrimaryATA = PIC_2_OFFSET + 6,   // IRQ14 (0x2E = 46)
    SecondaryATA = PIC_2_OFFSET + 7, // IRQ15 (0x2F = 47)
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }

    fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        unsafe {
            idt.double_fault.set_handler_fn(double_fault_handler)
                .set_stack_index(crate::gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        idt[InterruptIndex::Timer.as_usize()]
            .set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_usize()]
            .set_handler_fn(keyboard_interrupt_handler);
        idt[InterruptIndex::PrimaryATA.as_usize()]
            .set_handler_fn(primary_ata_interrupt_handler);
        idt[InterruptIndex::SecondaryATA.as_usize()]
            .set_handler_fn(secondary_ata_interrupt_handler);
        idt
    };
}

pub fn init_idt() {
    IDT.load();
}

pub fn init_pics() {
    unsafe {
        PICS.lock().initialize();
        // Unmask all interrupts (set mask to 0)
        PICS.lock().write_masks(0, 0);
    }
}

extern "x86-interrupt" fn breakpoint_handler(
    stack_frame: InterruptStackFrame)
{
    println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame, _error_code: u64) -> !
{
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame, error_code: u64)
{
    panic!("EXCEPTION: GENERAL PROTECTION FAULT (error code: {})\n{:#?}", error_code, stack_frame);
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame, error_code: x86_64::structures::idt::PageFaultErrorCode)
{
    use x86_64::registers::control::Cr2;

    crate::println!("\n╔══════════════════════════════════════════╗");
    crate::println!("║       EXCEPTION: PAGE FAULT             ║");
    crate::println!("╚══════════════════════════════════════════╝");

    let fault_addr = Cr2::read();
    crate::println!("Accessed Address: {:?}", fault_addr);
    crate::println!("Error Code: {:?}", error_code);
    crate::println!("  - Present: {}", error_code.contains(x86_64::structures::idt::PageFaultErrorCode::PROTECTION_VIOLATION));
    crate::println!("  - Write: {}", error_code.contains(x86_64::structures::idt::PageFaultErrorCode::CAUSED_BY_WRITE));
    crate::println!("  - User: {}", error_code.contains(x86_64::structures::idt::PageFaultErrorCode::USER_MODE));
    crate::println!("  - Reserved Write: {}", error_code.contains(x86_64::structures::idt::PageFaultErrorCode::MALFORMED_TABLE));
    crate::println!("  - Instruction Fetch: {}", error_code.contains(x86_64::structures::idt::PageFaultErrorCode::INSTRUCTION_FETCH));
    crate::println!("{:#?}", stack_frame);

    // Check if address is in user space range
    let addr_u64 = fault_addr.as_u64();
    if addr_u64 >= crate::memory::userspace::USER_SPACE_START &&
       addr_u64 < crate::memory::userspace::USER_SPACE_END {
        crate::println!("⚠️  Fault in USER SPACE range (0x{:X} - 0x{:X})",
            crate::memory::userspace::USER_SPACE_START,
            crate::memory::userspace::USER_SPACE_END);
    }

    panic!("Page fault");
}

extern "x86-interrupt" fn timer_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    crate::task::timer::tick();

    // Call scheduler tick for preemptive multitasking
    crate::process::scheduler::tick();

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    use x86_64::instructions::port::Port;

    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };
    crate::task::keyboard::add_scancode(scancode);

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

extern "x86-interrupt" fn primary_ata_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    // ATA interrupt fired - acknowledge it by sending EOI
    // The ATA driver handles the actual operation
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::PrimaryATA.as_u8());
    }
}

extern "x86-interrupt" fn secondary_ata_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    // Secondary ATA interrupt - acknowledge with EOI
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::SecondaryATA.as_u8());
    }
}

#[cfg(test)]
mod tests {
    #[test_case]
    fn test_breakpoint_exception() {
        // invoke a breakpoint exception
        x86_64::instructions::interrupts::int3();
    }
}
