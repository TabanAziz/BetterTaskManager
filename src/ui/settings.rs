// Settings page, "..." menu and persistence (HKCU\Software\BTM).

use std::ffi::c_void;
use std::sync::atomic::Ordering;

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::gfx::*;
use super::procs::{menu_item, popup, ColId};
use super::{App, Drag, Hit, Page, Settings, ThemePreference};
use crate::model::HelperState;
use crate::util::wide;

const KEY: &str = crate::product::SETTINGS_KEY;

mod gallery;

#[derive(Default)]
pub(super) struct PageState {
    gallery: gallery::Gallery,
    scroll: i32,
    max_scroll: i32,
    scrollbar: Option<(RECT, RECT)>,
}

impl PageState {
    pub(super) fn invalidate_gallery(&mut self) {
        self.gallery.invalidate();
    }
}

fn get_dword(name: &str) -> Option<u32> {
    let k = wide(KEY);
    let n = wide(name);
    let mut v = 0u32;
    let mut size = 4u32;
    let st = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            k.as_ptr(),
            n.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut v as *mut _ as *mut c_void,
            &mut size,
        )
    };
    if st == 0 {
        Some(v)
    } else {
        None
    }
}

fn set_dword(name: &str, v: u32) {
    let k = wide(KEY);
    let n = wide(name);
    unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), REG_DWORD, &v as *const _ as *const c_void, 4)
    };
}

fn get_string(name: &str) -> Option<String> {
    crate::sysinfo::reg_string(HKEY_CURRENT_USER, KEY, name)
}

fn set_string(name: &str, s: &str) {
    let k = wide(KEY);
    let n = wide(name);
    let v = wide(s);
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            k.as_ptr(),
            n.as_ptr(),
            REG_SZ,
            v.as_ptr() as *const c_void,
            (v.len() * 2) as u32,
        )
    };
}

pub fn load() -> Settings {
    read(get_dword, get_string)
}

pub fn defaults() -> Settings {
    read(|_| None, |_| None)
}

fn read(dword: impl Fn(&str) -> Option<u32>, string: impl Fn(&str) -> Option<String>) -> Settings {
    let page = match dword("StartPage").unwrap_or(0) {
        1 => Page::Perf,
        2 => Page::Details,
        3 => Page::History,
        4 => Page::Startup,
        5 => Page::Users,
        6 => Page::Services,
        _ => Page::Procs,
    };
    let placement = string("Placement").and_then(|s| {
        let v: Vec<i32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if v.len() == 4 && v[2] > v[0] + 200 && v[3] > v[1] + 150 {
            Some(RECT { left: v[0], top: v[1], right: v[2], bottom: v[3] })
        } else {
            None
        }
    });
    Settings {
        theme: string("Theme").as_deref().and_then(ThemePreference::from_key).unwrap_or_default(),
        start_page: page,
        interval_ms: dword("IntervalMs").filter(|v| matches!(v, 0 | 500 | 1000 | 4000)).unwrap_or(1000),
        topmost: dword("Topmost").unwrap_or(0) != 0,
        hint_dismissed: dword("HintDismissed").unwrap_or(0) != 0,
        cpu_logical: dword("CpuLogical").unwrap_or(0) != 0,
        placement,
        maximized: dword("Maximized").unwrap_or(0) != 0,
        nav_expanded: dword("NavExpanded").unwrap_or(1) != 0,
    }
}

fn col_id_name(id: ColId) -> &'static str {
    match id {
        ColId::Name => "name",
        ColId::Status => "status",
        ColId::Publisher => "publisher",
        ColId::Pid => "pid",
        ColId::ProcName => "procname",
        ColId::Cpu => "cpu",
        ColId::Mem => "mem",
        ColId::Disk => "disk",
        ColId::Gpu => "gpu",
        ColId::Net => "net",
        ColId::GpuEngine => "gpuengine",
        ColId::DName => "dname",
        ColId::DPid => "dpid",
        ColId::DStatus => "dstatus",
        ColId::DUser => "duser",
        ColId::DCpu => "dcpu",
        ColId::DMem => "dmem",
        ColId::DThreads => "dthreads",
        ColId::DHandles => "dhandles",
        ColId::DDesc => "ddesc",
    }
}

fn save_grid(prefix: &str, g: &super::procs::Grid) {
    let cols: Vec<String> =
        g.cols.iter().map(|c| format!("{}:{}:{}", col_id_name(c.id), c.w.round() as i32, c.visible as u8)).collect();
    set_string(&format!("{}Cols", prefix), &cols.join(","));
    set_string(&format!("{}Sort", prefix), col_id_name(g.sort));
    set_dword(&format!("{}Desc", prefix), g.desc as u32);
}

fn load_grid(prefix: &str, g: &mut super::procs::Grid) {
    if let Some(s) = get_string(&format!("{}Cols", prefix)) {
        for part in s.split(',') {
            let f: Vec<&str> = part.split(':').collect();
            if f.len() != 3 {
                continue;
            }
            if let Some(c) = g.cols.iter_mut().find(|c| col_id_name(c.id) == f[0]) {
                if let Ok(w) = f[1].parse::<f32>() {
                    c.w = w.clamp(40.0, 900.0);
                }
                c.visible = f[2] == "1" || c.id == ColId::Name || c.id == ColId::DName;
            }
        }
    }
    if let Some(s) = get_string(&format!("{}Sort", prefix)) {
        if let Some(c) = g.cols.iter().find(|c| col_id_name(c.id) == s) {
            g.sort = c.id;
        }
    }
    if let Some(d) = get_dword(&format!("{}Desc", prefix)) {
        g.desc = d != 0;
    }
}

pub fn apply_columns(a: &mut App) {
    for (name, table) in a.lists.saved() {
        if let Some(layout) = get_string(name) {
            table.restore(&layout);
        }
    }
    load_grid("Proc", &mut a.procs);
    load_grid("Det", &mut a.details);
}

/// Set by the off-screen render/benchmark mode: never touch the user's saved settings.
pub static NO_SAVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn save(a: &mut App) {
    if NO_SAVE.load(Ordering::Relaxed) {
        return;
    }
    set_string("Theme", a.settings.theme.key());
    set_dword(
        "StartPage",
        match a.settings.start_page {
            Page::Perf => 1,
            Page::Details => 2,
            Page::History => 3,
            Page::Startup => 4,
            Page::Users => 5,
            Page::Services => 6,
            _ => 0,
        },
    );
    set_dword("IntervalMs", a.settings.interval_ms);
    set_dword("Topmost", a.settings.topmost as u32);
    set_dword("HintDismissed", a.settings.hint_dismissed as u32);
    set_dword("CpuLogical", a.settings.cpu_logical as u32);
    set_dword("NavExpanded", a.settings.nav_expanded as u32);
    unsafe {
        let mut wp: WINDOWPLACEMENT = std::mem::zeroed();
        wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
        if GetWindowPlacement(a.hwnd, &mut wp) != 0 {
            let r = wp.rcNormalPosition;
            set_string("Placement", &format!("{},{},{},{}", r.left, r.top, r.right, r.bottom));
            set_dword("Maximized", (wp.showCmd == SW_SHOWMAXIMIZED as u32) as u32);
        }
    }
    save_grid("Proc", &a.procs);
    save_grid("Det", &a.details);
    for (name, table) in a.lists.saved() {
        set_string(name, &table.layout());
    }
}

fn set_interval(a: &mut App, ms: u32) {
    a.settings.interval_ms = ms;
    crate::sampler::control().interval_ms.store(ms, Ordering::Release);
    crate::sampler::control().wake();
    save(a);
    a.invalidate();
}

fn set_topmost(a: &mut App, on: bool) {
    a.settings.topmost = on;
    unsafe {
        SetWindowPos(a.hwnd, if on { HWND_TOPMOST } else { HWND_NOTOPMOST }, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
    }
    save(a);
    a.invalidate();
}

pub fn setup_helper(a: &mut App) {
    crate::admin::AdminTask::InstallHelper.request(a.hwnd);
    a.toast("Please confirm the Windows prompt…");
}

fn helper_state(a: &App) -> HelperState {
    match crate::sampler::control().helper_state.load(Ordering::Acquire) {
        1 => HelperState::Starting,
        2 => HelperState::Running,
        3 => HelperState::SelfElevated,
        _ => a.snap.as_ref().map(|s| s.helper).unwrap_or_default(),
    }
}

pub fn click(a: &mut App, id: u32) {
    match id {
        10 => pick_start_page(a),
        20 => set_interval(a, 500),
        21 => set_interval(a, 1000),
        22 => set_interval(a, 4000),
        23 => set_interval(a, 0),
        30 => {
            let on = !a.settings.topmost;
            set_topmost(a, on);
        }
        40 => {
            let task = if crate::shell_hook::state().is_ours() {
                crate::admin::AdminTask::RestoreShortcut
            } else {
                crate::admin::AdminTask::TakeShortcut
            };
            task.request(a.hwnd);
        }
        50 => setup_helper(a),
        51 => {
            crate::admin::AdminTask::RemoveHelper.request(a.hwnd);
        }
        _ => {}
    }
    a.invalidate();
}

const START_PAGES: [(Page, &str); 7] = [
    (Page::Procs, "Processes"),
    (Page::Perf, "Performance"),
    (Page::History, "App history"),
    (Page::Startup, "Startup apps"),
    (Page::Users, "Users"),
    (Page::Details, "Details"),
    (Page::Services, "Services"),
];

fn pick_start_page(a: &mut App) {
    let m = unsafe { CreatePopupMenu() };
    for (i, (page, name)) in START_PAGES.iter().enumerate() {
        menu_item(m, i + 1, name, true, *page == a.settings.start_page);
    }
    let picked = super::procs_popup(a, m);
    if let Some(&(page, _)) = picked.checked_sub(1).and_then(|i| START_PAGES.get(i)) {
        a.settings.start_page = page;
        save(a);
    }
}

pub(super) fn choose_theme(a: &mut App, theme: ThemePreference) {
    if a.settings.theme != theme {
        a.set_theme(theme);
        save(a);
    }
}

pub fn more_menu(a: &mut App) {
    let m = unsafe { CreatePopupMenu() };
    menu_item(m, 1, "Refresh now\tF5", true, false);
    unsafe { AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null()) };
    let iv = a.settings.interval_ms;
    menu_item(m, 2, "Update speed: High (0.5 s)", true, iv == 500);
    menu_item(m, 3, "Update speed: Normal (1 s)", true, iv == 1000);
    menu_item(m, 4, "Update speed: Low (4 s)", true, iv == 4000);
    menu_item(m, 5, "Update speed: Paused", true, iv == 0);
    unsafe { AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null()) };
    menu_item(m, 6, "Always on top", true, a.settings.topmost);
    menu_item(m, 7, "Settings", true, false);
    match popup(a, m) {
        1 => crate::sampler::control().wake(),
        2 => set_interval(a, 500),
        3 => set_interval(a, 1000),
        4 => set_interval(a, 4000),
        5 => set_interval(a, 0),
        6 => {
            let on = !a.settings.topmost;
            set_topmost(a, on);
        }
        7 => a.set_page(Page::Settings),
        _ => {}
    }
}

impl App {
    fn segmented(&mut self, x: i32, y: i32, items: &[(&str, u32, bool)]) {
        let fb = self.f.body;
        let h = self.px(32.0);
        let mut cx = x;
        for (label, id, on) in items {
            let w = self.cv.text_width(fb, label) + self.px(28.0);
            let hov = self.hover == Hit::Setting(*id);
            let (bg, fg) =
                if *on { (self.th.accent, self.th.accent_text()) } else { (self.th.control_bg(hov), self.th.text) };
            self.cv.round_rect(cx as f32, y as f32, w as f32, h as f32, self.px(4.0) as f32, bg, 1.0);
            self.cv.text(fb, label, (cx, y, w, h), fg, DT_C);
            self.hit(cx, y, w, h, Hit::Setting(*id));
            cx += w + self.px(6.0);
        }
    }

    fn toggle(&mut self, x: i32, y: i32, on: bool, id: u32) {
        let w = self.px(40.0);
        let h = self.px(20.0);
        let r = h as f32 / 2.0;
        if on {
            let c = self.th.accent;
            self.cv.round_rect(x as f32, y as f32, w as f32, h as f32, r, c, 1.0);
            self.cv.round_rect(
                (x + w - h + self.px(4.0)) as f32,
                (y + self.px(4.0)) as f32,
                (h - self.px(8.0)) as f32,
                (h - self.px(8.0)) as f32,
                r,
                self.th.accent_text(),
                1.0,
            );
        } else {
            let c = self.th.text2;
            self.cv.round_rect_outline(x as f32, y as f32, w as f32, h as f32, r, c, 1.0);
            self.cv.round_rect(
                (x + self.px(5.0)) as f32,
                (y + self.px(5.0)) as f32,
                (h - self.px(10.0)) as f32,
                (h - self.px(10.0)) as f32,
                r,
                c,
                1.0,
            );
        }
        self.hit(x - self.px(4.0), y - self.px(4.0), w + self.px(8.0), h + self.px(8.0), Hit::Setting(id));
    }

    fn card(&mut self, bounds: (i32, i32, i32, i32), reserve: i32, title: &str, description: &[&str]) {
        let (x, y, w, h) = bounds;
        let bg = self.th.color_for(0x232323, 0xFFFFFF, self.th.search_bg);
        self.cv.round_rect(x as f32, y as f32, w as f32, h as f32, self.px(6.0) as f32, bg, 1.0);
        let fb = self.f.body;
        let fs = self.f.small;
        let t1 = self.th.text;
        let t2 = self.th.text2;
        let text_width = w - self.px(32.0) - reserve;
        self.cv.text(fb, title, (x + self.px(16.0), y + self.px(12.0), text_width, self.px(20.0)), t1, DT_L);
        for (i, line) in description.iter().enumerate() {
            let top = y + self.px(32.0 + i as f32 * 17.0);
            self.cv.text(fs, line, (x + self.px(16.0), top, text_width, self.px(18.0)), t2, DT_L);
        }
    }

    pub fn draw_settings_page(&mut self, x: i32, y: i32, w: i32, h: i32) {
        let top = y + self.px(52.0);
        let viewport = RECT { left: x, top, right: x + w, bottom: y + h };
        let cx = x + self.px(16.0);
        let cw = (w - self.px(32.0)).min(self.px(760.0));
        let gap = self.px(8.0);
        let gallery_h = gallery::height(cw, |value| self.px(value));
        let ch = self.px(104.0);
        let stack_controls = cw < self.px(378.0);
        let sh = self.px(if stack_controls { 100.0 } else { 64.0 });
        let speed_extra = if stack_controls { self.px(38.0) } else { 0 };
        let content_height = self.px(4.0) + gallery_h + 2 * ch + speed_extra + 3 * sh + 7 * gap + self.px(22.0);
        self.settings_page.max_scroll = (content_height - (h - self.px(52.0))).max(0);
        self.settings_page.scroll = self.settings_page.scroll.min(self.settings_page.max_scroll);
        let mut cy = top + self.px(4.0) - self.settings_page.scroll;
        let first_hit = self.hits.len();
        self.cv.clip_rect(Some((x, top, w, (h - self.px(52.0)).max(0))));
        self.cv.clip_pixels(Some(viewport));

        self.draw_theme_gallery(cx, cy, cw, viewport);
        cy += gallery_h + gap;
        self.card((cx, cy, cw, ch), 0, "Default start page", &["The page BTM shows when it opens."]);
        let sp = self.settings.start_page;
        let current = START_PAGES.iter().find(|(p, _)| *p == sp).map_or("Processes", |(_, name)| *name);
        let label = format!("{current}   \u{25BE}");
        self.segmented(cx + self.px(16.0), cy + self.px(58.0), &[(&label, 10, false)]);
        cy += ch + gap;

        self.card((cx, cy, cw, ch + speed_extra), 0, "Real time update speed", &["How often the values are measured."]);
        let iv = self.settings.interval_ms;
        let speeds = [
            ("High (0.5 s)", 20, iv == 500),
            ("Normal (1 s)", 21, iv == 1000),
            ("Low (4 s)", 22, iv == 4000),
            ("Paused", 23, iv == 0),
        ];
        let speed_x = cx + self.px(16.0);
        let speed_y = cy + self.px(58.0);
        if stack_controls {
            self.segmented(speed_x, speed_y, &speeds[..2]);
            self.segmented(speed_x, speed_y + speed_extra, &speeds[2..]);
        } else {
            self.segmented(speed_x, speed_y, &speeds);
        }
        cy += ch + gap + speed_extra;

        let toggle_width = if stack_controls { 0 } else { self.px(56.0) };
        let toggle_x = if stack_controls { cx + self.px(16.0) } else { cx + cw - self.px(64.0) };
        let toggle_y = self.px(if stack_controls { 66.0 } else { 22.0 });
        self.card((cx, cy, cw, sh), toggle_width, "Always on top", &["BTM stays above other windows."]);
        let on = self.settings.topmost;
        self.toggle(toggle_x, cy + toggle_y, on, 30);
        cy += sh + gap;

        let reg = self.registered;
        let shortcut: &[&str] = if stack_controls {
            &["Replaces Windows Task Manager.", "Needs admin access. Can be reversed."]
        } else {
            &["Replaces the Windows Task Manager (needs administrator permission, reversible at any time)."]
        };
        self.card((cx, cy, cw, sh), toggle_width, "Ctrl+Shift+Esc opens BTM", shortcut);
        self.toggle(toggle_x, cy + toggle_y, reg, 40);
        cy += sh + gap;

        let hs = helper_state(self);
        let (status, installed) = match hs {
            HelperState::Running => ("Active: disk and network for all processes.", true),
            HelperState::Starting => ("Set up, starting…", true),
            HelperState::SelfElevated => ("Administrator mode: all values available.", true),
            HelperState::NotInstalled => ("Not set up: the Disk column stays empty.", false),
        };
        let label = if installed { "Remove" } else { "Set up" };
        let bw = self.cv.text_width(self.f.body, label) + self.px(28.0);
        let reserve = if stack_controls || hs == HelperState::SelfElevated { 0 } else { bw + gap };
        self.card((cx, cy, cw, sh), reserve, "Full process data (elevated helper)", &[status]);
        if hs != HelperState::SelfElevated {
            let fb = self.f.body;
            let id = if installed { 51 } else { 50 };
            let bx = if stack_controls { cx + self.px(16.0) } else { cx + cw - bw - self.px(16.0) };
            let by = cy + self.px(if stack_controls { 61.0 } else { 16.0 });
            let bh = self.px(32.0);
            let hov = self.hover == Hit::Setting(id);
            let (bg, fg) = if installed {
                (self.th.control_bg(hov), self.th.text)
            } else {
                let bg = if hov { self.th.accent } else { self.th.accent_strong };
                (bg, self.th.accent_text_on(bg))
            };
            self.cv.round_rect(bx as f32, by as f32, bw as f32, bh as f32, self.px(4.0) as f32, bg, 1.0);
            self.cv.text(fb, label, (bx, by, bw, bh), fg, DT_C);
            self.hit(bx, by, bw, bh, Hit::Setting(id));
        }
        cy += sh + gap * 2;

        let fs = self.f.small;
        let t3 = self.th.text3;
        self.cv.text(
            fs,
            concat!(
                "Better Task Manager ",
                env!("CARGO_PKG_VERSION"),
                " · MIT License · github.com/TabanAziz/BetterTaskManager"
            ),
            (cx + self.px(4.0), cy, cw, self.px(18.0)),
            t3,
            DT_L,
        );
        for (bounds, _) in &mut self.hits[first_hit..] {
            bounds.top = bounds.top.max(viewport.top);
            bounds.bottom = bounds.bottom.min(viewport.bottom);
        }
        self.cv.clip_rect(Some((x, y, w, h)));
        self.cv.clip_pixels(None);
        self.page_header(x, y, w, "Settings", &[]);
        self.draw_settings_scrollbar(viewport, content_height);
    }

    fn draw_settings_scrollbar(&mut self, viewport: RECT, content_height: i32) {
        let inset = self.px(4.0);
        let track = RECT {
            left: viewport.right - self.px(14.0),
            top: viewport.top + inset,
            right: viewport.right - inset,
            bottom: viewport.bottom - inset,
        };
        let height = track.bottom - track.top;
        let max_scroll = self.settings_page.max_scroll;
        self.settings_page.scrollbar = None;
        if max_scroll == 0 || height <= 0 {
            return;
        }
        let thumb_h = (height * height / content_height).clamp(self.px(24.0).min(height), height);
        let top = track.top + (height - thumb_h) * self.settings_page.scroll / max_scroll;
        let thumb = RECT { top, bottom: top + thumb_h, ..track };
        self.settings_page.scrollbar = Some((track, thumb));
        let width = self.px(4.0) as f32;
        let x = ((track.left + track.right) as f32 - width) * 0.5;
        let hot = self.hover == Hit::SettingsScroll || matches!(self.drag, Some(Drag::SettingsScroll { .. }));
        let fg = if hot { self.th.text2 } else { self.th.thumb_border };
        self.cv.round_rect(x, track.top as f32, width, height as f32, width / 2.0, self.th.divider, 1.0);
        self.cv.round_rect(x, top as f32, width, thumb_h as f32, width / 2.0, fg, 1.0);
        self.hits.push((track, Hit::SettingsScroll));
    }

    pub(super) fn settings_scroll_to(&mut self, offset: i32) {
        let settings = &mut self.settings_page;
        let offset = offset.clamp(0, settings.max_scroll);
        if offset != settings.scroll {
            settings.scroll = offset;
            self.hover = Hit::None;
            self.invalidate();
        }
    }

    pub(super) fn settings_wheel(&mut self, delta: i32) {
        self.settings_scroll_to(self.settings_page.scroll - delta * self.px(72.0) / 120);
    }

    pub(super) fn settings_scroll_press(&mut self, y: i32) -> Option<Drag> {
        let (track, thumb) = self.settings_page.scrollbar?;
        if y >= thumb.top && y < thumb.bottom {
            Some(Drag::SettingsScroll {
                start_y: y,
                start_scroll: self.settings_page.scroll,
                scroll_range: self.settings_page.max_scroll,
                thumb_range: (track.bottom - track.top - (thumb.bottom - thumb.top)).max(1),
            })
        } else {
            let page = track.bottom - track.top;
            self.settings_scroll_to(self.settings_page.scroll + if y < thumb.top { -page } else { page });
            None
        }
    }

    pub(super) fn settings_key(&mut self, key: u16) -> bool {
        let page = (self.client().1 - self.title_h() - self.px(52.0)).max(0);
        let offset = match key {
            VK_HOME => 0,
            VK_END => self.settings_page.max_scroll,
            VK_PRIOR => self.settings_page.scroll - page,
            VK_NEXT => self.settings_page.scroll + page,
            VK_UP => self.settings_page.scroll - self.px(40.0),
            VK_DOWN => self.settings_page.scroll + self.px(40.0),
            _ => return false,
        };
        self.settings_scroll_to(offset);
        true
    }
}
