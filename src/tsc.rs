//! TSC (Time Stamp Counter) calibration and nanosecond reads.
//!
//! Uses the PIT to measure how many TSC cycles elapse during a 50 ms window
//! and stores the resulting frequency in Hz.  Once calibrated, subsequent
//! reads convert TSC delta → nanoseconds without leaving the CPU.
//!
//! Calibration is best-effort: if the PIT setup is wrong or the CPU lies
//! about TSC stability, the resulting numbers are still monotonically
//! increasing but may not match wall-clock seconds exactly.  Real Linux
//! uses HPET + clocksource invariants to defend against this; we don't.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::instructions::port::Port;

/// Calibrated TSC frequency in cycles per second.
static FREQ_HZ: AtomicU64 = AtomicU64::new(0);
/// TSC value at calibration time (used as boot epoch for monotonic reads).
static BOOT_TSC: AtomicU64 = AtomicU64::new(0);
static CALIBRATED: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn read_tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Calibrate the TSC frequency by busy-waiting on the PIT for ~50 ms.
///
/// Note: this re-programs PIT channel 2 (the "speaker" channel) so it does
/// not interfere with the channel-0 system timer.
pub fn calibrate() {
    if CALIBRATED.load(Ordering::Relaxed) { return; }
    let freq = calibrate_via_pit_ch2();
    FREQ_HZ.store(freq, Ordering::Relaxed);
    BOOT_TSC.store(read_tsc(), Ordering::Relaxed);
    CALIBRATED.store(true, Ordering::Relaxed);
}

fn calibrate_via_pit_ch2() -> u64 {
    // Program PIT channel 2 in mode 0 (interrupt on terminal count) for a
    // ~50 ms count.  PIT base frequency is 1_193_182 Hz.
    const PIT_HZ: u64 = 1_193_182;
    const TARGET_MS: u64 = 50;
    let count = (PIT_HZ * TARGET_MS / 1000) as u16;

    unsafe {
        // Enable PIT channel 2 gate (port 0x61, bit 0).  Disable speaker (bit 1).
        let mut port61: Port<u8> = Port::new(0x61);
        let cur = port61.read();
        port61.write((cur & 0xFC) | 0x01);

        // Program channel 2 in lobyte/hibyte access mode 0.
        let mut cmd: Port<u8> = Port::new(0x43);
        cmd.write(0xB0); // ch2 | lo/hi byte | mode 0 | binary

        let mut data2: Port<u8> = Port::new(0x42);
        data2.write((count & 0xFF) as u8);
        data2.write((count >> 8) as u8);

        let start = read_tsc();
        // Wait for PIT_OUT2 (port 0x61 bit 5) to go high (count expired).
        let deadline_loops = 100_000_000u64;
        let mut loops = 0u64;
        while (port61.read() & 0x20) == 0 {
            loops += 1;
            if loops > deadline_loops { break; }
        }
        let end = read_tsc();

        // Disable channel 2 gate again.
        let cur = port61.read();
        port61.write(cur & 0xFC);

        let cycles = end.wrapping_sub(start);
        // cycles per TARGET_MS ms → cycles per second
        cycles.saturating_mul(1000) / TARGET_MS
    }
}

/// Calibrated frequency in Hz, or 0 if not calibrated.
pub fn freq_hz() -> u64 { FREQ_HZ.load(Ordering::Relaxed) }
pub fn boot_tsc() -> u64 { BOOT_TSC.load(Ordering::Relaxed) }
pub fn is_calibrated() -> bool { CALIBRATED.load(Ordering::Relaxed) }

/// Nanoseconds since [`calibrate`] was called.  Returns 0 if uncalibrated.
pub fn ns_since_boot() -> u64 {
    let f = freq_hz();
    if f == 0 { return 0; }
    let delta = read_tsc().wrapping_sub(boot_tsc());
    // ns = delta * 1e9 / f, computed carefully to avoid overflow on long runs.
    let secs = delta / f;
    let rem  = delta % f;
    secs * 1_000_000_000 + (rem * 1_000_000_000) / f
}
