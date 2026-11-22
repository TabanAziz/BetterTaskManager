//! Services: list, start, stop, restart.

use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Services as sc;

use crate::util::{from_wide_ptr, wide};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Stopped,
    Starting,
    Stopping,
    Running,
    Paused,
}

impl State {
    fn from_code(code: u32) -> State {
        match code {
            sc::SERVICE_RUNNING => State::Running,
            sc::SERVICE_START_PENDING | sc::SERVICE_CONTINUE_PENDING => State::Starting,
            sc::SERVICE_STOP_PENDING => State::Stopping,
            sc::SERVICE_PAUSE_PENDING | sc::SERVICE_PAUSED => State::Paused,
            _ => State::Stopped,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            State::Stopped => "Stopped",
            State::Starting => "Starting",
            State::Stopping => "Stopping",
            State::Running => "Running",
            State::Paused => "Paused",
        }
    }
}

#[derive(Clone)]
pub struct Service {
    pub name: String,
    pub display: String,
    pub pid: u32,
    pub state: State,
    pub can_stop: bool,
    /// svchost group, as in Task Manager's "Group" column.
    pub group: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
}

impl Action {
    pub fn word(self) -> &'static str {
        match self {
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
        }
    }

    pub fn parse(word: &str) -> Option<Action> {
        [Action::Start, Action::Stop, Action::Restart].into_iter().find(|a| a.word() == word)
    }
}

struct Handle(sc::SC_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { sc::CloseServiceHandle(self.0) };
    }
}

fn manager(access: u32) -> Result<Handle, u32> {
    unsafe {
        let handle = sc::OpenSCManagerW(std::ptr::null(), std::ptr::null(), access);
        if handle.is_null() {
            Err(GetLastError())
        } else {
            Ok(Handle(handle))
        }
    }
}

pub fn load() -> Result<Vec<Service>, String> {
    let manager = manager(sc::SC_MANAGER_ENUMERATE_SERVICE)
        .map_err(|code| format!("The service list is not available (error {code})."))?;
    let mut list = Vec::new();
    let mut resume = 0u32;
    // u64 for the alignment of the records at the front of the buffer.
    let mut buffer = vec![0u64; 32 * 1024];
    loop {
        let (mut needed, mut count) = (0u32, 0u32);
        let ok = unsafe {
            sc::EnumServicesStatusExW(
                manager.0,
                sc::SC_ENUM_PROCESS_INFO,
                sc::SERVICE_WIN32,
                sc::SERVICE_STATE_ALL,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * 8) as u32,
                &mut needed,
                &mut count,
                &mut resume,
                std::ptr::null(),
            ) != 0
        };
        const MORE_DATA: u32 = 234;
        let error = if ok { 0 } else { unsafe { GetLastError() } };
        if !ok && error != MORE_DATA {
            return Err(format!("The service list could not be read (error {error})."));
        }
        let records = unsafe {
            std::slice::from_raw_parts(buffer.as_ptr() as *const sc::ENUM_SERVICE_STATUS_PROCESSW, count as usize)
        };
        for record in records {
            let name = unsafe { from_wide_ptr(record.lpServiceName) };
            let status = &record.ServiceStatusProcess;
            let key = format!("SYSTEM\\CurrentControlSet\\Services\\{name}");
            let image = crate::reg::string(crate::reg::MACHINE, &key, "ImagePath").unwrap_or_default();
            list.push(Service {
                group: host_group(&image),
                display: unsafe { from_wide_ptr(record.lpDisplayName) },
                pid: status.dwProcessId,
                state: State::from_code(status.dwCurrentState),
                can_stop: status.dwControlsAccepted & sc::SERVICE_ACCEPT_STOP != 0,
                name,
            });
        }
        if ok {
            return Ok(list);
        }
        if count == 0 {
            buffer.resize(buffer.len() * 2, 0);
        }
    }
}

/// "...\svchost.exe -k netsvcs -p" → "netsvcs".
fn host_group(image: &str) -> String {
    let mut words = image.split_whitespace();
    if words.any(|word| word.eq_ignore_ascii_case("-k")) {
        words.next().unwrap_or_default().to_string()
    } else {
        String::new()
    }
}

/// Error 5 means the caller has to be elevated.
pub fn control(name: &str, action: Action) -> Result<(), u32> {
    let manager = manager(sc::SC_MANAGER_CONNECT)?;
    match action {
        Action::Start => start(&manager, name),
        Action::Stop => stop(&manager, name).map(|_| ()),
        Action::Restart => {
            let stopped = stop(&manager, name)?;
            start(&manager, name)?;
            // Dependents that were stopped for it come back, in reverse order.
            for dependent in stopped.iter().rev() {
                start(&manager, dependent)?;
            }
            Ok(())
        }
    }
}

fn open(manager: &Handle, name: &str, access: u32) -> Result<Handle, u32> {
    let name = wide(name);
    unsafe {
        let handle = sc::OpenServiceW(manager.0, name.as_ptr(), access);
        if handle.is_null() {
            Err(GetLastError())
        } else {
            Ok(Handle(handle))
        }
    }
}

fn start(manager: &Handle, name: &str) -> Result<(), u32> {
    const ALREADY_RUNNING: u32 = 1056;
    let service = open(manager, name, sc::SERVICE_START)?;
    unsafe {
        if sc::StartServiceW(service.0, 0, std::ptr::null()) != 0 {
            return Ok(());
        }
        match GetLastError() {
            ALREADY_RUNNING => Ok(()),
            code => Err(code),
        }
    }
}

/// Stops the active dependents first and returns their names.
fn stop(manager: &Handle, name: &str) -> Result<Vec<String>, u32> {
    const NOT_ACTIVE: u32 = 1062;
    let access = sc::SERVICE_STOP | sc::SERVICE_QUERY_STATUS | sc::SERVICE_ENUMERATE_DEPENDENTS;
    let service = open(manager, name, access)?;
    let dependents = active_dependents(&service);
    for dependent in &dependents {
        let other = open(manager, dependent, sc::SERVICE_STOP | sc::SERVICE_QUERY_STATUS)?;
        stop_one(&other).or_else(|code| if code == NOT_ACTIVE { Ok(()) } else { Err(code) })?;
    }
    stop_one(&service).or_else(|code| if code == NOT_ACTIVE { Ok(()) } else { Err(code) })?;
    Ok(dependents)
}

fn active_dependents(service: &Handle) -> Vec<String> {
    unsafe {
        let (mut needed, mut count) = (0u32, 0u32);
        sc::EnumDependentServicesW(service.0, sc::SERVICE_ACTIVE, std::ptr::null_mut(), 0, &mut needed, &mut count);
        if needed == 0 {
            return Vec::new();
        }
        let mut buffer = vec![0u64; needed.div_ceil(8) as usize];
        let records = buffer.as_mut_ptr() as *mut sc::ENUM_SERVICE_STATUSW;
        let size = (buffer.len() * 8) as u32;
        if sc::EnumDependentServicesW(service.0, sc::SERVICE_ACTIVE, records, size, &mut needed, &mut count) == 0 {
            return Vec::new();
        }
        (0..count as usize).map(|i| from_wide_ptr((*records.add(i)).lpServiceName)).collect()
    }
}

fn stop_one(service: &Handle) -> Result<(), u32> {
    const TIMEOUT: u32 = 1053;
    unsafe {
        let mut status: sc::SERVICE_STATUS = std::mem::zeroed();
        if sc::ControlService(service.0, sc::SERVICE_CONTROL_STOP, &mut status) == 0 {
            return Err(GetLastError());
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while status.dwCurrentState != sc::SERVICE_STOPPED {
            if Instant::now() > deadline {
                return Err(TIMEOUT);
            }
            std::thread::sleep(Duration::from_millis(150));
            if sc::QueryServiceStatus(service.0, &mut status) == 0 {
                return Err(GetLastError());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn svchost_group() {
        assert_eq!(super::host_group("C:\\Windows\\system32\\svchost.exe -k netsvcs -p"), "netsvcs");
        assert_eq!(super::host_group("\"C:\\A\\agent.exe\" --service"), "");
    }
}
