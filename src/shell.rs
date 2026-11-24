//! Handing things to the Windows shell.
//!
//! When BTM itself runs elevated, documents, links and settings pages go through
//! explorer.exe: their handlers are per-user registry entries that any program of the
//! user can change, and must not inherit administrator rights.

use std::{path::Path, process::Command, sync::OnceLock};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;
use windows_sys::Win32::System::SystemInformation::{GetSystemDirectoryW, GetSystemWindowsDirectoryW};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::util::{from_wide, wide};

pub fn elevated() -> bool {
    static ELEVATED: OnceLock<bool> = OnceLock::new();
    *ELEVATED.get_or_init(|| crate::sampler::token_elevation().unwrap_or(true))
}

/// Not %SystemRoot%: the user controls the environment of an elevated process.
pub fn system_directory() -> String {
    let mut buffer = [0u16; 260];
    let n = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    from_wide(&buffer[..n.min(buffer.len())])
}

fn explorer() -> String {
    let mut buffer = [0u16; 260];
    let n = unsafe { GetSystemWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    format!("{}\\explorer.exe", from_wide(&buffer[..n.min(buffer.len())]))
}

fn execute(owner: HWND, file: &str, arguments: &str) {
    let (verb, file, arguments) = (wide("open"), wide(file), wide(arguments));
    unsafe { ShellExecuteW(owner, verb.as_ptr(), file.as_ptr(), arguments.as_ptr(), std::ptr::null(), SW_SHOWNORMAL) };
}

/// A full path, URL or ms-settings: link.
pub fn open(owner: HWND, target: &str) {
    if elevated() {
        execute(owner, &explorer(), &format!("\"{target}\""));
    } else {
        execute(owner, target, "");
    }
}

pub fn show_in_folder(owner: HWND, path: &str) {
    if !elevated()
        && Path::new(path).is_absolute()
        && crate::sysinfo::reg_string(HKEY_CURRENT_USER, r"Software\Classes\Directory\shell", "").as_deref()
            == Some("BetterExplorer.Open")
    {
        if let Some(exe) = crate::sysinfo::reg_string(
            HKEY_CURRENT_USER,
            r"Software\Classes\Directory\shell\BetterExplorer.Open",
            "BetterExplorerExecutable",
        ) {
            let exe = Path::new(&exe);
            if exe.is_absolute()
                && exe.file_name().is_some_and(|name| name.eq_ignore_ascii_case("BetterExplorer.exe"))
                && Command::new(exe).args(["--select", path]).spawn().is_ok()
            {
                return;
            }
        }
    }
    execute(owner, &explorer(), &format!("/select,\"{path}\""));
}

pub fn search_online(owner: HWND, what: &str) {
    let query: String =
        what.chars().map(|c| if c.is_alphanumeric() || c == '.' || c == '-' { c } else { '+' }).collect();
    open(owner, &format!("https://www.bing.com/search?q={query}"));
}
