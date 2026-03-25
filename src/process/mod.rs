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
    pub nice: i8,                        // Nice value: -20 (highest priority) to 19 (lowest)
    pub pending_signals: u64,            // Bitmask of pending signals (bit N = signal N)
    pub signal_blocked: u64,             // Bitmask of blocked signals
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
            nice: 0,
            pending_signals: 0,
            signal_blocked: 0,
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
            nice: 0,
            pending_signals: 0,
            signal_blocked: 0,
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
            nice: self.nice,
            pending_signals: 0,      // Child starts with no pending signals
            signal_blocked: self.signal_blocked,  // Inherit signal mask
        }
    }
}

/// Standard POSIX signal numbers
pub mod signal {
    pub const SIGHUP: u32 = 1;
    pub const SIGINT: u32 = 2;
    pub const SIGQUIT: u32 = 3;
    pub const SIGILL: u32 = 4;
    pub const SIGTRAP: u32 = 5;
    pub const SIGABRT: u32 = 6;
    pub const SIGBUS: u32 = 7;
    pub const SIGFPE: u32 = 8;
    pub const SIGKILL: u32 = 9;
    pub const SIGUSR1: u32 = 10;
    pub const SIGSEGV: u32 = 11;
    pub const SIGUSR2: u32 = 12;
    pub const SIGPIPE: u32 = 13;
    pub const SIGALRM: u32 = 14;
    pub const SIGTERM: u32 = 15;
    pub const SIGCHLD: u32 = 17;
    pub const SIGCONT: u32 = 18;
    pub const SIGSTOP: u32 = 19;
    pub const SIGTSTP: u32 = 20;

    /// Check if a signal number is valid (1-31)
    pub fn is_valid(sig: u32) -> bool {
        sig >= 1 && sig <= 31
    }

    /// Get signal name
    pub fn name(sig: u32) -> &'static str {
        match sig {
            1 => "SIGHUP",
            2 => "SIGINT",
            3 => "SIGQUIT",
            4 => "SIGILL",
            5 => "SIGTRAP",
            6 => "SIGABRT",
            7 => "SIGBUS",
            8 => "SIGFPE",
            9 => "SIGKILL",
            10 => "SIGUSR1",
            11 => "SIGSEGV",
            12 => "SIGUSR2",
            13 => "SIGPIPE",
            14 => "SIGALRM",
            15 => "SIGTERM",
            17 => "SIGCHLD",
            18 => "SIGCONT",
            19 => "SIGSTOP",
            20 => "SIGTSTP",
            _ => "UNKNOWN",
        }
    }

    /// Default action for a signal
    pub enum DefaultAction {
        Terminate,
        Ignore,
        Stop,
        Continue,
    }

    /// Get default action for a signal
    pub fn default_action(sig: u32) -> DefaultAction {
        match sig {
            SIGCHLD => DefaultAction::Ignore,
            SIGCONT => DefaultAction::Continue,
            SIGSTOP | SIGTSTP => DefaultAction::Stop,
            _ => DefaultAction::Terminate,
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

    /// Send a signal to a process
    pub fn send_signal(&mut self, pid: Pid, sig: u32) -> Result<(), &'static str> {
        if !signal::is_valid(sig) {
            return Err("invalid signal number");
        }

        let process = self.get_process_mut(pid)
            .ok_or("process not found")?;

        // Can't signal terminated/zombie processes (except to check existence with sig 0)
        if sig == 0 {
            return Ok(()); // Just checking if process exists
        }

        if process.state == ProcessState::Terminated {
            return Err("process already terminated");
        }

        // SIGKILL and SIGSTOP cannot be blocked
        if sig == signal::SIGKILL || sig == signal::SIGSTOP {
            // Deliver immediately
            match signal::default_action(sig) {
                signal::DefaultAction::Terminate => {
                    process.state = ProcessState::Terminated;
                    process.exit_code = Some(128 + sig as i32);
                    if self.current_pid == Some(pid) {
                        self.current_pid = None;
                    }
                }
                signal::DefaultAction::Stop => {
                    process.state = ProcessState::Blocked;
                }
                _ => {}
            }
            return Ok(());
        }

        // Set pending signal bit
        process.pending_signals |= 1u64 << sig;
        Ok(())
    }

    /// Deliver pending signals for a process. Returns true if process was terminated.
    pub fn deliver_pending_signals(&mut self, pid: Pid) -> bool {
        let (pending, blocked) = {
            let process = match self.get_process(pid) {
                Some(p) => p,
                None => return false,
            };
            (process.pending_signals, process.signal_blocked)
        };

        // Find deliverable signals (pending & ~blocked)
        let deliverable = pending & !blocked;
        if deliverable == 0 {
            return false;
        }

        let mut terminated = false;

        for sig in 1..=31u32 {
            if deliverable & (1u64 << sig) != 0 {
                // Clear the pending bit
                if let Some(process) = self.get_process_mut(pid) {
                    process.pending_signals &= !(1u64 << sig);
                }

                // Apply default action
                match signal::default_action(sig) {
                    signal::DefaultAction::Terminate => {
                        if let Some(process) = self.get_process_mut(pid) {
                            process.state = ProcessState::Terminated;
                            process.exit_code = Some(128 + sig as i32);
                        }
                        if self.current_pid == Some(pid) {
                            self.current_pid = None;
                        }
                        terminated = true;
                        break; // Process is dead, stop delivering
                    }
                    signal::DefaultAction::Stop => {
                        if let Some(process) = self.get_process_mut(pid) {
                            process.state = ProcessState::Blocked;
                        }
                    }
                    signal::DefaultAction::Continue => {
                        if let Some(process) = self.get_process_mut(pid) {
                            if process.state == ProcessState::Blocked {
                                process.state = ProcessState::Ready;
                            }
                        }
                    }
                    signal::DefaultAction::Ignore => {}
                }
            }
        }

        terminated
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
