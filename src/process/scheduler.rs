/// Process scheduler
///
/// Implements a priority-based round-robin scheduler with sleep queues,
/// timer-based wakeups, and preemptive user-mode context switching.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;
use crate::process::{Pid, ProcessState, PROCESS_MANAGER};
use crate::process::context::switch_context;
use crate::task::timer::current_ticks;

/// Base quantum in timer ticks (for nice 0)
const BASE_QUANTUM: usize = 10;
/// Minimum quantum (for nice 19)
const MIN_QUANTUM: usize = 2;
/// Maximum quantum (for nice -20)
const MAX_QUANTUM: usize = 40;

struct SleepEntry {
    pid: Pid,
    wake_tick: u64,
}

fn quantum_from_nice(nice: i8) -> usize {
    let q = BASE_QUANTUM as i32 - (nice as i32 * (MAX_QUANTUM as i32 - MIN_QUANTUM as i32) / 39);
    (q as usize).clamp(MIN_QUANTUM, MAX_QUANTUM)
}

pub struct Scheduler {
    ready_queue: VecDeque<Pid>,
    quantum: usize,
    quantum_counter: usize,
    sleep_queue: Vec<SleepEntry>,
}

impl Scheduler {
    pub const fn new() -> Self {
        Scheduler {
            ready_queue: VecDeque::new(),
            quantum: BASE_QUANTUM,
            quantum_counter: 0,
            sleep_queue: Vec::new(),
        }
    }

    pub fn enqueue(&mut self, pid: Pid) {
        if !self.ready_queue.contains(&pid) {
            self.ready_queue.push_back(pid);
        }
    }

    pub fn dequeue(&mut self, pid: Pid) {
        self.ready_queue.retain(|&p| p != pid);
    }

    /// Get and remove the next process from the ready queue (public for ISR use).
    /// Uses an already-locked ProcessManager to avoid deadlock in ISR context.
    pub fn dequeue_next_with_pm(&mut self, pm: &crate::process::ProcessManager) -> Option<Pid> {
        if self.ready_queue.is_empty() {
            return None;
        }
        let mut best_idx = 0;
        let mut best_nice: i8 = 19;
        for (i, &pid) in self.ready_queue.iter().enumerate() {
            let nice = pm.get_process(pid).map(|p| p.nice).unwrap_or(19);
            if nice < best_nice {
                best_nice = nice;
                best_idx = i;
            }
        }
        self.ready_queue.remove(best_idx)
    }

    /// Get and remove the next process from the ready queue (locks PM internally).
    pub fn dequeue_next(&mut self) -> Option<Pid> {
        self.next_process()
    }

    /// Get the next process to run (priority-aware round-robin)
    fn next_process(&mut self) -> Option<Pid> {
        if self.ready_queue.is_empty() {
            return None;
        }

        let pm = PROCESS_MANAGER.lock();
        let mut best_idx = 0;
        let mut best_nice: i8 = 19;

        for (i, &pid) in self.ready_queue.iter().enumerate() {
            let nice = pm.get_process(pid).map(|p| p.nice).unwrap_or(19);
            if nice < best_nice {
                best_nice = nice;
                best_idx = i;
            }
        }
        drop(pm);

        self.ready_queue.remove(best_idx)
    }

    /// Full tick — used when preempting user-mode processes.
    /// Handles sleep wakeups, signal delivery, quantum counting, and preemption.
    pub fn tick(&mut self) {
        self.wake_sleepers();
        self.deliver_signals();

        self.quantum_counter += 1;
        if self.quantum_counter >= self.quantum {
            self.quantum_counter = 0;
            self.preempt();
            // After preempt, schedule the next user process
            self.schedule_user();
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

    /// Schedule next user process to run.
    /// Sets current_pid and quantum. The actual TrapFrame switch happens in
    /// timer_preempt_handler which reads the new current process's trap_frame.
    fn schedule_user(&mut self) {
        let next_pid = match self.next_process() {
            Some(pid) => pid,
            None => return,
        };

        let mut pm = PROCESS_MANAGER.lock();

        self.quantum_counter = 0;
        if let Some(next) = pm.get_process_mut(next_pid) {
            next.state = ProcessState::Running;
            self.quantum = quantum_from_nice(next.nice);
        }
        pm.current_pid = Some(next_pid);

        drop(pm);
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
            self.quantum = quantum_from_nice(next.nice);
        }
        pm.current_pid = Some(next_pid);

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
