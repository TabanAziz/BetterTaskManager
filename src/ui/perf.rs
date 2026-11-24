//! Performance page: device list with live thumbnails, detail graphs and statistics.
//!
//! The figures at the bottom are measured first and the graphs get the space that is
//! left, so nothing is squashed, overlaps or falls out of the window at any size.

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_UP};
use windows_sys::Win32::UI::WindowsAndMessaging::CreatePopupMenu;

use super::gfx::*;
use super::plot::{plot, Area, Series, Style};
use super::{App, Hit, Tool, G_MORE, G_RUN};
use crate::model::*;
use crate::util::*;

#[derive(Clone, Copy, PartialEq)]
enum Dev {
    Cpu,
    Mem,
    Disk(usize),
    Net(usize),
    Gpu(usize),
}

fn devices(p: &Perf) -> Vec<Dev> {
    let mut v = vec![Dev::Cpu, Dev::Mem];
    v.extend((0..p.disks.len()).map(Dev::Disk));
    v.extend((0..p.nets.len()).map(Dev::Net));
    v.extend((0..p.gpus.len()).map(Dev::Gpu));
    v
}

fn gb(bytes: u64) -> String {
    fmt_num(bytes as f64 / 1073741824.0, 1)
}

fn mb(bytes: u64) -> String {
    fmt_num(bytes as f64 / 1048576.0, 0)
}

fn percent(v: f32) -> String {
    format!("{}%", fmt_num(v.round() as f64, 0))
}

fn or_dash(text: &str) -> String {
    if text.is_empty() {
        "—".into()
    } else {
        text.into()
    }
}

fn yes_no(v: bool) -> String {
    if v {
        "Yes".into()
    } else {
        "No".into()
    }
}

/// The smallest round number at or above `peak`, round in the unit it is displayed in
/// (`base` 1024 for bytes, 1000 for bits).
fn axis_max(peak: f32, least: f32, base: f32) -> f32 {
    let target = peak.max(least);
    let mut unit = 1.0;
    while unit * base <= target {
        unit *= base;
    }
    [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0]
        .iter()
        .map(|m| m * unit)
        .find(|&v| v >= target)
        .unwrap_or(unit * base)
}

/// NaN (not measured yet) is ignored.
fn peak(a: &[f32], b: &[f32]) -> f32 {
    a.iter().chain(b).copied().fold(0.0, f32::max)
}

/// What the x axis spans at the chosen update speed.
fn time_span(interval_ms: u32) -> String {
    let interval = if interval_ms == 0 { 1000 } else { interval_ms };
    let seconds = HISTORY as u32 * interval / 1000;
    if seconds > 60 {
        format!("{} minutes", seconds / 60)
    } else {
        format!("{seconds} seconds")
    }
}

fn disk_title(d: &DiskInfo) -> String {
    if d.letters.is_empty() {
        format!("Disk {}", d.index)
    } else {
        format!("Disk {} ({})", d.index, d.letters)
    }
}

/// Columns for `n` logical-processor graphs in a w × h area: slightly wide cells, full rows.
fn core_columns(n: usize, w: i32, h: i32) -> usize {
    let cost = |cols: usize| {
        let rows = n.div_ceil(cols);
        let aspect = (w as f32 / cols as f32) / (h as f32 / rows as f32).max(1.0);
        let holes = if n.is_multiple_of(cols) { 0.0 } else { 0.35 };
        (aspect.max(0.01) / 1.8).ln().abs() + holes
    };
    (1..=n).min_by(|&a, &b| cost(a).total_cmp(&cost(b))).unwrap_or(1)
}

struct Stat {
    label: &'static str,
    value: String,
    /// Line sample in front of the figure: Some(dashed).
    key: Option<bool>,
}

fn stat(label: &'static str, value: String) -> Stat {
    Stat { label, value, key: None }
}

struct Summary {
    color: Color,
    history: Vec<f32>,
    max: f32,
    title: String,
    detail: String,
    value: String,
}

/// The block of figures and facts below the graphs, measured.
struct Info {
    /// (x, y, width, stat), relative to the block.
    stats: Vec<(i32, i32, i32, Stat)>,
    /// (x, y, label, value)
    facts: Vec<(i32, i32, &'static str, String)>,
    label_w: i32,
    value_w: i32,
    height: i32,
}

impl App {
    fn series_fill(&self, c: Color) -> Color {
        if self.th.custom.is_some() {
            mix(self.th.content, c, if self.th.dark { 0.27 } else { 0.14 })
        } else if self.th.dark {
            scale_color(c, 0.27)
        } else {
            mix(0xFFFFFF, c, 0.14)
        }
    }

    fn tick(&self) -> u64 {
        self.snap.as_ref().map_or(0, |s| s.seq)
    }

    fn frame(&mut self, r: Area, color: Color) {
        self.cv.hline(r.x, r.y, r.w, color);
        self.cv.hline(r.x, r.y + r.h - 1, r.w, color);
        self.cv.vline(r.x, r.y, r.h, color);
        self.cv.vline(r.x + r.w - 1, r.y, r.h, color);
    }

    fn chart(&mut self, r: Area, color: Color, series: &[Series], max: f32) {
        if r.w < 3 || r.h < 3 {
            return;
        }
        let bg = self.th.content;
        self.cv.fill(r.x, r.y, r.w, r.h, bg);
        let roomy = r.w >= self.px(90.0) && r.h >= self.px(36.0);
        let style = Style {
            line: color,
            fill: self.series_fill(color),
            grid: roomy.then_some(color),
            tick: self.tick(),
            stroke: 1.4 * self.px(100.0) as f32 / 100.0,
        };
        plot(&mut self.cv, Area { x: r.x + 1, y: r.y + 1, w: r.w - 2, h: r.h - 2 }, series, max, &style);
        self.frame(r, color);
    }

    fn graph_labels(&mut self, x: i32, y: i32, w: i32, left: &str, right: &str) {
        let (font, color, h) = (self.f.small, self.th.text2, self.px(18.0));
        let rw = (self.cv.text_width(font, right) + 2).min(w);
        self.cv.text(font, left, (x, y, w - rw - self.px(6.0), h), color, DT_L);
        self.cv.text(font, right, (x + w - rw, y, rw, h), color, DT_R);
    }

    /// Label, plot and, if given, the time axis; together they fill `r`.
    #[allow(clippy::too_many_arguments)]
    fn titled_chart(
        &mut self,
        r: Area,
        left: &str,
        right: &str,
        span: Option<&str>,
        color: Color,
        series: &[Series],
        max: f32,
    ) {
        let label = self.px(18.0);
        let axis = if span.is_some() { self.px(20.0) } else { 0 };
        self.graph_labels(r.x, r.y, r.w, left, right);
        let body = Area { x: r.x, y: r.y + label, w: r.w, h: r.h - label - axis };
        self.chart(body, color, series, max);
        if let Some(span) = span {
            self.graph_labels(r.x, body.y + body.h + self.px(2.0), r.w, span, "0");
        }
    }

    // ---------------------------------------------------------------- page

    pub fn draw_perf_page(&mut self, x: i32, y: i32, w: i32, h: i32) {
        let tools: Vec<(Tool, &str, &str, bool)> =
            vec![(Tool::RunTask, G_RUN, "Run new task", true), (Tool::More, G_MORE, "", true)];
        let top = self.page_header(x, y, w, "Performance", &tools) + self.px(4.0);
        let Some(snap) = self.snap.as_ref() else { return };
        let perf = snap.perf.clone();
        let totals = (snap.n_procs, snap.n_threads, snap.n_handles);
        let devs = devices(&perf);
        if self.perf_sel >= devs.len() {
            self.perf_sel = 0;
        }
        // In a narrow window the list shrinks to its thumbnails so the graphs stay.
        let wide = w >= self.px(236.0 + 300.0);
        let list_w = self.px(if wide { 236.0 } else { 84.0 });
        let bottom = y + h;
        self.device_list(&perf, &devs, Area { x, y: top, w: list_w, h: bottom - top }, wide);

        let margin = self.px(if wide { 20.0 } else { 10.0 });
        let r = Area { x: x + list_w + margin, y: top, w: w - list_w - 2 * margin, h: bottom - top - self.px(12.0) };
        if r.w < self.px(120.0) || r.h < self.px(90.0) {
            return;
        }
        match devs[self.perf_sel] {
            Dev::Cpu => self.cpu_detail(&perf, totals, r),
            Dev::Mem => self.mem_detail(&perf.mem, r),
            Dev::Disk(i) => self.disk_detail(&perf.disks[i], r),
            Dev::Net(i) => self.net_detail(&perf.nets[i], r),
            Dev::Gpu(i) => self.gpu_detail(&perf.gpus[i], r),
        }
    }

    fn device_list(&mut self, perf: &Perf, devs: &[Dev], r: Area, wide: bool) {
        let n = devs.len() as i32;
        let full = self.px(69.0);
        let three_lines = wide && n * full <= r.h;
        let pitch = if three_lines { full } else { self.px(51.0) };
        let item_h = pitch - self.px(5.0);
        let visible = ((r.h + self.px(5.0)) / pitch).max(1) as usize;
        let first = (self.perf_sel + 1).saturating_sub(visible);
        let (thumb_w, thumb_h) = (self.px(61.0), self.px(35.0));
        let tick = self.tick();

        for (slot, i) in (first..devs.len().min(first + visible)).enumerate() {
            let iy = r.y + slot as i32 * pitch;
            let (ix, iw) = (r.x + self.px(6.0), r.w - self.px(12.0));
            let selected = i == self.perf_sel;
            if selected || self.hover == Hit::PerfItem(i) {
                let c = if selected {
                    self.th.color_for(0x272727, 0xEAEAEA, self.th.chrome_sel)
                } else {
                    self.th.color_for(0x222222, 0xF0F0F0, self.th.chrome_hover)
                };
                let radius = self.px(4.0) as f32;
                self.cv.round_rect(ix as f32, iy as f32, iw as f32, item_h as f32, radius, c, 1.0);
            }
            let s = self.summary(perf, devs[i]);
            let tx = r.x + if wide { self.px(16.0) } else { (r.w - thumb_w) / 2 };
            let thumb = Area { x: tx, y: iy + (item_h - thumb_h) / 2, w: thumb_w, h: thumb_h };
            let (back, border) = (self.th.thumb_bg, self.th.thumb_border);
            self.cv.fill(thumb.x, thumb.y, thumb.w, thumb.h, back);
            let style = Style { line: s.color, fill: self.series_fill(s.color), grid: None, tick, stroke: 1.2 };
            let inner = Area { x: thumb.x + 1, y: thumb.y + 1, w: thumb.w - 2, h: thumb.h - 2 };
            plot(&mut self.cv, inner, &[Series { values: &s.history, dashed: false }], s.max, &style);
            self.frame(thumb, border);

            if wide {
                let (lx, lw) = (r.x + self.px(88.0), r.w - self.px(96.0));
                let (title_font, small) = (self.f.thumb_title, self.f.small);
                let (t1, t2) = (self.th.text, self.th.text2);
                self.cv.text(title_font, &s.title, (lx, iy + self.px(4.0), lw, self.px(20.0)), t1, DT_L);
                let mut ly = iy + self.px(24.0);
                if three_lines && !s.detail.is_empty() {
                    self.cv.text(small, &s.detail, (lx, ly, lw, self.px(16.0)), t2, DT_L);
                    ly += self.px(16.0);
                }
                self.cv.text(small, &s.value, (lx, ly, lw, self.px(16.0)), t2, DT_L);
            }
            self.hit(ix, iy, iw, item_h, Hit::PerfItem(i));
        }
    }

    fn summary(&self, p: &Perf, d: Dev) -> Summary {
        match d {
            Dev::Cpu => Summary {
                color: self.th.cpu,
                history: p.cpu.hist.clone(),
                max: 100.0,
                title: "CPU".into(),
                detail: String::new(),
                value: format!("{}  {} GHz", percent(p.cpu.util), fmt_num(p.cpu.speed_mhz as f64 / 1000.0, 2)),
            },
            Dev::Mem => {
                let m = &p.mem;
                let share = if m.total > 0 { m.in_use as f32 / m.total as f32 * 100.0 } else { 0.0 };
                Summary {
                    color: self.th.mem,
                    history: m.hist.clone(),
                    max: 100.0,
                    title: "Memory".into(),
                    detail: String::new(),
                    value: format!("{}/{} GB ({})", gb(m.in_use), gb(m.total), percent(share)),
                }
            }
            Dev::Disk(i) => {
                let d = &p.disks[i];
                Summary {
                    color: self.th.disk,
                    history: d.hist_active.clone(),
                    max: 100.0,
                    title: disk_title(d),
                    detail: d.kind.clone(),
                    value: percent(d.active),
                }
            }
            Dev::Net(i) => {
                let n = &p.nets[i];
                Summary {
                    color: self.th.net,
                    history: n.hist_recv.iter().zip(&n.hist_send).map(|(a, b)| a.max(*b)).collect(),
                    max: axis_max(peak(&n.hist_send, &n.hist_recv), 100_000.0, 1000.0),
                    title: n.title.clone(),
                    detail: n.alias.clone(),
                    value: net_short(n.send_bps, n.recv_bps),
                }
            }
            Dev::Gpu(i) => {
                let g = &p.gpus[i];
                let temp = g.temp_c.map(|t| format!(" ({} °C)", fmt_num(t.round() as f64, 0))).unwrap_or_default();
                Summary {
                    color: self.th.gpu,
                    history: g.hist_util.clone(),
                    max: 100.0,
                    title: format!("GPU {}", g.index),
                    detail: g.name.clone(),
                    value: format!("{}{}", percent(g.util), temp),
                }
            }
        }
    }

    // ---------------------------------------------------------------- figures

    /// Task Manager's arrangement (figures left, facts right) if it fits, otherwise both
    /// wrap; in a very short window facts go first, then figures.
    fn measure_info(&self, w: i32, max_h: i32, groups: Vec<Vec<Stat>>, facts: Vec<(&'static str, String)>) -> Info {
        let (small, big) = (self.f.small, self.f.stat_val);
        let (gap, line, fact_line) = (self.px(22.0), self.px(50.0), self.px(20.0));
        let key_w = self.px(12.0);
        let facts: Vec<(&'static str, String)> = facts.into_iter().filter(|f| !f.1.is_empty()).collect();
        let label_w = facts.iter().map(|f| self.cv.text_width(small, f.0)).max().unwrap_or(0) + self.px(14.0);
        let value_w = facts.iter().map(|f| self.cv.text_width(small, &f.1)).max().unwrap_or(0) + 2;
        let value_w = value_w.min((w - label_w).max(0));
        let table_w = label_w + value_w;

        let group_lens: Vec<usize> = groups.iter().map(Vec::len).collect();
        let stats: Vec<Stat> = groups.into_iter().flatten().collect();
        let widths: Vec<i32> = stats
            .iter()
            .map(|s| {
                let text = self.cv.text_width(small, s.label).max(self.cv.text_width(big, &s.value));
                text + 2 + if s.key.is_some() { key_w } else { 0 }
            })
            .collect();
        let columns = |lens: &[usize]| -> Vec<i32> {
            let mut cols = vec![0; lens.iter().copied().max().unwrap_or(0)];
            let mut next = 0;
            for &len in lens {
                for col in cols.iter_mut().take(len) {
                    *col = widths[next].max(*col);
                    next += 1;
                }
            }
            cols
        };
        let total = |cols: &[i32]| cols.iter().sum::<i32>() + gap * (cols.len() as i32 - 1).max(0);

        let cols = columns(&group_lens);
        let stats_w = total(&cols);
        let beside_h = (group_lens.len() as i32 * line).max(facts.len() as i32 * fact_line);
        let beside = stats_w + gap + table_w <= w && beside_h <= max_h;

        let (lens, cols) = if beside {
            (group_lens, cols)
        } else {
            let wrap = |per_row: usize| -> Vec<usize> {
                (0..stats.len()).step_by(per_row).map(|at| per_row.min(stats.len() - at)).collect()
            };
            let per_row = (1..=stats.len()).rev().find(|&k| total(&columns(&wrap(k))) <= w).unwrap_or(1);
            let lens = wrap(per_row);
            let cols = columns(&lens);
            (lens, cols)
        };
        let keep_rows = lens.len().min((max_h / line).max(0) as usize);
        let keep_rows = if beside { lens.len() } else { keep_rows };

        let mut info = Info { stats: Vec::new(), facts: Vec::new(), label_w, value_w, height: 0 };
        let mut rest = stats.into_iter();
        for (row, &len) in lens.iter().take(keep_rows).enumerate() {
            let mut sx = 0;
            for &cw in cols.iter().take(len) {
                if let Some(s) = rest.next() {
                    info.stats.push((sx, row as i32 * line, cw.min(w - sx), s));
                }
                sx += cw + gap;
            }
        }
        info.height = keep_rows as i32 * line;

        if beside {
            let fx = (w - table_w).min(w * 11 / 20).max(stats_w + gap);
            for (i, (label, value)) in facts.into_iter().enumerate() {
                info.facts.push((fx, i as i32 * fact_line, label, value));
            }
            info.height = beside_h;
        } else if !facts.is_empty() && keep_rows == lens.len() {
            let across = ((w + gap) / (table_w + gap).max(1)).clamp(1, 3);
            let down = (facts.len() as i32 + across - 1) / across;
            let fy = info.height + self.px(4.0);
            if fy + down * fact_line <= max_h {
                for (i, (label, value)) in facts.into_iter().enumerate() {
                    let (col, row) = (i as i32 / down, i as i32 % down);
                    info.facts.push((col * (table_w + gap), fy + row * fact_line, label, value));
                }
                info.height = fy + down * fact_line;
            }
        }
        info
    }

    fn draw_info(&mut self, x: i32, y: i32, info: &Info, color: Color) {
        let (small, big) = (self.f.small, self.f.stat_val);
        let (t1, t2) = (self.th.text, self.th.text2);
        let key_w = self.px(12.0);
        for (sx, sy, sw, s) in &info.stats {
            let (mut tx, mut tw, ty) = (x + sx, *sw, y + sy);
            if let Some(dashed) = s.key {
                let (from, to) = (ty + self.px(3.0), ty + self.px(41.0));
                let (stroke, dash) = (self.px(2.0).max(2), self.px(4.0).max(3));
                let mut ky = from;
                while ky < to {
                    let len = if dashed { dash.min(to - ky) } else { to - ky };
                    self.cv.fill(tx, ky, stroke, len, color);
                    ky += if dashed { dash + self.px(3.0).max(2) } else { to - from };
                }
                tx += key_w;
                tw -= key_w;
            }
            self.cv.text(small, s.label, (tx, ty, tw, self.px(16.0)), t2, DT_L);
            self.cv.text(big, &s.value, (tx, ty + self.px(16.0), tw, self.px(28.0)), t1, DT_L);
        }
        for (fx, fy, label, value) in &info.facts {
            let line = self.px(20.0);
            self.cv.text(small, label, (x + fx, y + fy, info.label_w, line), t2, DT_L);
            self.cv.text(small, value, (x + fx + info.label_w, y + fy, info.value_w, line), t1, DT_L);
        }
    }

    /// Title and figures of a detail view; returns the area left for graphs.
    fn detail_frame(
        &mut self,
        r: Area,
        heading: (&str, &str),
        color: Color,
        graphs_need: f32,
        stats: Vec<Vec<Stat>>,
        facts: Vec<(&'static str, String)>,
    ) -> Area {
        let (big, name_font, text) = (self.f.perf_title, self.f.perf_name, self.th.text);
        let head = self.px(44.0);
        let title_w = (self.cv.text_width(big, heading.0) + 2).min(r.w);
        self.cv.text(big, heading.0, (r.x, r.y, title_w, head), text, DT_L);
        let nx = r.x + title_w + self.px(16.0);
        self.cv.text(name_font, heading.1, (nx, r.y + self.px(4.0), r.x + r.w - nx, head), text, DT_R);

        let top = r.y + head + self.px(4.0);
        let body = r.y + r.h - top;
        let gap = self.px(10.0);
        let info = self.measure_info(r.w, body - self.px(graphs_need) - gap, stats, facts);
        self.draw_info(r.x, top + body - info.height, &info, color);
        let used = if info.height > 0 { info.height + gap } else { 0 };
        Area { x: r.x, y: top, w: r.w, h: body - used }
    }

    // ---------------------------------------------------------------- devices

    fn cpu_detail(&mut self, p: &Perf, totals: (u32, u32, u32), r: Area) {
        let c = &p.cpu;
        let stats = vec![
            vec![
                stat("Utilization", percent(c.util)),
                stat("Speed", format!("{} GHz", fmt_num(c.speed_mhz as f64 / 1000.0, 2))),
            ],
            vec![
                stat("Processes", totals.0.to_string()),
                stat("Threads", totals.1.to_string()),
                stat("Handles", totals.2.to_string()),
            ],
            vec![stat("Up time", fmt_uptime(p.uptime_s))],
        ];
        let virtualization = match c.virtualization {
            Some(true) => "Enabled",
            Some(false) => "Disabled",
            None => "Not available",
        };
        let facts = vec![
            ("Base speed:", format!("{} GHz", fmt_num(c.base_mhz as f64 / 1000.0, 2))),
            ("Sockets:", c.sockets.to_string()),
            ("Cores:", c.cores.to_string()),
            ("Logical processors:", c.logical.to_string()),
            ("Virtualization:", virtualization.to_string()),
            ("L1 cache:", fmt_bytes(c.l1 as f64)),
            ("L2 cache:", fmt_bytes(c.l2 as f64)),
            ("L3 cache:", fmt_bytes(c.l3 as f64)),
        ];
        let color = self.th.cpu;
        let area = self.detail_frame(r, ("CPU", &c.name), color, 130.0, stats, facts);
        let span = time_span(self.settings.interval_ms);
        let (label, axis) = (self.px(18.0), self.px(20.0));
        let body = Area { x: area.x, y: area.y + label, w: area.w, h: area.h - label - axis };
        self.graph_labels(area.x, area.y, area.w, "% Utilization", "100%");

        let n = c.lp_hist.len();
        if self.settings.cpu_logical && n > 0 && body.h > 0 {
            let cols = core_columns(n, body.w, body.h);
            let rows = n.div_ceil(cols);
            let gap = self.px(4.0);
            // Cell edges from the running total, so rounding never leaves a ragged right side.
            let edge = |i: usize, count: usize, size: i32| i as i32 * (size + gap) / count as i32;
            for (i, history) in c.lp_hist.iter().enumerate() {
                let (col, row) = (i % cols, i / cols);
                let (cx, cy) = (edge(col, cols, body.w), edge(row, rows, body.h));
                let cell = Area {
                    x: body.x + cx,
                    y: body.y + cy,
                    w: edge(col + 1, cols, body.w) - cx - gap,
                    h: edge(row + 1, rows, body.h) - cy - gap,
                };
                self.chart(cell, color, &[Series { values: history, dashed: false }], 100.0);
            }
        } else {
            self.chart(body, color, &[Series { values: &c.hist, dashed: false }], 100.0);
        }
        self.hit(body.x, body.y, body.w, body.h, Hit::PerfGraph);
        self.graph_labels(area.x, body.y + body.h + self.px(2.0), area.w, &span, "0");
    }

    fn mem_detail(&mut self, m: &MemInfo, r: Area) {
        let stats = vec![
            vec![
                stat("In use (compressed)", format!("{} GB ({} GB)", gb(m.in_use), gb(m.compressed))),
                stat("Available", format!("{} GB", gb(m.avail))),
            ],
            vec![
                stat("Committed", format!("{}/{} GB", gb(m.committed), gb(m.commit_limit))),
                stat("Cached", format!("{} GB", gb(m.cached))),
            ],
            vec![
                stat("Paged pool", format!("{} MB", mb(m.paged_pool))),
                stat("Non-paged pool", format!("{} MB", mb(m.nonpaged_pool))),
            ],
        ];
        let speed = if m.speed_mts > 0 { format!("{} MT/s", m.speed_mts) } else { "—".into() };
        let slots = if m.slots_total > 0 { format!("{} of {}", m.slots_used, m.slots_total) } else { "—".into() };
        let facts = vec![
            ("Speed:", speed),
            ("Slots used:", slots),
            ("Form factor:", or_dash(&m.form_factor)),
            ("Hardware reserved:", format!("{} MB", mb(m.hw_reserved))),
        ];
        let color = self.th.mem;
        let installed = format!("{} GB", gb(m.total + m.hw_reserved));
        let area = self.detail_frame(r, ("Memory", &installed), color, 170.0, stats, facts);
        let span = time_span(self.settings.interval_ms);

        let (label, bar) = (self.px(20.0), self.px(26.0));
        let composition = if area.h >= self.px(150.0) { self.px(4.0) + label + bar } else { 0 };
        let top = Area { h: area.h - composition, ..area };
        let series = [Series { values: &m.hist, dashed: false }];
        self.titled_chart(top, "Memory usage", &format!("{} GB", gb(m.total)), Some(&span), color, &series, 100.0);
        if composition == 0 {
            return;
        }
        let ly = top.y + top.h + self.px(4.0);
        self.graph_labels(area.x, ly, area.w, "Memory composition", "");
        let bar_area = Area { x: area.x, y: ly + label, w: area.w, h: bar };
        let in_use = m.total.saturating_sub(m.avail).saturating_sub(m.modified);
        let parts = [
            (in_use, scale_color(color, 0.55)),
            (m.modified, scale_color(color, 0.40)),
            (m.standby, scale_color(color, 0.20)),
            (m.free, self.th.content),
        ];
        let (mut seen, mut left) = (0u64, bar_area.x);
        for (bytes, shade) in parts {
            seen += bytes;
            let share = (seen as f64 / m.total.max(1) as f64).min(1.0);
            let right = bar_area.x + (share * bar_area.w as f64).round() as i32;
            if right > left {
                self.cv.fill(left, bar_area.y, right - left, bar, shade);
                self.cv.vline(left, bar_area.y, bar, color);
                left = right;
            }
        }
        self.frame(bar_area, color);
    }

    fn disk_detail(&mut self, d: &DiskInfo, r: Area) {
        let size = |bytes: u64| if bytes > 0 { fmt_bytes(bytes as f64) } else { "—".to_string() };
        let stats = vec![
            vec![
                stat("Active time", percent(d.active)),
                stat("Average response time", format!("{} ms", fmt_num(d.resp_ms as f64, 1))),
            ],
            vec![
                Stat { label: "Read speed", value: fmt_bytes_rate(d.read_bps as f64), key: Some(false) },
                Stat { label: "Write speed", value: fmt_bytes_rate(d.write_bps as f64), key: Some(true) },
            ],
        ];
        let facts = vec![
            ("Capacity:", size(d.capacity)),
            ("Formatted:", size(d.formatted)),
            ("System disk:", yes_no(d.system)),
            ("Page file:", yes_no(d.pagefile)),
            ("Type:", d.kind.clone()),
        ];
        let color = self.th.disk;
        let title = disk_title(d);
        let area = self.detail_frame(r, (&title, &d.model), color, 190.0, stats, facts);
        let span = time_span(self.settings.interval_ms);

        let active = [Series { values: &d.hist_active, dashed: false }];
        if area.h < self.px(170.0) {
            self.titled_chart(area, "Active time", "100%", Some(&span), color, &active, 100.0);
            return;
        }
        let gap = self.px(6.0);
        let upper = Area { h: (area.h - gap) * 55 / 100, ..area };
        let lower = Area { y: upper.y + upper.h + gap, h: area.h - gap - upper.h, ..area };
        self.titled_chart(upper, "Active time", "100%", Some(&span), color, &active, 100.0);
        let max = axis_max(peak(&d.hist_read, &d.hist_write), 1024.0 * 1024.0, 1024.0);
        let rate = [Series { values: &d.hist_read, dashed: false }, Series { values: &d.hist_write, dashed: true }];
        self.titled_chart(lower, "Disk transfer rate", &fmt_bytes_rate(max as f64), Some(&span), color, &rate, max);
    }

    fn net_detail(&mut self, n: &NetInfo, r: Area) {
        let stats = vec![
            vec![Stat { label: "Send", value: fmt_bits(n.send_bps as f64), key: Some(true) }],
            vec![Stat { label: "Receive", value: fmt_bits(n.recv_bps as f64), key: Some(false) }],
        ];
        let mut facts = vec![("Adapter name:", n.alias.clone())];
        if !n.ssid.is_empty() {
            facts.push(("SSID:", n.ssid.clone()));
        }
        facts.push(("Connection type:", n.conn_type.clone()));
        facts.push(("IPv4 address:", or_dash(&n.ipv4)));
        facts.push(("IPv6 address:", or_dash(&n.ipv6)));
        if let Some(signal) = n.signal {
            facts.push(("Signal strength:", format!("{signal}%")));
        }
        if n.link_bps > 0 {
            facts.push(("Link speed:", fmt_bits(n.link_bps as f64)));
        }
        let color = self.th.net;
        let area = self.detail_frame(r, (&n.title, &n.description), color, 130.0, stats, facts);
        let span = time_span(self.settings.interval_ms);
        let max = axis_max(peak(&n.hist_send, &n.hist_recv), 100_000.0, 1000.0);
        let series = [Series { values: &n.hist_recv, dashed: false }, Series { values: &n.hist_send, dashed: true }];
        self.titled_chart(area, "Throughput", &fmt_bits(max as f64), Some(&span), color, &series, max);
    }

    fn gpu_detail(&mut self, g: &GpuInfo, r: Area) {
        let mut stats = vec![
            vec![
                stat("Utilization", percent(g.util)),
                stat("Dedicated GPU memory", format!("{}/{} GB", gb(g.ded_used), gb(g.ded_total))),
            ],
            vec![
                stat(
                    "GPU memory",
                    format!("{}/{} GB", gb(g.ded_used + g.shared_used), gb(g.ded_total + g.shared_total)),
                ),
                stat("Shared GPU memory", format!("{}/{} GB", gb(g.shared_used), gb(g.shared_total))),
            ],
        ];
        if let Some(t) = g.temp_c {
            stats.push(vec![stat("GPU temperature", format!("{} °C", fmt_num(t.round() as f64, 0)))]);
        }
        let facts = vec![
            ("Driver version:", g.driver_version.clone()),
            ("Driver date:", g.driver_date.clone()),
            ("Physical location:", g.location.clone()),
        ];
        let color = self.th.gpu;
        let title = format!("GPU {}", g.index);
        let area = self.detail_frame(r, (&title, &g.name), color, 250.0, stats, facts);
        let span = time_span(self.settings.interval_ms);

        // Engines two abreast, then the memory graphs; short windows drop rows from the bottom.
        let (axis, row_gap, gutter) = (self.px(20.0), self.px(5.0), self.px(14.0));
        let engines = g.engines.len().min(4);
        let engine_rows = engines.div_ceil(2) as i32;
        let memory = [
            ("Dedicated GPU memory usage", g.ded_total, &g.hist_ded),
            ("Shared GPU memory usage", g.shared_total, &g.hist_shared),
        ];
        let space = area.h - axis;
        let rows = (engine_rows + memory.len() as i32).min((space + row_gap) / (self.px(52.0) + row_gap)).max(1);
        let row_h = (space - row_gap * (rows - 1)) / rows;
        for row in 0..rows {
            let cell = Area { x: area.x, y: area.y + row * (row_h + row_gap), w: area.w, h: row_h };
            if row < engine_rows {
                let half = (cell.w - gutter) / 2;
                for side in 0..2 {
                    let Some(e) = g.engines.get((row * 2 + side) as usize) else { break };
                    let part = Area { x: cell.x + side * (half + gutter), w: half, ..cell };
                    let series = [Series { values: &e.hist, dashed: false }];
                    self.titled_chart(part, &e.name, &percent(e.util), None, color, &series, 100.0);
                }
            } else if let Some(&(label, total, history)) = memory.get((row - engine_rows) as usize) {
                let series = [Series { values: history, dashed: false }];
                self.titled_chart(cell, label, &format!("{} GB", gb(total)), None, color, &series, 100.0);
            }
        }
        self.graph_labels(area.x, area.y + area.h - axis + self.px(2.0), area.w, &span, "0");
    }
}

/// Thumbnail text with one unit for both directions: "S: 40,0 R: 64,0 Kbps".
fn net_short(send: f32, recv: f32) -> String {
    let m = send.max(recv);
    let (div, unit) = if m >= 1e9 {
        (1e9, "Gbps")
    } else if m >= 1e6 {
        (1e6, "Mbps")
    } else {
        (1e3, "Kbps")
    };
    format!("S: {} R: {} {}", fmt_num(send as f64 / div, 1), fmt_num(recv as f64 / div, 1), unit)
}

pub fn on_key(a: &mut App, vk: u16) -> bool {
    let n = a.snap.as_ref().map(|s| devices(&s.perf).len()).unwrap_or(0);
    if n == 0 {
        return false;
    }
    if vk == VK_DOWN {
        a.perf_sel = (a.perf_sel + 1).min(n - 1);
    } else if vk == VK_UP {
        a.perf_sel = a.perf_sel.saturating_sub(1);
    } else {
        return false;
    }
    a.invalidate();
    true
}

pub fn context_menu(a: &mut App) {
    let is_cpu = a.snap.as_ref().map(|s| devices(&s.perf).get(a.perf_sel) == Some(&Dev::Cpu)).unwrap_or(false);
    if !is_cpu {
        return;
    }
    let m = unsafe { CreatePopupMenu() };
    super::procs_menu_item(m, 1, "Overall utilization", true, !a.settings.cpu_logical);
    super::procs_menu_item(m, 2, "Logical processors", true, a.settings.cpu_logical);
    match super::procs_popup(a, m) {
        1 => a.settings.cpu_logical = false,
        2 => a.settings.cpu_logical = true,
        _ => return,
    }
    super::settings::save(a);
    a.invalidate();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_limits_are_round_in_their_display_unit() {
        let mib = 1024.0 * 1024.0;
        assert_eq!(axis_max(22.1 * mib, mib, 1024.0), 50.0 * mib);
        assert_eq!(axis_max(0.0, mib, 1024.0), mib);
        assert_eq!(axis_max(0.0, 100_000.0, 1000.0), 100_000.0);
        assert_eq!(axis_max(1_300_000.0, 100_000.0, 1000.0), 2_000_000.0);
        assert_eq!(axis_max(f32::NAN, 100_000.0, 1000.0), 100_000.0);
    }

    #[test]
    fn time_axis_follows_update_speed() {
        assert_eq!(time_span(500), "30 seconds");
        assert_eq!(time_span(1000), "60 seconds");
        assert_eq!(time_span(4000), "4 minutes");
    }

    #[test]
    fn core_grid_fills_its_rows() {
        assert_eq!(16 % core_columns(16, 600, 330), 0);
        assert_eq!(12 % core_columns(12, 600, 330), 0);
    }
}
