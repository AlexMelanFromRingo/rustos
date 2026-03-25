/// Process scheduler
///
/// Implements a priority-based round-robin scheduler with sleep queues and
/// timer-based wakeups. Processes with lower nice values get more CPU time.

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

/// A sleeping process that should be woken at a specific tick
struct SleepEntry {
    pid: Pid,
    wake_tick: u64,
}

/// Scheduler state
pub struct Scheduler {
    /// Queue of ready processes (PIDs)
    ready_queue: VecDeque<Pid>,
    /// Current quantum for the running process (in timer ticks)
    quantum: usize,
    /// Current quantum counter
    quantum_counter: usize,
    /// Processes sleeping until a specific timer tick
    sleep_queue: Vec<SleepEntry>,
}

/// Calculate time quantum from nice value.
/// nice -20 → MAX_QUANTUM (40 ticks), nice 0 → BASE_QUANTUM (10), nice 19 → MIN_QUANTUM (2)
fn quantum_from_nice(nice: i8) -> usize {
    // Linear interpolation: quantum = BASE_QUANTUM - nice * scale
    // Scale so that nice -20 gives MAX_QUANTUM and nice 19 gives MIN_QUANTUM
    let q = BASE_QUANTUM as i32 - (nice as i32 * (MAX_QUANTUM as i32 - MIN_QUANTUM as i32) / 39);
    (q as usize).clamp(MIN_QUANTUM, MAX_QUANTUM)
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

    /// Add a process to the ready queue
    pub fn enqueue(&mut self, pid: Pid) {
        if !self.ready_queue.contains(&pid) {
            self.ready_queue.push_back(pid);
        }
    }

    /// Remove a process from the ready queue
    pub fn dequeue(&mut self, pid: Pid) {
        self.ready_queue.retain(|&p| p != pid);
    }

    /// Get the next process to run (priority-aware round-robin).
    /// Among processes at the same priority level, round-robin order is maintained.
    /// Picks the process with the lowest nice value (highest priority).
    fn next_process(&mut self) -> Option<Pid> {
        if self.ready_queue.is_empty() {
            return None;
        }

        // Find the index of the highest-priority process
        let pm = PROCESS_MANAGER.lock();
        let mut best_idx = 0;
        let mut best_nice: i8 = 19; // Lowest priority

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

    /// Called on each timer tick
    pub fn tick(&mut self) {
        // Wake any sleeping processes whose timer has expired
        self.wake_sleepers();

        self.quantum_counter += 1;
        if self.quantum_counter >= self.quantum {
            self.quantum_counter = 0;
            // Time quantum expired, trigger reschedule
            self.preempt();
        }
    }

    /// Check sleep queue and wake processes whose sleep time has elapsed
    fn wake_sleepers(&mut self) {
        let now = current_ticks();
        let mut i = 0;
        while i < self.sleep_queue.len() {
            if now >= self.sleep_queue[i].wake_tick {
                let entry = self.sleep_queue.swap_remove(i);
                // Unblock the process
                let mut pm = PROCESS_MANAGER.lock();
                if let Some(process) = pm.get_process_mut(entry.pid) {
                    if process.state == ProcessState::Blocked {
                        process.state = ProcessState::Ready;
                        self.enqueue(entry.pid);
                    }
                }
                drop(pm);
                // Don't increment i — swap_remove moved last element to current position
            } else {
                i += 1;
            }
        }
    }

    /// Put the current process to sleep for the specified number of timer ticks.
    /// Returns the PID that was put to sleep, or None if no current process.
    pub fn sleep_ticks(&mut self, ticks: u64) -> Option<Pid> {
        let mut pm = PROCESS_MANAGER.lock();

        let current_pid = pm.current_pid?;
        let process = pm.get_process_mut(current_pid)?;
        process.state = ProcessState::Blocked;
        pm.current_pid = None;

        drop(pm);

        let wake_tick = current_ticks() + ticks;
        self.sleep_queue.push(SleepEntry {
            pid: current_pid,
            wake_tick,
        });

        Some(current_pid)
    }

    /// Preempt current process (called on timer tick when quantum expires)
    fn preempt(&mut self) {
        let mut pm = PROCESS_MANAGER.lock();

        if let Some(current_pid) = pm.current_pid {
            if let Some(current) = pm.get_process_mut(current_pid) {
                // Only preempt if still running
                if current.state == ProcessState::Running {
                    current.state = ProcessState::Ready;
                    self.enqueue(current_pid);
                }
            }
            pm.current_pid = None;
        }

        drop(pm);

        // Schedule next process
        // Note: actual context switch will happen in schedule()
    }

    /// Schedule the next process to run
    ///
    /// This performs the actual context switch
    pub fn schedule(&mut self) {
        let mut pm = PROCESS_MANAGER.lock();

        // Get current process
        let current_pid = pm.current_pid;

        // Get next process from ready queue
        let next_pid = match self.next_process() {
            Some(pid) => pid,
            None => {
                // No ready processes, keep current or idle
                drop(pm);
                return;
            }
        };

        // If next process is the same as current, just continue
        if Some(next_pid) == current_pid {
            // Re-enqueue for fairness
            self.enqueue(next_pid);
            drop(pm);
            return;
        }

        // Reset quantum counter and set quantum based on process priority
        self.quantum_counter = 0;

        // Set next process as running and calculate its quantum
        if let Some(next) = pm.get_process_mut(next_pid) {
            next.state = ProcessState::Running;
            self.quantum = quantum_from_nice(next.nice);
        }
        pm.current_pid = Some(next_pid);

        // Perform context switch if there's a current process
        if let Some(curr_pid) = current_pid {
            // Get pointers to contexts
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

    /// Yield CPU to another process (cooperative scheduling)
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

    /// Block current process
    pub fn block(&mut self) {
        let mut pm = PROCESS_MANAGER.lock();

        if let Some(current_pid) = pm.current_pid {
            if let Some(current) = pm.get_process_mut(current_pid) {
                current.state = ProcessState::Blocked;
                // Don't enqueue - blocked processes aren't ready
            }
            pm.current_pid = None;
        }

        drop(pm);
        self.schedule();
    }

    /// Unblock a process and make it ready
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

    /// Get number of ready processes
    pub fn ready_count(&self) -> usize {
        self.ready_queue.len()
    }

    /// Get list of all PIDs in ready queue
    pub fn ready_list(&self) -> alloc::vec::Vec<Pid> {
        self.ready_queue.iter().copied().collect()
    }
}

/// Global scheduler
pub static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());

/// Initialize the scheduler
pub fn init() {
    // Scheduler is already initialized as a static
    // This function can be used for future initialization logic
}

/// Yield CPU to another process
pub fn yield_cpu() {
    SCHEDULER.lock().yield_cpu();
}

/// Block current process
pub fn block() {
    SCHEDULER.lock().block();
}

/// Unblock a process
pub fn unblock(pid: Pid) {
    SCHEDULER.lock().unblock(pid);
}

/// Schedule next process (can be called from interrupt handler)
pub fn schedule() {
    SCHEDULER.lock().schedule();
}

/// Called on each timer tick
pub fn tick() {
    SCHEDULER.lock().tick();
}

/// Spawn a new process with the given entry point
pub fn spawn(entry_point: usize, stack_size: usize) -> Pid {
    let mut pm = PROCESS_MANAGER.lock();
    let pid = pm.create_process(entry_point, stack_size);
    drop(pm);

    // Add to scheduler's ready queue
    SCHEDULER.lock().enqueue(pid);

    pid
}

/// Terminate a process
pub fn terminate(pid: Pid) {
    // Remove from ready queue
    SCHEDULER.lock().dequeue(pid);

    // Mark as terminated
    PROCESS_MANAGER.lock().terminate(pid);
}

/// Sleep current process for given number of ticks
pub fn sleep(ticks: u64) {
    SCHEDULER.lock().sleep_ticks(ticks);
    schedule();
}

/// Set nice value for a process (returns old nice value)
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

/// Get nice value for a process
pub fn get_nice(pid: Pid) -> Option<i8> {
    let pm = PROCESS_MANAGER.lock();
    pm.get_process(pid).map(|p| p.nice)
}

/// Terminate current process
pub fn exit() {
    let pid = PROCESS_MANAGER.lock().current_pid;
    if let Some(pid) = pid {
        terminate(pid);
        schedule(); // Schedule next process
    }
}
