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
- [x] Per-process address spaces (private PML4 per user process, kernel mappings shared by reference, user L4 entry deep-cloned) ✅
- [x] Contiguous DMA allocator (alloc_dma_contig, used by virtio / AHCI / NVMe / NIC drivers) ✅

### 1.2 Process Management
- [x] Per-process page tables (private PML4 per user process; kernel L4 shared by reference; user L4 deep-cloned; CR3 switched in timer ISR) ✅
- [x] Per-process file descriptor tables (FdTableAccess with process/kernel dispatch) ✅
- [x] Per-process working directory (Process.cwd, sys_chdir/sys_getcwd, SHELL_CWD fallback) ✅
- [x] Proper `fork()` with COW (PTE bit 9 marks COW, refcount table, page-fault handler resolves to private frame on write) ✅
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
- [~] Multi-core support — IPI plumbing + AP ping trampoline (INIT-SIPI-SIPI delivery, 0xDEADBEEF handshake at phys 0x9000, per-CPU CpuState array, opt-in `smp boot` shell command); long-mode trampoline + per-CPU run queues remain

### 1.4 Interrupt & Exception Handling
- [x] APIC support (LAPIC + IOAPIC discovery, EOI via LAPIC, 8259 PIC retired through ACPI MADT ISO routing) ✅
- [x] IOAPIC for IRQ routing (every existing IRQ programmed via apic::ioapic_route honouring MADT overrides) ✅
- [x] MSI/MSI-X support: pci::enable_msi programs Message Address (LAPIC) + Data, verified round-trip on e1000e ✅
- [x] NMI handling (logs RIP and continues, MCE halts) ✅
- [x] Kernel panic stack trace + DWARF symbolisation (function+offset (file:line) via build.rs nm + addr2line, two-pass build) ✅

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
- [x] Thread-local storage (TLS) — arch_prctl(2) ARCH_SET/GET_FS|GS, FSBASE/GSBASE MSRs, per-Process fs_base/gs_base ✅
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
- [x] Error recovery (BPB sanity validation on mount, FSInfo signature check + free-count rebuild from FAT scan) ✅

### 3.3 Ext2/Ext4 Filesystem
- [x] Ext2 read support (superblock, BGDT, inodes, direct + 1/2/3-level indirect blocks, dir entries, lookup-by-path) ✅
- [x] Ext2 mount from disk via virtio-blk (VirtioBlkSource adapter, auto-mount on boot if magic at sector 2) ✅
- [x] Ext2 write support (alloc inode/block, write_inode_data, add_dir_entry, create_root_file; round-trips with Linux debugfs) ✅
- [x] Ext4 read with extents (eh_magic 0xF30A, leaf + index nodes, sparse handling) ✅
- [x] Ext3-style journal replay (jbd2 magic 0xC03B3998, descriptor + commit walk, marks s_start = 0 after replay) ✅

### 3.4 Special Filesystems
- [x] `/proc` — process information filesystem (uptime, meminfo, version, cpuinfo, kmsg, loadavg, stat, per-pid, interrupts, diskstats, swaps, cmdline, partitions, sys/kernel/{ostype,osrelease,version,hostname}) ✅
- [x] `/sys` — sysfs (kernel info, device tree: serial, keyboard, timer, rtc, vga) ✅
- [x] `/dev` — device nodes (null, zero, random, urandom, console, tty, kmsg, mem) ✅
- [x] `/tmp` — tmpfs (RAM-backed, 128 files, 512 KiB/file) ✅
- [x] devtmpfs for automatic device node creation (DEV_NODES registry, DevKind, mknod shell command, ls -l shows c/b types) ✅

---

## Phase 4: Device Drivers

### 4.1 Storage
- [x] AHCI/SATA driver (Intel AHCI 1.3.1; per-port command list + FIS receive area; READ/WRITE_DMA_EXT; polling) ✅
- [x] NVMe driver (NVM Express 1.4; admin queue + I/O queue; IDENTIFY CONTROLLER + NAMESPACE; READ/WRITE; polling) ✅
- [x] Virtio-blk for QEMU performance (legacy I/O port layout, contiguous DMA, NO_INTERRUPT poll, ext2 mount on boot) ✅
- [x] Generic BlockDevice trait (vblk0/sd*/nvme0n1 unified; Ext2 mounts from any backend; per-device stat counters surface in /proc/diskstats) ✅
- [x] Partition table parsing (MBR; GPT detection via protective entry only) ✅

### 4.2 Network Stack
- [x] RTL8139 NIC driver (PCI 10ec:8139, 8K RX ring + 4 TX bounce buffers, polling, MAC read) ✅
- [x] E1000 NIC driver (PCI 8086:100E, MMIO, 32 RX + 32 TX descriptors with DMA bounce buffers, EEPROM-or-RAL MAC) ✅
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
- [x] Framebuffer driver: `src/framebuffer.rs` — `Surface` trait with `LinearFb` (MMIO) + `MemSurface` (heap, test-only); pixel-format-agnostic primitives (clear, fill_rect, rect outline, Bresenham line, 8×8 glyph + text), 7 dedicated tests ✅
- [x] Serial console / terminal emulation: `src/term.rs` — Paul Williams DEC ANSI parser FSM, full VT-220/xterm coverage (CUP, SGR incl. 256-colour, OSC titles, alt screen, DECSC/DECRC, cursor visibility, CSI parameter parsing), 16 dedicated tests ✅

### 4.4 Timer & Clock
- [x] HPET (MMIO at 0xFED0_0000, ENABLE_CNF, period→ns conversion) ✅
- [x] TSC (Time Stamp Counter) calibration via PIT channel 2, ns-precision monotonic ✅
- [x] RTC (Real-Time Clock) — CMOS MC146818, BCD/binary auto-detect ✅
- [x] `clock_gettime` with nanosecond precision (CLOCK_MONOTONIC backed by TSC) ✅

---

## Phase 5: User Space Environment

### 5.1 C Runtime / musl
- [x] Minimal in-tree user runtime (`src/userlib.rs`): syscall0–3 wrappers, write/read/exit/getpid/brk POSIX shims, bump-pointer malloc on top of brk, tiny printf with %d/%u/%x/%s/%c — covers what the in-tree `userspace::user_program_a/b` needs without third-party libc ✅
- [ ] Port musl libc (or newlib) for full POSIX compatibility
- [x] PIE (Position-Independent Executables) loader: ET_DYN accepted, PT_DYNAMIC scanned, R_X86_64_RELATIVE / R_X86_64_64 / GLOB_DAT / JUMP_SLOT applied via `apply_rela_with(... resolver)` — proven against synthetic ELFs in 4 dedicated tests ✅
- [ ] Dynamic linking support (ELF .so loading) — relocation engine done; PT_INTERP / DT_NEEDED resolution + symbol-versioning tables remain

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
- [x] Package repository support over network (`pkg fetch URL` + `pkg fetch-install URL` via HTTP/1.0 GET on TCP) ✅
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

### 6.4 Migration evaluation (2026-04)

After the kernel grew to ~50 KSLoC and started using `bootloader 0.9`'s
`map_physical_memory` heavily (DMA contiguous allocation, APIC MMIO,
LAPIC + IOAPIC discovery, virtio-blk/net/e1000 RX/TX descriptor rings),
upgrading is non-trivial:

* **0.11 / Edition 3** changes the entry-point ABI from `_start(BootInfo)`
  to a `bootloader_api`-driven entry; we'd refactor `kernel_main` and
  every `boot_info.physical_memory_offset` reader.  The new
  `BootInfo::physical_memory_offset` is `Optional<u64>` (firmware can
  decline the mapping) so every `phys_offset()` consumer needs a
  fallback path.  Worth it for UEFI but a significant churn (~ a day
  of careful work + re-running every device-driver self-test).
* **Limine** would give us UEFI + a richer info struct (modules, RSDP,
  framebuffer, SMP startup hand-off) but introduces a different build
  pipeline (`limine.conf`, Limine binary in tree, separate ESP image
  for UEFI).  Best after we have a real userspace + ACPI-MADT
  consumer that benefits from the SMP hand-off.

**Decision (deferred):** stay on bootloader 0.9 until either (a) the
tooling forces UEFI (current QEMU SeaBIOS is fine), or (b) Limine's
SMP info becomes load-bearing for the per-CPU work.  Both are
post-fork+COW + per-process page-table milestones.

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
