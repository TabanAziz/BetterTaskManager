use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{BitBlt, SRCCOPY};

use super::super::gfx::{Canvas, DT_C, DT_L};
use super::super::theme::{Theme, ThemePreference};
use super::super::{theme_visuals, App, Fonts, Hit};

const TILE_HEIGHT: f32 = 63.0;

fn columns(width: i32, px: &impl Fn(f32) -> i32) -> usize {
    if width < px(335.0) {
        2
    } else {
        3
    }
}

pub(super) fn height(width: i32, px: impl Fn(f32) -> i32) -> i32 {
    let rows = ThemePreference::ALL.len().div_ceil(columns(width, &px)) as i32;
    px(26.0) + rows * px(TILE_HEIGHT) + (rows - 1) * px(6.0)
}

#[derive(Default)]
pub(super) struct Gallery {
    atlas: Option<Atlas>,
}

struct Atlas {
    canvas: Canvas,
    width: i32,
    dpi: u32,
}

impl Gallery {
    pub(super) fn invalidate(&mut self) {
        self.atlas = None;
    }
}

impl App {
    pub(super) fn draw_theme_gallery(&mut self, x: i32, y: i32, width: i32, viewport: RECT) {
        let px = |value| self.px(value);
        if y >= viewport.bottom || y + height(width, px) <= viewport.top {
            return;
        }
        let cols = columns(width, &px);
        let heading = px(26.0);
        let gap = px(6.0);
        let w = (width - (cols - 1) as i32 * gap) / cols as i32;
        let h = px(TILE_HEIGHT);
        if w <= px(12.0) {
            return;
        }

        self.cv.text(self.f.body_semi, "Theme", (x, y, width, heading), self.th.text, DT_L);
        let gallery = &mut self.settings_page.gallery;
        if gallery.atlas.as_ref().is_none_or(|atlas| atlas.width != w || atlas.dpi != self.dpi) {
            gallery.atlas = Some(Atlas::new(w, h, self.dpi, &self.f, &self.th));
        }
        let atlas = gallery.atlas.as_ref().expect("theme atlas has just been built");
        for (i, theme) in ThemePreference::ALL.into_iter().enumerate() {
            let tx = x + (i % cols) as i32 * (w + gap);
            let ty = y + heading + (i / cols) as i32 * (h + gap);
            if ty >= viewport.bottom || ty + h <= viewport.top {
                continue;
            }
            let state = if self.settings.theme == theme { 2 } else { i32::from(self.hover == Hit::Theme(theme)) };
            unsafe { BitBlt(self.cv.hdc, tx, ty, w, h, atlas.canvas.hdc, state * w, i as i32 * h, SRCCOPY) };
            self.hits.push((RECT { left: tx, top: ty, right: tx + w, bottom: ty + h }, Hit::Theme(theme)));
        }
    }
}

impl Atlas {
    fn new(width: i32, h: i32, dpi: u32, fonts: &Fonts, active: &Theme) -> Self {
        let px = |v: f32| (v * dpi as f32 / 96.0).round() as i32;
        let mut canvas = Canvas::new();
        canvas.ensure(width * 3, h * ThemePreference::ALL.len() as i32);
        canvas.fill(0, 0, canvas.w, canvas.h, active.content);
        for (row, (theme, colors)) in ThemePreference::ALL.into_iter().zip(Theme::previews()).enumerate() {
            let y = row as i32 * h;
            let card = colors.color_for(0x232323, 0xFFFFFF, colors.search_bg);
            let padding = px(6.0);
            let art_h = h - px(26.0);
            for (state, border) in [colors.divider, colors.text2, colors.accent].into_iter().enumerate() {
                let x = state as i32 * width;
                let radius = px(6.0);
                let stroke = if state == 2 { px(2.0) } else { px(1.0) };
                canvas.round_rect(x as f32, y as f32, width as f32, h as f32, radius as f32, border, 1.0);
                canvas.round_rect(
                    (x + stroke) as f32,
                    (y + stroke) as f32,
                    (width - 2 * stroke) as f32,
                    (h - 2 * stroke) as f32,
                    (radius - stroke) as f32,
                    card,
                    1.0,
                );
                let preview =
                    RECT { left: x + padding, top: y + padding, right: x + width - padding, bottom: y + art_h };
                theme_visuals::paint_preview(&mut canvas, preview, &colors, dpi as f32 / 96.0);
                let caption_y = y + art_h + px(2.0);
                let caption_h = h - art_h - px(5.0);
                let check_w = if state == 2 { px(16.0) } else { 0 };
                canvas.text(
                    fonts.body,
                    theme.label(),
                    (x + padding, caption_y, width - 2 * padding - check_w, caption_h),
                    colors.text,
                    DT_L,
                );
                if state == 2 {
                    canvas.text(
                        fonts.icon_small,
                        "\u{E73E}",
                        (x + width - padding - check_w, caption_y, check_w, caption_h),
                        colors.text,
                        DT_C,
                    );
                }
            }
        }
        canvas.set_alpha_opaque(0, 0, canvas.w, canvas.h);
        Self { canvas, width, dpi }
    }
}
