// Data handed from the sampler thread to the UI once per refresh.

use std::sync::Arc;

use crate::meta::Meta;

pub const HISTORY: usize = 60;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProcKind {
    Normal,
    /// "Systemunterbrechungen": DPC + interrupt time, shown like a process.
    Interrupts,
    /// System Idle Process (Details page only).
    Idle,
}

#[derive(Clone)]
pub struct WinInfo {
    pub hwnd: isize,
    pub title: Arc<str>,
}

#[derive(Clone)]
#[allow(dead_code)] // some fields are kept for future columns (session, priority, ...)
pub struct Proc {
    pub pid: u32,
    /// Parent PID, 0 when the parent is gone or the PID was reused.
    pub ppid: u32,
    pub ctime: u64,
    pub exe: Arc<str>,
    pub meta: Option<Arc<Meta>>,
    pub package: Option<Arc<str>>,
    pub session: u32,
    pub threads: u32,
    pub handles: u32,
    pub priority: i32,
    /// Percent of the whole machine, scaled like Task Manager (processor utility).
    pub cpu: f32,
    pub cpu_time: u64,
    /// Private working set in bytes (Task Manager's "Arbeitsspeicher").
    pub mem: u64,
    pub commit: u64,
    pub ws: u64,
    /// Physical disk bytes/s, None when not measurable for this process.
    pub disk: Option<f32>,
    /// Network bytes/s, None when not measurable for this process.
    pub net: Option<f32>,
    pub gpu: f32,
    pub gpu_engine: Option<Arc<str>>,
    pub eco: bool,
    pub suspended: bool,
    pub services: Vec<Arc<str>>,
    pub windows: Vec<WinInfo>,
    pub user: Option<Arc<str>>,
    pub kind: ProcKind,
    /// Cleared when the process cannot be opened (access denied).
    pub accessible: bool,
}

impl Proc {
    /// Task Manager style display name: FileDescription, else exe without ".exe".
    pub fn display_name(&self) -> String {
        match self.kind {
            ProcKind::Interrupts => return "System interrupts".into(),
            ProcKind::Idle => return "System Idle Process".into(),
            ProcKind::Normal => {}
        }
        if self.pid == 4 {
            return "System".into();
        }
        let exe_l = self.exe.to_ascii_lowercase();
        if exe_l == "svchost.exe" && !self.services.is_empty() {
            return if self.services.len() == 1 {
                format!("Service Host: {}", self.services[0])
            } else {
                format!("Service Host: {} (+{})", self.services[0], self.services.len() - 1)
            };
        }
        if let Some(m) = &self.meta {
            if !m.description.is_empty() {
                return m.description.clone();
            }
        }
        without_exe(&self.exe).to_string()
    }

    pub fn publisher(&self) -> &str {
        self.meta.as_ref().map(|m| m.company.as_str()).unwrap_or("")
    }

    pub fn path(&self) -> &str {
        self.meta.as_ref().map(|m| m.path.as_str()).unwrap_or("")
    }

    pub fn icon(&self) -> isize {
        self.meta.as_ref().map(|m| m.icon()).unwrap_or(0)
    }
}

#[derive(Clone, Default)]
pub struct CpuInfo {
    pub name: String,
    pub base_mhz: u32,
    pub sockets: u32,
    pub cores: u32,
    pub logical: u32,
    pub l1: u64,
    pub l2: u64,
    pub l3: u64,
    pub virtualization: Option<bool>,
    pub util: f32,
    pub speed_mhz: f32,
    pub hist: Vec<f32>,
    pub lp_hist: Vec<Vec<f32>>,
    pub lp_util: Vec<f32>,
}

#[derive(Clone, Default)]
pub struct MemInfo {
    pub total: u64,
    pub avail: u64,
    pub in_use: u64,
    pub compressed: u64,
    pub committed: u64,
    pub commit_limit: u64,
    pub cached: u64,
    pub paged_pool: u64,
    pub nonpaged_pool: u64,
    pub modified: u64,
    pub standby: u64,
    pub free: u64,
    pub hw_reserved: u64,
    pub speed_mts: u32,
    pub slots_used: u32,
    pub slots_total: u32,
    pub form_factor: String,
    pub hist: Vec<f32>,
}

#[derive(Clone, Default)]
pub struct DiskInfo {
    pub index: u32,
    pub letters: String,
    pub model: String,
    pub kind: String,
    pub capacity: u64,
    pub formatted: u64,
    pub system: bool,
    pub pagefile: bool,
    pub active: f32,
    pub resp_ms: f32,
    pub read_bps: f32,
    pub write_bps: f32,
    pub hist_active: Vec<f32>,
    pub hist_read: Vec<f32>,
    pub hist_write: Vec<f32>,
}

#[derive(Clone, Default)]
pub struct NetInfo {
    pub luid: u64,
    pub title: String,
    pub alias: String,
    pub description: String,
    pub conn_type: String,
    pub ipv4: String,
    pub ipv6: String,
    pub ssid: String,
    pub signal: Option<u32>,
    pub link_bps: u64,
    pub send_bps: f32,
    pub recv_bps: f32,
    pub hist_send: Vec<f32>,
    pub hist_recv: Vec<f32>,
}

#[derive(Clone, Default)]
pub struct GpuEngine {
    pub name: Arc<str>,
    pub util: f32,
    pub hist: Vec<f32>,
}

#[derive(Clone, Default)]
#[allow(dead_code)]
pub struct GpuInfo {
    pub index: u32,
    pub luid: u64,
    pub name: String,
    pub util: f32,
    pub engines: Vec<GpuEngine>,
    pub ded_used: u64,
    pub ded_total: u64,
    pub shared_used: u64,
    pub shared_total: u64,
    pub temp_c: Option<f32>,
    pub driver_version: String,
    pub driver_date: String,
    pub location: String,
    pub hist_util: Vec<f32>,
    pub hist_ded: Vec<f32>,
    pub hist_shared: Vec<f32>,
}

#[derive(Clone, Default)]
pub struct Perf {
    pub cpu: CpuInfo,
    pub mem: MemInfo,
    pub disks: Vec<DiskInfo>,
    pub nets: Vec<NetInfo>,
    pub gpus: Vec<GpuInfo>,
    pub uptime_s: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HelperState {
    /// Elevated helper not installed: disk per process unavailable.
    #[default]
    NotInstalled,
    Starting,
    Running,
    /// BTM itself runs elevated; full data without helper.
    SelfElevated,
}

pub struct Snapshot {
    pub seq: u64,
    pub procs: Vec<Proc>,
    pub cpu_pct: f32,
    pub mem_pct: f32,
    pub disk_pct: f32,
    pub gpu_pct: f32,
    pub net_pct: f32,
    pub n_procs: u32,
    pub n_threads: u32,
    pub n_handles: u32,
    pub perf: Perf,
    pub helper: HelperState,
    pub sample_ms: f32,
}

/// "code.exe" -> "code". Process names can be any Unicode text.
fn without_exe(name: &str) -> &str {
    match name.len().checked_sub(4).and_then(|at| Some((name.get(..at)?, name.get(at..)?))) {
        Some((stem, extension)) if extension.eq_ignore_ascii_case(".exe") => stem,
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unicode_process_names_do_not_panic() {
        for name in ["\u{65E5}\u{672C}", "\u{6E38}\u{620F}", "s\u{FC}\u{DF}", "a", ""] {
            assert_eq!(super::without_exe(name), name);
        }
        assert_eq!(super::without_exe("\u{6E38}\u{620F}.EXE"), "\u{6E38}\u{620F}");
    }
}
