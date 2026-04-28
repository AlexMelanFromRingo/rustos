//! PS/2 mouse driver.
//!
//! Talks to the PS/2 controller (8042) through ports 0x60 (data) and
//! 0x64 (command/status).  Standard 3-byte movement packets:
//!
//!   byte 0: Y-overflow X-overflow Y-sign X-sign 1 mb rb lb
//!   byte 1: dx (signed, sign in byte 0)
//!   byte 2: dy (signed, sign in byte 0; remember Y is inverted)
//!
//! State across packet bytes is held in [`MOUSE`].  Reading the cursor
//! position or button state is via the [`MouseState`] snapshot.

use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU8, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_CMD: u16 = 0x64;

/// Cursor position (no boundary clamping; consumer can clamp to fb extents).
static CURSOR_X: AtomicI32 = AtomicI32::new(0);
static CURSOR_Y: AtomicI32 = AtomicI32::new(0);
static BUTTONS:  AtomicU8 = AtomicU8::new(0);
static PACKETS_RECEIVED: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, Copy)]
pub struct MouseState {
    pub x: i32,
    pub y: i32,
    pub buttons: u8,    // bit 0 = left, bit 1 = right, bit 2 = middle
    pub packets: u32,
}

pub fn snapshot() -> MouseState {
    MouseState {
        x: CURSOR_X.load(Ordering::Relaxed),
        y: CURSOR_Y.load(Ordering::Relaxed),
        buttons: BUTTONS.load(Ordering::Relaxed),
        packets: PACKETS_RECEIVED.load(Ordering::Relaxed),
    }
}

/// Per-driver byte assembly state for the 3-byte protocol.
struct MouseAssembler {
    buf: [u8; 3],
    idx: usize,
}

static MOUSE: Mutex<MouseAssembler> = Mutex::new(MouseAssembler { buf: [0; 3], idx: 0 });

/// Called by the IRQ12 handler with each byte from port 0x60.
pub fn input_byte(byte: u8) {
    let mut m = MOUSE.lock();
    // The first byte must always have bit 3 set (per spec); use it as a
    // resync signal — when we see it but our index isn't 0, we're out
    // of sync and must drop until the next valid header.
    if m.idx == 0 && byte & 0x08 == 0 {
        return;
    }
    let idx = m.idx;
    m.buf[idx] = byte;
    m.idx += 1;
    if m.idx < 3 { return; }

    let pkt = m.buf;
    m.idx = 0;
    drop(m);
    apply_packet(&pkt);
}

fn apply_packet(pkt: &[u8; 3]) {
    let header = pkt[0];
    let mut dx = pkt[1] as i32;
    let mut dy = pkt[2] as i32;
    if header & 0x10 != 0 { dx -= 256; } // X sign
    if header & 0x20 != 0 { dy -= 256; } // Y sign
    if header & 0x40 != 0 || header & 0x80 != 0 {
        // Overflow on this packet — discard motion, keep button state.
        BUTTONS.store(header & 0x07, Ordering::Relaxed);
        PACKETS_RECEIVED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    // PS/2 reports dy with an inverted sign convention from screen coords;
    // negate so up-screen (lower y) corresponds to negative dy on the wire.
    let dy = -dy;
    CURSOR_X.fetch_add(dx, Ordering::Relaxed);
    CURSOR_Y.fetch_add(dy, Ordering::Relaxed);
    BUTTONS.store(header & 0x07, Ordering::Relaxed);
    PACKETS_RECEIVED.fetch_add(1, Ordering::Relaxed);
}

// ---------- Initialisation ----------

unsafe fn ps2_wait_write() {
    let mut status: Port<u8> = Port::new(PS2_STATUS);
    for _ in 0..1_000_000 {
        if unsafe { status.read() } & 0x02 == 0 { return; }
    }
}

unsafe fn ps2_wait_read() {
    let mut status: Port<u8> = Port::new(PS2_STATUS);
    for _ in 0..1_000_000 {
        if unsafe { status.read() } & 0x01 != 0 { return; }
    }
}

unsafe fn ps2_send_cmd(byte: u8) {
    unsafe { ps2_wait_write(); }
    let mut cmd: Port<u8> = Port::new(PS2_CMD);
    unsafe { cmd.write(byte); }
}

unsafe fn ps2_send_data(byte: u8) {
    unsafe { ps2_wait_write(); }
    let mut data: Port<u8> = Port::new(PS2_DATA);
    unsafe { data.write(byte); }
}

unsafe fn ps2_send_to_mouse(byte: u8) {
    unsafe {
        ps2_send_cmd(0xD4);  // tell controller "next byte goes to aux"
        ps2_send_data(byte);
        // Skip the ACK byte if present.
        ps2_wait_read();
        let mut data: Port<u8> = Port::new(PS2_DATA);
        let _ = data.read();
    }
}

/// Initialise the PS/2 mouse: enable aux port, enable data reporting.
pub fn init() {
    unsafe {
        // Enable auxiliary device (mouse) IRQ in controller config.
        ps2_send_cmd(0xA8);                    // enable second PS/2 port
        ps2_send_cmd(0x20);                    // read controller config
        ps2_wait_read();
        let mut data: Port<u8> = Port::new(PS2_DATA);
        let mut cfg: u8 = data.read();
        cfg |= 0x02;                           // enable IRQ12
        cfg &= !0x20;                          // clear "disable mouse clock"
        ps2_send_cmd(0x60);
        ps2_send_data(cfg);

        // Default settings, enable streaming.
        ps2_send_to_mouse(0xF6); // set defaults
        ps2_send_to_mouse(0xF4); // enable data reporting
    }
    crate::klog_info!("PS/2 mouse: initialised (IRQ 12 enabled)");
}
