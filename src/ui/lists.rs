//! App history, Startup apps, Users and Services pages: rows, toolbars and commands.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::{SHObjectProperties, SHOP_FILEPATH};
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use super::gfx::DT_L;
use super::plot::Area;
use super::table::{col, heated, number, Cell, Drag, Icon, Part, Row, Surface, Table};
use super::{App, Page, Tool};
use crate::admin::AdminTask;
use crate::model::{Proc, ProcKind};
use crate::pages::{self, history, services, startup, users, Data, Feed};
use crate::shell;
use crate::util::{fmt_min_step, fmt_num, wide};

const ACCESS_DENIED: u32 = 5;
/// WM_APP_LIST wparam: something changed, read the feed again.
const RELOAD: usize = 1;

const G_SERVICE: &str = "\u{EA86}";
const G_USER: &str = "\u{E77B}";
const G_ACCOUNTS: &str = "\u{E716}";
const G_ENABLE: &str = "\u{E73E}";
const G_DISABLE: &str = "\u{F140}";
const G_DISCONNECT: &str = "\u{E8CD}";
const G_START: &str = "\u{E768}";
const G_STOP: &str = "\u{E71A}";
const G_RESTART: &str = "\u{E72C}";
const G_OPEN: &str = "\u{E8A7}";

pub fn is_list(page: Page) -> bool {
    matches!(page, Page::History | Page::Startup | Page::Users | Page::Services)
}

pub struct Lists {
    history: Table,
    startup: Table,
    users: Table,
    services: Table,
    apps: Option<Result<Vec<history::AppUsage>, String>>,
    apps_loaded: Option<Instant>,
    entries: Option<Vec<startup::Entry>>,
    sessions: Option<Vec<users::Session>>,
    service_list: Option<Result<Vec<services::Service>, String>>,
}

impl Lists {
    pub fn new() -> Lists {
        Lists {
            history: Table::new(
                vec![
                    col("Name", 280.0),
                    heated("CPU time", 90.0),
                    heated("Network", 100.0),
                    heated("Downloaded", 100.0),
                    heated("Uploaded", 100.0),
                ],
                2,
                true,
            ),
            startup: Table::new(
                vec![
                    col("Name", 250.0),
                    col("Publisher", 180.0),
                    col("Status", 78.0),
                    col("Startup impact", 104.0),
                    col("Type", 106.0),
                    col("Command line", 260.0),
                ],
                0,
                false,
            ),
            users: Table::new(
                vec![
                    col("User", 250.0),
                    col("Status", 110.0),
                    heated("CPU", 74.0),
                    heated("Memory", 92.0),
                    heated("Disk", 74.0),
                    heated("GPU", 74.0),
                    heated("Network", 80.0),
                ],
                2,
                true,
            ),
            services: Table::new(
                vec![
                    col("Name", 220.0),
                    number("PID", 64.0),
                    col("Description", 300.0),
                    col("Status", 80.0),
                    col("Group", 220.0),
                ],
                0,
                false,
            ),
            apps: None,
            apps_loaded: None,
            entries: None,
            sessions: None,
            service_list: None,
        }
    }

    pub fn table(&mut self, page: Page) -> Option<&mut Table> {
        match page {
            Page::History => Some(&mut self.history),
            Page::Startup => Some(&mut self.startup),
            Page::Users => Some(&mut self.users),
            Page::Services => Some(&mut self.services),
            _ => None,
        }
    }

    /// Name and table, for saving the column layout.
    pub fn saved(&mut self) -> [(&'static str, &mut Table); 4] {
        [
            ("ListHistory", &mut self.history),
            ("ListStartup", &mut self.startup),
            ("ListUsers", &mut self.users),
            ("ListServices", &mut self.services),
        ]
    }

    pub fn reset_scroll(&mut self) {
        for (_, table) in self.saved() {
            table.reset_scroll();
        }
    }

    fn entry(&self) -> Option<&startup::Entry> {
        let id = self.startup.selected.as_ref()?;
        self.entries.as_ref()?.iter().find(|e| &entry_id(e) == id)
    }

    fn service(&self) -> Option<&services::Service> {
        let id = self.services.selected.as_ref()?;
        self.service_list.as_ref()?.as_ref().ok()?.iter().find(|s| &s.name == id)
    }

    fn session(&self) -> Option<&users::Session> {
        let id = self.users.selected.as_ref()?;
        self.sessions.as_ref()?.iter().find(|s| &session_id(s) == id)
    }
}

fn entry_id(e: &startup::Entry) -> String {
    format!("{:?}|{}", e.source, e.id)
}

fn session_id(s: &users::Session) -> String {
    format!("session{}", s.id)
}

fn feed_of(page: Page) -> Option<Feed> {
    match page {
        Page::History => Some(Feed::History),
        Page::Startup => Some(Feed::Startup),
        Page::Users => Some(Feed::Users),
        Page::Services => Some(Feed::Services),
        _ => None,
    }
}

// ---------------------------------------------------------------- data flow

pub fn on_open(a: &mut App) {
    let Some(feed) = feed_of(a.page) else { return };
    // The history covers 30 days and takes seconds to read: not on every visit.
    let fresh = a.lists.apps_loaded.is_some_and(|at| at.elapsed() < Duration::from_secs(300));
    if feed != Feed::History || !fresh {
        pages::request(feed, a.hwnd);
    }
}

/// F5 or a finished command.
pub fn reload(a: &App) {
    if let Some(feed) = feed_of(a.page) {
        pages::request(feed, a.hwnd);
    }
}

pub fn on_sample(a: &App) {
    let seq = a.snap.as_ref().map_or(0, |s| s.seq);
    match a.page {
        Page::Services if seq.is_multiple_of(2) => pages::request(Feed::Services, a.hwnd),
        Page::Users if seq.is_multiple_of(5) => pages::request(Feed::Users, a.hwnd),
        _ => {}
    }
}

pub fn on_message(a: &mut App, wparam: usize) {
    absorb(a, pages::take());
    if wparam == RELOAD {
        reload(a);
    }
    rebuild(a);
    a.invalidate();
}

fn absorb(a: &mut App, feeds: Vec<Data>) {
    for data in feeds {
        match data {
            Data::History(apps) => {
                a.lists.apps_loaded = apps.is_ok().then(Instant::now);
                a.lists.apps = Some(apps);
            }
            Data::Startup(entries) => a.lists.entries = Some(entries),
            Data::Users(sessions) => a.lists.sessions = Some(sessions),
            Data::Services(list) => a.lists.service_list = Some(list),
        }
    }
}

#[cfg(feature = "demo")]
pub fn show(a: &mut App, feeds: Vec<Data>) {
    absorb(a, feeds);
}

// ---------------------------------------------------------------- rows

pub fn rebuild(a: &mut App) {
    let needle = a.search.trim().to_lowercase();
    let procs: &[Proc] = a.snap.as_ref().map_or(&[], |s| &s.procs);
    let mem_total = a.snap.as_ref().map_or(1, |s| s.perf.mem.total.max(1));
    let lists = &mut a.lists;
    match a.page {
        Page::History => {
            let apps = match &lists.apps {
                Some(Ok(apps)) => apps.as_slice(),
                _ => return,
            };
            lists.history.set_rows(history_rows(apps, procs, &needle));
        }
        Page::Startup => {
            if let Some(entries) = &lists.entries {
                lists.startup.set_rows(startup_rows(entries, &needle));
            }
        }
        Page::Users => {
            if let Some(sessions) = &lists.sessions {
                // A single signed-in user is the normal case: their processes are one click away.
                lists.users.set_rows(user_rows(sessions, procs, mem_total, &needle));
            }
        }
        Page::Services => {
            if let Some(Ok(list)) = &lists.service_list {
                lists.services.set_rows(service_rows(list, &needle));
            }
        }
        _ => {}
    }
}

fn level(value: f64, steps: [f64; 5]) -> usize {
    steps.iter().filter(|&&step| value >= step).count()
}

fn history_rows(apps: &[history::AppUsage], procs: &[Proc], needle: &str) -> Vec<Row> {
    struct Line {
        name: String,
        icon: Option<std::sync::Arc<crate::meta::Meta>>,
        cpu: u64,
        received: u64,
        sent: u64,
    }
    let mut lines: HashMap<String, Line> = HashMap::with_capacity(apps.len() + 64);
    for app in apps {
        let line =
            Line { name: app.name.clone(), icon: app.meta.clone(), cpu: 0, received: app.received, sent: app.sent };
        lines.insert(app.key.clone(), line);
    }
    for p in procs.iter().filter(|p| p.kind == ProcKind::Normal) {
        let family = p.package.as_deref().map(str::to_lowercase);
        let key = match family {
            Some(family) if lines.contains_key(&family) => family,
            _ if p.pid == 4 => "system".to_string(),
            _ if !p.path().is_empty() => p.path().to_lowercase(),
            _ => continue,
        };
        let line = lines.entry(key).or_insert_with(|| Line {
            name: p.display_name(),
            icon: p.meta.clone(),
            cpu: 0,
            received: 0,
            sent: 0,
        });
        line.cpu += p.cpu_time;
        if line.icon.is_none() {
            line.icon = p.meta.clone();
        }
    }
    // Updaters install every version into its own folder: one row per app, as in Task Manager.
    let mut by_name: HashMap<String, Line> = HashMap::with_capacity(lines.len());
    for (_, line) in lines {
        match by_name.get_mut(&line.name.to_lowercase()) {
            Some(known) => {
                known.cpu += line.cpu;
                known.received += line.received;
                known.sent += line.sent;
                if known.icon.as_ref().is_none_or(|m| m.icon() == 0) && line.icon.is_some() {
                    known.icon = line.icon;
                }
            }
            None => {
                by_name.insert(line.name.to_lowercase(), line);
            }
        }
    }
    let lines = by_name;
    let top_cpu = lines.values().map(|l| l.cpu).max().unwrap_or(0).max(1) as f64;
    let top_net = lines.values().map(|l| l.received + l.sent).max().unwrap_or(0).max(1) as f64;
    let share = [0.004, 0.04, 0.15, 0.4, 0.75];
    let bytes = |v: u64| {
        let text = if v >= 1 << 30 {
            format!("{} GB", fmt_num(v as f64 / 1073741824.0, 1))
        } else {
            format!("{} MB", fmt_min_step(v as f64 / 1048576.0, 1))
        };
        Cell::number(text, v as f64, level(v as f64 / top_net, share))
    };
    lines
        .into_iter()
        .filter(|(_, l)| needle.is_empty() || l.name.to_lowercase().contains(needle))
        .map(|(id, l)| Row {
            cells: vec![
                Cell::text(l.name),
                Cell::number(clock(l.cpu), l.cpu as f64, level(l.cpu as f64 / top_cpu, share)),
                bytes(l.received + l.sent),
                bytes(l.received),
                bytes(l.sent),
            ],
            icon: Icon::File(l.icon),
            id,
            children: Vec::new(),
        })
        .collect()
}

/// CPU time in 100 ns units → "1:02:03".
fn clock(ticks: u64) -> String {
    let seconds = ticks / 10_000_000;
    format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
}

fn startup_rows(entries: &[startup::Entry], needle: &str) -> Vec<Row> {
    entries
        .iter()
        .filter(|e| {
            needle.is_empty() || e.name.to_lowercase().contains(needle) || e.publisher.to_lowercase().contains(needle)
        })
        .map(|e| Row {
            id: entry_id(e),
            icon: Icon::File(e.meta.clone()),
            cells: vec![
                Cell::text(e.name.clone()),
                Cell::text(e.publisher.clone()),
                Cell::text(if e.enabled { "Enabled" } else { "Disabled" }),
                Cell::text(e.impact.label()),
                Cell::text(e.source.label()),
                Cell::text(e.command.clone()),
            ],
            children: Vec::new(),
        })
        .collect()
}

#[derive(Default)]
struct Usage {
    cpu: f64,
    mem: u64,
    disk: Option<f64>,
    gpu: f64,
    net: Option<f64>,
}

impl Usage {
    fn add(&mut self, p: &Proc) {
        self.cpu += p.cpu as f64;
        self.mem += p.mem;
        self.gpu += p.gpu as f64;
        if let Some(v) = p.disk {
            *self.disk.get_or_insert(0.0) += v as f64;
        }
        if let Some(v) = p.net {
            *self.net.get_or_insert(0.0) += v as f64;
        }
    }

    fn cells(&self, mem_total: u64) -> [Cell; 5] {
        let share = [1.0, 15.0, 30.0, 50.0, 75.0];
        let mem_mb = self.mem as f64 / 1048576.0;
        let mem_share = self.mem as f64 / mem_total as f64 * 100.0;
        let disk = self.disk.map(|v| v / 1048576.0);
        let net = self.net.map(|v| v * 8.0 / 1e6);
        let gpu = self.gpu.min(100.0);
        [
            Cell::number(format!("{}%", fmt_min_step(self.cpu, 1)), self.cpu, level(self.cpu, share)),
            Cell::number(format!("{} MB", fmt_num(mem_mb, 1)), mem_mb, level(mem_share, [1.5, 15.0, 30.0, 50.0, 75.0])),
            Cell::number(
                disk.map(|v| format!("{} MB/s", fmt_min_step(v, 1))).unwrap_or_default(),
                disk.unwrap_or(-1.0),
                level(disk.unwrap_or(0.0), [1.5, 10.0, 50.0, 100.0, 300.0]),
            ),
            Cell::number(format!("{}%", fmt_min_step(gpu, 1)), gpu, level(gpu, share)),
            Cell::number(
                net.map(|v| format!("{} Mbps", fmt_min_step(v, 1))).unwrap_or_default(),
                net.unwrap_or(-1.0),
                level(net.unwrap_or(0.0), [1.0, 10.0, 50.0, 100.0, 500.0]),
            ),
        ]
    }
}

fn user_rows(sessions: &[users::Session], procs: &[Proc], mem_total: u64, needle: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    for session in sessions {
        let user_hit = needle.is_empty() || session.name.to_lowercase().contains(needle);
        let mut sum = Usage::default();
        let mut children = Vec::new();
        for p in procs.iter().filter(|p| p.kind == ProcKind::Normal && p.session == session.id) {
            sum.add(p);
            let name = p.display_name();
            if !user_hit && !name.to_lowercase().contains(needle) {
                continue;
            }
            let mut usage = Usage::default();
            usage.add(p);
            let mut cells = vec![Cell::text(name), Cell::text(if p.suspended { "Suspended" } else { "" })];
            cells.extend(usage.cells(mem_total));
            rows_child(&mut children, p, cells);
        }
        if !user_hit && children.is_empty() {
            continue;
        }
        let name = format!("{} ({})", session.name, children.len());
        let mut cells = vec![Cell::text(name), Cell::text(if session.disconnected { "Disconnected" } else { "" })];
        cells.extend(sum.cells(mem_total));
        rows.push(Row { id: session_id(session), icon: Icon::Glyph(G_USER), cells, children });
    }
    rows
}

fn rows_child(children: &mut Vec<Row>, p: &Proc, cells: Vec<Cell>) {
    let id = format!("p{}:{}", p.pid, p.ctime);
    children.push(Row { id, icon: Icon::File(p.meta.clone()), cells, children: Vec::new() });
}

fn service_rows(list: &[services::Service], needle: &str) -> Vec<Row> {
    list.iter()
        .filter(|s| {
            needle.is_empty()
                || s.name.to_lowercase().contains(needle)
                || s.display.to_lowercase().contains(needle)
                || (s.pid != 0 && s.pid.to_string() == needle)
        })
        .map(|s| Row {
            id: s.name.clone(),
            icon: Icon::Glyph(G_SERVICE),
            cells: vec![
                Cell::text(s.name.clone()),
                Cell::number(if s.pid == 0 { String::new() } else { s.pid.to_string() }, s.pid as f64, 0),
                Cell::text(s.display.clone()),
                Cell::text(s.state.label()),
                Cell::text(s.group.clone()),
            ],
            children: Vec::new(),
        })
        .collect()
}

// ---------------------------------------------------------------- drawing

impl App {
    pub fn draw_list_page(&mut self, x: i32, y: i32, w: i32, h: i32) {
        let page = self.page;
        let mut tools: Vec<(Tool, &str, &str, bool)> = vec![(Tool::RunTask, super::G_RUN, "Run new task", true)];
        let title = match page {
            Page::History => "App history",
            Page::Startup => {
                let on = self.lists.entry().map(|e| e.enabled);
                tools.push((Tool::EnableStartup, G_ENABLE, "Enable", on == Some(false)));
                tools.push((Tool::DisableStartup, G_DISABLE, "Disable", on == Some(true)));
                "Startup apps"
            }
            Page::Users => {
                let selected = self.lists.session().is_some();
                tools.push((Tool::Disconnect, G_DISCONNECT, "Disconnect", selected));
                tools.push((Tool::Accounts, G_ACCOUNTS, "Manage user accounts", true));
                "Users"
            }
            Page::Services => {
                let (state, can_stop) =
                    self.lists.service().map(|s| (Some(s.state), s.can_stop)).unwrap_or((None, false));
                let running = state == Some(services::State::Running);
                tools.push((Tool::StartService, G_START, "Start", state == Some(services::State::Stopped)));
                tools.push((Tool::StopService, G_STOP, "Stop", running && can_stop));
                tools.push((Tool::RestartService, G_RESTART, "Restart", running && can_stop));
                tools.push((Tool::OpenServices, G_OPEN, "Open Services", true));
                "Services"
            }
            _ => return,
        };
        tools.push((Tool::More, super::G_MORE, "", true));
        let mut top = self.page_header(x, y, w, title, &tools);

        let note = match page {
            Page::History => Some(
                "Network usage of the last 30 days, as recorded by Windows. CPU time counts the processes running now.",
            ),
            Page::Startup => {
                let on = self.lists.entries.as_ref().map(|all| all.iter().filter(|e| e.enabled).count());
                on.map(|_| "Disabling keeps the entry: it just does not start with Windows any more.")
            }
            _ => None,
        };
        if let Some(note) = note {
            let (font, color, line) = (self.f.small, self.th.text2, self.px(26.0));
            let pad = self.px(17.0);
            self.cv.text(font, note, (x + pad, top - self.px(4.0), w - 2 * pad, line), color, DT_L);
            top += line;
        }

        let totals: Vec<Option<f32>> = match (page, self.snap.as_ref()) {
            (Page::Users, Some(s)) => {
                vec![None, None, Some(s.cpu_pct), Some(s.mem_pct), Some(s.disk_pct), Some(s.gpu_pct), Some(s.net_pct)]
            }
            _ => Vec::new(),
        };
        let loaded = match page {
            Page::History => self.lists.apps.as_ref().map(|r| r.as_ref().err().cloned()),
            Page::Startup => self.lists.entries.as_ref().map(|_| None),
            Page::Users => self.lists.sessions.as_ref().map(|_| None),
            _ => self.lists.service_list.as_ref().map(|r| r.as_ref().err().cloned()),
        };
        let empty = match loaded {
            None if page == Page::History => "Reading 30 days of usage history…".to_string(),
            None => "Loading…".to_string(),
            Some(Some(error)) => error,
            Some(None) => "No results".to_string(),
        };

        let area = Area { x, y: top, w, h: y + h - top };
        let App { cv, th, f, hits, hover, s, lists, .. } = self;
        let mut surface = Surface { cv, th, f, hits, hover: *hover, scale: *s };
        if let Some(table) = lists.table(page) {
            table.draw(&mut surface, area, &totals, &empty);
        }
        self.draw_toast(x, y, w, h);
    }
}

// ---------------------------------------------------------------- input

pub fn press(a: &mut App, part: Part, x: i32, y: i32, double: bool) -> Option<Drag> {
    let table = a.lists.table(a.page)?;
    match part {
        Part::Divider(col) => {
            let start_w = table.cols.get(col)?.width;
            return Some(Drag::Column { col, start_x: x, start_w });
        }
        Part::Scroll => return Some(table.scroll_press(y)),
        Part::Row(index) => {
            table.select(index);
            if double {
                table.toggle(index);
            }
        }
        Part::Chevron(index) => {
            table.select(index);
            table.toggle(index);
        }
        Part::Header(_) => {}
    }
    None
}

pub fn release(a: &mut App, part: Part) {
    if let (Part::Header(col), Some(table)) = (part, a.lists.table(a.page)) {
        table.sort_by(col);
        super::settings::save(a);
        a.invalidate();
    }
}

pub fn on_key(a: &mut App, key: u16) -> bool {
    let handled = a.lists.table(a.page).is_some_and(|table| table.key(key));
    if handled {
        a.invalidate();
    }
    handled
}

pub fn on_tool(a: &mut App, tool: Tool) {
    match tool {
        Tool::EnableStartup => switch_startup(a, true),
        Tool::DisableStartup => switch_startup(a, false),
        Tool::Disconnect => end_session(a, false),
        Tool::Accounts => shell::open(a.hwnd, "ms-settings:otherusers"),
        Tool::StartService => control(a, services::Action::Start),
        Tool::StopService => control(a, services::Action::Stop),
        Tool::RestartService => control(a, services::Action::Restart),
        Tool::OpenServices => shell::open(a.hwnd, &format!("{}\\services.msc", shell::system_directory())),
        _ => {}
    }
}

fn switch_startup(a: &mut App, on: bool) {
    let Some(entry) = a.lists.entry().cloned() else { return };
    match startup::set_enabled(entry.source, &entry.id, on) {
        Ok(()) => {
            a.toast(&format!("{} {}", entry.name, if on { "enabled" } else { "disabled" }));
            pages::request(Feed::Startup, a.hwnd);
        }
        Err(ACCESS_DENIED) if entry.source.machine_tag().is_some() => {
            AdminTask::SetStartup { source: entry.source, id: entry.id, on }.request(a.hwnd);
        }
        Err(code) => a.toast(&format!("{} could not be changed (error {code})", entry.name)),
    }
}

fn end_session(a: &mut App, sign_off: bool) {
    let Some(session) = a.lists.session().cloned() else { return };
    let question = if sign_off {
        format!("Sign off {}? Unsaved work in that session will be lost.", session.name)
    } else {
        format!("Disconnect {}? If this is your own session, you will return to the sign-in screen.", session.name)
    };
    let (body, caption) = (wide(&question), wide(crate::product::NAME));
    let style = wm::MB_YESNO | wm::MB_ICONWARNING | wm::MB_DEFBUTTON2;
    if unsafe { wm::MessageBoxW(a.hwnd, body.as_ptr(), caption.as_ptr(), style) } != wm::IDYES {
        return;
    }
    let done = if sign_off { users::sign_off(session.id) } else { users::disconnect(session.id) };
    if done {
        pages::request(Feed::Users, a.hwnd);
    } else {
        a.toast("Windows refused this for that session");
    }
}

fn control(a: &mut App, action: services::Action) {
    let Some(name) = a.lists.service().map(|s| s.name.clone()) else { return };
    a.toast(&format!("Asking {name} to {}…", action.word()));
    let window = a.hwnd as isize;
    // Stopping may take up to 30 seconds; the window stays responsive.
    std::thread::spawn(move || {
        let window = window as HWND;
        match services::control(&name, action) {
            Err(ACCESS_DENIED) => AdminTask::Service { name, action }.request(window),
            result => unsafe {
                if let Err(code) = result {
                    let body = wide(&format!("{name} could not {} (error {code}).", action.word()));
                    let caption = wide(crate::product::NAME);
                    wm::MessageBoxW(window, body.as_ptr(), caption.as_ptr(), wm::MB_ICONWARNING);
                }
                wm::PostMessageW(window, pages::WM_APP_LIST, RELOAD, 0);
            },
        }
    });
}

pub fn context_menu(a: &mut App, part: Part) {
    let (Part::Row(index) | Part::Chevron(index)) = part else { return };
    let page = a.page;
    let Some(table) = a.lists.table(page) else { return };
    table.select(index);
    let Some(row) = table.row(index) else { return };
    let name = row.cells.first().map(|c| c.text.clone()).unwrap_or_default();
    let id = row.id.clone();
    let file = match &row.icon {
        Icon::File(info) => info.as_ref().map(|m| m.path.clone()).unwrap_or_default(),
        Icon::Glyph(_) => String::new(),
    };
    a.invalidate();

    const PRIMARY: usize = 1;
    const SECOND: usize = 2;
    const THIRD: usize = 3;
    const DETAILS: usize = 4;
    const OPEN_FILE: usize = 5;
    const SEARCH: usize = 6;
    const PROPERTIES: usize = 7;
    const COPY: usize = 8;
    let menu = unsafe { wm::CreatePopupMenu() };
    let item = |id: usize, text: &str, enabled: bool| super::procs_menu_item(menu, id, text, enabled, false);

    let mut process: Option<(u32, u64)> = None;
    match page {
        Page::Startup => {
            let on = a.lists.entry().is_some_and(|e| e.enabled);
            item(PRIMARY, if on { "Disable" } else { "Enable" }, true);
        }
        Page::Users if a.lists.session().is_some() => {
            item(PRIMARY, "Disconnect", true);
            item(SECOND, "Sign off", true);
        }
        Page::Users => {
            process = id
                .strip_prefix('p')
                .and_then(|p| p.split_once(':'))
                .and_then(|(p, c)| Some((p.parse().ok()?, c.parse().ok()?)));
            item(DETAILS, "Go to details", process.is_some());
        }
        Page::Services => {
            let service = a.lists.service();
            let running = service.is_some_and(|s| s.state == services::State::Running);
            let can_stop = running && service.is_some_and(|s| s.can_stop);
            item(PRIMARY, "Start", service.is_some_and(|s| s.state == services::State::Stopped));
            item(SECOND, "Stop", can_stop);
            item(THIRD, "Restart", can_stop);
            let pid = service.map_or(0, |s| s.pid);
            process = a
                .snap
                .as_ref()
                .and_then(|s| s.procs.iter().find(|p| pid != 0 && p.pid == pid))
                .map(|p| (p.pid, p.ctime));
            item(DETAILS, "Go to details", process.is_some());
        }
        _ => {}
    }
    if page != Page::Services {
        item(OPEN_FILE, "Open file location", !file.is_empty());
        item(PROPERTIES, "Properties", !file.is_empty());
    }
    item(SEARCH, "Search online", true);
    item(COPY, "Copy name", true);

    let choice = super::procs_popup(a, menu);
    match (choice, page) {
        (PRIMARY, Page::Startup) => {
            let on = a.lists.entry().is_some_and(|e| e.enabled);
            switch_startup(a, !on);
        }
        (PRIMARY, Page::Users) => end_session(a, false),
        (SECOND, Page::Users) => end_session(a, true),
        (PRIMARY, Page::Services) => control(a, services::Action::Start),
        (SECOND, Page::Services) => control(a, services::Action::Stop),
        (THIRD, Page::Services) => control(a, services::Action::Restart),
        (DETAILS, _) => {
            if let Some((pid, created)) = process {
                super::procs::show_in_details(a, pid, created);
            }
        }
        (OPEN_FILE, _) => shell::show_in_folder(a.hwnd, &file),
        (PROPERTIES, _) => {
            let path = wide(&file);
            unsafe { SHObjectProperties(a.hwnd, SHOP_FILEPATH as u32, path.as_ptr(), std::ptr::null()) };
        }
        (SEARCH, _) => shell::search_online(a.hwnd, &name),
        (COPY, _) => super::set_clipboard_text(a.hwnd, &name),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cpu_time_format() {
        assert_eq!(super::clock(37_230_000_000), "1:02:03");
        assert_eq!(super::clock(0), "0:00:00");
    }
}
