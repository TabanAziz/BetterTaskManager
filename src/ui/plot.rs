//! Time-series plots.
//!
//! Lines are rasterised column by column: each column only looks at the few segments
//! that can reach it and only at the rows they cover, so a plot costs time proportional
//! to the length of its line and needs no per-frame mask. Dashes follow the arc length,
//! which keeps steep spikes connected and readable.

use super::gfx::{Canvas, Color};

const DASH_ON: f32 = 5.0;
const DASH_OFF: f32 = 3.5;
/// A vertical grid line every this many samples; they move along with the data.
const GRID_SAMPLES: u64 = 10;
const GRID_ROWS: i32 = 10;
/// Small plots get fewer rows, otherwise the grid turns into noise.
const MIN_ROW_HEIGHT: i32 = 14;

#[derive(Clone, Copy)]
pub struct Series<'a> {
    /// Oldest first. NaN means "not measured yet" and is left empty.
    pub values: &'a [f32],
    pub dashed: bool,
}

#[derive(Clone, Copy)]
pub struct Style {
    pub line: Color,
    pub fill: Color,
    pub grid: Option<Color>,
    /// Number of the newest sample.
    pub tick: u64,
    pub stroke: f32,
}

#[derive(Clone, Copy)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Draws `series` scaled to `0..=max`. The first one also gets the area underneath.
pub fn plot(cv: &mut Canvas, area: Area, series: &[Series], max: f32, style: &Style) {
    if area.w < 2 || area.h < 3 {
        return;
    }
    cv.flush();
    let to_y = |value: f32| {
        let share = if max > 0.0 { (value / max).clamp(0.0, 1.0) } else { 0.0 };
        // One pixel inset, so 0 % and 100 % are not cut in half by the border.
        1.0 + (1.0 - share) * (area.h - 2) as f32
    };
    if let Some(first) = series.first() {
        fill_below(cv, area, first.values, &to_y, style.fill);
    }
    if let Some(grid) = style.grid {
        draw_grid(cv, area, series.first().map_or(0, |s| s.values.len()), style.tick, grid);
    }
    for s in series {
        stroke(cv, area, s, &to_y, style);
    }
}

fn spacing(area: Area, samples: usize) -> f32 {
    area.w as f32 / (samples - 1) as f32
}

fn fill_below(cv: &mut Canvas, area: Area, values: &[f32], to_y: &impl Fn(f32) -> f32, color: Color) {
    let n = values.len();
    if n < 2 {
        return;
    }
    let step = spacing(area, n);
    for column in 0..area.w {
        let position = (column as f32 + 0.5) / step;
        let i = (position as usize).min(n - 2);
        let (a, b) = (values[i], values[i + 1]);
        if !a.is_finite() || !b.is_finite() {
            continue;
        }
        let (ya, yb) = (to_y(a), to_y(b));
        let top = ya + (yb - ya) * (position - i as f32).clamp(0.0, 1.0);
        let row = (top as i32).min(area.h - 1);
        cv.blend_px(area.x + column, area.y + row, color, ((row + 1) as f32 - top).clamp(0.0, 1.0));
        cv.column(area.x + column, area.y + row + 1, area.y + area.h, color);
    }
}

fn draw_grid(cv: &mut Canvas, area: Area, samples: usize, tick: u64, color: Color) {
    const STRENGTH: f32 = 0.17;
    let rows = [GRID_ROWS, 5, 4, 2].into_iter().find(|&n| area.h / n >= MIN_ROW_HEIGHT).unwrap_or(2);
    for k in 1..rows {
        let y = area.y + area.h * k / rows;
        for x in area.x..area.x + area.w {
            cv.blend_px(x, y, color, STRENGTH);
        }
    }
    if samples < 2 {
        return;
    }
    let step = spacing(area, samples);
    for i in 1..samples - 1 {
        if (tick + i as u64).is_multiple_of(GRID_SAMPLES) {
            let x = area.x + (i as f32 * step) as i32;
            for y in area.y..area.y + area.h {
                cv.blend_px(x, y, color, STRENGTH);
            }
        }
    }
}

struct Segment {
    x0: f32,
    y0: f32,
    dx: f32,
    dy: f32,
    /// Arc length of the line before this segment.
    before: f32,
    length: f32,
}

impl Segment {
    /// Rows covered between two x positions, widened by `reach`.
    fn rows(&self, from: f32, to: f32, reach: f32) -> (f32, f32) {
        let at = |x: f32| self.y0 + self.dy * ((x - self.x0) / self.dx).clamp(0.0, 1.0);
        let (a, b) = (at(from), at(to));
        (a.min(b) - reach, a.max(b) + reach)
    }

    fn coverage(&self, px: f32, py: f32, half: f32, dashed: bool) -> f32 {
        let along = ((px - self.x0) * self.dx + (py - self.y0) * self.dy) / (self.length * self.length);
        let t = along.clamp(0.0, 1.0);
        let (ex, ey) = (self.x0 + t * self.dx - px, self.y0 + t * self.dy - py);
        let line = (half + 0.5 - (ex * ex + ey * ey).sqrt()).clamp(0.0, 1.0);
        if !dashed || line == 0.0 {
            return line;
        }
        let phase = (self.before + t * self.length) % (DASH_ON + DASH_OFF);
        line * (phase.min(DASH_ON - phase) + 0.5).clamp(0.0, 1.0)
    }
}

fn stroke(cv: &mut Canvas, area: Area, series: &Series, to_y: &impl Fn(f32) -> f32, style: &Style) {
    let n = series.values.len();
    if n < 2 {
        return;
    }
    let step = spacing(area, n);
    let mut travelled = 0.0;
    let segments: Vec<Option<Segment>> = series
        .values
        .windows(2)
        .enumerate()
        .map(|(i, pair)| {
            if !pair[0].is_finite() || !pair[1].is_finite() {
                return None;
            }
            let (y0, dy) = (to_y(pair[0]), to_y(pair[1]) - to_y(pair[0]));
            let length = (step * step + dy * dy).sqrt();
            let segment = Segment { x0: i as f32 * step, y0, dx: step, dy, before: travelled, length };
            travelled += length;
            Some(segment)
        })
        .collect();

    let half = style.stroke / 2.0;
    let reach = half + 0.5;
    for column in 0..area.w {
        let cx = column as f32 + 0.5;
        let (from, to) = (cx - reach, cx + reach);
        let first = ((from / step).max(0.0) as usize).min(segments.len() - 1);
        let last = ((to / step).max(0.0) as usize).min(segments.len() - 1);
        let near = &segments[first..=last];

        let (mut top, mut bottom) = (f32::MAX, f32::MIN);
        for s in near.iter().flatten() {
            let (a, b) = s.rows(from, to, reach);
            top = top.min(a);
            bottom = bottom.max(b);
        }
        if top > bottom {
            continue;
        }
        for row in (top.floor().max(0.0) as i32)..(bottom.ceil().min(area.h as f32) as i32) {
            let cy = row as f32 + 0.5;
            let mut covered: f32 = 0.0;
            for s in near.iter().flatten() {
                covered = covered.max(s.coverage(cx, cy, half, series.dashed));
            }
            if covered > 0.0 {
                cv.blend_px(area.x + column, area.y + row, style.line, covered);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: i32 = 300;
    const H: i32 = 300;

    fn draw(values: &[f32], dashed: bool) -> Vec<bool> {
        let mut cv = Canvas::new();
        cv.ensure(W, H);
        cv.fill(0, 0, W, H, 0);
        let style = Style { line: 0xFFFFFF, fill: 0, grid: None, tick: 0, stroke: 1.5 };
        plot(&mut cv, Area { x: 0, y: 0, w: W, h: H }, &[Series { values, dashed }], 1.0, &style);
        let pixels = unsafe { std::slice::from_raw_parts(cv.px, (W * H) as usize) };
        pixels.iter().map(|p| p & 0xFF > 60).collect()
    }

    fn lit_rows(lit: &[bool]) -> usize {
        (10..H - 10).filter(|&y| (0..W).any(|x| lit[(y * W + x) as usize])).count()
    }

    fn spike() -> Vec<f32> {
        (0..60).map(|i| if i == 30 { 1.0 } else { 0.0 }).collect()
    }

    #[test]
    fn steep_spike_has_no_holes() {
        assert_eq!(lit_rows(&draw(&spike(), false)), (H - 20) as usize);
    }

    #[test]
    fn steep_dashed_spike_stays_a_dashed_line() {
        let lit = draw(&spike(), true);
        // Both flanks are dashed, so far more than half of all rows carry a dash.
        assert!(lit_rows(&lit) > (H - 20) as usize / 2);
        assert!(lit.iter().filter(|&&p| p).count() < draw(&spike(), false).iter().filter(|&&p| p).count());
    }

    #[test]
    fn limits_stay_inside_and_gaps_stay_empty() {
        assert!(draw(&[1.0; 60], false)[(W + 150) as usize]);
        assert!(draw(&[0.0; 60], false)[((H - 2) * W + 150) as usize]);
        assert!(!draw(&[f32::NAN; 60], false).contains(&true));
    }
}
