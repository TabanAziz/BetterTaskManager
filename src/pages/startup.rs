//! Startup apps: Run keys, Startup folders and packaged startup tasks.
//!
//! Enabling and disabling only writes Explorer's "StartupApproved" state (the switch
//! Task Manager and Settings use); the entries themselves are never edited or deleted.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::Com as com;
use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows_sys::Win32::UI::Shell as sh;

use crate::meta::{self, Meta};
use crate::reg::{self, HKEY};
use crate::util::{from_wide, from_wide_ptr, wide};

const CURRENT_VERSION: &str = "Software\\Microsoft\\Windows\\CurrentVersion";
const CURRENT_VERSION_32: &str = "Software\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion";
const APP_DATA: &str =
    "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\SystemAppData";
const IMPACT_VALUE: &str = "StartupImpact";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    UserRun,
    MachineRun,
    MachineRun32,
    UserFolder,
    MachineFolder,
    Packaged,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::UserRun | Source::MachineRun | Source::MachineRun32 => "Registry",
            Source::UserFolder | Source::MachineFolder => "Startup folder",
            Source::Packaged => "Packaged app",
        }
    }

    /// Sources that only an administrator can switch, with their command-line tag.
    pub fn machine_tag(self) -> Option<&'static str> {
        match self {
            Source::MachineRun => Some("run"),
            Source::MachineRun32 => Some("run32"),
            Source::MachineFolder => Some("folder"),
            _ => None,
        }
    }

    pub fn from_machine_tag(tag: &str) -> Option<Source> {
        [Source::MachineRun, Source::MachineRun32, Source::MachineFolder]
            .into_iter()
            .find(|s| s.machine_tag() == Some(tag))
    }

    fn approval(self) -> Option<(HKEY, String)> {
        let (hive, leaf) = match self {
            Source::UserRun => (reg::USER, "Run"),
            Source::MachineRun => (reg::MACHINE, "Run"),
            Source::MachineRun32 => (reg::MACHINE, "Run32"),
            Source::UserFolder => (reg::USER, "StartupFolder"),
            Source::MachineFolder => (reg::MACHINE, "StartupFolder"),
            Source::Packaged => return None,
        };
        Some((hive, format!("{CURRENT_VERSION}\\Explorer\\StartupApproved\\{leaf}")))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Impact {
    NotMeasured,
    Low,
    Medium,
    High,
}

impl Impact {
    pub fn label(self) -> &'static str {
        match self {
            Impact::NotMeasured => "Not measured",
            Impact::Low => "Low",
            Impact::Medium => "Medium",
            Impact::High => "High",
        }
    }

    /// Windows' documented limits: High above 1 s CPU or 3 MB disk, Low below 300 ms and 300 KB.
    fn classify(cpu_us: u64, disk_bytes: u64) -> Impact {
        if cpu_us > 1_000_000 || disk_bytes > 3 * 1024 * 1024 {
            Impact::High
        } else if cpu_us >= 300_000 || disk_bytes >= 300 * 1024 {
            Impact::Medium
        } else {
            Impact::Low
        }
    }
}

#[derive(Clone)]
pub struct Entry {
    pub source: Source,
    /// Value name, file name or "family\task": identifies the entry within its source.
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub command: String,
    pub meta: Option<Arc<Meta>>,
    pub enabled: bool,
    pub impact: Impact,
}

// ---------------------------------------------------------------- reading

pub fn load() -> Vec<Entry> {
    let com_ready = unsafe { com::CoInitializeEx(std::ptr::null(), com::COINIT_APARTMENTTHREADED as u32) } >= 0;
    let impacts = read_impacts();
    let mut entries = Vec::new();

    let keys = [
        (Source::UserRun, reg::USER, CURRENT_VERSION),
        (Source::MachineRun, reg::MACHINE, CURRENT_VERSION),
        (Source::MachineRun32, reg::MACHINE, CURRENT_VERSION_32),
    ];
    for (source, hive, root) in keys {
        let approved = approvals(source);
        for value in reg::values(hive, &format!("{root}\\Run")) {
            let Some(command) = value.text() else { continue };
            if value.name.is_empty() || command.trim().is_empty() {
                continue;
            }
            let path = executable_of(&expand(&command));
            entries.push(describe(source, value.name, command, path, &approved, &impacts));
        }
    }

    let folders = [(Source::UserFolder, sh::FOLDERID_Startup), (Source::MachineFolder, sh::FOLDERID_CommonStartup)];
    for (source, id) in folders {
        let Some(folder) = known_folder(&id) else { continue };
        let Ok(files) = std::fs::read_dir(&folder) else { continue };
        let approved = approvals(source);
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if name.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            let full = file.path().to_string_lossy().into_owned();
            let target = if name.to_lowercase().ends_with(".lnk") { link_target(&full) } else { None };
            let path = target.unwrap_or_else(|| full.clone());
            entries.push(describe(source, name, full, path, &approved, &impacts));
        }
    }

    packaged(&mut entries);
    if com_ready {
        unsafe { com::CoUninitialize() };
    }
    entries
}

fn approvals(source: Source) -> HashMap<String, bool> {
    let Some((hive, key)) = source.approval() else { return HashMap::new() };
    reg::values(hive, &key).into_iter().map(|v| (v.name.to_lowercase(), is_enabled(&v.data))).collect()
}

/// The first byte is even while enabled (02, 06) and odd while disabled (01, 03).
fn is_enabled(state: &[u8]) -> bool {
    state.first().is_none_or(|b| b & 1 == 0)
}

fn describe(
    source: Source,
    id: String,
    command: String,
    path: String,
    approved: &HashMap<String, bool>,
    impacts: &HashMap<String, Impact>,
) -> Entry {
    let info = (!path.is_empty()).then(|| meta::get(&path));
    let file = super::file_stem(&path).to_lowercase();
    // Launchers describe themselves, not the app they start.
    let launcher = ["rundll32", "cmd", "powershell", "pwsh", "wscript", "cscript", "update", "explorer", "conhost"]
        .contains(&file.as_str());
    let fallback = match source {
        Source::UserFolder | Source::MachineFolder => super::file_stem(&id),
        _ => id.clone(),
    };
    let name = match &info {
        Some(m) if !launcher && !m.description.is_empty() => m.description.clone(),
        _ => fallback,
    };
    Entry {
        enabled: approved.get(&id.to_lowercase()).copied().unwrap_or(true),
        impact: impacts.get(&path.to_lowercase()).copied().unwrap_or(Impact::NotMeasured),
        publisher: info.as_ref().map(|m| m.company.clone()).unwrap_or_default(),
        source,
        id,
        name,
        command,
        meta: info,
    }
}

fn packaged(entries: &mut Vec<Entry>) {
    let mut packages = None;
    for family in reg::subkeys(reg::USER, APP_DATA) {
        for task in reg::subkeys(reg::USER, &format!("{APP_DATA}\\{family}")) {
            let values = reg::values(reg::USER, &format!("{APP_DATA}\\{family}\\{task}"));
            let find = |name: &str| values.iter().find(|v| v.name.eq_ignore_ascii_case(name));
            let Some(state) = find("State").and_then(|v| v.dword()) else { continue };
            if find("LastDisabledTime").is_none() && find("UserEnabledStartupOnce").is_none() {
                continue;
            }
            let known = packages.get_or_insert_with(super::packages::Packages::scan);
            if !known.knows(&family) {
                continue; // uninstalled, only the state is left
            }
            let package = known.describe(&family);
            entries.push(Entry {
                source: Source::Packaged,
                id: format!("{family}\\{task}"),
                name: package.name,
                publisher: package.publisher,
                command: format!("{family} ({task})"),
                meta: None,
                enabled: state == 2 || state == 4,
                impact: Impact::NotMeasured,
            });
        }
    }
}

fn expand(text: &str) -> String {
    let source = wide(text);
    let mut out = vec![0u16; 4096];
    let n = unsafe { ExpandEnvironmentStringsW(source.as_ptr(), out.as_mut_ptr(), out.len() as u32) } as usize;
    if n == 0 || n > out.len() {
        text.to_string()
    } else {
        from_wide(&out[..n])
    }
}

/// The program part of a command line. Unquoted paths may contain spaces.
fn executable_of(command: &str) -> String {
    let command = command.trim();
    if let Some(rest) = command.strip_prefix('"') {
        return rest.split('"').next().unwrap_or_default().to_string();
    }
    let lower = command.to_ascii_lowercase();
    for extension in [".exe", ".bat", ".cmd", ".com"] {
        let mut from = 0;
        while let Some(found) = lower[from..].find(extension) {
            let end = from + found + extension.len();
            if matches!(lower.as_bytes().get(end), None | Some(b' ' | b'\t' | b',')) {
                return command[..end].to_string();
            }
            from = end;
        }
    }
    command.split_whitespace().next().unwrap_or_default().to_string()
}

fn known_folder(id: &GUID) -> Option<String> {
    unsafe {
        let mut raw = std::ptr::null_mut();
        let hr = sh::SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut raw);
        let path = (hr >= 0).then(|| from_wide_ptr(raw));
        com::CoTaskMemFree(raw as *const c_void);
        path
    }
}

fn link_target(link: &str) -> Option<String> {
    const SHELL_LINK: GUID = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
    const ISHELL_LINK_W: GUID = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
    const IPERSIST_FILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);
    type Raw = *mut c_void;
    type Query = unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> i32;
    type Release = unsafe extern "system" fn(Raw) -> u32;
    type Load = unsafe extern "system" fn(Raw, *const u16, u32) -> i32;
    type GetPath = unsafe extern "system" fn(Raw, *mut u16, i32, *mut c_void, u32) -> i32;
    unsafe fn method<F: Copy>(object: Raw, slot: usize) -> F {
        std::mem::transmute_copy(&*(*(object as *const *const Raw)).add(slot))
    }
    unsafe {
        let mut shell_link: Raw = std::ptr::null_mut();
        let context = com::CLSCTX_INPROC_SERVER;
        if com::CoCreateInstance(&SHELL_LINK, std::ptr::null_mut(), context, &ISHELL_LINK_W, &mut shell_link) < 0 {
            return None;
        }
        let mut target = None;
        let mut file: Raw = std::ptr::null_mut();
        if method::<Query>(shell_link, 0)(shell_link, &IPERSIST_FILE, &mut file) >= 0 {
            let path = wide(link);
            if method::<Load>(file, 5)(file, path.as_ptr(), 0) >= 0 {
                let mut out = [0u16; 1024];
                let hr = method::<GetPath>(shell_link, 3)(shell_link, out.as_mut_ptr(), 1024, std::ptr::null_mut(), 0);
                if hr >= 0 && out[0] != 0 {
                    target = Some(expand(&from_wide(&out)));
                }
            }
            method::<Release>(file, 2)(file);
        }
        method::<Release>(shell_link, 2)(shell_link);
        target
    }
}

// ---------------------------------------------------------------- switching

/// Machine sources need an elevated caller (error 5 otherwise).
pub fn set_enabled(source: Source, id: &str, on: bool) -> Result<(), u32> {
    let Some((hive, key)) = source.approval() else {
        if id.split('\\').count() != 2 {
            return Err(87); // ERROR_INVALID_PARAMETER
        }
        let key = format!("{APP_DATA}\\{id}");
        // StartupTaskState: 1 = disabled by the user, 2 = enabled.
        return reg::set_dword(reg::USER, &key, "State", if on { 2 } else { 1 });
    };
    reg::set_binary(hive, &key, id, &approval_bytes(on, now()))
}

fn approval_bytes(on: bool, when: u64) -> [u8; 12] {
    let mut state = [0u8; 12];
    state[0] = if on { 2 } else { 3 };
    if !on {
        state[4..].copy_from_slice(&when.to_le_bytes());
    }
    state
}

fn now() -> u64 {
    let mut t = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    unsafe { GetSystemTimeAsFileTime(&mut t) };
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

// ---------------------------------------------------------------- startup impact

fn read_impacts() -> HashMap<String, Impact> {
    let Some(text) = reg::string(reg::MACHINE, crate::product::MACHINE_KEY, IMPACT_VALUE) else {
        return HashMap::new();
    };
    text.split('|')
        .filter_map(|item| {
            let (path, level) = item.rsplit_once('=')?;
            let impact = match level {
                "H" => Impact::High,
                "M" => Impact::Medium,
                "L" => Impact::Low,
                _ => return None,
            };
            Some((path.to_string(), impact))
        })
        .collect()
}

/// Elevated helper: Windows keeps its boot measurements in a folder only administrators
/// can read. Publish just the resulting level per program where everyone can read it.
pub fn publish_impacts(user_sid: &str) {
    let mut system = [0u16; 260];
    let n = unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(system.as_mut_ptr(), system.len() as u32)
    } as usize;
    let folder = format!("{}\\wdi\\LogFiles\\StartupInfo", from_wide(&system[..n.min(system.len())]));
    let Ok(files) = std::fs::read_dir(folder) else { return };
    let newest = files
        .flatten()
        .filter(|f| {
            let name = f.file_name().to_string_lossy().to_string();
            name.starts_with(user_sid) && name.to_lowercase().ends_with(".xml")
        })
        .max_by_key(|f| f.metadata().and_then(|m| m.modified()).ok());
    let Some(bytes) = newest.and_then(|f| std::fs::read(f.path()).ok()) else { return };
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let list: Vec<String> = parse_startup_info(&text)
        .into_iter()
        .map(|(path, impact)| format!("{path}={}", &impact.label()[..1]))
        .collect();
    let _ = reg::set_string(reg::MACHINE, crate::product::MACHINE_KEY, IMPACT_VALUE, &list.join("|"));
}

fn parse_startup_info(xml: &str) -> HashMap<String, Impact> {
    let number = |block: &str, tag: &str| -> u64 {
        let Some(at) = block.find(tag) else { return 0 };
        let Some(open) = block[at..].find('>') else { return 0 };
        block[at + open + 1..].split('<').next().and_then(|n| n.trim().parse().ok()).unwrap_or(0)
    };
    let mut totals: HashMap<String, (u64, u64)> = HashMap::new();
    for block in xml.split("<Process ").skip(1) {
        let block = block.split("</Process>").next().unwrap_or(block);
        let Some(path) = block.find("Name=\"").and_then(|at| block[at + 6..].split('"').next()) else { continue };
        let total = totals.entry(path.replace("&amp;", "&").to_lowercase()).or_default();
        total.0 += number(block, "<CpuUsage");
        total.1 += number(block, "<DiskUsage");
    }
    totals.into_iter().map(|(path, (cpu, disk))| (path, Impact::classify(cpu, disk))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_from_command_line() {
        assert_eq!(executable_of("\"C:\\A B\\x.exe\" --min"), "C:\\A B\\x.exe");
        assert_eq!(executable_of("C:\\Program Files\\A\\a.exe /tray"), "C:\\Program Files\\A\\a.exe");
        assert_eq!(executable_of("C:\\my.exe.d\\run.exe"), "C:\\my.exe.d\\run.exe");
        assert_eq!(executable_of("tool --x"), "tool");
    }

    #[test]
    fn approval_state_matches_explorer() {
        assert!(is_enabled(&approval_bytes(true, 77)));
        assert!(!is_enabled(&approval_bytes(false, 77)));
        assert_eq!(approval_bytes(false, 77)[4], 77);
        assert!(is_enabled(&[6, 0, 0]) && is_enabled(&[]));
    }

    #[test]
    fn impact_from_boot_log() {
        let xml = r#"<StartupData><Process Name="C:\A\Big.exe" PID="4"><DiskUsage Units="bytes">9000000</DiskUsage><CpuUsage Units="us">5</CpuUsage></Process><Process Name="C:\s.exe" PID="9"><DiskUsage Units="bytes">10</DiskUsage><CpuUsage Units="us">10</CpuUsage></Process></StartupData>"#;
        let found = parse_startup_info(xml);
        assert_eq!(found["c:\\a\\big.exe"], Impact::High);
        assert_eq!(found["c:\\s.exe"], Impact::Low);
    }
}
