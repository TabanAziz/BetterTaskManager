//! Invented data for screenshots (`--features demo`, `render out.bmp "demo=1;..."`).
//!
//! Screenshots of a real system would publish account names, addresses, network names
//! and whatever happens to be running. Nothing in here is measured.

use std::sync::Arc;

use super::App;
use crate::model::*;
use crate::pages::{history, services, startup, users, Data};

const GB: u64 = 1 << 30;
const MB: u64 = 1 << 20;
const SESSION: u32 = 1;

struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) as f32 / (1u32 << 24) as f32
    }

    /// A believable curve around `base`.
    fn series(&mut self, base: f32, swing: f32, spike: f32) -> Vec<f32> {
        let mut drift = 0.0;
        (0..HISTORY)
            .map(|_| {
                drift = drift * 0.65 + (self.next() - 0.5) * swing;
                let burst = if self.next() > 0.93 { spike * (0.4 + 0.6 * self.next()) } else { 0.0 };
                (base + drift + burst).max(0.0)
            })
            .collect()
    }
}

struct App_ {
    /// A file every Windows has (real name and icon) or "Name.exe" for an invented app.
    file: &'static str,
    count: u32,
    cpu: f32,
    mem_mb: f32,
    disk_mb: f32,
    net_mbit: f32,
    gpu: f32,
    eco: bool,
}

#[allow(clippy::too_many_arguments)]
fn app(
    file: &'static str,
    count: u32,
    cpu: f32,
    mem_mb: f32,
    disk_mb: f32,
    net_mbit: f32,
    gpu: f32,
    eco: bool,
) -> App_ {
    App_ { file, count, cpu, mem_mb, disk_mb, net_mbit, gpu, eco }
}

fn processes() -> Vec<Proc> {
    const EDGE: &str = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
    let apps = [
        app(EDGE, 18, 6.4, 2140.0, 1.8, 14.2, 7.1, true),
        app("Voxel Engine Editor.exe", 4, 17.2, 3480.0, 12.6, 0.1, 38.4, false),
        app("Code Studio.exe", 9, 3.1, 1260.0, 0.6, 0.2, 1.2, false),
        app("C:\\Windows\\explorer.exe", 1, 1.4, 236.0, 0.2, 0.0, 0.6, false),
        app("Chat.exe", 6, 1.2, 612.0, 0.1, 0.8, 0.4, true),
        app("Music Player.exe", 3, 0.8, 284.0, 0.1, 1.6, 0.0, false),
        app("C:\\Windows\\System32\\dwm.exe", 1, 2.2, 142.0, 0.0, 0.0, 9.8, false),
        app("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", 2, 4.6, 168.0, 3.4, 0.0, 0.0, false),
        app("C:\\Windows\\System32\\SearchIndexer.exe", 1, 0.6, 54.0, 2.1, 0.0, 0.0, false),
        app("C:\\Windows\\System32\\notepad.exe", 1, 0.0, 38.0, 0.0, 0.0, 0.0, false),
        app("C:\\Windows\\System32\\cmd.exe", 1, 0.0, 4.0, 0.0, 0.0, 0.0, false),
        app("C:\\Windows\\regedit.exe", 1, 0.0, 12.0, 0.0, 0.0, 0.0, false),
        app("C:\\Windows\\System32\\SecurityHealthSystray.exe", 1, 0.0, 3.0, 0.0, 0.0, 0.0, false),
        app("C:\\Windows\\System32\\RuntimeBroker.exe", 1, 0.2, 22.0, 0.0, 0.0, 0.0, false),
        app("C:\\Windows\\System32\\spoolsv.exe", 1, 0.0, 9.0, 0.0, 0.0, 0.0, false),
        app("C:\\Windows\\System32\\dllhost.exe", 1, 0.0, 7.0, 0.0, 0.0, 0.0, false),
        app("C:\\Windows\\System32\\mmc.exe", 1, 0.1, 31.0, 0.0, 0.0, 0.0, false),
        app("C:\\Windows\\System32\\msinfo32.exe", 1, 0.0, 18.0, 0.0, 0.0, 0.0, false),
    ];
    let engine: Arc<str> = "GPU 0 - 3D".into();
    let mut list = Vec::new();
    let mut pid = 1200;
    for a in &apps {
        let installed = a.file.contains('\\') && std::path::Path::new(a.file).exists();
        let exe: Arc<str> = a.file.rsplit('\\').next().unwrap_or(a.file).into();
        let root = pid;
        for i in 0..a.count {
            // The first process carries about a third, like a browser's main process.
            let part = match (a.count, i) {
                (1, _) => 1.0,
                (_, 0) => 0.34,
                (n, _) => 0.66 / (n - 1) as f32,
            };
            list.push(Proc {
                pid,
                ppid: if i == 0 { 0 } else { root },
                ctime: u64::from(pid) * 1000,
                exe: exe.clone(),
                meta: installed.then(|| crate::meta::get(a.file)),
                package: None,
                session: SESSION,
                threads: 12 + i * 3,
                handles: 300 + i * 40,
                priority: 8,
                cpu: a.cpu * part,
                cpu_time: (a.cpu * part * 9e8) as u64,
                mem: (a.mem_mb * part * MB as f32) as u64,
                commit: (a.mem_mb * part * 1.3 * MB as f32) as u64,
                ws: (a.mem_mb * part * 1.1 * MB as f32) as u64,
                disk: Some(a.disk_mb * part * MB as f32),
                net: Some(a.net_mbit * part * 125_000.0),
                gpu: a.gpu * part,
                gpu_engine: (a.gpu > 0.0).then(|| engine.clone()),
                eco: a.eco,
                suspended: false,
                services: Vec::new(),
                windows: Vec::new(),
                user: Some("demo".into()),
                kind: ProcKind::Normal,
                accessible: true,
            });
            pid += 4;
        }
    }
    list
}

fn perf() -> Perf {
    let mut n = Noise(7);
    let cpu = CpuInfo {
        name: "Demo 8-Core Processor".into(),
        base_mhz: 3800,
        sockets: 1,
        cores: 8,
        logical: 16,
        l1: 512 * 1024,
        l2: 4 * MB,
        l3: 32 * MB,
        virtualization: Some(true),
        util: 38.0,
        speed_mhz: 4420.0,
        hist: n.series(36.0, 22.0, 35.0),
        lp_hist: (0..16).map(|i| n.series(16.0 + (i % 5) as f32 * 9.0, 22.0, 30.0)).collect(),
        lp_util: vec![38.0; 16],
    };
    let mem = MemInfo {
        total: 32 * GB - 120 * MB,
        avail: 13 * GB,
        in_use: 18 * GB,
        compressed: 640 * MB,
        committed: 27 * GB,
        commit_limit: 40 * GB,
        cached: 11 * GB,
        paged_pool: 820 * MB,
        nonpaged_pool: 610 * MB,
        modified: 700 * MB,
        standby: 10 * GB,
        free: 3 * GB,
        hw_reserved: 120 * MB,
        speed_mts: 3200,
        slots_used: 2,
        slots_total: 4,
        form_factor: "DIMM".into(),
        hist: n.series(57.0, 2.0, 0.0),
    };
    let disk = DiskInfo {
        index: 0,
        letters: "C:".into(),
        model: "Demo NVMe SSD 1 TB".into(),
        kind: "SSD (NVMe)".into(),
        capacity: 931 * GB,
        formatted: 930 * GB,
        system: true,
        pagefile: true,
        active: 14.0,
        resp_ms: 0.3,
        read_bps: 9.4 * MB as f32,
        write_bps: 11.8 * MB as f32,
        hist_active: n.series(14.0, 9.0, 50.0),
        hist_read: n.series(9.0 * MB as f32, 8.0 * MB as f32, 30.0 * MB as f32),
        hist_write: n.series(6.0 * MB as f32, 6.0 * MB as f32, 26.0 * MB as f32),
    };
    let net = NetInfo {
        luid: 1,
        title: "Ethernet".into(),
        alias: "Ethernet".into(),
        description: "Demo 2.5 GbE Controller".into(),
        conn_type: "Ethernet".into(),
        // RFC 5737 / RFC 3849 documentation addresses.
        ipv4: "192.0.2.24".into(),
        ipv6: "2001:db8::24".into(),
        link_bps: 2_500_000_000,
        send_bps: 2.1e6,
        recv_bps: 14.8e6,
        hist_send: n.series(2.2e6, 1.8e6, 6e6),
        hist_recv: n.series(12e6, 8e6, 22e6),
        ..Default::default()
    };
    let engines = [("3D", 42.0, 18.0), ("Copy", 6.0, 8.0), ("Video Encode", 12.0, 6.0), ("Video Decode", 22.0, 10.0)]
        .into_iter()
        .map(|(name, util, swing)| GpuEngine { name: name.into(), util, hist: n.series(util, swing, 14.0) })
        .collect();
    let gpu = GpuInfo {
        index: 0,
        luid: 1,
        name: "Demo Graphics 12 GB".into(),
        util: 42.0,
        engines,
        ded_used: 5 * GB,
        ded_total: 12 * GB,
        shared_used: 400 * MB,
        shared_total: 16 * GB,
        temp_c: Some(54.0),
        driver_version: "32.0.15.0000".into(),
        driver_date: "01.10.2026".into(),
        location: "PCI bus 1, device 0, function 0".into(),
        hist_util: n.series(42.0, 18.0, 14.0),
        hist_ded: n.series(42.0, 2.0, 0.0),
        hist_shared: n.series(3.0, 0.4, 0.0),
    };
    Perf { cpu, mem, disks: vec![disk], nets: vec![net], gpus: vec![gpu], uptime_s: 30_127 }
}

fn pages() -> Vec<Data> {
    let usage = |name: &str, down_mb: u64, up_mb: u64| history::AppUsage {
        key: name.to_lowercase(),
        name: name.into(),
        meta: None,
        received: down_mb * MB,
        sent: up_mb * MB,
    };
    let entry = |name: &str, publisher: &str, impact: startup::Impact, enabled: bool| startup::Entry {
        source: startup::Source::UserRun,
        id: name.into(),
        name: name.into(),
        publisher: publisher.into(),
        command: format!("\"C:\\Program Files\\{name}\\{name}.exe\" --autostart"),
        meta: None,
        enabled,
        impact,
    };
    let service = |name: &str, display: &str, pid: u32, group: &str| services::Service {
        name: name.into(),
        display: display.into(),
        pid,
        state: if pid == 0 { services::State::Stopped } else { services::State::Running },
        can_stop: pid != 0,
        group: group.into(),
    };
    use startup::Impact::*;
    vec![
        Data::History(Ok(vec![
            usage("Microsoft Edge", 48_200, 3_100),
            usage("Chat", 6_400, 2_800),
            usage("Music Player", 4_100, 120),
            usage("Code Studio", 1_900, 640),
        ])),
        Data::Startup(vec![
            entry("Chat", "Example Apps", High, true),
            entry("Music Player", "Example Audio", Medium, false),
            entry("Cloud Sync", "Example Cloud", Low, true),
        ]),
        Data::Users(vec![users::Session { id: SESSION, name: "demo".into(), disconnected: false }]),
        Data::Services(Ok(vec![
            service("AudioSrv", "Windows Audio", 3120, "LocalServiceNetworkRestricted"),
            service("BITS", "Background Intelligent Transfer Service", 0, "netsvcs"),
            service("EventLog", "Windows Event Log", 2044, "LocalServiceNetworkRestricted"),
            service("Spooler", "Print Spooler", 4312, ""),
        ])),
    ]
}

/// Replaces everything that is shown; call again right before drawing.
pub fn install(a: &mut App) {
    let procs = processes();
    let snapshot = Snapshot {
        seq: 4,
        n_procs: procs.len() as u32,
        n_threads: 2841,
        n_handles: 98_214,
        procs,
        cpu_pct: 38.0,
        mem_pct: 57.0,
        disk_pct: 14.0,
        gpu_pct: 42.0,
        net_pct: 1.0,
        perf: perf(),
        helper: HelperState::Running,
        sample_ms: 0.0,
    };
    a.snap = Some(Box::new(snapshot));
    a.settings.interval_ms = 1000;
    // Not the column layout saved on this PC.
    a.procs = super::procs::Grid::processes();
    a.details = super::procs::Grid::details();
    a.lists = super::lists::Lists::new();
    super::lists::show(a, pages());
}
