/// User space memory management
///
/// This module provides identity-mapped memory regions for user space programs.
/// User space uses the lower half of virtual address space (0x0000_0000 - 0x7FFF_FFFF).

use x86_64::{
    structures::paging::{
        Page, PhysFrame, Mapper, Size4KiB, PageTableFlags, FrameAllocator,
        mapper::MapToError,
    },
    VirtAddr, PhysAddr,
};

/// User space memory region
/// We allocate 32 MB starting at 0x00400000 (4 MB offset to avoid BIOS/VGA regions)
pub const USER_SPACE_START: u64 = 0x0040_0000;  // 4 MB
pub const USER_SPACE_SIZE: u64 = 0x0200_0000;   // 32 MB
pub const USER_SPACE_END: u64 = USER_SPACE_START + USER_SPACE_SIZE;

/// User stack size (256 KB per process)
pub const USER_STACK_SIZE: u64 = 256 * 1024;

/// User code region start (right after reserved area)
pub const USER_CODE_START: u64 = USER_SPACE_START + 0x1000;  // Skip first 4KB

/// Maximum number of user pages we can allocate
pub const MAX_USER_PAGES: usize = (USER_SPACE_SIZE / 4096) as usize;

/// Create identity mapping for user space region
///
/// Maps virtual addresses 0x00400000-0x02400000 to the same physical addresses.
/// This allows user mode code to access this memory region.
pub fn init_user_space_mapping<M, A>(
    mapper: &mut M,
    frame_allocator: &mut A,
) -> Result<(), MapToError<Size4KiB>>
where
    M: Mapper<Size4KiB>,
    A: FrameAllocator<Size4KiB>,
{
    // User space flags: Present, Writable, User-accessible
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE;

    // Map all pages in user space region
    let start_page = Page::containing_address(VirtAddr::new(USER_SPACE_START));
    let end_page = Page::containing_address(VirtAddr::new(USER_SPACE_END - 1));

    for page in Page::range_inclusive(start_page, end_page) {
        // Identity map: virtual address = physical address
        let frame = PhysFrame::containing_address(PhysAddr::new(page.start_address().as_u64()));

        // Map the page
        unsafe {
            mapper.map_to(page, frame, flags, frame_allocator)?
                .flush();
        }
    }

    Ok(())
}

/// Check if address is in user space range
pub fn is_user_space_address(addr: u64) -> bool {
    addr >= USER_SPACE_START && addr < USER_SPACE_END
}

/// Get a free user space address for code loading
/// Returns the start of user code region
pub fn get_user_code_region() -> VirtAddr {
    VirtAddr::new(USER_CODE_START)
}

/// Get user stack region (grows downward from top of user space)
pub fn get_user_stack_region() -> (VirtAddr, u64) {
    let stack_top = USER_SPACE_END - 0x1000;  // Leave 4KB guard page
    let stack_bottom = stack_top - USER_STACK_SIZE;

    (VirtAddr::new(stack_bottom), USER_STACK_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_space_addresses() {
        assert!(is_user_space_address(USER_SPACE_START));
        assert!(is_user_space_address(USER_SPACE_END - 1));
        assert!(!is_user_space_address(USER_SPACE_END));
        assert!(!is_user_space_address(0));
    }
}
