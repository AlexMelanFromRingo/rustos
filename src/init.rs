//! Init / service manager
//!
//! A simplified SysV-init / OpenRC-inspired service manager.  Tracks the system
//! runlevel, the catalog of services, and the desired vs. actual state of each.
//!
//! Services are described declaratively (name, exec hint, dependencies, restart
//! policy) and can be started, stopped, restarted, and queried.  The actual
//! per-tick work for each service is provided by the kernel: services are
//! lightweight ticker callbacks run from a service-tick loop, not independent
//! Unix processes.  This matches the current single-address-space kernel; once
//! per-process page tables and `fork()` land, this layer can be retargeted at
//! real user-mode processes with minimal disruption.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

/// Standard SysV-init runlevels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunLevel {
    Halt = 0,
    SingleUser = 1,
    MultiUserNoNet = 2,
    MultiUser = 3,
    Reserved = 4,
    Graphical = 5,
    Reboot = 6,
}

impl RunLevel {
    pub fn from_u8(n: u8) -> Option<Self> {
        match n {
            0 => Some(RunLevel::Halt),
            1 => Some(RunLevel::SingleUser),
            2 => Some(RunLevel::MultiUserNoNet),
            3 => Some(RunLevel::MultiUser),
            4 => Some(RunLevel::Reserved),
            5 => Some(RunLevel::Graphical),
            6 => Some(RunLevel::Reboot),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            RunLevel::Halt => "0 (halt)",
            RunLevel::SingleUser => "1 (single-user)",
            RunLevel::MultiUserNoNet => "2 (multi-user, no network)",
            RunLevel::MultiUser => "3 (multi-user)",
            RunLevel::Reserved => "4 (reserved)",
            RunLevel::Graphical => "5 (graphical)",
            RunLevel::Reboot => "6 (reboot)",
        }
    }
}

/// What a service should do when it exits unexpectedly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    /// Never auto-restart.
    Never,
    /// Restart on failure only.
    OnFailure,
    /// Always restart.
    Always,
}

/// Lifecycle state of a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

impl ServiceState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceState::Stopped => "stopped",
            ServiceState::Starting => "starting",
            ServiceState::Running => "running",
            ServiceState::Stopping => "stopping",
            ServiceState::Failed => "failed",
        }
    }
}

/// A unit a service can declare it needs before starting.
#[derive(Debug, Clone)]
pub struct Service {
    pub name: String,
    pub description: String,
    /// Hint at the executable path / kernel routine name.  Currently informational.
    pub exec: String,
    /// Names of services that must already be Running before this one starts.
    pub requires: Vec<String>,
    /// Default runlevel mask: bit `n` set means start at runlevel n.
    pub runlevels: u8,
    pub restart: RestartPolicy,
    pub state: ServiceState,
    /// Tick at which this service entered its current state.
    pub state_since_tick: u64,
    /// Number of times this service has been (re)started.
    pub start_count: u32,
    /// PID of the backing kernel process, if any.
    pub pid: Option<usize>,
}

impl Service {
    pub fn new(name: &str, description: &str, exec: &str) -> Self {
        Service {
            name: name.to_string(),
            description: description.to_string(),
            exec: exec.to_string(),
            requires: Vec::new(),
            runlevels: 0b0011_1100, // levels 2,3,4,5 by default
            restart: RestartPolicy::OnFailure,
            state: ServiceState::Stopped,
            state_since_tick: 0,
            start_count: 0,
            pid: None,
        }
    }

    pub fn requires(mut self, dep: &str) -> Self {
        self.requires.push(dep.to_string());
        self
    }

    pub fn runlevels(mut self, mask: u8) -> Self {
        self.runlevels = mask;
        self
    }

    pub fn restart(mut self, policy: RestartPolicy) -> Self {
        self.restart = policy;
        self
    }

    /// Should this service be active at the given runlevel?
    pub fn enabled_at(&self, level: RunLevel) -> bool {
        let bit = level as u8;
        (self.runlevels & (1u8 << bit)) != 0
    }

    /// Pretty status line, similar to `systemctl status`.
    pub fn status_line(&self, current_tick: u64) -> String {
        let dur = current_tick.saturating_sub(self.state_since_tick) / 100;
        alloc::format!(
            "{:<12} [{:<8}] (started: {}, age: {}s)  {}",
            self.name,
            self.state.as_str(),
            self.start_count,
            dur,
            self.description
        )
    }
}

/// The service manager (analogous to PID 1).
pub struct InitSystem {
    services: BTreeMap<String, Service>,
    runlevel: RunLevel,
    /// Boot tick (0 unless captured early).
    pub boot_tick: u64,
}

impl InitSystem {
    pub const fn new() -> Self {
        InitSystem {
            services: BTreeMap::new(),
            runlevel: RunLevel::SingleUser,
            boot_tick: 0,
        }
    }

    pub fn register(&mut self, svc: Service) {
        self.services.insert(svc.name.clone(), svc);
    }

    pub fn get(&self, name: &str) -> Option<&Service> {
        self.services.get(name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Service> {
        self.services.get_mut(name)
    }

    pub fn services(&self) -> impl Iterator<Item = &Service> {
        self.services.values()
    }

    pub fn runlevel(&self) -> RunLevel {
        self.runlevel
    }

    /// Move to a new runlevel.  Stops services not enabled at the new level and
    /// starts services that are.
    pub fn set_runlevel(&mut self, level: RunLevel) -> Result<(), &'static str> {
        let prev = self.runlevel;
        self.runlevel = level;
        if prev == level {
            return Ok(());
        }

        // First pass: stop anything no longer enabled.
        let to_stop: Vec<String> = self.services.values()
            .filter(|s| s.state == ServiceState::Running && !s.enabled_at(level))
            .map(|s| s.name.clone())
            .collect();
        for name in to_stop {
            let _ = self.stop(&name);
        }

        // Second pass: start what should now be enabled.
        let to_start: Vec<String> = self.services.values()
            .filter(|s| s.state == ServiceState::Stopped && s.enabled_at(level))
            .map(|s| s.name.clone())
            .collect();
        for name in to_start {
            let _ = self.start(&name);
        }

        Ok(())
    }

    /// Start a service, recursively starting its dependencies first.
    pub fn start(&mut self, name: &str) -> Result<(), &'static str> {
        // Check existence first
        if !self.services.contains_key(name) {
            return Err("service not found");
        }

        // Start dependencies first
        let deps: Vec<String> = self.services[name].requires.clone();
        for dep in deps {
            let need_start = match self.services.get(&dep) {
                Some(d) => d.state != ServiceState::Running,
                None => return Err("missing dependency"),
            };
            if need_start {
                self.start(&dep)?;
            }
        }

        let svc = self.services.get_mut(name).unwrap();
        if svc.state == ServiceState::Running {
            return Ok(());
        }

        svc.state = ServiceState::Starting;
        svc.state_since_tick = crate::task::timer::current_ticks();
        svc.start_count += 1;

        // In a full system this is where we'd fork+exec.  For the in-kernel
        // service model, the service is just registered as Running.
        svc.state = ServiceState::Running;
        crate::klog_info!("init: started service '{}'", name);
        Ok(())
    }

    /// Stop a service.
    pub fn stop(&mut self, name: &str) -> Result<(), &'static str> {
        let svc = self.services.get_mut(name).ok_or("service not found")?;
        if svc.state == ServiceState::Stopped {
            return Ok(());
        }
        svc.state = ServiceState::Stopping;
        svc.state_since_tick = crate::task::timer::current_ticks();
        // Tear down would happen here.
        svc.state = ServiceState::Stopped;
        svc.pid = None;
        crate::klog_info!("init: stopped service '{}'", name);
        Ok(())
    }

    /// Restart a service.
    pub fn restart(&mut self, name: &str) -> Result<(), &'static str> {
        self.stop(name)?;
        self.start(name)
    }

    /// Mark a service Failed (called by the watchdog when a backing job dies).
    pub fn mark_failed(&mut self, name: &str) {
        if let Some(svc) = self.services.get_mut(name) {
            svc.state = ServiceState::Failed;
            svc.state_since_tick = crate::task::timer::current_ticks();
            crate::klog_warn!("init: service '{}' failed", name);
            if matches!(svc.restart, RestartPolicy::Always | RestartPolicy::OnFailure) {
                let name_owned = svc.name.clone();
                let _ = self.start(&name_owned);
            }
        }
    }

    /// Number of services currently running.
    pub fn running_count(&self) -> usize {
        self.services.values().filter(|s| s.state == ServiceState::Running).count()
    }

    /// Number of registered services.
    pub fn total_count(&self) -> usize {
        self.services.len()
    }
}

/// Global init system.
pub static INIT: Mutex<InitSystem> = Mutex::new(InitSystem::new());

/// Default service catalog — registered at boot.
pub fn install_default_services() {
    let mut init = INIT.lock();

    // Helper for the runlevel mask: levels 2, 3, 4, 5
    let multiuser = (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5);
    let all_levels = (1 << 1) | multiuser; // include single-user

    init.register(
        Service::new("klogd", "Kernel log forwarder", "/sbin/klogd")
            .runlevels(all_levels)
            .restart(RestartPolicy::Always),
    );
    init.register(
        Service::new("syslogd", "System log daemon", "/sbin/syslogd")
            .runlevels(multiuser)
            .restart(RestartPolicy::Always),
    );
    init.register(
        Service::new("cron", "Periodic command scheduler", "/usr/sbin/cron")
            .runlevels(multiuser)
            .restart(RestartPolicy::OnFailure),
    );
    init.register(
        Service::new("getty", "Console login (tty0)", "/sbin/getty")
            .runlevels(all_levels)
            .restart(RestartPolicy::Always),
    );
    init.register(
        Service::new("udev", "Device node manager", "/sbin/udevd")
            .runlevels(all_levels)
            .restart(RestartPolicy::Always),
    );
    init.register(
        Service::new("network", "Network interface manager", "/sbin/network")
            .requires("udev")
            .runlevels(multiuser)
            .restart(RestartPolicy::OnFailure),
    );
    init.register(
        Service::new("sshd", "OpenSSH server", "/usr/sbin/sshd")
            .requires("network")
            .runlevels(multiuser)
            .restart(RestartPolicy::OnFailure),
    );
    init.register(
        Service::new("rtcsync", "Wall-clock time synchroniser", "/sbin/rtcsync")
            .runlevels(all_levels)
            .restart(RestartPolicy::Never),
    );
}

/// Enter the configured default runlevel.  Called once after FS init.
pub fn boot_to_default_runlevel() {
    install_default_services();
    let mut init = INIT.lock();
    init.boot_tick = crate::task::timer::current_ticks();
    drop(init);
    let _ = INIT.lock().set_runlevel(RunLevel::MultiUser);
}

/// Initialize without forcing a runlevel transition (for very early boot).
pub fn init() {
    install_default_services();
    INIT.lock().boot_tick = crate::task::timer::current_ticks();

    // Lay out /bin with the in-tree coreutil binaries so users can
    // `cat /bin/echo` and (eventually) `exec /bin/echo hi` against
    // real ELFs rather than shell builtins.
    let n = crate::coreutils::populate_bin();
    crate::klog_info!("coreutils: installed {} binaries to /bin", n);
}
