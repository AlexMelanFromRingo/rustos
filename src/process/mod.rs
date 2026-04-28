/// Process management and context switching
pub mod context;
pub mod scheduler;

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use crate::process::context::{Context, TrapFrame};

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

/// Process Control Block
#[derive(Debug)]
pub struct Process {
    pub pid: Pid,
    pub parent_pid: Option<Pid>,
    pub state: ProcessState,
    pub context: Context,
    pub stack: Vec<u8>,
    pub exit_code: Option<i32>,
    pub user_stack_addr: Option<u64>,
    pub user_stack_size: Option<u64>,
    pub entry_point: Option<u64>,
    pub nice: i8,
    pub pending_signals: u64,
    pub signal_blocked: u64,

    // === NEW: Preemptive multitasking fields ===

    /// Full trap frame saved when this user process is preempted by timer interrupt.
    /// None if process hasn't been preempted yet (or is a kernel process).
    pub trap_frame: TrapFrame,

    /// Whether this process has a valid trap frame to restore
    pub has_trap_frame: bool,

    /// Whether this is a user-mode process (Ring 3)
    pub is_user: bool,

    /// Kernel stack slot index (from KernelStackPool)
    pub kernel_stack_slot: Option<usize>,

    /// Kernel stack top address (for TSS.RSP0)
    pub kernel_stack_top: u64,

    /// Page table physical address (CR3 value) — 0 means use kernel page table
    pub cr3: u64,

    /// Current working directory (per-process, used by chdir/getcwd).
    pub cwd: String,

    /// Process group ID (defaults to pid).  Changed by setpgid/setpgrp.
    pub pgid: Pid,
    /// Session ID (defaults to pid).  Changed by setsid (and only there).
    pub sid: Pid,

    /// Custom signal handlers, indexed by signal number 1..=31.
    /// Entry == 0 (SIG_DFL) means default action; 1 (SIG_IGN) means ignore;
    /// any other value is the user-mode handler entry point.
    pub sigactions: [u64; 32],

    /// Effective Linux-style capabilities held by this process.
    /// Stored as a raw u64 bitmap.  See `crate::capability::Capability`.
    pub cap_effective: u64,
    /// Permitted set: maximum the process can ever raise effective to.
    pub cap_permitted: u64,

    /// CFS virtual runtime — accumulated normalised CPU time.  The
    /// scheduler always picks the runnable process with the smallest
    /// vruntime, which guarantees long-term fairness regardless of
    /// arrival order.  See `crate::process::scheduler::Scheduler::tick`
    /// for the increment formula.
    pub vruntime: u64,

    /// Per-thread storage base for FS segment.  Set by `arch_prctl(2)`
    /// with ARCH_SET_FS, written into IA32_FS_BASE on context switch.
    /// musl/glibc point this at the TCB so `mov %fs:0, %rax` reads
    /// thread-local storage in O(1).
    pub fs_base: u64,
    /// Per-thread storage base for GS segment (kernel uses GS for the
    /// per-CPU pointer; user-space gets it via ARCH_SET_GS).
    pub gs_base: u64,
}

impl Process {
    /// Create a new kernel-mode process
    pub fn new(pid: Pid, entry_point: usize, stack_size: usize) -> Self {
        let mut stack = Vec::with_capacity(stack_size);
        stack.resize(stack_size, 0);
        let stack_top = stack.as_ptr() as usize + stack_size;
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
            trap_frame: TrapFrame::empty(),
            has_trap_frame: false,
            is_user: false,
            kernel_stack_slot: None,
            kernel_stack_top: 0,
            cr3: 0,
            cwd: String::from("/"),
            pgid: pid,
            sid: pid,
            sigactions: [0u64; 32],
            cap_effective: u64::MAX, // kernel processes default to full caps
            cap_permitted: u64::MAX,
            vruntime: 0,
            fs_base: 0,
            gs_base: 0,
        }
    }

    /// Create a user-mode process with pre-initialized trap frame.
    /// The trap frame is set up so IRETQ lands at `entry_point` in Ring 3.
    pub fn new_user(pid: Pid, parent_pid: Option<Pid>, entry_point: u64,
                    user_stack_top: u64) -> Self {
        // Allocate a per-process kernel stack
        let (kstack_top, kstack_slot) = crate::gdt::allocate_kernel_stack()
            .expect("out of kernel stacks");

        let trap = TrapFrame::new_user(
            entry_point,
            user_stack_top,
            crate::gdt::user_cs_value(),
            crate::gdt::user_ss_value(),
        );

        Process {
            pid,
            parent_pid,
            state: ProcessState::Ready,
            context: Context::default(),
            stack: Vec::new(),
            exit_code: None,
            user_stack_addr: Some(user_stack_top),
            user_stack_size: None,
            entry_point: Some(entry_point),
            nice: 0,
            pending_signals: 0,
            signal_blocked: 0,
            trap_frame: trap,
            has_trap_frame: true,
            is_user: true,
            kernel_stack_slot: Some(kstack_slot),
            kernel_stack_top: kstack_top,
            cr3: 0, // 0 = use kernel page table (shared address space for now)
            cwd: String::from("/"),
            pgid: pid,
            sid: pid,
            sigactions: [0u64; 32],
            cap_effective: u64::MAX, // kernel processes default to full caps
            cap_permitted: u64::MAX,
            vruntime: 0,
            fs_base: 0,
            gs_base: 0,
        }
    }

    /// Clone this process (for fork)
    pub fn clone(&self, new_pid: Pid) -> Self {
        let (kstack_top, kstack_slot) = if self.is_user {
            crate::gdt::allocate_kernel_stack()
                .map(|(top, slot)| (top, Some(slot)))
                .unwrap_or((0, None))
        } else {
            (0, None)
        };

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
            pending_signals: 0,
            signal_blocked: self.signal_blocked,
            trap_frame: self.trap_frame,
            has_trap_frame: self.has_trap_frame,
            is_user: self.is_user,
            kernel_stack_slot: kstack_slot,
            kernel_stack_top: kstack_top,
            cr3: 0, // TODO: clone page table for fork
            cwd: self.cwd.clone(),
            pgid: self.pgid, // child inherits parent's group
            sid: self.sid,   // and session
            sigactions: self.sigactions, // and signal handlers
            cap_effective: self.cap_effective, // and capability sets
            cap_permitted: self.cap_permitted,
            vruntime: self.vruntime,
            fs_base: self.fs_base,
            gs_base: self.gs_base,
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

    pub fn is_valid(sig: u32) -> bool {
        sig >= 1 && sig <= 31
    }

    pub fn name(sig: u32) -> &'static str {
        match sig {
            1 => "SIGHUP", 2 => "SIGINT", 3 => "SIGQUIT", 4 => "SIGILL",
            5 => "SIGTRAP", 6 => "SIGABRT", 7 => "SIGBUS", 8 => "SIGFPE",
            9 => "SIGKILL", 10 => "SIGUSR1", 11 => "SIGSEGV", 12 => "SIGUSR2",
            13 => "SIGPIPE", 14 => "SIGALRM", 15 => "SIGTERM",
            17 => "SIGCHLD", 18 => "SIGCONT", 19 => "SIGSTOP", 20 => "SIGTSTP",
            _ => "UNKNOWN",
        }
    }

    pub enum DefaultAction {
        Terminate,
        Ignore,
        Stop,
        Continue,
    }

    pub fn default_action(sig: u32) -> DefaultAction {
        match sig {
            SIGCHLD => DefaultAction::Ignore,
            SIGCONT => DefaultAction::Continue,
            SIGSTOP | SIGTSTP => DefaultAction::Stop,
            _ => DefaultAction::Terminate,
        }
    }
}

/// Process Manager
pub struct ProcessManager {
    processes: Vec<Process>,
    pub current_pid: Option<Pid>,
    pub next_pid: Pid,
}

impl ProcessManager {
    pub const fn new() -> Self {
        ProcessManager {
            processes: Vec::new(),
            current_pid: None,
            next_pid: 1, // PID 0 reserved for idle/kernel
        }
    }

    pub fn create_process(&mut self, entry_point: usize, stack_size: usize) -> Pid {
        let pid = self.next_pid;
        self.next_pid += 1;
        let process = Process::new(pid, entry_point, stack_size);
        self.processes.push(process);
        crate::syscall::filedesc::register_process(pid, crate::syscall::filedesc::FileDescriptorTable::with_stdio());
        pid
    }

    /// Create a user-mode process ready for preemptive scheduling
    pub fn create_user_process(&mut self, entry_point: u64, user_stack_top: u64,
                                parent_pid: Option<Pid>) -> Pid {
        let pid = self.next_pid;
        self.next_pid += 1;
        let process = Process::new_user(pid, parent_pid, entry_point, user_stack_top);
        self.processes.push(process);
        crate::syscall::filedesc::register_process(pid, crate::syscall::filedesc::FileDescriptorTable::with_stdio());
        pid
    }

    /// Insert a pre-built process into the process table.
    pub fn insert_process(&mut self, process: Process) {
        self.processes.push(process);
    }

    pub fn get_process_mut(&mut self, pid: Pid) -> Option<&mut Process> {
        self.processes.iter_mut().find(|p| p.pid == pid)
    }

    pub fn current_process(&self) -> Option<&Process> {
        self.current_pid
            .and_then(|pid| self.processes.iter().find(|p| p.pid == pid))
    }

    pub fn current_process_mut(&mut self) -> Option<&mut Process> {
        self.current_pid
            .and_then(|pid| self.processes.iter_mut().find(|p| p.pid == pid))
    }

    pub fn set_current(&mut self, pid: Pid) {
        if let Some(process) = self.get_process_mut(pid) {
            process.state = ProcessState::Running;
            self.current_pid = Some(pid);
        }
    }

    pub fn processes(&self) -> &[Process] {
        &self.processes
    }

    pub fn ready_processes(&self) -> Vec<Pid> {
        self.processes
            .iter()
            .filter(|p| p.state == ProcessState::Ready)
            .map(|p| p.pid)
            .collect()
    }

    pub fn terminate(&mut self, pid: Pid) {
        // Free kernel stack if allocated
        if let Some(process) = self.get_process_mut(pid) {
            process.state = ProcessState::Terminated;
            if let Some(slot) = process.kernel_stack_slot.take() {
                crate::gdt::free_kernel_stack(slot);
            }
        }
        if self.current_pid == Some(pid) {
            self.current_pid = None;
        }
    }

    pub fn all_processes(&self) -> &Vec<Process> {
        &self.processes
    }

    pub fn get_process(&self, pid: Pid) -> Option<&Process> {
        self.processes.iter().find(|p| p.pid == pid)
    }

    pub fn send_signal(&mut self, pid: Pid, sig: u32) -> Result<(), &'static str> {
        if !signal::is_valid(sig) {
            return Err("invalid signal number");
        }

        let process = self.get_process_mut(pid)
            .ok_or("process not found")?;

        if sig == 0 {
            return Ok(());
        }

        if process.state == ProcessState::Terminated {
            return Err("process already terminated");
        }

        if sig == signal::SIGKILL || sig == signal::SIGSTOP {
            match signal::default_action(sig) {
                signal::DefaultAction::Terminate => {
                    process.state = ProcessState::Terminated;
                    process.exit_code = Some(128 + sig as i32);
                    if let Some(slot) = process.kernel_stack_slot.take() {
                        crate::gdt::free_kernel_stack(slot);
                    }
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

        process.pending_signals |= 1u64 << sig;
        Ok(())
    }

    pub fn deliver_pending_signals(&mut self, pid: Pid) -> bool {
        let (pending, blocked) = {
            let process = match self.get_process(pid) {
                Some(p) => p,
                None => return false,
            };
            (process.pending_signals, process.signal_blocked)
        };

        let deliverable = pending & !blocked;
        if deliverable == 0 {
            return false;
        }

        let mut terminated = false;

        for sig in 1..=31u32 {
            if deliverable & (1u64 << sig) != 0 {
                if let Some(process) = self.get_process_mut(pid) {
                    process.pending_signals &= !(1u64 << sig);
                }

                match signal::default_action(sig) {
                    signal::DefaultAction::Terminate => {
                        if let Some(process) = self.get_process_mut(pid) {
                            process.state = ProcessState::Terminated;
                            process.exit_code = Some(128 + sig as i32);
                            if let Some(slot) = process.kernel_stack_slot.take() {
                                crate::gdt::free_kernel_stack(slot);
                            }
                        }
                        if self.current_pid == Some(pid) {
                            self.current_pid = None;
                        }
                        terminated = true;
                        break;
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

    pub fn fork(&mut self, parent_pid: Pid) -> Result<Pid, &'static str> {
        let child_pid = self.next_pid;
        self.next_pid += 1;

        let child = {
            let parent = self.get_process(parent_pid)
                .ok_or("Parent process not found")?;
            parent.clone(child_pid)
        };

        self.processes.push(child);
        // Clone parent's FD table for the child
        crate::syscall::filedesc::clone_process_fds(parent_pid, child_pid);
        Ok(child_pid)
    }

    pub fn exec(&mut self, pid: Pid, entry_point: u64, stack_addr: u64, stack_size: u64) -> Result<(), &'static str> {
        let process = self.get_process_mut(pid)
            .ok_or("Process not found")?;

        process.entry_point = Some(entry_point);
        process.user_stack_addr = Some(stack_addr);
        process.user_stack_size = Some(stack_size);
        process.context = Context::default();
        process.state = ProcessState::Ready;

        Ok(())
    }

    pub fn exit(&mut self, pid: Pid, exit_code: i32) {
        let parent_pid = self.get_process(pid).and_then(|p| p.parent_pid);

        if let Some(process) = self.get_process_mut(pid) {
            process.exit_code = Some(exit_code);
            process.state = ProcessState::Zombie;
            if let Some(slot) = process.kernel_stack_slot.take() {
                crate::gdt::free_kernel_stack(slot);
            }
        }

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

    pub fn wait(&mut self, parent_pid: Pid) -> Option<(Pid, i32)> {
        let child = self.processes.iter()
            .find(|p| p.parent_pid == Some(parent_pid) && p.state == ProcessState::Zombie)
            .map(|p| (p.pid, p.exit_code.unwrap_or(-1)));

        if let Some((child_pid, exit_code)) = child {
            self.processes.retain(|p| p.pid != child_pid);
            crate::syscall::filedesc::unregister_process(child_pid);
            Some((child_pid, exit_code))
        } else {
            let has_children = self.processes.iter()
                .any(|p| p.parent_pid == Some(parent_pid) && p.state != ProcessState::Terminated);

            if has_children {
                if let Some(parent) = self.get_process_mut(parent_pid) {
                    parent.state = ProcessState::Waiting;
                }
            }

            None
        }
    }

    /// Remove all terminated and zombie processes from the process list.
    /// Called after preemptive tests to clean up stale entries.
    pub fn cleanup_dead_processes(&mut self) {
        // Collect PIDs of dead processes before removing them
        let dead_pids: Vec<Pid> = self.processes.iter()
            .filter(|p| p.state == ProcessState::Terminated || p.state == ProcessState::Zombie)
            .map(|p| p.pid)
            .collect();
        for pid in &dead_pids {
            crate::syscall::filedesc::unregister_process(*pid);
        }
        self.processes.retain(|p| {
            p.state != ProcessState::Terminated && p.state != ProcessState::Zombie
        });
        self.current_pid = None;
    }

    pub fn get_children(&self, parent_pid: Pid) -> Vec<Pid> {
        self.processes.iter()
            .filter(|p| p.parent_pid == Some(parent_pid))
            .map(|p| p.pid)
            .collect()
    }
}

/// Global process manager
pub static PROCESS_MANAGER: Mutex<ProcessManager> = Mutex::new(ProcessManager::new());

/// Fallback cwd used by `chdir`/`getcwd` when no user process is running
/// (e.g. when called from the shell, which is currently a kernel task).
pub static SHELL_CWD: Mutex<String> = Mutex::new(String::new());

/// Resolve the current working directory: prefer the running process'
/// `cwd`, fall back to `SHELL_CWD`, default to "/".
pub fn current_cwd() -> String {
    let pm = PROCESS_MANAGER.lock();
    if let Some(p) = pm.current_process() {
        if !p.cwd.is_empty() {
            return p.cwd.clone();
        }
    }
    drop(pm);
    let shell = SHELL_CWD.lock();
    if !shell.is_empty() {
        shell.clone()
    } else {
        String::from("/")
    }
}

/// Set the current working directory.  Updates the running process if any,
/// otherwise the shell-fallback slot.
pub fn set_current_cwd(path: &str) {
    let mut pm = PROCESS_MANAGER.lock();
    if let Some(p) = pm.current_process_mut() {
        p.cwd = String::from(path);
        return;
    }
    drop(pm);
    *SHELL_CWD.lock() = String::from(path);
}
