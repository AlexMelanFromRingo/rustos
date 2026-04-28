#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rustos::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use core::panic::PanicInfo;
use bootloader::{BootInfo, entry_point};

// Import VGA macros
#[macro_use]
extern crate rustos;

entry_point!(kernel_main);

/// This function is called on panic.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Print message + frame-pointer backtrace to serial (VGA WRITER may
    // be held by the panicking code).  Then a second pass to VGA so the
    // user sees something on screen.
    rustos::backtrace::print_panic(info);
    println!("{}", info);
    rustos::hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rustos::test_panic_handler(info)
}

/// Entry point for our kernel.
/// The bootloader will call this function.
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    use rustos::memory;
    use x86_64::VirtAddr;

    println!("RustOS - A minimal operating system written in Rust");
    println!("Based on Phil Opp's excellent tutorials");
    println!("  https://os.phil-opp.com/");
    println!();

    // Initialize GDT, IDT and PIC
    rustos::init();

    // Initialize memory management
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe {
        memory::BitmapFrameAllocator::init(&boot_info.memory_map)
    };

    // Report physical memory
    let total_mb = memory::total_usable_memory() / (1024 * 1024);
    println!("Physical memory: {} MiB usable", total_mb);

    // Initialize heap
    rustos::allocator::init_heap(&mut mapper, &mut frame_allocator)
        .expect("heap initialization failed");

    // Now that heap is available, start kernel logging
    klog_info!("RustOS kernel starting");
    klog_info!("Physical memory: {} MiB usable", total_mb);
    klog_info!("GDT, IDT, PIC initialized");
    klog_info!("Heap allocator initialized (16 MiB)");
    klog_info!("Bitmap frame allocator active");

    // Initialize user space identity mapping
    memory::userspace::init_user_space_mapping(&mut mapper, &mut frame_allocator)
        .expect("user space mapping failed");
    println!("User space memory mapped ({}MB at 0x{:08X})",
        memory::userspace::USER_SPACE_SIZE / (1024 * 1024),
        memory::userspace::USER_SPACE_START);
    klog_info!("User space mapped: {}MB at 0x{:08X}",
        memory::userspace::USER_SPACE_SIZE / (1024 * 1024),
        memory::userspace::USER_SPACE_START);

    // Store frame allocator globally for runtime use (guard pages, etc.)
    memory::store_frame_allocator(frame_allocator);
    klog_info!("Frame allocator stored globally");

    // Initialize ATA driver AFTER heap is ready
    rustos::drivers::ata::init();
    klog_info!("ATA driver initialized");

    // PS/2 mouse + HPET register-only timer.
    rustos::drivers::mouse::init();
    rustos::drivers::hpet::init();

    // PCI bus scan + virtio-net probe.  If no virtio-net device is
    // attached (run without `-device virtio-net-pci`), the probe logs
    // a warning and the global stays None.
    rustos::drivers::pci::scan();
    rustos::drivers::virtio_net::init();

    // Initialize syscall support (after heap and GDT)
    rustos::init_syscall();
    klog_info!("SYSCALL/SYSRET support initialized");

    // Register kernel slab caches and exercise them once for sanity.
    rustos::slab_caches::init();
    klog_info!("Slab allocator initialized: {} caches",
        rustos::slab::SLAB_REGISTRY.snapshot().len());

    // Initialise resource limits with sensible defaults.
    rustos::rlimit::init();

    // Capabilities table: root gets all, others get none.
    rustos::capability::init();

    // CPU hardening: SMEP / SMAP / canary seed.
    // (Disabled: enabling SMEP/SMAP via CR4 caused early-boot hangs on
    // some QEMU revisions because the kernel still services pre-init
    // BIOS code with kernel-CPL.  Will re-enable once kernel page tables
    // own the entire boot path.)
    // rustos::hardening::enable();

    // Calibrate the TSC against the PIT for nanosecond-precision time reads.
    rustos::tsc::calibrate();
    klog_info!("TSC calibrated at {} Hz", rustos::tsc::freq_hz());

    // KASLR for vmalloc: slide the base address by a random page offset.
    rustos::vmalloc::randomise_base(rustos::tsc::read_tsc() ^
        rustos::drivers::rtc::read_datetime().to_unix_timestamp());
    klog_info!("vmalloc KASLR base: {:#018x}", rustos::vmalloc::vmalloc_start());

    // Bring the network stack up: registers the loopback interface.
    rustos::net::init();
    klog_info!("Network stack initialized: lo @ 127.0.0.1");

    // Provision /var/www so the demo httpd has something to serve.
    rustos::httpd::install_default_docroot();

    // Initialize user database
    rustos::users::init();
    rustos::users::init_creds();
    klog_info!("User database initialized");

    // Create /etc directory and populate with user/group files
    {
        use rustos::fs::vfs::VfsContext;
        let _ = VfsContext::mkdir("/etc");
        let _ = VfsContext::mkdir("/root");
        let _ = VfsContext::mkdir("/home");
        let db = rustos::users::USER_DB.lock();
        let _ = VfsContext::write("/etc/passwd", db.to_passwd_string().into_bytes());
        let _ = VfsContext::write("/etc/group", db.to_group_string().into_bytes());
        let _ = VfsContext::write("/etc/hostname", b"rustos\n".to_vec());
        let _ = VfsContext::write("/etc/os-release",
            b"NAME=\"RustOS\"\nID=rustos\nVERSION=\"0.1.0\"\nPRETTY_NAME=\"RustOS 0.1.0\"\n".to_vec());
        let motd = "\n\
Welcome to RustOS 0.1.0  (kernel x86_64-rustos)\n\
\n\
 * Documentation:  https://os.phil-opp.com/\n\
 * Source code:    https://github.com/AlexMelanFromRingo/rustos\n\
\n\
This is an experimental, in-development OS written from scratch in Rust.\n\
Type 'help' to list available shell commands.\n\
\n";
        let _ = VfsContext::write("/etc/motd", motd.as_bytes().to_vec());
        let _ = VfsContext::write("/etc/issue",
            b"RustOS 0.1.0 \\n \\l\n".to_vec());
        // /etc/getty.conf — uncomment require_login=1 to force a login
        // prompt at boot; the default leaves auto-root on (matching the
        // historical behaviour).  /etc/securetty lists the TTYs root may
        // log in on directly.
        let _ = VfsContext::write("/etc/getty.conf",
            b"# require_login=1   # uncomment to require login at boot\n\
              terminal=tty0\n".to_vec());
        let _ = VfsContext::write("/etc/securetty",
            b"console\ntty0\nttyS0\n".to_vec());
        let _ = VfsContext::mkdir("/var");
        let _ = VfsContext::mkdir("/var/log");
        let _ = VfsContext::write("/etc/hosts",
            b"# Static name resolution\n\
              127.0.0.1   localhost rustos\n\
              127.0.1.1   rustos.localdomain\n\
              ::1         ip6-localhost ip6-loopback\n\
              255.255.255.255  broadcasthost\n".to_vec());
        let _ = VfsContext::write("/etc/resolv.conf",
            b"# Resolver order: cache -> /etc/hosts -> wire DNS (UDP/53).\n\
              # Loopback-only build: queries to non-loopback resolvers will\n\
              # fall back to /etc/hosts since there's no NIC driver yet.\n\
              nameserver 127.0.0.1\n".to_vec());
        let _ = VfsContext::write("/etc/sudoers",
            b"# /etc/sudoers - minimal syntax: USER ALL=(ALL) [NOPASSWD]\n\
              root  ALL=(ALL) NOPASSWD\n\
              wheel ALL=(ALL)\n".to_vec());
    }
    klog_info!("Filesystem populated (/etc, /root, /home)");

    // Populate the system mount table.  Done after FAT32 mount attempt.
    rustos::fs::vfs::install_default_mounts();
    klog_info!("Mount table populated: {} entries",
        rustos::fs::vfs::list_mounts().len());

    // devtmpfs: register the standard set of /dev nodes.
    rustos::fs::devfs::install_default_nodes();
    klog_info!("devtmpfs populated: {} nodes",
        rustos::fs::devfs::list_nodes().len());

    // Initialise the SysV-style init system and bring the system up to multi-user.
    rustos::init::install_default_services();
    {
        let mut init = rustos::init::INIT.lock();
        init.boot_tick = rustos::task::timer::current_ticks();
    }
    let _ = rustos::init::INIT.lock().set_runlevel(rustos::init::RunLevel::MultiUser);
    let running = rustos::init::INIT.lock().running_count();
    let total = rustos::init::INIT.lock().total_count();
    klog_info!("init: runlevel 3, {} of {} services running", running, total);

    println!("Kernel initialized successfully!");
    klog_info!("Kernel initialization complete");
    println!();

    #[cfg(test)]
    test_main();

    println!("Starting RustOS Shell...");
    println!("Type 'help' for available commands");
    println!();

    // Install default crontab and load any persisted entries.
    rustos::cron::install_defaults();
    rustos::cron::save();
    rustos::cron::reload();

    let mut executor = rustos::task::executor::Executor::new();
    executor.spawn(rustos::task::Task::new(keyboard_task()));
    executor.spawn(rustos::task::Task::new(status_task()));
    executor.spawn(rustos::task::Task::new(syslogd_task()));
    executor.spawn(rustos::task::Task::new(crond_task()));
    executor.spawn(rustos::task::Task::new(httpd_task()));
    executor.spawn(rustos::task::Task::new(tcp_retx_task()));
    executor.run();
}

/// Periodic retransmit ticker for the TCP stack.  Walks every endpoint
/// and re-sends any segments past their RTO.
async fn tcp_retx_task() {
    use rustos::task::timer::Timer;
    loop {
        Timer::new(2).await; // ~110 ms
        rustos::net::tcp::retransmit_tick();
    }
}

/// HTTP daemon: listens on 127.0.0.1:80, serves /var/www on every request.
async fn httpd_task() {
    use rustos::net::tcp::listen;
    use rustos::net::{Ipv4Addr, SocketAddrV4};
    use rustos::task::timer::Timer;

    let addr = SocketAddrV4 { ip: Ipv4Addr::LOCALHOST, port: 80 };
    let listener = match listen(addr, 16) {
        Ok(l) => l,
        Err(_) => return,
    };
    rustos::httpd::set_global_listener(listener);
    rustos::syslog::log(
        rustos::syslog::Facility::Daemon,
        rustos::syslog::Severity::Notice,
        "httpd",
        "HTTP server listening on 127.0.0.1:80".into(),
    );

    loop {
        // serve_once internally accepts, services, and closes a single
        // connection.  When there's nothing pending it returns Err — yield
        // briefly so we don't hot-spin.
        match rustos::httpd::serve_once(listener) {
            Ok(_) => {}
            Err(_) => { Timer::new(2).await; }
        }
    }
}

/// Periodic crond: every ~5 seconds checks all crontab entries.
async fn crond_task() {
    use rustos::task::timer::Timer;

    rustos::syslog::log(
        rustos::syslog::Facility::Cron,
        rustos::syslog::Severity::Notice,
        "crond",
        "cron daemon started".into(),
    );

    loop {
        Timer::new(91).await;        // ~5 seconds
        rustos::cron::tick();
    }
}

/// Periodic flusher that copies fresh syslog entries to /var/log/messages.
async fn syslogd_task() {
    use rustos::task::timer::Timer;

    // Drop a startup record so /var/log/messages is non-empty after boot.
    rustos::syslog::log(
        rustos::syslog::Facility::Syslog,
        rustos::syslog::Severity::Notice,
        "syslogd",
        "syslog daemon started".into(),
    );

    loop {
        // Flush every ~10 seconds (PIT ~18.2 Hz).
        Timer::new(182).await;
        rustos::syslog::flush_to_messages_log("rustos");
    }
}

async fn status_task() {
    use rustos::task::timer::Timer;

    loop {
        // Wait ~60 seconds (assuming ~18.2 Hz timer)
        Timer::new(1092).await;

        // Periodic system status
        let ticks = rustos::task::timer::current_ticks();
        let uptime = ticks / 18;
        println!("[uptime: {}s]", uptime);
    }
}

/// Try to mount FAT32 filesystem automatically at startup
fn try_mount_fat32() -> Result<(), &'static str> {
    use rustos::fs::fat32::{Fat32, FAT32};

    // Try to create FAT32 instance (this reads boot sector and validates)
    let fat32_fs = Fat32::new()?;

    // Store in global singleton
    let mut fat32 = FAT32.lock();
    *fat32 = Some(fat32_fs);
    drop(fat32);

    Ok(())
}

async fn keyboard_task() {
    use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
    use futures_util::stream::StreamExt;
    use rustos::task::keyboard::{InputStream, InputEvent};
    use rustos::shell::Shell;
    use rustos::vga_buffer::WRITER;
    use x86_64::instructions::interrupts;

    // Enable serial input interrupts
    rustos::serial::enable_serial_interrupts();

    // Try to auto-mount FAT32 at startup
    match try_mount_fat32() {
        Ok(()) => {
            println!("FAT32 filesystem mounted successfully!");
            println!("Files and command history will persist across reboots.");
            klog_info!("FAT32 filesystem mounted");
        }
        Err(e) => {
            println!("Could not mount FAT32: {}", e);
            println!("Using RAM disk (data will be lost on reboot).");
            klog_warn!("FAT32 mount failed: {}, using RAMDISK", e);
        }
    }
    println!();
    println!("Active filesystem: {}", rustos::fs::vfs::VfsContext::filesystem_name());
    println!();

    let mut input_stream = InputStream::new();
    let mut keyboard = Keyboard::new(
        ScancodeSet1::new(),
        layouts::Us104Key,
        HandleControl::MapLettersToUnicode,
    );

    let mut shell = Shell::new();

    // Display /etc/issue then /etc/motd at boot (Ubuntu Server style).
    if let Ok(issue) = rustos::fs::vfs::VfsContext::read("/etc/issue") {
        if let Ok(s) = core::str::from_utf8(&issue) {
            let host = "rustos";
            let kern = "x86_64-rustos";
            print!("{}", s.replace("\\n", host).replace("\\l", kern));
        }
    }

    // Optional getty-style login.  If /etc/getty.conf contains a line
    // `require_login=1`, ask for credentials before dropping to the shell.
    let require_login = match rustos::fs::vfs::VfsContext::read("/etc/getty.conf") {
        Ok(d) => core::str::from_utf8(&d)
            .map(|s| s.lines().any(|l| l.trim() == "require_login=1"))
            .unwrap_or(false),
        Err(_) => false,
    };
    if require_login {
        // Cycle until login succeeds.  cmd_login() prints "Login incorrect"
        // on failure but doesn't loop, so we re-invoke until CURRENT_CREDS
        // shows a non-65534 (non-guest) uid.
        loop {
            shell.set_buffer_for("login");
            shell.execute();
            let euid = rustos::users::CURRENT_CREDS.lock().euid;
            if euid != 65534 { break; }
        }
    } else if let Ok(motd) = rustos::fs::vfs::VfsContext::read("/etc/motd") {
        if let Ok(s) = core::str::from_utf8(&motd) {
            print!("{}", s);
        }
    }
    shell.print_prompt();

    while let Some(event) = input_stream.next().await {
        // Drain any commands queued by background tasks (cron, init, etc.).
        // Most subsystems run their command directly via a transient Shell,
        // but the queue is still here for callers that want to interleave
        // with the user's actual shell session.
        if !rustos::shell::SHELL_COMMAND_QUEUE.lock().is_empty() {
            print!("\n");
            shell.drain_queued_commands();
            shell.print_prompt();
        }

        // Decode the event into a key action
        let decoded_key = match event {
            InputEvent::Scancode(scancode) => {
                if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
                    keyboard.process_keyevent(key_event)
                } else {
                    None
                }
            }
            InputEvent::SerialByte(byte) => {
                // Convert serial byte to DecodedKey
                match byte {
                    b'\r' | b'\n' => Some(DecodedKey::Unicode('\n')),
                    0x7F | 0x08 => Some(DecodedKey::Unicode('\u{0008}')), // DEL/BS → backspace
                    0x09 => Some(DecodedKey::Unicode('\t')),
                    0x01 => Some(DecodedKey::Unicode('\u{0001}')), // Ctrl+A
                    0x05 => Some(DecodedKey::Unicode('\u{0005}')), // Ctrl+E
                    0x15 => Some(DecodedKey::Unicode('\u{0015}')), // Ctrl+U
                    0x17 => Some(DecodedKey::Unicode('\u{0017}')), // Ctrl+W
                    0x1B => None, // Escape sequence start — ignore for now
                    b if b >= 0x20 && b <= 0x7E => Some(DecodedKey::Unicode(b as char)),
                    _ => None,
                }
            }
        };

        if let Some(key) = decoded_key {
                let prompt_len = shell.prompt_len();
                match key {
                    DecodedKey::Unicode(character) => {
                        if character == '\n' {
                            println!();
                            shell.execute();
                            shell.print_prompt();
                        } else if character == '\u{0001}' {
                            // Ctrl+A - Move to beginning of line
                            shell.move_cursor_home();
                            interrupts::without_interrupts(|| {
                                let mut writer = WRITER.lock();
                                writer.set_cursor_column(prompt_len);
                            });
                        } else if character == '\u{0005}' {
                            // Ctrl+E - Move to end of line
                            let buffer_len = shell.buffer_len();
                            shell.move_cursor_end();
                            interrupts::without_interrupts(|| {
                                let mut writer = WRITER.lock();
                                writer.set_cursor_column(prompt_len + buffer_len);
                            });
                        } else if character == '\u{0015}' {
                            // Ctrl+U - Delete from cursor to beginning
                            let old_len = shell.buffer_len();
                            if shell.delete_to_beginning() {
                                let (_clear_len, new_text) = shell.redraw_line();
                                let new_len = new_text.len();

                                interrupts::without_interrupts(|| {
                                    let mut writer = WRITER.lock();
                                    // Move cursor to start of buffer (after prompt "> ")
                                    writer.set_cursor_column(prompt_len);
                                    drop(writer);

                                    // Print new text
                                    print!("{}", new_text);
                                    // Clear remaining old characters
                                    for _ in 0..(old_len - new_len) {
                                        print!(" ");
                                    }

                                    // Set cursor to beginning (after prompt)
                                    writer = WRITER.lock();
                                    writer.set_cursor_column(prompt_len);
                                });
                            }
                        } else if character == '\u{0017}' {
                            // Ctrl+W - Delete word backward
                            let old_len = shell.buffer_len();
                            if shell.delete_word_backward() {
                                let (_clear_len, new_text) = shell.redraw_line();
                                let new_len = new_text.len();
                                let cursor_pos = shell.get_cursor_pos();

                                interrupts::without_interrupts(|| {
                                    let mut writer = WRITER.lock();
                                    // Move cursor to start of buffer (after prompt "> ")
                                    writer.set_cursor_column(prompt_len);
                                    drop(writer);

                                    // Print new text
                                    print!("{}", new_text);
                                    // Clear remaining old characters
                                    for _ in 0..(old_len - new_len) {
                                        print!(" ");
                                    }

                                    // Set cursor to correct position
                                    writer = WRITER.lock();
                                    writer.set_cursor_column(prompt_len + cursor_pos);
                                });
                            }
                        } else if character == '\u{0008}' {
                            // Backspace as Unicode character - use absolute positioning
                            let old_len = shell.buffer_len();
                            if shell.backspace() {
                                let (_clear_len, new_text) = shell.redraw_line();
                                let new_len = new_text.len();
                                let cursor_pos = shell.get_cursor_pos();

                                interrupts::without_interrupts(|| {
                                    let mut writer = WRITER.lock();
                                    // Move cursor to start of buffer (after prompt "> ")
                                    writer.set_cursor_column(prompt_len);
                                    drop(writer);

                                    // Print new text
                                    print!("{}", new_text);
                                    // Clear remaining old characters
                                    for _ in 0..(old_len - new_len) {
                                        print!(" ");
                                    }

                                    // Set cursor to correct position
                                    writer = WRITER.lock();
                                    writer.set_cursor_column(prompt_len + cursor_pos);
                                });
                            }
                        } else if character == '\t' {
                            // Tab for autocomplete - use absolute positioning
                            let old_len = shell.buffer_len();
                            if let Some(completed) = shell.autocomplete() {
                                let new_len = completed.len();

                                interrupts::without_interrupts(|| {
                                    let mut writer = WRITER.lock();
                                    // Move cursor to start of buffer (after prompt "> ")
                                    writer.set_cursor_column(prompt_len);
                                    drop(writer);

                                    // Print completed command
                                    print!("{}", completed);
                                    // Clear remaining old characters if new is shorter
                                    if new_len < old_len {
                                        for _ in 0..(old_len - new_len) {
                                            print!(" ");
                                        }
                                    }

                                    // Set cursor to end of completed text
                                    writer = WRITER.lock();
                                    writer.set_cursor_column(prompt_len + new_len);
                                });
                                shell.set_buffer(completed);
                            }
                        } else if character >= ' ' && character <= '~' {
                            // Only printable ASCII - insert at cursor position
                            let old_len = shell.buffer_len();
                            shell.add_char(character);
                            let cursor_pos = shell.get_cursor_pos();

                            // Redraw entire buffer
                            let (_clear_len, new_text) = shell.redraw_line();
                            let new_len = new_text.len();

                            interrupts::without_interrupts(|| {
                                let mut writer = WRITER.lock();
                                // Move cursor to start of buffer (after prompt "> ")
                                writer.set_cursor_column(prompt_len);
                                drop(writer);

                                // Print new text
                                print!("{}", new_text);
                                // Clear remaining old characters if any
                                if old_len > new_len {
                                    for _ in 0..(old_len - new_len) {
                                        print!(" ");
                                    }
                                }

                                // Set cursor to correct position (prompt + cursor_pos)
                                writer = WRITER.lock();
                                writer.set_cursor_column(prompt_len + cursor_pos);
                            });
                        }
                        // Ignore other control characters
                    }
                    DecodedKey::RawKey(key_code) => {
                        use pc_keyboard::KeyCode;
                        match key_code {
                            KeyCode::Backspace => {
                                // Backspace at cursor position
                                let old_len = shell.buffer_len();
                                if shell.backspace() {
                                    let (_clear_len, new_text) = shell.redraw_line();
                                    let new_len = new_text.len();
                                    let cursor_pos = shell.get_cursor_pos();

                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Move cursor to start of buffer (after prompt "> ")
                                        writer.set_cursor_column(prompt_len);
                                        drop(writer);

                                        // Print new text
                                        print!("{}", new_text);
                                        // Clear remaining old characters
                                        for _ in 0..(old_len - new_len) {
                                            print!(" ");
                                        }

                                        // Set cursor to correct position
                                        writer = WRITER.lock();
                                        writer.set_cursor_column(prompt_len + cursor_pos);
                                    });
                                }
                            }
                            KeyCode::Delete => {
                                // Delete character at cursor
                                let old_len = shell.buffer_len();
                                if shell.delete_char() {
                                    let (_clear_len, new_text) = shell.redraw_line();
                                    let new_len = new_text.len();
                                    let cursor_pos = shell.get_cursor_pos();

                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Move cursor to start of buffer (after prompt "> ")
                                        writer.set_cursor_column(prompt_len);
                                        drop(writer);

                                        // Print new text
                                        print!("{}", new_text);
                                        // Clear remaining old characters
                                        for _ in 0..(old_len - new_len) {
                                            print!(" ");
                                        }

                                        // Set cursor to correct position
                                        writer = WRITER.lock();
                                        writer.set_cursor_column(prompt_len + cursor_pos);
                                    });
                                }
                            }
                            KeyCode::ArrowLeft => {
                                // Move cursor left using absolute positioning
                                if shell.move_cursor_left() {
                                    let cursor_pos = shell.get_cursor_pos();
                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Set cursor to absolute position (prompt + cursor_pos)
                                        writer.set_cursor_column(prompt_len + cursor_pos);
                                    });
                                }
                            }
                            KeyCode::ArrowRight => {
                                // Move cursor right using absolute positioning
                                if shell.move_cursor_right() {
                                    let cursor_pos = shell.get_cursor_pos();
                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Set cursor to absolute position (prompt + cursor_pos)
                                        writer.set_cursor_column(prompt_len + cursor_pos);
                                    });
                                }
                            }
                            KeyCode::Home => {
                                // Move to start of line
                                shell.move_cursor_home();
                                // Set VGA cursor to prompt position (2 chars for "> ")
                                interrupts::without_interrupts(|| {
                                    let mut writer = WRITER.lock();
                                    writer.set_cursor_column(prompt_len);
                                });
                            }
                            KeyCode::End => {
                                // Move to end of line
                                let buffer_len = shell.buffer_len();
                                shell.move_cursor_end();
                                // Set VGA cursor to prompt + buffer length
                                interrupts::without_interrupts(|| {
                                    let mut writer = WRITER.lock();
                                    writer.set_cursor_column(prompt_len + buffer_len);
                                });
                            }
                            KeyCode::ArrowUp => {
                                let old_len = shell.buffer_len();
                                if let Some(cmd) = shell.history_up() {
                                    let new_len = cmd.len();

                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Move cursor to start of buffer (after prompt "> ")
                                        writer.set_cursor_column(prompt_len);
                                        drop(writer);

                                        // Print new command
                                        print!("{}", cmd);
                                        // Clear remaining old characters if new is shorter
                                        if new_len < old_len {
                                            for _ in 0..(old_len - new_len) {
                                                print!(" ");
                                            }
                                        }

                                        // Set cursor to end of new command
                                        writer = WRITER.lock();
                                        writer.set_cursor_column(prompt_len + new_len);
                                    });
                                    shell.set_buffer(cmd);
                                }
                            }
                            KeyCode::ArrowDown => {
                                let old_len = shell.buffer_len();
                                if let Some(cmd) = shell.history_down() {
                                    let new_len = cmd.len();

                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Move cursor to start of buffer (after prompt "> ")
                                        writer.set_cursor_column(prompt_len);
                                        drop(writer);

                                        // Print new command
                                        print!("{}", cmd);
                                        // Clear remaining old characters if new is shorter
                                        if new_len < old_len {
                                            for _ in 0..(old_len - new_len) {
                                                print!(" ");
                                            }
                                        }

                                        // Set cursor to end of new command
                                        writer = WRITER.lock();
                                        writer.set_cursor_column(prompt_len + new_len);
                                    });
                                    shell.set_buffer(cmd);
                                }
                            }
                            KeyCode::Tab => {
                                // Autocomplete command or filename
                                let old_len = shell.buffer_len();
                                if let Some(completed) = shell.autocomplete() {
                                    let new_len = completed.len();

                                    interrupts::without_interrupts(|| {
                                        let mut writer = WRITER.lock();
                                        // Move cursor to start of buffer (after prompt "> ")
                                        writer.set_cursor_column(prompt_len);
                                        drop(writer);

                                        // Print completed command
                                        print!("{}", completed);
                                        // Clear remaining old characters if new is shorter
                                        if new_len < old_len {
                                            for _ in 0..(old_len - new_len) {
                                                print!(" ");
                                            }
                                        }

                                        // Set cursor to end of completed text
                                        writer = WRITER.lock();
                                        writer.set_cursor_column(prompt_len + new_len);
                                    });
                                    shell.set_buffer(completed);
                                }
                            }
                            _ => {} // Ignore other special keys
                        }
                    }
                }
        }
    }
}

#[test_case]
fn trivial_assertion() {
    assert_eq!(1, 1);
}
