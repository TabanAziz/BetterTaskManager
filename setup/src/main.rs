//! Better Task Manager setup.
//!
//! Installs for the current user, needs no administrator rights and never changes
//! anything before the person confirmed the dialog. There is no silent mode.
#![windows_subsystem = "windows"]

mod files;
// One source for every name that setup and app must agree on.
#[allow(dead_code)]
#[path = "../../src/product.rs"]
mod product;
mod win;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use win::{Answer, Dialog};

const NAME: &str = "Better Task Manager";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const HOMEPAGE: &str = "https://github.com/TabanAziz/BetterTaskManager";
const UNINSTALL_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\BetterTaskManager";

const APP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/BTM.exe"));
const LICENSE: &[u8] = include_bytes!("../../LICENSE");
const NOTICES: &[u8] = include_bytes!("../../THIRD-PARTY-LICENSES");
const LICENSE_FILE: &str = "LICENSE.txt";
const NOTICES_FILE: &str = "THIRD-PARTY-LICENSES.txt";

fn main() -> ExitCode {
    win::harden_dll_search();
    let arguments: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let words: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let result = match words.as_slice() {
        [] => install(),
        ["uninstall"] => uninstall(),
        // Second stage: a copy in %TEMP%, because a running program cannot delete itself.
        ["finish-uninstall", parent, settings] => finish_uninstall(parent, *settings == "delete-settings"),
        // For the release script: can the dialog be shown on this Windows? Shows nothing.
        ["self-check"] => return if win::dialogs_available() { ExitCode::SUCCESS } else { ExitCode::FAILURE },
        _ => return ExitCode::from(2),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            win::show(&Dialog { heading: "That did not work", text: &why, button: None, checkbox: None });
            ExitCode::FAILURE
        }
    }
}

fn install_dir() -> Result<PathBuf, String> {
    let programs = win::user_programs().ok_or("Your Programs folder could not be found.")?;
    Ok(programs.join(NAME))
}

/// Features that live machine-wide because they were switched on with administrator rights.
fn machine_features() -> bool {
    let helper = win::machine_value(product::MACHINE_KEY, "HelperPath").is_some();
    let shortcut = win::machine_value(&product::task_manager_options_key(), "Debugger")
        .is_some_and(|value| value.contains("\\Better Task Manager\\BTM.exe"));
    helper || shortcut
}

fn install() -> Result<(), String> {
    let dir = install_dir()?;
    let update = dir.join(files::APP).is_file();
    let elevated_parts = machine_features();

    let heading = format!("{} {NAME} {VERSION}", if update { "Update to" } else { "Install" });
    let mut text = format!(
        "Installs for your user account only, no administrator rights needed.\n\nFolder: {}\n\n\
         BTM appears in the Start menu and can be removed in Settings > Apps.",
        dir.display()
    );
    if elevated_parts {
        text.push_str(
            "\n\nYou use the elevated helper or the Ctrl+Shift+Esc shortcut. Windows will ask once for permission \
             to update their protected copy.",
        );
    }
    let button = if update { "Update" } else { "Install" };
    let checkbox = Some(("Create a desktop shortcut", !update));
    let answer = win::show(&Dialog { heading: &heading, text: &text, button: Some(button), checkbox });
    let Answer::Confirmed { checked } = answer else { return Ok(()) };

    let failed = |what: &str, error: std::io::Error| format!("{what}: {error}");
    let me = std::env::current_exe().and_then(std::fs::read).map_err(|e| failed("Setup could not read itself", e))?;
    let payload = files::Payload { app: APP, setup: &me, texts: &[(LICENSE_FILE, LICENSE), (NOTICES_FILE, NOTICES)] };
    files::put(&dir, &payload).map_err(|e| failed("The files could not be copied", e))?;

    let app = dir.join(files::APP);
    register(&dir, &app, APP.len() + me.len())?;
    if let Some(menu) = win::start_menu() {
        win::create_shortcut(&menu.join(format!("{NAME}.lnk")), &app, &dir)?;
    }
    if checked {
        if let Some(desktop) = win::desktop() {
            win::create_shortcut(&desktop.join(format!("{NAME}.lnk")), &app, &dir)?;
        }
    }
    let mut done = "Start it from the Start menu. To make Ctrl+Shift+Esc open BTM or to fill the per-process Disk \
                    column, open Settings inside BTM."
        .to_string();
    if elevated_parts && !win::run_elevated(&app, "admin refresh") {
        done = "BTM was installed, but its protected copy was not updated. Switch the helper or the shortcut off and \
                on again in BTM's Settings."
            .to_string();
    }
    let finished =
        Dialog { heading: "Better Task Manager is ready", text: &done, button: Some("Start now"), checkbox: None };
    if let Answer::Confirmed { .. } = win::show(&finished) {
        let _ = std::process::Command::new(&app).current_dir(&dir).spawn();
    }
    Ok(())
}

fn register(dir: &Path, app: &Path, bytes: usize) -> Result<(), String> {
    let path = |p: &Path| p.display().to_string();
    let uninstall = format!("\"{}\" uninstall", path(&dir.join(files::UNINSTALLER)));
    let texts = [
        ("DisplayName", NAME.to_string()),
        ("DisplayVersion", VERSION.to_string()),
        ("Publisher", "TabanAziz".to_string()),
        ("DisplayIcon", path(app)),
        ("InstallLocation", path(dir)),
        ("UninstallString", uninstall),
        ("URLInfoAbout", HOMEPAGE.to_string()),
    ];
    for (name, value) in &texts {
        win::set_text(UNINSTALL_KEY, name, value)?;
    }
    win::set_number(UNINSTALL_KEY, "EstimatedSize", (bytes / 1024) as u32)?;
    win::set_number(UNINSTALL_KEY, "NoModify", 1)?;
    win::set_number(UNINSTALL_KEY, "NoRepair", 1)
}

fn uninstall() -> Result<(), String> {
    let elevated_parts = machine_features();
    let mut text = "Removes the program and its shortcuts.".to_string();
    if elevated_parts {
        text.push_str(
            "\n\nWindows will ask for permission to give Ctrl+Shift+Esc back to the Windows Task Manager and to \
             remove the elevated helper.",
        );
    }
    let question = Dialog {
        heading: "Uninstall Better Task Manager?",
        text: &text,
        button: Some("Uninstall"),
        checkbox: Some(("Also delete my BTM settings", false)),
    };
    let Answer::Confirmed { checked } = win::show(&question) else { return Ok(()) };

    let app = install_dir()?.join(files::APP);
    if elevated_parts && !win::run_elevated(&app, "admin cleanup") {
        return Err("Administrator permission was not given, so nothing was removed.".into());
    }
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let copy = std::env::temp_dir().join(format!("BTM-uninstall-{}.exe", std::process::id()));
    std::fs::copy(&me, &copy).map_err(|e| format!("The uninstaller could not be prepared: {e}"))?;
    std::process::Command::new(&copy)
        .arg("finish-uninstall")
        .arg(std::process::id().to_string())
        .arg(if checked { "delete-settings" } else { "keep-settings" })
        .current_dir(std::env::temp_dir())
        .spawn()
        .map_err(|e| format!("The uninstaller could not be started: {e}"))?;
    Ok(())
}

fn finish_uninstall(parent: &str, delete_settings: bool) -> Result<(), String> {
    if let Ok(pid) = parent.parse::<u32>() {
        win::wait_for(pid);
    }
    let dir = install_dir()?;
    files::remove(&dir, &[LICENSE_FILE, NOTICES_FILE])
        .map_err(|e| format!("Some files could not be removed. Close BTM and uninstall again. ({e})"))?;
    for folder in [win::start_menu(), win::desktop()].into_iter().flatten() {
        let _ = std::fs::remove_file(folder.join(format!("{NAME}.lnk")));
    }
    win::delete_user_key(UNINSTALL_KEY);
    if delete_settings {
        win::delete_user_key(product::SETTINGS_KEY);
        if let Some(local) = win::local_app_data() {
            let data = local.join("BetterTaskManager");
            let _ = std::fs::remove_file(data.join("namecache.tsv"));
            let _ = std::fs::remove_dir(&data);
        }
    }
    let gone = Dialog { heading: "Better Task Manager was removed", text: "", button: None, checkbox: None };
    win::show(&gone);
    Ok(())
}
