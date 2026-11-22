//! Lets Ctrl+Shift+Esc open BTM.
//!
//! Windows starts the "Debugger" configured for an image in place of that image. The
//! value always names the protected Program Files copy (see `protected`), and a value
//! written by another tool is never deleted by us.

use windows_sys::Win32::System::Registry::{RegDeleteKeyValueW, RegSetKeyValueW, HKEY_LOCAL_MACHINE, REG_SZ};

use crate::protected;
use crate::util::wide;

const VALUE: &str = "Debugger";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Redirect {
    /// Ctrl+Shift+Esc opens the Windows Task Manager.
    Windows,
    Ours,
    /// Another replacement (for example Process Explorer) is registered.
    Foreign(String),
}

impl Redirect {
    pub fn is_ours(&self) -> bool {
        *self == Redirect::Ours
    }
}

pub fn state() -> Redirect {
    let Some(command) =
        crate::sysinfo::reg_string(HKEY_LOCAL_MACHINE, &crate::product::task_manager_options_key(), VALUE)
            .filter(|c| !c.is_empty())
    else {
        return Redirect::Windows;
    };
    let ours = protected::executable()
        .map(|exe| command.trim_matches('"').eq_ignore_ascii_case(&exe.to_string_lossy()))
        .unwrap_or(false);
    if ours {
        Redirect::Ours
    } else {
        Redirect::Foreign(command)
    }
}

/// Elevated.
pub fn enable() -> Result<(), String> {
    let exe = protected::deploy()?;
    let (key, value, data) =
        (wide(&crate::product::task_manager_options_key()), wide(VALUE), wide(&format!("\"{}\"", exe.display())));
    let bytes = std::mem::size_of_val(data.as_slice()) as u32;
    match unsafe {
        RegSetKeyValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), REG_SZ, data.as_ptr().cast(), bytes)
    } {
        0 => Ok(()),
        code => Err(format!("The shortcut could not be changed (error {code}).")),
    }
}

/// Elevated.
pub fn disable() -> Result<(), String> {
    if state().is_ours() {
        let (key, value) = (wide(&crate::product::task_manager_options_key()), wide(VALUE));
        let code = unsafe { RegDeleteKeyValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr()) };
        if code != 0 {
            return Err(format!("The shortcut could not be restored (error {code})."));
        }
    }
    protected::remove_if_unused();
    Ok(())
}
