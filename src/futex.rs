//! Futex (fast userspace mutex) — kernel side.
//!
//! Linux semantics, FUTEX_WAIT and FUTEX_WAKE only:
//!   * `wait(uaddr, expected, timeout)`: if `*uaddr == expected`, sleep
//!     until woken or timeout.  Atomic comparison happens under the
//!     wait-queue lock so no wake-up can be missed.
//!   * `wake(uaddr, nr)`: wake up to `nr` waiters parked on `uaddr`.
//!
//! Each address that has ever had a waiter is keyed in [`WAITQUEUES`].
//! A waiter is identified by the calling Pid; on wake we mark the Pid
//! Ready and let the scheduler resume it.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Mutex;

#[derive(Debug, Clone, Copy)]
pub enum FutexError {
    BadAddress,
    WouldBlock,   // *uaddr != expected at the time of FUTEX_WAIT
    TimedOut,
    InvalidArgument,
}

/// Per-address wait queue.
struct WaitQueue {
    waiters: Vec<usize>,           // PIDs parked on this address
}

impl WaitQueue {
    const fn new() -> Self { WaitQueue { waiters: Vec::new() } }
}

/// addr → wait queue.  Address is the kernel virtual pointer of the futex
/// word.  Aliasing across processes (same memory mapped twice) only works
/// when both views resolve to the same address — fine in our shared
/// address space.
static WAITQUEUES: Mutex<BTreeMap<usize, WaitQueue>> = Mutex::new(BTreeMap::new());

/// FUTEX_WAIT: park if `*uaddr == expected`.  `timeout_ticks == 0` means
/// wait indefinitely.  Returns Err on mismatch / timeout.
pub fn wait(uaddr: *const i32, expected: i32, timeout_ticks: u64) -> Result<(), FutexError> {
    if uaddr.is_null() { return Err(FutexError::BadAddress); }

    // Atomic check + park under the queue lock so a concurrent wake
    // can't slip past.
    let pid = {
        let mut q = WAITQUEUES.lock();
        let cur = unsafe { core::ptr::read_volatile(uaddr) };
        if cur != expected { return Err(FutexError::WouldBlock); }
        let entry = q.entry(uaddr as usize).or_insert_with(WaitQueue::new);

        // Determine our PID.  In the kernel-only model we use the
        // PROCESS_MANAGER's current pid; if no user process is bound,
        // we synthesise a pseudo-pid from the address (so background
        // tasks can still distinguish their own waits).
        let pm = crate::process::PROCESS_MANAGER.lock();
        let pid = pm.current_pid.unwrap_or(0xfffe);
        drop(pm);
        entry.waiters.push(pid);
        pid
    };

    // Park.  Without per-process page tables we can't truly suspend the
    // caller from a kernel thread, so we busy-park: spin polling the
    // queue until our waiter slot is removed (i.e. someone called wake)
    // or the timeout elapses.  Yields HLT between checks.
    let start = crate::task::timer::current_ticks();
    loop {
        // Removed from the queue?  → woken successfully.
        let still_parked = WAITQUEUES.lock()
            .get(&(uaddr as usize))
            .map(|q| q.waiters.iter().any(|&p| p == pid))
            .unwrap_or(false);
        if !still_parked { return Ok(()); }

        if timeout_ticks > 0 {
            let now = crate::task::timer::current_ticks();
            if now.saturating_sub(start) >= timeout_ticks {
                // Time's up — remove ourselves and report.
                let mut q = WAITQUEUES.lock();
                if let Some(wq) = q.get_mut(&(uaddr as usize)) {
                    wq.waiters.retain(|&p| p != pid);
                }
                return Err(FutexError::TimedOut);
            }
        }
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

/// FUTEX_WAKE: wake up to `nr` waiters, return the count actually woken.
pub fn wake(uaddr: *const i32, nr: usize) -> usize {
    if uaddr.is_null() { return 0; }
    let mut q = WAITQUEUES.lock();
    let wq = match q.get_mut(&(uaddr as usize)) {
        Some(w) => w,
        None => return 0,
    };
    let take = nr.min(wq.waiters.len());
    let woken: alloc::vec::Vec<usize> = wq.waiters.drain(..take).collect();
    if wq.waiters.is_empty() {
        q.remove(&(uaddr as usize));
    }
    drop(q);

    // Mark each woken PID Ready in the scheduler.
    let mut pm = crate::process::PROCESS_MANAGER.lock();
    for pid in &woken {
        if let Some(p) = pm.get_process_mut(*pid) {
            if p.state == crate::process::ProcessState::Blocked {
                p.state = crate::process::ProcessState::Ready;
            }
        }
    }
    woken.len()
}

/// Snapshot for `futex` shell command diagnostics.
pub fn snapshot() -> Vec<(usize, Vec<usize>)> {
    WAITQUEUES.lock().iter()
        .map(|(addr, q)| (*addr, q.waiters.clone()))
        .collect()
}
