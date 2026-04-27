//! Linux-style `epoll` built on top of [`fd_readiness`].
//!
//! Each `epoll` instance has its own set of "interest" entries (fd → events
//! mask + user data).  `epoll_wait` walks the interest list, polls each FD
//! with [`fd_readiness`], and returns the ready entries.  Edge-triggered
//! mode (`EPOLLET`) is approximated by remembering the last revents per fd
//! and only reporting transitions.
//!
//! No per-instance ready list / wake-up wiring yet — every wait is a
//! readiness scan.  That mirrors what `select(2)` does and is fine for the
//! current FD-count scale.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Mutex;

pub mod flags {
    pub const EPOLLIN:  u32 = 0x001;
    pub const EPOLLPRI: u32 = 0x002;
    pub const EPOLLOUT: u32 = 0x004;
    pub const EPOLLERR: u32 = 0x008;
    pub const EPOLLHUP: u32 = 0x010;
    pub const EPOLLET:  u32 = 0x8000_0000;
}

pub mod ops {
    pub const EPOLL_CTL_ADD: i32 = 1;
    pub const EPOLL_CTL_DEL: i32 = 2;
    pub const EPOLL_CTL_MOD: i32 = 3;
}

/// Linux-compatible epoll_event ABI: 12 bytes, packed.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct EpollEvent {
    pub events: u32,
    pub data:   u64,
}

#[derive(Debug, Clone)]
struct InterestEntry {
    events: u32,
    data:   u64,
    last_revents: i16,
}

struct EpollInstance {
    interest: BTreeMap<i32, InterestEntry>,
}

impl EpollInstance {
    fn new() -> Self { EpollInstance { interest: BTreeMap::new() } }
}

pub struct EpollTable {
    instances: Vec<Option<EpollInstance>>,
}

impl EpollTable {
    pub const fn new() -> Self { EpollTable { instances: Vec::new() } }

    fn create(&mut self) -> i32 {
        for (i, slot) in self.instances.iter_mut().enumerate() {
            if slot.is_none() { *slot = Some(EpollInstance::new()); return i as i32; }
        }
        self.instances.push(Some(EpollInstance::new()));
        (self.instances.len() - 1) as i32
    }

    fn get_mut(&mut self, ep: i32) -> Option<&mut EpollInstance> {
        if ep < 0 { return None; }
        self.instances.get_mut(ep as usize).and_then(|s| s.as_mut())
    }

    fn close(&mut self, ep: i32) {
        if ep >= 0 && (ep as usize) < self.instances.len() {
            self.instances[ep as usize] = None;
        }
    }
}

pub static EPOLL: Mutex<EpollTable> = Mutex::new(EpollTable::new());

/// `epoll_create1` — create a new epoll instance.  Returns the epfd (>=0).
pub fn epoll_create() -> i32 {
    EPOLL.lock().create()
}

/// `epoll_ctl` — add/modify/remove an interest entry.
pub fn epoll_ctl(epfd: i32, op: i32, fd: i32, ev: EpollEvent) -> i32 {
    let mut table = EPOLL.lock();
    let inst = match table.get_mut(epfd) {
        Some(i) => i,
        None => return -22, // EINVAL
    };
    match op {
        x if x == ops::EPOLL_CTL_ADD => {
            if inst.interest.contains_key(&fd) { return -17; } // EEXIST
            inst.interest.insert(fd, InterestEntry {
                events: ev.events, data: ev.data, last_revents: 0,
            });
            0
        }
        x if x == ops::EPOLL_CTL_MOD => {
            match inst.interest.get_mut(&fd) {
                Some(e) => { e.events = ev.events; e.data = ev.data; 0 }
                None => -2, // ENOENT
            }
        }
        x if x == ops::EPOLL_CTL_DEL => {
            if inst.interest.remove(&fd).is_some() { 0 } else { -2 }
        }
        _ => -22,
    }
}

/// `epoll_wait` — poll all interest entries.  Fills `out` with ready events.
/// `timeout_ms`: -1 wait forever, 0 poll once, >0 wait up to N ms.
/// Returns number of ready events (>=0) or negative errno.
pub fn epoll_wait(epfd: i32, out: &mut [EpollEvent], timeout_ms: i32) -> isize {
    use crate::syscall::poll::{fd_readiness, flags as pflags};

    if out.is_empty() { return 0; }
    const MS_PER_TICK: u64 = 55;
    let start = crate::task::timer::current_ticks();
    let deadline = if timeout_ms < 0 {
        u64::MAX
    } else {
        start + (timeout_ms as u64 / MS_PER_TICK).max(1)
    };

    loop {
        let mut count = 0isize;
        {
            let mut table = EPOLL.lock();
            let inst = match table.get_mut(epfd) {
                Some(i) => i,
                None => return -22,
            };
            for (fd, entry) in inst.interest.iter_mut() {
                // Translate epoll events → poll events
                let mut req: i16 = 0;
                if entry.events & flags::EPOLLIN  != 0 { req |= pflags::POLLIN; }
                if entry.events & flags::EPOLLOUT != 0 { req |= pflags::POLLOUT; }
                if entry.events & flags::EPOLLPRI != 0 { req |= pflags::POLLPRI; }

                let revents = fd_readiness(*fd as usize, req);
                if revents == 0 { continue; }

                // ET mode: only report transitions
                if entry.events & flags::EPOLLET != 0 && revents == entry.last_revents {
                    continue;
                }
                entry.last_revents = revents;

                let mut emitted: u32 = 0;
                if revents & pflags::POLLIN  != 0 { emitted |= flags::EPOLLIN; }
                if revents & pflags::POLLOUT != 0 { emitted |= flags::EPOLLOUT; }
                if revents & pflags::POLLPRI != 0 { emitted |= flags::EPOLLPRI; }
                if revents & pflags::POLLERR != 0 { emitted |= flags::EPOLLERR; }
                if revents & pflags::POLLHUP != 0 { emitted |= flags::EPOLLHUP; }

                if (count as usize) < out.len() {
                    out[count as usize] = EpollEvent { events: emitted, data: entry.data };
                    count += 1;
                }
            }
        }

        if count > 0 || timeout_ms == 0 { return count; }
        let now = crate::task::timer::current_ticks();
        if now >= deadline { return 0; }
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

/// Close an epoll instance.
pub fn epoll_close(epfd: i32) {
    EPOLL.lock().close(epfd);
}
