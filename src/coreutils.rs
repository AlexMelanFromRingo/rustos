//! In-tree coreutils — small POSIX utilities built as static x86-64
//! ELF binaries and embedded in the kernel.  Closes the long-standing
//! "coreutils as ELF binaries" item from the ROADMAP: the shell still
//! exposes builtin shortcuts, but `/bin/echo`, `/bin/cat`, etc. are
//! also real binaries that load + relocate + execute through the same
//! ELF path real user programs take.
//!
//! ## Pipeline
//!
//!   1. `coreutils_src/<name>.c` — POSIX C source, freestanding, calls
//!      raw syscalls via macros in `syscalls.h`.
//!   2. `crt.s` — minimal _start that unpacks argc/argv from the SysV
//!      x86-64 stack-init protocol and calls `int main(int, char**)`.
//!   3. `build.rs` builds each with
//!      gcc -nostdlib -static -no-pie -ffreestanding -fno-builtin
//!      -fno-stack-protector -O2 -Wl,--build-id=none -Wl,-z,noexecstack
//!      then embeds the ELF bytes via include_bytes!.
//!   4. This module exposes them as a `&'static [CoreUtilBin]`; the
//!      init path (see `crate::init` / `populate_bin`) writes each
//!      into the RAMDISK under `/bin/<name>` at boot.
//!
//! ## Why not Rust user binaries?
//!
//! Building Rust binaries for a freestanding x86_64-unknown-none target
//! and getting them through the kernel's ELF path involves a separate
//! Cargo workspace, a custom target spec, and the linker dance to keep
//! them static — not impossible, but a lot more moving parts than `gcc
//! -nostdlib`.  A C path also makes it easy to extend the set later by
//! dropping in another .c file.

/// One coreutil binary entry — name (basename, no path) + ELF bytes.
pub struct CoreUtilBin {
    pub name: &'static str,
    pub bytes: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/coreutils_blobs.rs"));

/// Number of coreutil binaries embedded.  Useful for tests + diagnostics.
pub fn count() -> usize { COREUTILS.len() }

/// Find a binary by name.  Linear scan over `COREUTILS`; the table is
/// small (≤ ~20 entries forever) so a hash map would be wasteful.
pub fn find(name: &str) -> Option<&'static CoreUtilBin> {
    COREUTILS.iter().find(|b| b.name == name)
}

/// Verify every embedded binary starts with the ELF magic `\x7FELF`
/// and has reasonable size (>= 256 bytes — even the smallest static
/// `true.c` produces ~5 KiB; <= 1 MiB — sanity ceiling).  Returns
/// the first violation as `Err(name, reason)`, or `Ok(count)` on
/// success.  Called by both the boot path (warns and skips broken
/// blobs) and the test suite.
pub fn audit() -> Result<usize, (&'static str, &'static str)> {
    for b in COREUTILS {
        if b.bytes.len() < 256 { return Err((b.name, "too small")); }
        if b.bytes.len() > 1 << 20 { return Err((b.name, "too large")); }
        if b.bytes.len() < 4 || b.bytes[..4] != [0x7F, b'E', b'L', b'F'] {
            return Err((b.name, "missing ELF magic"));
        }
        // ET_EXEC = 2, ET_DYN = 3.  All our coreutils are -no-pie =>
        // ET_EXEC.  This catches a build flag regression that would
        // produce ET_DYN by accident.
        if b.bytes.len() >= 18 {
            let e_type = u16::from_le_bytes([b.bytes[16], b.bytes[17]]);
            if e_type != 2 {
                return Err((b.name, "expected ET_EXEC (2)"));
            }
        }
    }
    Ok(COREUTILS.len())
}

/// Install every embedded coreutil into `/bin/<name>` in the RAMDISK.
/// Idempotent: re-running overwrites the existing entries (RAMDISK's
/// `write` semantics).  Called once during boot; the shell can also
/// invoke it via the `coreutils install` subcommand for manual reset.
pub fn populate_bin() -> usize {
    use crate::fs::ramdisk::RAMDISK;
    let mut rd = RAMDISK.lock();
    let _ = rd.create_directory("/bin");
    let mut installed = 0usize;
    for b in COREUTILS {
        let path = alloc::format!("/bin/{}", b.name);
        // write_file takes Vec<u8>; we copy each blob once at boot.
        if rd.write_file(&path, b.bytes.to_vec()).is_ok() {
            installed += 1;
        }
    }
    installed
}
