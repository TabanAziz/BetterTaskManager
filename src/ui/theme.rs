use std::ffi::c_void;

use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD};

use super::gfx::{mix, rgb, Color};
use crate::util::wide;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
    DeepSpace,
    PaperWhite,
    CandyRush,
    Frost,
    Eclipse,
    CyberGrid,
}

impl ThemePreference {
    pub const ALL: [Self; 9] = [
        Self::System,
        Self::Light,
        Self::Dark,
        Self::DeepSpace,
        Self::PaperWhite,
        Self::CandyRush,
        Self::Frost,
        Self::Eclipse,
        Self::CyberGrid,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::DeepSpace => "Deep Space",
            Self::PaperWhite => "Paper White",
            Self::CandyRush => "Candy Rush",
            Self::Frost => "Frost",
            Self::Eclipse => "Eclipse",
            Self::CyberGrid => "Cyber Grid",
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
            Self::DeepSpace => "deep-space",
            Self::PaperWhite => "paper-white",
            Self::CandyRush => "candy-rush",
            Self::Frost => "frost",
            Self::Eclipse => "eclipse",
            Self::CyberGrid => "cyber-grid",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|theme| theme.key() == key)
    }
}

struct CustomPalette {
    dark: bool,
    chrome: Color,
    title: Color,
    sidebar: Color,
    field: Color,
    content: Color,
    text: Color,
    muted: Color,
    edge: Color,
    active: Color,
    accent: Color,
}

const FALLBACK_ACCENT: [Color; 8] = [0x99EBFF, 0x4CC2FF, 0x0091F8, 0x0078D4, 0x0067C0, 0x003E92, 0x001A68, 0xF7630C];

#[derive(Clone, Copy)]
pub(super) enum RowTint {
    Normal,
    Hover,
    Selected,
}

impl RowTint {
    pub(super) fn strength(self, dark: bool) -> f32 {
        match self {
            Self::Normal => 0.0,
            Self::Hover => 0.05,
            Self::Selected if dark => 0.10,
            Self::Selected => 0.08,
        }
    }
}

pub struct Theme {
    pub dark: bool,
    pub custom: Option<ThemePreference>,
    pub title_bg: Color,
    pub nav_bg: Color,
    pub content: Color,
    pub text: Color,
    pub text2: Color,
    pub text3: Color,
    pub divider: Color,
    heat: [[(Color, Color); 3]; 6],
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
    pub fn load(preference: ThemePreference) -> Self {
        match preference {
            ThemePreference::System => Self::standard(apps_use_light(), Self::windows_accent()),
            ThemePreference::Light => Self::standard(true, Self::windows_accent()),
            ThemePreference::Dark => Self::standard(false, Self::windows_accent()),
            custom => Self::custom(custom),
        }
    }

    pub(super) fn previews() -> [Self; ThemePreference::ALL.len()] {
        let light = apps_use_light();
        let accent = Self::windows_accent();
        ThemePreference::ALL.map(|preference| match preference {
            ThemePreference::System => Self::standard(light, accent),
            ThemePreference::Light => Self::standard(true, accent),
            ThemePreference::Dark => Self::standard(false, accent),
            custom => Self::custom(custom),
        })
    }

    fn windows_accent() -> [Color; 8] {
        accent_palette().unwrap_or(FALLBACK_ACCENT)
    }

    fn custom(preference: ThemePreference) -> Self {
        let palette = custom_palette(preference);
        let mut theme = Self::standard(!palette.dark, FALLBACK_ACCENT);
        theme.custom = Some(preference);
        theme.title_bg = palette.title;
        theme.nav_bg = palette.sidebar;
        theme.content = palette.content;
        theme.text = palette.text;
        theme.text2 = palette.muted;
        theme.text3 = palette.muted;
        theme.divider = palette.edge;
        theme.header_hot = palette.active;
        theme.accent = palette.accent;
        theme.accent_strong = mix(palette.accent, palette.text, 0.14);
        theme.thumb_bg = palette.field;
        theme.thumb_border = palette.edge;
        theme.overlay = palette.accent;
        theme.chrome_fg = palette.text;
        theme.chrome_hover = palette.chrome;
        theme.chrome_sel = palette.active;
        theme.search_bg = palette.field;
        theme.search_hover = mix(palette.field, palette.accent, 0.06);
        theme.search_focus = mix(palette.field, palette.accent, 0.10);
        theme.search_border = palette.edge;
        theme.search_hint = palette.muted;
        theme.make_heat_palette();
        theme
    }

    fn make_heat_palette(&mut self) {
        // Keep low values close to the table surface and give the upper levels room to stand out.
        let source = if self.accent == self.content { self.text } else { self.accent };
        self.heat = [0.08, 0.19, 0.32, 0.46, 0.61, 0.78].map(|strength| {
            let base = mix(self.content, source, strength);
            [RowTint::Normal, RowTint::Hover, RowTint::Selected].map(|state| {
                let bg = mix(base, self.overlay, state.strength(self.dark));
                (bg, readable_text(bg, self.text))
            })
        });
    }

    pub(super) fn heat_cell(&self, level: usize, state: RowTint) -> (Color, Color) {
        self.heat[level][state as usize]
    }

    pub fn use_mica(&self) -> bool {
        self.custom.is_none()
    }

    pub fn color_for(&self, dark: Color, light: Color, custom: Color) -> Color {
        if self.custom.is_some() {
            custom
        } else if self.dark {
            dark
        } else {
            light
        }
    }

    pub fn accent_text(&self) -> Color {
        self.accent_text_on(self.accent)
    }

    pub fn accent_text_on(&self, background: Color) -> Color {
        readable_text(background, if self.dark { 0x000000 } else { 0xFFFFFF })
    }

    pub(super) fn readable_text_on(&self, background: Color) -> Color {
        readable_text(background, self.text)
    }

    pub fn control_bg(&self, hover: bool) -> Color {
        if hover {
            self.color_for(0x3A3A3A, 0xE8E8E8, self.header_hot)
        } else {
            self.color_for(0x2D2D2D, 0xF3F3F3, self.chrome_hover)
        }
    }

    fn standard(light: bool, pal: [Color; 8]) -> Self {
        let mut theme = if !light {
            let content = 0x191919;
            Theme {
                dark: true,
                custom: None,
                title_bg: 0,
                nav_bg: 0,
                content,
                text: 0xFFFFFF,
                text2: 0xC8C8C8,
                text3: 0x9A9A9A,
                divider: 0x2D2D2D,
                heat: [[(0, 0); 3]; 6],
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
            Theme {
                dark: false,
                custom: None,
                title_bg: 0xF3F3F3,
                nav_bg: 0xF3F3F3,
                content,
                text: 0x1A1A1A,
                text2: 0x5C5C5C,
                text3: 0x6B6B6B,
                divider: 0xE5E5E5,
                heat: [[(0, 0); 3]; 6],
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
        };
        theme.make_heat_palette();
        theme
    }
}

fn luminance(color: Color) -> f32 {
    let linear = |shift: u32| {
        let channel = ((color >> shift) & 0xFF) as f32 / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(16) + 0.7152 * linear(8) + 0.0722 * linear(0)
}

fn contrast(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn readable_text(background: Color, preferred: Color) -> Color {
    let bg = luminance(background);
    if contrast(bg, luminance(preferred)) >= 4.5 {
        preferred
    } else if contrast(bg, 0.0) >= contrast(bg, 1.0) {
        0x000000
    } else {
        0xFFFFFF
    }
}

// Match BetterExplorer's named themes, with colors in the DIB's RGB order.
fn custom_palette(preference: ThemePreference) -> CustomPalette {
    use ThemePreference::*;

    match preference {
        DeepSpace => CustomPalette {
            dark: true,
            chrome: 0x141831,
            title: 0x1B193F,
            sidebar: 0x11152A,
            field: 0x232840,
            content: 0x0F1223,
            text: 0xEFEFFF,
            muted: 0xB4BDE0,
            edge: 0x414670,
            active: 0x2F2B5B,
            accent: 0xC2ABFF,
        },
        PaperWhite => CustomPalette {
            dark: false,
            chrome: 0xF7F3E8,
            title: 0xF2ECDC,
            sidebar: 0xF6F0E1,
            field: 0xFFFBF0,
            content: 0xFFFDF6,
            text: 0x232B39,
            muted: 0x555E6F,
            edge: 0xCAC0A8,
            active: 0xE0E9F6,
            accent: 0x2B5091,
        },
        CandyRush => CustomPalette {
            dark: false,
            chrome: 0xFBE0F0,
            title: 0xF9D5EB,
            sidebar: 0xFDE9F4,
            field: 0xFFF3FA,
            content: 0xFFF8FC,
            text: 0x431631,
            muted: 0x6B3757,
            edge: 0xD99BC1,
            active: 0xF5C1E0,
            accent: 0x971B5F,
        },
        Frost => CustomPalette {
            dark: false,
            chrome: 0xDFF2F9,
            title: 0xD5EDF8,
            sidebar: 0xE6F5FB,
            field: 0xF3FCFF,
            content: 0xF8FDFF,
            text: 0x193949,
            muted: 0x426576,
            edge: 0xA4C8D7,
            active: 0xC5E5F2,
            accent: 0x175C81,
        },
        Eclipse => CustomPalette {
            dark: true,
            chrome: 0x231C19,
            title: 0x2B1E19,
            sidebar: 0x1E1817,
            field: 0x35261F,
            content: 0x161312,
            text: 0xFCEFE1,
            muted: 0xCFB49E,
            edge: 0x5B4233,
            active: 0x432F26,
            accent: 0xFFAF71,
        },
        CyberGrid => CustomPalette {
            dark: true,
            chrome: 0x0D2424,
            title: 0x0D2C2B,
            sidebar: 0x0A2021,
            field: 0x133636,
            content: 0x09191B,
            text: 0xE1FCF2,
            muted: 0x93C4B7,
            edge: 0x31685E,
            active: 0x14433B,
            accent: 0x67F4C1,
        },
        System | Light | Dark => unreachable!("standard themes use Windows colors"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_button_text_stays_readable_for_the_default_windows_accent() {
        let theme = Theme::standard(true, FALLBACK_ACCENT);
        assert_eq!(theme.accent_text(), 0xFFFFFF);
    }

    #[test]
    fn heat_levels_and_chrome_text_remain_distinct_in_every_theme() {
        for (preference, theme) in ThemePreference::ALL.into_iter().zip(Theme::previews()) {
            for bg in [theme.nav_bg, theme.chrome_hover, theme.chrome_sel, theme.title_bg] {
                assert!(contrast(luminance(bg), luminance(theme.chrome_fg)) >= 4.5, "{preference:?}: chrome");
            }
            for (bg, ink) in [
                (theme.header_hot, theme.text2),
                (theme.control_bg(true), theme.text2),
                (theme.content, theme.text3),
                (theme.search_focus, theme.search_hint),
                (theme.accent, theme.accent_text()),
                (theme.accent_strong, theme.accent_text_on(theme.accent_strong)),
            ] {
                assert!(contrast(luminance(bg), luminance(ink)) >= 4.5, "{preference:?}: {bg:06X}/{ink:06X}");
            }
            for tint in [RowTint::Normal, RowTint::Hover, RowTint::Selected] {
                let mut previous: Option<Color> = None;
                for level in 0..6 {
                    let (bg, fg) = theme.heat_cell(level, tint);
                    assert!(contrast(luminance(bg), luminance(fg)) >= 4.5, "{preference:?}: heat level {level}");
                    if let Some(previous) = previous {
                        let distance: u32 =
                            [0, 8, 16].into_iter().map(|s| ((bg >> s) & 255).abs_diff((previous >> s) & 255)).sum();
                        assert!(distance >= 12, "{preference:?}: adjacent heat levels {level}");
                    }
                    previous = Some(bg);
                }
            }
        }
    }

    #[test]
    fn saved_theme_names_round_trip_and_fall_back_to_system() {
        let choices =
            ["system", "light", "dark", "deep-space", "paper-white", "candy-rush", "frost", "eclipse", "cyber-grid"];
        assert_eq!(ThemePreference::ALL.map(ThemePreference::key), choices);
        for (choice, expected) in choices.into_iter().zip(ThemePreference::ALL) {
            assert_eq!(ThemePreference::from_key(choice), Some(expected));
        }
        assert_eq!(ThemePreference::from_key("future-theme").unwrap_or_default(), ThemePreference::System);
        assert_eq!(ThemePreference::default(), ThemePreference::System);
    }

    #[test]
    fn standard_themes_keep_windows_chrome() {
        for (preference, dark) in [(ThemePreference::Light, false), (ThemePreference::Dark, true)] {
            let theme = Theme::load(preference);
            assert_eq!(theme.dark, dark);
            assert!(theme.use_mica());
        }
        assert!(Theme::load(ThemePreference::System).use_mica());
    }

    #[test]
    fn custom_theme_colors_match_better_explorer() {
        for (preference, expected) in [
            (ThemePreference::DeepSpace, (true, 0x1B193F, 0x11152A, 0x0F1223, 0xC2ABFF)),
            (ThemePreference::PaperWhite, (false, 0xF2ECDC, 0xF6F0E1, 0xFFFDF6, 0x2B5091)),
            (ThemePreference::CandyRush, (false, 0xF9D5EB, 0xFDE9F4, 0xFFF8FC, 0x971B5F)),
            (ThemePreference::Frost, (false, 0xD5EDF8, 0xE6F5FB, 0xF8FDFF, 0x175C81)),
            (ThemePreference::Eclipse, (true, 0x2B1E19, 0x1E1817, 0x161312, 0xFFAF71)),
            (ThemePreference::CyberGrid, (true, 0x0D2C2B, 0x0A2021, 0x09191B, 0x67F4C1)),
        ] {
            let theme = Theme::load(preference);
            assert_eq!((theme.dark, theme.title_bg, theme.nav_bg, theme.content, theme.accent), expected);
            assert_eq!(theme.custom, Some(preference));
        }
    }
}
