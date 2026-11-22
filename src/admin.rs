//! Operations that need administrator rights.
//!
//! The window never runs elevated. It describes the work as an `AdminTask`, starts a
//! short-lived elevated BTM for exactly that task, and refreshes when that has exited.

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, FILETIME, HANDLE, HWND};
use windows_sys::Win32::System::Threading as th;
use windows_sys::Win32::UI::Shell as sh;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use crate::pages::{services, startup};
use crate::util::wide;
use crate::{helper, product, shell_hook};

/// A PID alone can be reused while the UAC prompt is open; with the creation time it cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessId {
    pub pid: u32,
    pub created: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdminTask {
    EndProcesses(Vec<ProcessId>),
    InstallHelper,
    RemoveHelper,
    TakeShortcut,
    RestoreShortcut,
    SetStartup {
        source: startup::Source,
        id: String,
        on: bool,
    },
    Service {
        name: String,
        action: services::Action,
    },
    /// Used by setup: bring the Program Files copy up to this version.
    Refresh,
    /// Everything machine-wide, used by the uninstaller.
    Cleanup,
}

impl AdminTask {
    pub fn parse(verb: &str, operands: &[String]) -> Option<Self> {
        Some(match verb {
            "end" => {
                let ids = operands
                    .iter()
                    .map(|op| {
                        let (pid, created) = op.split_once('@')?;
                        Some(ProcessId { pid: pid.parse().ok()?, created: created.parse().ok()? })
                    })
                    .collect::<Option<Vec<_>>>()?;
                if ids.is_empty() {
                    return None;
                }
                Self::EndProcesses(ids)
            }
            "install-helper" => Self::InstallHelper,
            "remove-helper" => Self::RemoveHelper,
            "take-shortcut" => Self::TakeShortcut,
            "restore-shortcut" => Self::RestoreShortcut,
            "startup" => match operands {
                [tag, id, state] => Self::SetStartup {
                    source: startup::Source::from_machine_tag(tag)?,
                    id: from_hex(id)?,
                    on: match state.as_str() {
                        "on" => true,
                        "off" => false,
                        _ => return None,
                    },
                },
                _ => return None,
            },
            "service" => match operands {
                [word, name] => Self::Service { action: services::Action::parse(word)?, name: from_hex(name)? },
                _ => return None,
            },
            "refresh" => Self::Refresh,
            "cleanup" => Self::Cleanup,
            _ => return None,
        })
    }

    fn command_line(&self) -> String {
        let rest = match self {
            Self::EndProcesses(ids) => {
                ids.iter().fold(String::from("end"), |line, id| format!("{line} {}@{}", id.pid, id.created))
            }
            Self::InstallHelper => "install-helper".into(),
            Self::RemoveHelper => "remove-helper".into(),
            Self::TakeShortcut => "take-shortcut".into(),
            Self::RestoreShortcut => "restore-shortcut".into(),
            // Names are hex encoded: they may hold spaces and quotes.
            Self::SetStartup { source, id, on } => format!(
                "startup {} {} {}",
                source.machine_tag().unwrap_or("none"),
                to_hex(id),
                if *on { "on" } else { "off" }
            ),
            Self::Service { name, action } => format!("service {} {}", action.word(), to_hex(name)),
            Self::Refresh => "refresh".into(),
            Self::Cleanup => "cleanup".into(),
        };
        format!("admin {rest}")
    }

    /// Runs inside the elevated process.
    pub fn execute(&self) -> i32 {
        let result = match self {
            Self::EndProcesses(ids) => end_all(ids),
            Self::InstallHelper => helper::install(),
            Self::RemoveHelper => helper::uninstall(),
            Self::TakeShortcut => shell_hook::enable(),
            Self::RestoreShortcut => shell_hook::disable(),
            Self::SetStartup { source, id, on } => startup::set_enabled(*source, id, *on)
                .map_err(|code| format!("The startup app could not be changed (error {code}).")),
            Self::Service { name, action } => services::control(name, *action)
                .map_err(|code| format!("{name} could not {} (error {code}).", action.word())),
            Self::Refresh => crate::protected::deploy().map(|_| ()),
            Self::Cleanup => shell_hook::disable().and(helper::uninstall()),
        };
        match result {
            Ok(()) => 0,
            Err(why) => {
                let (body, caption) = (wide(&why), wide(product::NAME));
                unsafe {
                    wm::MessageBoxW(std::ptr::null_mut(), body.as_ptr(), caption.as_ptr(), wm::MB_ICONWARNING);
                }
                1
            }
        }
    }

    /// Runs inside the window's process; `notify` receives `WM_APP_DONE` when finished.
    pub fn request(self, notify: HWND) {
        let target = notify as isize;
        let _ = std::thread::Builder::new().name("elevation".into()).spawn(move || {
            let window = target as HWND;
            if let Ok(program) = std::env::current_exe() {
                launch_and_wait(window, &program.to_string_lossy(), &self.command_line());
            }
            unsafe { wm::PostMessageW(window, crate::ui::WM_APP_DONE, 0, 0) };
        });
    }
}

fn to_hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

fn from_hex(hex: &str) -> Option<String> {
    if hex.is_empty() || !hex.len().is_multiple_of(2) || hex.len() > 1024 || !hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let bytes =
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect::<Option<_>>()?;
    String::from_utf8(bytes).ok().filter(|text: &String| !text.contains('\0'))
}

fn launch_and_wait(owner: HWND, program: &str, arguments: &str) {
    let strings = [wide("runas"), wide(program), wide(arguments)];
    let mut info = sh::SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<sh::SHELLEXECUTEINFOW>() as u32,
        // NOASYNC: this thread has no message loop.
        fMask: sh::SEE_MASK_NOASYNC | sh::SEE_MASK_NOCLOSEPROCESS,
        hwnd: owner,
        nShow: wm::SW_HIDE,
        lpVerb: strings[0].as_ptr(),
        lpFile: strings[1].as_ptr(),
        lpParameters: strings[2].as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    unsafe {
        let started = sh::ShellExecuteExW(&mut info) != 0;
        if started && !info.hProcess.is_null() {
            th::WaitForSingleObject(info.hProcess, th::INFINITE);
            CloseHandle(info.hProcess);
        }
    }
}

fn end_all(ids: &[ProcessId]) -> Result<(), String> {
    let survivors = ids.iter().filter(|id| end_process(**id).is_err()).count();
    match survivors {
        0 => Ok(()),
        n => Err(format!(
            "{n} of {} processes could not be ended. They are protected by Windows or have already exited.",
            ids.len()
        )),
    }
}

/// Opens the process only if it still is the one that was listed.
pub fn open_exact(id: ProcessId, access: u32) -> Result<HANDLE, u32> {
    const NOT_FOUND: u32 = 1168;
    unsafe {
        let process = th::OpenProcess(access | th::PROCESS_QUERY_LIMITED_INFORMATION, 0, id.pid);
        if process.is_null() {
            return Err(GetLastError());
        }
        let mut times = [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 4];
        let [created, exited, kernel, user] = &mut times;
        let known = th::GetProcessTimes(process, created, exited, kernel, user) != 0;
        let stamp = (u64::from(times[0].dwHighDateTime) << 32) + u64::from(times[0].dwLowDateTime);
        if known && stamp == id.created {
            return Ok(process);
        }
        CloseHandle(process);
        Err(NOT_FOUND)
    }
}

pub fn end_process(id: ProcessId) -> Result<(), u32> {
    let process = open_exact(id, th::PROCESS_TERMINATE)?;
    unsafe {
        let result = if th::TerminateProcess(process, 1) != 0 { Ok(()) } else { Err(GetLastError()) };
        CloseHandle(process);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(task: AdminTask) {
        let line = task.command_line();
        let words: Vec<String> = line.split(' ').skip(2).map(str::to_string).collect();
        assert_eq!(AdminTask::parse(line.split(' ').nth(1).unwrap(), &words), Some(task));
    }

    #[test]
    fn names_survive_the_command_line() {
        round_trip(AdminTask::SetStartup {
            source: startup::Source::MachineRun, id: "My \"App\" ü".into(), on: false
        });
        round_trip(AdminTask::Service { name: "AORUS LCD Panel Service".into(), action: services::Action::Restart });
    }

    #[test]
    fn elevated_startup_switch_is_machine_only() {
        let bad = ["user".to_string(), to_hex("x"), "on".to_string()];
        assert_eq!(AdminTask::parse("startup", &bad), None);
    }
}

#[cfg(test)]
mod process_tests {
    use super::*;
    use crate::nt::ProcBuf;
    use std::os::windows::process::CommandExt;

    /// Only ever ends the child that this test started itself.
    #[test]
    fn end_process_checks_the_creation_time() {
        let mut child = std::process::Command::new(format!(r"{}\PING.EXE", crate::shell::system_directory()))
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .creation_flags(th::CREATE_NO_WINDOW)
            .spawn()
            .expect("child");
        let mut buf = ProcBuf::new();
        assert!(buf.query(5));
        let listed = buf.iter(false).find(|p| p.pid() == child.id()).map(|p| p.create_time()).expect("listed");
        assert!(end_process(ProcessId { pid: child.id(), created: listed + 1 }).is_err());
        assert!(child.try_wait().unwrap().is_none(), "a reused PID must survive");
        assert!(end_process(ProcessId { pid: child.id(), created: listed }).is_ok());
        assert_eq!(child.wait().unwrap().code(), Some(1));
    }
}
