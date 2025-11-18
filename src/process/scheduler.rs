/// Process scheduler
///
/// Implements a simple round-robin scheduler for cooperative and preemptive multitasking.

use alloc::collections::VecDeque;
use spin::Mutex;
use crate::process::{Pid, ProcessState, PROCESS_MANAGER};
use crate::process::context::switch_context;

/// Scheduler state
pub struct Scheduler {
    /// Queue of ready processes (PIDs)
    ready_queue: VecDeque<Pid>,
    /// Time quantum for round-robin (in timer ticks)
    quantum: usize,
    /// Current quantum counter
    quantum_counter: usize,
}

impl Scheduler {
    pub const fn new() -> Self {
        Scheduler {
            ready_queue: VecDeque::new(),
            quantum: 10, // 10 timer ticks per process
            quantum_counter: 0,
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

    /// Get the next process to run (round-robin)
    fn next_process(&mut self) -> Option<Pid> {
        self.ready_queue.pop_front()
    }

    /// Called on each timer tick
    pub fn tick(&mut self) {
        self.quantum_counter += 1;
        if self.quantum_counter >= self.quantum {
            self.quantum_counter = 0;
            // Time quantum expired, trigger reschedule
            self.preempt();
        }
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

        // Reset quantum counter for new process
        self.quantum_counter = 0;

        // Set next process as running
        if let Some(next) = pm.get_process_mut(next_pid) {
            next.state = ProcessState::Running;
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

/// Terminate current process
pub fn exit() {
    let pid = PROCESS_MANAGER.lock().current_pid;
    if let Some(pid) = pid {
        terminate(pid);
        schedule(); // Schedule next process
    }
}
