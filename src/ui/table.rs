//! The table used by App history, Startup apps, Users and Services: sortable, resizable
//! columns, one level of expandable children, optional heat colours and header totals.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse as vk;

use super::gfx::*;
use super::plot::Area;
use super::theme::RowTint;
use super::{Fonts, Hit, Theme, G_CHEV_D, G_CHEV_R, G_CHEV_U};
use crate::meta::Meta;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Header(usize),
    Divider(usize),
    Row(usize),
    Chevron(usize),
    Scroll,
}

#[derive(Clone, Copy)]
pub enum Drag {
    Column { col: usize, start_x: i32, start_w: f32 },
    Scroll { start_y: i32, start_scroll: f32 },
}

pub struct Col {
    pub title: &'static str,
    /// Device independent pixels.
    pub width: f32,
    pub right: bool,
    pub heat: bool,
}

pub fn col(title: &'static str, width: f32) -> Col {
    Col { title, width, right: false, heat: false }
}

pub fn number(title: &'static str, width: f32) -> Col {
    Col { title, width, right: true, heat: false }
}

pub fn heated(title: &'static str, width: f32) -> Col {
    Col { title, width, right: true, heat: true }
}

pub enum Key {
    Number(f64),
    Text(String),
}

pub struct Cell {
    pub text: String,
    key: Key,
    heat: usize,
}

impl Cell {
    pub fn text(text: impl Into<String>) -> Cell {
        let text = text.into();
        Cell { key: Key::Text(text.to_lowercase()), text, heat: 0 }
    }

    pub fn number(text: String, value: f64, heat: usize) -> Cell {
        Cell { text, key: Key::Number(value), heat }
    }

    fn compare(&self, other: &Cell) -> Ordering {
        match (&self.key, &other.key) {
            (Key::Number(a), Key::Number(b)) => a.total_cmp(b),
            (Key::Text(a), Key::Text(b)) => a.cmp(b),
            (Key::Number(_), Key::Text(_)) => Ordering::Less,
            (Key::Text(_), Key::Number(_)) => Ordering::Greater,
        }
    }
}

pub enum Icon {
    /// Icons are loaded in the background, so they are looked up while drawing.
    File(Option<Arc<Meta>>),
    Glyph(&'static str),
}

pub struct Row {
    pub id: String,
    pub icon: Icon,
    pub cells: Vec<Cell>,
    pub children: Vec<Row>,
}

pub struct Table {
    pub cols: Vec<Col>,
    sort: usize,
    desc: bool,
    scroll: f32,
    pub selected: Option<String>,
    pub expanded: HashSet<String>,
    rows: Vec<Row>,
    /// What is on screen: (row, child).
    flat: Vec<(usize, Option<usize>)>,
    body: (i32, i32),
    row_h: i32,
}

/// The parts of the app a table needs to draw itself.
pub struct Surface<'a> {
    pub cv: &'a mut Canvas,
    pub th: &'a Theme,
    pub f: &'a Fonts,
    pub hits: &'a mut Vec<(RECT, Hit)>,
    pub hover: Hit,
    pub scale: f32,
}

impl Surface<'_> {
    fn px(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    fn hit(&mut self, r: Area, part: Part) {
        let rect = RECT { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h };
        self.hits.push((rect, Hit::List(part)));
    }
}

impl Table {
    pub fn new(cols: Vec<Col>, sort: usize, desc: bool) -> Table {
        Table {
            cols,
            sort,
            desc,
            scroll: 0.0,
            selected: None,
            expanded: HashSet::new(),
            rows: Vec::new(),
            flat: Vec::new(),
            body: (0, 0),
            row_h: 28,
        }
    }

    pub fn set_rows(&mut self, rows: Vec<Row>) {
        self.rows = rows;
        self.arrange();
    }

    fn arrange(&mut self) {
        let (col, desc) = (self.sort, self.desc);
        let order = |a: &Row, b: &Row| {
            let by_cell = match (a.cells.get(col), b.cells.get(col)) {
                (Some(x), Some(y)) => x.compare(y),
                _ => Ordering::Equal,
            };
            let by_cell = if desc { by_cell.reverse() } else { by_cell };
            // A stable order for equal values, so rows do not jump on every refresh.
            by_cell.then_with(|| a.id.cmp(&b.id))
        };
        self.rows.sort_by(order);
        self.flat.clear();
        for (i, row) in self.rows.iter_mut().enumerate() {
            self.flat.push((i, None));
            if !row.children.is_empty() && self.expanded.contains(&row.id) {
                row.children.sort_by(order);
                self.flat.extend((0..row.children.len()).map(|child| (i, Some(child))));
            }
        }
    }

    pub fn row(&self, index: usize) -> Option<&Row> {
        let &(top, child) = self.flat.get(index)?;
        match child {
            None => self.rows.get(top),
            Some(child) => self.rows.get(top)?.children.get(child),
        }
    }

    fn selected_index(&self) -> Option<usize> {
        let id = self.selected.as_ref()?;
        (0..self.flat.len()).find(|&i| self.row(i).is_some_and(|r| &r.id == id))
    }

    pub fn select(&mut self, index: usize) {
        if let Some(id) = self.row(index).map(|r| r.id.clone()) {
            self.selected = Some(id);
        }
    }

    pub fn toggle(&mut self, index: usize) {
        let Some(id) = self.row(index).filter(|r| !r.children.is_empty()).map(|r| r.id.clone()) else { return };
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.arrange();
    }

    pub fn sort_by(&mut self, col: usize) {
        if self.sort == col {
            self.desc = !self.desc;
        } else if let Some(c) = self.cols.get(col) {
            self.sort = col;
            // Numbers start with the largest, text with A.
            self.desc = c.right;
        }
        self.arrange();
    }

    pub fn reset_scroll(&mut self) {
        self.scroll = 0.0;
    }

    fn clamp(&mut self) {
        let room = (self.flat.len() as i32 * self.row_h - self.body.1).max(0) as f32;
        self.scroll = self.scroll.clamp(0.0, room);
    }

    pub fn wheel(&mut self, delta: i32) {
        self.scroll -= delta as f32 / 120.0 * 3.0 * self.row_h as f32;
        self.clamp();
    }

    fn thumb(&self) -> Option<(f32, f32, f32)> {
        let content = (self.flat.len() as i32 * self.row_h) as f32;
        let (top, height) = (self.body.0 as f32, self.body.1 as f32);
        if content <= height || height <= 0.0 {
            return None;
        }
        let size = (height / content * height).max(24.0).min(height);
        let per_pixel = (content - height) / (height - size).max(1.0);
        Some((top + self.scroll / per_pixel, size, per_pixel))
    }

    /// A press on the track centres the thumb there.
    pub fn scroll_press(&mut self, y: i32) -> Drag {
        if let Some((top, size, per_pixel)) = self.thumb() {
            let y = y as f32;
            if y < top || y > top + size {
                self.scroll = (y - self.body.0 as f32 - size / 2.0) * per_pixel;
                self.clamp();
            }
        }
        Drag::Scroll { start_y: y, start_scroll: self.scroll }
    }

    pub fn drag(&mut self, drag: Drag, x: i32, y: i32, scale: f32) {
        match drag {
            Drag::Column { col, start_x, start_w } => {
                if let Some(c) = self.cols.get_mut(col) {
                    c.width = (start_w + (x - start_x) as f32 / scale).max(40.0);
                }
            }
            Drag::Scroll { start_y, start_scroll } => {
                if let Some((_, _, per_pixel)) = self.thumb() {
                    self.scroll = start_scroll + (y - start_y) as f32 * per_pixel;
                    self.clamp();
                }
            }
        }
    }

    pub fn key(&mut self, key: u16) -> bool {
        let n = self.flat.len();
        if n == 0 {
            return false;
        }
        let page = (self.body.1 / self.row_h.max(1)).max(1) as usize;
        let at = self.selected_index();
        let target = match key {
            vk::VK_DOWN => at.map_or(0, |i| (i + 1).min(n - 1)),
            vk::VK_UP => at.map_or(0, |i| i.saturating_sub(1)),
            vk::VK_NEXT => at.map_or(0, |i| (i + page).min(n - 1)),
            vk::VK_PRIOR => at.map_or(0, |i| i.saturating_sub(page)),
            vk::VK_HOME => 0,
            vk::VK_END => n - 1,
            vk::VK_RIGHT | vk::VK_LEFT => {
                let Some(i) = at else { return false };
                let open = self.row(i).is_some_and(|r| self.expanded.contains(&r.id));
                if open != (key == vk::VK_RIGHT) {
                    self.toggle(i);
                }
                return true;
            }
            _ => return false,
        };
        self.select(target);
        let top = (target as i32 * self.row_h) as f32;
        self.scroll = self.scroll.min(top).max(top + (self.row_h - self.body.1) as f32);
        self.clamp();
        true
    }

    /// "sort,desc,width,width,..."
    pub fn layout(&self) -> String {
        let mut text = format!("{},{}", self.sort, self.desc as u8);
        for c in &self.cols {
            text.push_str(&format!(",{}", c.width.round() as i32));
        }
        text
    }

    pub fn restore(&mut self, layout: &str) {
        let Ok(values) = layout.split(',').map(|v| v.trim().parse::<i32>()).collect::<Result<Vec<_>, _>>() else {
            return;
        };
        if values.len() != self.cols.len() + 2 {
            return; // saved by a version with other columns
        }
        if (0..self.cols.len() as i32).contains(&values[0]) {
            self.sort = values[0] as usize;
            self.desc = values[1] != 0;
        }
        for (c, &width) in self.cols.iter_mut().zip(&values[2..]) {
            c.width = width.clamp(40, 1200) as f32;
        }
    }

    fn widths(&self, s: &Surface, room: i32) -> Vec<i32> {
        let mut widths: Vec<i32> = self.cols.iter().map(|c| s.px(c.width)).collect();
        let mut over = widths.iter().sum::<i32>() - room;
        // Too narrow: text columns give way from the right, the name column last.
        for i in (0..widths.len()).rev() {
            if over <= 0 {
                break;
            }
            if !self.cols[i].right {
                let least = if i == 0 { s.px(120.0) } else { s.px(64.0) };
                let give = over.min((widths[i] - least).max(0));
                widths[i] -= give;
                over -= give;
            }
        }
        widths
    }

    /// `totals` fills the big percentages above heat columns; `empty` shows when there are no rows.
    pub fn draw(&mut self, s: &mut Surface, area: Area, totals: &[Option<f32>], empty: &str) {
        let tall = !totals.is_empty();
        let header = s.px(if tall { 52.0 } else { 36.0 });
        self.row_h = s.px(28.0);
        self.body = (area.y + header + 1, (area.h - header - 1).max(0));
        self.clamp();
        let widths = self.widths(s, area.w - s.px(12.0));
        self.draw_header(s, area, header, &widths, totals);
        self.draw_rows(s, area, &widths);
        if self.flat.is_empty() {
            let line = (area.x, self.body.0 + s.px(20.0), area.w, s.px(24.0));
            s.cv.text(s.f.body, empty, line, s.th.text2, DT_C);
        }
        self.draw_scrollbar(s, area);
    }

    fn draw_header(&self, s: &mut Surface, area: Area, h: i32, widths: &[i32], totals: &[Option<f32>]) {
        let (th, f) = (s.th, s.f);
        let pad = s.px(8.0);
        let tall = !totals.is_empty();
        s.cv.hline(area.x, area.y, area.w, th.divider);
        let mut x = area.x;
        for (i, (c, &w)) in self.cols.iter().zip(widths).enumerate() {
            let cell = Area { x, y: area.y + 1, w, h: h - 1 };
            let total = totals.get(i).copied().flatten();
            if c.heat && total.is_some_and(|v| v >= 75.0) {
                s.cv.fill(cell.x, cell.y, cell.w, cell.h, th.header_hot);
            }
            if s.hover == Hit::List(Part::Header(i)) {
                s.cv.blend(cell.x, cell.y, cell.w, cell.h, th.overlay, 0.04);
            }
            let align = if c.right { DT_R } else { DT_L };
            let left = if i == 0 { s.px(16.0) } else { pad };
            let (ty, th_) = if tall { (area.y + s.px(30.0), s.px(18.0)) } else { (area.y, h) };
            if let Some(v) = total {
                let text = format!("{}%", crate::util::fmt_num(v.round() as f64, 0));
                s.cv.text(f.hdr_val, &text, (x, area.y + s.px(6.0), w - pad, s.px(26.0)), th.text, DT_R);
            }
            s.cv.text(f.small, c.title, (x + left, ty, w - left - pad, th_), th.text, align);
            if i == self.sort {
                let glyph = if self.desc { G_CHEV_D } else { G_CHEV_U };
                s.cv.text(f.icon_tiny, glyph, (x, area.y + s.px(1.0), w, s.px(10.0)), th.text2, DT_C);
            }
            s.cv.vline(x + w - 1, area.y + 1, h - 1, th.divider);
            s.hit(Area { x, y: area.y, w, h }, Part::Header(i));
            let grip = s.px(4.0);
            s.hit(Area { x: x + w - grip, y: area.y, w: grip * 2, h }, Part::Divider(i));
            x += w;
        }
        s.cv.hline(area.x, area.y + h, area.w, th.divider);
    }

    fn draw_rows(&self, s: &mut Surface, area: Area, widths: &[i32]) {
        let (th, f) = (s.th, s.f);
        let (top, height) = self.body;
        let rh = self.row_h;
        let bottom = top + height;
        let (pad, icon_size) = (s.px(8.0), s.px(16.0));
        let fallback_icon = crate::meta::default_icon_if_loaded();
        let has_tree = self.rows.iter().any(|r| !r.children.is_empty());
        let first = (self.scroll / rh as f32) as usize;
        let mut y = top - (self.scroll - (first as i32 * rh) as f32).round() as i32;

        s.cv.clip_rect(Some((area.x, top, area.w, height)));
        for index in first..self.flat.len() {
            if y >= bottom {
                break;
            }
            let Some(row) = self.row(index) else { break };
            let child = self.flat[index].1.is_some();
            let (clip_top, clip_bottom) = (y.max(top), (y + rh).min(bottom));
            let tint = if self.selected.as_deref() == Some(row.id.as_str()) {
                RowTint::Selected
            } else if matches!(s.hover, Hit::List(Part::Row(i) | Part::Chevron(i)) if i == index) {
                RowTint::Hover
            } else {
                RowTint::Normal
            };
            let overlay = tint.strength(th.dark);

            let mut x = area.x;
            for (i, ((c, &w), cell)) in self.cols.iter().zip(widths).zip(&row.cells).enumerate() {
                let ink = if c.heat {
                    let (back, ink) = th.heat_cell(cell.heat.min(5), tint);
                    s.cv.fill(x, clip_top, w, clip_bottom - clip_top, back);
                    let edge = th.color_for(0x484848, 0x484848, th.divider);
                    s.cv.vline(x, clip_top, clip_bottom - clip_top, mix(back, edge, 0.4));
                    ink
                } else {
                    if overlay > 0.0 {
                        s.cv.blend(x, clip_top, w, clip_bottom - clip_top, th.overlay, overlay);
                    }
                    th.text
                };
                let mut text_x = x + pad;
                if i == 0 {
                    let indent = match (has_tree, child) {
                        (false, _) => s.px(16.0),
                        (true, false) => s.px(38.0),
                        (true, true) => s.px(60.0),
                    };
                    let ix = x + indent;
                    match &row.icon {
                        // GDI does not clip icons against our pixel fills, only whole rows get one.
                        Icon::File(info) => {
                            let icon = info.as_ref().map(|m| m.icon()).filter(|&h| h != 0).unwrap_or(fallback_icon);
                            s.cv.icon(icon, ix, y + (rh - icon_size) / 2, icon_size);
                        }
                        Icon::Glyph(glyph) => {
                            s.cv.text(f.icon, glyph, (ix, y, icon_size, rh), th.text2, DT_C);
                        }
                    }
                    text_x = ix + icon_size + s.px(6.0);
                }
                let align = if c.right { DT_R } else { DT_L };
                s.cv.text(f.body, &cell.text, (text_x, y, x + w - pad - text_x, rh), ink, align);
                x += w;
            }
            let visible = Area { x: area.x, y: clip_top, w: area.w - s.px(12.0), h: clip_bottom - clip_top };
            if overlay > 0.0 {
                s.cv.blend(x, clip_top, area.x + area.w - x, visible.h, th.overlay, overlay);
            }
            s.hit(visible, Part::Row(index));
            if !row.children.is_empty() {
                let open = self.expanded.contains(&row.id);
                let glyph = if open { G_CHEV_D } else { G_CHEV_R };
                s.cv.text(f.icon_small, glyph, (area.x + s.px(16.0), y, s.px(14.0), rh), th.text, DT_C);
                s.hit(Area { x: area.x + s.px(8.0), w: s.px(30.0), ..visible }, Part::Chevron(index));
            }
            y += rh;
        }
        s.cv.clip_rect(Some((area.x, area.y, area.w, area.h)));
    }

    fn draw_scrollbar(&self, s: &mut Surface, area: Area) {
        let Some((top, size, _)) = self.thumb() else { return };
        let hot = s.hover == Hit::List(Part::Scroll);
        let width = s.px(if hot { 6.0 } else { 2.0 });
        let x = (area.x + area.w - s.px(4.0) - width) as f32;
        let color = s.th.color_for(0x959595, 0x8A8A8A, s.th.text2);
        s.cv.round_rect(x, top, width as f32, size, width as f32 / 2.0, color, 1.0);
        let track = s.px(12.0);
        s.hit(Area { x: area.x + area.w - track, y: self.body.0, w: track, h: self.body.1 }, Part::Scroll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, value: f64) -> Row {
        let cells = vec![Cell::text(id), Cell::number(value.to_string(), value, 0)];
        Row { id: id.into(), icon: Icon::Glyph(""), cells, children: Vec::new() }
    }

    #[test]
    fn sorting_expanding_and_layout() {
        let mut t = Table::new(vec![col("Name", 200.0), number("CPU", 80.0)], 0, false);
        let mut b = row("b", 5.0);
        b.children = vec![row("x", 1.0), row("y", 4.0)];
        t.set_rows(vec![b, row("a", 1.0), row("c", 9.0)]);
        assert_eq!(t.row(0).unwrap().id, "a");
        t.sort_by(1); // numbers start descending
        assert_eq!(t.row(0).unwrap().id, "c");
        t.toggle(1);
        assert_eq!(t.row(2).unwrap().id, "y");
        let saved = t.layout();
        let mut restored = Table::new(vec![col("Name", 10.0), number("CPU", 10.0)], 0, false);
        restored.restore(&saved);
        assert_eq!(restored.layout(), saved);
    }
}
