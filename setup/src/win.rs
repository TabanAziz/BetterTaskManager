//! The Win32 that setup needs.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Com as com;
use windows_sys::Win32::System::LibraryLoader as loader;
use windows_sys::Win32::System::Registry as r;
use windows_sys::Win32::System::Threading as th;
use windows_sys::Win32::UI::Controls as ui;
use windows_sys::Win32::UI::Shell as sh;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

fn wide(text: &str) -> Vec<u16> {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    units.push(0);
    units
}

pub fn harden_dll_search() {
    unsafe { loader::SetDefaultDllDirectories(loader::LOAD_LIBRARY_SEARCH_SYSTEM32) };
}

// ---------------------------------------------------------------- dialog

pub struct Dialog<'a> {
    pub heading: &'a str,
    pub text: &'a str,
    /// The confirming button; None shows just "Close".
    pub button: Option<&'a str>,
    pub checkbox: Option<(&'a str, bool)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Confirmed { checked: bool },
    Dismissed,
}

type TaskDialog = unsafe extern "system" fn(*const ui::TASKDIALOGCONFIG, *mut i32, *mut i32, *mut i32) -> i32;

/// Resolved at run time: only Common Controls 6 (from the manifest) has it.
fn task_dialog() -> Option<TaskDialog> {
    let library = wide("comctl32.dll");
    unsafe {
        let module = loader::LoadLibraryW(library.as_ptr());
        if module.is_null() {
            return None;
        }
        let found = loader::GetProcAddress(module, c"TaskDialogIndirect".as_ptr().cast())?;
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, TaskDialog>(found))
    }
}

pub fn dialogs_available() -> bool {
    task_dialog().is_some()
}

pub fn show(dialog: &Dialog) -> Answer {
    const CONFIRM: i32 = 100;
    let Some(task_dialog) = task_dialog() else { return Answer::Dismissed };
    let (title, heading, text) = (wide("Better Task Manager Setup"), wide(dialog.heading), wide(dialog.text));
    let button = dialog.button.map(wide);
    let checkbox = dialog.checkbox.map(|(label, on)| (wide(label), on));
    unsafe {
        let mut config: ui::TASKDIALOGCONFIG = std::mem::zeroed();
        config.cbSize = std::mem::size_of::<ui::TASKDIALOGCONFIG>() as u32;
        config.hInstance = loader::GetModuleHandleW(std::ptr::null());
        config.dwFlags = ui::TDF_ALLOW_DIALOG_CANCELLATION;
        config.pszWindowTitle = title.as_ptr();
        config.Anonymous1.pszMainIcon = std::ptr::without_provenance(1); // resource 1 in setup.rc
        config.pszMainInstruction = heading.as_ptr();
        config.pszContent = text.as_ptr();

        let custom = button.as_ref().map(|b| ui::TASKDIALOG_BUTTON { nButtonID: CONFIRM, pszButtonText: b.as_ptr() });
        match &custom {
            Some(custom) => {
                config.cButtons = 1;
                config.pButtons = custom;
                config.nDefaultButton = CONFIRM;
                config.dwCommonButtons = ui::TDCBF_CANCEL_BUTTON;
            }
            None => config.dwCommonButtons = ui::TDCBF_CLOSE_BUTTON,
        }
        if let Some((label, on)) = &checkbox {
            config.pszVerificationText = label.as_ptr();
            if *on {
                config.dwFlags |= ui::TDF_VERIFICATION_FLAG_CHECKED;
            }
        }
        let (mut pressed, mut checked) = (0i32, 0i32);
        let hr = task_dialog(&config, &mut pressed, std::ptr::null_mut(), &mut checked);
        if hr >= 0 && pressed == CONFIRM {
            Answer::Confirmed { checked: checked != 0 }
        } else {
            Answer::Dismissed
        }
    }
}

// ---------------------------------------------------------------- folders

fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let mut raw = std::ptr::null_mut();
        // KF_FLAG_CREATE: the per-user Programs folder may not exist yet.
        let hr = sh::SHGetKnownFolderPath(id, 0x8000, std::ptr::null_mut(), &mut raw);
        let path = (hr >= 0 && !raw.is_null()).then(|| {
            let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
            PathBuf::from(String::from_utf16_lossy(std::slice::from_raw_parts(raw, len)))
        });
        com::CoTaskMemFree(raw as *const c_void);
        path
    }
}

pub fn user_programs() -> Option<PathBuf> {
    known_folder(&sh::FOLDERID_UserProgramFiles)
}

pub fn start_menu() -> Option<PathBuf> {
    known_folder(&sh::FOLDERID_Programs)
}

pub fn desktop() -> Option<PathBuf> {
    known_folder(&sh::FOLDERID_Desktop)
}

pub fn local_app_data() -> Option<PathBuf> {
    known_folder(&sh::FOLDERID_LocalAppData)
}

// ---------------------------------------------------------------- shortcut

pub fn create_shortcut(link: &Path, target: &Path, dir: &Path) -> Result<(), String> {
    const SHELL_LINK: GUID = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
    const ISHELL_LINK_W: GUID = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
    const IPERSIST_FILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);
    const QUERY: usize = 0;
    const RELEASE: usize = 2;
    const SAVE: usize = 6;
    const SET_DESCRIPTION: usize = 7;
    const SET_DIRECTORY: usize = 9;
    const SET_PATH: usize = 20;
    type Raw = *mut c_void;
    type Query = unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> i32;
    type Release = unsafe extern "system" fn(Raw) -> u32;
    type SetText = unsafe extern "system" fn(Raw, *const u16) -> i32;
    type Save = unsafe extern "system" fn(Raw, *const u16, i32) -> i32;
    unsafe fn method<F: Copy>(object: Raw, slot: usize) -> F {
        std::mem::transmute_copy(&*(*(object as *const *const Raw)).add(slot))
    }

    let text = |p: &Path| wide(&p.to_string_lossy());
    let (file, path, folder, about) = (text(link), text(target), text(dir), wide("Better Task Manager"));
    unsafe {
        let initialised = com::CoInitializeEx(std::ptr::null(), com::COINIT_APARTMENTTHREADED as u32) >= 0;
        let mut object: Raw = std::ptr::null_mut();
        let context = com::CLSCTX_INPROC_SERVER;
        let mut hr = com::CoCreateInstance(&SHELL_LINK, std::ptr::null_mut(), context, &ISHELL_LINK_W, &mut object);
        if hr >= 0 {
            hr = method::<SetText>(object, SET_PATH)(object, path.as_ptr());
            method::<SetText>(object, SET_DIRECTORY)(object, folder.as_ptr());
            method::<SetText>(object, SET_DESCRIPTION)(object, about.as_ptr());
            let mut persist: Raw = std::ptr::null_mut();
            if hr >= 0 {
                hr = method::<Query>(object, QUERY)(object, &IPERSIST_FILE, &mut persist);
            }
            if hr >= 0 {
                hr = method::<Save>(persist, SAVE)(persist, file.as_ptr(), 1);
                method::<Release>(persist, RELEASE)(persist);
            }
            method::<Release>(object, RELEASE)(object);
        }
        if initialised {
            com::CoUninitialize();
        }
        if hr >= 0 {
            Ok(())
        } else {
            Err(format!("The shortcut {} could not be created.", link.display()))
        }
    }
}

// ---------------------------------------------------------------- registry

fn set(key: &str, name: &str, kind: u32, bytes: &[u8]) -> Result<(), String> {
    let (key, value) = (wide(key), wide(name));
    let data = bytes.as_ptr().cast();
    let status = unsafe {
        r::RegSetKeyValueW(r::HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), kind, data, bytes.len() as u32)
    };
    if status == 0 {
        Ok(())
    } else {
        Err(format!("The registry value {name} could not be written (error {status})."))
    }
}

pub fn set_text(key: &str, name: &str, value: &str) -> Result<(), String> {
    let bytes: Vec<u8> = wide(value).into_iter().flat_map(u16::to_le_bytes).collect();
    set(key, name, r::REG_SZ, &bytes)
}

pub fn set_number(key: &str, name: &str, value: u32) -> Result<(), String> {
    set(key, name, r::REG_DWORD, &value.to_le_bytes())
}

pub fn machine_value(key: &str, name: &str) -> Option<String> {
    let (key, name) = (wide(key), wide(name));
    let mut buffer = [0u16; 1024];
    let mut bytes = std::mem::size_of_val(&buffer) as u32;
    let status = unsafe {
        r::RegGetValueW(
            r::HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            r::RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    (status == 0).then(|| String::from_utf16_lossy(buffer.split(|&u| u == 0).next().unwrap_or_default()))
}

/// Only called with the two constant keys of BTM.
pub fn delete_user_key(key: &str) {
    if !key.ends_with("\\BetterTaskManager") {
        return;
    }
    let key = wide(key);
    unsafe { r::RegDeleteTreeW(r::HKEY_CURRENT_USER, key.as_ptr()) };
}

// ---------------------------------------------------------------- processes

/// Shows the UAC prompt; true when the program ran and reported success.
pub fn run_elevated(program: &Path, arguments: &str) -> bool {
    let (verb, file, args) = (wide("runas"), wide(&program.to_string_lossy()), wide(arguments));
    unsafe {
        let mut info: sh::SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<sh::SHELLEXECUTEINFOW>() as u32;
        info.fMask = sh::SEE_MASK_NOASYNC | sh::SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = args.as_ptr();
        info.nShow = wm::SW_SHOWNORMAL;
        if sh::ShellExecuteExW(&mut info) == 0 || info.hProcess.is_null() {
            return false;
        }
        th::WaitForSingleObject(info.hProcess, th::INFINITE);
        let mut code = 1u32;
        th::GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        code == 0
    }
}

pub fn wait_for(pid: u32) {
    unsafe {
        let process = th::OpenProcess(th::PROCESS_SYNCHRONIZE, 0, pid);
        if !process.is_null() {
            th::WaitForSingleObject(process, 30_000);
            CloseHandle(process);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn shortcut_is_written() {
        let dir = std::env::temp_dir();
        let link = dir.join(format!("btm-setup-test-{}.lnk", std::process::id()));
        super::create_shortcut(&link, &std::env::current_exe().unwrap(), &dir).unwrap();
        assert!(link.is_file());
        std::fs::remove_file(&link).unwrap();
    }
}
