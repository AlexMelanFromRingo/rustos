# RustOS Development Roadmap

Target: Ubuntu Server-like CLI operating system.
Reference architecture: Linux/Unix-like.

---

## Phase 1: Kernel Hardening (Current → Stable Foundation)

### 1.1 Memory Management Improvements
- [x] Increase heap from 100 KiB to 16 MiB ✅
- [x] Implement proper `FrameAllocator` with bitmap (O(1) alloc/dealloc) ✅
- [x] Add frame deallocation support ✅
- [ ] Implement slab allocator for kernel objects (replace fixed-size block allocator)
- [ ] Add kernel virtual memory allocator (vmalloc equivalent)
- [x] Guard pages for kernel stack overflow detection ✅
- [ ] Per-process address spaces (separate page tables per process)

### 1.2 Process Management
- [ ] Per-process page tables (full address space isolation)
- [ ] Per-process file descriptor tables (current: single global table)
- [ ] Per-process working directory (current: shell-level only)
- [ ] Proper `fork()` with COW (Copy-On-Write) pages
- [ ] `waitpid()` with blocking (current: non-blocking poll)
- [x] Signal delivery (SIGTERM, SIGKILL, SIGINT, SIGCHLD, etc.) ✅
- [ ] Process groups and sessions
- [x] Environment variables (shell-level: export/unset/$VAR expansion) ✅
- [ ] Resource limits (rlimits)

### 1.3 Scheduler Improvements
- [x] Priority-based scheduling (nice values -20..19, dynamic quantum) ✅
- [ ] CFS (Completely Fair Scheduler) inspired design
- [x] Proper process blocking on I/O (sleep queues) ✅
- [ ] Multi-core support (per-CPU run queues, SMP init)

### 1.4 Interrupt & Exception Handling
- [ ] APIC support (replace legacy 8259 PIC for multi-core)
- [ ] IOAPIC for IRQ routing
- [ ] MSI/MSI-X support for modern devices
- [ ] NMI handling
- [ ] Proper kernel panic with stack trace (using DWARF unwind info)

---

## Phase 2: System Call Layer (Linux ABI Compatibility)

### 2.1 Core Syscalls
- [ ] `mmap` / `munmap` — memory-mapped files and anonymous mappings
- [x] `brk` — heap management for user programs ✅
- [x] `lseek` — file seek ✅
- [x] `stat` / `fstat` — file metadata (Linux-compatible 144-byte struct) ✅
- [ ] `ioctl` — device control
- [x] `dup` / `dup2` — file descriptor duplication ✅
- [x] `pipe` — inter-process communication (4 KiB kernel buffer) ✅
- [ ] `select` / `poll` / `epoll` — I/O multiplexing
- [ ] `socket` / `bind` / `listen` / `accept` / `connect` — networking
- [ ] `sendto` / `recvfrom` — network I/O
- [x] `clock_gettime` — time (REALTIME from RTC, MONOTONIC from PIT) ✅
- [x] `getuid` / `getgid` / `geteuid` / `getegid` — user management ✅
- [x] `mkdir` / `rmdir` / `unlink` / `rename` ✅
- [ ] `link` / `symlink`

### 2.2 Signal System
- [x] `kill` — send signal (syscall #62) ✅
- [ ] `signal` / `sigaction` — custom signal handlers
- [x] `sigprocmask` — signal blocking (per-process blocked mask) ✅
- [x] Signal delivery during scheduler tick ✅

### 2.3 Thread Support
- [ ] `clone` syscall with CLONE_THREAD
- [ ] Thread-local storage (TLS)
- [ ] Futex for userspace synchronization

---

## Phase 3: Filesystem Layer

### 3.1 VFS Enhancement
- [ ] Proper inode abstraction
- [ ] Dentry cache (directory entry cache)
- [ ] Mount table with multiple mount points
- [ ] Path resolution with symlinks
- [ ] File locking (flock, fcntl)
- [ ] File permissions (rwxrwxrwx, uid/gid)
- [ ] Timestamps (atime, mtime, ctime)

### 3.2 FAT32 Completion
- [ ] Long Filename (LFN) support (currently 8.3 only)
- [ ] Subdirectory navigation and nested paths
- [ ] Write to both FAT copies
- [ ] FSInfo sector updates for free cluster tracking
- [ ] File truncation
- [ ] Proper error recovery

### 3.3 Ext2/Ext4 Filesystem
- [ ] Ext2 read support (simpler, good starting point)
- [ ] Ext2 write support
- [ ] Ext4 basic support (extents, large files)
- [ ] Journal support for crash recovery

### 3.4 Special Filesystems
- [x] `/proc` — process information filesystem (uptime, meminfo, version, cpuinfo, kmsg, loadavg, stat, per-pid) ✅
- [ ] `/sys` — sysfs for device information
- [ ] `/dev` — device nodes (character and block devices)
- [ ] `/tmp` — tmpfs (RAM-backed filesystem)
- [ ] devtmpfs for automatic device node creation

---

## Phase 4: Device Drivers

### 4.1 Storage
- [ ] AHCI/SATA driver (replace PIO ATA with DMA)
- [ ] NVMe driver for modern storage
- [ ] Virtio-blk for QEMU performance
- [ ] Partition table parsing (MBR and GPT)

### 4.2 Network Stack
- [ ] RTL8139 NIC driver (simple, well-documented)
- [ ] E1000 NIC driver (Intel, used by QEMU)
- [ ] Virtio-net driver (QEMU paravirt)
- [ ] Ethernet frame handling
- [ ] ARP (Address Resolution Protocol)
- [ ] IPv4 stack (IP, ICMP)
- [ ] UDP
- [ ] TCP (connection management, flow control, congestion control)
- [ ] DHCP client
- [ ] DNS resolver
- [ ] Network socket API
- [ ] `ping` command
- [ ] `wget` / `curl` equivalent

### 4.3 Input/Output
- [ ] PS/2 mouse driver
- [ ] USB HID (keyboard/mouse via UHCI/EHCI/xHCI)
- [ ] Framebuffer driver (VESA/VBE for graphics mode)
- [ ] Serial console improvements (full terminal emulation)

### 4.4 Timer & Clock
- [ ] HPET (High Precision Event Timer)
- [ ] TSC (Time Stamp Counter) calibration
- [x] RTC (Real-Time Clock) — CMOS MC146818, BCD/binary auto-detect ✅
- [ ] `clock_gettime` with nanosecond precision (currently second-level from RTC)

---

## Phase 5: User Space Environment

### 5.1 C Runtime / musl
- [ ] Implement minimal C runtime (crt0, syscall wrappers)
- [ ] Port musl libc (or newlib) for POSIX compatibility
- [ ] Dynamic linking support (ELF .so loading)
- [ ] Position-Independent Executables (PIE)

### 5.2 Shell Improvements
- [ ] Job control (background processes with &, fg, bg)
- [ ] Shell scripting (if/then/else, for, while loops)
- [x] Environment variable expansion ($VAR) ✅
- [ ] Command substitution ($(cmd))
- [ ] Here documents (<<EOF)
- [ ] Glob expansion (*.txt)

### 5.3 Core Utilities
- [ ] Port coreutils (or implement in Rust): ls, cat, cp, mv, rm, mkdir, chmod, chown, etc.
- [ ] `init` process (PID 1) — proper system initialization
- [ ] `login` / `getty` — user authentication
- [ ] `/etc/passwd`, `/etc/shadow` — user database
- [ ] `su` / `sudo` — privilege escalation
- [x] `top` — process monitor (one-shot) ✅
- [x] `mount` / `umount` — filesystem mounting (FAT32) ✅
- [x] `dmesg` — kernel log (ring buffer, 512 entries, log levels) ✅

### 5.4 Package Manager
- [ ] Simple package format (.tar.gz with manifest)
- [ ] Package repository support (over network)
- [ ] Dependency resolution
- [ ] Install / remove / update operations

---

## Phase 6: Bootloader Study & Migration

### 6.1 Current State: `bootloader` Crate v0.9.x
The `bootloader` crate currently handles:
- Setting up long mode (64-bit)
- Creating initial page tables with physical memory mapping
- Parsing memory map from BIOS/UEFI
- Loading kernel ELF into memory
- Passing BootInfo struct to kernel

### 6.2 Migration Options
**Option A: Upgrade to bootloader v0.11+ (Edition 3)**
- New API, UEFI support
- Simpler configuration
- Less control over boot process

**Option B: Custom Bootloader**
- Full control over boot process
- BIOS boot: Stage 1 (MBR, 512 bytes) → Stage 2 (real mode → protected → long mode)
- UEFI boot: EFI application that sets up kernel
- Implement own memory map parsing
- Much more complex but educational

**Option C: Limine Protocol**
- Modern bootloader protocol
- Both BIOS and UEFI support
- Well-documented, growing community
- Easier than custom, more control than `bootloader` crate

### 6.3 Recommendation
Start with **Option A** (upgrade to bootloader v0.11+) for UEFI support, then evaluate **Option C** (Limine) for production. Custom bootloader (Option B) is only worth it if boot-level control is a project goal.

---

## Phase 7: Security

- [ ] ASLR (Address Space Layout Randomization)
- [ ] Stack canaries
- [ ] NX bit enforcement (W^X policy)
- [ ] Capability-based security model
- [ ] Seccomp-like syscall filtering
- [ ] Kernel hardening (SMEP, SMAP, KASLR)

---

## Priority Order

1. **Memory management** (Phase 1.1) — foundation for everything else
2. **Per-process isolation** (Phase 1.2) — critical for security
3. **Filesystem layer** (Phase 3.1-3.2) — needed for usable system
4. **Network stack** (Phase 4.2) — user's explicit goal
5. **Syscall completion** (Phase 2) — enables user programs
6. **User space** (Phase 5) — makes OS actually usable
7. **Bootloader migration** (Phase 6) — can be done incrementally
8. **Security** (Phase 7) — harden after functional

---

## Technical Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Allocator | Slab + buddy system | Industry standard, O(1) allocation |
| Scheduler | CFS-inspired | Fair, scales well, proven design |
| Network | smoltcp crate | Mature, no_std, TCP/IP stack |
| Filesystem | Ext2 primary | Simpler than ext4, widely supported |
| Bootloader | Upgrade to 0.11, then Limine | Incremental migration path |
| User ABI | Linux x86_64 | Maximum compatibility |
| C library | musl port | Small, correct, well-tested |
