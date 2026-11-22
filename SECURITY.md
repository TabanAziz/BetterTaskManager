# Security

## Reporting a problem

Please use [private vulnerability reporting][report] instead of a public issue.
Only the latest release receives fixes.

## Design

BTM's window runs without administrator rights. Three things can run elevated:

1. **Single tasks** (`BTM.exe admin ...`), each behind its own UAC prompt: ending a
   process, setting up the helper or the shortcut, switching a machine-wide startup
   app, controlling a service. Processes are identified by PID *and* creation time.
2. **The data helper** (`BTM.exe helper`), an on-demand scheduled task that the user
   sets up with one UAC prompt. It takes no commands and reads no data from shared
   memory. It writes process counters into a section only after checking that the
   user owns that object, writes startup impact levels to
   `HKLM\SOFTWARE\BetterTaskManager`, and exits 15 seconds after the last request.
3. **The Ctrl+Shift+Esc redirect**, an Image File Execution Options `Debugger`
   value for `taskmgr.exe`.

Rules that changes must keep:

- The scheduled task and the redirect only ever point to
  `%ProgramFiles%\Better Task Manager\BTM.exe`, which needs administrator rights to change.
- Elevated code takes system paths from Windows APIs, never from environment
  variables, and writes nothing into user-writable folders.
- DLLs are loaded from System32 only, including in setup.
- An elevated window hands links and settings pages to Explorer instead of opening
  per-user handlers itself.
- Setup and uninstall delete only files they own, never a directory tree.

Releases are not code-signed. Each one ships `SHA256SUMS.txt`.

[report]: https://github.com/TabanAziz/BetterTaskManager/security/advisories/new
