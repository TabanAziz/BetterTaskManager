//! Better Task Manager (BTM): entry point.
#![windows_subsystem = "windows"]

mod admin;
mod cli;
mod helper;
mod meta;
mod model;
mod nt;
mod pages;
mod pdh;
mod product;
mod protected;
mod reg;
mod sampler;
mod shell;
mod shell_hook;
mod sysinfo;
mod ui;
mod util;

use std::process::ExitCode;

use windows_sys::Win32::System::LibraryLoader::{SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32};

/// The UI is English, so ask Windows for English file descriptions and stock dialogs
/// as well (falls back to the display language where no English resources exist).
/// Number and date formats still follow the user's region.
fn speak_english() {
    use windows_sys::Win32::Globalization::{SetProcessPreferredUILanguages, MUI_LANGUAGE_NAME};
    let list = util::wide("en-US\0"); // double-NUL terminated
    let mut accepted = 0u32;
    unsafe { SetProcessPreferredUILanguages(MUI_LANGUAGE_NAME, list.as_ptr(), &mut accepted) };
}

fn main() -> ExitCode {
    // BTM also runs elevated from a user-writable folder (UAC tasks), so DLLs may only
    // ever come from System32, including the delay-loaded ones.
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) };
    util::trace("main");
    speak_english();

    let arguments: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let status = match cli::parse(&arguments) {
        cli::Mode::Window => {
            // Relative names given to the shell must not resolve next to a downloaded file.
            let _ = std::env::set_current_dir(shell::system_directory());
            ui::run();
            0
        }
        cli::Mode::Helper => {
            helper::run_helper();
            0
        }
        cli::Mode::Admin(task) => task.execute(),
        cli::Mode::Render { output, spec } => {
            ui::testmode::render(&output, &spec);
            0
        }
        cli::Mode::Dump { output } => {
            sampler::dump(&output);
            0
        }
        cli::Mode::Invalid => 2,
    };
    ExitCode::from(status as u8)
}
