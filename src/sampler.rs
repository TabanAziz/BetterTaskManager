// Background sampler: collects everything once per refresh on its own thread and hands
// a finished Snapshot to the UI. The UI thread never blocks on system calls.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HWND, LPARAM};
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows_sys::Win32::Security::{
    GetTokenInformation, LookupAccountSidW, TokenElevation, TokenUser, TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::Packaging::Appx::GetPackageFamilyName;
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, EnumServicesStatusExW, OpenSCManagerW, ENUM_SERVICE_STATUS_PROCESSW, SC_ENUM_PROCESS_INFO,
    SC_MANAGER_ENUMERATE_SERVICE, SERVICE_ACTIVE, SERVICE_WIN32,
};
use windows_sys::Win32::System::SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcess, OpenProcessToken, SetEvent, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowLongW, GetWindowThreadProcessId, InternalGetWindowText, IsWindowVisible,
    PostMessageW, GWL_EXSTYLE, GW_OWNER, WS_EX_TOOLWINDOW,
};

use crate::helper;
use crate::meta;
use crate::model::*;
use crate::nt::{self, ProcBuf, ProcessorPerf};
use crate::pdh::{Counter, Pdh};
use crate::sysinfo::{self, DeviceMap, GpuAdapter};
use crate::util::{from_wide, qpc_ms, qpc_now};

pub const WM_APP_SNAPSHOT: u32 = 0x8000 + 1;

pub struct Control {
    hwnd: AtomicIsize,
    /// Refresh interval in ms; 0 = paused.
    pub interval_ms: AtomicU32,
    wake: isize,
    mailbox: Mutex<Option<Box<Snapshot>>>,
    pub helper_state: AtomicU32,
    pub want_helper_restart: AtomicBool,
}

static CTL: OnceLock<Control> = OnceLock::new();

pub fn control() -> &'static Control {
    CTL.get_or_init(|| Control {
        hwnd: AtomicIsize::new(0),
        interval_ms: AtomicU32::new(1000),
        wake: unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) } as isize,
        mailbox: Mutex::new(None),
        helper_state: AtomicU32::new(0),
        want_helper_restart: AtomicBool::new(false),
    })
}

impl Control {
    /// Ask for an immediate refresh (after End Task, resume, ...).
    pub fn wake(&self) {
        unsafe { SetEvent(self.wake as HANDLE) };
    }
    pub fn take(&self) -> Option<Box<Snapshot>> {
        self.mailbox.lock().unwrap().take()
    }
}

/// Start sampling (idempotent). Called before the window exists so the first process
/// list is collected while Windows is still creating the window.
pub fn start_early() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::Builder::new()
        .name("sampler".into())
        .stack_size(1 << 20)
        .spawn(|| {
            meta::load_disk_cache();
            let mut s = Sampler::new();
            s.run();
        })
        .ok();
}

/// Attach the window; delivers a snapshot that was ready before the window existed.
pub fn start(hwnd: HWND) {
    let c = control();
    c.hwnd.store(hwnd as isize, Ordering::Release);
    start_early();
    if c.mailbox.lock().unwrap().is_some() {
        unsafe { PostMessageW(hwnd, WM_APP_SNAPSHOT, 0, 0) };
    }
}

struct Tracked {
    ctime: u64,
    exe: Arc<str>,
    handle: HANDLE,
    meta: Option<Arc<meta::Meta>>,
    package: Option<Arc<str>>,
    user: Option<Arc<str>>,
    prev_cpu: u64,
    prev_net: Option<u64>,
    prev_disk: Option<u64>,
    gen: u64,
    user_tried: bool,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { CloseHandle(self.handle) };
        }
    }
}

struct PdhSet {
    pdh: Pdh,
    cpu_util: Option<Counter>,
    cpu_perf: Option<Counter>,
    cpu_lp: Option<Counter>,
    gpu_eng: Option<Counter>,
    gpu_ded: Option<Counter>,
    gpu_shared: Option<Counter>,
    disk_idle: Option<Counter>,
    disk_resp: Option<Counter>,
    disk_read: Option<Counter>,
    disk_write: Option<Counter>,
    mem_modified: Option<Counter>,
    mem_standby: [Option<Counter>; 3],
    mem_free: Option<Counter>,
    mem_paged: Option<Counter>,
    mem_nonpaged: Option<Counter>,
    mem_committed: Option<Counter>,
    mem_commit_limit: Option<Counter>,
    scratch: Vec<(String, f64)>,
}

impl PdhSet {
    fn new() -> Option<PdhSet> {
        let mut pdh = Pdh::open()?;
        let mut add = |p: &str| pdh.add(p);
        let cpu_util = add("\\Processor Information(_Total)\\% Processor Utility");
        let cpu_perf = add("\\Processor Information(_Total)\\% Processor Performance");
        let cpu_lp = add("\\Processor Information(*)\\% Processor Utility");
        let gpu_eng = add("\\GPU Engine(*)\\Utilization Percentage");
        let gpu_ded = add("\\GPU Adapter Memory(*)\\Dedicated Usage");
        let gpu_shared = add("\\GPU Adapter Memory(*)\\Shared Usage");
        let disk_idle = add("\\PhysicalDisk(*)\\% Idle Time");
        let disk_resp = add("\\PhysicalDisk(*)\\Avg. Disk sec/Transfer");
        let disk_read = add("\\PhysicalDisk(*)\\Disk Read Bytes/sec");
        let disk_write = add("\\PhysicalDisk(*)\\Disk Write Bytes/sec");
        let mem_modified = add("\\Memory\\Modified Page List Bytes");
        let mem_standby = [
            add("\\Memory\\Standby Cache Reserve Bytes"),
            add("\\Memory\\Standby Cache Normal Priority Bytes"),
            add("\\Memory\\Standby Cache Core Bytes"),
        ];
        let mem_free = add("\\Memory\\Free & Zero Page List Bytes");
        let mem_paged = add("\\Memory\\Pool Paged Bytes");
        let mem_nonpaged = add("\\Memory\\Pool Nonpaged Bytes");
        let mem_committed = add("\\Memory\\Committed Bytes");
        let mem_commit_limit = add("\\Memory\\Commit Limit");
        Some(PdhSet {
            pdh,
            cpu_util,
            cpu_perf,
            cpu_lp,
            gpu_eng,
            gpu_ded,
            gpu_shared,
            disk_idle,
            disk_resp,
            disk_read,
            disk_write,
            mem_modified,
            mem_standby,
            mem_free,
            mem_paged,
            mem_nonpaged,
            mem_committed,
            mem_commit_limit,
            scratch: Vec::new(),
        })
    }

    fn val(&self, c: Option<Counter>) -> Option<f64> {
        c.and_then(|c| self.pdh.value(c))
    }

    fn arr(&mut self, c: Option<Counter>) -> Vec<(String, f64)> {
        let mut out = std::mem::take(&mut self.scratch);
        out.clear();
        if let Some(c) = c {
            self.pdh.array(c, &mut out);
        }
        out
    }
}

struct Sampler {
    buf: ProcBuf,
    tracked: HashMap<u32, Tracked>,
    gen: u64,
    seq: u64,
    devmap: DeviceMap,
    devmap_tick: u64,
    nlp: usize,
    prev_cpu: Vec<ProcessorPerf>,
    prev_qpc: i64,
    pdh: Option<PdhSet>,
    services: HashMap<u32, Vec<Arc<str>>>,
    services_tick: u64,
    gpus: Vec<GpuAdapter>,
    gpu_temp_tick: u64,
    disks: Vec<DiskInfo>,
    net_prev: HashMap<u64, (u64, u64)>,
    nets_tick: u64,
    perf: Perf,
    static_done: bool,
    sid_cache: HashMap<Vec<u8>, Arc<str>>,
    interned: HashMap<String, Arc<str>>,
    elevated: bool,
    helper: helper::Client,
    phases: Vec<(&'static str, f64)>,
    static_rx: Option<std::sync::mpsc::Receiver<StaticInit>>,
}

/// Slow one-time initialisation (PDH counter registration alone takes ~0.8 s) runs on
/// its own thread so the first refreshes are not delayed by it.
struct StaticInit {
    pdh: Option<PdhSet>,
    cpu: CpuInfo,
    mem: MemInfo,
    gpus: Vec<GpuAdapter>,
    nets: Vec<NetInfo>,
}

fn static_init() -> StaticInit {
    let pdh = PdhSet::new();
    if let Some(p) = &pdh {
        p.pdh.collect();
    }
    let mut ms: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    ms.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    unsafe { GlobalMemoryStatusEx(&mut ms) };
    StaticInit {
        pdh,
        cpu: sysinfo::cpu_static(),
        mem: sysinfo::mem_static(ms.ullTotalPhys),
        gpus: sysinfo::gpus_static(),
        nets: sysinfo::net_adapters(),
    }
}

fn push_hist(h: &mut Vec<f32>, v: f32) {
    if h.len() != HISTORY {
        h.clear();
        h.resize(HISTORY, f32::NAN); // NaN = not yet measured (not drawn)
    }
    h.remove(0);
    h.push(v);
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut tok = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
            return false;
        }
        let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret = 0u32;
        let ok = GetTokenInformation(tok, TokenElevation, &mut e as *mut _ as *mut c_void, 4, &mut ret) != 0;
        CloseHandle(tok);
        ok && e.TokenIsElevated != 0
    }
}

impl Sampler {
    fn new() -> Self {
        let nlp = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        let nlp = {
            let si = unsafe {
                let mut si: windows_sys::Win32::System::SystemInformation::SYSTEM_INFO = std::mem::zeroed();
                windows_sys::Win32::System::SystemInformation::GetSystemInfo(&mut si);
                si.dwNumberOfProcessors as usize
            };
            si.max(nlp).max(1)
        };
        let elevated = is_elevated();
        Sampler {
            buf: ProcBuf::new(),
            tracked: HashMap::with_capacity(1024),
            gen: 0,
            seq: 0,
            devmap: DeviceMap::new(),
            devmap_tick: 0,
            nlp,
            prev_cpu: nt::processor_times(nlp),
            prev_qpc: qpc_now(),
            pdh: None,
            services: HashMap::new(),
            services_tick: u64::MAX,
            gpus: Vec::new(),
            gpu_temp_tick: 0,
            disks: Vec::new(),
            net_prev: HashMap::new(),
            nets_tick: 0,
            perf: Perf::default(),
            static_done: false,
            sid_cache: HashMap::new(),
            interned: HashMap::new(),
            elevated,
            helper: helper::Client::new(elevated),
            phases: Vec::new(),
            static_rx: None,
        }
    }

    fn intern(&mut self, s: &str) -> Arc<str> {
        if let Some(a) = self.interned.get(s) {
            return a.clone();
        }
        let a: Arc<str> = Arc::from(s);
        self.interned.insert(s.to_string(), a.clone());
        a
    }

    fn run(&mut self) {
        let ctl = control();
        let mut first = true;
        loop {
            if !self.static_done {
                self.start_static();
            }
            self.sample_and_post();
            let wait = if first {
                first = false;
                400
            } else {
                match ctl.interval_ms.load(Ordering::Acquire) {
                    0 => 0xFFFF_FFFF,
                    n => n,
                }
            };
            unsafe { WaitForSingleObject(ctl.wake as HANDLE, wait) };
        }
    }

    fn start_static(&mut self) {
        self.static_done = true;
        let (tx, rx) = std::sync::mpsc::channel();
        self.static_rx = Some(rx);
        std::thread::Builder::new()
            .name("static-init".into())
            .spawn(move || {
                let _ = tx.send(static_init());
            })
            .ok();
    }

    /// Install the static info once the init thread has finished.
    fn poll_static(&mut self) {
        let Some(rx) = &self.static_rx else { return };
        let Ok(st) = rx.try_recv() else { return };
        self.static_rx = None;
        self.pdh = st.pdh;
        let hist = std::mem::take(&mut self.perf.cpu.hist);
        let lp_hist = std::mem::take(&mut self.perf.cpu.lp_hist);
        self.perf.cpu = st.cpu;
        self.perf.cpu.hist = hist;
        self.perf.cpu.lp_hist = lp_hist;
        let mhist = std::mem::take(&mut self.perf.mem.hist);
        self.perf.mem = st.mem;
        self.perf.mem.hist = mhist;
        self.gpus = st.gpus;
        self.perf.nets = st.nets;
        self.nets_tick = self.gen;
    }

    /// Diagnostic only: synchronous static init.
    fn init_static(&mut self) {
        self.start_static();
        if let Some(rx) = self.static_rx.take() {
            if let Ok(st) = rx.recv() {
                let (tx, rx2) = std::sync::mpsc::channel();
                let _ = tx.send(st);
                self.static_rx = Some(rx2);
                self.poll_static();
            }
        }
    }

    fn lookup_user(&mut self, sid: &[u8]) -> Option<Arc<str>> {
        if let Some(n) = self.sid_cache.get(sid) {
            return Some(n.clone());
        }
        let mut name = [0u16; 256];
        let mut dom = [0u16; 256];
        let mut nl = 256u32;
        let mut dl = 256u32;
        let mut use_ = 0i32;
        let ok = unsafe {
            LookupAccountSidW(
                std::ptr::null(),
                sid.as_ptr() as *mut c_void,
                name.as_mut_ptr(),
                &mut nl,
                dom.as_mut_ptr(),
                &mut dl,
                &mut use_,
            )
        } != 0;
        let s: Arc<str> = if ok { Arc::from(from_wide(&name).as_str()) } else { Arc::from("") };
        self.sid_cache.insert(sid.to_vec(), s.clone());
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    fn token_user(&mut self, h: HANDLE) -> Option<Arc<str>> {
        unsafe {
            let mut tok = std::ptr::null_mut();
            if OpenProcessToken(h, TOKEN_QUERY, &mut tok) == 0 {
                return None;
            }
            let mut buf = [0u64; 32];
            let mut ret = 0u32;
            let ok = GetTokenInformation(tok, TokenUser, buf.as_mut_ptr() as *mut c_void, 256, &mut ret) != 0;
            CloseHandle(tok);
            if !ok {
                return None;
            }
            let tu = &*(buf.as_ptr() as *const TOKEN_USER);
            let sid = tu.User.Sid as *const u8;
            if sid.is_null() {
                return None;
            }
            let sub = *sid.add(1) as usize;
            let bytes = std::slice::from_raw_parts(sid, 8 + 4 * sub).to_vec();
            self.lookup_user(&bytes)
        }
    }

    fn refresh_services(&mut self) {
        self.services.clear();
        unsafe {
            let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ENUMERATE_SERVICE);
            if scm.is_null() {
                return;
            }
            let mut buf = vec![0u64; (256 << 10) / 8];
            let mut needed = 0u32;
            let mut count = 0u32;
            let mut resume = 0u32;
            loop {
                let ok = EnumServicesStatusExW(
                    scm,
                    SC_ENUM_PROCESS_INFO,
                    SERVICE_WIN32,
                    SERVICE_ACTIVE,
                    buf.as_mut_ptr() as *mut u8,
                    (buf.len() * 8) as u32,
                    &mut needed,
                    &mut count,
                    &mut resume,
                    std::ptr::null(),
                ) != 0;
                let items =
                    std::slice::from_raw_parts(buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW, count as usize);
                for it in items {
                    let pid = it.ServiceStatusProcess.dwProcessId;
                    if pid == 0 {
                        continue;
                    }
                    let name = crate::util::from_wide_ptr(it.lpDisplayName);
                    let a = self.intern(&name);
                    self.services.entry(pid).or_default().push(a);
                }
                if ok || resume == 0 {
                    break;
                }
            }
            CloseServiceHandle(scm);
        }
        for v in self.services.values_mut() {
            v.sort();
        }
    }

    fn mark(&mut self, name: &'static str) {
        let now = qpc_now();
        self.phases.push((name, now as f64));
    }

    fn sample_and_post(&mut self) {
        let t0 = qpc_now();
        self.phases.clear();
        self.phases.push(("start", t0 as f64));
        self.poll_static();
        let ctl = control();
        self.helper.request();

        let full = self.elevated && self.buf.query(nt::SYSTEM_FULL_PROCESS_INFORMATION_CLASS);
        if !full && !self.buf.query(nt::SYSTEM_PROCESS_INFORMATION_CLASS) {
            return;
        }
        self.mark("query");
        let now_qpc = qpc_now();
        let elapsed_s = (qpc_ms(self.prev_qpc, now_qpc) / 1000.0).max(0.001);
        self.prev_qpc = now_qpc;

        // ---- system CPU times
        let cur_cpu = nt::processor_times(self.nlp);
        let mut busy = 0f64;
        let mut total = 0f64;
        let mut dpc_int = 0f64;
        let mut lp_time_util = Vec::with_capacity(cur_cpu.len());
        let mut idle_total = 0f64;
        for (i, c) in cur_cpu.iter().enumerate() {
            let p = self.prev_cpu.get(i).copied().unwrap_or_default();
            let idle = (c.idle - p.idle).max(0) as f64;
            let ker = (c.kernel - p.kernel).max(0) as f64;
            let usr = (c.user - p.user).max(0) as f64;
            let t = ker + usr;
            busy += (t - idle).max(0.0);
            total += t;
            idle_total += idle;
            dpc_int += ((c.dpc - p.dpc).max(0) + (c.interrupt - p.interrupt).max(0)) as f64;
            lp_time_util.push(if t > 0.0 { ((t - idle) / t * 100.0).clamp(0.0, 100.0) as f32 } else { 0.0 });
        }
        self.prev_cpu = cur_cpu;
        let time_util = if total > 0.0 { (busy / total * 100.0) as f32 } else { 0.0 };

        self.mark("cputimes");
        // ---- PDH (CPU utility, GPU, disks, memory lists)
        let mut util_total = time_util;
        let mut perf_pct = None;
        let mut lp_util: Vec<f32> = lp_time_util.clone();
        let mut gpu_eng: Vec<(String, f64)> = Vec::new();
        let mut gpu_ded: Vec<(String, f64)> = Vec::new();
        let mut gpu_sh: Vec<(String, f64)> = Vec::new();
        let mut disk_rows: Vec<(String, f64)> = Vec::new();
        let mut disk_resp: Vec<(String, f64)> = Vec::new();
        let mut disk_read: Vec<(String, f64)> = Vec::new();
        let mut disk_write: Vec<(String, f64)> = Vec::new();
        let mut memvals = [None::<f64>; 8];
        if let Some(p) = &mut self.pdh {
            if p.pdh.collect() {
                if let Some(u) = p.val(p.cpu_util) {
                    util_total = (u as f32).clamp(0.0, 100.0);
                }
                perf_pct = p.val(p.cpu_perf);
                let lps = p.arr(p.cpu_lp);
                let mut per: Vec<(usize, f32)> = lps
                    .iter()
                    .filter(|(n, _)| !n.contains("_Total"))
                    .filter_map(|(n, v)| {
                        let mut it = n.split(',');
                        let g: usize = it.next()?.parse().ok()?;
                        let i: usize = it.next()?.parse().ok()?;
                        Some((g * 64 + i, (*v as f32).clamp(0.0, 100.0)))
                    })
                    .collect();
                per.sort_by_key(|x| x.0);
                if per.len() == lp_util.len() {
                    lp_util = per.into_iter().map(|x| x.1).collect();
                }
                p.scratch = lps;
                gpu_eng = p.arr(p.gpu_eng);
                p.scratch = Vec::new();
                gpu_ded = p.arr(p.gpu_ded);
                p.scratch = Vec::new();
                gpu_sh = p.arr(p.gpu_shared);
                p.scratch = Vec::new();
                disk_rows = p.arr(p.disk_idle);
                p.scratch = Vec::new();
                disk_resp = p.arr(p.disk_resp);
                p.scratch = Vec::new();
                disk_read = p.arr(p.disk_read);
                p.scratch = Vec::new();
                disk_write = p.arr(p.disk_write);
                p.scratch = Vec::new();
                memvals = [
                    p.val(p.mem_modified),
                    Some(p.mem_standby.iter().filter_map(|c| p.val(*c)).sum()),
                    p.val(p.mem_free),
                    p.val(p.mem_paged),
                    p.val(p.mem_nonpaged),
                    p.val(p.mem_committed),
                    p.val(p.mem_commit_limit),
                    None,
                ];
            }
        }
        self.mark("pdh");
        // Task Manager scales per-process CPU by processor utility so the rows add up
        // to the header value.
        let scale = if time_util > 0.5 { (util_total / time_util).clamp(0.25, 4.0) } else { 1.0 };

        // ---- helper data (elevated: physical disk bytes + network for every process)
        let ext_ok = self.helper.collect();
        let ext_map = std::mem::take(&mut self.helper.map);
        let ext = if ext_ok { Some(&ext_map) } else { None };
        let helper_ok = ext_ok || full;

        self.mark("helper");
        // ---- processes
        self.gen += 1;
        let gen = self.gen;
        if self.devmap_tick == 0 || self.gen - self.devmap_tick > 30 {
            self.devmap = DeviceMap::new();
            self.devmap_tick = self.gen;
        }
        if self.services_tick == u64::MAX || self.gen.saturating_sub(self.services_tick) >= 5 {
            self.refresh_services();
            self.services_tick = self.gen;
        }
        let cap = total.max(1.0);
        self.mark("svc");
        let mut procs: Vec<Proc> = Vec::with_capacity(self.tracked.len() + 16);
        let mut n_threads = 0u32;
        let mut n_handles = 0u32;
        let mut memcomp = 0u64;
        let pbuf = std::mem::replace(&mut self.buf, ProcBuf::empty());
        let views: Vec<_> = pbuf.iter(full).collect();
        {
            // Resolve image paths and version resources of new processes in parallel.
            let mut paths: Vec<String> = Vec::new();
            for v in &views {
                let pid = v.pid();
                if pid <= 4 {
                    continue;
                }
                let new = self.tracked.get(&pid).map(|t| t.ctime != v.create_time()).unwrap_or(true);
                if new {
                    if let Some(p) = nt::image_path_by_pid(pid).and_then(|p| self.devmap.to_dos(&p)) {
                        paths.push(p);
                    }
                }
            }
            self.mark("paths");
            paths.sort_unstable_by_key(|a| a.to_lowercase());
            paths.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
            paths.retain(|p| !meta::is_cached(p));
            let to_read = paths.iter().filter(|p| meta::needs_read(p)).count();
            if to_read > 16 {
                let threads = self.nlp.clamp(2, 8);
                let chunk = paths.len().div_ceil(threads);
                std::thread::scope(|sc| {
                    for part in paths.chunks(chunk) {
                        sc.spawn(move || {
                            for p in part {
                                meta::get(p);
                            }
                        });
                    }
                });
            } else {
                for p in &paths {
                    meta::get_quiet(p);
                }
                meta::kick();
            }
        }
        self.mark("meta");
        for v in &views {
            let pid = v.pid();
            let ctime = v.create_time();
            n_threads += v.threads();
            n_handles += v.handles();
            let cpu_time = v.kernel_time() + v.user_time();
            let is_new = match self.tracked.get(&pid) {
                Some(t) => t.ctime != ctime,
                None => true,
            };
            if is_new {
                let name = if pid == 0 { "System Idle Process".to_string() } else { v.image_name() };
                let exe = self.intern(&name);
                let handle = if pid > 4 {
                    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) }
                } else {
                    std::ptr::null_mut()
                };
                let meta = if pid > 4 {
                    nt::image_path_by_pid(pid).and_then(|p| self.devmap.to_dos(&p)).map(|p| meta::get(&p))
                } else {
                    None
                };
                let package = if !handle.is_null() { package_family(handle).map(|p| self.intern(&p)) } else { None };
                let user = None; // resolved lazily below (only the Details page needs it)
                self.tracked.insert(
                    pid,
                    Tracked {
                        ctime,
                        exe,
                        handle,
                        meta,
                        package,
                        user,
                        prev_cpu: cpu_time,
                        prev_net: None,
                        prev_disk: None,
                        gen,
                        user_tried: false,
                    },
                );
            }
            let t = self.tracked.get_mut(&pid).unwrap();
            t.gen = gen;
            let dcpu = cpu_time.saturating_sub(t.prev_cpu) as f64;
            t.prev_cpu = cpu_time;
            let raw = if is_new { 0.0 } else { (dcpu / cap * 100.0) as f32 };

            // Network: from the elevated helper when present, else our own handle.
            let mut net = None;
            let mut disk = None;
            let mut net_total: Option<u64> = None;
            let mut disk_total: Option<u64> = None;
            if let Some(e) = ext.and_then(|m| m.get(&(pid, ctime))) {
                net_total = Some(e.net_in + e.net_out);
                disk_total = Some(e.disk_read + e.disk_write);
            } else if let Some((r, w)) = v.disk_bytes() {
                disk_total = Some(r + w);
            }
            if net_total.is_none() && !t.handle.is_null() {
                if let Some(nc) = nt::process_net(t.handle) {
                    net_total = Some(nc.bytes_in + nc.bytes_out);
                }
            }
            if let Some(nt_) = net_total {
                if let Some(prev) = t.prev_net {
                    net = Some((nt_.saturating_sub(prev) as f64 / elapsed_s) as f32);
                } else {
                    net = Some(0.0);
                }
                t.prev_net = Some(nt_);
            }
            if let Some(dt) = disk_total {
                if let Some(prev) = t.prev_disk {
                    disk = Some((dt.saturating_sub(prev) as f64 / elapsed_s) as f32);
                } else {
                    disk = Some(0.0);
                }
                t.prev_disk = Some(dt);
            }
            let eco = !t.handle.is_null() && nt::process_eco(t.handle);
            let kind = if pid == 0 { ProcKind::Idle } else { ProcKind::Normal };
            if &*t.exe == "Memory Compression" {
                memcomp = v.working_set();
            }
            procs.push(Proc {
                pid,
                ppid: v.ppid(),
                ctime,
                exe: t.exe.clone(),
                meta: t.meta.clone(),
                package: t.package.clone(),
                session: v.session(),
                threads: v.threads(),
                handles: v.handles(),
                priority: v.base_priority(),
                cpu: if pid == 0 { (idle_total / cap * 100.0) as f32 } else { (raw * scale).min(100.0) },
                cpu_time,
                mem: v.private_ws(),
                commit: v.private_bytes(),
                ws: v.working_set(),
                disk,
                net,
                gpu: 0.0,
                gpu_engine: None,
                eco,
                suspended: pid > 4 && v.all_threads_suspended(),
                services: self.services.get(&pid).cloned().unwrap_or_default(),
                windows: Vec::new(),
                user: t.user.clone(),
                kind,
                accessible: !t.handle.is_null(),
            });
        }
        drop(views);
        self.mark("loop");
        // Usernames for processes we could not open, from the elevated SID data.
        if let Some(e) = ext {
            let missing: Vec<(usize, Vec<u8>)> = procs
                .iter()
                .enumerate()
                .filter(|(_, p)| p.user.is_none())
                .filter_map(|(i, p)| e.get(&(p.pid, p.ctime)).filter(|x| !x.sid.is_empty()).map(|x| (i, x.sid.clone())))
                .collect();
            for (i, sid) in missing {
                let u = self.lookup_user(&sid);
                if let Some(t) = self.tracked.get_mut(&procs[i].pid) {
                    t.user = u.clone();
                }
                procs[i].user = u;
            }
        } else if full {
            let views: Vec<(usize, Vec<u8>)> =
                pbuf.iter(true).enumerate().filter_map(|(i, v)| v.user_sid().map(|s| (i, s.to_vec()))).collect();
            for (i, sid) in views {
                if i < procs.len() && procs[i].user.is_none() {
                    let u = self.lookup_user(&sid);
                    procs[i].user = u;
                }
            }
        }
        self.buf = pbuf;
        self.tracked.retain(|_, t| t.gen == gen);
        // Owner names: a few per refresh, so the first refresh stays fast.
        {
            let todo: Vec<(u32, HANDLE)> = self
                .tracked
                .iter()
                .filter(|(_, t)| t.user.is_none() && !t.user_tried && !t.handle.is_null())
                .take(96)
                .map(|(p, t)| (*p, t.handle))
                .collect();
            for (pid, h) in todo {
                let u = self.token_user(h);
                if let Some(t) = self.tracked.get_mut(&pid) {
                    t.user_tried = true;
                    t.user = u;
                }
            }
        }
        if self.seq % 15 == 2 {
            meta::save_disk_cache_if_dirty();
        }
        self.helper.map = ext_map;

        self.mark("procloop");
        // Parent links are only valid if the parent is older than the child (PID reuse).
        {
            let ctimes: HashMap<u32, u64> = procs.iter().map(|p| (p.pid, p.ctime)).collect();
            for p in procs.iter_mut() {
                match ctimes.get(&p.ppid) {
                    Some(&pc) if p.ppid != p.pid && pc <= p.ctime && p.ppid != 0 => {}
                    _ => p.ppid = 0,
                }
            }
        }

        self.mark("parents");
        // ---- top-level windows per process
        let wins = enum_windows();
        if !wins.is_empty() {
            let idx: HashMap<u32, usize> = procs.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();
            for (pid, w) in wins {
                if let Some(&i) = idx.get(&pid) {
                    procs[i].windows.push(w);
                }
            }
        }

        self.mark("windows");
        // ---- GPU per process / per adapter
        let mut gpu_max = 0f32;
        {
            // (pid, luid, eng) -> util
            let mut per_proc: HashMap<u32, HashMap<(u64, u32), f32>> = HashMap::new();
            let mut per_eng: HashMap<(u64, u32), (f32, Arc<str>)> = HashMap::new();
            for (name, val) in &gpu_eng {
                if let Some((pid, luid, eng, ty)) = parse_gpu_instance(name) {
                    let v = *val as f32;
                    *per_proc.entry(pid).or_default().entry((luid, eng)).or_insert(0.0) += v;
                    let tyl = self.intern(&pretty_engine(ty));
                    let e = per_eng.entry((luid, eng)).or_insert((0.0, tyl));
                    e.0 += v;
                }
            }
            let gpu_index: HashMap<u64, u32> = self.gpus.iter().map(|g| (g.luid, g.info.index)).collect();
            let idx: HashMap<u32, usize> = procs.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();
            for (pid, engines) in &per_proc {
                if let Some(&i) = idx.get(pid) {
                    if let Some((&(luid, eng), &v)) =
                        engines.iter().max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                    {
                        procs[i].gpu = v.clamp(0.0, 100.0);
                        if v > 0.0 {
                            if let (Some(gi), Some((_, ty))) = (gpu_index.get(&luid), per_eng.get(&(luid, eng))) {
                                let label = format!("GPU {} - {}", gi, ty);
                                procs[i].gpu_engine = Some(self.intern(&label));
                            }
                        }
                    }
                }
            }
            let ded: HashMap<u64, f64> = sum_by_luid(&gpu_ded);
            let sh: HashMap<u64, f64> = sum_by_luid(&gpu_sh);
            let temp_due = self.gen.saturating_sub(self.gpu_temp_tick) >= 2;
            if temp_due {
                self.gpu_temp_tick = self.gen;
            }
            let mut infos = Vec::with_capacity(self.gpus.len());
            for g in &mut self.gpus {
                let mut info = std::mem::take(&mut g.info);
                // Engine types for this adapter: utilisation = busiest engine of that type.
                let mut types: Vec<(Arc<str>, f32)> = Vec::new();
                for (&(luid, _eng), (v, ty)) in &per_eng {
                    if luid != g.luid {
                        continue;
                    }
                    match types.iter_mut().find(|t| t.0 == *ty) {
                        Some(t) => t.1 = t.1.max(*v),
                        None => types.push((ty.clone(), *v)),
                    }
                }
                // Keep engine order stable: known types first.
                let order = |n: &str| match n {
                    "3D" => 0,
                    "Copy" => 1,
                    "Video Encode" => 2,
                    "Video Decode" => 3,
                    _ => 4,
                };
                for (ty, v) in types {
                    match info.engines.iter_mut().find(|e| e.name == ty) {
                        Some(e) => e.util = v.clamp(0.0, 100.0),
                        None => info.engines.push(GpuEngine { name: ty, util: v.clamp(0.0, 100.0), hist: Vec::new() }),
                    }
                }
                info.engines.sort_by(|a, b| order(&a.name).cmp(&order(&b.name)).then(a.name.cmp(&b.name)));
                for e in info.engines.iter_mut() {
                    let present = per_eng.iter().any(|(&(l, _), (_, t))| l == g.luid && *t == e.name);
                    if !present {
                        e.util = 0.0;
                    }
                    push_hist(&mut e.hist, e.util);
                }
                info.util = info.engines.iter().map(|e| e.util).fold(0.0, f32::max);
                info.ded_used = ded.get(&g.luid).copied().unwrap_or(0.0) as u64;
                info.shared_used = sh.get(&g.luid).copied().unwrap_or(0.0) as u64;
                if temp_due {
                    info.temp_c = sysinfo::gpu_temperature(g.handle);
                }
                push_hist(&mut info.hist_util, info.util);
                push_hist(
                    &mut info.hist_ded,
                    if info.ded_total > 0 { info.ded_used as f32 / info.ded_total as f32 * 100.0 } else { 0.0 },
                );
                push_hist(
                    &mut info.hist_shared,
                    if info.shared_total > 0 {
                        info.shared_used as f32 / info.shared_total as f32 * 100.0
                    } else {
                        0.0
                    },
                );
                gpu_max = gpu_max.max(info.util);
                infos.push(info.clone());
                g.info = info;
            }
            self.perf.gpus = infos;
        }

        self.mark("gpu");
        // ---- interrupts pseudo process (shown on the Processes page)
        procs.push(Proc {
            pid: u32::MAX,
            ppid: 0,
            ctime: 0,
            exe: self.intern("Interrupts"),
            meta: None,
            package: None,
            session: 0,
            threads: 0,
            handles: 0,
            priority: 0,
            cpu: ((dpc_int / cap * 100.0) as f32 * scale).min(100.0),
            cpu_time: 0,
            mem: 0,
            commit: 0,
            ws: 0,
            disk: None,
            net: None,
            gpu: 0.0,
            gpu_engine: None,
            eco: false,
            suspended: false,
            services: Vec::new(),
            windows: Vec::new(),
            user: None,
            kind: ProcKind::Interrupts,
            accessible: false,
        });

        // ---- memory
        let mut ms: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        ms.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        unsafe { GlobalMemoryStatusEx(&mut ms) };
        {
            let m = &mut self.perf.mem;
            m.total = ms.ullTotalPhys;
            m.avail = ms.ullAvailPhys;
            m.in_use = ms.ullTotalPhys.saturating_sub(ms.ullAvailPhys);
            m.compressed = memcomp;
            m.modified = memvals[0].unwrap_or(0.0) as u64;
            m.standby = memvals[1].unwrap_or(0.0) as u64;
            m.free = memvals[2].unwrap_or(0.0) as u64;
            m.cached = m.standby + m.modified;
            m.paged_pool = memvals[3].unwrap_or(0.0) as u64;
            m.nonpaged_pool = memvals[4].unwrap_or(0.0) as u64;
            m.committed = memvals[5].map(|v| v as u64).unwrap_or(ms.ullTotalPageFile - ms.ullAvailPageFile);
            m.commit_limit = memvals[6].map(|v| v as u64).unwrap_or(ms.ullTotalPageFile);
            let pct = if m.total > 0 { m.in_use as f32 / m.total as f32 * 100.0 } else { 0.0 };
            push_hist(&mut m.hist, pct);
        }
        let mem_pct = self.perf.mem.hist.last().copied().unwrap_or(0.0);

        // ---- CPU perf page
        {
            let c = &mut self.perf.cpu;
            c.util = util_total;
            c.speed_mhz = perf_pct.map(|p| p as f32 * c.base_mhz as f32 / 100.0).unwrap_or(c.base_mhz as f32);
            push_hist(&mut c.hist, util_total);
            if c.lp_hist.len() != lp_util.len() {
                c.lp_hist = vec![Vec::new(); lp_util.len()];
            }
            for (h, v) in c.lp_hist.iter_mut().zip(lp_util.iter()) {
                push_hist(h, *v);
            }
            c.lp_util = lp_util;
        }

        self.mark("mem+cpu");
        // ---- disks (discovered from the first PDH sample that has instances)
        if self.disks.is_empty() && !disk_rows.is_empty() {
            let mut ds: Vec<DiskInfo> = disk_rows
                .iter()
                .filter(|(n, _)| n != "_Total")
                .filter_map(|(n, _)| {
                    let mut it = n.splitn(2, ' ');
                    let idx: u32 = it.next()?.parse().ok()?;
                    let letters = it.next().unwrap_or("").to_string();
                    Some(sysinfo::disk_static(idx, &letters))
                })
                .collect();
            ds.sort_by_key(|d| d.index);
            self.disks = ds;
        }
        let mut disk_max = 0f32;
        {
            let find = |arr: &Vec<(String, f64)>, idx: u32| -> Option<f64> {
                arr.iter()
                    .find(|(n, _)| n.split(' ').next().and_then(|s| s.parse::<u32>().ok()) == Some(idx))
                    .map(|x| x.1)
            };
            for d in self.disks.iter_mut() {
                let idle = find(&disk_rows, d.index);
                d.active = idle.map(|i| (100.0 - i as f32).clamp(0.0, 100.0)).unwrap_or(0.0);
                d.resp_ms = find(&disk_resp, d.index).map(|v| (v * 1000.0) as f32).unwrap_or(0.0);
                d.read_bps = find(&disk_read, d.index).unwrap_or(0.0) as f32;
                d.write_bps = find(&disk_write, d.index).unwrap_or(0.0) as f32;
                push_hist(&mut d.hist_active, d.active);
                push_hist(&mut d.hist_read, d.read_bps);
                push_hist(&mut d.hist_write, d.write_bps);
                disk_max = disk_max.max(d.active);
            }
            self.perf.disks = self.disks.clone();
        }

        self.mark("disks");
        // ---- network adapters
        let mut net_max = 0f32;
        {
            if self.gen.saturating_sub(self.nets_tick) >= 10 {
                let fresh = sysinfo::net_adapters();
                let old = std::mem::take(&mut self.perf.nets);
                self.perf.nets = fresh
                    .into_iter()
                    .map(|mut n| {
                        if let Some(o) = old.iter().find(|o| o.luid == n.luid) {
                            n.hist_send = o.hist_send.clone();
                            n.hist_recv = o.hist_recv.clone();
                        }
                        n
                    })
                    .collect();
                self.nets_tick = self.gen;
            }
            let luids: Vec<u64> = self.perf.nets.iter().map(|n| n.luid).collect();
            let counters = if_counters(&luids);
            for n in self.perf.nets.iter_mut() {
                if let Some(&(i, o)) = counters.get(&n.luid) {
                    let (pi, po) = self.net_prev.get(&n.luid).copied().unwrap_or((i, o));
                    n.recv_bps = (i.saturating_sub(pi) as f64 * 8.0 / elapsed_s) as f32;
                    n.send_bps = (o.saturating_sub(po) as f64 * 8.0 / elapsed_s) as f32;
                    self.net_prev.insert(n.luid, (i, o));
                }
                push_hist(&mut n.hist_send, n.send_bps);
                push_hist(&mut n.hist_recv, n.recv_bps);
                if n.link_bps > 0 {
                    net_max = net_max.max(((n.send_bps + n.recv_bps) as f64 / n.link_bps as f64 * 100.0) as f32);
                }
            }
        }
        self.perf.uptime_s = unsafe { GetTickCount64() } / 1000;

        self.mark("nets");
        let n_procs = procs.iter().filter(|p| p.kind == ProcKind::Normal).count() as u32;
        self.seq += 1;
        let helper_state = if full {
            HelperState::SelfElevated
        } else if helper_ok {
            HelperState::Running
        } else {
            self.helper.state()
        };
        ctl.helper_state.store(helper_state as u32, Ordering::Release);
        if ctl.want_helper_restart.swap(false, Ordering::AcqRel) {
            self.helper.reset();
        }
        let snap = Box::new(Snapshot {
            seq: self.seq,
            procs,
            cpu_pct: util_total,
            mem_pct,
            disk_pct: disk_max,
            gpu_pct: gpu_max,
            net_pct: net_max.min(100.0),
            n_procs,
            n_threads,
            n_handles,
            perf: self.perf.clone(),
            helper: helper_state,
            sample_ms: qpc_ms(t0, qpc_now()) as f32,
        });
        *ctl.mailbox.lock().unwrap() = Some(snap);
        let hwnd = ctl.hwnd.load(Ordering::Acquire);
        if hwnd != 0 {
            unsafe { PostMessageW(hwnd as HWND, WM_APP_SNAPSHOT, 0, 0) };
        }
    }
}

fn package_family(h: HANDLE) -> Option<String> {
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    let r = unsafe { GetPackageFamilyName(h, &mut len, buf.as_mut_ptr()) };
    if r == 0 {
        let s = from_wide(&buf);
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

fn sum_by_luid(arr: &[(String, f64)]) -> HashMap<u64, f64> {
    let mut m = HashMap::new();
    for (n, v) in arr {
        if let Some(l) = parse_luid(n) {
            *m.entry(l).or_insert(0.0) += *v;
        }
    }
    m
}

fn parse_hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).ok()
}

/// "luid_0x00000000_0x0000D1F0_phys_0" -> luid key
fn parse_luid(name: &str) -> Option<u64> {
    let i = name.find("luid_")?;
    let rest = &name[i + 5..];
    let mut it = rest.split('_');
    let hi = parse_hex(it.next()?)?;
    let lo = parse_hex(it.next()?)?;
    Some(sysinfo::luid_key(hi as i32, lo))
}

/// "pid_1234_luid_0x0_0xD1F0_phys_0_eng_0_engtype_3D" -> (pid, luid, eng, "3D")
fn parse_gpu_instance(name: &str) -> Option<(u32, u64, u32, &str)> {
    let rest = name.strip_prefix("pid_")?;
    let us = rest.find('_')?;
    let pid: u32 = rest[..us].parse().ok()?;
    let luid = parse_luid(rest)?;
    let ei = rest.find("_eng_")?;
    let after = &rest[ei + 5..];
    let eng: u32 = after.split('_').next()?.parse().ok()?;
    let ti = rest.find("engtype_")?;
    Some((pid, luid, eng, &rest[ti + 8..]))
}

/// "VideoDecode" -> "Video Decode"; leaves "3D", "Copy", "Compute_0" alone.
fn pretty_engine(t: &str) -> String {
    let mut out = String::with_capacity(t.len() + 4);
    let mut prev_lower = false;
    for ch in t.chars() {
        if ch.is_ascii_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = ch.is_ascii_lowercase();
        out.push(ch);
    }
    if out.is_empty() {
        "GPU".into()
    } else {
        out
    }
}

fn if_counters(luids: &[u64]) -> HashMap<u64, (u64, u64)> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetIfEntry2, MIB_IF_ROW2};
    let mut m = HashMap::new();
    for &l in luids {
        let mut row: MIB_IF_ROW2 = unsafe { std::mem::zeroed() };
        row.InterfaceLuid.Value = l;
        if unsafe { GetIfEntry2(&mut row) } == 0 {
            m.insert(l, (row.InOctets, row.OutOctets));
        }
    }
    m
}

fn enum_windows() -> Vec<(u32, WinInfo)> {
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let out = &mut *(lp as *mut Vec<(u32, WinInfo)>);
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        if !GetWindow(hwnd, GW_OWNER).is_null() {
            return 1;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW != 0 {
            return 1;
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, &mut cloaked as *mut _ as *mut c_void, 4) == 0
            && cloaked != 0
        {
            return 1;
        }
        let mut buf = [0u16; 256];
        let n = InternalGetWindowText(hwnd, buf.as_mut_ptr(), 256);
        if n <= 0 {
            return 1;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let title = String::from_utf16_lossy(&buf[..n as usize]);
        out.push((pid, WinInfo { hwnd: hwnd as isize, title: Arc::from(title.as_str()) }));
        1
    }
    let mut out: Vec<(u32, WinInfo)> = Vec::new();
    unsafe { EnumWindows(Some(cb), &mut out as *mut _ as LPARAM) };
    out
}

/// Diagnostic: run a few sampling rounds synchronously and write a text report.
pub fn dump(path: &str) {
    use std::fmt::Write as _;
    let mut out = String::new();
    let t_new = qpc_now();
    meta::load_disk_cache();
    let mut s = Sampler::new();
    let _ = writeln!(out, "Sampler::new {:.1} ms, elevated={}", qpc_ms(t_new, qpc_now()), s.elevated);
    let mut last: Option<Box<Snapshot>> = None;
    for round in 0..4 {
        let t = qpc_now();
        s.sample_and_post();
        let ms = qpc_ms(t, qpc_now());
        let snap = control().take();
        let _ = writeln!(
            out,
            "round {} sample {:.1} ms (internal {:.1})",
            round,
            ms,
            snap.as_ref().map(|x| x.sample_ms).unwrap_or(-1.0)
        );
        let mut ph = String::new();
        for w in s.phases.windows(2) {
            let _ = write!(ph, "{}={:.1} ", w[1].0, (w[1].1 - w[0].1) * 1000.0 / crate::util::qpc_freq() as f64);
        }
        let _ = writeln!(out, "   phases: {}", ph);
        if round == 0 {
            let t2 = qpc_now();
            s.init_static();
            let _ = writeln!(out, "init_static {:.1} ms", qpc_ms(t2, qpc_now()));
        }
        last = snap;
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
    if let Some(sn) = last {
        let _ = writeln!(
            out,
            "\nprocs {} threads {} handles {} cpu {:.1}% mem {:.1}% disk {:.1}% gpu {:.1}% net {:.1}% helper {:?}",
            sn.n_procs,
            sn.n_threads,
            sn.n_handles,
            sn.cpu_pct,
            sn.mem_pct,
            sn.disk_pct,
            sn.gpu_pct,
            sn.net_pct,
            sn.helper
        );
        let mut ps: Vec<&Proc> = sn.procs.iter().collect();
        ps.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
        let _ = writeln!(out, "\nTOP CPU:");
        for p in ps.iter().take(30) {
            let _ = writeln!(
                out,
                "{:>6} {:>6} {:<34} cpu {:>5.1} mem {:>8.1}MB disk {:>9} net {:>9} gpu {:>5.1} {:<16} eco {} susp {} acc {} pkg {:?} svc {:?} win {} user {:?}",
                p.pid,
                p.ppid,
                p.display_name().chars().take(34).collect::<String>(),
                p.cpu,
                p.mem as f64 / 1048576.0,
                p.disk.map(|d| format!("{:.1}KB/s", d / 1024.0)).unwrap_or("-".into()),
                p.net.map(|d| format!("{:.1}KB/s", d / 1024.0)).unwrap_or("-".into()),
                p.gpu,
                p.gpu_engine.as_deref().unwrap_or(""),
                p.eco,
                p.suspended,
                p.accessible,
                p.package.as_deref(),
                p.services.iter().take(2).map(|s| s.to_string()).collect::<Vec<_>>(),
                p.windows.len(),
                p.user.as_deref()
            );
        }
        let sum: f32 = sn.procs.iter().filter(|p| p.kind != ProcKind::Idle).map(|p| p.cpu).sum();
        let _ = writeln!(out, "sum of process cpu (excl idle) = {:.1}%", sum);
        let nameless = sn.procs.iter().filter(|p| p.meta.is_none()).count();
        let _ = writeln!(out, "procs without path: {}", nameless);
        for p in sn.procs.iter().filter(|p| p.meta.is_none()).take(20) {
            let _ = writeln!(out, "   nopath {} {}", p.pid, p.exe);
        }
        let pf = &sn.perf;
        let c = &pf.cpu;
        let _ = writeln!(out, "\nCPU '{}' base {} MHz speed {:.0} MHz sockets {} cores {} lps {} L1 {} L2 {} L3 {} virt {:?} util {:.1} lp_util n={}",
            c.name, c.base_mhz, c.speed_mhz, c.sockets, c.cores, c.logical, c.l1, c.l2, c.l3, c.virtualization, c.util, c.lp_util.len());
        let m = &pf.mem;
        let _ = writeln!(out, "MEM total {:.1}G avail {:.1}G inuse {:.1}G comp {:.2}G commit {:.1}/{:.1}G cached {:.1}G paged {:.0}M nonpaged {:.0}M mod {:.0}M standby {:.1}G free {:.1}G hwres {:.0}M speed {} slots {}/{} ff {}",
            m.total as f64/1e9*0.931, m.avail as f64/1073741824.0, m.in_use as f64/1073741824.0, m.compressed as f64/1073741824.0,
            m.committed as f64/1073741824.0, m.commit_limit as f64/1073741824.0, m.cached as f64/1073741824.0,
            m.paged_pool as f64/1048576.0, m.nonpaged_pool as f64/1048576.0, m.modified as f64/1048576.0, m.standby as f64/1073741824.0, m.free as f64/1073741824.0,
            m.hw_reserved as f64/1048576.0, m.speed_mts, m.slots_used, m.slots_total, m.form_factor);
        for d in &pf.disks {
            let _ = writeln!(out, "DISK {} '{}' model '{}' kind '{}' cap {:.1}G fmt {:.1}G sys {} pf {} active {:.1}% resp {:.1}ms r {:.0} w {:.0}",
                d.index, d.letters, d.model, d.kind, d.capacity as f64/1e9, d.formatted as f64/1e9, d.system, d.pagefile, d.active, d.resp_ms, d.read_bps, d.write_bps);
        }
        for n in &pf.nets {
            let _ = writeln!(
                out,
                "NET '{}' alias '{}' desc '{}' type '{}' v4 {} v6 {} ssid '{}' sig {:?} link {} send {:.0} recv {:.0}",
                n.title,
                n.alias,
                n.description,
                n.conn_type,
                n.ipv4,
                n.ipv6,
                n.ssid,
                n.signal,
                n.link_bps,
                n.send_bps,
                n.recv_bps
            );
        }
        for g in &pf.gpus {
            let _ = writeln!(out, "GPU {} '{}' util {:.1} ded {:.2}/{:.2}G shared {:.2}/{:.2}G temp {:?} drv '{}' date '{}' loc '{}' engines {:?}",
                g.index, g.name, g.util, g.ded_used as f64/1073741824.0, g.ded_total as f64/1073741824.0, g.shared_used as f64/1073741824.0, g.shared_total as f64/1073741824.0,
                g.temp_c, g.driver_version, g.driver_date, g.location, g.engines.iter().map(|e| format!("{}={:.1}", e.name, e.util)).collect::<Vec<_>>());
        }
        let _ = writeln!(out, "uptime {} s", pf.uptime_s);
    }
    std::fs::write(path, out).ok();
}
