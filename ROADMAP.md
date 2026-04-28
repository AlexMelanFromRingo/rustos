# RustOS Development Roadmap

Target: Ubuntu Server-like CLI operating system.
Reference architecture: Linux/Unix-like.

---

## Phase 1: Kernel Hardening (Current → Stable Foundation)

### 1.1 Memory Management Improvements
- [x] Increase heap from 100 KiB to 16 MiB ✅
- [x] Implement proper `FrameAllocator` with bitmap (O(1) alloc/dealloc) ✅
- [x] Add frame deallocation support ✅
- [x] Implement slab allocator for kernel objects (Bonwick-style object cache, kmalloc sizes 16..4096, /proc/slabinfo) ✅
- [x] Add kernel virtual memory allocator (vmalloc/vfree, 64 MiB region, bitmap free list, /vmalloc-test) ✅
- [x] Guard pages for kernel stack overflow detection ✅
- [ ] Per-process address spaces (separate page tables per process)

### 1.2 Process Management
- [ ] Per-process page tables (full address space isolation)
- [x] Per-process file descriptor tables (FdTableAccess with process/kernel dispatch) ✅
- [x] Per-process working directory (Process.cwd, sys_chdir/sys_getcwd, SHELL_CWD fallback) ✅
- [ ] Proper `fork()` with COW (Copy-On-Write) pages
- [x] `waitpid()` with blocking (200k-iter retry loop with HLT yield + WNOHANG support) ✅
- [x] Signal delivery (SIGTERM, SIGKILL, SIGINT, SIGCHLD, etc.) ✅
- [x] TTY device abstraction (line discipline, raw/cooked mode) ✅
- [x] Process groups and sessions (pgid/sid on Process, syscalls 109/121/124/112, ps -o PGID/SID) ✅
- [x] Environment variables (shell-level: export/unset/$VAR expansion) ✅
- [x] Resource limits (15 RLIMIT_* resources, getrlimit/setrlimit syscalls 97/160, ulimit shell command) ✅

### 1.3 Scheduler Improvements
- [x] Priority-based scheduling (nice values -20..19, dynamic quantum) ✅
- [x] CFS (Completely Fair Scheduler) inspired design (vruntime + Linux nice weights) ✅
- [x] Proper process blocking on I/O (sleep queues) ✅
- [ ] Multi-core support (per-CPU run queues, SMP init)

### 1.4 Interrupt & Exception Handling
- [ ] APIC support (replace legacy 8259 PIC for multi-core)
- [ ] IOAPIC for IRQ routing
- [ ] MSI/MSI-X support for modern devices
- [x] NMI handling (logs RIP and continues, MCE halts) ✅
- [x] Kernel panic stack trace (RBP chain via force-frame-pointers; DWARF symbolisation pending) ✅

---

## Phase 2: System Call Layer (Linux ABI Compatibility)

### 2.1 Core Syscalls
- [x] `mmap` / `munmap` — anonymous private mappings with frame allocation ✅
- [x] `brk` — heap management for user programs ✅
- [x] `lseek` — file seek ✅
- [x] `stat` / `fstat` — file metadata (Linux-compatible 144-byte struct) ✅
- [x] `ioctl` — terminal control (TIOCGWINSZ, TCGETS, FIONREAD) ✅
- [x] `dup` / `dup2` — file descriptor duplication ✅
- [x] `pipe` — inter-process communication (4 KiB kernel buffer) ✅
- [x] `select` / `poll` — I/O multiplexing (do_poll, do_select, fd_readiness, pollfd ABI struct, FD_SETSIZE=1024) ✅
- [x] `epoll` — epoll_create/_ctl/_wait, EPOLLIN/OUT/PRI/ERR/HUP/ET, syscalls 213/232/233 ✅
- [x] AF_UNIX domain sockets (SOCK_STREAM listen/accept/connect, SOCK_DGRAM sendto/recvfrom) ✅
- [x] `socket` / `bind` — kernel-side AF_INET/SOCK_DGRAM (UDP) ✅
- [x] `listen` / `accept` / `connect` — TCP loopback (3-way handshake, send/recv, FIN close) ✅
- [x] `sendto` / `recvfrom` — UDP datagram I/O over loopback ✅
- [x] `clock_gettime` — time (REALTIME from RTC, MONOTONIC from PIT) ✅
- [x] `getuid` / `getgid` / `geteuid` / `getegid` — user management ✅
- [x] `mkdir` / `rmdir` / `unlink` / `rename` ✅
- [x] `symlink` / `readlink` ✅

### 2.2 Signal System
- [x] `kill` — send signal (syscall #62) ✅
- [x] `signal` / `sigaction` — per-process sigactions array, syscalls 13/48 ✅
- [x] `sigprocmask` — signal blocking (per-process blocked mask) ✅
- [x] Signal delivery during scheduler tick ✅

### 2.3 Thread Support
- [x] `clone` syscall (delegates to fork; CLONE_THREAD flag noted but VM/files not yet shared) ✅
- [ ] Thread-local storage (TLS)
- [x] Futex for userspace synchronization (FUTEX_WAIT/WAKE, per-uaddr wait queues, syscall 202) ✅

---

## Phase 3: Filesystem Layer

### 3.1 VFS Enhancement
- [x] Proper inode abstraction (Inode struct with stable ino, mode, uid, gid, size, nlink) ✅
- [x] Dentry cache (path -&gt; Inode, /proc/inodes view) ✅
- [x] Mount table with multiple mount points (MOUNT_TABLE, /proc/mounts, mount/-t/umount) ✅
- [x] Path resolution with symlinks ✅
- [x] File locking (flock — LOCK_SH/EX/NB/UN, advisory lock table) ✅
- [x] File locking (fcntl byte-range — ByteLockTable, overlap detection, fcntl shell command) ✅
- [x] File permissions (rwxrwxrwx, uid/gid) ✅
- [x] Timestamps (atime, mtime, ctime) ✅

### 3.2 FAT32 Completion
- [x] Long Filename (LFN) support (VFAT entries decoded with checksum verify, sequence + UTF-16LE) ✅
- [x] Subdirectory navigation and nested paths (find_directory_cluster walks components) ✅
- [x] Write to both FAT copies (mirror per BPB num_fats; preserves reserved top 4 bits) ✅
- [x] FSInfo sector updates for free cluster tracking (decrements on alloc, increments on free, last-alloc hint) ✅
- [x] File truncation (truncate_chain with explicit keep_clusters parameter) ✅
- [ ] Proper error recovery

### 3.3 Ext2/Ext4 Filesystem
- [x] Ext2 read support (superblock, BGDT, inodes, direct + 1/2/3-level indirect blocks, dir entries, lookup-by-path) ✅
- [x] Ext2 mount from disk via virtio-blk (VirtioBlkSource adapter, auto-mount on boot if magic at sector 2) ✅
- [ ] Ext2 write support
- [ ] Ext4 basic support (extents, large files)
- [ ] Journal support for crash recovery

### 3.4 Special Filesystems
- [x] `/proc` — process information filesystem (uptime, meminfo, version, cpuinfo, kmsg, loadavg, stat, per-pid) ✅
- [x] `/sys` — sysfs (kernel info, device tree: serial, keyboard, timer, rtc, vga) ✅
- [x] `/dev` — device nodes (null, zero, random, urandom, console, tty, kmsg, mem) ✅
- [x] `/tmp` — tmpfs (RAM-backed, 128 files, 512 KiB/file) ✅
- [x] devtmpfs for automatic device node creation (DEV_NODES registry, DevKind, mknod shell command, ls -l shows c/b types) ✅

---

## Phase 4: Device Drivers

### 4.1 Storage
- [ ] AHCI/SATA driver (replace PIO ATA with DMA)
- [ ] NVMe driver for modern storage
- [x] Virtio-blk for QEMU performance (legacy I/O port layout, contiguous DMA, NO_INTERRUPT poll, ext2 mount on boot) ✅
- [x] Partition table parsing (MBR; GPT detection via protective entry only) ✅

### 4.2 Network Stack
- [x] RTL8139 NIC driver (PCI 10ec:8139, 8K RX ring + 4 TX bounce buffers, polling, MAC read) ✅
- [ ] E1000 NIC driver (Intel, used by QEMU)
- [x] Virtio-net driver (PCI scan, BAR0 I/O, RESET→ACK→DRIVER→FEATURES→DRIVER_OK, RX/TX virtqueues, MAC read from device config) ✅
- [x] Ethernet frame handling (build_frame, EthHeader parse, ethertypes IPv4/ARP/IPv6) ✅
- [x] ARP (RFC 826: REQUEST/REPLY build+parse, ARP cache, arp shell command) ✅
- [x] IPv4 stack (IP header parse/serialise, RFC 1071 checksum) ✅
- [x] ICMP echo (ping over loopback works end-to-end) ✅
- [x] UDP (build_datagram + ipv4_input dispatch + sendto/recvfrom) ✅
- [x] TCP loopback (handshake/send/recv/close) — congestion control NOT implemented ✅
- [x] DHCP client (DISCOVER/OFFER/REQUEST/ACK build+parse, dhclient shell command, RFC 2131 wire format) ✅
- [x] DNS resolver (RFC 1035 A-record query/response, /etc/resolv.conf, TTL-bounded cache, /etc/hosts fallback) ✅
- [x] Network socket API (AF_INET/UDP/TCP + AF_UNIX, kernel-side socket/bind/listen/accept/connect/sendto/recvfrom/close) ✅
- [x] `ping` command (real ICMP echo) ✅
- [x] `wget` / `curl` equivalent (HTTP/1.0 client, -O / --, dns-aware) ✅

### 4.3 Input/Output
- [x] PS/2 mouse driver (IRQ 12, 8042 init, 3-byte packet decode, X/Y/buttons) ✅
- [ ] USB HID (keyboard/mouse via UHCI/EHCI/xHCI)
- [ ] Framebuffer driver (VESA/VBE for graphics mode)
- [ ] Serial console improvements (full terminal emulation)

### 4.4 Timer & Clock
- [x] HPET (MMIO at 0xFED0_0000, ENABLE_CNF, period→ns conversion) ✅
- [x] TSC (Time Stamp Counter) calibration via PIT channel 2, ns-precision monotonic ✅
- [x] RTC (Real-Time Clock) — CMOS MC146818, BCD/binary auto-detect ✅
- [x] `clock_gettime` with nanosecond precision (CLOCK_MONOTONIC backed by TSC) ✅

---

## Phase 5: User Space Environment

### 5.1 C Runtime / musl
- [ ] Implement minimal C runtime (crt0, syscall wrappers)
- [ ] Port musl libc (or newlib) for POSIX compatibility
- [ ] Dynamic linking support (ELF .so loading)
- [ ] Position-Independent Executables (PIE)

### 5.2 Shell Improvements
- [x] Job control (background processes with &, fg, bg, jobs) ✅
- [x] Shell scripting (if/then/elif/else/fi, while/do/done, for/in/do/done) ✅
- [x] Environment variable expansion ($VAR, $?) ✅
- [x] Command substitution ($(cmd)) ✅
- [x] Here documents (<<MARKER ... MARKER, materialised to /tmp/.heredoc.tmp) ✅
- [x] Glob expansion (*.txt, /dev/n*) ✅

### 5.3 Core Utilities
- [ ] Port coreutils (or implement in Rust): ls, cat, cp, mv, rm, mkdir, chmod, chown, etc.
- [x] `init` / service manager — runlevels, dependencies, restart policies ✅
- [x] `login` / `getty` — interactive login at boot via /etc/getty.conf require_login=1 ✅
- [x] `/etc/passwd`, `/etc/group` — user database (User/Group/UserDb) ✅
- [x] `su` — switch user with password prompt ✅
- [x] `passwd` — change own password ✅
- [x] `useradd` — add new user (root only) ✅
- [x] `sudo` — fine-grained privilege escalation (parses /etc/sudoers, NOPASSWD support, syslog logging) ✅
- [x] `top` — process monitor (one-shot) ✅
- [x] `mount` / `umount` — filesystem mounting (FAT32) ✅
- [x] `dmesg` — kernel log (ring buffer, 512 entries, log levels) ✅
- [x] `syslog` / `logger` — RFC 3164 ring buffer with periodic flush to /var/log/messages ✅

### 5.4 Package Manager
- [x] Simple package format (text manifest + inline FILE blocks) ✅
- [ ] Package repository support (over network)
- [x] Dependency resolution (DEPENDS in manifest, refuse install/remove on missing/required-by) ✅
- [x] Install / remove / list / info operations (`pkg` shell command, /var/lib/pkg) ✅

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

- [x] ASLR for vmalloc (16 MiB random slide on top of nominal base, TSC + RTC seed) ✅
- [x] Stack canaries (canary()/assert_canary() helpers + force-frame-pointers + RBP-chain backtrace) ✅
- [x] NX bit enforcement on vmalloc data pages (W^X) ✅
- [x] Capability-based security model (20 Linux caps, getcap/setcap, current_has/check API) ✅
- [x] Seccomp-like syscall filtering (per-PID filter, Allow/Errno/Kill/Log actions, dispatch hook) ✅
- [x] Kernel hardening: CPUID-probe + CR4 wiring for SMEP/SMAP, KASLR via vmalloc base randomisation ✅

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
