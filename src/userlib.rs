//! Minimal user-space runtime library — the kernel-resident counterpart
//! of musl/relibc, designed so user programs compiled in-tree (see
//! `userspace::user_program_a/b`) can call POSIX-style functions
//! without each one re-implementing syscall plumbing.
//!
//! All entries here issue raw `syscall` instructions per the
//! System V x86-64 ABI: %rax = number, args in %rdi/%rsi/%rdx/%r10/
//! %r8/%r9, return in %rax (negative = -errno).  Numbers match the
//! `crate::syscall::numbers` table.
//!
//! ## What's not here
//!
//! * stdio buffering — `write_str` is unbuffered.
//! * `malloc` — currently calls `brk()` once and bumps a pointer; no
//!   freeing, no defragmentation.  A real allocator follows once we
//!   have a stable userspace heap.
//! * Floating-point printf %f/%e — formatting only handles integers,
//!   strings, and char.

#![cfg(target_arch = "x86_64")]
#![allow(dead_code)]

use core::arch::asm;

pub const SYS_READ:    usize = 0;
pub const SYS_WRITE:   usize = 1;
pub const SYS_OPEN:    usize = 2;
pub const SYS_CLOSE:   usize = 3;
pub const SYS_MMAP:    usize = 9;
pub const SYS_BRK:     usize = 12;
pub const SYS_EXIT:    usize = 60;
pub const SYS_GETPID:  usize = 39;
pub const SYS_NANOSLEEP: usize = 35;

/// Issue a raw 0-arg syscall.
#[inline(always)]
pub unsafe fn syscall0(num: usize) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "syscall",
            in("rax") num,
            lateout("rax") ret,
            out("rcx") _, out("r11") _,
            options(nostack, preserves_flags),
        );
    }
    ret
}

#[inline(always)]
pub unsafe fn syscall1(num: usize, a: usize) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "syscall",
            in("rax") num, in("rdi") a,
            lateout("rax") ret,
            out("rcx") _, out("r11") _,
            options(nostack, preserves_flags),
        );
    }
    ret
}

#[inline(always)]
pub unsafe fn syscall2(num: usize, a: usize, b: usize) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "syscall",
            in("rax") num, in("rdi") a, in("rsi") b,
            lateout("rax") ret,
            out("rcx") _, out("r11") _,
            options(nostack, preserves_flags),
        );
    }
    ret
}

#[inline(always)]
pub unsafe fn syscall3(num: usize, a: usize, b: usize, c: usize) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "syscall",
            in("rax") num, in("rdi") a, in("rsi") b, in("rdx") c,
            lateout("rax") ret,
            out("rcx") _, out("r11") _,
            options(nostack, preserves_flags),
        );
    }
    ret
}

// -----------------------------------------------------------------------------
// POSIX-flavour wrappers
// -----------------------------------------------------------------------------

/// Write `buf` to file descriptor `fd`.  Returns bytes written or
/// negative -errno.
pub fn write(fd: i32, buf: &[u8]) -> isize {
    unsafe { syscall3(SYS_WRITE, fd as usize, buf.as_ptr() as usize, buf.len()) }
}

/// Read up to `buf.len()` bytes into `buf` from `fd`.  Returns bytes
/// read or negative -errno.
pub fn read(fd: i32, buf: &mut [u8]) -> isize {
    unsafe { syscall3(SYS_READ, fd as usize, buf.as_mut_ptr() as usize, buf.len()) }
}

/// Terminate the process with the given code.  Doesn't return.
pub fn exit(code: i32) -> ! {
    unsafe { syscall1(SYS_EXIT, code as usize); }
    loop { unsafe { core::arch::asm!("ud2") } }
}

pub fn getpid() -> i32 {
    unsafe { syscall0(SYS_GETPID) as i32 }
}

/// Bump the data-segment break by `delta` bytes (use 0 to query
/// current).  Returns the new break, or the previous one on failure.
pub fn brk(addr: usize) -> usize {
    unsafe { syscall1(SYS_BRK, addr) as usize }
}

/// Convenience: write a string to stdout (fd 1).  Returns the number
/// of bytes successfully written, or 0 on error.
pub fn print(s: &str) -> usize {
    let n = write(1, s.as_bytes());
    if n < 0 { 0 } else { n as usize }
}

/// Convenience: write a string + newline.
pub fn println(s: &str) {
    print(s);
    print("\n");
}

// -----------------------------------------------------------------------------
// Bump-pointer "malloc"
// -----------------------------------------------------------------------------
//
// Initial heap: 64 KiB allocated via `brk(initial_brk + 64 KiB)`.
// `malloc` returns a pointer into the heap and bumps the cursor.
// No `free`.  Sufficient for trivial user programs; relibc-style
// allocator is the proper successor.

const HEAP_BYTES: usize = 64 * 1024;
static mut HEAP_BASE: usize = 0;
static mut HEAP_CUR: usize  = 0;
static mut HEAP_END: usize  = 0;

/// One-shot heap initialiser; idempotent.
pub fn heap_init() {
    unsafe {
        if HEAP_BASE != 0 { return; }
        let cur = brk(0);
        let end = brk(cur + HEAP_BYTES);
        if end <= cur {
            // brk failed; leave heap disabled.
            HEAP_BASE = 0;
            return;
        }
        HEAP_BASE = cur;
        HEAP_CUR  = cur;
        HEAP_END  = end;
    }
}

/// Allocate `size` bytes (8-byte aligned).  Returns 0 on failure.
pub fn malloc(size: usize) -> usize {
    unsafe {
        if HEAP_BASE == 0 { heap_init(); }
        let aligned = (size + 7) & !7;
        if HEAP_CUR + aligned > HEAP_END { return 0; }
        let p = HEAP_CUR;
        HEAP_CUR += aligned;
        p
    }
}

/// Free is a no-op in this allocator.  Kept so callers don't have to
/// `#[cfg]` around it once a real allocator lands.
pub fn free(_ptr: usize) {}

// -----------------------------------------------------------------------------
// Tiny printf — supports %d (i64), %u (u64), %x (lowercase hex u64),
// %s (bytes-or-str), %c (single u8), %% (literal).
// -----------------------------------------------------------------------------

/// Argument enum for the printf path.  The variadic crate ecosystem is
/// std-only; we emulate it with an explicit tagged union.
pub enum Arg<'a> {
    I(i64),
    U(u64),
    S(&'a [u8]),
    C(u8),
}

/// `printf(fd, fmt, args)` — writes the formatted string to `fd`.
/// Unknown specifiers are passed through verbatim.  Returns total
/// bytes written.
pub fn printf(fd: i32, fmt: &str, args: &[Arg]) -> usize {
    let mut written = 0usize;
    let bytes = fmt.as_bytes();
    let mut i = 0;
    let mut arg_idx = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' || i + 1 >= bytes.len() {
            let n = write(fd, &bytes[i..i+1]);
            if n > 0 { written += n as usize; }
            i += 1;
            continue;
        }
        let spec = bytes[i + 1];
        i += 2;
        match spec {
            b'%' => { let _ = write(fd, b"%"); written += 1; }
            b's' if arg_idx < args.len() => {
                if let Arg::S(s) = args[arg_idx] {
                    let n = write(fd, s);
                    if n > 0 { written += n as usize; }
                }
                arg_idx += 1;
            }
            b'c' if arg_idx < args.len() => {
                if let Arg::C(c) = args[arg_idx] {
                    let n = write(fd, &[c]);
                    if n > 0 { written += n as usize; }
                }
                arg_idx += 1;
            }
            b'd' if arg_idx < args.len() => {
                if let Arg::I(v) = args[arg_idx] {
                    let mut buf = [0u8; 24];
                    let s = format_signed(v, &mut buf);
                    let n = write(fd, s);
                    if n > 0 { written += n as usize; }
                }
                arg_idx += 1;
            }
            b'u' if arg_idx < args.len() => {
                if let Arg::U(v) = args[arg_idx] {
                    let mut buf = [0u8; 24];
                    let s = format_unsigned(v, 10, &mut buf);
                    let n = write(fd, s);
                    if n > 0 { written += n as usize; }
                }
                arg_idx += 1;
            }
            b'x' if arg_idx < args.len() => {
                if let Arg::U(v) = args[arg_idx] {
                    let mut buf = [0u8; 24];
                    let s = format_unsigned(v, 16, &mut buf);
                    let n = write(fd, s);
                    if n > 0 { written += n as usize; }
                }
                arg_idx += 1;
            }
            _ => {
                let raw = &[b'%', spec];
                let n = write(fd, raw);
                if n > 0 { written += n as usize; }
            }
        }
    }
    written
}

/// Helper: format `v` as signed decimal into the back of `buf` and
/// return the slice covering the formatted bytes.  Public so tests
/// can assert on it without firing a syscall.
pub fn format_signed<'a>(v: i64, buf: &'a mut [u8]) -> &'a [u8] {
    if v == i64::MIN {
        // i64::MIN can't be negated; encode the well-known constant.
        let s = b"-9223372036854775808";
        let len = s.len().min(buf.len());
        buf[..len].copy_from_slice(&s[..len]);
        return &buf[..len];
    }
    let neg = v < 0;
    let mut x = v.unsigned_abs();
    let mut pos = buf.len();
    if x == 0 {
        if pos > 0 { pos -= 1; buf[pos] = b'0'; }
    } else {
        while x > 0 && pos > 0 {
            pos -= 1;
            buf[pos] = b'0' + (x % 10) as u8;
            x /= 10;
        }
    }
    if neg && pos > 0 { pos -= 1; buf[pos] = b'-'; }
    &buf[pos..]
}

/// Helper: format `v` as unsigned in base `base` (10 or 16).
pub fn format_unsigned<'a>(v: u64, base: u64, buf: &'a mut [u8]) -> &'a [u8] {
    let mut x = v;
    let mut pos = buf.len();
    if x == 0 {
        if pos > 0 { pos -= 1; buf[pos] = b'0'; }
    } else {
        while x > 0 && pos > 0 {
            pos -= 1;
            let d = (x % base) as u8;
            buf[pos] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
            x /= base;
        }
    }
    &buf[pos..]
}

// =============================================================================
// POSIX-shaped string / byte ops, mirrored on the Rust side so kernel
// tests can drive the same algorithms the C `libc.c` ships to userspace.
// =============================================================================
//
// The C side lives in `coreutils_src/lib/libc.c`; this Rust side provides
// equivalent primitives so we can unit-test the algorithms without firing
// a syscall.  Keeping them in sync means a regression in either side is
// caught by the test suite — independently of whether userspace runs.

/// strlen — byte count up to (but not including) the first NUL.  Returns
/// the full slice length if no NUL is present.
pub fn rs_strlen(s: &[u8]) -> usize {
    s.iter().position(|&b| b == 0).unwrap_or(s.len())
}

/// strcmp — return < 0, 0, > 0 ordering of two NUL-terminated byte slices,
/// without reading past the first NUL.  Identical to the C semantics.
pub fn rs_strcmp(a: &[u8], b: &[u8]) -> i32 {
    let mut i = 0;
    loop {
        let ca = if i < a.len() { a[i] } else { 0 };
        let cb = if i < b.len() { b[i] } else { 0 };
        if ca == 0 || cb == 0 || ca != cb {
            return ca as i32 - cb as i32;
        }
        i += 1;
    }
}

/// strncmp — bounded version of strcmp.  Returns 0 once `n` bytes match
/// even if neither side has hit a NUL.
pub fn rs_strncmp(a: &[u8], b: &[u8], n: usize) -> i32 {
    for i in 0..n {
        let ca = if i < a.len() { a[i] } else { 0 };
        let cb = if i < b.len() { b[i] } else { 0 };
        if ca == 0 || cb == 0 || ca != cb {
            return ca as i32 - cb as i32;
        }
    }
    0
}

/// memchr — first occurrence of `c` in the first `n` bytes of `s`, or
/// None if not found.
pub fn rs_memchr(s: &[u8], c: u8, n: usize) -> Option<usize> {
    let m = n.min(s.len());
    s[..m].iter().position(|&b| b == c)
}

/// memmove — overlapping byte copy.  `dst` and `src` may alias; we
/// determine direction so the result is always exactly `src`'s prior
/// content.
pub fn rs_memmove(dst: &mut [u8], src_start: usize, dst_start: usize, n: usize) {
    if dst_start == src_start || n == 0 { return; }
    if dst_start < src_start {
        // forward copy
        for i in 0..n {
            dst[dst_start + i] = dst[src_start + i];
        }
    } else {
        // backward copy
        for i in (0..n).rev() {
            dst[dst_start + i] = dst[src_start + i];
        }
    }
}

/// strtol — parse a signed integer in `base` from `s`.  `base = 0` auto-
/// detects: `0x` → 16, leading `0` → 8, otherwise 10.  Returns
/// `(value, bytes_consumed)`; bytes_consumed = 0 means "no digits".
pub fn rs_strtol(s: &[u8], mut base: u32) -> (i64, usize) {
    let mut i = 0;
    while i < s.len() && (s[i] == b' ' || (s[i] >= b'\t' && s[i] <= b'\r')) { i += 1; }
    let neg = if i < s.len() && s[i] == b'-' { i += 1; true }
              else if i < s.len() && s[i] == b'+' { i += 1; false }
              else { false };
    if (base == 0 || base == 16) && i + 1 < s.len() && s[i] == b'0' && (s[i+1] == b'x' || s[i+1] == b'X') {
        base = 16; i += 2;
    } else if base == 0 && i < s.len() && s[i] == b'0' {
        base = 8; i += 1;
    } else if base == 0 {
        base = 10;
    }
    let start = i;
    let mut acc: i64 = 0;
    while i < s.len() {
        let c = s[i];
        let d: i64 = if c.is_ascii_digit() { (c - b'0') as i64 }
                     else if c >= b'a' && c <= b'z' { (c - b'a') as i64 + 10 }
                     else if c >= b'A' && c <= b'Z' { (c - b'A') as i64 + 10 }
                     else { -1 };
        if d < 0 || d >= base as i64 { break; }
        acc = acc * (base as i64) + d;
        i += 1;
    }
    if i == start { (0, 0) }
    else { ((if neg { -acc } else { acc }), i) }
}
