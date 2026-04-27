//! Seccomp-style syscall filter, BPF-evaluated.
//!
//! Each filter is a sequence of classic-BPF instructions evaluated against
//! a `seccomp_data`-shaped buffer:
//!
//!     u32 nr;             // syscall number
//!     u32 arch;           // SECCOMP_AUDIT_ARCH_X86_64 = 0xc000003e
//!     u64 instruction_pointer;
//!     u64 args[6];
//!
//! The interpreter implements the cBPF subset Linux's seccomp uses: LD/LDX
//! immediates, ALU ops, JMP/JEQ/JGT/JSET, RET (with return value =
//! seccomp action).  Instructions follow the classic 8-byte layout
//! (opcode, jt, jf, k).
//!
//! Return values follow the Linux seccomp action namespace:
//!   SECCOMP_RET_KILL_PROCESS = 0x80000000
//!   SECCOMP_RET_TRAP         = 0x00030000
//!   SECCOMP_RET_ERRNO        = 0x00050000  (low 16 bits = errno)
//!   SECCOMP_RET_LOG          = 0x7ffc0000
//!   SECCOMP_RET_ALLOW        = 0x7fff0000
//!
//! When no filter is installed for the current process the dispatcher
//! treats it as ALLOW.

use alloc::vec::Vec;
use spin::Mutex;

// ---------- BPF opcodes ----------

const BPF_LD:  u16 = 0x00;
const BPF_LDX: u16 = 0x01;
const BPF_ST:  u16 = 0x02;
const BPF_STX: u16 = 0x03;
const BPF_ALU: u16 = 0x04;
const BPF_JMP: u16 = 0x05;
const BPF_RET: u16 = 0x06;
const BPF_MISC: u16 = 0x07;

const BPF_W: u16 = 0x00;
const BPF_H: u16 = 0x08;
const BPF_B: u16 = 0x10;

const BPF_IMM: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_IND: u16 = 0x40;
const BPF_MEM: u16 = 0x60;
const BPF_LEN: u16 = 0x80;
const BPF_MSH: u16 = 0xa0;

const BPF_ADD: u16 = 0x00;
const BPF_SUB: u16 = 0x10;
const BPF_MUL: u16 = 0x20;
const BPF_DIV: u16 = 0x30;
const BPF_OR:  u16 = 0x40;
const BPF_AND: u16 = 0x50;
const BPF_LSH: u16 = 0x60;
const BPF_RSH: u16 = 0x70;
const BPF_NEG: u16 = 0x80;
const BPF_MOD: u16 = 0x90;
const BPF_XOR: u16 = 0xa0;

const BPF_JA:   u16 = 0x00;
const BPF_JEQ:  u16 = 0x10;
const BPF_JGT:  u16 = 0x20;
const BPF_JGE:  u16 = 0x30;
const BPF_JSET: u16 = 0x40;

const BPF_K: u16 = 0x00;
const BPF_X: u16 = 0x08;

#[inline] fn class(c: u16) -> u16 { c & 0x07 }
#[inline] fn size(c: u16) -> u16 { c & 0x18 }
#[inline] fn mode(c: u16) -> u16 { c & 0xe0 }
#[inline] fn op(c: u16)   -> u16 { c & 0xf0 }
#[inline] fn src(c: u16)  -> u16 { c & 0x08 }

// ---------- BPF instruction ----------

/// One classic-BPF instruction (struct sock_filter on Linux).
#[derive(Debug, Clone, Copy)]
pub struct BpfInsn {
    pub code: u16,
    pub jt:   u8,
    pub jf:   u8,
    pub k:    u32,
}

// ---------- Seccomp data ----------

/// Linux's `struct seccomp_data` layout the BPF program reads from with
/// LD ABS K=offset.  The fields are public so test code can populate them.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SeccompData {
    pub nr: u32,
    pub arch: u32,
    pub instruction_pointer: u64,
    pub args: [u64; 6],
}

impl SeccompData {
    /// Read a 32-bit word at byte offset `off` (BPF_LD|BPF_W|BPF_ABS).
    fn load_word(&self, off: u32) -> Option<u32> {
        // The struct is 64 bytes total.
        const LEN: u32 = 64;
        if off + 4 > LEN { return None; }
        // Reinterpret the struct as bytes.  Endianness: cBPF reads in host
        // byte order; on x86_64 that is little-endian.
        let bytes: &[u8] = unsafe {
            core::slice::from_raw_parts(self as *const _ as *const u8, LEN as usize)
        };
        Some(u32::from_le_bytes([
            bytes[off as usize],
            bytes[off as usize + 1],
            bytes[off as usize + 2],
            bytes[off as usize + 3],
        ]))
    }
}

// ---------- BPF action ----------

pub mod action {
    pub const RET_KILL_PROCESS: u32 = 0x8000_0000;
    pub const RET_TRAP:         u32 = 0x0003_0000;
    pub const RET_ERRNO:        u32 = 0x0005_0000;
    pub const RET_LOG:          u32 = 0x7ffc_0000;
    pub const RET_ALLOW:        u32 = 0x7fff_0000;
    pub const ACTION_MASK:      u32 = 0xffff_0000;
    pub const DATA_MASK:        u32 = 0x0000_ffff;
}

// ---------- Interpreter ----------

const SCRATCH: usize = 16;

pub struct Vm<'a> {
    program: &'a [BpfInsn],
    data:    &'a SeccompData,
    a: u32,
    x: u32,
    mem: [u32; SCRATCH],
}

impl<'a> Vm<'a> {
    pub fn new(program: &'a [BpfInsn], data: &'a SeccompData) -> Self {
        Vm { program, data, a: 0, x: 0, mem: [0; SCRATCH] }
    }

    /// Run the program to its first RET instruction; return the value.
    /// Programs that fall off the end without RET return RET_KILL_PROCESS.
    pub fn run(mut self) -> u32 {
        let mut pc = 0usize;
        let max = self.program.len();
        let mut steps = 0usize;
        while pc < max && steps < 4096 {
            steps += 1;
            let ins = self.program[pc];
            let code = ins.code;
            match class(code) {
                BPF_LD => {
                    match (size(code), mode(code)) {
                        (BPF_W, BPF_ABS) => {
                            self.a = self.data.load_word(ins.k).unwrap_or(0);
                        }
                        (BPF_W, BPF_IMM) => {
                            self.a = ins.k;
                        }
                        (BPF_W, BPF_LEN) => {
                            self.a = 64;
                        }
                        (BPF_W, BPF_MEM) => {
                            let idx = (ins.k as usize) % SCRATCH;
                            self.a = self.mem[idx];
                        }
                        _ => return action::RET_KILL_PROCESS,
                    }
                }
                BPF_LDX => {
                    match (size(code), mode(code)) {
                        (BPF_W, BPF_IMM) => self.x = ins.k,
                        (BPF_W, BPF_MEM) => {
                            let idx = (ins.k as usize) % SCRATCH;
                            self.x = self.mem[idx];
                        }
                        (BPF_W, BPF_LEN) => self.x = 64,
                        _ => return action::RET_KILL_PROCESS,
                    }
                }
                BPF_ST => {
                    let idx = (ins.k as usize) % SCRATCH;
                    self.mem[idx] = self.a;
                }
                BPF_STX => {
                    let idx = (ins.k as usize) % SCRATCH;
                    self.mem[idx] = self.x;
                }
                BPF_ALU => {
                    let rhs = if src(code) == BPF_X { self.x } else { ins.k };
                    self.a = match op(code) {
                        BPF_ADD => self.a.wrapping_add(rhs),
                        BPF_SUB => self.a.wrapping_sub(rhs),
                        BPF_MUL => self.a.wrapping_mul(rhs),
                        BPF_DIV => if rhs == 0 { return action::RET_KILL_PROCESS } else { self.a / rhs },
                        BPF_MOD => if rhs == 0 { return action::RET_KILL_PROCESS } else { self.a % rhs },
                        BPF_OR  => self.a | rhs,
                        BPF_AND => self.a & rhs,
                        BPF_XOR => self.a ^ rhs,
                        BPF_LSH => self.a.wrapping_shl(rhs),
                        BPF_RSH => self.a.wrapping_shr(rhs),
                        BPF_NEG => 0u32.wrapping_sub(self.a),
                        _ => return action::RET_KILL_PROCESS,
                    };
                }
                BPF_JMP => {
                    let rhs = if src(code) == BPF_X { self.x } else { ins.k };
                    let take: bool = match op(code) {
                        BPF_JA => {
                            // Unconditional jump uses k as offset.
                            pc = pc.wrapping_add(1).wrapping_add(ins.k as usize);
                            continue;
                        }
                        BPF_JEQ  => self.a == rhs,
                        BPF_JGT  => self.a >  rhs,
                        BPF_JGE  => self.a >= rhs,
                        BPF_JSET => (self.a & rhs) != 0,
                        _ => return action::RET_KILL_PROCESS,
                    };
                    let off = if take { ins.jt } else { ins.jf } as usize;
                    pc = pc.wrapping_add(1).wrapping_add(off);
                    continue;
                }
                BPF_RET => {
                    // RET|K returns k; RET|A returns A.
                    return if src(code) == BPF_X { self.x } else if mode(code) == 0x10 { self.a } else { ins.k };
                }
                BPF_MISC => {
                    // TAX (0x07) copies A→X, TXA (0x87) copies X→A.
                    match code {
                        0x07 => self.x = self.a,
                        0x87 => self.a = self.x,
                        _ => return action::RET_KILL_PROCESS,
                    }
                }
                _ => return action::RET_KILL_PROCESS,
            }
            pc += 1;
        }
        action::RET_KILL_PROCESS
    }
}

// ---------- Per-process filter table ----------

#[derive(Debug, Clone)]
pub struct Filter {
    pub program: Vec<BpfInsn>,
    pub pid: usize,
}

const MAX_FILTERS: usize = 16;

pub struct FilterTable {
    slots: [Option<Filter>; MAX_FILTERS],
}

impl FilterTable {
    pub const fn new() -> Self {
        const N: Option<Filter> = None;
        FilterTable { slots: [N; MAX_FILTERS] }
    }

    pub fn install(&mut self, pid: usize, prog: Vec<BpfInsn>) -> bool {
        for slot in self.slots.iter_mut() {
            if let Some(existing) = slot {
                if existing.pid == pid {
                    existing.program = prog;
                    return true;
                }
            }
        }
        for slot in self.slots.iter_mut() {
            if slot.is_none() {
                *slot = Some(Filter { program: prog, pid });
                return true;
            }
        }
        false
    }

    pub fn for_pid(&self, pid: usize) -> Option<&Filter> {
        self.slots.iter().filter_map(|s| s.as_ref()).find(|f| f.pid == pid)
    }

    pub fn remove(&mut self, pid: usize) {
        for slot in self.slots.iter_mut() {
            if let Some(f) = slot { if f.pid == pid { *slot = None; return; } }
        }
    }

    pub fn count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

pub static FILTERS: Mutex<FilterTable> = Mutex::new(FilterTable::new());

// ---------- High-level evaluation entry point ----------

/// Action dispatch summary, what the syscall layer reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Errno(u16),
    Log,
    Kill,
    Trap,
}

pub fn check(pid: usize, syscall_num: usize, args: [u64; 6]) -> Decision {
    let table = FILTERS.lock();
    let filter = match table.for_pid(pid) { Some(f) => f, None => return Decision::Allow };

    let data = SeccompData {
        nr: syscall_num as u32,
        arch: 0xc000_003e, // SECCOMP_AUDIT_ARCH_X86_64
        instruction_pointer: 0,
        args,
    };

    let ret = Vm::new(&filter.program, &data).run();
    let masked = ret & action::ACTION_MASK;
    match masked {
        action::RET_ALLOW => Decision::Allow,
        action::RET_LOG   => Decision::Log,
        action::RET_TRAP  => Decision::Trap,
        action::RET_ERRNO => Decision::Errno((ret & action::DATA_MASK) as u16),
        action::RET_KILL_PROCESS => Decision::Kill,
        _ => Decision::Allow,
    }
}

// ---------- Convenience program builders ----------

/// Build a flat allow-list program: every syscall in `allowed` returns
/// ALLOW, anything else returns the given default action.
pub fn build_allowlist(allowed: &[u32], default: u32) -> Vec<BpfInsn> {
    let mut prog = Vec::new();
    // LD nr
    prog.push(BpfInsn { code: BPF_LD | BPF_W | BPF_ABS, jt: 0, jf: 0, k: 0 });
    // For each allowed syscall: JEQ k, jt -> ret allow, jf -> next test.
    // We use a linear chain: JEQ allowed[i], 0, 1 (skip the next ret if not eq);
    // ret allow.
    for &nr in allowed {
        prog.push(BpfInsn { code: BPF_JMP | BPF_JEQ | BPF_K, jt: 0, jf: 1, k: nr });
        prog.push(BpfInsn { code: BPF_RET | BPF_K, jt: 0, jf: 0, k: action::RET_ALLOW });
    }
    // Default action.
    prog.push(BpfInsn { code: BPF_RET | BPF_K, jt: 0, jf: 0, k: default });
    prog
}
