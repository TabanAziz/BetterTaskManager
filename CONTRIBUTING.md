# Contributing

Issues and pull requests are welcome. Two rules come before features:

1. **It must not get slower.** Nothing new may run at start-up or on every refresh
   unless the visible page needs it.
2. **It must not crash.** Releases use `panic = "abort"`, so never index or slice
   data from Windows, firmware or other programs without a bounds check.

Before opening a pull request, run the local gate (there is no hosted CI):

```powershell
powershell -ExecutionPolicy Bypass -File tools\check.ps1
```

## Map of the code

| Path | |
| --- | --- |
| `src/sampler.rs` | One background thread that measures everything once per refresh |
| `src/pages/` | Data for App history, Startup apps, Users and Services, loaded on demand |
| `src/ui/` | One window, one back buffer, all custom drawn |
| `src/ui/plot.rs` | Graph rasterizer |
| `src/admin.rs`, `src/helper.rs`, `src/protected.rs` | Everything elevated, see SECURITY.md |
| `setup/` | Per-user installer |

## Testing the UI without touching your desktop

```powershell
BTM.exe render out.bmp "page=perf;perf=0;w=1257;h=800;wait=5000"
BTM.exe render out.txt "bench=20"
cargo test live_ -- --ignored --nocapture   # reads your real system, changes nothing
```

Screenshots for the repository must use demo data, real ones leak account names,
addresses and running programs:

```powershell
cargo build --release --features demo --target-dir target\demo
target\demo\release\BTM.exe render out.bmp "demo=1;page=procs;wait=6000"
```
