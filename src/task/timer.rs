use core::{
    pin::Pin,
    future::Future,
    task::{Context, Poll},
    sync::atomic::{AtomicU64, Ordering},
};

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Called by timer interrupt handler
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Get current tick count
pub fn current_ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Sleep for approximately N timer ticks
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

    fn poll(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<()> {
        if current_ticks() >= self.wake_time {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}
