/// Simple user space memory allocator
///
/// Manages allocation of memory within the identity-mapped user space region.
/// Uses a simple bump allocator for code/data and separate stack allocator.

use super::userspace::{USER_CODE_START, USER_SPACE_END, USER_STACK_SIZE};
use x86_64::VirtAddr;
use spin::Mutex;

/// User space memory allocator
pub struct UserAllocator {
    /// Next free address for code/data allocation
    next_code_addr: u64,
    /// End of allocatable region (before stack)
    code_region_end: u64,
    /// Next stack top (stacks grow downward)
    next_stack_top: u64,
}

impl UserAllocator {
    const fn new() -> Self {
        // Code region: from USER_CODE_START to stack region
        // Stack region: top 1 MB of user space
        const STACK_REGION_SIZE: u64 = 1024 * 1024;  // 1 MB for all stacks

        UserAllocator {
            next_code_addr: USER_CODE_START,
            code_region_end: USER_SPACE_END - STACK_REGION_SIZE,
            next_stack_top: USER_SPACE_END,
        }
    }

    /// Allocate memory for user code/data
    ///
    /// Returns the virtual address of the allocated region.
    /// Size is rounded up to page size (4096 bytes).
    pub fn allocate_code(&mut self, size: usize) -> Option<VirtAddr> {
        // Round up to page size
        let size = (size + 4095) & !4095;
        let size = size as u64;

        if self.next_code_addr + size > self.code_region_end {
            return None;  // Out of memory
        }

        let addr = self.next_code_addr;
        self.next_code_addr += size;

        Some(VirtAddr::new(addr))
    }

    /// Allocate a user stack
    ///
    /// Returns (stack_bottom, stack_size).
    /// Stack grows downward from stack_top.
    pub fn allocate_stack(&mut self) -> Option<(VirtAddr, u64)> {
        if self.next_stack_top < self.code_region_end + USER_STACK_SIZE {
            return None;  // Out of stack space
        }

        let stack_bottom = self.next_stack_top - USER_STACK_SIZE;
        let stack_top = self.next_stack_top;

        // Reserve space for next stack (with 4KB guard page)
        self.next_stack_top = stack_bottom - 4096;  // Guard page

        Some((VirtAddr::new(stack_bottom), USER_STACK_SIZE))
    }

    /// Get available code memory
    pub fn available_code_memory(&self) -> u64 {
        self.code_region_end - self.next_code_addr
    }

    /// Reset allocator (for testing or cleanup)
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

/// Global user space allocator
pub static USER_ALLOCATOR: Mutex<UserAllocator> = Mutex::new(UserAllocator::new());

/// Allocate user code memory
pub fn allocate_user_code(size: usize) -> Option<VirtAddr> {
    USER_ALLOCATOR.lock().allocate_code(size)
}

/// Allocate user stack
pub fn allocate_user_stack() -> Option<(VirtAddr, u64)> {
    USER_ALLOCATOR.lock().allocate_stack()
}

/// Get available code memory
pub fn available_user_memory() -> u64 {
    USER_ALLOCATOR.lock().available_code_memory()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_allocation() {
        let mut alloc = UserAllocator::new();

        // Allocate 4KB
        let addr1 = alloc.allocate_code(4096).unwrap();
        assert_eq!(addr1.as_u64(), USER_CODE_START);

        // Allocate another 4KB
        let addr2 = alloc.allocate_code(4096).unwrap();
        assert_eq!(addr2.as_u64(), USER_CODE_START + 4096);
    }

    #[test]
    fn test_stack_allocation() {
        let mut alloc = UserAllocator::new();

        let (bottom, size) = alloc.allocate_stack().unwrap();
        assert_eq!(size, USER_STACK_SIZE);
        assert!(bottom.as_u64() < USER_SPACE_END);
    }
}
