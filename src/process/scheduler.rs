/// Process scheduler — CFS-style virtual runtime.
///
/// Each runnable process carries a `vruntime` (in `Process` struct), which
/// is *normalised* CPU time: every tick the running process spends on
/// the CPU adds `BASE_WEIGHT / weight(nice)` to its vruntime, where the
/// weight table follows Linux CFS:
///
///     nice = -20 ⇒ weight = 88761  (much faster vruntime growth slowdown)
///     nice =   0 ⇒ weight =  1024  (BASE_WEIGHT — neutral)
///     nice =  19 ⇒ weight =    15
///
/// The next process to run is always the runnable one with the smallest
/// vruntime, which guarantees long-term proportional fairness regardless
/// of arrival order.  When a freshly-runnable process enters the queue
/// (new fork, wake from sleep) its vruntime is set to the current
/// minimum so it can't dominate the CPU just because it sat at zero
/// while others accumulated time.
///
/// Sleep queues, signal delivery, and the kernel-mode tick are
/// unchanged — only the pick-next and quantum arithmetic moved to CFS.
///
/// Implementation notes:
/// * The runqueue is a `BTreeMap<u64, Vec<Pid>>` keyed by vruntime so
///   `O(log n)` insert/remove and `O(1)` peek-min work.
/// * To bound the number of context switches per second we keep a
///   minimum quantum (`SCHED_MIN_GRANULARITY`).  We only re-pick after
///   that many ticks even if a smaller-vruntime process became runnable.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;
use crate::process::{Pid, ProcessState, PROCESS_MANAGER};
use crate::process::context::switch_context;
use crate::task::timer::current_ticks;

/// Linux CFS weight table.  Index = nice + 20 (so nice=-20 is index 0).
/// Each successive nice level changes the weight by a factor of ~1.25.
const NICE_WEIGHTS: [u32; 40] = [
    88761, 71755, 56483, 46273, 36291,
    29154, 23254, 18705, 14949, 11916,
     9548,  7620,  6100,  4904,  3906,
     3121,  2501,  1991,  1586,  1277,
     1024,   820,   655,   526,   423,
      335,   272,   215,   172,   137,
      110,    87,    70,    56,    45,
       36,    29,    23,    18,    15,
];
/// Weight for nice=0 — used as the numerator when scaling vruntime.
const BASE_WEIGHT: u32 = 1024;

fn weight_for_nice(nice: i8) -> u32 {
    let idx = (nice as i32 + 20).clamp(0, 39) as usize;
    NICE_WEIGHTS[idx]
}

/// Increment in vruntime for one tick of CPU time, given a nice value.
/// We multiply by 1024 so the integer arithmetic keeps useful precision
/// for low-priority (high-nice) processes.
fn vtick_for_nice(nice: i8) -> u64 {
    (BASE_WEIGHT as u64 * BASE_WEIGHT as u64) / weight_for_nice(nice) as u64
}

/// Minimum slice the running process gets before we consider re-picking.
/// Without this the scheduler would context-switch on every tick whenever
/// two processes had equal vruntimes (which is the common case).
const SCHED_MIN_GRANULARITY: usize = 2;
/// Targeted period over which all runnable processes get one slice.
/// Larger values give bigger slices when many processes are runnable.
const SCHED_LATENCY: usize = 16;

struct SleepEntry {
    pid: Pid,
    wake_tick: u64,
}

/// Time slice for the current process — varies with the number of
/// runnable processes so the total period stays bounded.
fn slice_for(runnable: usize) -> usize {
    let s = SCHED_LATENCY / runnable.max(1);
    s.max(SCHED_MIN_GRANULARITY)
}

pub struct Scheduler {
    /// Runnable processes — kept as an unordered VecDeque for O(1)
    /// add/remove; we sort by vruntime in `next_process` against the
    /// current `Process.vruntime` values.  At scale this would become
    /// a BTreeMap<vruntime, Pid>, but for tens of processes the linear
    /// scan is faster and avoids keeping vruntime duplicated in two
    /// places (the BTree key would need updating on every tick).
    ready_queue: VecDeque<Pid>,
    /// Slice the current process is allowed to hold.
    quantum: usize,
    /// Ticks the current process has been running this slice.
    quantum_counter: usize,
    sleep_queue: Vec<SleepEntry>,
    /// Tracks the global minimum vruntime ever observed; new and
    /// awakening processes start at this value so they get a fair
    /// (but not infinite) head start.
    min_vruntime: u64,
}

impl Scheduler {
    pub const fn new() -> Self {
        Scheduler {
            ready_queue: VecDeque::new(),
            quantum: SCHED_LATENCY,
            quantum_counter: 0,
            sleep_queue: Vec::new(),
            min_vruntime: 0,
        }
    }

    pub fn enqueue(&mut self, pid: Pid) {
        if self.ready_queue.contains(&pid) { return; }
        // Lift this process's vruntime to at least min_vruntime so it
        // can't sit at 0 and starve the queue when we picked from it.
        let mut pm = PROCESS_MANAGER.lock();
        if let Some(p) = pm.get_process_mut(pid) {
            if p.vruntime < self.min_vruntime {
                p.vruntime = self.min_vruntime;
            }
        }
        drop(pm);
        self.ready_queue.push_back(pid);
        self.publish_load_to_smp();
    }

    pub fn dequeue(&mut self, pid: Pid) {
        self.ready_queue.retain(|&p| p != pid);
        self.publish_load_to_smp();
    }

    /// Mirror the global ready-queue length into the BSP's `smp::CpuState`
    /// run_queue_len atomic, so observers (`smp::total_runnable`,
    /// `smp::least_loaded_cpu`, the `top` shell command) see reality.
    /// This is a stepping stone toward real per-CPU run queues — once the
    /// AP-side scheduler ticks fire from `ap_main`, this publish call will
    /// be replaced by the AP's own enqueue/dequeue against its slot.
    fn publish_load_to_smp(&self) {
        if let Some(slot) = crate::smp::cpu_state(crate::apic::lapic_id()) {
            slot.run_queue_len.store(
                self.ready_queue.len(),
                core::sync::atomic::Ordering::Release,
            );
        }
    }

    /// Pick the runnable PID with the smallest vruntime, given an
    /// already-locked ProcessManager (so we can call this from the
    /// timer ISR without deadlock).  Removes the chosen PID from the
    /// run queue.
    pub fn dequeue_next_with_pm(&mut self, pm: &crate::process::ProcessManager) -> Option<Pid> {
        if self.ready_queue.is_empty() { return None; }
        let mut best_idx = 0usize;
        let mut best_v = u64::MAX;
        for (i, &pid) in self.ready_queue.iter().enumerate() {
            let v = pm.get_process(pid).map(|p| p.vruntime).unwrap_or(u64::MAX);
            if v < best_v { best_v = v; best_idx = i; }
        }
        if best_v != u64::MAX && best_v > self.min_vruntime {
            self.min_vruntime = best_v;
        }
        self.ready_queue.remove(best_idx)
    }

    /// Same as `dequeue_next_with_pm` but takes the PM lock itself.
    pub fn dequeue_next(&mut self) -> Option<Pid> { self.next_process() }

    fn next_process(&mut self) -> Option<Pid> {
        if self.ready_queue.is_empty() { return None; }
        let pm = PROCESS_MANAGER.lock();
        let mut best_idx = 0usize;
        let mut best_v = u64::MAX;
        for (i, &pid) in self.ready_queue.iter().enumerate() {
            let v = pm.get_process(pid).map(|p| p.vruntime).unwrap_or(u64::MAX);
            if v < best_v { best_v = v; best_idx = i; }
        }
        drop(pm);
        if best_v != u64::MAX && best_v > self.min_vruntime {
            self.min_vruntime = best_v;
        }
        self.ready_queue.remove(best_idx)
    }

    /// Full tick — used when preempting user-mode processes.  Charges
    /// the running process's vruntime, handles sleep wakeups and
    /// signal delivery, then preempts when the slice is exhausted.
    pub fn tick(&mut self) {
        self.charge_running_vruntime();
        self.wake_sleepers();
        self.deliver_signals();

        self.quantum_counter += 1;
        if self.quantum_counter >= self.quantum {
            self.quantum_counter = 0;
            self.preempt();
            self.schedule_user();
        }
    }

    /// Add one tick's worth of normalised CPU time to the currently
    /// running process's vruntime.  Bounded above by saturating add
    /// so a long-running niced process can never wrap.
    fn charge_running_vruntime(&mut self) {
        let mut pm = PROCESS_MANAGER.lock();
        if let Some(pid) = pm.current_pid {
            if let Some(p) = pm.get_process_mut(pid) {
                p.vruntime = p.vruntime.saturating_add(vtick_for_nice(p.nice));
            }
        }
    }

    /// Kernel-only tick — used when timer fires in kernel mode.
    /// Only handles sleep wakeups and signal delivery, no context switching.
    pub fn tick_kernel_only(&mut self) {
        self.wake_sleepers();
        self.deliver_signals();
        // No preemption in kernel mode — the async executor handles scheduling
    }

    fn deliver_signals(&mut self) {
        let mut pm = PROCESS_MANAGER.lock();
        if let Some(pid) = pm.current_pid {
            let terminated = pm.deliver_pending_signals(pid);
            if terminated {
                self.dequeue(pid);
            }
        }
    }

    fn wake_sleepers(&mut self) {
        let now = current_ticks();
        let mut i = 0;
        while i < self.sleep_queue.len() {
            if now >= self.sleep_queue[i].wake_tick {
                let entry = self.sleep_queue.swap_remove(i);
                let mut pm = PROCESS_MANAGER.lock();
                if let Some(process) = pm.get_process_mut(entry.pid) {
                    if process.state == ProcessState::Blocked {
                        process.state = ProcessState::Ready;
                        self.enqueue(entry.pid);
                    }
                }
                drop(pm);
            } else {
                i += 1;
            }
        }
    }

    pub fn sleep_ticks(&mut self, ticks: u64) -> Option<Pid> {
        let mut pm = PROCESS_MANAGER.lock();
        let current_pid = pm.current_pid?;
        let process = pm.get_process_mut(current_pid)?;
        process.state = ProcessState::Blocked;
        pm.current_pid = None;
        drop(pm);

        let wake_tick = current_ticks() + ticks;
        self.sleep_queue.push(SleepEntry { pid: current_pid, wake_tick });
        Some(current_pid)
    }

    /// Put a specific PID to sleep for N ticks (for background jobs)
    pub fn sleep_pid(&mut self, pid: Pid, ticks: u64) {
        let wake_tick = current_ticks() + ticks;
        self.sleep_queue.push(SleepEntry { pid, wake_tick });
    }

    /// Wait for a specific PID to terminate (blocking poll)
    pub fn is_pid_done(&self, pid: Pid) -> bool {
        let pm = PROCESS_MANAGER.lock();
        if let Some(proc) = pm.processes.iter().find(|p| p.pid == pid) {
            matches!(proc.state, ProcessState::Terminated | ProcessState::Zombie)
        } else {
            true // process doesn't exist anymore = done
        }
    }

    /// Preempt current user process — move it to ready queue
    fn preempt(&mut self) {
        let mut pm = PROCESS_MANAGER.lock();

        if let Some(current_pid) = pm.current_pid {
            if let Some(current) = pm.get_process_mut(current_pid) {
                if current.state == ProcessState::Running {
                    current.state = ProcessState::Ready;
                    self.enqueue(current_pid);
                }
            }
            pm.current_pid = None;
        }

        drop(pm);
    }

    /// Schedule next user process to run.  Sets current_pid and the
    /// CFS slice.  The actual TrapFrame switch happens in
    /// timer_preempt_handler which reads the new current process's
    /// trap_frame.
    fn schedule_user(&mut self) {
        let next_pid = match self.next_process() {
            Some(pid) => pid,
            None => return,
        };
        let runnable = self.ready_queue.len() + 1; // +1 for the picked one
        let slice = slice_for(runnable);

        let mut pm = PROCESS_MANAGER.lock();
        self.quantum_counter = 0;
        if let Some(next) = pm.get_process_mut(next_pid) {
            next.state = ProcessState::Running;
        }
        pm.current_pid = Some(next_pid);
        drop(pm);
        self.quantum = slice;
    }

    /// Schedule next process for kernel cooperative switching
    pub fn schedule(&mut self) {
        let mut pm = PROCESS_MANAGER.lock();

        let current_pid = pm.current_pid;
        let next_pid = match self.next_process() {
            Some(pid) => pid,
            None => {
                drop(pm);
                return;
            }
        };

        if Some(next_pid) == current_pid {
            self.enqueue(next_pid);
            drop(pm);
            return;
        }

        self.quantum_counter = 0;

        if let Some(next) = pm.get_process_mut(next_pid) {
            next.state = ProcessState::Running;
        }
        pm.current_pid = Some(next_pid);
        let runnable = self.ready_queue.len() + 1;
        self.quantum = slice_for(runnable);

        if let Some(curr_pid) = current_pid {
            let current_ctx = pm.get_process_mut(curr_pid)
                .map(|p| &mut p.context as *mut _);
            let next_ctx = pm.get_process_mut(next_pid)
                .map(|p| &p.context as *const _);

            if let (Some(curr), Some(next)) = (current_ctx, next_ctx) {
                drop(pm);
                unsafe {
                    switch_context(curr, next);
                }
                return;
            }
        }

        drop(pm);
    }

    pub fn yield_cpu(&mut self) {
        let mut pm = PROCESS_MANAGER.lock();

        if let Some(current_pid) = pm.current_pid {
            if let Some(current) = pm.get_process_mut(current_pid) {
                current.state = ProcessState::Ready;
                self.enqueue(current_pid);
            }
            pm.current_pid = None;
        }

        drop(pm);
        self.schedule();
    }

    pub fn block(&mut self) {
        let mut pm = PROCESS_MANAGER.lock();

        if let Some(current_pid) = pm.current_pid {
            if let Some(current) = pm.get_process_mut(current_pid) {
                current.state = ProcessState::Blocked;
            }
            pm.current_pid = None;
        }

        drop(pm);
        self.schedule();
    }

    pub fn unblock(&mut self, pid: Pid) {
        let mut pm = PROCESS_MANAGER.lock();

        if let Some(process) = pm.get_process_mut(pid) {
            if process.state == ProcessState::Blocked {
                process.state = ProcessState::Ready;
                self.enqueue(pid);
            }
        }

        drop(pm);
    }

    pub fn ready_count(&self) -> usize {
        self.ready_queue.len()
    }

    pub fn ready_list(&self) -> alloc::vec::Vec<Pid> {
        self.ready_queue.iter().copied().collect()
    }
}

/// Global scheduler
pub static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());

pub fn init() {}

pub fn yield_cpu() {
    SCHEDULER.lock().yield_cpu();
}

pub fn block() {
    SCHEDULER.lock().block();
}

pub fn unblock(pid: Pid) {
    SCHEDULER.lock().unblock(pid);
}

pub fn schedule() {
    SCHEDULER.lock().schedule();
}

/// Called on each timer tick (for backward compatibility — kernel mode only)
pub fn tick() {
    SCHEDULER.lock().tick_kernel_only();
}

pub fn spawn(entry_point: usize, stack_size: usize) -> Pid {
    let mut pm = PROCESS_MANAGER.lock();
    let pid = pm.create_process(entry_point, stack_size);
    drop(pm);
    SCHEDULER.lock().enqueue(pid);
    pid
}

/// Spawn a user-mode process for preemptive scheduling.
/// Allocates kernel stack and creates process struct BEFORE locking PM
/// to avoid deadlock with timer ISR (which also locks PM).
pub fn spawn_user(entry_point: u64, user_stack_top: u64) -> Pid {
    // Allocate PID under brief PM lock
    let pid = {
        let mut pm = PROCESS_MANAGER.lock();
        let pid = pm.next_pid;
        pm.next_pid += 1;
        pid
    };
    // Build process struct WITHOUT holding PM (allocates kernel stack on heap)
    let process = crate::process::Process::new_user(pid, None, entry_point, user_stack_top);
    // Insert into PM under brief lock
    PROCESS_MANAGER.lock().insert_process(process);
    SCHEDULER.lock().enqueue(pid);
    pid
}

pub fn terminate(pid: Pid) {
    SCHEDULER.lock().dequeue(pid);
    PROCESS_MANAGER.lock().terminate(pid);
}

pub fn sleep(ticks: u64) {
    SCHEDULER.lock().sleep_ticks(ticks);
    schedule();
}

pub fn set_nice(pid: Pid, nice: i8) -> Option<i8> {
    let clamped = nice.clamp(-20, 19);
    let mut pm = PROCESS_MANAGER.lock();
    if let Some(process) = pm.get_process_mut(pid) {
        let old = process.nice;
        process.nice = clamped;
        Some(old)
    } else {
        None
    }
}

pub fn get_nice(pid: Pid) -> Option<i8> {
    let pm = PROCESS_MANAGER.lock();
    pm.get_process(pid).map(|p| p.nice)
}

pub fn exit() {
    let pid = PROCESS_MANAGER.lock().current_pid;
    if let Some(pid) = pid {
        terminate(pid);
        schedule();
    }
}
