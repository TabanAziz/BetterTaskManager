// Drawing layer: one 32-bit DIB back buffer per window. GDI draws text and icons into it;
// anti-aliased shapes (rounded rects, graph lines, area fills) are rasterised directly
// into the pixel memory. Pixels are 0xAARRGGBB; alpha matters only for the Mica areas.

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{DrawIconEx, DI_NORMAL};

use crate::util::wide;

pub type Color = u32; // 0xRRGGBB

#[inline]
pub fn rgb(r: u8, g: u8, b: u8) -> Color {
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}
#[inline]
pub fn cr(c: Color) -> u32 {
    // 0xRRGGBB -> COLORREF 0x00BBGGRR
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}
#[inline]
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let ch = |s: u32| -> u32 {
        let x = ((a >> s) & 0xFF) as f32;
        let y = ((b >> s) & 0xFF) as f32;
        ((x + (y - x) * t).round() as u32) & 0xFF
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}
#[inline]
pub fn scale_color(c: Color, f: f32) -> Color {
    mix(0, c, f)
}

pub struct Canvas {
    pub hdc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    pub px: *mut u32,
    pub w: i32,
    pub h: i32,
}

impl Canvas {
    pub fn new() -> Canvas {
        let hdc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        unsafe {
            SetBkMode(hdc, TRANSPARENT as i32);
        }
        Canvas { hdc, bmp: std::ptr::null_mut(), old: std::ptr::null_mut(), px: std::ptr::null_mut(), w: 0, h: 0 }
    }

    pub fn ensure(&mut self, w: i32, h: i32) {
        let w = w.max(1);
        let h = h.max(1);
        if w == self.w && h == self.h && !self.bmp.is_null() {
            return;
        }
        unsafe {
            if !self.bmp.is_null() {
                SelectObject(self.hdc, self.old);
                DeleteObject(self.bmp);
            }
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h;
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = BI_RGB;
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            self.bmp = CreateDIBSection(self.hdc, &bi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            self.px = bits as *mut u32;
            self.old = SelectObject(self.hdc, self.bmp);
        }
        self.w = w;
        self.h = h;
    }

    #[inline]
    pub(super) fn flush(&self) {
        unsafe { GdiFlush() };
    }

    #[inline]
    fn clip(&self, x: i32, y: i32, w: i32, h: i32) -> Option<(i32, i32, i32, i32)> {
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + w).min(self.w);
        let y1 = (y + h).min(self.h);
        if x1 <= x0 || y1 <= y0 {
            None
        } else {
            Some((x0, y0, x1, y1))
        }
    }

    /// Opaque fill (alpha = 255).
    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        self.flush();
        let Some((x0, y0, x1, y1)) = self.clip(x, y, w, h) else { return };
        let v = 0xFF00_0000 | c;
        for yy in y0..y1 {
            let row =
                unsafe { std::slice::from_raw_parts_mut(self.px.add((yy * self.w + x0) as usize), (x1 - x0) as usize) };
            row.fill(v);
        }
    }

    /// Fill with raw ARGB (used to clear Mica areas to transparent).
    pub fn fill_raw(&mut self, x: i32, y: i32, w: i32, h: i32, v: u32) {
        self.flush();
        let Some((x0, y0, x1, y1)) = self.clip(x, y, w, h) else { return };
        for yy in y0..y1 {
            let row =
                unsafe { std::slice::from_raw_parts_mut(self.px.add((yy * self.w + x0) as usize), (x1 - x0) as usize) };
            row.fill(v);
        }
    }

    #[inline]
    pub(super) fn blend_px(&mut self, x: i32, y: i32, c: Color, a: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || a <= 0.0 {
            return;
        }
        let p = unsafe { &mut *self.px.add((y * self.w + x) as usize) };
        let d = *p & 0xFF_FFFF;
        let alpha = *p >> 24;
        let m = mix(d, c, a);
        let na = if alpha == 0xFF { 0xFF } else { (alpha as f32 + (255.0 - alpha as f32) * a).round() as u32 };
        *p = (na << 24) | m;
    }

    /// Alpha-blend a rectangle of colour over the existing pixels.
    pub fn blend(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color, a: f32) {
        self.flush();
        let Some((x0, y0, x1, y1)) = self.clip(x, y, w, h) else { return };
        for yy in y0..y1 {
            for xx in x0..x1 {
                self.blend_px(xx, yy, c, a);
            }
        }
    }

    /// Opaque vertical span y0..y1; the caller has flushed GDI.
    pub(super) fn column(&mut self, x: i32, y0: i32, y1: i32, c: Color) {
        if x < 0 || x >= self.w {
            return;
        }
        for y in y0.max(0)..y1.min(self.h) {
            unsafe { *self.px.add((y * self.w + x) as usize) = 0xFF00_0000 | c };
        }
    }

    #[inline]
    fn run(&mut self, yy: i32, xa: i32, xb: i32, c: Color, a: f32) {
        let xa = xa.max(0);
        let xb = xb.min(self.w);
        if xb <= xa || yy < 0 || yy >= self.h {
            return;
        }
        if a >= 0.999 {
            let row =
                unsafe { std::slice::from_raw_parts_mut(self.px.add((yy * self.w + xa) as usize), (xb - xa) as usize) };
            row.fill(0xFF00_0000 | c);
        } else {
            for xx in xa..xb {
                self.blend_px(xx, yy, c, a);
            }
        }
    }

    /// Anti-aliased rounded rectangle. Only the corner/edge bands are rasterised per
    /// pixel; interiors are plain spans, so large panels cost almost nothing.
    #[allow(clippy::too_many_arguments)]
    pub fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color, a: f32) {
        self.flush();
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let x1 = (x + w).ceil() as i32;
        let y1 = (y + h).ceil() as i32;
        let band = r.ceil() as i32 + 1;
        let (hx, hy) = (x + w / 2.0, y + h / 2.0);
        let cov = |xx: i32, yy: i32| -> f32 {
            let px = xx as f32 + 0.5;
            let py = yy as f32 + 0.5;
            let cx = (px - hx).abs() - (w / 2.0 - r);
            let cy = (py - hy).abs() - (h / 2.0 - r);
            let ox = cx.max(0.0);
            let oy = cy.max(0.0);
            let d = (ox * ox + oy * oy).sqrt() + cx.max(cy).min(0.0) - r;
            (0.5 - d).clamp(0.0, 1.0)
        };
        for yy in y0.max(0)..y1.min(self.h) {
            if yy < y0 + band || yy >= y1 - band {
                for xx in x0.max(0)..x1.min(self.w) {
                    let k = cov(xx, yy);
                    if k > 0.0 {
                        self.blend_px(xx, yy, c, k * a);
                    }
                }
            } else {
                let l1 = (x0 + band).min(x1);
                let r0 = (x1 - band).max(l1);
                for xx in x0.max(0)..l1.min(self.w) {
                    let k = cov(xx, yy);
                    if k > 0.0 {
                        self.blend_px(xx, yy, c, k * a);
                    }
                }
                self.run(yy, l1, r0, c, a);
                for xx in r0.max(0)..x1.min(self.w) {
                    let k = cov(xx, yy);
                    if k > 0.0 {
                        self.blend_px(xx, yy, c, k * a);
                    }
                }
            }
        }
    }

    /// 1px anti-aliased rounded outline (only touches pixels near the border).
    #[allow(clippy::too_many_arguments)]
    pub fn round_rect_outline(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color, a: f32) {
        self.flush();
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        let x0 = x.floor() as i32 - 1;
        let y0 = y.floor() as i32 - 1;
        let x1 = (x + w).ceil() as i32 + 1;
        let y1 = (y + h).ceil() as i32 + 1;
        let band = r.ceil() as i32 + 2;
        let (hx, hy) = (x + w / 2.0, y + h / 2.0);
        let cov = |xx: i32, yy: i32| -> f32 {
            let px = xx as f32 + 0.5;
            let py = yy as f32 + 0.5;
            let cx = (px - hx).abs() - (w / 2.0 - r);
            let cy = (py - hy).abs() - (h / 2.0 - r);
            let ox = cx.max(0.0);
            let oy = cy.max(0.0);
            let d = (ox * ox + oy * oy).sqrt() + cx.max(cy).min(0.0) - r;
            (1.0 - (d + 0.5).abs()).clamp(0.0, 1.0)
        };
        for yy in y0.max(0)..y1.min(self.h) {
            let full = yy < y0 + band || yy >= y1 - band;
            let mut xx = x0.max(0);
            let end = x1.min(self.w);
            while xx < end {
                if !full && xx >= x0 + band && xx < x1 - band {
                    xx = x1 - band;
                    continue;
                }
                let k = cov(xx, yy);
                if k > 0.0 {
                    self.blend_px(xx, yy, c, k * a);
                }
                xx += 1;
            }
        }
    }

    /// Thin horizontal / vertical lines.
    pub fn hline(&mut self, x: i32, y: i32, w: i32, c: Color) {
        self.fill(x, y, w, 1, c);
    }
    pub fn vline(&mut self, x: i32, y: i32, h: i32, c: Color) {
        self.fill(x, y, 1, h, c);
    }

    pub fn set_alpha_opaque(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.flush();
        let Some((x0, y0, x1, y1)) = self.clip(x, y, w, h) else { return };
        for yy in y0..y1 {
            let row =
                unsafe { std::slice::from_raw_parts_mut(self.px.add((yy * self.w + x0) as usize), (x1 - x0) as usize) };
            for p in row {
                *p |= 0xFF00_0000;
            }
        }
    }

    /// Turn "light content drawn on black" into premultiplied alpha so it composes over Mica.
    pub fn premultiply_from_black(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.flush();
        let Some((x0, y0, x1, y1)) = self.clip(x, y, w, h) else { return };
        for yy in y0..y1 {
            let row =
                unsafe { std::slice::from_raw_parts_mut(self.px.add((yy * self.w + x0) as usize), (x1 - x0) as usize) };
            for p in row {
                let r = (*p >> 16) & 0xFF;
                let g = (*p >> 8) & 0xFF;
                let b = *p & 0xFF;
                let a = r.max(g).max(b);
                *p = (a << 24) | (r << 16) | (g << 8) | b;
            }
        }
    }

    pub fn text(&self, font: HFONT, s: &str, rc: (i32, i32, i32, i32), c: Color, flags: u32) {
        if s.is_empty() || rc.2 <= 0 {
            return;
        }
        let w: Vec<u16> = s.encode_utf16().collect();
        let mut r = RECT { left: rc.0, top: rc.1, right: rc.0 + rc.2, bottom: rc.1 + rc.3 };
        unsafe {
            SelectObject(self.hdc, font);
            SetTextColor(self.hdc, cr(c));
            // Fast path: measure once and place with ExtTextOut; DrawText (slower) only
            // when the text needs an ellipsis.
            let mut sz = windows_sys::Win32::Foundation::SIZE { cx: 0, cy: 0 };
            GetTextExtentPoint32W(self.hdc, w.as_ptr(), w.len() as i32, &mut sz);
            if sz.cx <= rc.2 || flags & DT_END_ELLIPSIS == 0 {
                let x = if flags & DT_RIGHT != 0 {
                    r.right - sz.cx
                } else if flags & DT_CENTER != 0 {
                    r.left + (rc.2 - sz.cx) / 2
                } else {
                    r.left
                };
                let y = if flags & DT_VCENTER != 0 { r.top + (rc.3 - sz.cy) / 2 } else { r.top };
                ExtTextOutW(self.hdc, x, y, ETO_CLIPPED, &r, w.as_ptr(), w.len() as u32, std::ptr::null());
            } else {
                DrawTextW(self.hdc, w.as_ptr(), w.len() as i32, &mut r, flags | DT_NOPREFIX | DT_SINGLELINE);
            }
        }
    }

    pub fn text_width(&self, font: HFONT, s: &str) -> i32 {
        if s.is_empty() {
            return 0;
        }
        let w: Vec<u16> = s.encode_utf16().collect();
        let mut sz = windows_sys::Win32::Foundation::SIZE { cx: 0, cy: 0 };
        unsafe {
            SelectObject(self.hdc, font);
            GetTextExtentPoint32W(self.hdc, w.as_ptr(), w.len() as i32, &mut sz);
        }
        sz.cx
    }

    pub fn icon(&self, hicon: isize, x: i32, y: i32, size: i32) {
        if hicon == 0 {
            return;
        }
        unsafe {
            DrawIconEx(self.hdc, x, y, hicon as _, size, size, 0, std::ptr::null_mut(), DI_NORMAL);
        }
    }

    pub fn present(&self, dst: HDC, x: i32, y: i32, w: i32, h: i32) {
        unsafe {
            GdiFlush();
            BitBlt(dst, x, y, w, h, self.hdc, x, y, SRCCOPY);
        }
    }

    /// Clip GDI text to a rectangle (None removes the clip).
    pub fn clip_rect(&self, r: Option<(i32, i32, i32, i32)>) {
        unsafe {
            match r {
                Some((x, y, w, h)) => {
                    let rgn = CreateRectRgn(x, y, x + w, y + h);
                    SelectClipRgn(self.hdc, rgn);
                    DeleteObject(rgn);
                }
                None => {
                    SelectClipRgn(self.hdc, std::ptr::null_mut());
                }
            }
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            if !self.bmp.is_null() {
                SelectObject(self.hdc, self.old);
                DeleteObject(self.bmp);
            }
            DeleteDC(self.hdc);
        }
    }
}

pub const DT_L: u32 = DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS;
pub const DT_R: u32 = DT_RIGHT | DT_VCENTER | DT_END_ELLIPSIS;
pub const DT_C: u32 = DT_CENTER | DT_VCENTER;

pub fn make_font(face: &str, px: i32, weight: i32, quality: u32) -> HFONT {
    let f = wide(face);
    unsafe {
        CreateFontW(
            -px,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_TT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            quality,
            0,
            f.as_ptr(),
        )
    }
}

pub fn font_exists(face: &str) -> bool {
    unsafe extern "system" fn cb(_: *const LOGFONTW, _: *const TEXTMETRICW, _: u32, lp: isize) -> i32 {
        *(lp as *mut bool) = true;
        0
    }
    let mut lf: LOGFONTW = unsafe { std::mem::zeroed() };
    lf.lfCharSet = DEFAULT_CHARSET;
    for (i, ch) in face.encode_utf16().take(31).enumerate() {
        lf.lfFaceName[i] = ch;
    }
    let mut found = false;
    unsafe {
        let dc = GetDC(std::ptr::null_mut());
        EnumFontFamiliesExW(dc, &lf, Some(cb), &mut found as *mut bool as isize, 0);
        ReleaseDC(std::ptr::null_mut(), dc);
    }
    found
}
