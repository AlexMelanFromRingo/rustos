//! `poll(2)` and `select(2)` syscall implementation.
//!
//! Both system calls expose I/O readiness checking on a set of file
//! descriptors with an optional timeout.  This module provides:
//!
//! * Linux-compatible `POLL*` event flags and the `PollFd` C-ABI struct.
//! * [`fd_readiness`] for synchronously querying the readiness of a single FD.
//! * [`do_poll`] / [`do_select`] entry points that loop over the request set,
//!   collect readiness, and respect a millisecond timeout.
//!
//! In a single-process kernel without true blocking-on-FD semantics yet, the
//! "wait" portion is implemented as a busy-wait polling loop that checks the
//! TSC-derived `current_ticks()` clock.  Once the scheduler grows proper
//! per-FD wait queues we can replace this with a `WAIT_FOR_FD(fd, mask)`
//! state.

use crate::syscall::filedesc::{get_fd_table, FileKind};
use crate::syscall::pipe::{pipe_bytes_available, pipe_read_closed, pipe_write_closed, pipe_writable};

/// Linux POLL_* event flags.
pub mod flags {
    pub const POLLIN:     i16 = 0x0001; // ready for read
    pub const POLLPRI:    i16 = 0x0002; // urgent data
    pub const POLLOUT:    i16 = 0x0004; // ready for write
    pub const POLLERR:    i16 = 0x0008; // error
    pub const POLLHUP:    i16 = 0x0010; // peer closed
    pub const POLLNVAL:   i16 = 0x0020; // invalid request
    pub const POLLRDNORM: i16 = 0x0040;
    pub const POLLWRNORM: i16 = 0x0100;
}

/// Linux pollfd ABI struct (12 bytes packed).  Mirrored exactly so user
/// pointers can be cast.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PollFd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

/// Compute revents for an FD based on its current state.  Returns the bitwise
/// OR of POLLIN/POLLOUT/POLLERR/POLLHUP/POLLNVAL flags.
pub fn fd_readiness(fd: usize, requested: i16) -> i16 {
    use flags::*;

    if fd >= 4096 {
        return POLLNVAL;
    }

    let mut revents = 0i16;

    // Snapshot the FileKind so we can drop the FD table guard before doing
    // any blocking work (and to avoid lifetime issues).
    let kind = {
        let table = get_fd_table();
        match table.get(fd) {
            Some(f) => f.kind.clone(),
            None => return POLLNVAL,
        }
    };

    match kind {
        FileKind::Regular => {
            // Regular files are always considered ready for both read and write.
            if requested & POLLIN != 0 { revents |= POLLIN | POLLRDNORM; }
            if requested & POLLOUT != 0 { revents |= POLLOUT | POLLWRNORM; }
        }
        FileKind::PipeRead(id) => {
            let avail = pipe_bytes_available(id);
            if avail > 0 && (requested & POLLIN != 0) {
                revents |= POLLIN | POLLRDNORM;
            }
            if pipe_write_closed(id) {
                revents |= POLLHUP;
            }
        }
        FileKind::PipeWrite(id) => {
            if pipe_writable(id) && (requested & POLLOUT != 0) {
                revents |= POLLOUT | POLLWRNORM;
            }
            if pipe_read_closed(id) {
                revents |= POLLERR;
            }
        }
    }

    // Special-case stdin: route through TTY input buffer.
    if fd == 0 && requested & POLLIN != 0 {
        if crate::tty::TTY0.lock().bytes_available() > 0 {
            revents |= POLLIN | POLLRDNORM;
        }
    }
    // stdout / stderr are always writable.
    if (fd == 1 || fd == 2) && requested & POLLOUT != 0 {
        revents |= POLLOUT | POLLWRNORM;
    }

    revents
}

/// Core poll loop.  Returns the number of FDs that became ready (or 0 on
/// timeout).  `timeout_ms` of -1 means wait forever; 0 means do not wait.
pub fn do_poll(fds: &mut [PollFd], timeout_ms: i32) -> isize {
    // Tick frequency: PIT runs at ~18.2 Hz (55 ms per tick).
    const MS_PER_TICK: u64 = 55;
    let start = crate::task::timer::current_ticks();
    let deadline = if timeout_ms < 0 {
        u64::MAX
    } else {
        start + (timeout_ms as u64 / MS_PER_TICK).max(1)
    };

    loop {
        let mut ready = 0isize;
        for pfd in fds.iter_mut() {
            pfd.revents = fd_readiness(pfd.fd as usize, pfd.events);
            if pfd.revents != 0 {
                ready += 1;
            }
        }
        if ready > 0 || timeout_ms == 0 {
            return ready;
        }
        let now = crate::task::timer::current_ticks();
        if now >= deadline {
            return 0;
        }
        // Yield: enable interrupts and halt until next tick.
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

/// `select()` style FD-set, fixed at 1024 bits like Linux `fd_set`.
pub const FD_SETSIZE: usize = 1024;

#[derive(Debug, Clone, Copy)]
pub struct FdSet {
    bits: [u64; FD_SETSIZE / 64],
}

impl FdSet {
    pub const fn empty() -> Self {
        FdSet { bits: [0; FD_SETSIZE / 64] }
    }
    pub fn set(&mut self, fd: usize) {
        if fd < FD_SETSIZE { self.bits[fd / 64] |= 1u64 << (fd % 64); }
    }
    pub fn clear(&mut self, fd: usize) {
        if fd < FD_SETSIZE { self.bits[fd / 64] &= !(1u64 << (fd % 64)); }
    }
    pub fn is_set(&self, fd: usize) -> bool {
        fd < FD_SETSIZE && (self.bits[fd / 64] & (1u64 << (fd % 64))) != 0
    }
    pub fn zero(&mut self) {
        for b in self.bits.iter_mut() { *b = 0; }
    }
}

/// `select()` wrapper.  All sets are translated into `PollFd`s and dispatched
/// to [`do_poll`], then results are written back into the sets.
pub fn do_select(
    nfds: usize,
    readfds: &mut FdSet,
    writefds: &mut FdSet,
    exceptfds: &mut FdSet,
    timeout_ms: i32,
) -> isize {
    use flags::*;

    let mut polls = alloc::vec::Vec::new();
    for fd in 0..nfds.min(FD_SETSIZE) {
        let mut events: i16 = 0;
        if readfds.is_set(fd)   { events |= POLLIN; }
        if writefds.is_set(fd)  { events |= POLLOUT; }
        if exceptfds.is_set(fd) { events |= POLLPRI; }
        if events != 0 {
            polls.push(PollFd { fd: fd as i32, events, revents: 0 });
        }
    }

    let ready = do_poll(&mut polls, timeout_ms);

    readfds.zero();
    writefds.zero();
    exceptfds.zero();

    if ready <= 0 {
        return ready;
    }

    let mut count = 0isize;
    for pfd in polls.iter() {
        if pfd.revents == 0 { continue; }
        let fd = pfd.fd as usize;
        if pfd.revents & (POLLIN | POLLRDNORM) != 0 {
            readfds.set(fd);
            count += 1;
        }
        if pfd.revents & (POLLOUT | POLLWRNORM) != 0 {
            writefds.set(fd);
            count += 1;
        }
        if pfd.revents & (POLLPRI | POLLERR | POLLHUP) != 0 {
            exceptfds.set(fd);
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fdset_smoke() {
        let mut s = FdSet::empty();
        assert!(!s.is_set(7));
        s.set(7);
        assert!(s.is_set(7));
        s.clear(7);
        assert!(!s.is_set(7));
    }
}
