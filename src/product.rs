//! Every name under which Better Task Manager appears on a system.

pub const NAME: &str = "Better Task Manager";
pub const EXE: &str = "BTM.exe";

/// Per-user settings (HKCU) and machine-wide markers (HKLM).
pub const SETTINGS_KEY: &str = "Software\\BetterTaskManager";
pub const MACHINE_KEY: &str = "SOFTWARE\\BetterTaskManager";
/// Folder below %LOCALAPPDATA% for caches.
pub const DATA_DIR: &str = "BetterTaskManager";

pub const WINDOW_CLASS: &str = "BetterTaskManager.Window";
pub const OFFSCREEN_CLASS: &str = "BetterTaskManager.Offscreen";

/// Where Windows looks for a replacement of taskmgr.exe. Setup uses this file too.
pub fn task_manager_options_key() -> String {
    ["SOFTWARE", "Microsoft", "Windows NT", "CurrentVersion", "Image File Execution Options", "taskmgr.exe"].join("\\")
}
