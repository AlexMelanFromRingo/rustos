//! PIT-driven kernel timer + Timer futures.
//!
//! Each timer interrupt bumps `TICKS` and wakes every task that registered
//! a waker for the next tick.  An earlier version of this module used a
//! single `AtomicWaker`, which silently starved every-but-one Timer future
//! whenever multiple background tasks (cron, syslogd, httpd, status, …)
//! were waiting concurrently.  We now keep a `Vec<Waker>` so all sleepers
//! are woken on every tick.

use core::{
    pin::Pin,
    future::Future,
    task::{Context, Poll, Waker},
    sync::atomic::{AtomicU64, Ordering},
};
use alloc::vec::Vec;
use spin::Mutex;

static TICKS: AtomicU64 = AtomicU64::new(0);

static WAKERS: Mutex<Vec<Waker>> = Mutex::new(Vec::new());

fn register_waker(w: &Waker) {
    let mut list = WAKERS.lock();
    if !list.iter().any(|other| other.will_wake(w)) {
        list.push(w.clone());
    }
}

fn drain_and_wake_all() {
    let drained: Vec<Waker> = {
        let mut list = WAKERS.lock();
        core::mem::take(&mut *list)
    };
    for w in drained {
        w.wake();
    }
}

/// Called by the timer interrupt handler.  Bumps the tick counter and
/// wakes every Timer future that registered for the next firing.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    drain_and_wake_all();
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
        // Re-check after registering to avoid the classic register/notify
        // race where the tick fires between our first check and our
        // registration.
        if current_ticks() >= self.wake_time {
            return Poll::Ready(());
        }
        Poll::Pending
    }
}
