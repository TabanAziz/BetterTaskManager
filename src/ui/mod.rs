// Main window: Task Manager style shell (Mica title bar + navigation, content panel),
// all drawn into one back buffer. No child controls, so nothing flickers and painting
// a full frame costs about a millisecond.

#[cfg(feature = "demo")]
mod demo;
pub mod gfx;
mod lists;
mod perf;
mod plot;
mod procs;
pub(crate) mod settings;
mod table;
pub mod testmode;
pub(crate) mod window;

pub(crate) use procs::{menu_item as procs_menu_item, popup as procs_popup};

use std::collections::HashSet;
use std::ffi::c_void;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD};
use windows_sys::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::meta;
use crate::model::Snapshot;
use crate::sampler;
use crate::util::wide;
use gfx::*;

pub const WM_APP_DONE: u32 = 0x8000 + 3;
const TIMER_CARET: usize = 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Procs,
    Perf,
    History,
    Startup,
    Users,
    Details,
    Services,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    RunTask,
    EndTask,
    Efficiency,
    EnableStartup,
    DisableStartup,
    Disconnect,
    Accounts,
    StartService,
    StopService,
    RestartService,
    OpenServices,
    More,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    None,
    Nav(Page),
    NavToggle,
    Search,
    SearchClear,
    Tool(Tool),
    Hdr(Page, usize),
    HdrDiv(Page, usize),
    Row(Page, usize),
    Chevron(Page, usize),
    VScroll(Page),
    PerfItem(usize),
    PerfGraph,
    Setting(u32),
    List(table::Part),
    InfoSetup,
    InfoClose,
}

pub struct Theme {
    pub dark: bool,
    pub content: Color,
    pub text: Color,
    pub text2: Color,
    pub text3: Color,
    pub divider: Color,
    pub heat: [Color; 6],
    pub header_hot: Color,
    pub accent: Color,
    pub accent_strong: Color,
    pub cpu: Color,
    pub mem: Color,
    pub disk: Color,
    pub net: Color,
    pub gpu: Color,
    pub leaf: Color,
    pub thumb_bg: Color,
    pub thumb_border: Color,
    pub overlay: Color,
    /// Title bar / navigation (Mica area): foreground, hover and selection fills,
    /// search box colours.
    pub chrome_fg: Color,
    pub chrome_hover: Color,
    pub chrome_sel: Color,
    pub search_bg: Color,
    pub search_hover: Color,
    pub search_focus: Color,
    pub search_border: Color,
    pub search_hint: Color,
}

fn accent_palette() -> Option<[Color; 8]> {
    let k = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent");
    let v = wide("AccentPalette");
    let mut buf = [0u8; 32];
    let mut size = 32u32;
    let st = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            k.as_ptr(),
            v.as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut c_void,
            &mut size,
        )
    };
    if st != 0 || size < 32 {
        return None;
    }
    let mut p = [0u32; 8];
    for i in 0..8 {
        p[i] = rgb(buf[i * 4], buf[i * 4 + 1], buf[i * 4 + 2]);
    }
    Some(p)
}

fn apps_use_light() -> bool {
    let k = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let v = wide("AppsUseLightTheme");
    let mut val = 0u32;
    let mut size = 4u32;
    let st = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            k.as_ptr(),
            v.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut val as *mut _ as *mut c_void,
            &mut size,
        )
    };
    st == 0 && val != 0
}

impl Theme {
    pub fn load() -> Theme {
        Theme::load_variant(apps_use_light())
    }

    pub fn load_variant(light: bool) -> Theme {
        let pal = accent_palette()
            .unwrap_or([0x99EBFF, 0x4CC2FF, 0x0091F8, 0x0078D4, 0x0067C0, 0x003E92, 0x001A68, 0xF7630C]);
        if !light {
            let content = 0x191919;
            let base = pal[5];
            Theme {
                dark: true,
                content,
                text: 0xFFFFFF,
                text2: 0xC8C8C8,
                text3: 0x9A9A9A,
                divider: 0x2D2D2D,
                heat: [mix(content, base, 0.30), mix(content, base, 0.70), base, pal[4], pal[3], pal[2]],
                header_hot: 0x272727,
                accent: pal[1],
                accent_strong: pal[2],
                cpu: 0x39B8E3,
                mem: 0x5C9EFA,
                disk: 0x84C718,
                net: 0xF5628E,
                gpu: 0xDE68F2,
                leaf: 0x6CCB5F,
                thumb_bg: 0x292929,
                thumb_border: 0x757575,
                overlay: 0xFFFFFF,
                chrome_fg: 0xFFFFFF,
                chrome_hover: 0x0B0B0B,
                chrome_sel: 0x101010,
                search_bg: 0x323232,
                search_hover: 0x3A3A3A,
                search_focus: 0x1F1F1F,
                search_border: 0x444444,
                search_hint: 0xA8A8A8,
            }
        } else {
            let content = 0xF9F9F9;
            let base = pal[0];
            Theme {
                dark: false,
                content,
                text: 0x1A1A1A,
                text2: 0x5C5C5C,
                text3: 0x8A8A8A,
                divider: 0xE5E5E5,
                heat: [
                    mix(content, base, 0.25),
                    mix(content, base, 0.55),
                    base,
                    pal[1],
                    mix(pal[1], pal[2], 0.5),
                    pal[2],
                ],
                header_hot: 0xEDEDED,
                accent: pal[4],
                accent_strong: pal[3],
                cpu: 0x117DBB,
                mem: 0x8B12AE,
                disk: 0x4DA60C,
                net: 0xA74F01,
                gpu: 0x9528B4,
                leaf: 0x0F7B0F,
                thumb_bg: 0xFFFFFF,
                thumb_border: 0xBDBDBD,
                overlay: 0x000000,
                chrome_fg: 0x1A1A1A,
                chrome_hover: 0xE9E9E9,
                chrome_sel: 0xE5E5E5,
                search_bg: 0xFBFBFB,
                search_hover: 0xF6F6F6,
                search_focus: 0xFFFFFF,
                search_border: 0xE0E0E0,
                search_hint: 0x5F5F5F,
            }
        }
    }
}

pub struct Fonts {
    pub body: HFONT,
    pub body_semi: HFONT,
    pub small: HFONT,
    pub caption: HFONT,
    pub title: HFONT,
    pub hdr_val: HFONT,
    pub nav: HFONT,
    pub icon: HFONT,
    pub icon_small: HFONT,
    pub icon_tiny: HFONT,
    pub perf_title: HFONT,
    pub perf_name: HFONT,
    pub stat_val: HFONT,
    pub thumb_title: HFONT,
}

impl Fonts {
    fn new(s: f32) -> Fonts {
        let text = if font_exists("Segoe UI Variable Text") { "Segoe UI Variable Text" } else { "Segoe UI" };
        let text_semi = if font_exists("Segoe UI Variable Text Semibold") {
            "Segoe UI Variable Text Semibold"
        } else {
            "Segoe UI Semibold"
        };
        let disp = if font_exists("Segoe UI Variable Display") { "Segoe UI Variable Display" } else { "Segoe UI" };
        let disp_semi = if font_exists("Segoe UI Variable Display Semib") {
            "Segoe UI Variable Display Semib"
        } else {
            "Segoe UI Semibold"
        };
        let small = if font_exists("Segoe UI Variable Small") { "Segoe UI Variable Small" } else { "Segoe UI" };
        let icons = if font_exists("Segoe Fluent Icons") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" };
        let p = |v: f32| (v * s).round() as i32;
        const CT: u32 = 5; // CLEARTYPE_QUALITY
        const AA: u32 = 4; // ANTIALIASED_QUALITY (for Mica areas: no colour fringes)
        Fonts {
            body: make_font(text, p(12.0), 400, AA),
            body_semi: make_font(text_semi, p(12.0), 600, CT),
            small: make_font(small, p(12.0), 400, AA),
            caption: make_font(small, p(12.0), 400, AA),
            title: make_font(disp_semi, p(14.0), 600, CT),
            hdr_val: make_font(disp, p(17.0), 400, AA),
            nav: make_font(text, p(14.0), 400, AA),
            icon: make_font(icons, p(16.0), 400, AA),
            icon_small: make_font(icons, p(12.0), 400, AA),
            icon_tiny: make_font(icons, p(8.0), 400, AA),
            perf_title: make_font(disp_semi, p(24.0), 600, CT),
            perf_name: make_font(disp, p(15.0), 400, CT),
            stat_val: make_font(disp, p(19.0), 400, CT),
            thumb_title: make_font(text, p(14.0), 400, CT),
        }
    }
    fn destroy(&self) {
        for f in [
            self.body,
            self.body_semi,
            self.small,
            self.caption,
            self.title,
            self.hdr_val,
            self.nav,
            self.icon,
            self.icon_small,
            self.icon_tiny,
            self.perf_title,
            self.perf_name,
            self.stat_val,
            self.thumb_title,
        ] {
            unsafe { DeleteObject(f) };
        }
    }
}

// Glyphs from Segoe Fluent Icons.
pub const G_HAMBURGER: &str = "\u{E700}";
pub const G_PROCS: &str = "\u{E8A9}";
pub const G_PERF: &str = "\u{E9D9}";
pub const G_DETAILS: &str = "\u{E8FD}";
pub const G_SETTINGS: &str = "\u{E713}";
pub const G_SEARCH: &str = "\u{E721}";
pub const G_RUN: &str = "\u{ECC8}";
pub const G_END: &str = "\u{E733}";
pub const G_LEAF: &str = "\u{EC0A}";
pub const G_MORE: &str = "\u{E712}";
pub const G_HISTORY: &str = "\u{E81C}";
pub const G_STARTUP: &str = "\u{EC4A}";
pub const G_USERS: &str = "\u{E716}";
pub const G_SERVICES: &str = "\u{EA86}";
pub const G_CHEV_R: &str = "\u{E76C}";
pub const G_CHEV_D: &str = "\u{E70D}";
pub const G_CHEV_U: &str = "\u{E70E}";
pub const G_CLOSE: &str = "\u{E711}";
pub const G_SHIELD: &str = "\u{EA18}";

pub struct Settings {
    pub start_page: Page,
    pub interval_ms: u32,
    pub topmost: bool,
    pub hint_dismissed: bool,
    pub cpu_logical: bool,
    pub placement: Option<RECT>,
    pub maximized: bool,
    pub nav_expanded: bool,
}

pub struct App {
    pub hwnd: HWND,
    pub dpi: u32,
    pub s: f32,
    pub th: Theme,
    pub f: Fonts,
    pub cv: Canvas,
    pub page: Page,
    pub snap: Option<Box<Snapshot>>,
    pub hits: Vec<(RECT, Hit)>,
    pub hover: Hit,
    pub pressed: Hit,
    pub search: String,
    pub search_focus: bool,
    pub caret_on: bool,
    pub search_all_selected: bool,
    pub procs: procs::Grid,
    pub details: procs::Grid,
    pub expanded: HashSet<procs::GroupKey>,
    pub perf_sel: usize,
    pub lists: lists::Lists,
    pub nav_peek: bool,
    pub settings: Settings,
    pub drag: Option<Drag>,
    pub busy_note: Option<(String, u64)>,
    pub registered: bool,
    pub last_mouse: (i32, i32),
    pub paint_stats: [f64; 5],
}

#[derive(Clone, Copy)]
pub enum Drag {
    Column { page: Page, col: usize, start_x: i32, start_w: i32 },
    Scroll { page: Page, start_y: i32, start_scroll: f32 },
    List(table::Drag),
}

static mut APP: *mut App = std::ptr::null_mut();

pub fn app() -> &'static mut App {
    unsafe { &mut *APP }
}

pub fn app_opt() -> Option<&'static mut App> {
    unsafe {
        if APP.is_null() {
            None
        } else {
            Some(&mut *APP)
        }
    }
}

impl App {
    #[inline]
    pub fn px(&self, v: f32) -> i32 {
        (v * self.s).round() as i32
    }

    fn nav_fits(&self) -> bool {
        self.client().0 >= self.px(820.0)
    }

    /// A narrow window folds the navigation to icons, like Task Manager; the menu button still opens it.
    fn nav_open(&self) -> bool {
        if self.nav_fits() {
            self.settings.nav_expanded
        } else {
            self.nav_peek
        }
    }

    pub fn nav_w(&self) -> i32 {
        if self.nav_open() {
            self.px(240.0)
        } else {
            self.px(48.0)
        }
    }
    pub fn title_h(&self) -> i32 {
        self.px(56.0)
    }

    pub fn client(&self) -> (i32, i32) {
        let mut rc: RECT = unsafe { std::mem::zeroed() };
        unsafe { GetClientRect(self.hwnd, &mut rc) };
        (rc.right - rc.left, rc.bottom - rc.top)
    }

    pub fn hit(&mut self, x: i32, y: i32, w: i32, h: i32, hit: Hit) {
        self.hits.push((RECT { left: x, top: y, right: x + w, bottom: y + h }, hit));
    }

    pub fn invalidate(&self) {
        unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 0) };
    }

    fn hit_at(&self, x: i32, y: i32) -> Hit {
        for (r, h) in self.hits.iter().rev() {
            if x >= r.left && x < r.right && y >= r.top && y < r.bottom {
                return *h;
            }
        }
        Hit::None
    }

    fn search_rect(&self) -> (i32, i32, i32, i32) {
        let (w, _) = self.client();
        let sw = self.px(380.0).min((w - self.px(420.0)).max(self.px(160.0)));
        let x = ((w - sw) / 2).max(self.px(150.0));
        (x, self.px(12.0), sw, self.px(32.0))
    }

    // ------------------------------------------------------------------ painting

    pub(super) fn paint(&mut self) {
        let q0 = crate::util::qpc_now();
        let (w, h) = self.client();
        self.cv.ensure(w, h);
        self.hits.clear();
        // Everything outside the content panel is Mica: draw light content on black,
        // then convert to premultiplied alpha.
        let dark = self.th.dark;
        if dark {
            self.cv.fill_raw(0, 0, w, h, 0);
        } else {
            self.cv.fill(0, 0, w, h, 0xF3F3F3);
        }
        self.draw_titlebar(w);
        self.draw_nav(h);
        let th = self.title_h();
        let nw = self.nav_w();
        if dark {
            self.cv.premultiply_from_black(0, 0, w, th);
            self.cv.premultiply_from_black(0, th, nw, h - th);
        }
        let q1 = crate::util::qpc_now();

        // Content panel with a rounded top-left corner.
        let px = nw;
        let py = th;
        let pw = w - nw;
        let ph = h - th;
        let r = self.px(8.0);
        let content = self.th.content;
        self.cv.round_rect(px as f32, py as f32, (pw + r * 2) as f32, (ph + r * 2) as f32, r as f32, content, 1.0);
        self.cv.fill(px + r, py, pw - r, ph, content);
        self.cv.fill(px, py + r, pw, ph - r, content);
        let border = if self.th.dark { 0x2B2B2B } else { 0xE5E5E5 };
        self.cv.round_rect_outline(
            px as f32,
            py as f32,
            (pw + r * 4) as f32,
            (ph + r * 4) as f32,
            r as f32,
            border,
            0.6,
        );
        self.cv.clip_rect(Some((px, py, pw, ph)));
        let q2 = crate::util::qpc_now();
        match self.page {
            Page::Procs => self.draw_procs_page(px, py, pw, ph),
            Page::Details => self.draw_details_page(px, py, pw, ph),
            Page::Perf => self.draw_perf_page(px, py, pw, ph),
            Page::Settings => self.draw_settings_page(px, py, pw, ph),
            Page::History | Page::Startup | Page::Users | Page::Services => self.draw_list_page(px, py, pw, ph),
        }
        self.cv.clip_rect(None);
        unsafe { GdiFlush() };
        let q3 = crate::util::qpc_now();
        self.cv.set_alpha_opaque(px + r, py, pw - r, ph);
        self.cv.set_alpha_opaque(px, py + r, r, ph - r);
        let q4 = crate::util::qpc_now();
        let m = |a: i64, b: i64| crate::util::qpc_ms(a, b);
        self.paint_stats[0] += m(q0, q1);
        self.paint_stats[1] += m(q1, q2);
        self.paint_stats[2] += m(q2, q3);
        self.paint_stats[3] += m(q3, q4);
        self.paint_stats[4] += 1.0;
    }

    fn draw_titlebar(&mut self, w: i32) {
        let s = self.s;
        // App icon + name.
        let ico = app_icon(16.0 * s);
        self.cv.icon(ico, self.px(16.0), self.px(20.0), (16.0 * s).round() as i32);
        let fc = self.f.caption;
        let fg = self.th.chrome_fg;
        self.cv.text(fc, crate::product::NAME, (self.px(48.0), 0, self.px(178.0), self.px(56.0)), fg, DT_L);
        // Search box.
        let (x, y, sw, sh) = self.search_rect();
        let focus = self.search_focus;
        let hover = self.hover == Hit::Search;
        let bg = if focus {
            self.th.search_focus
        } else if hover {
            self.th.search_hover
        } else {
            self.th.search_bg
        };
        let sborder = self.th.search_border;
        let shint = self.th.search_hint;
        self.cv.round_rect(x as f32, y as f32, sw as f32, sh as f32, self.px(4.0) as f32, bg, 1.0);
        self.cv.round_rect_outline(
            x as f32 + 0.5,
            y as f32 + 0.5,
            sw as f32 - 1.0,
            sh as f32 - 1.0,
            self.px(4.0) as f32,
            sborder,
            1.0,
        );
        if focus {
            // Accent underline like WinUI text boxes.
            let acc = self.th.accent;
            self.cv.fill(x + self.px(1.0), y + sh - self.px(2.0), sw - self.px(2.0), self.px(2.0), acc);
        }
        let fi = self.f.icon;
        self.cv.text(fi, G_SEARCH, (x + self.px(12.0), y, self.px(16.0), sh), shint, DT_C);
        let tx = x + self.px(40.0);
        let tw = sw - self.px(76.0);
        let fb = self.f.nav;
        if self.search.is_empty() {
            self.cv.text(fb, "Type a name, publisher, or PID to search", (tx, y, tw, sh), shint, DT_L);
        } else {
            if self.search_all_selected && focus {
                let tw2 = self.cv.text_width(fb, &self.search).min(tw);
                self.cv.fill(tx, y + self.px(7.0), tw2, sh - self.px(14.0), 0x2A5A8A);
            }
            self.cv.text(fb, &self.search.clone(), (tx, y, tw, sh), fg, DT_L);
            // Clear button.
            let cx = x + sw - self.px(32.0);
            let hc = self.hover == Hit::SearchClear;
            if hc {
                self.cv.round_rect(
                    cx as f32,
                    (y + self.px(4.0)) as f32,
                    self.px(26.0) as f32,
                    (sh - self.px(8.0)) as f32,
                    self.px(3.0) as f32,
                    self.th.chrome_sel,
                    1.0,
                );
            }
            let fs = self.f.icon_small;
            self.cv.text(fs, G_CLOSE, (cx, y, self.px(26.0), sh), shint, DT_C);
            self.hit(cx, y, self.px(26.0), sh, Hit::SearchClear);
        }
        if focus && self.caret_on && !self.search_all_selected {
            let cw = self.cv.text_width(fb, &self.search);
            let cx = tx + cw.min(tw) + 1;
            self.cv.fill(cx, y + self.px(8.0), self.px(1.0).max(1), sh - self.px(16.0), fg);
        }
        self.hits.insert(0, (RECT { left: x, top: y, right: x + sw, bottom: y + sh }, Hit::Search));
        let _ = w;
    }

    fn draw_nav(&mut self, h: i32) {
        let nw = self.nav_w();
        let fi = self.f.icon;
        let fnav = self.f.nav;
        let expanded = self.nav_open();
        // Hamburger.
        let hy = self.px(61.0);
        let hh = self.px(40.0);
        if self.hover == Hit::NavToggle {
            self.cv.round_rect(
                self.px(4.0) as f32,
                hy as f32,
                self.px(40.0) as f32,
                hh as f32,
                self.px(4.0) as f32,
                self.th.chrome_hover,
                1.0,
            );
        }
        let fg = self.th.chrome_fg;
        self.cv.text(fi, G_HAMBURGER, (self.px(4.0), hy, self.px(40.0), hh), fg, DT_C);
        self.hit(self.px(4.0), hy, self.px(40.0), hh, Hit::NavToggle);
        let items: [(Page, &str, &str); 7] = [
            (Page::Procs, G_PROCS, "Processes"),
            (Page::Perf, G_PERF, "Performance"),
            (Page::History, G_HISTORY, "App history"),
            (Page::Startup, G_STARTUP, "Startup apps"),
            (Page::Users, G_USERS, "Users"),
            (Page::Details, G_DETAILS, "Details"),
            (Page::Services, G_SERVICES, "Services"),
        ];
        let first = self.px(103.0);
        let settings_y = h - self.px(43.0);
        let pitch = self.px(40.0).min((settings_y - first) / items.len() as i32);
        let ih = pitch - self.px(4.0);
        let mut all: Vec<(Page, &str, &str, i32)> = Vec::new();
        for (i, (p, g, t)) in items.into_iter().enumerate() {
            all.push((p, g, t, first + i as i32 * pitch));
        }
        all.push((Page::Settings, G_SETTINGS, "Settings", settings_y));
        for (p, g, t, y) in all {
            let sel = self.page == p;
            let hov = self.hover == Hit::Nav(p);
            let x = self.px(4.0);
            let w = nw - self.px(8.0);
            if sel || hov {
                let c = if sel { self.th.chrome_sel } else { self.th.chrome_hover };
                self.cv.round_rect(x as f32, y as f32, w as f32, ih as f32, self.px(4.0) as f32, c, 1.0);
            }
            if sel {
                let ph = self.px(16.0);
                self.cv.round_rect(
                    x as f32,
                    (y + (ih - ph) / 2) as f32,
                    self.px(3.0) as f32,
                    ph as f32,
                    self.px(1.5) as f32,
                    self.th.accent,
                    1.0,
                );
            }
            self.cv.text(fi, g, (self.px(4.0), y, self.px(40.0), ih), fg, DT_C);
            if expanded {
                self.cv.text(fnav, t, (self.px(48.0), y, nw - self.px(56.0), ih), fg, DT_L);
            }
            self.hit(x, y, w, ih, Hit::Nav(p));
        }
    }

    // ------------------------------------------------------------------ toolbar helper

    /// Draws a Task Manager style command button; returns its width.
    #[allow(clippy::too_many_arguments)]
    pub fn tool_button(
        &mut self,
        right: i32,
        y: i32,
        h: i32,
        glyph: &str,
        label: &str,
        tool: Tool,
        enabled: bool,
    ) -> i32 {
        let fi = self.f.icon;
        let fb = self.f.body;
        let tw = if label.is_empty() { 0 } else { self.cv.text_width(fb, label) };
        let pad = self.px(11.0);
        let w = if label.is_empty() { self.px(40.0) } else { pad + self.px(16.0) + self.px(10.0) + tw + pad };
        let x = right - w;
        let hov = self.hover == Hit::Tool(tool) && enabled;
        if hov {
            let c = if self.th.dark { 0x2D2D2D } else { 0xEAEAEA };
            self.cv.round_rect(x as f32, y as f32, w as f32, h as f32, self.px(4.0) as f32, c, 1.0);
        }
        let col = if enabled { self.th.text } else { self.th.text3 };
        if label.is_empty() {
            self.cv.text(fi, glyph, (x, y, w, h), col, DT_C);
        } else {
            self.cv.text(fi, glyph, (x + pad, y, self.px(16.0), h), col, DT_C);
            self.cv.text(fb, label, (x + pad + self.px(26.0), y, tw + 2, h), col, DT_L);
        }
        if enabled {
            self.hit(x, y, w, h, Hit::Tool(tool));
        }
        w
    }

    /// Page title + command bar row; returns the y below it.
    pub fn page_header(&mut self, x: i32, y: i32, w: i32, title: &str, tools: &[(Tool, &str, &str, bool)]) -> i32 {
        let rh = self.px(52.0);
        let ft = self.f.title;
        let tc = self.th.text;
        self.cv.text(ft, title, (x + self.px(17.0), y, self.px(300.0), rh), tc, DT_L);
        let title_w = self.cv.text_width(ft, title) + self.px(30.0);
        let labels_w: i32 = tools
            .iter()
            .map(|t| {
                let label =
                    if t.2.is_empty() { self.px(0.0) } else { self.cv.text_width(self.f.body, t.2) + self.px(8.0) };
                self.px(45.0) + label
            })
            .sum();
        let labelled = title_w + labels_w + self.px(13.0) <= w;
        let mut right = x + w - self.px(8.0);
        let by = y + self.px(10.0);
        let bh = self.px(32.0);
        for (i, (tool, g, l, en)) in tools.iter().enumerate().rev() {
            let bw = self.tool_button(right, by, bh, g, if labelled { l } else { "" }, *tool, *en);
            right -= bw + self.px(4.0);
            if i == 1 && tools.len() > 3 {
                // separates Run new task from the page's own commands
                let dc = self.th.divider;
                self.cv.vline(right, by + self.px(4.0), bh - self.px(8.0), dc);
                right -= self.px(5.0);
            }
        }
        y + rh
    }

    // ------------------------------------------------------------------ snapshot

    pub(super) fn on_snapshot(&mut self) {
        if let Some(s) = sampler::control().take() {
            if self.snap.is_none() {
                crate::util::trace("first snapshot received");
            }
            self.snap = Some(s);
            lists::on_sample(self);
            self.rebuild_rows();
            self.invalidate();
        }
    }

    pub fn rebuild_rows(&mut self) {
        if lists::is_list(self.page) {
            lists::rebuild(self);
        } else {
            procs::rebuild(self);
        }
    }

    pub fn set_page(&mut self, p: Page) {
        if self.page != p {
            self.page = p;
            self.nav_peek = false;
            lists::on_open(self);
            self.rebuild_rows();
            self.invalidate();
        }
    }

    // ------------------------------------------------------------------ input

    fn on_mouse_move(&mut self, x: i32, y: i32) {
        self.last_mouse = (x, y);
        if let Some(d) = self.drag {
            match d {
                Drag::Column { page, col, start_x, start_w } => {
                    let g = if page == Page::Procs { &mut self.procs } else { &mut self.details };
                    let nw = ((start_w as f32 * self.s + (x - start_x) as f32) / self.s).max(40.0);
                    if let Some(c) = g.cols.get_mut(col) {
                        c.w = nw;
                    }
                    self.invalidate();
                }
                Drag::Scroll { page, start_y, start_scroll } => {
                    procs::drag_scroll(self, page, start_y, start_scroll, y);
                    self.invalidate();
                }
                Drag::List(drag) => {
                    let (page, scale) = (self.page, self.s);
                    if let Some(table) = self.lists.table(page) {
                        table.drag(drag, x, y, scale);
                    }
                    self.invalidate();
                }
            }
            return;
        }
        let h = self.hit_at(x, y);
        if h != self.hover {
            self.hover = h;
            self.invalidate();
        }
        unsafe {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut tme);
        }
    }

    pub(super) fn on_lbutton_down(&mut self, x: i32, y: i32, dbl: bool) {
        let h = self.hit_at(x, y);
        self.pressed = h;
        let was_focus = self.search_focus;
        if h != Hit::Search && h != Hit::SearchClear {
            self.set_search_focus(false);
        }
        match h {
            Hit::Search => {
                if !was_focus {
                    self.search_all_selected = !self.search.is_empty();
                } else {
                    self.search_all_selected = false;
                }
                self.set_search_focus(true);
            }
            Hit::SearchClear => {
                self.search.clear();
                self.search_all_selected = false;
                self.set_search_focus(true);
                self.rebuild_rows();
            }
            Hit::HdrDiv(page, col) => {
                let g = if page == Page::Procs { &self.procs } else { &self.details };
                let w = g.cols.get(col).map(|c| c.w).unwrap_or(100.0);
                self.drag = Some(Drag::Column { page, col, start_x: x, start_w: w.round() as i32 });
                unsafe { SetCapture(self.hwnd) };
            }
            Hit::VScroll(page) => {
                procs::scroll_click(self, page, y);
                let g = if page == Page::Procs { &self.procs } else { &self.details };
                self.drag = Some(Drag::Scroll { page, start_y: y, start_scroll: g.scroll });
                unsafe { SetCapture(self.hwnd) };
            }
            Hit::Row(page, i) => {
                procs::select_row(self, page, i);
                if dbl {
                    procs::activate_row(self, page, i);
                }
            }
            Hit::Chevron(page, i) => {
                procs::select_row(self, page, i);
                procs::toggle_expand(self, page, i);
            }
            Hit::PerfItem(i) => {
                self.perf_sel = i;
            }
            Hit::List(part) => {
                if let Some(drag) = lists::press(self, part, x, y, dbl) {
                    self.drag = Some(Drag::List(drag));
                    unsafe { SetCapture(self.hwnd) };
                }
            }
            _ => {}
        }
        self.invalidate();
    }

    pub(super) fn on_lbutton_up(&mut self, x: i32, y: i32) {
        if self.drag.take().is_some() {
            unsafe { ReleaseCapture() };
            self.invalidate();
            return;
        }
        let h = self.hit_at(x, y);
        if h != self.pressed {
            return;
        }
        match h {
            Hit::Nav(p) => self.set_page(p),
            Hit::List(part) => lists::release(self, part),
            Hit::NavToggle => {
                if self.nav_fits() {
                    self.settings.nav_expanded = !self.settings.nav_expanded;
                } else {
                    self.nav_peek = !self.nav_peek;
                }
                self.invalidate();
            }
            Hit::Hdr(page, col) => procs::header_click(self, page, col),
            Hit::Tool(t) => self.on_tool(t),
            Hit::Setting(id) => settings::click(self, id),
            Hit::InfoSetup => settings::setup_helper(self),
            Hit::InfoClose => {
                self.settings.hint_dismissed = true;
                settings::save(self);
                self.invalidate();
            }
            _ => {}
        }
    }

    fn on_rbutton_up(&mut self, x: i32, y: i32) {
        let h = self.hit_at(x, y);
        match h {
            Hit::Row(page, i) | Hit::Chevron(page, i) => {
                procs::select_row(self, page, i);
                self.invalidate();
                procs::context_menu(self, page, i);
            }
            Hit::Hdr(page, _) | Hit::HdrDiv(page, _) => procs::column_menu(self, page),
            Hit::PerfGraph | Hit::PerfItem(_) => perf::context_menu(self),
            Hit::List(part) => lists::context_menu(self, part),
            _ => {}
        }
    }

    pub fn on_tool(&mut self, t: Tool) {
        match t {
            Tool::RunTask => run_new_task(self.hwnd),
            Tool::EndTask => procs::end_selected(self),
            Tool::Efficiency => procs::toggle_efficiency_selected(self),
            Tool::More => settings::more_menu(self),
            _ => lists::on_tool(self, t),
        }
    }

    fn on_wheel(&mut self, delta: i32) {
        match self.page {
            Page::Procs | Page::Details => {
                let page = self.page;
                let rows = 3.0;
                let rh = procs::row_h(self) as f32;
                let g = if page == Page::Procs { &mut self.procs } else { &mut self.details };
                g.scroll -= delta as f32 / 120.0 * rows * rh;
                procs::clamp_scroll(self, page);
                let (x, y) = self.last_mouse;
                self.hover = self.hit_at(x, y);
                self.invalidate();
            }
            page => {
                if let Some(table) = self.lists.table(page) {
                    table.wheel(delta);
                    let (x, y) = self.last_mouse;
                    self.hover = self.hit_at(x, y);
                    self.invalidate();
                }
            }
        }
    }

    pub fn set_search_focus(&mut self, on: bool) {
        if self.search_focus == on {
            return;
        }
        self.search_focus = on;
        self.caret_on = true;
        unsafe {
            if on {
                SetTimer(self.hwnd, TIMER_CARET, GetCaretBlinkTime().max(300), None);
            } else {
                KillTimer(self.hwnd, TIMER_CARET);
                self.search_all_selected = false;
            }
        }
        self.invalidate();
    }

    fn on_char(&mut self, ch: u16) {
        if !self.search_focus {
            return;
        }
        match ch {
            8 => {
                if self.search_all_selected {
                    self.search.clear();
                } else {
                    self.search.pop();
                }
            }
            1 => {
                // Ctrl+A
                self.search_all_selected = true;
                self.invalidate();
                return;
            }
            22 => {
                if let Some(t) = clipboard_text() {
                    if self.search_all_selected {
                        self.search.clear();
                    }
                    let t: String = t.chars().filter(|c| !c.is_control()).collect();
                    self.search.push_str(&t);
                }
            }
            3 => {
                set_clipboard_text(self.hwnd, &self.search);
                return;
            }
            13 | 27 | 9 => return,
            c if c >= 32 => {
                if self.search_all_selected {
                    self.search.clear();
                }
                if let Some(c) = char::from_u32(c as u32) {
                    if self.search.chars().count() < 120 {
                        self.search.push(c);
                    }
                }
            }
            _ => return,
        }
        self.search_all_selected = false;
        self.caret_on = true;
        self.procs.scroll = 0.0;
        self.details.scroll = 0.0;
        self.lists.reset_scroll();
        self.rebuild_rows();
        self.invalidate();
    }

    pub(super) fn on_key(&mut self, vk: u16) -> bool {
        let ctrl = unsafe { GetKeyState(VK_CONTROL as i32) } < 0;
        if ctrl && (vk == b'F' as u16 || vk == b'E' as u16) {
            self.search_all_selected = !self.search.is_empty();
            self.set_search_focus(true);
            return true;
        }
        if self.search_focus {
            match vk {
                v if v == VK_ESCAPE => {
                    if self.search.is_empty() {
                        self.set_search_focus(false);
                    } else {
                        self.search.clear();
                        self.rebuild_rows();
                    }
                    self.invalidate();
                    return true;
                }
                v if v == VK_DOWN || v == VK_RETURN || v == VK_TAB => {
                    self.set_search_focus(false);
                    return true;
                }
                v if v == VK_DELETE => {
                    if self.search_all_selected {
                        self.search.clear();
                        self.search_all_selected = false;
                        self.rebuild_rows();
                        self.invalidate();
                    }
                    return true;
                }
                _ => return false,
            }
        }
        if vk == VK_F5 {
            sampler::control().wake();
            lists::reload(self);
            return true;
        }
        match self.page {
            Page::Procs | Page::Details => procs::on_key(self, vk),
            Page::Perf => perf::on_key(self, vk),
            _ => lists::on_key(self, vk),
        }
    }

    pub fn toast(&mut self, msg: &str) {
        self.busy_note =
            Some((msg.to_string(), unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() }));
        self.invalidate();
    }
}

pub fn app_icon(size: f32) -> isize {
    use std::sync::atomic::{AtomicIsize, Ordering};
    static SMALL: AtomicIsize = AtomicIsize::new(0);
    let h = SMALL.load(Ordering::Acquire);
    if h != 0 {
        return h;
    }
    let v = window::app_icon(size.round() as i32);
    SMALL.store(v, Ordering::Release);
    v
}

fn run_new_task(hwnd: HWND) {
    // The shell's own "Ausführen" dialog (shell32 ordinal 61), as Task Manager uses.
    type RunFileDlg = unsafe extern "system" fn(HWND, HICON, *const u16, *const u16, *const u16, u32);
    unsafe {
        let lib = wide("shell32.dll");
        let m = windows_sys::Win32::System::LibraryLoader::LoadLibraryW(lib.as_ptr());
        if m.is_null() {
            return;
        }
        if let Some(f) = windows_sys::Win32::System::LibraryLoader::GetProcAddress(m, 61 as *const u8) {
            let f: RunFileDlg = std::mem::transmute(f);
            let title = wide("Create new task");
            let prompt = wide(
                "Type the name of a program, folder, document, or Internet resource, and Windows will open it for you.",
            );
            f(hwnd, std::ptr::null_mut(), std::ptr::null(), title.as_ptr(), prompt.as_ptr(), 0);
        }
    }
}

fn clipboard_text() -> Option<String> {
    use windows_sys::Win32::System::DataExchange::*;
    use windows_sys::Win32::System::Memory::*;
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let h = GetClipboardData(13);
        let mut out = None;
        if !h.is_null() {
            let p = GlobalLock(h as _) as *const u16;
            if !p.is_null() {
                out = Some(crate::util::from_wide_ptr(p));
                GlobalUnlock(h as _);
            }
        }
        CloseClipboard();
        out
    }
}

pub fn set_clipboard_text(hwnd: HWND, s: &str) {
    use windows_sys::Win32::System::DataExchange::*;
    use windows_sys::Win32::System::Memory::*;
    let w = wide(s);
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return;
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2);
        if !h.is_null() {
            let p = GlobalLock(h) as *mut u16;
            std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
            GlobalUnlock(h);
            SetClipboardData(13, h as _);
        }
        CloseClipboard();
    }
}

// ---------------------------------------------------------------------- dark menus

fn enable_dark_menus(hwnd: HWND) {
    // uxtheme ordinals 135 (SetPreferredAppMode), 133 (AllowDarkModeForWindow), 136 (FlushMenuThemes).
    unsafe {
        let lib = wide("uxtheme.dll");
        let m = windows_sys::Win32::System::LibraryLoader::LoadLibraryW(lib.as_ptr());
        if m.is_null() {
            return;
        }
        let gp = |o: usize| windows_sys::Win32::System::LibraryLoader::GetProcAddress(m, o as *const u8);
        if let Some(f) = gp(135) {
            let f: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            f(1); // AllowDark: follow the system setting
        }
        if let Some(f) = gp(133) {
            let f: unsafe extern "system" fn(HWND, i32) -> i32 = std::mem::transmute(f);
            f(hwnd, 1);
        }
        if let Some(f) = gp(136) {
            let f: unsafe extern "system" fn() = std::mem::transmute(f);
            f();
        }
    }
}

// ---------------------------------------------------------------------- window plumbing

fn frame_metrics(dpi: u32) -> (i32, i32) {
    unsafe {
        let pad = GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
        (GetSystemMetricsForDpi(SM_CXFRAME, dpi) + pad, GetSystemMetricsForDpi(SM_CYFRAME, dpi) + pad)
    }
}

unsafe fn apply_frame(hwnd: HWND, dark: bool) {
    let on: i32 = if dark { 1 } else { 0 };
    DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, &on as *const _ as *const c_void, 4);
    let backdrop: i32 = 2; // DWMSBT_MAINWINDOW (Mica)
    DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE as u32, &backdrop as *const _ as *const c_void, 4);
    let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
    DwmExtendFrameIntoClientArea(hwnd, &m);
}

pub(super) unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Let DWM handle the caption buttons it draws in our extended frame.
    // Only non-client messages: in the client area DWM would otherwise swallow clicks.
    let mut lr: LRESULT = 0;
    let nc = msg == WM_NCHITTEST || (0x00A0..=0x00AD).contains(&msg) || msg == 0x02A0 || msg == 0x02A2;
    if nc && DwmDefWindowProc(hwnd, msg, wp, lp, &mut lr) != 0 {
        return lr;
    }
    match msg {
        WM_NCCREATE => {
            crate::util::trace("WM_NCCREATE");
        }
        WM_NCCALCSIZE if wp != 0 => {
            crate::util::trace("WM_NCCALCSIZE");
            let p = &mut *(lp as *mut NCCALCSIZE_PARAMS);
            let dpi = GetDpiForWindow(hwnd);
            let (fx, fy) = frame_metrics(dpi);
            let r = &mut p.rgrc[0];
            r.left += fx;
            r.right -= fx;
            r.bottom -= fy;
            if IsZoomed(hwnd) != 0 {
                r.top += fy;
            }
            return 0;
        }
        WM_NCHITTEST => {
            let def = DefWindowProcW(hwnd, msg, wp, lp);
            if def != HTCLIENT as isize {
                return def;
            }
            let Some(a) = app_opt() else { return def };
            let mut pt = POINT { x: (lp & 0xFFFF) as i16 as i32, y: ((lp >> 16) & 0xFFFF) as i16 as i32 };
            ScreenToClient(hwnd, &mut pt);
            let (fx, fy) = frame_metrics(a.dpi);
            let (w, _) = a.client();
            if IsZoomed(hwnd) == 0 && pt.y < fy {
                return if pt.x < fx * 2 {
                    HTTOPLEFT as isize
                } else if pt.x > w - fx * 2 {
                    HTTOPRIGHT as isize
                } else {
                    HTTOP as isize
                };
            }
            if pt.y < a.title_h() {
                let (sx, sy, sw, sh) = a.search_rect();
                if pt.x >= sx && pt.x < sx + sw && pt.y >= sy && pt.y < sy + sh {
                    return HTCLIENT as isize;
                }
                if pt.x < a.px(40.0) && pt.y < a.px(48.0) {
                    return HTSYSMENU as isize;
                }
                return HTCAPTION as isize;
            }
            return HTCLIENT as isize;
        }
        WM_CREATE => {
            crate::util::trace("WM_CREATE");
            let dpi = GetDpiForWindow(hwnd);
            let s = dpi as f32 / 96.0;
            let th = Theme::load();
            crate::util::trace("theme");
            apply_frame(hwnd, th.dark);
            crate::util::trace("apply_frame");
            enable_dark_menus(hwnd);
            crate::util::trace("dark_menus");
            let settings = settings::load();
            crate::util::trace("settings");
            let fonts = Fonts::new(s);
            crate::util::trace("fonts");
            let mut a = Box::new(App {
                hwnd,
                dpi,
                s,
                th,
                f: fonts,
                cv: Canvas::new(),
                page: settings.start_page,
                snap: None,
                hits: Vec::with_capacity(256),
                hover: Hit::None,
                pressed: Hit::None,
                search: String::new(),
                search_focus: false,
                caret_on: false,
                search_all_selected: false,
                procs: procs::Grid::processes(),
                details: procs::Grid::details(),
                expanded: HashSet::new(),
                perf_sel: 0,
                lists: lists::Lists::new(),
                nav_peek: false,
                settings,
                drag: None,
                busy_note: None,
                registered: crate::shell_hook::state().is_ours(),
                last_mouse: (0, 0),
                paint_stats: [0.0; 5],
            });
            settings::apply_columns(&mut a);
            crate::util::trace("app");
            APP = Box::into_raw(a);
            let a = app();
            meta::set_notify_window(hwnd);
            meta::set_icon_size((16.0 * s).round() as i32);
            sampler::control().interval_ms.store(a.settings.interval_ms, std::sync::atomic::Ordering::Release);
            sampler::start(hwnd);
            crate::util::trace("sampler started");
            lists::on_open(a);
            if a.settings.topmost {
                SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
            }
            // (WM_NCCALCSIZE is handled from the very first message, so no
            // SWP_FRAMECHANGED round trip is needed here - it cost ~45 ms.)
            crate::util::trace("WM_CREATE done");
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            if let Some(a) = app_opt() {
                if a.snap.is_none() {
                    // A snapshot may already be waiting: show data in the very first frame.
                    a.on_snapshot();
                }
                a.paint();
                let (w, h) = a.client();
                a.cv.present(hdc, 0, 0, w, h);
                crate::util::trace(if a.snap.is_some() { "paint (data)" } else { "paint (empty)" });
            }
            EndPaint(hwnd, &ps);
            return 0;
        }
        WM_SIZE => {
            if let Some(a) = app_opt() {
                procs::clamp_scroll(a, Page::Procs);
                procs::clamp_scroll(a, Page::Details);
                a.invalidate();
            }
            return 0;
        }
        WM_GETMINMAXINFO => {
            let mi = &mut *(lp as *mut MINMAXINFO);
            let s = GetDpiForWindow(hwnd) as f32 / 96.0;
            mi.ptMinTrackSize.x = (560.0 * s) as i32;
            mi.ptMinTrackSize.y = (380.0 * s) as i32;
            return 0;
        }
        WM_DPICHANGED => {
            if let Some(a) = app_opt() {
                let dpi = (wp & 0xFFFF) as u32;
                a.dpi = dpi;
                a.s = dpi as f32 / 96.0;
                a.f.destroy();
                a.f = Fonts::new(a.s);
                meta::set_icon_size((16.0 * a.s).round() as i32);
                let r = &*(lp as *const RECT);
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                a.invalidate();
            }
            return 0;
        }
        WM_SETTINGCHANGE => {
            if let Some(a) = app_opt() {
                a.th = Theme::load();
                apply_frame(hwnd, a.th.dark);
                a.invalidate();
            }
        }
        WM_DWMCOLORIZATIONCOLORCHANGED => {
            if let Some(a) = app_opt() {
                a.th = Theme::load();
                a.invalidate();
            }
        }
        WM_ACTIVATE => {
            if let Some(a) = app_opt() {
                a.invalidate();
            }
        }
        sampler::WM_APP_SNAPSHOT => {
            if let Some(a) = app_opt() {
                a.on_snapshot();
            }
            return 0;
        }
        meta::WM_APP_META => {
            if let Some(a) = app_opt() {
                a.rebuild_rows();
                a.invalidate();
            }
            return 0;
        }
        WM_APP_DONE => {
            if let Some(a) = app_opt() {
                a.registered = crate::shell_hook::state().is_ours();
                sampler::control().want_helper_restart.store(true, std::sync::atomic::Ordering::Release);
                sampler::control().wake();
                lists::reload(a);
                a.invalidate();
            }
            return 0;
        }
        crate::pages::WM_APP_LIST => {
            if let Some(a) = app_opt() {
                lists::on_message(a, wp);
            }
            return 0;
        }
        WM_TIMER => {
            if wp == TIMER_CARET {
                if let Some(a) = app_opt() {
                    a.caret_on = !a.caret_on;
                    a.invalidate();
                }
            }
            return 0;
        }
        WM_MOUSEMOVE => {
            if let Some(a) = app_opt() {
                a.on_mouse_move((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            }
            return 0;
        }
        WM_MOUSELEAVE => {
            if let Some(a) = app_opt() {
                if a.hover != Hit::None && a.drag.is_none() {
                    a.hover = Hit::None;
                    a.invalidate();
                }
            }
            return 0;
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            if let Some(a) = app_opt() {
                SetFocus(hwnd);
                a.on_lbutton_down(
                    (lp & 0xFFFF) as i16 as i32,
                    ((lp >> 16) & 0xFFFF) as i16 as i32,
                    msg == WM_LBUTTONDBLCLK,
                );
            }
            return 0;
        }
        WM_LBUTTONUP => {
            if let Some(a) = app_opt() {
                a.on_lbutton_up((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            }
            return 0;
        }
        WM_RBUTTONUP => {
            if let Some(a) = app_opt() {
                a.on_rbutton_up((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            }
            return 0;
        }
        WM_MOUSEWHEEL => {
            if let Some(a) = app_opt() {
                a.on_wheel(((wp >> 16) & 0xFFFF) as i16 as i32);
            }
            return 0;
        }
        WM_CAPTURECHANGED => {
            if let Some(a) = app_opt() {
                a.drag = None;
            }
        }
        WM_SETCURSOR => {
            if let Some(a) = app_opt() {
                if (lp & 0xFFFF) as u32 == HTCLIENT {
                    let mut pt = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut pt);
                    ScreenToClient(hwnd, &mut pt);
                    let h = a.hit_at(pt.x, pt.y);
                    let cur = match (h, a.drag) {
                        (_, Some(Drag::Column { .. }))
                        | (Hit::HdrDiv(..) | Hit::List(table::Part::Divider(_)), _)
                        | (_, Some(Drag::List(table::Drag::Column { .. }))) => IDC_SIZEWE,
                        (Hit::Search, _) => IDC_IBEAM,
                        _ => IDC_ARROW,
                    };
                    SetCursor(LoadCursorW(std::ptr::null_mut(), cur));
                    return 1;
                }
            }
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if let Some(a) = app_opt() {
                if a.on_key(wp as u16) {
                    return 0;
                }
            }
        }
        WM_CHAR => {
            if let Some(a) = app_opt() {
                a.on_char(wp as u16);
            }
            return 0;
        }
        WM_CLOSE => {
            if let Some(a) = app_opt() {
                settings::save(a);
            }
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

pub fn run() {
    if window::activate_existing() {
        return;
    }
    let saved = settings::load();
    sampler::control().interval_ms.store(saved.interval_ms, std::sync::atomic::Ordering::Release);
    sampler::start_early();

    let bounds = saved.placement.map_or_else(
        || window::Bounds::default_for(unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForSystem() } as f32 / 96.0),
        |r| window::Bounds { left: r.left, top: r.top, width: r.right - r.left, height: r.bottom - r.top },
    );
    let Some(main_window) = window::create(window::Purpose::App, bounds) else { return };
    // SHOWDEFAULT respects "run minimised" in shortcuts.
    let how = if saved.maximized { SW_SHOWMAXIMIZED } else { SW_SHOWDEFAULT };
    unsafe {
        ShowWindow(main_window, how);
        UpdateWindow(main_window);
    }
    crate::util::trace("first frame");
    window::run_until_closed();
}
