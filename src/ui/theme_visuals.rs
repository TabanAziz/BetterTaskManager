// Static BetterExplorer motifs in the free space beside the title-bar search box.
use windows_sys::Win32::{
    Foundation::{POINT, RECT},
    Graphics::Gdi::{
        Ellipse, GetStockObject, IntersectClipRect, Polygon, Polyline, RestoreDC, SaveDC, SelectObject,
        SetDCBrushColor, SetDCPenColor, DC_BRUSH, DC_PEN, HDC, NULL_BRUSH, NULL_PEN,
    },
};

use super::gfx::{cr, mix};
use super::theme::{Theme, ThemePreference};

pub fn paint_motif(dc: HDC, bounds: RECT, colors: &Theme, scale: f32) {
    use ThemePreference::*;

    let Some(theme) = colors.custom else { return };
    let width = bounds.right - bounds.left;
    let height = (bounds.bottom - bounds.top).min((32.0 * scale).round() as i32);
    if width < (32.0 * scale).round() as i32 || height < 3 {
        return;
    }
    let saved = unsafe { SaveDC(dc) };
    if saved == 0 {
        return;
    }
    unsafe {
        IntersectClipRect(dc, bounds.left, bounds.top, bounds.right, bounds.bottom);
    }
    // Keep the focal point at the right as the available title space gets narrower.
    let art = Artwork {
        dc,
        left: bounds.right - height * 144 / 32,
        top: bounds.top + (bounds.bottom - bounds.top - height) / 2,
        height,
    };
    match theme {
        DeepSpace => deep_space(&art, colors),
        PaperWhite => paper_white(&art, colors),
        CandyRush => candy_rush(&art, colors),
        Frost => frost(&art, colors),
        Eclipse => eclipse(&art, colors),
        CyberGrid => cyber_grid(&art, colors),
        System | Light | Dark => {}
    }
    unsafe {
        RestoreDC(dc, saved);
    }
}

struct Artwork {
    dc: HDC,
    left: i32,
    top: i32,
    height: i32,
}

impl Artwork {
    fn point(&self, (x, y): (i32, i32)) -> POINT {
        POINT { x: self.left + x * self.height / 32, y: self.top + y * self.height / 32 }
    }

    fn line<const N: usize>(&self, color: u32, points: [(i32, i32); N]) {
        let points = points.map(|point| self.point(point));
        unsafe {
            SelectObject(self.dc, GetStockObject(DC_PEN));
            SetDCPenColor(self.dc, cr(color));
            Polyline(self.dc, points.as_ptr(), N as i32);
        }
    }

    fn polygon<const N: usize>(&self, color: u32, points: [(i32, i32); N]) {
        let points = points.map(|point| self.point(point));
        unsafe {
            SelectObject(self.dc, GetStockObject(NULL_PEN));
            SelectObject(self.dc, GetStockObject(DC_BRUSH));
            SetDCBrushColor(self.dc, cr(color));
            Polygon(self.dc, points.as_ptr(), N as i32);
        }
    }

    fn oval(&self, color: u32, from: (i32, i32), to: (i32, i32)) {
        let from = self.point(from);
        let to = self.point(to);
        unsafe {
            SelectObject(self.dc, GetStockObject(NULL_PEN));
            SelectObject(self.dc, GetStockObject(DC_BRUSH));
            SetDCBrushColor(self.dc, cr(color));
            Ellipse(self.dc, from.x, from.y, to.x, to.y);
        }
    }

    fn ring(&self, color: u32, from: (i32, i32), to: (i32, i32)) {
        let from = self.point(from);
        let to = self.point(to);
        unsafe {
            SelectObject(self.dc, GetStockObject(NULL_BRUSH));
            SelectObject(self.dc, GetStockObject(DC_PEN));
            SetDCPenColor(self.dc, cr(color));
            Ellipse(self.dc, from.x, from.y, to.x, to.y);
        }
    }

    fn sparkle(&self, color: u32, x: i32, y: i32, size: i32) {
        self.line(color, [(x - size, y), (x + size + 1, y)]);
        self.line(color, [(x, y - size), (x, y + size + 1)]);
    }
}

fn deep_space(art: &Artwork, colors: &Theme) {
    let violet = |percent: u8| mix(colors.title_bg, colors.accent, f32::from(percent) / 100.0);
    art.sparkle(violet(28), 35, 20, 2);
    art.sparkle(violet(53), 72, 15, 3);
    art.sparkle(violet(37), 91, 6, 2);
    art.sparkle(violet(72), 140, 24, 3);
    art.oval(violet(42), (53, 7), (55, 9));
    art.oval(violet(52), (83, 24), (85, 26));
    art.line(violet(43), [(94, 22), (102, 15), (117, 8), (132, 5), (140, 8), (139, 12)]);
    art.oval(violet(32), (107, 5), (130, 28));
    art.oval(violet(49), (111, 7), (128, 23));
    art.line(violet(80), [(94, 22), (93, 26), (102, 28), (114, 26), (129, 20), (139, 12)]);
}

fn paper_white(art: &Artwork, colors: &Theme) {
    let ink = |percent: u8| mix(colors.title_bg, colors.accent, f32::from(percent) / 100.0);
    for (y, start) in [(7, 57), (13, 38), (19, 53), (25, 28)] {
        art.line(ink(25), [(start, y), (103, y)]);
    }
    art.line(ink(38), [(98, 5), (98, 28)]);
    let sheet = [(109, 4), (128, 4), (136, 12), (136, 28), (109, 28)];
    art.polygon(colors.content, sheet);
    art.line(ink(67), [(109, 4), (128, 4), (136, 12), (136, 28), (109, 28), (109, 4)]);
    art.line(ink(55), [(128, 4), (128, 12), (136, 12)]);
    art.line(ink(80), [(114, 16), (129, 16)]);
    art.line(ink(47), [(114, 20), (131, 20)]);
    art.line(ink(47), [(114, 24), (125, 24)]);
}

fn candy_rush(art: &Artwork, colors: &Theme) {
    let berry = |percent: u8| mix(colors.title_bg, colors.accent, f32::from(percent) / 100.0);
    art.oval(berry(24), (71, 17), (79, 25));
    art.oval(berry(39), (82, 6), (87, 11));
    art.oval(berry(22), (51, 12), (55, 16));
    art.sparkle(berry(58), 96, 7, 2);
    art.polygon(berry(39), [(108, 13), (98, 9), (98, 22), (106, 23), (112, 19)]);
    art.polygon(berry(39), [(129, 13), (139, 9), (140, 22), (132, 24), (127, 19)]);
    art.line(berry(74), [(100, 13), (107, 17), (100, 19)]);
    art.line(berry(74), [(137, 13), (131, 17), (138, 20)]);
    art.oval(berry(70), (105, 5), (133, 28));
    art.oval(berry(20), (107, 7), (131, 26));
    art.line(berry(67), [(112, 9), (119, 13), (123, 17), (126, 23)]);
    art.line(berry(45), [(109, 14), (116, 17), (120, 21), (122, 25)]);
    art.line(colors.content, [(119, 8), (124, 10), (127, 13)]);
}

fn frost(art: &Artwork, colors: &Theme) {
    let ice = |percent: u8| mix(colors.title_bg, colors.accent, f32::from(percent) / 100.0);
    art.line(ice(22), [(40, 29), (59, 14), (73, 24), (86, 11), (103, 24)]);
    art.polygon(ice(25), [(74, 28), (91, 12), (104, 28)]);
    art.polygon(colors.search_bg, [(91, 12), (88, 28), (74, 28)]);
    art.polygon(ice(41), [(98, 25), (116, 3), (138, 15), (124, 29)]);
    art.polygon(colors.content, [(98, 25), (116, 3), (113, 17)]);
    art.polygon(ice(26), [(116, 3), (138, 15), (113, 17)]);
    art.polygon(ice(67), [(113, 17), (138, 15), (124, 29)]);
    art.line(ice(76), [(98, 25), (116, 3), (138, 15), (124, 29), (98, 25), (113, 17)]);
    art.sparkle(ice(54), 141, 6, 2);
}

fn eclipse(art: &Artwork, colors: &Theme) {
    let bronze = |percent: u8| mix(colors.title_bg, colors.accent, f32::from(percent) / 100.0);
    art.line(bronze(16), [(29, 20), (60, 20), (69, 18), (97, 18)]);
    art.line(bronze(31), [(61, 16), (104, 16)]);
    art.line(bronze(22), [(77, 12), (103, 13)]);
    art.line(bronze(35), [(129, 16), (142, 16)]);
    art.ring(bronze(24), (102, 0), (135, 32));
    art.oval(bronze(56), (105, 3), (132, 30));
    art.oval(bronze(91), (107, 4), (130, 29));
    art.oval(colors.content, (106, 7), (127, 28));
    art.line(bronze(78), [(117, 4), (122, 5), (128, 9), (130, 14)]);
    art.sparkle(bronze(74), 131, 7, 2);
}

fn cyber_grid(art: &Artwork, colors: &Theme) {
    let mint = |percent: u8| mix(colors.title_bg, colors.accent, f32::from(percent) / 100.0);
    for x in [60, 72, 84, 96, 108, 120, 132] {
        art.line(mint(16), [(x, 4), (x, 28)]);
    }
    for y in [4, 12, 20, 28] {
        art.line(mint(16), [(60, y), (142, y)]);
    }
    art.line(mint(58), [(63, 19), (86, 19), (94, 11), (109, 11)]);
    art.line(mint(70), [(86, 27), (100, 27), (107, 20), (110, 20)]);
    art.line(mint(58), [(125, 11), (133, 11), (138, 6)]);
    art.line(mint(83), [(125, 20), (133, 20), (139, 26)]);
    art.polygon(mint(30), [(112, 7), (125, 7), (130, 12), (130, 20), (125, 25), (112, 25), (107, 20), (107, 12)]);
    art.line(
        mint(82),
        [(112, 7), (125, 7), (130, 12), (130, 20), (125, 25), (112, 25), (107, 20), (107, 12), (112, 7)],
    );
    art.polygon(mint(83), [(115, 12), (122, 12), (122, 20), (115, 20)]);
    for (x, y) in [(63, 19), (86, 27), (138, 6), (139, 26)] {
        art.oval(mint(91), (x - 2, y - 2), (x + 2, y + 2));
    }
}
