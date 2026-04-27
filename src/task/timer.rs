//! PIT-driven kernel timer + Timer futures.
//!
//! Each timer interrupt bumps `TICKS` and wakes every task that registered
//! a waker for the next tick.  Earlier versions used a single
//! `AtomicWaker`, which silently starved every-but-one Timer future
//! whenever multiple background tasks were waiting concurrently.  We now
//! keep a `Vec<Waker>`.
//!
//! Locking discipline:
//!   * `tick()` runs inside a hardware ISR.  It must not block on a lock
//!     that mainline code holds.  We acquire the wake-list lock with
//!     `try_lock` only — if mainline is mid-update we drop the wake on
//!     this tick (the next tick will deliver it).
//!   * Mainline `register_waker()` disables interrupts while it holds the
//!     lock so the ISR can never preempt it mid-critical-section.

use core::{
    pin::Pin,
    future::Future,
    task::{Context, Poll, Waker},
    sync::atomic::{AtomicU64, Ordering},
};
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::interrupts;

static TICKS: AtomicU64 = AtomicU64::new(0);

static WAKERS: Mutex<Vec<Waker>> = Mutex::new(Vec::new());

fn register_waker(w: &Waker) {
    interrupts::without_interrupts(|| {
        let mut list = WAKERS.lock();
        if !list.iter().any(|other| other.will_wake(w)) {
            list.push(w.clone());
        }
    });
}

/// Called from the timer ISR.  Best-effort: if mainline is mid-update we
/// skip waking on this tick rather than deadlock.
fn drain_and_wake_all_isr_safe() {
    if let Some(mut list) = WAKERS.try_lock() {
        let drained: Vec<Waker> = core::mem::take(&mut *list);
        // Drop the guard before invoking wakers (those may push back into
        // the queue when they reschedule — taking the lock recursively
        // would deadlock).
        drop(list);
        for w in drained {
            w.wake();
        }
    }
    // else: the next tick will pick up these wakers.
}

/// Called by the timer interrupt handler.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    drain_and_wake_all_isr_safe();
}

/// Get current tick count
pub fn current_ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Sleep for approximately N timer ticks.
pub struct Timer {
    wake_time: u64,
}

impl Timer {
    pub fn new(ticks: u64) -> Self {
        Timer {
            wake_time: current_ticks() + ticks,
        }
    }
}

impl Future for Timer {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<()> {
        if current_ticks() >= self.wake_time {
            return Poll::Ready(());
        }
        register_waker(cx.waker());
        if current_ticks() >= self.wake_time {
            return Poll::Ready(());
        }
        Poll::Pending
    }
}
