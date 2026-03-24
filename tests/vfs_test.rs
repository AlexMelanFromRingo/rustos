#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rustos::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use rustos::fs::vfs::{FileSystem, VfsError};
use rustos::fs::ramdisk::RamDisk;
use alloc::vec::Vec;

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    use rustos::allocator;
    use rustos::memory::{self, BitmapFrameAllocator};
    use x86_64::VirtAddr;

    rustos::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { BitmapFrameAllocator::init(&boot_info.memory_map) };
    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("heap initialization failed");

    test_main();
    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rustos::test_panic_handler(info)
}

#[test_case]
fn test_ramdisk_write_read() {
    let mut ramdisk = RamDisk::new();
    let data = b"Hello, RustOS!".to_vec();

    // Write file
    assert!(ramdisk.write("test.txt", data.clone()).is_ok());

    // Read file
    let read_data = ramdisk.read("test.txt").expect("Failed to read file");
    assert_eq!(read_data, data);
}

#[test_case]
fn test_ramdisk_overwrite() {
    let mut ramdisk = RamDisk::new();

    // Write initial content
    ramdisk.write("file.txt", b"initial".to_vec()).expect("Failed to write");

    // Overwrite
    ramdisk.write("file.txt", b"overwritten".to_vec()).expect("Failed to overwrite");

    // Verify
    let content = ramdisk.read("file.txt").expect("Failed to read");
    assert_eq!(content, b"overwritten");
}

#[test_case]
fn test_ramdisk_delete() {
    let mut ramdisk = RamDisk::new();

    // Write and delete
    ramdisk.write("temp.txt", b"temporary".to_vec()).expect("Failed to write");
    assert!(ramdisk.exists("temp.txt"));

    ramdisk.delete("temp.txt").expect("Failed to delete");
    assert!(!ramdisk.exists("temp.txt"));
}

#[test_case]
fn test_ramdisk_file_not_found() {
    let ramdisk = RamDisk::new();

    match ramdisk.read("nonexistent.txt") {
        Err(VfsError::FileNotFound) => {},
        _ => panic!("Expected FileNotFound error"),
    }
}

#[test_case]
fn test_ramdisk_list_files() {
    let mut ramdisk = RamDisk::new();

    // Empty initially
    assert_eq!(ramdisk.list().len(), 0);

    // Add files
    ramdisk.write("file1.txt", b"one".to_vec()).expect("Failed to write");
    ramdisk.write("file2.txt", b"two".to_vec()).expect("Failed to write");
    ramdisk.write("file3.txt", b"three".to_vec()).expect("Failed to write");

    // Verify list
    let files = ramdisk.list();
    assert_eq!(files.len(), 3);
}

#[test_case]
fn test_ramdisk_space_tracking() {
    let mut ramdisk = RamDisk::new();

    let initial_used = ramdisk.used_space();
    assert_eq!(initial_used, 0);

    // Write file
    let data = b"Hello, World!".to_vec();
    ramdisk.write("test.txt", data.clone()).expect("Failed to write");

    // Check space used
    assert_eq!(ramdisk.used_space(), data.len());

    // Delete file
    ramdisk.delete("test.txt").expect("Failed to delete");
    assert_eq!(ramdisk.used_space(), 0);
}

#[test_case]
fn test_ramdisk_filename_too_long() {
    let mut ramdisk = RamDisk::new();

    // Create a filename longer than 255 characters
    let long_name = "a".repeat(256);

    match ramdisk.write(&long_name, b"data".to_vec()) {
        Err(VfsError::InvalidName) => {},
        _ => panic!("Expected InvalidName error for long filename"),
    }
}

#[test_case]
fn test_ramdisk_file_too_large() {
    let mut ramdisk = RamDisk::new();

    // Create data larger than 1 MiB
    let large_data = vec![0u8; 1024 * 1024 + 1];

    match ramdisk.write("large.bin", large_data) {
        Err(VfsError::FileTooLarge) => {},
        _ => panic!("Expected FileTooLarge error"),
    }
}

#[test_case]
fn test_ramdisk_too_many_files() {
    let mut ramdisk = RamDisk::new();

    // Try to create 257 files (max is 256)
    for i in 0..256 {
        let filename = alloc::format!("file{}.txt", i);
        ramdisk.write(&filename, b"data".to_vec()).expect("Failed to write");
    }

    // 257th file should fail
    match ramdisk.write("file256.txt", b"data".to_vec()) {
        Err(VfsError::TooManyFiles) => {},
        _ => panic!("Expected TooManyFiles error"),
    }
}
