// Processes page (Task Manager style app groups with heat map) and Details page
// (flat process list). Both use the same custom-drawn grid.

use std::collections::HashMap;
use std::sync::Arc;

use windows_sys::Win32::Foundation::{CloseHandle, HWND, POINT, RECT};
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::{SHObjectProperties, SHOP_FILEPATH};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::gfx::*;
use super::{App, Hit, Page, Tool, G_CHEV_D, G_CHEV_R, G_CHEV_U, G_CLOSE, G_END, G_LEAF, G_MORE, G_RUN, G_SHIELD};
use crate::model::{Proc, ProcKind};
use crate::util::{fmt_int, fmt_min_step, fmt_num, wide};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ColId {
    Name,
    Status,
    Publisher,
    Pid,
    ProcName,
    Cpu,
    Mem,
    Disk,
    Gpu,
    Net,
    GpuEngine,
    // Details
    DName,
    DPid,
    DStatus,
    DUser,
    DCpu,
    DMem,
    DThreads,
    DHandles,
    DDesc,
}

pub struct Col {
    pub id: ColId,
    pub title: &'static str,
    pub w: f32,
    pub right: bool,
    pub visible: bool,
    pub heat: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum GroupKey {
    Pkg(Arc<str>),
    Root(u32, u64),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum RowKey {
    Group(GroupKey),
    Proc(u32, u64),
    Win(isize),
    Svc(u32, Arc<str>),
}

#[derive(Clone)]
pub struct Row {
    pub key: RowKey,
    pub depth: u8,
    pub expandable: bool,
    pub expanded: bool,
    pub name: String,
    pub icon: isize,
    pub eco: bool,
    pub suspended: bool,
    pub cpu: f32,
    pub mem: u64,
    pub disk: Option<f32>,
    pub net: Option<f32>,
    pub gpu: f32,
    pub gpu_engine: String,
    pub publisher: String,
    pub pid: Option<u32>,
    pub procname: String,
    pub procs: Vec<(u32, u64)>,
    pub values: bool,
    pub user: String,
    pub threads: u32,
    pub handles: u32,
    pub path: String,
    pub hwnd: isize,
    pub group: Option<GroupKey>,
}

pub struct Grid {
    pub cols: Vec<Col>,
    pub sort: ColId,
    pub desc: bool,
    pub scroll: f32,
    pub sel: Option<RowKey>,
    pub rows: Vec<Row>,
}

fn col(id: ColId, title: &'static str, w: f32, right: bool, visible: bool, heat: bool) -> Col {
    Col { id, title, w, right, visible, heat }
}

impl Grid {
    pub fn processes() -> Grid {
        Grid {
            cols: vec![
                col(ColId::Name, "Name", 234.0, false, true, false),
                col(ColId::Status, "Status", 120.0, false, true, false),
                col(ColId::Publisher, "Publisher", 140.0, false, false, false),
                col(ColId::Pid, "PID", 64.0, true, false, false),
                col(ColId::ProcName, "Process name", 140.0, false, false, false),
                col(ColId::Cpu, "CPU", 74.0, true, true, true),
                col(ColId::Mem, "Memory", 74.0, true, true, true),
                col(ColId::Disk, "Disk", 74.0, true, true, true),
                col(ColId::Gpu, "GPU", 74.0, true, true, true),
                col(ColId::Net, "Network", 80.0, true, true, true),
                col(ColId::GpuEngine, "GPU engine", 110.0, false, false, false),
            ],
            sort: ColId::Cpu,
            desc: true,
            scroll: 0.0,
            sel: None,
            rows: Vec::new(),
        }
    }
    pub fn details() -> Grid {
        Grid {
            cols: vec![
                col(ColId::DName, "Name", 230.0, false, true, false),
                col(ColId::DPid, "PID", 70.0, true, true, false),
                col(ColId::DStatus, "Status", 125.0, false, true, false),
                col(ColId::DUser, "User name", 120.0, false, true, false),
                col(ColId::DCpu, "CPU", 48.0, true, true, false),
                col(ColId::DMem, "Memory (private)", 160.0, true, true, false),
                col(ColId::DThreads, "Threads", 70.0, true, false, false),
                col(ColId::DHandles, "Handles", 70.0, true, false, false),
                col(ColId::DDesc, "Description", 260.0, false, true, false),
            ],
            sort: ColId::DName,
            desc: false,
            scroll: 0.0,
            sel: None,
            rows: Vec::new(),
        }
    }
}

pub fn row_h(a: &App) -> i32 {
    a.px(28.0)
}

fn header_h(a: &App, page: Page) -> i32 {
    if page == Page::Procs {
        a.px(52.0)
    } else {
        a.px(36.0)
    }
}

fn grid(a: &mut App, page: Page) -> &mut Grid {
    if page == Page::Procs {
        &mut a.procs
    } else {
        &mut a.details
    }
}

// ---------------------------------------------------------------------- row building

fn proc_row(p: &Proc, depth: u8, group: Option<GroupKey>) -> Row {
    Row {
        key: RowKey::Proc(p.pid, p.ctime),
        depth,
        expandable: false,
        expanded: false,
        name: p.display_name(),
        icon: p.icon(),
        eco: p.eco,
        suspended: p.suspended,
        cpu: p.cpu,
        mem: p.mem,
        disk: p.disk,
        net: p.net,
        gpu: p.gpu,
        gpu_engine: p.gpu_engine.as_deref().unwrap_or("").to_string(),
        publisher: p.publisher().to_string(),
        pid: if p.kind == ProcKind::Normal || p.kind == ProcKind::Idle { Some(p.pid) } else { None },
        procname: p.exe.to_string(),
        procs: if p.kind == ProcKind::Normal { vec![(p.pid, p.ctime)] } else { Vec::new() },
        values: true,
        user: p.user.as_deref().unwrap_or("").to_string(),
        threads: p.threads,
        handles: p.handles,
        path: p.path().to_string(),
        hwnd: p.windows.first().map(|w| w.hwnd).unwrap_or(0),
        group,
    }
}

fn child_row(key: RowKey, name: String, icon: isize, group: GroupKey, hwnd: isize) -> Row {
    Row {
        key,
        depth: 1,
        expandable: false,
        expanded: false,
        name,
        icon,
        eco: false,
        suspended: false,
        cpu: 0.0,
        mem: 0,
        disk: None,
        net: None,
        gpu: 0.0,
        gpu_engine: String::new(),
        publisher: String::new(),
        pid: None,
        procname: String::new(),
        procs: Vec::new(),
        values: false,
        user: String::new(),
        threads: 0,
        handles: 0,
        path: String::new(),
        hwnd,
        group: Some(group),
    }
}

fn cmp_rows(a: &Row, b: &Row, sort: ColId, desc: bool) -> std::cmp::Ordering {
    let name = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
    let f = |x: f32, y: f32| x.total_cmp(&y);
    let o = match sort {
        ColId::Name | ColId::DDesc => name(),
        ColId::DName => a.procname.to_lowercase().cmp(&b.procname.to_lowercase()),
        ColId::Status | ColId::DStatus => {
            (a.eco as u8 + 2 * a.suspended as u8).cmp(&(b.eco as u8 + 2 * b.suspended as u8))
        }
        ColId::Publisher => a.publisher.to_lowercase().cmp(&b.publisher.to_lowercase()),
        ColId::Pid | ColId::DPid => a.pid.unwrap_or(0).cmp(&b.pid.unwrap_or(0)),
        ColId::ProcName => a.procname.to_lowercase().cmp(&b.procname.to_lowercase()),
        ColId::Cpu | ColId::DCpu => f(a.cpu, b.cpu),
        ColId::Mem | ColId::DMem => a.mem.cmp(&b.mem),
        ColId::Disk => f(a.disk.unwrap_or(-1.0), b.disk.unwrap_or(-1.0)),
        ColId::Gpu => f(a.gpu, b.gpu),
        ColId::Net => f(a.net.unwrap_or(-1.0), b.net.unwrap_or(-1.0)),
        ColId::GpuEngine => a.gpu_engine.cmp(&b.gpu_engine),
        ColId::DUser => a.user.to_lowercase().cmp(&b.user.to_lowercase()),
        ColId::DThreads => a.threads.cmp(&b.threads),
        ColId::DHandles => a.handles.cmp(&b.handles),
    };
    let o = if desc { o.reverse() } else { o };
    o.then_with(name)
}

fn matches(p: &Proc, needle: &str, name: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    name.to_lowercase().contains(needle)
        || p.exe.to_lowercase().contains(needle)
        || p.publisher().to_lowercase().contains(needle)
        || p.pid.to_string().contains(needle)
}

pub fn rebuild(a: &mut App) {
    let Some(snap) = a.snap.as_ref() else { return };
    let needle = a.search.trim().to_lowercase();
    match a.page {
        Page::Procs => {
            let rows = build_groups(&snap.procs, &needle, &a.expanded, a.procs.sort, a.procs.desc);
            a.procs.rows = rows;
            clamp_scroll(a, Page::Procs);
        }
        Page::Details => {
            let mut rows: Vec<Row> = snap
                .procs
                .iter()
                .filter(|p| p.kind != ProcKind::Interrupts)
                .filter(|p| matches(p, &needle, &p.display_name()))
                .map(|p| {
                    let mut r = proc_row(p, 0, None);
                    if p.kind == ProcKind::Idle {
                        r.procname = "System Idle Process".into();
                        r.name = "Percentage of time the processor is idle".into();
                        r.procs.clear();
                    } else if p.pid == 4 {
                        r.name = "NT Kernel & System".into();
                    }
                    if r.user.is_empty() && p.pid <= 4 {
                        r.user = "SYSTEM".into();
                    }
                    r
                })
                .collect();
            let (s, d) = (a.details.sort, a.details.desc);
            rows.sort_by(|x, y| cmp_rows(x, y, s, d));
            a.details.rows = rows;
            clamp_scroll(a, Page::Details);
        }
        _ => {}
    }
}

fn is_console_host(exe: &str) -> bool {
    exe.eq_ignore_ascii_case("conhost.exe") || exe.eq_ignore_ascii_case("OpenConsole.exe")
}

fn build_groups(
    procs: &[Proc],
    needle: &str,
    expanded: &std::collections::HashSet<GroupKey>,
    sort: ColId,
    desc: bool,
) -> Vec<Row> {
    let list: Vec<&Proc> = procs.iter().filter(|p| p.kind != ProcKind::Idle).collect();
    let idx: HashMap<u32, usize> = list.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();
    // Group key per process: packaged apps by package; desktop apps by chains of the
    // same executable (chrome.exe -> chrome.exe ...); console hosts join their client.
    let mut keys: Vec<Option<GroupKey>> = vec![None; list.len()];
    fn key_of(
        i: usize,
        list: &[&Proc],
        idx: &HashMap<u32, usize>,
        keys: &mut Vec<Option<GroupKey>>,
        depth: u32,
    ) -> GroupKey {
        if let Some(k) = &keys[i] {
            return k.clone();
        }
        let p = list[i];
        let k = if let Some(pkg) = &p.package {
            GroupKey::Pkg(pkg.clone())
        } else if depth < 64 && p.ppid != 0 && idx.contains_key(&p.ppid) {
            let pi = idx[&p.ppid];
            let parent = list[pi];
            let same = parent.exe.eq_ignore_ascii_case(&p.exe) && parent.package.is_none();
            let console = is_console_host(&p.exe) && parent.pid > 4 && !parent.exe.eq_ignore_ascii_case("csrss.exe");
            if same || console {
                key_of(pi, list, idx, keys, depth + 1)
            } else {
                GroupKey::Root(p.pid, p.ctime)
            }
        } else {
            GroupKey::Root(p.pid, p.ctime)
        };
        keys[i] = Some(k.clone());
        k
    }
    for i in 0..list.len() {
        key_of(i, &list, &idx, &mut keys, 0);
    }
    let mut groups: Vec<(GroupKey, Vec<usize>)> = Vec::new();
    let mut gidx: HashMap<GroupKey, usize> = HashMap::new();
    for (i, k) in keys.into_iter().enumerate() {
        let k = k.unwrap();
        match gidx.get(&k) {
            Some(&g) => groups[g].1.push(i),
            None => {
                gidx.insert(k.clone(), groups.len());
                groups.push((k, vec![i]));
            }
        }
    }

    let mut out_groups: Vec<(Row, Vec<Row>)> = Vec::with_capacity(groups.len());
    for (key, members) in groups {
        // Root: the member whose parent is not in the group; prefer one with a window.
        let in_group: std::collections::HashSet<u32> = members.iter().map(|&i| list[i].pid).collect();
        let root_i = members
            .iter()
            .copied()
            .filter(|&i| !in_group.contains(&list[i].ppid))
            .min_by_key(|&i| (list[i].windows.is_empty(), list[i].ctime))
            .unwrap_or(members[0]);
        let root = list[root_i];
        let base_name = root.display_name();
        let n = members.len();
        let name = if n > 1 { format!("{} ({})", base_name, n) } else { base_name.clone() };
        let grp_match = needle.is_empty() || {
            let l = name.to_lowercase();
            l.contains(needle) || root.publisher().to_lowercase().contains(needle)
        };
        let mut children: Vec<Row> = Vec::new();
        let mut any_child_match = false;
        if n > 1 {
            for &i in &members {
                let p = list[i];
                let dn = p.display_name();
                let m = matches(p, needle, &dn);
                any_child_match |= m;
                if grp_match || m {
                    children.push(proc_row(p, 1, Some(key.clone())));
                }
            }
        } else {
            any_child_match = matches(root, needle, &base_name);
            if !root.services.is_empty() {
                for s in &root.services {
                    children.push(child_row(
                        RowKey::Svc(root.pid, s.clone()),
                        s.to_string(),
                        root.icon(),
                        key.clone(),
                        0,
                    ));
                }
            } else {
                for w in &root.windows {
                    children.push(child_row(
                        RowKey::Win(w.hwnd),
                        w.title.to_string(),
                        root.icon(),
                        key.clone(),
                        w.hwnd,
                    ));
                }
            }
        }
        if !(grp_match || any_child_match) {
            continue;
        }
        // Aggregate.
        let mut g = proc_row(root, 0, Some(key.clone()));
        g.key =
            if n > 1 || !children.is_empty() { RowKey::Group(key.clone()) } else { RowKey::Proc(root.pid, root.ctime) };
        g.name = name;
        g.expandable = !children.is_empty();
        g.expanded = g.expandable && (expanded.contains(&key) || (!needle.is_empty() && !grp_match && n > 1));
        if n > 1 {
            g.cpu = 0.0;
            g.mem = 0;
            g.gpu = 0.0;
            g.disk = None;
            g.net = None;
            g.eco = false;
            g.suspended = true;
            g.procs.clear();
            g.pid = None;
            let mut best_gpu = (0f32, String::new());
            for &i in &members {
                let p = list[i];
                g.cpu += p.cpu;
                g.mem += p.mem;
                g.gpu += p.gpu;
                if p.gpu > best_gpu.0 {
                    best_gpu = (p.gpu, p.gpu_engine.as_deref().unwrap_or("").to_string());
                }
                if let Some(d) = p.disk {
                    g.disk = Some(g.disk.unwrap_or(0.0) + d);
                }
                if let Some(nv) = p.net {
                    g.net = Some(g.net.unwrap_or(0.0) + nv);
                }
                g.eco |= p.eco;
                g.suspended &= p.suspended;
                g.procs.push((p.pid, p.ctime));
            }
            g.cpu = g.cpu.min(100.0);
            g.gpu = g.gpu.min(100.0);
            g.gpu_engine = best_gpu.1;
        }
        children.sort_by(|x, y| if x.values { cmp_rows(x, y, sort, desc) } else { x.name.cmp(&y.name) });
        out_groups.push((g, children));
    }
    out_groups.sort_by(|x, y| cmp_rows(&x.0, &y.0, sort, desc));
    let mut rows = Vec::with_capacity(out_groups.len() + 32);
    for (g, ch) in out_groups {
        let exp = g.expanded;
        rows.push(g);
        if exp {
            rows.extend(ch);
        }
    }
    rows
}

// ---------------------------------------------------------------------- formatting

fn heat_level(id: ColId, r: &Row, a: &App) -> usize {
    let mem_total = a.snap.as_ref().map(|s| s.perf.mem.total).unwrap_or(1).max(1) as f64;
    let (v, t): (f64, [f64; 5]) = match id {
        ColId::Cpu => (r.cpu as f64, [1.0, 15.0, 30.0, 50.0, 75.0]),
        ColId::Gpu => (r.gpu as f64, [1.0, 15.0, 30.0, 50.0, 75.0]),
        ColId::Mem => (r.mem as f64 / mem_total * 100.0, [1.5, 15.0, 30.0, 50.0, 75.0]),
        ColId::Disk => (r.disk.unwrap_or(0.0) as f64 / 1048576.0, [1.5, 10.0, 50.0, 100.0, 300.0]),
        ColId::Net => (r.net.unwrap_or(0.0) as f64 * 8.0 / 1e6, [1.0, 10.0, 50.0, 100.0, 500.0]),
        _ => return 0,
    };
    t.iter().filter(|&&x| v >= x).count()
}

fn cell_text(id: ColId, r: &Row) -> String {
    if !r.values {
        return String::new();
    }
    match id {
        ColId::Cpu => format!("{}%", fmt_min_step(r.cpu as f64, 1)),
        ColId::Mem => format!("{} MB", fmt_min_step(r.mem as f64 / 1048576.0, 1)),
        ColId::Disk => match r.disk {
            Some(d) => format!("{} MB/s", fmt_min_step(d as f64 / 1048576.0, 1)),
            None => String::new(),
        },
        ColId::Gpu => format!("{}%", fmt_min_step(r.gpu as f64, 1)),
        ColId::Net => match r.net {
            Some(n) => format!("{} Mbps", fmt_min_step(n as f64 * 8.0 / 1e6, 1)),
            None => String::new(),
        },
        ColId::GpuEngine => r.gpu_engine.clone(),
        ColId::Publisher => r.publisher.clone(),
        ColId::Pid | ColId::DPid => r.pid.map(|p| p.to_string()).unwrap_or_default(),
        ColId::ProcName => r.procname.clone(),
        ColId::DName => r.procname.clone(),
        ColId::DStatus => {
            if r.suspended {
                "Suspended".into()
            } else {
                "Running".into()
            }
        }
        ColId::DUser => r.user.clone(),
        ColId::DCpu => format!("{:02}", r.cpu.round() as i64),
        ColId::DMem => format!("{} K", fmt_int(r.mem / 1024)),
        ColId::DThreads => fmt_int(r.threads as u64),
        ColId::DHandles => fmt_int(r.handles as u64),
        ColId::DDesc => r.name.clone(),
        ColId::Name | ColId::Status => String::new(),
    }
}

fn header_value(a: &App, id: ColId) -> Option<f32> {
    let s = a.snap.as_ref()?;
    match id {
        ColId::Cpu => Some(s.cpu_pct),
        ColId::Mem => Some(s.mem_pct),
        ColId::Disk => Some(s.disk_pct),
        ColId::Gpu => Some(s.gpu_pct),
        ColId::Net => Some(s.net_pct),
        _ => None,
    }
}

// ---------------------------------------------------------------------- layout

/// Visible columns with their pixel x offsets (relative to grid x) and widths.
fn layout_cols(a: &App, page: Page, avail: i32) -> Vec<(usize, i32, i32)> {
    let g = if page == Page::Procs { &a.procs } else { &a.details };
    let vis: Vec<usize> = (0..g.cols.len()).filter(|&i| g.cols[i].visible).collect();
    let mut widths: Vec<i32> = vis.iter().map(|&i| a.px(g.cols[i].w)).collect();
    // Too wide for the window: first narrow the last (description-like) column, then the
    // name column, so the important columns stay readable.
    let mut over = widths.iter().sum::<i32>() - avail;
    if over > 0 && page == Page::Details && widths.len() > 1 {
        let last = widths.len() - 1;
        let take = over.min((widths[last] - a.px(80.0)).max(0));
        widths[last] -= take;
        over -= take;
    }
    if over > 0 && !widths.is_empty() {
        let take = over.min((widths[0] - a.px(120.0)).max(0));
        widths[0] -= take;
    }
    let mut x = 0;
    let mut v = Vec::with_capacity(vis.len());
    for (k, &i) in vis.iter().enumerate() {
        v.push((i, x, widths[k]));
        x += widths[k];
    }
    v
}

pub fn clamp_scroll(a: &mut App, page: Page) {
    let (_, h) = a.client();
    let view = (h - a.title_h() - a.px(52.0) - header_h(a, page) - info_bar_h(a, page)).max(1) as f32;
    let rh = row_h(a) as f32;
    let g = grid(a, page);
    let content = g.rows.len() as f32 * rh;
    let max = (content - view).max(0.0);
    g.scroll = g.scroll.clamp(0.0, max);
}

fn info_bar_h(a: &App, page: Page) -> i32 {
    if page == Page::Procs && show_info_bar(a) {
        a.px(44.0)
    } else {
        0
    }
}

fn show_info_bar(a: &App) -> bool {
    !a.settings.hint_dismissed
        && a.snap.as_ref().map(|s| s.helper == crate::model::HelperState::NotInstalled).unwrap_or(false)
}

// ---------------------------------------------------------------------- drawing

impl App {
    pub fn draw_procs_page(&mut self, x: i32, y: i32, w: i32, h: i32) {
        let sel_ok = self.procs.sel.is_some();
        let sel_eco = self.selected_row(Page::Procs).map(|r| r.eco).unwrap_or(false);
        let sel_has_procs = self.selected_row(Page::Procs).map(|r| !r.procs.is_empty()).unwrap_or(false);
        let tools: Vec<(Tool, &str, &str, bool)> = vec![
            (Tool::RunTask, G_RUN, "Run new task", true),
            (Tool::EndTask, G_END, "End task", sel_ok && sel_has_procs),
            (
                Tool::Efficiency,
                G_LEAF,
                if sel_eco { "Efficiency mode off" } else { "Efficiency mode" },
                sel_ok && sel_has_procs,
            ),
            (Tool::More, G_MORE, "", true),
        ];
        let gy = self.page_header(x, y, w, "Processes", &tools);
        let mut gy = gy;
        if show_info_bar(self) {
            gy = self.draw_info_bar(x, gy, w);
        }
        self.draw_grid(Page::Procs, x, gy, w, y + h - gy);
        self.draw_toast(x, y, w, h);
    }

    pub fn draw_details_page(&mut self, x: i32, y: i32, w: i32, h: i32) {
        let sel_ok = self.details.sel.is_some();
        let sel_has_procs = self.selected_row(Page::Details).map(|r| !r.procs.is_empty()).unwrap_or(false);
        let sel_eco = self.selected_row(Page::Details).map(|r| r.eco).unwrap_or(false);
        let tools: Vec<(Tool, &str, &str, bool)> = vec![
            (Tool::RunTask, G_RUN, "Run new task", true),
            (Tool::EndTask, G_END, "End task", sel_ok && sel_has_procs),
            (
                Tool::Efficiency,
                G_LEAF,
                if sel_eco { "Efficiency mode off" } else { "Efficiency mode" },
                sel_ok && sel_has_procs,
            ),
            (Tool::More, G_MORE, "", true),
        ];
        let gy = self.page_header(x, y, w, "Details", &tools);
        self.draw_grid(Page::Details, x, gy, w, y + h - gy);
        self.draw_toast(x, y, w, h);
    }

    fn draw_info_bar(&mut self, x: i32, y: i32, w: i32) -> i32 {
        let bh = self.px(36.0);
        let bx = x + self.px(12.0);
        let bw = w - self.px(24.0);
        let bg = if self.th.dark { 0x2B2B2B } else { 0xF0F0F0 };
        self.cv.round_rect(bx as f32, y as f32, bw as f32, bh as f32, self.px(4.0) as f32, bg, 1.0);
        let fi = self.f.icon;
        let acc = self.th.accent;
        self.cv.text(fi, G_SHIELD, (bx + self.px(10.0), y, self.px(18.0), bh), acc, DT_C);
        let fb = self.f.body;
        let t2 = self.th.text;
        let msg = "Per-process disk values need administrator permission once.";
        let btn_w = self.cv.text_width(fb, "Set up") + self.px(24.0);
        let close_w = self.px(32.0);
        self.cv.text(fb, msg, (bx + self.px(38.0), y, bw - self.px(48.0) - btn_w - close_w, bh), t2, DT_L);
        let bx2 = bx + bw - close_w - btn_w - self.px(6.0);
        let hov = self.hover == Hit::InfoSetup;
        let c = if hov { self.th.accent } else { self.th.accent_strong };
        self.cv.round_rect(
            bx2 as f32,
            (y + self.px(4.0)) as f32,
            btn_w as f32,
            (bh - self.px(8.0)) as f32,
            self.px(4.0) as f32,
            c,
            1.0,
        );
        self.cv.text(fb, "Set up", (bx2, y, btn_w, bh), 0x000000, DT_C);
        self.hit(bx2, y + self.px(4.0), btn_w, bh - self.px(8.0), Hit::InfoSetup);
        let cx = bx + bw - close_w;
        if self.hover == Hit::InfoClose {
            self.cv.round_rect(
                cx as f32,
                (y + self.px(4.0)) as f32,
                (close_w - self.px(4.0)) as f32,
                (bh - self.px(8.0)) as f32,
                self.px(4.0) as f32,
                0x3A3A3A,
                1.0,
            );
        }
        let fs = self.f.icon_small;
        let tc = self.th.text2;
        self.cv.text(fs, G_CLOSE, (cx, y, close_w - self.px(4.0), bh), tc, DT_C);
        self.hit(cx, y, close_w, bh, Hit::InfoClose);
        y + bh + self.px(8.0)
    }

    pub fn draw_toast(&mut self, x: i32, y: i32, w: i32, h: i32) {
        let Some((msg, t)) = self.busy_note.clone() else { return };
        let now = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
        if now - t > 4000 {
            self.busy_note = None;
            return;
        }
        let fb = self.f.body;
        let tw = self.cv.text_width(fb, &msg) + self.px(32.0);
        let bh = self.px(36.0);
        let bx = x + (w - tw) / 2;
        let by = y + h - bh - self.px(16.0);
        let bg = if self.th.dark { 0x2C2C2C } else { 0xFFFFFF };
        self.cv.round_rect(bx as f32, by as f32, tw as f32, bh as f32, self.px(6.0) as f32, bg, 1.0);
        self.cv.round_rect_outline(bx as f32, by as f32, tw as f32, bh as f32, self.px(6.0) as f32, 0x444444, 1.0);
        let tc = self.th.text;
        self.cv.text(fb, &msg, (bx, by, tw, bh), tc, DT_C);
        unsafe { SetTimer(self.hwnd, 99, 4100, Some(toast_timer)) };
    }

    pub fn selected_row(&self, page: Page) -> Option<&Row> {
        let g = if page == Page::Procs { &self.procs } else { &self.details };
        let k = g.sel.as_ref()?;
        g.rows.iter().find(|r| &r.key == k)
    }

    fn draw_grid(&mut self, page: Page, x: i32, y: i32, w: i32, h: i32) {
        let hh = header_h(self, page);
        let rh = row_h(self);
        let sb_w = self.px(12.0);
        let cols = layout_cols(self, page, w - sb_w);
        let th_text = self.th.text;
        let th_text2 = self.th.text2;
        let divider = self.th.divider;
        let dark = self.th.dark;
        let (sort, desc) = {
            let g = if page == Page::Procs { &self.procs } else { &self.details };
            (g.sort, g.desc)
        };
        // ---------------- header
        self.cv.hline(x, y, w, divider);
        for &(ci, cx, cw) in &cols {
            let (id, title, right, heat) = {
                let g = if page == Page::Procs { &self.procs } else { &self.details };
                let c = &g.cols[ci];
                (c.id, c.title, c.right, c.heat)
            };
            let x0 = x + cx;
            let hv = header_value(self, id);
            if heat && hv.map(|v| v >= 75.0).unwrap_or(false) {
                let hc = self.th.header_hot;
                self.cv.fill(x0, y + 1, cw, hh - 1, hc);
            }
            if self.hover == Hit::Hdr(page, ci) {
                let oc = self.th.overlay;
                self.cv.blend(x0, y + 1, cw, hh - 1, oc, 0.04);
            }
            let pad = self.px(8.0);
            let lpad = if ci == 0 && page == Page::Procs { self.px(16.0) } else { pad };
            let fs = self.f.small;
            if heat && page == Page::Procs {
                let fv = self.f.hdr_val;
                let txt = hv.map(|v| format!("{}%", fmt_num(v.round() as f64, 0))).unwrap_or_default();
                self.cv.text(fv, &txt, (x0, y + self.px(6.0), cw - pad, self.px(26.0)), th_text, DT_R);
                self.cv.text(fs, title, (x0 + pad, y + self.px(30.0), cw - 2 * pad, self.px(18.0)), th_text, DT_R);
            } else {
                let ly = if page == Page::Procs { y + self.px(30.0) } else { y };
                let lh = if page == Page::Procs { self.px(18.0) } else { hh };
                let fl = if right { DT_R } else { DT_L };
                self.cv.text(fs, title, (x0 + lpad, ly, cw - lpad - pad, lh), th_text, fl);
            }
            if id == sort {
                let fsm = self.f.icon_tiny;
                let g = if desc { G_CHEV_D } else { G_CHEV_U };
                let gy = if page == Page::Procs { y + self.px(2.0) } else { y + self.px(1.0) };
                self.cv.text(fsm, g, (x0, gy, cw, self.px(10.0)), th_text2, DT_C);
            }
            self.cv.vline(x0 + cw - 1, y + 1, hh - 1, divider);
            self.hit(x0, y, cw, hh, Hit::Hdr(page, ci));
            let dw = self.px(4.0);
            self.hit(x0 + cw - dw, y, dw * 2, hh, Hit::HdrDiv(page, ci));
        }
        self.cv.hline(x, y + hh, w, divider);

        // ---------------- rows
        let body_y = y + hh + 1;
        let body_h = (h - hh - 1).max(0);
        let n = {
            let g = if page == Page::Procs { &self.procs } else { &self.details };
            g.rows.len()
        };
        let scroll = {
            let g = if page == Page::Procs { &self.procs } else { &self.details };
            g.scroll
        };
        self.cv.clip_rect(Some((x, body_y, w, body_h)));
        let first = (scroll / rh as f32).floor().max(0.0) as usize;
        let off = -(scroll - first as f32 * rh as f32).round() as i32;
        let visible = (body_h / rh + 2) as usize;
        let fbody = self.f.body;
        let fi = self.f.icon;
        let fsm = self.f.icon_small;
        let icon_px = (16.0 * self.s).round() as i32;
        let def_icon = crate::meta::default_icon_if_loaded();
        for vi in 0..visible {
            let ri = first + vi;
            if ri >= n {
                break;
            }
            let ry = body_y + off + vi as i32 * rh;
            let row = {
                let g = if page == Page::Procs { &self.procs } else { &self.details };
                g.rows[ri].clone()
            };
            let selected = {
                let g = if page == Page::Procs { &self.procs } else { &self.details };
                g.sel.as_ref() == Some(&row.key)
            };
            let hovered = matches!(self.hover, Hit::Row(p, i) | Hit::Chevron(p, i) if p == page && i == ri);
            let overlay = if selected {
                if dark {
                    0.10
                } else {
                    0.08
                }
            } else if hovered {
                0.05
            } else {
                0.0
            };
            for &(ci, cx, cw) in &cols {
                let (id, right, heat) = {
                    let g = if page == Page::Procs { &self.procs } else { &self.details };
                    let c = &g.cols[ci];
                    (c.id, c.right, c.heat)
                };
                let x0 = x + cx;
                let pad = self.px(8.0);
                if heat && row.values && page == Page::Procs {
                    let lvl = heat_level(id, &row, self);
                    let mut bg = self.th.heat[lvl.min(5)];
                    let unknown = (id == ColId::Disk && row.disk.is_none()) || (id == ColId::Net && row.net.is_none());
                    if unknown {
                        bg = self.th.heat[0];
                    }
                    self.cv.fill(x0, ry, cw, rh, bg);
                    let line = mix(bg, 0x484848, 0.4);
                    self.cv.vline(x0, ry, rh, line);
                }
                if overlay > 0.0 {
                    let oc = self.th.overlay;
                    self.cv.blend(x0, ry, cw, rh, oc, overlay);
                }
                match id {
                    ColId::Name => {
                        let indent = row.depth as i32 * self.px(22.0);
                        if row.expandable {
                            let g = if row.expanded { G_CHEV_D } else { G_CHEV_R };
                            self.cv.text(fsm, g, (x0 + self.px(16.0), ry, self.px(14.0), rh), th_text, DT_C);
                            self.hit(x0 + self.px(8.0), ry, self.px(30.0), rh, Hit::Chevron(page, ri));
                        }
                        let ix = x0 + self.px(38.0) + indent;
                        let ic = if row.icon != 0 { row.icon } else { def_icon };
                        let show_icon = !(matches!(row.key, RowKey::Svc(..)));
                        if show_icon {
                            self.cv.icon(ic, ix, ry + (rh - icon_px) / 2, icon_px);
                        } else {
                            let tc = th_text2;
                            self.cv.text(fsm, "\u{E912}", (ix, ry, icon_px, rh), tc, DT_C);
                        }
                        let tx = ix + icon_px + self.px(6.0);
                        let tw = x0 + cw - tx - pad;
                        self.cv.text(fbody, &row.name, (tx, ry, tw, rh), th_text, DT_L);
                    }
                    ColId::Status => {
                        if row.values {
                            if row.eco {
                                let lc = self.th.leaf;
                                self.cv.text(fi, G_LEAF, (x0 + cw - self.px(34.0), ry, self.px(22.0), rh), lc, DT_C);
                            }
                            if row.suspended {
                                self.cv.text(
                                    fbody,
                                    "Suspended",
                                    (x0 + pad, ry, cw - self.px(40.0), rh),
                                    th_text2,
                                    DT_L,
                                );
                            }
                        }
                    }
                    _ => {
                        let t = cell_text(id, &row);
                        let fl = if right { DT_R } else { DT_L };
                        let col = if page == Page::Details && id == ColId::DStatus && row.suspended {
                            th_text2
                        } else {
                            th_text
                        };
                        self.cv.text(fbody, &t, (x0 + pad, ry, cw - 2 * pad, rh), col, fl);
                    }
                }
            }
            // Trailing area right of the last column keeps the hover/selection band.
            if overlay > 0.0 {
                let end = cols.last().map(|c| c.1 + c.2).unwrap_or(0);
                let oc = self.th.overlay;
                self.cv.blend(x + end, ry, w - end, rh, oc, overlay);
            }
            self.hits.push((
                RECT { left: x, top: ry.max(body_y), right: x + w - sb_w, bottom: (ry + rh).min(body_y + body_h) },
                Hit::Row(page, ri),
            ));
            // Chevron hit must win over the row hit: re-push it on top.
            if row.expandable {
                let cx0 = x + cols.first().map(|c| c.1).unwrap_or(0) + self.px(8.0);
                self.hit(cx0, ry, self.px(30.0), rh, Hit::Chevron(page, ri));
            }
        }
        if n == 0 {
            let msg = if self.snap.is_none() { "Loading…" } else { "No results" };
            self.cv.text(fbody, msg, (x, body_y + self.px(20.0), w, self.px(24.0)), th_text2, DT_C);
        }
        self.cv.clip_rect(None);
        self.cv.clip_rect(Some((x, y, w, h)));

        // ---------------- scrollbar (thin, Windows 11 style)
        let content = n as f32 * rh as f32;
        if content > body_h as f32 && body_h > 0 {
            let hov = matches!(self.hover, Hit::VScroll(p) if p == page)
                || matches!(self.drag, Some(super::Drag::Scroll { .. }));
            let tw = if hov { self.px(6.0) } else { self.px(2.0) };
            let tx = x + w - self.px(4.0) - tw;
            let thumb_h = ((body_h as f32 / content) * body_h as f32).max(self.px(24.0) as f32);
            let ty = body_y as f32 + (scroll / (content - body_h as f32)) * (body_h as f32 - thumb_h);
            if hov {
                let tb = if dark { 0x202020 } else { 0xEEEEEE };
                self.cv.round_rect(
                    (x + w - self.px(12.0)) as f32,
                    body_y as f32,
                    self.px(10.0) as f32,
                    body_h as f32,
                    self.px(5.0) as f32,
                    tb,
                    1.0,
                );
            }
            let tcol = if dark { 0x959595 } else { 0x8A8A8A };
            self.cv.round_rect(tx as f32, ty, tw as f32, thumb_h, tw as f32 / 2.0, tcol, 1.0);
            self.hit(x + w - sb_w, body_y, sb_w, body_h, Hit::VScroll(page));
        }
    }
}

unsafe extern "system" fn toast_timer(hwnd: HWND, _: u32, id: usize, _: u32) {
    KillTimer(hwnd, id);
    if let Some(a) = super::app_opt() {
        a.invalidate();
    }
}

// ---------------------------------------------------------------------- interaction

pub fn header_click(a: &mut App, page: Page, ci: usize) {
    let g = grid(a, page);
    let Some(c) = g.cols.get(ci) else { return };
    let id = c.id;
    if g.sort == id {
        g.desc = !g.desc;
    } else {
        g.sort = id;
        g.desc = matches!(
            id,
            ColId::Cpu
                | ColId::Mem
                | ColId::Disk
                | ColId::Gpu
                | ColId::Net
                | ColId::DCpu
                | ColId::DMem
                | ColId::DThreads
                | ColId::DHandles
        );
    }
    rebuild(a);
    super::settings::save(a);
    a.invalidate();
}

pub fn select_row(a: &mut App, page: Page, i: usize) {
    let g = grid(a, page);
    if let Some(r) = g.rows.get(i) {
        g.sel = Some(r.key.clone());
    }
}

pub fn toggle_expand(a: &mut App, page: Page, i: usize) {
    if page != Page::Procs {
        return;
    }
    let Some(r) = a.procs.rows.get(i) else { return };
    if !r.expandable {
        return;
    }
    let Some(k) = r.group.clone() else { return };
    if r.expanded {
        a.expanded.remove(&k);
    } else {
        a.expanded.insert(k);
    }
    rebuild(a);
    a.invalidate();
}

pub fn activate_row(a: &mut App, page: Page, i: usize) {
    let g = grid(a, page);
    let Some(r) = g.rows.get(i).cloned() else { return };
    if r.expandable {
        toggle_expand(a, page, i);
    } else if r.hwnd != 0 {
        switch_to(r.hwnd);
    }
}

fn switch_to(hwnd: isize) {
    unsafe {
        let h = hwnd as HWND;
        if IsIconic(h) != 0 {
            ShowWindow(h, SW_RESTORE);
        }
        SetForegroundWindow(h);
    }
}

pub fn scroll_click(a: &mut App, page: Page, y: i32) {
    let _ = (a, page, y);
}

pub fn drag_scroll(a: &mut App, page: Page, start_y: i32, start_scroll: f32, y: i32) {
    let (_, h) = a.client();
    let body_h = (h - a.title_h() - a.px(52.0) - header_h(a, page) - info_bar_h(a, page)).max(1) as f32;
    let rh = row_h(a) as f32;
    let g = grid(a, page);
    let content = g.rows.len() as f32 * rh;
    if content <= body_h {
        return;
    }
    let thumb_h = (body_h / content * body_h).max(24.0);
    let ratio = (content - body_h) / (body_h - thumb_h).max(1.0);
    g.scroll = start_scroll + (y - start_y) as f32 * ratio;
    clamp_scroll(a, page);
}

fn ensure_visible(a: &mut App, page: Page, i: usize) {
    let (_, h) = a.client();
    let body_h = (h - a.title_h() - a.px(52.0) - header_h(a, page) - info_bar_h(a, page) - 1).max(1) as f32;
    let rh = row_h(a) as f32;
    let g = grid(a, page);
    let top = i as f32 * rh;
    if top < g.scroll {
        g.scroll = top;
    } else if top + rh > g.scroll + body_h {
        g.scroll = top + rh - body_h;
    }
    clamp_scroll(a, page);
}

pub fn on_key(a: &mut App, vk: u16) -> bool {
    let page = a.page;
    let n = grid(a, page).rows.len();
    if n == 0 {
        return false;
    }
    let cur = {
        let g = grid(a, page);
        g.sel.as_ref().and_then(|k| g.rows.iter().position(|r| &r.key == k))
    };
    let (_, h) = a.client();
    let per_page = ((h - a.px(200.0)) / row_h(a)).max(1) as usize;
    let target = match vk {
        v if v == VK_DOWN => Some(cur.map(|c| (c + 1).min(n - 1)).unwrap_or(0)),
        v if v == VK_UP => Some(cur.map(|c| c.saturating_sub(1)).unwrap_or(0)),
        v if v == VK_NEXT => Some(cur.map(|c| (c + per_page).min(n - 1)).unwrap_or(0)),
        v if v == VK_PRIOR => Some(cur.map(|c| c.saturating_sub(per_page)).unwrap_or(0)),
        v if v == VK_HOME => Some(0),
        v if v == VK_END => Some(n - 1),
        v if v == VK_RIGHT || v == VK_LEFT => {
            if let Some(c) = cur {
                let r = grid(a, page).rows[c].clone();
                if r.expandable && (r.expanded == (vk == VK_LEFT)) {
                    toggle_expand(a, page, c);
                } else if vk == VK_LEFT && r.depth > 0 {
                    // jump to parent group
                    let p = grid(a, page).rows[..c].iter().rposition(|x| x.depth == 0);
                    if let Some(p) = p {
                        select_row(a, page, p);
                        ensure_visible(a, page, p);
                    }
                }
                a.invalidate();
            }
            return true;
        }
        v if v == VK_RETURN => {
            if let Some(c) = cur {
                activate_row(a, page, c);
            }
            return true;
        }
        v if v == VK_DELETE => {
            end_selected(a);
            return true;
        }
        v if v == VK_APPS => {
            if let Some(c) = cur {
                context_menu(a, page, c);
            }
            return true;
        }
        _ => None,
    };
    if let Some(t) = target {
        select_row(a, page, t);
        ensure_visible(a, page, t);
        a.invalidate();
        return true;
    }
    false
}

// ---------------------------------------------------------------------- actions

const CRITICAL: [&str; 8] =
    ["csrss.exe", "wininit.exe", "winlogon.exe", "smss.exe", "services.exe", "lsass.exe", "system", "registry"];

fn proc_exe(a: &App, pid: u32, ctime: u64) -> String {
    a.snap
        .as_ref()
        .and_then(|s| s.procs.iter().find(|p| p.pid == pid && p.ctime == ctime))
        .map(|p| p.exe.to_string())
        .unwrap_or_default()
}

pub fn end_selected(a: &mut App) {
    let page = a.page;
    let Some(r) = a.selected_row(page).cloned() else { return };
    let mut targets = r.procs.clone();
    if targets.is_empty() {
        if let RowKey::Win(_) = r.key {
            // A window row ends its process.
            if let Some(g) = &r.group {
                if let Some(gr) = grid(a, page).rows.iter().find(|x| x.group.as_ref() == Some(g) && x.depth == 0) {
                    targets = gr.procs.clone();
                }
            }
        }
    }
    if targets.is_empty() {
        return;
    }
    let critical = targets.iter().any(|&(pid, ct)| {
        let e = proc_exe(a, pid, ct).to_lowercase();
        CRITICAL.contains(&e.as_str())
    });
    if critical {
        let msg = wide(&format!(
            "“{}” is a critical Windows process. Ending it will make Windows unstable or shut it down, and unsaved data will be lost.\n\nEnd it anyway?",
            r.name
        ));
        let cap = wide("Better Task Manager");
        let res = unsafe { MessageBoxW(a.hwnd, msg.as_ptr(), cap.as_ptr(), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) };
        if res != IDYES {
            return;
        }
    }
    let mut denied: Vec<(u32, u64)> = Vec::new();
    let mut ended = 0;
    for &(pid, created) in &targets {
        match crate::admin::end_process(crate::admin::ProcessId { pid, created }) {
            Ok(()) => ended += 1,
            Err(5) => denied.push((pid, created)),
            Err(_) => {}
        }
    }
    if !denied.is_empty() {
        let msg = wide(&format!(
            "“{}” runs with higher privileges and can only be ended as administrator.\n\nEnd it as administrator?",
            r.name
        ));
        let cap = wide("Better Task Manager");
        let res = unsafe { MessageBoxW(a.hwnd, msg.as_ptr(), cap.as_ptr(), MB_YESNO | MB_ICONQUESTION) };
        if res == IDYES {
            let ids = denied.iter().map(|&(pid, created)| crate::admin::ProcessId { pid, created }).collect();
            crate::admin::AdminTask::EndProcesses(ids).request(a.hwnd);
        }
    } else if ended > 0 {
        a.toast(&format!("{} ended", r.name));
    }
    crate::sampler::control().wake();
}

pub fn toggle_efficiency_selected(a: &mut App) {
    let page = a.page;
    let Some(r) = a.selected_row(page).cloned() else { return };
    let on = !r.eco;
    let mut failed = 0;
    for &(pid, created) in &r.procs {
        if !set_efficiency(crate::admin::ProcessId { pid, created }, on) {
            failed += 1;
        }
    }
    if failed > 0 {
        a.toast("Efficiency mode could not be changed for every process (access denied)");
    } else {
        a.toast(if on { "Efficiency mode turned on" } else { "Efficiency mode turned off" });
    }
    crate::sampler::control().wake();
}

pub fn set_efficiency(id: crate::admin::ProcessId, on: bool) -> bool {
    let Ok(h) = crate::admin::open_exact(id, PROCESS_SET_INFORMATION) else { return false };
    unsafe {
        let st = PROCESS_POWER_THROTTLING_STATE {
            Version: 1,
            ControlMask: if on { 1 } else { 0 },
            StateMask: if on { 1 } else { 0 },
        };
        let ok = SetProcessInformation(
            h,
            ProcessPowerThrottling,
            &st as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        ) != 0;
        SetPriorityClass(h, if on { IDLE_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS });
        CloseHandle(h);
        ok
    }
}

const CM_EXPAND: usize = 1;
const CM_SWITCH: usize = 2;
const CM_END: usize = 3;
const CM_ECO: usize = 4;
const CM_OPEN: usize = 5;
const CM_DETAILS: usize = 6;
const CM_PROPS: usize = 7;
const CM_SEARCH: usize = 8;
const CM_COPY: usize = 9;

pub fn menu_item(m: HMENU, id: usize, text: &str, enabled: bool, checked: bool) {
    let t = wide(text);
    let mut flags = MF_STRING;
    if !enabled {
        flags |= MF_GRAYED;
    }
    if checked {
        flags |= MF_CHECKED;
    }
    unsafe { AppendMenuW(m, flags, id, t.as_ptr()) };
}

fn menu_sep(m: HMENU) {
    unsafe { AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null()) };
}

pub fn popup(a: &App, m: HMENU) -> usize {
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        let r = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, a.hwnd, std::ptr::null());
        DestroyMenu(m);
        r.max(0) as usize
    }
}

pub fn context_menu(a: &mut App, page: Page, i: usize) {
    let Some(r) = grid(a, page).rows.get(i).cloned() else { return };
    let m = unsafe { CreatePopupMenu() };
    if r.expandable {
        menu_item(m, CM_EXPAND, if r.expanded { "Collapse" } else { "Expand" }, true, false);
    }
    if r.hwnd != 0 {
        menu_item(m, CM_SWITCH, "Switch to", true, false);
    }
    if r.expandable || r.hwnd != 0 {
        menu_sep(m);
    }
    let has = !r.procs.is_empty();
    menu_item(m, CM_END, "End task", has || matches!(r.key, RowKey::Win(_)), false);
    menu_item(m, CM_ECO, "Efficiency mode", has, r.eco);
    menu_sep(m);
    menu_item(m, CM_OPEN, "Open file location", !r.path.is_empty(), false);
    menu_item(m, CM_SEARCH, "Search online", true, false);
    if page == Page::Procs {
        menu_item(m, CM_DETAILS, "Go to details", r.procs.len() == 1, false);
    }
    menu_item(m, CM_PROPS, "Properties", !r.path.is_empty(), false);
    menu_sep(m);
    menu_item(m, CM_COPY, "Copy name", true, false);
    match popup(a, m) {
        CM_EXPAND => toggle_expand(a, page, i),
        CM_SWITCH => switch_to(r.hwnd),
        CM_END => end_selected(a),
        CM_ECO => toggle_efficiency_selected(a),
        CM_OPEN => crate::shell::show_in_folder(a.hwnd, &r.path),
        CM_SEARCH => crate::shell::search_online(a.hwnd, &format!("{} {}", r.procname, r.name)),
        CM_DETAILS => {
            if let Some(&(pid, ct)) = r.procs.first() {
                a.set_page(Page::Details);
                a.details.sel = Some(RowKey::Proc(pid, ct));
                rebuild(a);
                if let Some(idx) = a.details.rows.iter().position(|x| x.key == RowKey::Proc(pid, ct)) {
                    ensure_visible(a, Page::Details, idx);
                }
                a.invalidate();
            }
        }
        CM_PROPS => {
            let p = wide(&r.path);
            unsafe { SHObjectProperties(a.hwnd, SHOP_FILEPATH as u32, p.as_ptr(), std::ptr::null()) };
        }
        CM_COPY => super::set_clipboard_text(a.hwnd, &r.name),
        _ => {}
    }
    a.invalidate();
}

pub fn column_menu(a: &mut App, page: Page) {
    let m = unsafe { CreatePopupMenu() };
    {
        let g = grid(a, page);
        for (i, c) in g.cols.iter().enumerate() {
            let fixed = i == 0;
            menu_item(m, 100 + i, c.title, !fixed, c.visible);
        }
    }
    let r = popup(a, m);
    if r >= 100 {
        let g = grid(a, page);
        if let Some(c) = g.cols.get_mut(r - 100) {
            c.visible = !c.visible;
        }
        super::settings::save(a);
        a.invalidate();
    }
}

pub fn show_in_details(a: &mut App, pid: u32, created: u64) {
    a.search.clear();
    a.set_page(Page::Details);
    let key = RowKey::Proc(pid, created);
    rebuild(a);
    if let Some(index) = a.details.rows.iter().position(|row| row.key == key) {
        ensure_visible(a, Page::Details, index);
    }
    a.details.sel = Some(key);
    a.invalidate();
}
