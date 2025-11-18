/// Process management and context switching
pub mod context;
pub mod scheduler;

use alloc::vec::Vec;
use spin::Mutex;
use crate::process::context::Context;

/// Process ID type
pub type Pid = usize;

/// Process state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Ready,
    Running,
    Blocked,
    Terminated,
}

/// Process Control Block - contains all information about a process
#[derive(Debug)]
pub struct Process {
    pub pid: Pid,
    pub state: ProcessState,
    pub context: Context,
    pub stack: Vec<u8>,
}

impl Process {
    /// Create a new process with the given entry point and stack size
    pub fn new(pid: Pid, entry_point: usize, stack_size: usize) -> Self {
        let mut stack = Vec::with_capacity(stack_size);
        stack.resize(stack_size, 0);

        // Get stack top (stack grows downward)
        let stack_top = stack.as_ptr() as usize + stack_size;

        // Initialize context with entry point and stack
        let context = Context::new(entry_point, stack_top);

        Process {
            pid,
            state: ProcessState::Ready,
            context,
            stack,
        }
    }
}

/// Process Manager - manages all processes
pub struct ProcessManager {
    processes: Vec<Process>,
    pub current_pid: Option<Pid>,
    next_pid: Pid,
}

impl ProcessManager {
    pub const fn new() -> Self {
        ProcessManager {
            processes: Vec::new(),
            current_pid: None,
            next_pid: 0,
        }
    }

    /// Create a new process
    pub fn create_process(&mut self, entry_point: usize, stack_size: usize) -> Pid {
        let pid = self.next_pid;
        self.next_pid += 1;

        let process = Process::new(pid, entry_point, stack_size);
        self.processes.push(process);

        pid
    }

    /// Get mutable reference to a process by PID
    pub fn get_process_mut(&mut self, pid: Pid) -> Option<&mut Process> {
        self.processes.iter_mut().find(|p| p.pid == pid)
    }

    /// Get reference to current process
    pub fn current_process(&self) -> Option<&Process> {
        self.current_pid
            .and_then(|pid| self.processes.iter().find(|p| p.pid == pid))
    }

    /// Get mutable reference to current process
    pub fn current_process_mut(&mut self) -> Option<&mut Process> {
        self.current_pid
            .and_then(|pid| self.processes.iter_mut().find(|p| p.pid == pid))
    }

    /// Set current process
    pub fn set_current(&mut self, pid: Pid) {
        if let Some(process) = self.get_process_mut(pid) {
            process.state = ProcessState::Running;
            self.current_pid = Some(pid);
        }
    }

    /// Get list of ready processes
    pub fn ready_processes(&self) -> Vec<Pid> {
        self.processes
            .iter()
            .filter(|p| p.state == ProcessState::Ready)
            .map(|p| p.pid)
            .collect()
    }

    /// Terminate a process
    pub fn terminate(&mut self, pid: Pid) {
        if let Some(process) = self.get_process_mut(pid) {
            process.state = ProcessState::Terminated;
        }
        if self.current_pid == Some(pid) {
            self.current_pid = None;
        }
    }

    /// Get all processes
    pub fn all_processes(&self) -> &Vec<Process> {
        &self.processes
    }

    /// Get a process by PID
    pub fn get_process(&self, pid: Pid) -> Option<&Process> {
        self.processes.iter().find(|p| p.pid == pid)
    }
}

/// Global process manager
pub static PROCESS_MANAGER: Mutex<ProcessManager> = Mutex::new(ProcessManager::new());
