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
pub const ET_EXEC: u16 = 2;  // Statically-linked executable
pub const ET_DYN:  u16 = 3;  // Position-independent (PIE) or shared object

/// Machine type
const EM_X86_64: u16 = 62;  // AMD x86-64

/// Program header types
pub const PT_LOAD:    u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_INTERP:  u32 = 3;
pub const PT_PHDR:    u32 = 6;

/// Program header flags
const _PF_X: u32 = 1;  // Executable
const _PF_W: u32 = 2;  // Writable
const _PF_R: u32 = 4;  // Readable

// ---- Dynamic-section tags (subset, ELF gabi §5) ----
pub const DT_NULL:     i64 = 0;
pub const DT_PLTRELSZ: i64 = 2;
pub const DT_STRTAB:   i64 = 5;
pub const DT_SYMTAB:   i64 = 6;
pub const DT_RELA:     i64 = 7;
pub const DT_RELASZ:   i64 = 8;
pub const DT_RELAENT:  i64 = 9;
pub const DT_STRSZ:    i64 = 10;
pub const DT_SYMENT:   i64 = 11;
pub const DT_PLTGOT:   i64 = 3;
pub const DT_JMPREL:   i64 = 23;
pub const DT_PLTREL:   i64 = 20;

// ---- Relocation types we apply (System V x86-64 ABI §4.4.1) ----
pub const R_X86_64_NONE:     u32 = 0;
pub const R_X86_64_64:       u32 = 1;
pub const R_X86_64_GLOB_DAT: u32 = 6;
pub const R_X86_64_JUMP_SLOT: u32 = 7;
pub const R_X86_64_RELATIVE: u32 = 8;

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

        // Check type — accept both static executables (ET_EXEC) and
        // position-independent / shared objects (ET_DYN).  ET_DYN
        // covers PIE binaries which we relocate via the dynamic
        // section's R_X86_64_RELATIVE entries (see `apply_pie_relocations`).
        if header.e_type != ET_EXEC && header.e_type != ET_DYN {
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

    /// Get program headers (public so the dynamic-section helpers can
    /// walk PT_DYNAMIC).
    pub fn program_headers(&self) -> &[ProgramHeader] {
        self.program_headers_inner()
    }

    fn program_headers_inner(&self) -> &[ProgramHeader] {
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
        let phdrs = self.program_headers_inner();

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

    /// Returns true iff this binary is position-independent (ET_DYN).
    pub fn is_pie(&self) -> bool { self.header.e_type == ET_DYN }

    /// Locate the PT_DYNAMIC segment, if any, and return the file
    /// offset + size in bytes.  PIE binaries always have one; static
    /// ET_EXEC may not.
    pub fn dynamic_segment(&self) -> Option<(usize, usize)> {
        for ph in self.program_headers_inner() {
            if ph.p_type == PT_DYNAMIC {
                return Some((ph.p_offset as usize, ph.p_filesz as usize));
            }
        }
        None
    }

    /// Walk the dynamic section and return (tag, value) pairs.
    /// Each entry is 16 bytes: i64 d_tag + u64 d_val/d_ptr.
    pub fn dynamic_entries(&self) -> alloc::vec::Vec<(i64, u64)> {
        let mut out = alloc::vec::Vec::new();
        let (off, len) = match self.dynamic_segment() { Some(x) => x, None => return out };
        if off + len > self.data.len() { return out; }
        let body = &self.data[off..off + len];
        let mut i = 0;
        while i + 16 <= body.len() {
            let tag = i64::from_le_bytes([
                body[i], body[i+1], body[i+2], body[i+3],
                body[i+4], body[i+5], body[i+6], body[i+7],
            ]);
            let val = u64::from_le_bytes([
                body[i+8], body[i+9], body[i+10], body[i+11],
                body[i+12], body[i+13], body[i+14], body[i+15],
            ]);
            out.push((tag, val));
            if tag == DT_NULL { break; }
            i += 16;
        }
        out
    }

    /// Apply a single Elf64_Rela entry.  `image_base` is where the
    /// binary actually landed (relative to link address 0 for PIE).
    /// `resolve` is a callback the caller provides to resolve named
    /// symbols — invoked for GLOB_DAT and JUMP_SLOT.  Pass a no-op
    /// closure if you don't yet have a symbol table; those relocs
    /// will then return Err.
    pub fn apply_rela_with<F: FnMut(u32) -> Option<u64>>(
        image: &mut [u8], image_base: u64,
        r_offset: u64, r_info: u64, r_addend: i64,
        mut resolve: F,
    ) -> Result<(), &'static str>
    {
        let r_type = (r_info & 0xFFFF_FFFF) as u32;
        let r_sym  = (r_info >> 32) as u32;
        let off = r_offset as usize;
        if off + 8 > image.len() { return Err("rela: r_offset out of bounds"); }
        match r_type {
            R_X86_64_NONE => Ok(()),
            R_X86_64_RELATIVE => {
                // B + A
                let val = (image_base as i64).wrapping_add(r_addend) as u64;
                image[off..off + 8].copy_from_slice(&val.to_le_bytes());
                Ok(())
            }
            R_X86_64_64 => {
                // S + A.  When r_sym == 0 (self-relative) S is 0.
                let s = if r_sym == 0 { 0 }
                        else { resolve(r_sym).ok_or("rela: unresolved symbol")? };
                let val = (s as i64).wrapping_add(r_addend) as u64;
                image[off..off + 8].copy_from_slice(&val.to_le_bytes());
                Ok(())
            }
            R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => {
                // Word-sized: GOT slot ⟵ S.  PLT lazy-binding short-
                // circuits this to immediate resolution since we don't
                // implement PLT trampolines.  Addend is ignored.
                let s = resolve(r_sym).ok_or("rela: unresolved symbol")?;
                image[off..off + 8].copy_from_slice(&s.to_le_bytes());
                Ok(())
            }
            _ => Err("rela: unsupported relocation type"),
        }
    }

    /// Convenience wrapper used by tests + the PIE loader: applies a
    /// relocation with no symbol resolver (suitable for RELATIVE and
    /// self-referential R_X86_64_64).
    pub fn apply_rela(image: &mut [u8], image_base: u64,
                      r_offset: u64, r_info: u64, r_addend: i64)
        -> Result<(), &'static str>
    {
        Self::apply_rela_with(image, image_base, r_offset, r_info, r_addend,
                              |_| None)
    }

    /// Apply every R_X86_64_RELATIVE relocation found in the dynamic
    /// section to the loaded image.  Returns the number of
    /// relocations applied so the caller can log it.
    pub fn apply_pie_relocations(&self, image: &mut [u8], image_base: u64)
        -> Result<usize, &'static str>
    {
        let entries = self.dynamic_entries();
        let mut rela_off: Option<u64> = None;
        let mut rela_sz:  Option<u64> = None;
        let mut rela_ent: Option<u64> = None;
        for &(tag, val) in &entries {
            match tag {
                DT_RELA    => rela_off = Some(val),
                DT_RELASZ  => rela_sz  = Some(val),
                DT_RELAENT => rela_ent = Some(val),
                _ => {}
            }
        }
        let (rela_off, rela_sz, rela_ent) = match (rela_off, rela_sz, rela_ent) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => return Ok(0), // No RELA table — nothing to do.
        };
        if rela_ent < 24 { return Err("rela: entry size < 24"); }

        // The DT_RELA value is a virtual address (relative to link
        // base = 0 for PIE).  Convert to image offset.
        let mut count = 0usize;
        let mut pos = rela_off as usize;
        let end = (rela_off + rela_sz) as usize;
        while pos + (rela_ent as usize) <= end && pos + 24 <= image.len() {
            let r_offset = u64::from_le_bytes([
                image[pos],   image[pos+1], image[pos+2],  image[pos+3],
                image[pos+4], image[pos+5], image[pos+6],  image[pos+7],
            ]);
            let r_info = u64::from_le_bytes([
                image[pos+8],  image[pos+9],  image[pos+10], image[pos+11],
                image[pos+12], image[pos+13], image[pos+14], image[pos+15],
            ]);
            let r_addend = i64::from_le_bytes([
                image[pos+16], image[pos+17], image[pos+18], image[pos+19],
                image[pos+20], image[pos+21], image[pos+22], image[pos+23],
            ]);
            // Only apply RELATIVE here; other types belong to dyn-link.
            let r_type = (r_info & 0xFFFFFFFF) as u32;
            if r_type == R_X86_64_RELATIVE {
                Self::apply_rela(image, image_base, r_offset, r_info, r_addend)?;
                count += 1;
            }
            pos += rela_ent as usize;
        }
        Ok(count)
    }
}

/// Load and execute an ELF file from VFS
///
/// This function DOES return after user program exits, despite calling
/// exec_with_return_proper which is marked -> !. The magic happens via
/// restore_kernel_context_and_return() which manually returns here.
pub fn load_and_exec(path: &str) {
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
                match RAMDISK.lock().read(path) {
                    Ok(data) => data,
                    Err(e) => {
                        crate::println!("Failed to read ELF from filesystem: {:?}", e);
                        return;
                    }
                }
            }
        } else {
            // Only RAMDISK available
            match RAMDISK.lock().read(path) {
                Ok(data) => data,
                Err(e) => {
                    crate::println!("Failed to read ELF from RAMDISK: {:?}", e);
                    return;
                }
            }
        }
    };

    // Parse ELF
    let loader = match ElfLoader::new(&elf_data) {
        Ok(l) => l,
        Err(e) => {
            crate::println!("Failed to parse ELF: {:?}", e);
            return;
        }
    };

    // Load into memory
    let (entry_point, _low, _high) = match loader.load() {
        Ok(result) => result,
        Err(e) => {
            crate::println!("Failed to load ELF: {:?}", e);
            return;
        }
    };

    // Allocate user stack
    let (stack_bottom, stack_size) = match user_allocator::allocate_user_stack() {
        Some(stack) => stack,
        None => {
            crate::println!("Failed to allocate user stack");
            return;
        }
    };

    // Execute in user mode with proper context saving
    // When process calls exit(), restore_kernel_context_and_return() will
    // magically return execution to here by restoring saved registers.
    // From compiler's perspective, exec_with_return_proper never returns (-> !),
    // but in reality execution continues here after sys_exit restores context.
    unsafe {
        crate::userspace::exec_with_return_proper(entry_point.as_u64(), stack_bottom.as_u64(), stack_size);
    }
}
