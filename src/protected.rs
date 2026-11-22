//! The administrator-owned copy of BTM in Program Files.
//!
//! Anything Windows may start with elevated rights (the data helper task, the
//! Ctrl+Shift+Esc redirect) must only ever point here, never at the per-user install,
//! which every process of the user could overwrite.

use std::path::PathBuf;

use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath};

use crate::product;
use crate::util::{from_wide_ptr, wide};

/// Resolved through the shell, not %ProgramFiles%: a user can override environment
/// variables, and this path is used by elevated code.
pub fn directory() -> Result<PathBuf, String> {
    let mut raw: *mut u16 = std::ptr::null_mut();
    let hr = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, std::ptr::null_mut(), &mut raw) };
    let base = (hr >= 0).then(|| unsafe { from_wide_ptr(raw) });
    unsafe { CoTaskMemFree(raw as *const _) };
    base.map(|b| PathBuf::from(b).join(product::NAME))
        .ok_or_else(|| "The Program Files folder could not be located.".to_string())
}

pub fn executable() -> Result<PathBuf, String> {
    Ok(directory()?.join(product::EXE))
}

fn running_from(target: &std::path::Path) -> bool {
    std::env::current_exe().map(|me| me.as_os_str().eq_ignore_ascii_case(target.as_os_str())).unwrap_or(false)
}

/// Elevated: copies the running executable into Program Files.
pub fn deploy() -> Result<PathBuf, String> {
    let target = executable()?;
    if running_from(&target) {
        return Ok(target);
    }
    let failed = |e: std::io::Error| format!("Could not write {}: {e}", target.display());
    let me = std::env::current_exe().map_err(failed)?;
    std::fs::create_dir_all(directory()?).map_err(failed)?;
    if std::fs::copy(&me, &target).is_err() {
        // A running image cannot be overwritten, but it can be renamed.
        let retired = target.with_extension("old");
        let _ = std::fs::remove_file(&retired);
        std::fs::rename(&target, &retired).map_err(failed)?;
        std::fs::copy(&me, &target).map_err(failed)?;
        delete_at_reboot(&retired);
    }
    Ok(target)
}

/// Elevated: removes the copy once neither the helper nor the shortcut redirect needs it.
pub fn remove_if_unused() {
    if crate::helper::installed_path().is_some() || crate::shell_hook::state().is_ours() {
        return;
    }
    let (Ok(target), Ok(dir)) = (executable(), directory()) else { return };
    if std::fs::remove_file(&target).is_err() {
        delete_at_reboot(&target);
        delete_at_reboot(&dir);
    } else {
        let _ = std::fs::remove_dir(&dir); // only succeeds when empty
    }
}

fn delete_at_reboot(path: &std::path::Path) {
    let p = wide(&path.to_string_lossy());
    unsafe { MoveFileExW(p.as_ptr(), std::ptr::null(), MOVEFILE_DELAY_UNTIL_REBOOT) };
}
