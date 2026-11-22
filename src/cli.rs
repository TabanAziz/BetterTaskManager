//! Command line. Only the first argument selects a mode.

use crate::admin::AdminTask;

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Window,
    Helper,
    Admin(AdminTask),
    /// Developer: draw a page into a bitmap without showing a window.
    Render {
        output: String,
        spec: String,
    },
    /// Developer: write one sample as text.
    Dump {
        output: String,
    },
    Invalid,
}

pub fn parse(arguments: &[String]) -> Mode {
    let Some((mode, rest)) = arguments.split_first() else { return Mode::Window };
    let operand = |index: usize, fallback: &str| rest.get(index).map_or(fallback, String::as_str).to_owned();
    match mode.as_str() {
        "helper" => Mode::Helper,
        "admin" => rest
            .split_first()
            .and_then(|(verb, operands)| AdminTask::parse(verb, operands))
            .map_or(Mode::Invalid, Mode::Admin),
        "render" => Mode::Render { output: operand(0, "btm-render.bmp"), spec: operand(1, "") },
        "dump" => Mode::Dump { output: operand(0, "btm-dump.txt") },
        // As the Task Manager replacement we are handed taskmgr.exe's path and switches.
        _ => Mode::Window,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::ProcessId;

    fn mode(line: &[&str]) -> Mode {
        parse(&line.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn plain_start_and_task_manager_redirect_open_the_window() {
        assert_eq!(mode(&[]), Mode::Window);
        assert_eq!(mode(&["C:\\Windows\\System32\\Taskmgr.exe", "/4"]), Mode::Window);
    }

    #[test]
    fn elevated_kill_needs_pid_and_creation_time() {
        let id = ProcessId { pid: 42, created: 9 };
        assert_eq!(mode(&["admin", "end", "42@9"]), Mode::Admin(AdminTask::EndProcesses(vec![id])));
        for bad in
            [&["admin", "end"][..], &["admin", "end", "42"], &["admin", "end", "42@9", "x"], &["admin", "format"]]
        {
            assert_eq!(mode(bad), Mode::Invalid);
        }
    }
}
