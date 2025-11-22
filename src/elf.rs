/// ELF (Executable and Linkable Format) parser and loader
///
/// Supports loading 64-bit ELF executables into user space.

use crate::memory::user_allocator;
use x86_64::VirtAddr;
use core::mem;

/// ELF magic number: 0x7F 'E' 'L' 'F'
const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

/// ELF class
const ELFCLASS64: u8 = 2;  // 64-bit

/// ELF data encoding
const ELFDATA2LSB: u8 = 1;  // Little-endian

/// ELF version
const EV_CURRENT: u8 = 1;

/// OS/ABI
const ELFOSABI_SYSV: u8 = 0;  // System V ABI

/// ELF type
const ET_EXEC: u16 = 2;  // Executable file

/// Machine type
const EM_X86_64: u16 = 62;  // AMD x86-64

/// Program header types
const PT_LOAD: u32 = 1;  // Loadable segment

/// Program header flags
const _PF_X: u32 = 1;  // Executable
const _PF_W: u32 = 2;  // Writable
const _PF_R: u32 = 4;  // Readable

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ElfHeader {
    pub e_ident: [u8; 16],      // Magic number and other info
    pub e_type: u16,            // Object file type
    pub e_machine: u16,         // Architecture
    pub e_version: u32,         // Object file version
    pub e_entry: u64,           // Entry point virtual address
    pub e_phoff: u64,           // Program header table file offset
    pub e_shoff: u64,           // Section header table file offset
    pub e_flags: u32,           // Processor-specific flags
    pub e_ehsize: u16,          // ELF header size
    pub e_phentsize: u16,       // Program header table entry size
    pub e_phnum: u16,           // Program header table entry count
    pub e_shentsize: u16,       // Section header table entry size
    pub e_shnum: u16,           // Section header table entry count
    pub e_shstrndx: u16,        // Section header string table index
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProgramHeader {
    pub p_type: u32,            // Segment type
    pub p_flags: u32,           // Segment flags
    pub p_offset: u64,          // Segment file offset
    pub p_vaddr: u64,           // Segment virtual address
    pub p_paddr: u64,           // Segment physical address
    pub p_filesz: u64,          // Segment size in file
    pub p_memsz: u64,           // Segment size in memory
    pub p_align: u64,           // Segment alignment
}

#[derive(Debug)]
pub enum ElfError {
    InvalidMagic,
    UnsupportedClass,
    UnsupportedEndianness,
    UnsupportedVersion,
    UnsupportedABI,
    NotExecutable,
    WrongArchitecture,
    InvalidHeaderSize,
    NoLoadableSegments,
    AllocationFailed,
    TooSmall,
}

pub struct ElfLoader<'a> {
    data: &'a [u8],
    header: ElfHeader,
}

impl<'a> ElfLoader<'a> {
    /// Create a new ELF loader from raw bytes
    pub fn new(data: &'a [u8]) -> Result<Self, ElfError> {
        if data.len() < mem::size_of::<ElfHeader>() {
            return Err(ElfError::TooSmall);
        }

        // Parse ELF header
        let header = unsafe {
            core::ptr::read_unaligned(data.as_ptr() as *const ElfHeader)
        };

        // Validate ELF header
        Self::validate_header(&header)?;

        Ok(Self { data, header })
    }

    /// Validate ELF header
    fn validate_header(header: &ElfHeader) -> Result<(), ElfError> {
        // Check magic number
        if header.e_ident[0..4] != ELF_MAGIC {
            return Err(ElfError::InvalidMagic);
        }

        // Check class (64-bit)
        if header.e_ident[4] != ELFCLASS64 {
            return Err(ElfError::UnsupportedClass);
        }

        // Check endianness (little-endian)
        if header.e_ident[5] != ELFDATA2LSB {
            return Err(ElfError::UnsupportedEndianness);
        }

        // Check version
        if header.e_ident[6] != EV_CURRENT {
            return Err(ElfError::UnsupportedVersion);
        }

        // Check OS/ABI (we accept SYSV)
        if header.e_ident[7] != ELFOSABI_SYSV {
            return Err(ElfError::UnsupportedABI);
        }

        // Check type (executable)
        if header.e_type != ET_EXEC {
            return Err(ElfError::NotExecutable);
        }

        // Check machine (x86-64)
        if header.e_machine != EM_X86_64 {
            return Err(ElfError::WrongArchitecture);
        }

        // Check header size
        if header.e_ehsize as usize != mem::size_of::<ElfHeader>() {
            return Err(ElfError::InvalidHeaderSize);
        }

        Ok(())
    }

    /// Get ELF entry point
    pub fn entry_point(&self) -> u64 {
        self.header.e_entry
    }

    /// Get program headers
    fn program_headers(&self) -> &[ProgramHeader] {
        let offset = self.header.e_phoff as usize;
        let count = self.header.e_phnum as usize;
        let size = self.header.e_phentsize as usize;

        if size != mem::size_of::<ProgramHeader>() {
            return &[];
        }

        let ptr = unsafe {
            self.data.as_ptr().add(offset) as *const ProgramHeader
        };

        unsafe {
            core::slice::from_raw_parts(ptr, count)
        }
    }

    /// Load ELF into user space memory
    /// Returns (entry_point, lowest_addr, highest_addr)
    pub fn load(&self) -> Result<(VirtAddr, u64, u64), ElfError> {
        let phdrs = self.program_headers();

        if phdrs.is_empty() {
            return Err(ElfError::NoLoadableSegments);
        }

        let mut lowest_addr = u64::MAX;
        let mut highest_addr = 0u64;

        // First pass: find address range (only consider segments in user space)
        for phdr in phdrs {
            if phdr.p_type != PT_LOAD {
                continue;
            }

            let start = phdr.p_vaddr;
            let end = phdr.p_vaddr + phdr.p_memsz;

            // Skip segments outside user space (e.g., ELF metadata at low addresses)
            if start < super::memory::userspace::USER_SPACE_START {
                continue;
            }

            if start < lowest_addr {
                lowest_addr = start;
            }
            if end > highest_addr {
                highest_addr = end;
            }
        }

        if lowest_addr == u64::MAX {
            return Err(ElfError::NoLoadableSegments);
        }

        // Allocate memory for entire ELF (code + data + bss)
        let total_size = (highest_addr - lowest_addr) as usize;

        // For ET_EXEC, we must load at the exact address specified in ELF
        // Check if lowest_addr is at user space start (0x400000)
        let base_addr = if lowest_addr == super::memory::userspace::USER_SPACE_START {
            // Direct mapping - ELF expects to be at 0x400000
            VirtAddr::new(lowest_addr)
        } else {
            // Relocatable - allocate anywhere
            user_allocator::allocate_user_code(total_size)
                .ok_or(ElfError::AllocationFailed)?
        };

        crate::println!("ELF: lowest_addr=0x{:X}, highest_addr=0x{:X}, base_addr=0x{:X}",
                        lowest_addr, highest_addr, base_addr.as_u64());

        // Second pass: load segments
        for phdr in phdrs {
            if phdr.p_type != PT_LOAD {
                continue;
            }

            // Skip segments outside user space (same as first pass)
            if phdr.p_vaddr < super::memory::userspace::USER_SPACE_START {
                continue;
            }

            let file_offset = phdr.p_offset as usize;
            let vaddr_offset = (phdr.p_vaddr - lowest_addr) as usize;
            let filesz = phdr.p_filesz as usize;
            let memsz = phdr.p_memsz as usize;

            // Calculate destination address in allocated memory
            let dest_addr = base_addr.as_u64() + vaddr_offset as u64;
            let dest_ptr = dest_addr as *mut u8;

            unsafe {
                // Copy data from file
                if filesz > 0 {
                    let src = self.data.as_ptr().add(file_offset);
                    core::ptr::copy_nonoverlapping(src, dest_ptr, filesz);
                }

                // Zero-fill BSS (memsz > filesz)
                if memsz > filesz {
                    let bss_ptr = dest_ptr.add(filesz);
                    core::ptr::write_bytes(bss_ptr, 0, memsz - filesz);
                }
            }

            // Note: We can't set page permissions directly in this simple implementation
            // In a real OS, we would mark pages as R/W/X based on phdr.p_flags
        }

        // Calculate actual entry point in loaded memory
        let entry_offset = self.header.e_entry - lowest_addr;
        let entry_addr = base_addr.as_u64() + entry_offset;

        Ok((VirtAddr::new(entry_addr), lowest_addr, highest_addr))
    }
}

/// Load and execute an ELF file from VFS
pub fn load_and_exec(path: &str) -> Result<(), ElfError> {
    use crate::fs::ramdisk::RAMDISK;
    use crate::fs::fat32::FAT32;
    use crate::fs::vfs::FileSystem;

    // Read ELF file from filesystem
    let elf_data = {
        // Try FAT32 first
        if let Some(ref fs) = *FAT32.lock() {
            if let Ok(data) = fs.read(path) {
                data
            } else {
                // Fallback to RAMDISK
                RAMDISK.lock().read(path)
                    .map_err(|_| ElfError::TooSmall)?
            }
        } else {
            // Only RAMDISK available
            RAMDISK.lock().read(path)
                .map_err(|_| ElfError::TooSmall)?
        }
    };

    // Parse ELF
    let loader = ElfLoader::new(&elf_data)?;

    // Load into memory
    let (entry_point, _low, _high) = loader.load()?;

    // Allocate user stack
    let (stack_bottom, stack_size) = user_allocator::allocate_user_stack()
        .ok_or(ElfError::AllocationFailed)?;

    // Jump to user mode at entry point
    unsafe {
        crate::userspace::jump_to_ring3(entry_point.as_u64(), stack_bottom.as_u64(), stack_size);
    }
}
