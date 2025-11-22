/// Embedded files - binaries and resources compiled into the kernel

/// Hello world ELF binary
pub const HELLO_ELF: &[u8] = include_bytes!("../hello.elf");
