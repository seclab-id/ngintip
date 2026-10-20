//! Process/system resource stats for the Settings > Usage panel. A single System
//! instance is kept and refreshed on each poll so CPU% is a real delta over the
//! poll interval. Reads are cheap and never block the proxy.

use std::time::Instant;

use serde::Serialize;
use sysinfo::{Pid, ProcessesToUpdate, System};

pub struct Usage {
    sys: System,
    pid: Pid,
    started: Instant,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    /// Resident memory of this process, in bytes.
    pub mem_bytes: u64,
    /// Total physical memory of the machine, in bytes.
    pub total_mem_bytes: u64,
    /// CPU used by this process since the last poll, in percent (can exceed 100
    /// across multiple cores).
    pub cpu_percent: f32,
    /// Logical CPU count, so the UI can show cpu% relative to the machine.
    pub cpu_count: usize,
    /// Seconds since the app started.
    pub uptime_secs: u64,
}

impl Usage {
    pub fn new() -> Self {
        let pid = sysinfo::get_current_pid().unwrap_or(Pid::from(0));
        let mut sys = System::new();
        sys.refresh_memory();
        sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        Usage { sys, pid, started: Instant::now() }
    }

    pub fn snapshot(&mut self) -> UsageStats {
        self.sys.refresh_memory();
        self.sys.refresh_processes(ProcessesToUpdate::Some(&[self.pid]), true);
        let (mem_bytes, cpu_percent) = self
            .sys
            .process(self.pid)
            .map(|p| (p.memory(), p.cpu_usage()))
            .unwrap_or((0, 0.0));
        UsageStats {
            mem_bytes,
            total_mem_bytes: self.sys.total_memory(),
            cpu_percent,
            cpu_count: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
            uptime_secs: self.started.elapsed().as_secs(),
        }
    }
}

impl Default for Usage {
    fn default() -> Self {
        Self::new()
    }
}

/// Best-effort resident memory of this process, in bytes (used by crash dumps).
pub fn current_rss() -> u64 {
    let Ok(pid) = sysinfo::get_current_pid() else { return 0 };
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).map(|p| p.memory()).unwrap_or(0)
}
