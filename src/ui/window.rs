//! Top-level window plumbing shared by the app and the off-screen renderer.

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use crate::product;
use crate::util::wide;

/// Resource number of the icon in resources/btm.rc.
const ICON: usize = 1;

pub enum Purpose {
    App,
    /// Never shown, never activated.
    Offscreen {
        demo: bool,
    },
}

#[derive(Clone, Copy)]
pub struct Bounds {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

impl Bounds {
    pub fn default_for(scale: f32) -> Self {
        let px = |dip: f32| (dip * scale).round() as i32;
        Self { left: wm::CW_USEDEFAULT, top: wm::CW_USEDEFAULT, width: px(1040.0), height: px(720.0) }
    }
}

pub fn app_icon(pixels: i32) -> isize {
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        wm::LoadImageW(module, ICON as *const u16, wm::IMAGE_ICON, pixels, pixels, 0) as isize
    }
}

pub fn create(purpose: Purpose, at: Bounds) -> Option<HWND> {
    let (class, extended, demo) = match purpose {
        Purpose::App => (product::WINDOW_CLASS, 0, false),
        Purpose::Offscreen { demo } => (product::OFFSCREEN_CLASS, wm::WS_EX_NOACTIVATE | wm::WS_EX_TOOLWINDOW, demo),
    };
    let (class, caption) = (wide(class), wide(product::NAME));
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let mut definition: wm::WNDCLASSEXW = std::mem::zeroed();
        definition.cbSize = std::mem::size_of_val(&definition) as u32;
        definition.hInstance = module;
        definition.lpszClassName = class.as_ptr();
        definition.lpfnWndProc = Some(super::wndproc);
        definition.style = wm::CS_DBLCLKS;
        if let Purpose::App = purpose {
            let metric = |index| wm::GetSystemMetrics(index);
            definition.hCursor = wm::LoadCursorW(std::ptr::null_mut(), wm::IDC_ARROW);
            definition.hIconSm = app_icon(metric(wm::SM_CXSMICON)) as _;
            definition.hIcon = app_icon(metric(wm::SM_CXICON)) as _;
        }
        wm::RegisterClassExW(&definition);

        let Bounds { left, top, width, height } = at;
        let parent = std::ptr::null_mut();
        let menu = std::ptr::null_mut();
        let extra = if demo { std::ptr::from_ref(&demo).cast() } else { std::ptr::null() };
        let style = wm::WS_OVERLAPPEDWINDOW;
        let window = wm::CreateWindowExW(
            extended,
            class.as_ptr(),
            caption.as_ptr(),
            style,
            left,
            top,
            width,
            height,
            parent,
            menu,
            module,
            extra,
        );
        (!window.is_null()).then_some(window)
    }
}

/// Pressing Ctrl+Shift+Esc again brings the open window forward, like Task Manager.
pub fn activate_existing() -> bool {
    let class = wide(product::WINDOW_CLASS);
    unsafe {
        let open = wm::FindWindowW(class.as_ptr(), std::ptr::null());
        if open.is_null() {
            return false;
        }
        if wm::IsIconic(open) != 0 {
            wm::ShowWindowAsync(open, wm::SW_RESTORE);
        }
        wm::SetForegroundWindow(open);
        true
    }
}

pub fn run_until_closed() {
    unsafe {
        let mut message: wm::MSG = std::mem::zeroed();
        loop {
            match wm::GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) {
                0 | -1 => break,
                _ => dispatch(&message),
            }
        }
    }
}

pub fn dispatch_pending() {
    unsafe {
        let mut message: wm::MSG = std::mem::zeroed();
        while wm::PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, wm::PM_REMOVE) != 0 {
            dispatch(&message);
        }
    }
}

unsafe fn dispatch(message: &wm::MSG) {
    wm::TranslateMessage(message);
    wm::DispatchMessageW(message);
}
