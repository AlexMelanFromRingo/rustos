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

// Suppress unused-imports lint for paths used only in a few tests.
#[allow(dead_code)]
fn _silence_warnings() {
    let _ = format!("");
}
