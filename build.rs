//! Build-time helper: read the previously-built kernel binary and
//! emit a Rust source file with a sorted (address, size, name) symbol
//! table.  The kernel's `crate::symbols` module includes this file via
//! `include!(concat!(env!("OUT_DIR"), "/symbols.rs"))`.
//!
//! ## Two-pass model
//!
//! `build.rs` runs before the kernel itself is compiled, so the very
//! first build produces an empty table.  After that build emits the
//! ELF, the *next* `cargo build` reads it and embeds the symbols of
//! that previous binary.  Symbol addresses for stable code drift only
//! by a few hundred bytes between near-identical builds, so a panic
//! backtrace looking up a RIP against the prior table picks the right
//! function 99% of the time — exactly the same trick the Linux
//! kernel's `kallsyms` two-stage build uses.

use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

fn main() {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");

    // ────────────────────────────────────────────────────────────
    // Assemble the AP long-mode trampoline.
    //
    // We hand-assemble it via GNU `as` rather than embedding hex bytes
    // because the trampoline mixes 16/32/64-bit code with three mode
    // transitions and labels referenced across them.  Hand-encoding
    // would be brittle; letting `as` resolve the cross-mode jumps and
    // the GDT base label is dramatically more reliable.
    //
    // Linked at physical 0x8000 — the BSP copies the binary to that
    // page before firing INIT-SIPI-SIPI.  All absolute references
    // resolve to 0x8000 + relative-offset, which is the address each
    // mode actually executes at (real-mode CS:IP, identity-mapped
    // protected/long).
    //
    // We also extract patched-field offsets via `nm` so the Rust side
    // can locate `param_cr3`, `param_stack`, `param_entry`, and
    // `param_cpu_id` without re-deriving the layout.
    build_ap_trampoline(&out_dir);

    // ────────────────────────────────────────────────────────────
    // Compile in-tree coreutils into static x86_64 ELF binaries
    // (gcc -nostdlib -static).  Embedded into the kernel so init can
    // populate /bin in the RAMDISK at boot.
    build_coreutils(&out_dir);

    let dest = Path::new(&out_dir).join("symbols.rs");

    // Allow custom location via env, default to release target dir.
    let kernel_path = env::var("RUSTOS_KERNEL_BINARY")
        .unwrap_or_else(|_| "target/x86_64-rustos/release/rustos".to_string());

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", kernel_path);
    println!("cargo:rerun-if-env-changed=RUSTOS_KERNEL_BINARY");

    let mut symbols: Vec<(u64, u64, String)> = Vec::new();
    if Path::new(&kernel_path).exists() {
        let nm = Command::new("nm")
            .args(&["--defined-only", "--print-size", &kernel_path])
            .output();
        if let Ok(out) = nm {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                // Format: "addr size type name" — name may be missing
                // for non-function symbols; size is sometimes blank.
                let mut it = line.split_whitespace();
                let addr_s = match it.next() { Some(s) => s, None => continue };
                let size_s = match it.next() { Some(s) => s, None => continue };
                let typ_s = match it.next() { Some(s) => s, None => continue };
                let name = match it.next() { Some(s) => s, None => continue };
                // Only accept text symbols ('T' = global, 't' = local).
                if !(typ_s == "T" || typ_s == "t") { continue; }
                let addr = match u64::from_str_radix(addr_s, 16) { Ok(v) => v, Err(_) => continue };
                let size = u64::from_str_radix(size_s, 16).unwrap_or(0);
                if addr == 0 { continue; }
                symbols.push((addr, size, name.to_string()));
            }
        }
    }

    symbols.sort_by_key(|t| t.0);
    symbols.dedup_by_key(|t| t.0);

    // Run addr2line in batch mode to resolve every symbol's start
    // address to a (file, line) pair from DWARF.  We only resolve
    // symbol *starts* (not every interior PC), which keeps the
    // embedded table small while still giving "function at file:line"
    // panic traces.  addr2line stays silent on unresolved addresses
    // (prints "??:?"), which we filter out.
    let mut source_lines: Vec<String> = vec![String::new(); symbols.len()];
    if !symbols.is_empty() && Path::new(&kernel_path).exists() {
        let mut input = String::new();
        for (addr, _, _) in &symbols {
            input.push_str(&format!("{:x}\n", addr));
        }
        use std::io::Write as _;
        if let Ok(mut child) = Command::new("addr2line")
            .args(&["-e", &kernel_path])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(input.as_bytes());
            }
            if let Ok(out) = child.wait_with_output() {
                let s = String::from_utf8_lossy(&out.stdout);
                for (i, line) in s.lines().enumerate() {
                    if i >= source_lines.len() { break; }
                    let line = line.trim();
                    // addr2line prints "??:?" or "??:0" for unresolved.
                    if line.starts_with("??:") || line == ":?" || line.is_empty() {
                        continue;
                    }
                    // Strip any leading kernel src prefix to keep
                    // entries short — the user knows the project root.
                    let trimmed = line.rsplit_once("/rustos/")
                        .map(|(_, t)| t.to_string())
                        .unwrap_or_else(|| line.to_string());
                    source_lines[i] = trimmed;
                }
            }
        }
    }

    let mut f = fs::File::create(&dest).expect("create symbols.rs");
    writeln!(f, "// AUTO-GENERATED by build.rs — do not edit").unwrap();
    writeln!(f, "// Source: {}", kernel_path).unwrap();
    writeln!(f, "pub static SYMBOLS: &[(u64, u64, &str, &str)] = &[").unwrap();
    let mut resolved = 0usize;
    for (i, (addr, size, name)) in symbols.iter().enumerate() {
        // Escape backslashes and double-quotes; Rust symbol names from
        // the mangler are otherwise plain ASCII.
        let escaped_name = name.replace('\\', "\\\\").replace('"', "\\\"");
        let escaped_loc  = source_lines[i].replace('\\', "\\\\").replace('"', "\\\"");
        if !escaped_loc.is_empty() { resolved += 1; }
        writeln!(f, "    ({:#x}, {:#x}, \"{}\", \"{}\"),",
            addr, size, escaped_name, escaped_loc).unwrap();
    }
    writeln!(f, "];").unwrap();
    println!("cargo:warning=symbols.rs: embedded {} symbols ({} with source line)",
        symbols.len(), resolved);
}

/// Assemble `src/smp_trampoline.s` into a raw binary blob and emit a
/// generated Rust file that exposes the bytes plus the patched-field
/// offsets.  Pipeline:
///
///     as --64 -o out.o src/smp_trampoline.s
///     ld -Ttext=0x8000 -nostdlib -o out.elf out.o
///     objcopy -O binary out.elf out.bin
///     nm out.elf  →  parse to find param_* offsets
///
/// Failure aborts the build with a clear diagnostic — the kernel can't
/// link without this blob, since `src/smp.rs` includes it.
fn build_ap_trampoline(out_dir: &str) {
    let asm_path = "src/smp_trampoline.s";
    println!("cargo:rerun-if-changed={}", asm_path);

    let obj_path = format!("{}/smp_trampoline.o", out_dir);
    let elf_path = format!("{}/smp_trampoline.elf", out_dir);
    let bin_path = format!("{}/smp_trampoline.bin", out_dir);
    let rs_path  = format!("{}/smp_trampoline_data.rs", out_dir);

    // 1. Assemble.
    let r = Command::new("as")
        .args(&["--64", "-o", &obj_path, asm_path])
        .output()
        .expect("build.rs: GNU `as` not in PATH (apt install binutils)");
    if !r.status.success() {
        panic!("build.rs: as failed:\n{}", String::from_utf8_lossy(&r.stderr));
    }

    // 2. Link at physical 0x8000.  -z noexecstack silences a binutils
    // warning about an executable stack section appearing in the obj.
    let r = Command::new("ld")
        .args(&[
            "-nostdlib", "-Ttext=0x8000", "-z", "noexecstack",
            "-o", &elf_path, &obj_path,
        ])
        .output()
        .expect("build.rs: ld not in PATH");
    if !r.status.success() {
        panic!("build.rs: ld failed:\n{}", String::from_utf8_lossy(&r.stderr));
    }

    // 3. Extract raw binary.
    let r = Command::new("objcopy")
        .args(&["-O", "binary", &elf_path, &bin_path])
        .output()
        .expect("build.rs: objcopy not in PATH");
    if !r.status.success() {
        panic!("build.rs: objcopy failed:\n{}", String::from_utf8_lossy(&r.stderr));
    }

    // 4. Parse `nm` for symbol addresses.  We care about:
    //      ap_trampoline_start (== 0x8000)
    //      ap_trampoline_end
    //      param_cr3 / param_stack / param_entry / param_cpu_id
    let nm = Command::new("nm").arg(&elf_path).output()
        .expect("build.rs: nm not in PATH");
    let mut start = 0u64;
    let mut end   = 0u64;
    let mut p_cr3 = 0u64;
    let mut p_stk = 0u64;
    let mut p_ent = 0u64;
    let mut p_cpu = 0u64;
    for line in String::from_utf8_lossy(&nm.stdout).lines() {
        let mut it = line.split_whitespace();
        let addr_s = match it.next() { Some(s) => s, None => continue };
        let _typ   = match it.next() { Some(s) => s, None => continue };
        let name   = match it.next() { Some(s) => s, None => continue };
        let addr = match u64::from_str_radix(addr_s, 16) { Ok(v) => v, Err(_) => continue };
        match name {
            "ap_trampoline_start" => start = addr,
            "ap_trampoline_end"   => end   = addr,
            "param_cr3"     => p_cr3 = addr,
            "param_stack"   => p_stk = addr,
            "param_entry"   => p_ent = addr,
            "param_cpu_id"  => p_cpu = addr,
            _ => {}
        }
    }
    if start != 0x8000 {
        panic!("build.rs: ap_trampoline_start should link at 0x8000, got {:#x}", start);
    }
    let len = end.checked_sub(start).expect("ap_trampoline_end < start") as usize;

    // 5. Emit the generated Rust file.
    let mut src = String::new();
    src.push_str("// AUTO-GENERATED by build.rs from src/smp_trampoline.s — do not edit\n");
    src.push_str(&format!(
        "pub const AP_LM_TRAMPOLINE: &[u8; {}] = include_bytes!(\"{}\");\n",
        len, bin_path));
    src.push_str(&format!("pub const AP_LM_TRAMPOLINE_LEN: usize = {};\n", len));
    src.push_str(&format!("pub const AP_LM_OFF_CR3: usize = 0x{:x};\n",   p_cr3 - start));
    src.push_str(&format!("pub const AP_LM_OFF_STACK: usize = 0x{:x};\n", p_stk - start));
    src.push_str(&format!("pub const AP_LM_OFF_ENTRY: usize = 0x{:x};\n", p_ent - start));
    src.push_str(&format!("pub const AP_LM_OFF_CPU_ID: usize = 0x{:x};\n", p_cpu - start));
    fs::write(&rs_path, src).expect("write smp_trampoline_data.rs");

    println!("cargo:warning=ap_trampoline: {} bytes, params at +{:x}/{:x}/{:x}/{:x}",
        len, p_cr3 - start, p_stk - start, p_ent - start, p_cpu - start);
}

/// Build the in-tree coreutils into static x86-64 ELF binaries and emit
/// a Rust file with their bytes embedded.  The kernel installs them in
/// /bin in the RAMDISK at boot.
///
/// Pipeline per binary:
///     gcc -nostdlib -static -ffreestanding -fno-builtin -O2
///         -nostartfiles -no-pie crt.s NAME.c -o bin_NAME.elf
/// `crt.s` provides _start (unpacks the SysV stack into argc/argv,
/// calls main, exits with main's return).  -static + no-pie give us a
/// classical fixed-address executable; the kernel's existing ELF
/// loader handles ET_EXEC.
fn build_coreutils(out_dir: &str) {
    let src_dir = "coreutils_src";
    println!("cargo:rerun-if-changed={}/syscalls.h", src_dir);
    println!("cargo:rerun-if-changed={}/crt.s", src_dir);

    let utils = ["true", "false", "echo", "pwd", "hostname", "cat"];
    let mut binaries: Vec<(String, String, usize)> = Vec::new();

    for util in utils.iter() {
        let c_path = format!("{}/{}.c", src_dir, util);
        let elf_path = format!("{}/bin_{}.elf", out_dir, util);
        println!("cargo:rerun-if-changed={}", c_path);

        let r = Command::new("gcc")
            .args(&[
                "-nostdlib", "-static", "-ffreestanding", "-fno-builtin",
                "-fno-stack-protector", "-no-pie", "-O2",
                "-Wl,--build-id=none",
                "-Wl,-z,noexecstack",
                "-o", &elf_path,
                &format!("{}/crt.s", src_dir),
                &c_path,
            ])
            .output()
            .expect("build.rs: gcc not in PATH");
        if !r.status.success() {
            panic!("build.rs: gcc {} failed:\n{}", util,
                String::from_utf8_lossy(&r.stderr));
        }

        let bytes = fs::read(&elf_path)
            .unwrap_or_else(|e| panic!("read {}: {}", elf_path, e));
        let len = bytes.len();
        // Sanity check: must be ELF.
        assert!(bytes.len() >= 4 && &bytes[..4] == &[0x7F, b'E', b'L', b'F'],
            "build.rs: {} is not an ELF binary", elf_path);
        binaries.push((util.to_string(), elf_path.clone(), len));
    }

    // Emit src/coreutils_blobs.rs equivalent in OUT_DIR.
    let rs_path = format!("{}/coreutils_blobs.rs", out_dir);
    let mut src = String::new();
    src.push_str("// AUTO-GENERATED by build.rs — do not edit\n");
    src.push_str("// (CoreUtilBin is in scope at the include! site)\n\n");
    for (name, path, len) in &binaries {
        let const_name = name.to_uppercase();
        src.push_str(&format!(
            "pub const BIN_{}: &[u8; {}] = include_bytes!(\"{}\");\n",
            const_name, len, path));
    }
    src.push_str("\npub const COREUTILS: &[CoreUtilBin] = &[\n");
    for (name, _, _) in &binaries {
        let const_name = name.to_uppercase();
        src.push_str(&format!(
            "    CoreUtilBin {{ name: \"{}\", bytes: BIN_{} }},\n",
            name, const_name));
    }
    src.push_str("];\n");
    fs::write(&rs_path, src).expect("write coreutils_blobs.rs");

    let total: usize = binaries.iter().map(|(_, _, n)| *n).sum();
    println!("cargo:warning=coreutils: {} binaries, {} bytes total",
        binaries.len(), total);
}
