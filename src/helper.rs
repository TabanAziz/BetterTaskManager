// Optional elevated helper.
//
// Windows only reveals per-process *physical disk* bytes (and data for processes of
// other users, e.g. services) to elevated callers. Task Manager is auto-elevated by
// Windows; BTM is not. Instead of a UAC prompt on every start, the user can set
// up a scheduled task once (one UAC prompt). BTM then starts that task in the
// background; it runs `BTM.exe helper` elevated from Program Files (only
// admins can modify that copy) and writes read-only counters into shared memory.
//
// Security notes: the helper accepts no commands. Its only input is "a request event
// was signalled"; it never reads data from the shared section, only writes process
// counters into it, and exits when no BTM asks for data for 15 seconds.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows_sys::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ, FILE_MAP_WRITE,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows_sys::Win32::System::Registry::{RegDeleteTreeW, RegSetKeyValueW, HKEY_LOCAL_MACHINE, REG_SZ};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateProcessW, GetCurrentProcess, GetExitCodeProcess, OpenEventW, OpenProcess, OpenProcessToken,
    ResetEvent, SetEvent, WaitForSingleObject, CREATE_NO_WINDOW, EVENT_MODIFY_STATE, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
};

use crate::model::HelperState;
use crate::nt::{self, ProcBuf};
use crate::util::wide;

const SECTION: &str = "Local\\BetterTaskManager.ElevatedData.v1";
const EV_REQUEST: &str = "Local\\BetterTaskManager.ElevatedRequest.v1";
const EV_READY: &str = "Local\\BetterTaskManager.ElevatedReady.v1";
pub const TASK_NAME: &str = "Better Task Manager\\Elevated Data";
const REG_KEY: &str = crate::product::MACHINE_KEY;

const MAGIC: u32 = 0x4254_4D44; // "BTMD"
const FORMAT: u32 = 1;
const HEADER: usize = 64;
const ENTRY: usize = 120;
const MAX_ENTRIES: usize = 8192;
const SECTION_SIZE: usize = HEADER + ENTRY * MAX_ENTRIES;

pub fn build_id() -> u64 {
    env!("BTM_BUILD_ID").parse().unwrap_or(0)
}

#[derive(Clone, Default)]
pub struct Entry {
    pub disk_read: u64,
    pub disk_write: u64,
    pub net_in: u64,
    pub net_out: u64,
    pub sid: Vec<u8>,
}

pub struct Client {
    disabled: bool,
    section: HANDLE,
    view: *mut u8,
    req: HANDLE,
    ready: HANDLE,
    installed: Option<bool>,
    launched_qpc: Option<i64>,
    last_seq: u64,
    last_data_qpc: Option<i64>,
    pub helper_build: u64,
    pub map: HashMap<(u32, u64), Entry>,
}

unsafe impl Send for Client {}

/// Installed helper path from HKLM (written by the elevated installer).
pub fn installed_path() -> Option<String> {
    let p = crate::sysinfo::reg_string(HKEY_LOCAL_MACHINE, REG_KEY, "HelperPath")?;
    let w = wide(&p);
    if unsafe { GetFileAttributesW(w.as_ptr()) } == INVALID_FILE_ATTRIBUTES {
        return None;
    }
    Some(p)
}

impl Client {
    pub fn new(elevated: bool) -> Client {
        Client {
            disabled: elevated,
            section: std::ptr::null_mut(),
            view: std::ptr::null_mut(),
            req: std::ptr::null_mut(),
            ready: std::ptr::null_mut(),
            installed: None,
            launched_qpc: None,
            last_seq: 0,
            last_data_qpc: None,
            helper_build: 0,
            map: HashMap::new(),
        }
    }

    pub fn reset(&mut self) {
        self.installed = None;
        self.launched_qpc = None;
    }

    fn ensure_objects(&mut self) -> bool {
        if !self.view.is_null() {
            return true;
        }
        unsafe {
            let n = wide(SECTION);
            let s = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                SECTION_SIZE as u32,
                n.as_ptr(),
            );
            if s.is_null() {
                return false;
            }
            let v: MEMORY_MAPPED_VIEW_ADDRESS = MapViewOfFile(s, FILE_MAP_READ, 0, 0, SECTION_SIZE);
            if v.Value.is_null() {
                CloseHandle(s);
                return false;
            }
            let r = wide(EV_REQUEST);
            let d = wide(EV_READY);
            self.req = CreateEventW(std::ptr::null(), 0, 0, r.as_ptr());
            self.ready = CreateEventW(std::ptr::null(), 0, 0, d.as_ptr());
            self.section = s;
            self.view = v.Value as *mut u8;
        }
        true
    }

    pub fn state(&self) -> HelperState {
        if self.disabled {
            return HelperState::SelfElevated;
        }
        match self.installed {
            Some(true) => {
                if self.last_data_qpc.is_some() {
                    HelperState::Running
                } else {
                    HelperState::Starting
                }
            }
            _ => HelperState::NotInstalled,
        }
    }

    /// Start of a sampling round: ask the helper for fresh data (launching it if needed).
    pub fn request(&mut self) {
        if self.disabled {
            return;
        }
        if self.installed.is_none() {
            self.installed = Some(installed_path().is_some());
        }
        if self.installed != Some(true) || !self.ensure_objects() {
            return;
        }
        unsafe {
            ResetEvent(self.ready);
            SetEvent(self.req);
        }
        let now = crate::util::qpc_now();
        let stale = match self.last_data_qpc {
            Some(t) => crate::util::qpc_ms(t, now) > 5000.0,
            None => true,
        };
        let may_launch = match self.launched_qpc {
            Some(t) => crate::util::qpc_ms(t, now) > 20_000.0,
            None => true,
        };
        if stale && may_launch {
            self.launched_qpc = Some(now);
            launch_task();
        }
    }

    /// End of a sampling round: wait briefly for the helper and read its table.
    pub fn collect(&mut self) -> bool {
        if self.disabled || self.installed != Some(true) || self.view.is_null() {
            return false;
        }
        unsafe {
            if WaitForSingleObject(self.ready, 60) != WAIT_OBJECT_0 {
                // Keep the previous table for one round if the helper is just slow.
                let fresh = self
                    .last_data_qpc
                    .map(|t| crate::util::qpc_ms(t, crate::util::qpc_now()) < 2500.0)
                    .unwrap_or(false);
                return fresh && !self.map.is_empty();
            }
            let base = self.view;
            if std::ptr::read_volatile(base as *const u32) != MAGIC
                || std::ptr::read_volatile(base.add(4) as *const u32) != FORMAT
            {
                return false;
            }
            let seq_ptr = &*(base.add(8) as *const AtomicU64);
            for _ in 0..4 {
                let s1 = seq_ptr.load(Ordering::Acquire);
                if s1 & 1 == 1 {
                    std::thread::yield_now();
                    continue;
                }
                let count = (std::ptr::read_volatile(base.add(16) as *const u32) as usize).min(MAX_ENTRIES);
                let build = std::ptr::read_volatile(base.add(24) as *const u64);
                self.map.clear();
                for i in 0..count {
                    let e = base.add(HEADER + i * ENTRY);
                    let pid = std::ptr::read_volatile(e as *const u32);
                    let sid_len = (std::ptr::read_volatile(e.add(4) as *const u32) as usize).min(68);
                    let ctime = std::ptr::read_volatile(e.add(8) as *const u64);
                    let mut sid = vec![0u8; sid_len];
                    std::ptr::copy_nonoverlapping(e.add(48), sid.as_mut_ptr(), sid_len);
                    self.map.insert(
                        (pid, ctime),
                        Entry {
                            disk_read: std::ptr::read_volatile(e.add(16) as *const u64),
                            disk_write: std::ptr::read_volatile(e.add(24) as *const u64),
                            net_in: std::ptr::read_volatile(e.add(32) as *const u64),
                            net_out: std::ptr::read_volatile(e.add(40) as *const u64),
                            sid,
                        },
                    );
                }
                let s2 = seq_ptr.load(Ordering::Acquire);
                if s1 == s2 && s1 != self.last_seq {
                    self.last_seq = s1;
                    self.helper_build = build;
                    self.last_data_qpc = Some(crate::util::qpc_now());
                    return true;
                }
            }
            false
        }
    }
}

fn system_dir_exe(name: &str) -> String {
    format!("{}\\{}", crate::shell::system_directory(), name)
}

/// Run a console tool hidden, optionally waiting for its exit code.
pub fn run_hidden(exe: &str, args: &str, wait: bool) -> Option<u32> {
    let cmd = format!("\"{}\" {}", exe, args);
    let mut cmdw = wide(&cmd);
    let exew = wide(exe);
    unsafe {
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        if CreateProcessW(
            exew.as_ptr(),
            cmdw.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        ) == 0
        {
            return None;
        }
        CloseHandle(pi.hThread);
        let mut code = 0u32;
        if wait {
            WaitForSingleObject(pi.hProcess, 30_000);
            GetExitCodeProcess(pi.hProcess, &mut code);
        }
        CloseHandle(pi.hProcess);
        Some(code)
    }
}

fn launch_task() {
    run_hidden(&system_dir_exe("schtasks.exe"), &format!("/run /tn \"{}\"", TASK_NAME), false);
}

// ------------------------------------------------------------------ helper process

pub fn run_helper() {
    unsafe {
        let n = wide(SECTION);
        let s = OpenFileMappingW(FILE_MAP_WRITE | READ_CONTROL, 0, n.as_ptr());
        if s.is_null() {
            return;
        }
        let v = MapViewOfFile(s, FILE_MAP_WRITE, 0, 0, SECTION_SIZE);
        if v.Value.is_null() {
            return;
        }
        let base = v.Value as *mut u8;
        let r = wide(EV_REQUEST);
        let d = wide(EV_READY);
        let req = OpenEventW(0x0010_0000 | EVENT_MODIFY_STATE | READ_CONTROL, 0, r.as_ptr());
        let ready = OpenEventW(EVENT_MODIFY_STATE | READ_CONTROL, 0, d.as_ptr());
        if req.is_null() || ready.is_null() {
            return;
        }
        let Some(sid) = current_user_sid() else { return };
        // Local\\ names can be planted as links to objects of SYSTEM or of elevated programs.
        if ![s, req, ready].into_iter().all(|object| owned_by(object, &sid)) {
            return;
        }
        crate::pages::startup::publish_impacts(&sid);
        let mut buf = ProcBuf::new();
        let mut handles: HashMap<u32, (u64, HANDLE)> = HashMap::new();
        let seq = &*(base.add(8) as *const AtomicU64);
        std::ptr::write_volatile(base as *mut u32, MAGIC);
        std::ptr::write_volatile(base.add(4) as *mut u32, FORMAT);
        std::ptr::write_volatile(base.add(24) as *mut u64, build_id());
        std::ptr::write_volatile(base.add(32) as *mut u32, std::process::id());
        loop {
            if WaitForSingleObject(req, 15_000) != WAIT_OBJECT_0 {
                break; // nobody asked for 15 s: BTM is closed or paused
            }
            if !buf.query(nt::SYSTEM_FULL_PROCESS_INFORMATION_CLASS) {
                break; // not elevated after all
            }
            let s0 = seq.load(Ordering::Relaxed);
            seq.store(s0 | 1, Ordering::Release);
            let mut i = 0usize;
            let mut alive: Vec<u32> = Vec::with_capacity(1024);
            for p in buf.iter(true) {
                if i >= MAX_ENTRIES {
                    break;
                }
                let pid = p.pid();
                let ctime = p.create_time();
                alive.push(pid);
                let (dr, dw) = p.disk_bytes().unwrap_or((0, 0));
                let h = match handles.get(&pid) {
                    Some(&(c, h)) if c == ctime => h,
                    _ => {
                        if let Some((_, old)) = handles.remove(&pid) {
                            if !old.is_null() {
                                CloseHandle(old);
                            }
                        }
                        let h = if pid > 4 {
                            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
                        } else {
                            std::ptr::null_mut()
                        };
                        handles.insert(pid, (ctime, h));
                        h
                    }
                };
                let net = if h.is_null() { None } else { nt::process_net(h) };
                let e = base.add(HEADER + i * ENTRY);
                std::ptr::write_volatile(e as *mut u32, pid);
                let sid = p.user_sid().unwrap_or(&[]);
                let sl = sid.len().min(68);
                std::ptr::write_volatile(e.add(4) as *mut u32, sl as u32);
                std::ptr::write_volatile(e.add(8) as *mut u64, ctime);
                std::ptr::write_volatile(e.add(16) as *mut u64, dr);
                std::ptr::write_volatile(e.add(24) as *mut u64, dw);
                std::ptr::write_volatile(e.add(32) as *mut u64, net.map(|n| n.bytes_in).unwrap_or(0));
                std::ptr::write_volatile(e.add(40) as *mut u64, net.map(|n| n.bytes_out).unwrap_or(0));
                std::ptr::copy_nonoverlapping(sid.as_ptr(), e.add(48), sl);
                i += 1;
            }
            std::ptr::write_volatile(base.add(16) as *mut u32, i as u32);
            seq.store((s0 | 1) + 1, Ordering::Release);
            SetEvent(ready);
            // Drop handles of exited processes.
            if handles.len() > alive.len() + 64 {
                let set: std::collections::HashSet<u32> = alive.into_iter().collect();
                handles.retain(|pid, (_, h)| {
                    let keep = set.contains(pid);
                    if !keep && !h.is_null() {
                        CloseHandle(*h);
                    }
                    keep
                });
            }
        }
        UnmapViewOfFile(v);
        CloseHandle(s);
    }
}

// ------------------------------------------------------------------ install / uninstall (elevated)

const READ_CONTROL: u32 = 0x0002_0000;

unsafe fn owned_by(object: HANDLE, sid: &str) -> bool {
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;
    let mut owner = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    let none = std::ptr::null_mut();
    let status = GetSecurityInfo(
        object,
        SE_KERNEL_OBJECT,
        OWNER_SECURITY_INFORMATION,
        &mut owner,
        none,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut descriptor,
    );
    if status != 0 {
        return false;
    }
    let mut text: *mut u16 = std::ptr::null_mut();
    let same = ConvertSidToStringSidW(owner, &mut text) != 0 && crate::util::from_wide_ptr(text) == sid;
    windows_sys::Win32::Foundation::LocalFree(text as *mut c_void);
    windows_sys::Win32::Foundation::LocalFree(descriptor);
    same
}

fn current_user_sid() -> Option<String> {
    unsafe {
        let mut tok = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
            return None;
        }
        let mut buf = [0u64; 32];
        let mut ret = 0u32;
        let ok = GetTokenInformation(tok, TokenUser, buf.as_mut_ptr() as *mut c_void, 256, &mut ret) != 0;
        CloseHandle(tok);
        if !ok {
            return None;
        }
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(tu.User.Sid, &mut s) == 0 {
            return None;
        }
        let out = crate::util::from_wide_ptr(s);
        windows_sys::Win32::Foundation::LocalFree(s as *mut c_void);
        Some(out)
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Elevated: put a protected copy into Program Files and register the on-demand task.
pub fn install() -> Result<(), String> {
    let target = crate::protected::deploy()?;
    let dest = target.display().to_string();
    let sid = current_user_sid().ok_or("Your user account could not be determined.")?;
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.3" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Better Task Manager: provides read-only disk and network counters for all processes. Started on demand, exits by itself.</Description>
  </RegistrationInfo>
  <Principals>
    <Principal id="Author">
      <UserId>{sid}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>true</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{cmd}"</Command>
      <Arguments>helper</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        sid = xml_escape(&sid),
        cmd = xml_escape(&dest)
    );
    // schtasks wants UTF-16 with BOM. The file lives in the protected folder: in %TEMP%
    // any program of the user could swap it before schtasks reads it elevated.
    let xml_file = target.with_file_name("helper-task.xml");
    let bytes: Vec<u8> = std::iter::once(0xFEFFu16).chain(xml.encode_utf16()).flat_map(u16::to_le_bytes).collect();
    std::fs::write(&xml_file, bytes).map_err(|e| format!("The task definition could not be written: {e}"))?;
    let xml_path = xml_file.display().to_string();
    let code = run_hidden(
        &system_dir_exe("schtasks.exe"),
        &format!("/create /tn \"{}\" /xml \"{}\" /f", TASK_NAME, xml_path),
        true,
    );
    let _ = std::fs::remove_file(&xml_file);
    if code != Some(0) {
        return Err(format!("The scheduled task could not be created (code {:?}).", code));
    }
    unsafe {
        let k = wide(REG_KEY);
        let name = wide("HelperPath");
        let val = wide(&dest);
        let r = RegSetKeyValueW(
            HKEY_LOCAL_MACHINE,
            k.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            val.as_ptr() as *const c_void,
            (val.len() * 2) as u32,
        );
        if r != 0 {
            return Err("The registry entry could not be written.".into());
        }
    }
    Ok(())
}

/// Elevated: remove task, registry marker and, if unused, the protected copy.
pub fn uninstall() -> Result<(), String> {
    run_hidden(&system_dir_exe("schtasks.exe"), &format!("/delete /tn \"{}\" /f", TASK_NAME), true);
    let key = wide(REG_KEY);
    unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, key.as_ptr()) };
    crate::protected::remove_if_unused();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_accepts_only_objects_of_the_user() {
        unsafe {
            let name = wide(&format!(r"Local\BetterTaskManager.Test.{}", std::process::id()));
            let none = std::ptr::null();
            let created = CreateFileMappingW(INVALID_HANDLE_VALUE, none, PAGE_READWRITE, 0, 4096, name.as_ptr());
            let opened = OpenFileMappingW(FILE_MAP_WRITE | READ_CONTROL, 0, name.as_ptr());
            assert!(owned_by(opened, &current_user_sid().expect("sid")));
            assert!(!owned_by(opened, "S-1-5-18"));
            let view = MapViewOfFile(opened, FILE_MAP_WRITE, 0, 0, 4096);
            assert!(!view.Value.is_null());
            UnmapViewOfFile(view);
            CloseHandle(opened);
            CloseHandle(created);
        }
    }
}
