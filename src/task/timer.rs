use core::{
    pin::Pin,
    future::Future,
    task::{Context, Poll},
    sync::atomic::{AtomicU64, Ordering},
};
use futures_util::task::AtomicWaker;

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Global waker for timer-based futures.
/// When a timer tick occurs, all pending timer futures are woken
/// so the executor re-polls them.
static TIMER_WAKER: AtomicWaker = AtomicWaker::new();

/// Called by timer interrupt handler
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    // Wake any futures waiting on timer
    TIMER_WAKER.wake();
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

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<()> {
        if current_ticks() >= self.wake_time {
            Poll::Ready(())
        } else {
            // Register waker so timer interrupt can wake us
            TIMER_WAKER.register(cx.waker());
            // Re-check after registering to avoid race condition
            if current_ticks() >= self.wake_time {
                TIMER_WAKER.take();
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }
}
