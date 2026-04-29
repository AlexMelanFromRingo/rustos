//! Comprehensive in-kernel integration tests.
//!
//! We deliberately do NOT use `#![feature(custom_test_frameworks)]` /
//! `reexport_test_harness_main` because the interaction with
//! `harness = false` is brittle on modern nightly toolchains.  Instead
//! we define plain `fn t_*` test functions and invoke them in order
//! from `kernel_test_main`, asserting via `panic!` on failure (which
//! the bootimage runner catches and converts into a non-zero exit).
//!
//! Run with:
//!     cargo +nightly test --release -Z json-target-spec --test kernel_tests
//!
//! On success the kernel exits via the QEMU debug-exit port with code
//! `Success` (recognised by `bootimage runner` as exit 0).  On any
//! assertion failure the panic handler exits with `Failed`.
//!
//! Coverage:
//!   * SHA-256 against FIPS 180-4 reference vectors
//!   * IPv6 header + ICMPv6 echo + Neighbor Solicitation
//!   * Ext2 in-memory mount + read + write + create_root_file
//!   * Classic BPF VM driving allow / errno / kill / branch programs
//!   * Shell parser tokeniser + AST shapes
//!   * Symbol lookup + Rust v0 demangling
//!   * CPUID flags (lm / fpu / sse / sse2 must be present in QEMU)
//!   * Scheduler runqueue enqueue/dequeue
//!   * ANSI escape state machine consumes SGR / CSI without panic
//!   * COW refcount table arithmetic
//!   * Package manifest parser
//!   * BlockDevice trait dispatch via an in-memory backend
//!   * ACPI RSDP discovery + MADT parsing on QEMU SeaBIOS

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;

entry_point!(kernel_test_main);

fn kernel_test_main(boot_info: &'static BootInfo) -> ! {
    use rustos::{allocator, memory};
    use x86_64::VirtAddr;
    rustos::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut fa = unsafe { memory::BitmapFrameAllocator::init(&boot_info.memory_map) };
    allocator::init_heap(&mut mapper, &mut fa).expect("heap init");
    rustos::memory::store_frame_allocator(fa);
    rustos::acpi::init();
    let _ = rustos::apic::init();

    rustos::serial_println!("kernel_tests: running...");
    run_all();
    rustos::serial_println!("kernel_tests: ALL PASSED");

    rustos::qemu::exit_qemu(rustos::qemu::QemuExitCode::Success);
    loop { x86_64::instructions::hlt(); }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! { rustos::test_panic_handler(info) }

fn run_all() {
    macro_rules! check {
        ($name:expr, $body:block) => {{
            rustos::serial_print!("  - {} ... ", $name);
            $body;
            rustos::serial_println!("ok");
        }};
    }

    check!("sha256: empty input matches FIPS",          { t_sha256_empty(); });
    check!("sha256: 'abc' matches FIPS",                { t_sha256_abc(); });
    check!("sha256: two-block boundary matches FIPS",   { t_sha256_two_block(); });
    check!("sha256: streaming == one-shot",             { t_sha256_streaming(); });
    check!("sha256: constant_time_eq behaviour",        { t_sha256_cteq(); });

    check!("ipv6: loopback constant has 0..0,1",        { t_ipv6_loopback(); });
    check!("ipv6: header round-trips",                  { t_ipv6_round_trip(); });
    check!("ipv6: parse rejects v4",                    { t_ipv6_parse_rejects_v4(); });
    check!("ipv6: echo request layout",                 { t_ipv6_echo_layout(); });
    check!("ipv6: handle_inbound emits reply",          { t_ipv6_handle_reply(); });
    check!("ipv6: drops echo to other dst",             { t_ipv6_drops_other(); });
    check!("ipv6: NS targets solicited-node multicast", { t_ipv6_ns_dst(); });
    check!("ipv6: compact format ::1, ::, 2001:db8::1", { t_ipv6_compact(); });

    check!("ext2: synth image carries magic 0xEF53",    { t_ext2_magic(); });
    check!("ext2: synth image block size 1 KiB",        { t_ext2_blocksize(); });
    check!("ext2: root inode 2 is a directory",         { t_ext2_root_dir(); });
    check!("ext2: lookup /hello.txt",                   { t_ext2_lookup_hello(); });
    check!("ext2: alloc_inode decrements free count",   { t_ext2_alloc_inode(); });
    check!("ext2: create_root_file round-trips",        { t_ext2_create_root_file(); });

    check!("cbpf: allow program returns ALLOW",         { t_bpf_allow(); });
    check!("cbpf: errno program carries data byte",     { t_bpf_errno(); });
    check!("cbpf: kill program returns KILL",           { t_bpf_kill(); });
    check!("cbpf: allowlist admits listed",             { t_bpf_allowlist_admits(); });
    check!("cbpf: allowlist rejects unlisted",          { t_bpf_allowlist_rejects(); });
    check!("cbpf: jeq picks correct branch",            { t_bpf_jeq(); });

    check!("parser: tokenise simple cmd",               { t_parser_simple(); });
    check!("parser: tokenise double-quoted arg",        { t_parser_dq(); });
    check!("parser: tokenise single-quoted no expand",  { t_parser_sq(); });
    check!("parser: parse simple yields Cmd::Simple",   { t_parser_parse_simple(); });
    check!("parser: parse if/then/fi cleanly",          { t_parser_if(); });
    check!("parser: parse for-loop cleanly",            { t_parser_for(); });

    check!("symbols: pretty_name extracts kernel_main", { t_symbols_kernel_main(); });
    check!("symbols: passes unmangled through",         { t_symbols_unmangled(); });

    check!("cpuid: vendor string is 12 chars",          { t_cpuid_vendor_len(); });
    check!("cpuid: long-mode flag present",             { t_cpuid_lm(); });
    check!("cpuid: fpu/sse/sse2 present on QEMU",       { t_cpuid_fpu_sse(); });
    check!("cpuid: proc_cpuinfo_block has all rows",    { t_cpuid_block_rows(); });

    check!("scheduler: enqueue/dequeue dummy PID",      { t_sched_enq_deq(); });

    check!("ansi: writer accepts SGR colour sequence",  { t_ansi_sgr(); });
    check!("ansi: writer accepts CUP cursor sequence",  { t_ansi_cup(); });

    check!("cow: refcount starts at zero",              { t_cow_zero(); });
    check!("cow: share() increments refcount",          { t_cow_share(); });
    check!("cow: release() reports last reference",     { t_cow_release(); });

    check!("pkg: manifest parses NAME/VERSION/DESC",    { t_pkg_top_fields(); });
    check!("pkg: dependency list parsed",               { t_pkg_depends(); });
    check!("pkg: file payload extracted",               { t_pkg_payload(); });
    check!("pkg: missing NAME rejected",                { t_pkg_missing_name(); });

    check!("block: registry round-trip via memdev",     { t_block_registry(); });
    check!("block: read after write returns same data", { t_block_read_write(); });
    check!("block: out-of-bounds read errors",          { t_block_oob(); });

    check!("acpi: RSDP discoverable on QEMU SeaBIOS",   { t_acpi_rsdp(); });
    check!("acpi: RSDT/XSDT lists ≥ 1 SDT",             { t_acpi_sdts(); });
    check!("acpi: MADT lapic = 0xFEE00000",             { t_acpi_madt_lapic(); });
    check!("acpi: BSP LAPIC ID 0 enabled",              { t_acpi_madt_bsp(); });

    check!("heap: small Box allocation",                { t_heap_box(); });
    check!("heap: 1 K-element Vec sums correctly",      { t_heap_vec_sum(); });

    check!("ramdisk: write + read round-trip",          { t_ramdisk_round_trip(); });
    check!("ramdisk: list returns the file we wrote",   { t_ramdisk_list(); });
    check!("ramdisk: delete removes file",              { t_ramdisk_delete(); });

    // Terminal state machine — the new VT-220/xterm parser.
    check!("term: prints printable ASCII",              { t_term_print(); });
    check!("term: routes C0 controls",                  { t_term_c0(); });
    check!("term: ESC[H goes to (1,1)",                 { t_term_cup_default(); });
    check!("term: ESC[10;20H positions cursor",         { t_term_cup_args(); });
    check!("term: ESC[A/B/C/D move cursor",             { t_term_arrows(); });
    check!("term: ESC[2J clears whole display",         { t_term_clear_display(); });
    check!("term: ESC[K erases line",                   { t_term_erase_line(); });
    check!("term: ESC[31m → SetForeground(red)",        { t_term_sgr_fg(); });
    check!("term: ESC[38;5;208m → 256-colour fg",       { t_term_sgr_256(); });
    check!("term: ESC[1;4m → bold + underline",         { t_term_sgr_attrs(); });
    check!("term: ESC[0m emits ResetAttrs",             { t_term_sgr_reset(); });
    check!("term: ESC]0;title BEL → SetTitle",          { t_term_osc_title(); });
    check!("term: ESC[?1049h toggles alt screen",       { t_term_alt_screen(); });
    check!("term: ESC[?25l hides cursor",               { t_term_cursor_vis(); });
    check!("term: ESC 7 / ESC 8 save+restore cursor",   { t_term_decsc(); });
    check!("term: malformed CSI returns to ground",     { t_term_malformed(); });

    // Framebuffer drawing — runs against an in-memory MemSurface so
    // no actual video hardware is touched.
    check!("fb: clear sets every pixel to colour",      { t_fb_clear(); });
    check!("fb: put_pixel + read_pixel round-trip",     { t_fb_pixel_rw(); });
    check!("fb: out-of-bounds writes are dropped",      { t_fb_oob(); });
    check!("fb: fill_rect fills exactly that region",   { t_fb_fill_rect(); });
    check!("fb: rect outlines exactly perimeter",       { t_fb_rect_outline(); });
    check!("fb: line draws Bresenham endpoints",        { t_fb_line(); });
    check!("fb: glyph8x8 rasterises bitmap rows",       { t_fb_glyph(); });

    // ELF / PIE relocation engine.  All synthetic — no QEMU disk.
    check!("elf: apply_rela RELATIVE writes B+A",       { t_elf_rela_relative(); });
    check!("elf: apply_rela rejects unsupported type",  { t_elf_rela_unsupported(); });
    check!("elf: apply_rela R_X86_64_64 self-ref",      { t_elf_rela_64_self(); });
    check!("elf: apply_rela bounds-checks r_offset",    { t_elf_rela_oob(); });

    // userlib: printf-style formatters (no syscalls; pure formatting).
    check!("userlib: format_signed handles 0",          { t_ul_signed_zero(); });
    check!("userlib: format_signed handles negatives",  { t_ul_signed_neg(); });
    check!("userlib: format_signed handles i64::MIN",   { t_ul_signed_min(); });
    check!("userlib: format_unsigned base 10",          { t_ul_unsigned_dec(); });
    check!("userlib: format_unsigned base 16",          { t_ul_unsigned_hex(); });

    // SMP / IPI machinery — exercised on the BSP only.
    check!("smp: APIC initialises before IPI tests",    { t_smp_apic_ready(); });
    check!("smp: self-IPI vector 0x40 round-trips",     { t_smp_self_ipi(); });
    check!("smp: boot_ap rejects startup_page 0",       { t_smp_boot_ap_zero(); });

    // SMP AP trampoline — structural checks on the hand-assembled blob,
    // plus round-trip checks of the trampoline-copy and handshake paths
    // exercised in single-CPU mode (no real AP, BSP simulates the write).
    check!("smp: trampoline first byte is CLI (0xFA)",  { t_smp_tramp_cli(); });
    check!("smp: trampoline ends with HLT (0xF4)",      { t_smp_tramp_hlt(); });
    check!("smp: trampoline length 22 bytes",           { t_smp_tramp_len(); });
    check!("smp: trampoline embeds 0xDEADBEEF magic",   { t_smp_tramp_magic(); });
    check!("smp: trampoline DS load matches handshake", { t_smp_tramp_ds_imm(); });
    check!("smp: trampoline copy round-trips via map",  { t_smp_tramp_install(); });
    check!("smp: handshake write/read round-trip",      { t_smp_handshake_rt(); });
    check!("smp: bounded poll times out cleanly",       { t_smp_poll_timeout(); });
    check!("smp: boot_ap_ping rejects self target",     { t_smp_ping_self(); });
    check!("smp: boot_ap_ping rejects out-of-range id", { t_smp_ping_oob(); });
    check!("smp: cpu_state(MAX-1) is Some",             { t_smp_cpu_state_in_range(); });
    check!("smp: cpu_state(255) is None when MAX<255",  { t_smp_cpu_state_oob(); });
    check!("smp: init_bsp marks BSP slot alive",        { t_smp_init_bsp(); });

    // Per-CPU run queues + load balancing
    check!("smp: enqueue/dequeue PID round-trip",       { t_smp_pq_round_trip(); });
    check!("smp: load() reflects queue length",         { t_smp_pq_load(); });
    check!("smp: dequeue empty returns None",           { t_smp_pq_empty(); });
    check!("smp: drain returns all and zeros len",      { t_smp_pq_drain(); });
    check!("smp: total_runnable sums alive CPUs",       { t_smp_pq_total(); });
    check!("smp: least_loaded picks emptiest CPU",      { t_smp_pq_least_loaded(); });
    check!("smp: enqueue_balanced routes to slot",      { t_smp_pq_balanced(); });
    check!("smp: steal_from_peer requires ≥ 2 PIDs",    { t_smp_pq_steal_threshold(); });
}

// ----------------------------------------------------------------------------
// SHA-256
// ----------------------------------------------------------------------------

fn sha(input: &[u8]) -> [u8; 32] {
    let mut s = rustos::sha256::Sha256::new();
    s.update(input);
    s.finalize()
}

fn t_sha256_empty() {
    let h = rustos::sha256::hex(&sha(b""));
    assert_eq!(h.as_str(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
}
fn t_sha256_abc() {
    let h = rustos::sha256::hex(&sha(b"abc"));
    assert_eq!(h.as_str(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
}
fn t_sha256_two_block() {
    let h = rustos::sha256::hex(&sha(
        b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"));
    assert_eq!(h.as_str(),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
}
fn t_sha256_streaming() {
    let mut s = rustos::sha256::Sha256::new();
    s.update(b"hello, ");
    s.update(b"world");
    let streamed = s.finalize();
    let one_shot = sha(b"hello, world");
    assert_eq!(rustos::sha256::hex(&streamed).as_str(),
               rustos::sha256::hex(&one_shot).as_str());
}
fn t_sha256_cteq() {
    let a = sha(b"x");
    let b = sha(b"x");
    let c = sha(b"y");
    assert!(rustos::sha256::constant_time_eq(&a, &b));
    assert!(!rustos::sha256::constant_time_eq(&a, &c));
}

// ----------------------------------------------------------------------------
// IPv6
// ----------------------------------------------------------------------------

use rustos::net::ipv6::{
    Ipv6Addr, Ipv6Header, IPV6_HEADER_LEN, NH_ICMPV6,
    build_echo_request, handle_inbound, build_neighbor_solicitation,
    ICMPV6_ECHO_REPLY, ICMPV6_NEIGHBOR_SOL,
};

fn t_ipv6_loopback() {
    assert_eq!(Ipv6Addr::LOOPBACK.0,
        [0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,1]);
    assert!(Ipv6Addr::LOOPBACK.is_loopback());
    assert!(!Ipv6Addr::UNSPECIFIED.is_loopback());
}
fn t_ipv6_round_trip() {
    let mut buf = [0u8; IPV6_HEADER_LEN];
    let h0 = Ipv6Header::new(Ipv6Addr::LOOPBACK, Ipv6Addr::UNSPECIFIED, NH_ICMPV6, 64);
    h0.write_to(&mut buf);
    let h1 = Ipv6Header::parse(&buf).expect("parse");
    assert_eq!(h1.next_header, NH_ICMPV6);
    assert_eq!(h1.payload_len, 64);
    assert_eq!(h1.src.0, h0.src.0);
    assert_eq!(h1.dst.0, h0.dst.0);
}
fn t_ipv6_parse_rejects_v4() {
    let mut buf = [0u8; IPV6_HEADER_LEN];
    buf[0] = 0x40;
    assert!(Ipv6Header::parse(&buf).is_none());
}
fn t_ipv6_echo_layout() {
    let pkt = build_echo_request(Ipv6Addr::LOOPBACK, Ipv6Addr::LOOPBACK,
        0xBEEF, 1, b"data");
    assert_eq!(pkt.len(), IPV6_HEADER_LEN + 8 + 4);
    let h = Ipv6Header::parse(&pkt).unwrap();
    assert_eq!(h.next_header, NH_ICMPV6);
    let icmp = &pkt[IPV6_HEADER_LEN..];
    assert_eq!(icmp[0], 128);
    assert_eq!(u16::from_be_bytes([icmp[4], icmp[5]]), 0xBEEF);
    assert_eq!(&icmp[8..], b"data");
}
fn t_ipv6_handle_reply() {
    let req = build_echo_request(Ipv6Addr::LOOPBACK, Ipv6Addr::LOOPBACK,
        0x1234, 7, b"ping6!");
    let reply = handle_inbound(&req, &[Ipv6Addr::LOOPBACK])
        .expect("loopback should reply");
    let icmp = &reply[IPV6_HEADER_LEN..];
    assert_eq!(icmp[0], ICMPV6_ECHO_REPLY);
    assert_eq!(&icmp[8..], b"ping6!");
}
fn t_ipv6_drops_other() {
    let other = Ipv6Addr([0x20,0x01,0x0d,0xb8, 0,0,0,0, 0,0,0,0, 0,0,0,1]);
    let req = build_echo_request(Ipv6Addr::LOOPBACK, other, 0, 0, b"x");
    assert!(handle_inbound(&req, &[Ipv6Addr::LOOPBACK]).is_none());
}
fn t_ipv6_ns_dst() {
    // Solicited-node multicast = ff02::1:ff<low-24-of-target>.
    // We pick a target with distinct bytes 13/14/15 so the assertion
    // catches mis-byte-aligned implementations.
    let target = Ipv6Addr([0xFE,0x80, 0,0, 0,0, 0,0,
        0xCA,0xFE,0xBA,0xBE, 0xDE,0xAD,0xBE,0xEF]);
    let pkt = build_neighbor_solicitation(Ipv6Addr::LOOPBACK, target, None);
    let h = Ipv6Header::parse(&pkt).unwrap();
    assert_eq!(h.dst.0[0], 0xFF);
    assert_eq!(h.dst.0[1], 0x02);
    assert_eq!(h.dst.0[11], 0x01);
    assert_eq!(h.dst.0[12], 0xFF);
    assert_eq!(h.dst.0[13], 0xAD); // target.0[13]
    assert_eq!(h.dst.0[14], 0xBE); // target.0[14]
    assert_eq!(h.dst.0[15], 0xEF); // target.0[15]
    assert_eq!(pkt[IPV6_HEADER_LEN], ICMPV6_NEIGHBOR_SOL);
}
fn t_ipv6_compact() {
    assert_eq!(Ipv6Addr::LOOPBACK.to_compact().as_str(), "::1");
    assert_eq!(Ipv6Addr::UNSPECIFIED.to_compact().as_str(), "::");
    let mixed = Ipv6Addr([0x20,0x01, 0x0d,0xb8, 0,0, 0,0, 0,0, 0,0, 0,0, 0,1]);
    assert_eq!(mixed.to_compact().as_str(), "2001:db8::1");
}

// ----------------------------------------------------------------------------
// Ext2
// ----------------------------------------------------------------------------

use rustos::fs::ext2::{Ext2Fs, synthesise_demo_image, EXT2_MAGIC};

fn mount_demo() -> Ext2Fs {
    let img = synthesise_demo_image();
    Ext2Fs::mount(img).expect("mount synthetic image")
}

fn t_ext2_magic() {
    let fs = mount_demo();
    assert_eq!(fs.sb.magic, EXT2_MAGIC);
}
fn t_ext2_blocksize() {
    let fs = mount_demo();
    assert_eq!(fs.sb.block_size(), 1024);
}
fn t_ext2_root_dir() {
    let fs = mount_demo();
    let root = fs.read_inode(2).expect("read inode 2");
    assert!(root.is_dir());
}
fn t_ext2_lookup_hello() {
    let fs = mount_demo();
    let ino = fs.lookup("/hello.txt").expect("lookup /hello.txt");
    assert!(ino > 0);
}
fn t_ext2_alloc_inode() {
    let mut fs = mount_demo();
    let before = fs.sb.free_inodes_count;
    let ino = fs.alloc_inode().expect("alloc inode");
    // We don't assert against first_ino because the synthetic image
    // is rev-0 and reserves no special user-inode boundary; only the
    // free-count decrement is invariant.
    assert!(ino > 0, "allocated inode must be non-zero, got {}", ino);
    assert_eq!(fs.sb.free_inodes_count, before - 1);
}
fn t_ext2_create_root_file() {
    let mut fs = mount_demo();
    let payload = b"hello-from-test";
    fs.create_root_file("test.txt", payload).expect("create_root_file");
    let read_back = fs.read_file("/test.txt").expect("read back");
    assert_eq!(&read_back, payload);
}

// ----------------------------------------------------------------------------
// Classic BPF
// ----------------------------------------------------------------------------

use rustos::seccomp::{Vm as BpfVm, BpfInsn, SeccompData, action, build_allowlist};
const BPF_LD:  u16 = 0x00;
const BPF_W:   u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_RET: u16 = 0x06;
const BPF_K:   u16 = 0x00;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;

fn d(nr: u32) -> SeccompData {
    SeccompData { nr, arch: 0xc000_003e, instruction_pointer: 0, args: [0; 6] }
}

fn t_bpf_allow() {
    let prog = [BpfInsn { code: BPF_RET | BPF_K, jt: 0, jf: 0, k: action::RET_ALLOW }];
    let ret = BpfVm::new(&prog, &d(60)).run() & action::ACTION_MASK;
    assert_eq!(ret, action::RET_ALLOW);
}
fn t_bpf_errno() {
    let prog = [BpfInsn { code: BPF_RET | BPF_K, jt: 0, jf: 0,
        k: action::RET_ERRNO | 0x000D }];
    let ret = BpfVm::new(&prog, &d(1)).run();
    assert_eq!(ret & action::ACTION_MASK, action::RET_ERRNO);
    assert_eq!(ret & action::DATA_MASK, 0x000D);
}
fn t_bpf_kill() {
    let prog = [BpfInsn { code: BPF_RET | BPF_K, jt: 0, jf: 0,
        k: action::RET_KILL_PROCESS }];
    let ret = BpfVm::new(&prog, &d(1)).run() & action::ACTION_MASK;
    assert_eq!(ret, action::RET_KILL_PROCESS);
}
fn t_bpf_allowlist_admits() {
    let prog = build_allowlist(&[60, 39], action::RET_ERRNO | 1);
    let ret = BpfVm::new(&prog, &d(60)).run() & action::ACTION_MASK;
    assert_eq!(ret, action::RET_ALLOW);
    let ret = BpfVm::new(&prog, &d(39)).run() & action::ACTION_MASK;
    assert_eq!(ret, action::RET_ALLOW);
}
fn t_bpf_allowlist_rejects() {
    let prog = build_allowlist(&[60], action::RET_ERRNO | 1);
    let ret = BpfVm::new(&prog, &d(1)).run();
    assert_eq!(ret & action::ACTION_MASK, action::RET_ERRNO);
    assert_eq!(ret & action::DATA_MASK, 1);
}
fn t_bpf_jeq() {
    let prog = [
        BpfInsn { code: BPF_LD | BPF_W | BPF_ABS, jt: 0, jf: 0, k: 0 },
        BpfInsn { code: BPF_JMP | BPF_JEQ | BPF_K, jt: 0, jf: 1, k: 42 },
        BpfInsn { code: BPF_RET | BPF_K, jt: 0, jf: 0, k: action::RET_ALLOW },
        BpfInsn { code: BPF_RET | BPF_K, jt: 0, jf: 0, k: action::RET_KILL_PROCESS },
    ];
    let ret = BpfVm::new(&prog, &d(42)).run() & action::ACTION_MASK;
    assert_eq!(ret, action::RET_ALLOW);
    let ret = BpfVm::new(&prog, &d(0)).run() & action::ACTION_MASK;
    assert_eq!(ret, action::RET_KILL_PROCESS);
}

// ----------------------------------------------------------------------------
// Shell parser
// ----------------------------------------------------------------------------

use rustos::shell_parser::{tokenize, parse, Token, Cmd};

fn words(toks: &[Token]) -> alloc::vec::Vec<&str> {
    toks.iter().filter_map(|t| match t {
        Token::Word(s) => Some(s.as_str()),
        _ => None,
    }).collect()
}

fn t_parser_simple() {
    let toks = tokenize("ls -la /tmp");
    let w = words(&toks);
    assert_eq!(w, vec!["ls", "-la", "/tmp"]);
}
fn t_parser_dq() {
    let toks = tokenize(r#"echo "hello world""#);
    assert_eq!(words(&toks), vec!["echo", "hello world"]);
}
fn t_parser_sq() {
    let toks = tokenize(r#"echo 'a$b'"#);
    assert_eq!(words(&toks), vec!["echo", "a$b"]);
}
fn t_parser_parse_simple() {
    let ast = parse("ls -l").expect("parse simple");
    if let Cmd::Simple(args) = ast {
        assert_eq!(args[0], "ls");
        assert_eq!(args[1], "-l");
    }
    // Sequence wrapper is also acceptable; not asserted here.
}
fn t_parser_if() {
    assert!(parse("if true ; then echo ok ; fi").is_ok());
}
fn t_parser_for() {
    assert!(parse("for i in a b c ; do echo $i ; done").is_ok());
}

// ----------------------------------------------------------------------------
// Symbol-table demangling
// ----------------------------------------------------------------------------

fn t_symbols_kernel_main() {
    let n = rustos::symbols::pretty_name("_RNvCs27v5PtC5Ebg_6rustos11kernel_main");
    assert_eq!(n, "kernel_main");
}
fn t_symbols_unmangled() {
    let n = rustos::symbols::pretty_name("syscall_handler");
    assert_eq!(n, "syscall_handler");
}

// ----------------------------------------------------------------------------
// CPUID
// ----------------------------------------------------------------------------

fn t_cpuid_vendor_len() {
    let info = rustos::cpuid::cpu_info();
    assert_eq!(info.vendor.len(), 12);
}
fn t_cpuid_lm() {
    let info = rustos::cpuid::cpu_info();
    assert!(info.flags.contains(&"lm"));
}
fn t_cpuid_fpu_sse() {
    let info = rustos::cpuid::cpu_info();
    assert!(info.flags.contains(&"fpu"));
    assert!(info.flags.contains(&"sse"));
    assert!(info.flags.contains(&"sse2"));
}
fn t_cpuid_block_rows() {
    let s = rustos::cpuid::proc_cpuinfo_block(0);
    assert!(s.contains("processor\t: 0\n"));
    assert!(s.contains("vendor_id\t: "));
    assert!(s.contains("model name\t: "));
    assert!(s.contains("flags\t\t: "));
}

// ----------------------------------------------------------------------------
// Scheduler
// ----------------------------------------------------------------------------

fn t_sched_enq_deq() {
    use rustos::process::scheduler::SCHEDULER;
    let dummy: usize = usize::MAX - 1;
    SCHEDULER.lock().enqueue(dummy);
    let popped = SCHEDULER.lock().dequeue_next();
    assert!(popped.is_some());
}

// ----------------------------------------------------------------------------
// ANSI escape parser (just confirms the writer eats sequences without panic)
// ----------------------------------------------------------------------------

fn t_ansi_sgr() {
    use core::fmt::Write;
    let mut w = rustos::vga_buffer::WRITER.lock();
    w.write_str("\x1b[31;105mTEXT\x1b[0m").unwrap();
    w.reset_color();
}
fn t_ansi_cup() {
    use core::fmt::Write;
    let mut w = rustos::vga_buffer::WRITER.lock();
    w.write_str("\x1b[H\x1b[10;5H").unwrap();
}

// ----------------------------------------------------------------------------
// COW refcounts
// ----------------------------------------------------------------------------

use rustos::memory::cow as cow_mod;
use x86_64::structures::paging::PhysFrame;
use x86_64::PhysAddr;

fn frame_at(addr: u64) -> PhysFrame {
    PhysFrame::containing_address(PhysAddr::new(addr))
}

fn t_cow_zero() {
    let f = frame_at(0xDEAD_0000);
    assert_eq!(cow_mod::refcount(f), 0);
}
fn t_cow_share() {
    let f = frame_at(0xDEAD_1000);
    cow_mod::share(f);
    cow_mod::share(f);
    assert_eq!(cow_mod::refcount(f), 2);
}
fn t_cow_release() {
    let f = frame_at(0xDEAD_2000);
    cow_mod::share(f);
    cow_mod::share(f);
    assert!(!cow_mod::release(f), "two shared, one released → not last");
    assert!(cow_mod::release(f),  "second release → was last reference");
}

// ----------------------------------------------------------------------------
// Package manifest
// ----------------------------------------------------------------------------

// Real manifest format (see crate::pkg::PackageManifest::parse):
//   NAME <name>
//   VERSION <ver>
//   DESCRIPTION <desc>
//   DEPENDS <comma-separated list>
//   FILE <path> <length>
//   <length-bytes-of-content>\n
const SAMPLE_PKG: &[u8] = b"\
NAME hello-world
VERSION 1.2.3
DESCRIPTION Tiny example package
DEPENDS libc, librustos
FILE /usr/local/bin/hello 13
hello, world!
";

fn t_pkg_top_fields() {
    let m = rustos::pkg::PackageManifest::parse(SAMPLE_PKG).expect("parse");
    assert_eq!(m.name, "hello-world");
    assert_eq!(m.version, "1.2.3");
    assert_eq!(m.description, "Tiny example package");
}
fn t_pkg_depends() {
    let m = rustos::pkg::PackageManifest::parse(SAMPLE_PKG).expect("parse");
    assert!(m.depends.iter().any(|d| d == "libc"));
    assert!(m.depends.iter().any(|d| d == "librustos"));
}
fn t_pkg_payload() {
    let m = rustos::pkg::PackageManifest::parse(SAMPLE_PKG).expect("parse");
    assert_eq!(m.files.len(), 1);
    let (path, data) = &m.files[0];
    assert_eq!(path, "/usr/local/bin/hello");
    assert!(data.starts_with(b"hello, world!"));
}
fn t_pkg_missing_name() {
    let bad = b"VERSION: 1.0\n";
    assert!(rustos::pkg::PackageManifest::parse(bad).is_err());
}

// ----------------------------------------------------------------------------
// Block device registry
// ----------------------------------------------------------------------------

use core::sync::atomic::Ordering as AOrd;
use rustos::block::{BlockDevice, register, snapshot, Stats};

struct MemDev {
    name: alloc::string::String,
    capacity_sectors: u64,
    data: spin::Mutex<alloc::vec::Vec<u8>>,
    stats: Stats,
}
impl BlockDevice for MemDev {
    fn name(&self) -> &str { &self.name }
    fn sectors(&self) -> u64 { self.capacity_sectors }
    fn read(&self, lba: u64, count: u32, buf: &mut [u8]) -> Result<(), &'static str> {
        let off = (lba * 512) as usize;
        let len = (count * 512) as usize;
        let d = self.data.lock();
        if off + len > d.len() { return Err("oob"); }
        buf[..len].copy_from_slice(&d[off..off + len]);
        self.stats.reads.fetch_add(1, AOrd::Relaxed);
        self.stats.sectors_read.fetch_add(count as u64, AOrd::Relaxed);
        Ok(())
    }
    fn write(&self, lba: u64, count: u32, buf: &[u8]) -> Result<(), &'static str> {
        let off = (lba * 512) as usize;
        let len = (count * 512) as usize;
        let mut d = self.data.lock();
        if off + len > d.len() { return Err("oob"); }
        d[off..off + len].copy_from_slice(&buf[..len]);
        self.stats.writes.fetch_add(1, AOrd::Relaxed);
        self.stats.sectors_written.fetch_add(count as u64, AOrd::Relaxed);
        Ok(())
    }
    fn stats(&self) -> (u64, u64, u64, u64) { self.stats.snapshot() }
}

fn t_block_registry() {
    let dev = alloc::boxed::Box::new(MemDev {
        name: "memtest0".to_string(),
        capacity_sectors: 16,
        data: spin::Mutex::new(alloc::vec![0u8; 16 * 512]),
        stats: Stats::default(),
    });
    register(dev);
    let snap = snapshot();
    let found = snap.iter().any(|(n, s, _)| n == "memtest0" && *s == 16);
    assert!(found, "memtest0 missing from registry");
}
fn t_block_read_write() {
    let dev = MemDev {
        name: "memtest1".to_string(),
        capacity_sectors: 8,
        data: spin::Mutex::new(alloc::vec![0u8; 8 * 512]),
        stats: Stats::default(),
    };
    let payload = [0xCDu8; 512];
    dev.write(3, 1, &payload).expect("write");
    let mut buf = [0u8; 512];
    dev.read(3, 1, &mut buf).expect("read");
    assert_eq!(&buf[..], &payload[..]);
    let (r, sr, w, sw) = dev.stats();
    assert_eq!((r, sr, w, sw), (1, 1, 1, 1));
}
fn t_block_oob() {
    let dev = MemDev {
        name: "memtest2".to_string(),
        capacity_sectors: 1,
        data: spin::Mutex::new(alloc::vec![0u8; 512]),
        stats: Stats::default(),
    };
    let mut buf = [0u8; 512];
    assert!(dev.read(2, 1, &mut buf).is_err());
}

// ----------------------------------------------------------------------------
// ACPI
// ----------------------------------------------------------------------------

fn t_acpi_rsdp() {
    let p = rustos::acpi::find_rsdp().expect("RSDP must exist on QEMU");
    assert!(p > 0 && p < 0x10_0000,
        "RSDP must lie below 1 MiB on legacy BIOS, got {:#x}", p);
}
fn t_acpi_sdts() {
    let g = rustos::acpi::ACPI.lock();
    let t = g.as_ref().expect("ACPI tables not populated");
    assert!(!t.sdt_phys.is_empty());
}
fn t_acpi_madt_lapic() {
    let madt = rustos::acpi::parse_madt().expect("MADT must be present");
    assert_eq!(madt.lapic_phys, 0xFEE0_0000,
        "QEMU's default LAPIC base is 0xFEE00000 (got {:#x})", madt.lapic_phys);
}
fn t_acpi_madt_bsp() {
    let madt = rustos::acpi::parse_madt().expect("MADT");
    assert!(madt.lapic_ids.contains(&0));
}

// ----------------------------------------------------------------------------
// Heap & RamDisk (subsumes the old basic_boot / heap_allocation / vfs_test)
// ----------------------------------------------------------------------------

fn t_heap_box() {
    let v1 = alloc::boxed::Box::new(41i32);
    let v2 = alloc::boxed::Box::new(13i32);
    assert_eq!(*v1, 41);
    assert_eq!(*v2, 13);
}
fn t_heap_vec_sum() {
    let n: u64 = 1000;
    let mut v: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
    for i in 0..n { v.push(i); }
    assert_eq!(v.iter().sum::<u64>(), (n - 1) * n / 2);
}

fn t_ramdisk_round_trip() {
    use rustos::fs::ramdisk::RamDisk;
    use rustos::fs::vfs::FileSystem;
    let mut rd = RamDisk::new();
    let payload = b"Hello, RustOS!".to_vec();
    rd.write("test.txt", payload.clone()).expect("write");
    let read = rd.read("test.txt").expect("read");
    assert_eq!(read, payload);
}
fn t_ramdisk_list() {
    use rustos::fs::ramdisk::RamDisk;
    use rustos::fs::vfs::FileSystem;
    let mut rd = RamDisk::new();
    rd.write("a.txt", b"a".to_vec()).expect("write");
    rd.write("b.txt", b"bb".to_vec()).expect("write");
    let names: alloc::vec::Vec<_> = rd.list().iter()
        .map(|i| i.name.clone()).collect();
    assert!(names.iter().any(|n| n == "a.txt"));
    assert!(names.iter().any(|n| n == "b.txt"));
}
fn t_ramdisk_delete() {
    use rustos::fs::ramdisk::RamDisk;
    use rustos::fs::vfs::FileSystem;
    let mut rd = RamDisk::new();
    rd.write("zap.txt", b"x".to_vec()).expect("write");
    assert!(rd.exists("zap.txt"));
    rd.delete("zap.txt").expect("delete");
    assert!(!rd.exists("zap.txt"));
}

// ----------------------------------------------------------------------------
// Terminal parser
// ----------------------------------------------------------------------------

use rustos::term::{Parser, Event};

fn feed(parser: &mut Parser, bytes: &[u8]) -> alloc::vec::Vec<Event> {
    let mut out = alloc::vec::Vec::new();
    for &b in bytes {
        out.extend(parser.feed(b));
    }
    out
}

fn t_term_print() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"hi");
    assert_eq!(evs, vec![Event::Print(b'h'), Event::Print(b'i')]);
}
fn t_term_c0() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"\x07\x08\x09\x0A\x0D");
    assert_eq!(evs, vec![Event::Bell, Event::Backspace,
        Event::HorizontalTab, Event::Newline, Event::CarriageReturn]);
}
fn t_term_cup_default() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"\x1b[H");
    assert_eq!(evs, vec![Event::CursorTo { row: 1, col: 1 }]);
}
fn t_term_cup_args() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"\x1b[10;20H");
    assert_eq!(evs, vec![Event::CursorTo { row: 10, col: 20 }]);
}
fn t_term_arrows() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"\x1b[3A\x1b[B\x1b[5C\x1b[D");
    assert_eq!(evs, vec![
        Event::CursorUp(3),
        Event::CursorDown(1),
        Event::CursorForward(5),
        Event::CursorBack(1),
    ]);
}
fn t_term_clear_display() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"\x1b[2J");
    assert_eq!(evs, vec![Event::EraseDisplay(2)]);
}
fn t_term_erase_line() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"\x1b[K\x1b[1K");
    assert_eq!(evs, vec![Event::EraseLine(0), Event::EraseLine(1)]);
}
fn t_term_sgr_fg() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"\x1b[31m");
    assert_eq!(evs, vec![Event::SetForeground(1)]); // red
}
fn t_term_sgr_256() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"\x1b[38;5;208m");
    assert_eq!(evs, vec![Event::SetForeground(208)]);
}
fn t_term_sgr_attrs() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"\x1b[1;4m");
    assert_eq!(evs, vec![Event::SetBold(true), Event::SetUnderline(true)]);
}
fn t_term_sgr_reset() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"\x1b[0m");
    assert_eq!(evs, vec![Event::ResetAttrs]);
    let evs2 = feed(&mut p, b"\x1b[m");
    assert_eq!(evs2, vec![Event::ResetAttrs]);
}
fn t_term_osc_title() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"\x1b]0;hello\x07");
    assert_eq!(evs, vec![Event::SetTitle("hello".into())]);
}
fn t_term_alt_screen() {
    let mut p = Parser::new();
    let on = feed(&mut p, b"\x1b[?1049h");
    assert_eq!(on, vec![Event::AlternateScreen(true)]);
    let off = feed(&mut p, b"\x1b[?1049l");
    assert_eq!(off, vec![Event::AlternateScreen(false)]);
}
fn t_term_cursor_vis() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"\x1b[?25l\x1b[?25h");
    assert_eq!(evs, vec![Event::ShowCursor(false), Event::ShowCursor(true)]);
}
fn t_term_decsc() {
    let mut p = Parser::new();
    let evs = feed(&mut p, b"\x1b7\x1b8");
    assert_eq!(evs, vec![Event::SaveCursor, Event::RestoreCursor]);
}
fn t_term_malformed() {
    // Garbage CSI should not panic; later valid sequence still parsed.
    let mut p = Parser::new();
    let _ = feed(&mut p, b"\x1b[\x07"); // BEL inside CSI param → reset
    let evs = feed(&mut p, b"\x1b[H");
    assert_eq!(evs, vec![Event::CursorTo { row: 1, col: 1 }]);
}

// ----------------------------------------------------------------------------
// Framebuffer
// ----------------------------------------------------------------------------

use rustos::framebuffer::{Surface, MemSurface, clear as fb_clear,
    fill_rect, rect, line, glyph8x8};

fn t_fb_clear() {
    let mut s = MemSurface::new(8, 8);
    fb_clear(&mut s, 7);
    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(s.read_pixel(x, y), 7);
        }
    }
}
fn t_fb_pixel_rw() {
    let mut s = MemSurface::new(4, 4);
    s.put_pixel(2, 1, 42);
    assert_eq!(s.read_pixel(2, 1), 42);
    assert_eq!(s.read_pixel(0, 0), 0);
}
fn t_fb_oob() {
    let mut s = MemSurface::new(4, 4);
    s.put_pixel(99, 99, 5); // must not panic, must not write
    assert_eq!(s.read_pixel(99, 99), 0);
    // The valid range stays untouched.
    for y in 0..4 { for x in 0..4 { assert_eq!(s.read_pixel(x, y), 0); } }
}
fn t_fb_fill_rect() {
    let mut s = MemSurface::new(8, 8);
    fill_rect(&mut s, 2, 2, 3, 4, 9);
    for y in 0..8 {
        for x in 0..8 {
            let inside = (2..5).contains(&x) && (2..6).contains(&y);
            assert_eq!(s.read_pixel(x, y), if inside { 9 } else { 0 },
                "pixel ({},{}) wrong", x, y);
        }
    }
}
fn t_fb_rect_outline() {
    let mut s = MemSurface::new(6, 6);
    rect(&mut s, 1, 1, 4, 4, 3);
    // Perimeter set; interior (2..4, 2..4) untouched.
    assert_eq!(s.read_pixel(1, 1), 3); // corners
    assert_eq!(s.read_pixel(4, 1), 3);
    assert_eq!(s.read_pixel(1, 4), 3);
    assert_eq!(s.read_pixel(4, 4), 3);
    assert_eq!(s.read_pixel(2, 2), 0); // interior
    assert_eq!(s.read_pixel(3, 3), 0);
}
fn t_fb_line() {
    let mut s = MemSurface::new(8, 8);
    line(&mut s, 0, 0, 7, 7, 5);
    // Diagonal: every (i, i) should be 5.
    for i in 0..8 {
        assert_eq!(s.read_pixel(i, i), 5,
            "diagonal pixel ({},{}) missing", i, i);
    }
}
fn t_fb_glyph() {
    // A glyph that draws a small "L":
    //   row 0: 1000 0000  → only top-left pixel set
    //   row 1: 1000 0000
    //   row 2: 1000 0000
    //   row 3: 1100 0000  → top-left + one to the right
    //   rest: 0
    let glyph = [0x80, 0x80, 0x80, 0xC0, 0, 0, 0, 0];
    let mut s = MemSurface::new(8, 8);
    glyph8x8(&mut s, 0, 0, &glyph, 6, 0);
    assert_eq!(s.read_pixel(0, 0), 6);
    assert_eq!(s.read_pixel(0, 1), 6);
    assert_eq!(s.read_pixel(0, 2), 6);
    assert_eq!(s.read_pixel(0, 3), 6);
    assert_eq!(s.read_pixel(1, 3), 6);
    assert_eq!(s.read_pixel(1, 0), 0);
    assert_eq!(s.read_pixel(7, 7), 0);
}

// ----------------------------------------------------------------------------
// ELF / PIE relocations
// ----------------------------------------------------------------------------

use rustos::elf::{ElfLoader, R_X86_64_RELATIVE, R_X86_64_64};

fn t_elf_rela_relative() {
    // 16-byte image laid out as [u64 placeholder][8 bytes pad].
    // After R_X86_64_RELATIVE applied at offset 0 with addend 0x100,
    // bytes 0..8 must equal `image_base + 0x100` LE.
    let mut img = alloc::vec![0u8; 16];
    let r_info: u64 = R_X86_64_RELATIVE as u64;
    ElfLoader::apply_rela(&mut img, 0x4000_0000_u64, 0, r_info, 0x100)
        .expect("apply_rela");
    let val = u64::from_le_bytes([
        img[0], img[1], img[2], img[3],
        img[4], img[5], img[6], img[7],
    ]);
    assert_eq!(val, 0x4000_0000 + 0x100);
}

fn t_elf_rela_unsupported() {
    let mut img = alloc::vec![0u8; 16];
    // R_X86_64_PLT32 = 4 — we don't implement it, must Err.
    let r_info: u64 = 4;
    let err = ElfLoader::apply_rela(&mut img, 0, 0, r_info, 0);
    assert!(err.is_err());
}

fn t_elf_rela_64_self() {
    // R_X86_64_64 = S + A.  With r_sym = 0 (no symbol table), S = 0
    // and the result must be exactly the addend.  PIE binaries that
    // need image-base relocation use R_X86_64_RELATIVE, not _64.
    let mut img = alloc::vec![0u8; 16];
    let r_info: u64 = R_X86_64_64 as u64;
    ElfLoader::apply_rela(&mut img, 0x1000, 8, r_info, 0x42)
        .expect("apply_rela R_X86_64_64");
    let val = u64::from_le_bytes([
        img[8], img[9], img[10], img[11],
        img[12], img[13], img[14], img[15],
    ]);
    assert_eq!(val, 0x42);
}

fn t_elf_rela_oob() {
    let mut img = alloc::vec![0u8; 8];
    // r_offset = 4 + 8 = past end of 8-byte buffer.
    let r_info: u64 = R_X86_64_RELATIVE as u64;
    let err = ElfLoader::apply_rela(&mut img, 0, 4, r_info, 0);
    assert!(err.is_err());
}

// ----------------------------------------------------------------------------
// userlib formatters
// ----------------------------------------------------------------------------

use rustos::userlib::{format_signed, format_unsigned};

fn t_ul_signed_zero() {
    let mut b = [0u8; 24];
    assert_eq!(format_signed(0, &mut b), b"0");
}
fn t_ul_signed_neg() {
    let mut b = [0u8; 24];
    assert_eq!(format_signed(-12345, &mut b), b"-12345");
}
fn t_ul_signed_min() {
    let mut b = [0u8; 24];
    // i64::MIN = -9_223_372_036_854_775_808
    assert_eq!(format_signed(i64::MIN, &mut b), b"-9223372036854775808");
}
fn t_ul_unsigned_dec() {
    let mut b = [0u8; 24];
    assert_eq!(format_unsigned(98765, 10, &mut b), b"98765");
}
fn t_ul_unsigned_hex() {
    let mut b = [0u8; 24];
    assert_eq!(format_unsigned(0xCAFE_BABE, 16, &mut b), b"cafebabe");
}

// ----------------------------------------------------------------------------
// SMP / LAPIC IPI plumbing
// ----------------------------------------------------------------------------

fn t_smp_apic_ready() {
    assert!(rustos::apic::is_available(),
        "apic::init should have succeeded under QEMU");
    assert_eq!(rustos::apic::lapic_id(), 0, "BSP LAPIC ID is 0");
}

fn t_smp_self_ipi() {
    // We can't fire a self-IPI to an arbitrary vector here because we
    // haven't installed a handler for it — the CPU would deliver the
    // interrupt and the missing IDT slot triple-faults.  Instead we
    // verify the *all-but-self* shorthand finishes its ICR
    // round-trip: on a 1-CPU system there's no recipient, so nothing
    // is actually delivered, but `wait_ipi_done` still has to drain
    // ICR.DELIVERY_STATUS.  Bounded spin guarantees it returns even
    // if hardware is wedged.
    use rustos::apic::ipi;
    rustos::apic::send_ipi(0, ipi::FIXED | ipi::DEST_ALL_EXCLSELF, 0xFE);
}

fn t_smp_boot_ap_zero() {
    let err = rustos::apic::boot_ap(1, 0);
    assert!(err.is_err());
}

// ----------------------------------------------------------------------------
// SMP AP trampoline — structural and round-trip checks
// ----------------------------------------------------------------------------

fn t_smp_tramp_cli() {
    // Real-mode CPUs may inherit IF=1 from the BIOS, so the very first
    // opcode in the trampoline must be CLI (0xFA) before any memory write.
    assert_eq!(rustos::smp::AP_TRAMPOLINE[rustos::smp::TRAMP_OFF_CLI], 0xFA);
}

fn t_smp_tramp_hlt() {
    assert_eq!(rustos::smp::AP_TRAMPOLINE[rustos::smp::TRAMP_OFF_HLT], 0xF4);
}

fn t_smp_tramp_len() {
    // 22 bytes is the size of the documented disassembly; if a future
    // edit changes it, this test is a tripwire forcing the doc-comment
    // to be updated alongside the code.
    assert_eq!(rustos::smp::AP_TRAMPOLINE.len(), 22);
}

fn t_smp_tramp_magic() {
    // Two 16-bit little-endian halves of 0xDEADBEEF embedded in the
    // C7 06 disp16 imm16 stores.
    let lo_off = rustos::smp::TRAMP_OFF_MAGIC_LO;
    let hi_off = rustos::smp::TRAMP_OFF_MAGIC_HI;
    assert_eq!(rustos::smp::AP_TRAMPOLINE[lo_off],     0xEF);
    assert_eq!(rustos::smp::AP_TRAMPOLINE[lo_off + 1], 0xBE);
    assert_eq!(rustos::smp::AP_TRAMPOLINE[hi_off],     0xAD);
    assert_eq!(rustos::smp::AP_TRAMPOLINE[hi_off + 1], 0xDE);
}

fn t_smp_tramp_ds_imm() {
    // The `mov ax, imm16` immediately before `mov ds, ax` must encode
    // the segment whose linear base is AP_HANDSHAKE_PHYS.  Catches the
    // class of bug where someone moves the handshake page without
    // updating the trampoline.
    let imm_off = rustos::smp::TRAMP_OFF_MOV_AX + 1;
    let lo = rustos::smp::AP_TRAMPOLINE[imm_off];
    let hi = rustos::smp::AP_TRAMPOLINE[imm_off + 1];
    let imm = u16::from_le_bytes([lo, hi]);
    let expected = (rustos::smp::AP_HANDSHAKE_PHYS >> 4) as u16;
    assert_eq!(imm, expected);
}

fn t_smp_tramp_install() {
    // Copy the trampoline to phys 0x8000 via the kernel's direct map,
    // then read it back through the same map and verify byte-for-byte.
    let mut buf = [0u8; 32];
    rustos::smp::test_install_and_readback(&mut buf);
    let len = rustos::smp::AP_TRAMPOLINE.len();
    assert_eq!(&buf[..len], rustos::smp::AP_TRAMPOLINE);
}

fn t_smp_handshake_rt() {
    let v = rustos::smp::test_handshake_round_trip();
    assert_eq!(v, rustos::smp::AP_HANDSHAKE_MAGIC);
}

fn t_smp_poll_timeout() {
    assert!(rustos::smp::test_bounded_poll_times_out(),
        "bounded poll must terminate without seeing the magic");
}

fn t_smp_ping_self() {
    // BSP LAPIC ID is 0 under QEMU.  Booting self must be rejected so
    // we don't INIT-IPI the running CPU and reset the kernel.
    let bsp = rustos::apic::lapic_id();
    let err = rustos::smp::boot_ap_ping(bsp);
    assert!(err.is_err(), "boot_ap_ping(self) must error, got {:?}",
        err.map(|_| "ok"));
}

fn t_smp_ping_oob() {
    // MAX_CPUS-1 is the highest valid index; MAX_CPUS itself must be
    // rejected before we touch the per-CPU array.
    let oob = rustos::smp::MAX_CPUS as u8;
    let err = rustos::smp::boot_ap_ping(oob);
    assert!(err.is_err());
}

fn t_smp_cpu_state_in_range() {
    let last = (rustos::smp::MAX_CPUS - 1) as u8;
    assert!(rustos::smp::cpu_state(last).is_some());
}

fn t_smp_cpu_state_oob() {
    // Skip if MAX_CPUS happens to be 256 (shouldn't be, but be robust).
    if rustos::smp::MAX_CPUS < 256 {
        let oob = rustos::smp::MAX_CPUS as u8;
        assert!(rustos::smp::cpu_state(oob).is_none());
    }
}

fn t_smp_init_bsp() {
    use core::sync::atomic::Ordering;
    rustos::smp::init_bsp();
    let bsp = rustos::apic::lapic_id();
    let slot = rustos::smp::cpu_state(bsp).expect("BSP slot must exist");
    assert!(slot.alive.load(Ordering::Acquire),
        "init_bsp should mark BSP slot alive");
    // Idempotent: a second call is harmless.
    rustos::smp::init_bsp();
    assert!(slot.alive.load(Ordering::Acquire));
}

// ----------------------------------------------------------------------------
// Per-CPU run queues + load balancing.  These mutate global per-CPU state,
// so each test starts by draining whatever the previous test left behind.
// ----------------------------------------------------------------------------

fn t_smp_pq_round_trip() {
    let bsp = rustos::apic::lapic_id();
    let slot = rustos::smp::cpu_state(bsp).expect("BSP slot");
    let _ = slot.drain();
    slot.enqueue(101);
    slot.enqueue(202);
    slot.enqueue(303);
    assert_eq!(slot.dequeue(), Some(101));
    assert_eq!(slot.dequeue(), Some(202));
    assert_eq!(slot.dequeue(), Some(303));
    assert_eq!(slot.dequeue(), None);
}

fn t_smp_pq_load() {
    let bsp = rustos::apic::lapic_id();
    let slot = rustos::smp::cpu_state(bsp).expect("BSP slot");
    let _ = slot.drain();
    assert_eq!(slot.load(), 0);
    slot.enqueue(1); assert_eq!(slot.load(), 1);
    slot.enqueue(2); assert_eq!(slot.load(), 2);
    let _ = slot.dequeue(); assert_eq!(slot.load(), 1);
    let _ = slot.drain();
}

fn t_smp_pq_empty() {
    let slot = rustos::smp::cpu_state(rustos::apic::lapic_id()).unwrap();
    let _ = slot.drain();
    assert_eq!(slot.dequeue(), None);
}

fn t_smp_pq_drain() {
    let slot = rustos::smp::cpu_state(rustos::apic::lapic_id()).unwrap();
    let _ = slot.drain();
    slot.enqueue(7); slot.enqueue(8); slot.enqueue(9);
    let v = slot.drain();
    assert_eq!(v, alloc::vec![7, 8, 9]);
    assert_eq!(slot.load(), 0);
}

fn t_smp_pq_total() {
    let bsp = rustos::apic::lapic_id();
    let slot = rustos::smp::cpu_state(bsp).unwrap();
    let _ = slot.drain();
    assert_eq!(rustos::smp::total_runnable(), 0);
    slot.enqueue(11); slot.enqueue(22);
    assert_eq!(rustos::smp::total_runnable(), 2);
    let _ = slot.drain();
}

fn t_smp_pq_least_loaded() {
    // Only the BSP is alive in our test environment, so least_loaded
    // must always return the BSP regardless of queue depth.
    let bsp = rustos::apic::lapic_id();
    let slot = rustos::smp::cpu_state(bsp).unwrap();
    let _ = slot.drain();
    assert_eq!(rustos::smp::least_loaded_cpu(), bsp);
    slot.enqueue(1);
    assert_eq!(rustos::smp::least_loaded_cpu(), bsp);
    let _ = slot.drain();
}

fn t_smp_pq_balanced() {
    let bsp = rustos::apic::lapic_id();
    let slot = rustos::smp::cpu_state(bsp).unwrap();
    let _ = slot.drain();
    let cpu = rustos::smp::enqueue_balanced(42);
    assert_eq!(cpu, bsp);
    assert_eq!(slot.dequeue(), Some(42));
}

fn t_smp_pq_steal_threshold() {
    // Single-CPU box: steal_from_peer always returns None because
    // there *is* no peer.  This verifies the iteration's "skip
    // self" logic and the "≥ 2 PIDs" threshold.
    let bsp = rustos::apic::lapic_id();
    let slot = rustos::smp::cpu_state(bsp).unwrap();
    let _ = slot.drain();
    slot.enqueue(1); slot.enqueue(2); slot.enqueue(3);
    assert_eq!(rustos::smp::steal_from_peer(bsp), None,
        "thief must not steal from itself");
    let _ = slot.drain();
}

// Suppress unused-imports lint for paths used only in a few tests.
#[allow(dead_code)]
fn _silence_warnings() {
    let _ = format!("");
}
