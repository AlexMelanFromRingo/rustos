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
    Waiting,     // Waiting for child process
    Zombie,      // Terminated but not reaped
    Terminated,
}

impl ProcessState {
    /// Get human-readable state string
    pub fn as_str(&self) -> &'static str {
        match self {
            ProcessState::Ready => "READY",
            ProcessState::Running => "RUNNING",
            ProcessState::Blocked => "BLOCKED",
            ProcessState::Waiting => "WAITING",
            ProcessState::Zombie => "ZOMBIE",
            ProcessState::Terminated => "DEAD",
        }
    }
}

/// Process Control Block - contains all information about a process
#[derive(Debug)]
pub struct Process {
    pub pid: Pid,
    pub parent_pid: Option<Pid>,
    pub state: ProcessState,
    pub context: Context,
    pub stack: Vec<u8>,
    pub exit_code: Option<i32>,
    pub user_stack_addr: Option<u64>,    // User space stack address
    pub user_stack_size: Option<u64>,    // User space stack size
    pub entry_point: Option<u64>,        // Entry point for exec
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
            parent_pid: None,
            state: ProcessState::Ready,
            context,
            stack,
            exit_code: None,
            user_stack_addr: None,
            user_stack_size: None,
            entry_point: Some(entry_point as u64),
        }
    }

    /// Create a user space process with ELF entry point
    pub fn new_user(pid: Pid, parent_pid: Option<Pid>, entry_point: u64,
                    stack_addr: u64, stack_size: u64) -> Self {
        Process {
            pid,
            parent_pid,
            state: ProcessState::Ready,
            context: Context::default(),
            stack: Vec::new(),  // User processes use identity-mapped stack
            exit_code: None,
            user_stack_addr: Some(stack_addr),
            user_stack_size: Some(stack_size),
            entry_point: Some(entry_point),
        }
    }

    /// Clone this process (for fork)
    pub fn clone(&self, new_pid: Pid) -> Self {
        Process {
            pid: new_pid,
            parent_pid: Some(self.pid),
            state: ProcessState::Ready,
            context: self.context,
            stack: self.stack.clone(),
            exit_code: None,
            user_stack_addr: self.user_stack_addr,
            user_stack_size: self.user_stack_size,
            entry_point: self.entry_point,
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

    /// Get all processes (for ps command)
    pub fn processes(&self) -> &[Process] {
        &self.processes
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

    /// Fork current process - create a copy
    pub fn fork(&mut self, parent_pid: Pid) -> Result<Pid, &'static str> {
        // Create new PID first
        let child_pid = self.next_pid;
        self.next_pid += 1;

        // Get parent process and clone it
        let child = {
            let parent = self.get_process(parent_pid)
                .ok_or("Parent process not found")?;
            parent.clone(child_pid)
        };

        // Add to process list
        self.processes.push(child);

        Ok(child_pid)
    }

    /// Execute new program in process (replaces current program)
    pub fn exec(&mut self, pid: Pid, entry_point: u64, stack_addr: u64, stack_size: u64) -> Result<(), &'static str> {
        let process = self.get_process_mut(pid)
            .ok_or("Process not found")?;

        // Replace entry point and stack
        process.entry_point = Some(entry_point);
        process.user_stack_addr = Some(stack_addr);
        process.user_stack_size = Some(stack_size);

        // Reset context (will be set up when process runs)
        process.context = Context::default();
        process.state = ProcessState::Ready;

        Ok(())
    }

    /// Exit current process with exit code
    pub fn exit(&mut self, pid: Pid, exit_code: i32) {
        // Get parent PID before borrowing process mutably
        let parent_pid = self.get_process(pid).and_then(|p| p.parent_pid);

        // Set exit code and become zombie
        if let Some(process) = self.get_process_mut(pid) {
            process.exit_code = Some(exit_code);
            process.state = ProcessState::Zombie;
        }

        // Wake up parent if it's waiting
        if let Some(parent_pid) = parent_pid {
            if let Some(parent) = self.get_process_mut(parent_pid) {
                if parent.state == ProcessState::Waiting {
                    parent.state = ProcessState::Ready;
                }
            }
        }

        if self.current_pid == Some(pid) {
            self.current_pid = None;
        }
    }

    /// Wait for child process to exit
    pub fn wait(&mut self, parent_pid: Pid) -> Option<(Pid, i32)> {
        // Find any zombie child
        let child = self.processes.iter()
            .find(|p| p.parent_pid == Some(parent_pid) && p.state == ProcessState::Zombie)
            .map(|p| (p.pid, p.exit_code.unwrap_or(-1)));

        if let Some((child_pid, exit_code)) = child {
            // Reap the zombie
            self.processes.retain(|p| p.pid != child_pid);
            Some((child_pid, exit_code))
        } else {
            // No zombie children, check if any children are still running
            let has_children = self.processes.iter()
                .any(|p| p.parent_pid == Some(parent_pid) && p.state != ProcessState::Terminated);

            if has_children {
                // Mark parent as waiting
                if let Some(parent) = self.get_process_mut(parent_pid) {
                    parent.state = ProcessState::Waiting;
                }
            }

            None
        }
    }

    /// Get all children of a process
    pub fn get_children(&self, parent_pid: Pid) -> Vec<Pid> {
        self.processes.iter()
            .filter(|p| p.parent_pid == Some(parent_pid))
            .map(|p| p.pid)
            .collect()
    }
}

/// Global process manager
pub static PROCESS_MANAGER: Mutex<ProcessManager> = Mutex::new(ProcessManager::new());
