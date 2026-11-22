// Off-screen render mode for automated visual checks:
//   BTM.exe render <out.bmp> "page=procs;w=1257;h=860;wait=2500;expand=2;sel=1"
// The window is never shown, never takes focus and never touches the mouse.

use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::{app_opt, Page};

pub fn render(out: &str, spec: &str) {
    if !cfg!(feature = "demo") && spec.contains("demo=") {
        // Never hand out real data where invented data was asked for.
        return;
    }
    super::settings::NO_SAVE.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut kv = std::collections::HashMap::new();
    for part in spec.split(';') {
        if let Some((k, v)) = part.split_once('=') {
            kv.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let get = |k: &str, d: i32| kv.get(k).and_then(|v| v.parse::<i32>().ok()).unwrap_or(d);
    let w = get("w", 1257);
    let h = get("h", 860);
    let wait = get("wait", 2500) as u64;
    unsafe {
        let bounds = super::window::Bounds { left: 0, top: 0, width: w, height: h };
        let Some(hwnd) = super::window::create(super::window::Purpose::Offscreen, bounds) else { return };
        if let Some(secs) = kv.get("bench").and_then(|v| v.parse::<u64>().ok()) {
            bench(out, secs);
            DestroyWindow(hwnd);
            return;
        }
        let start = std::time::Instant::now();
        let mut applied = false;
        while start.elapsed().as_millis() < wait as u128 {
            super::window::dispatch_pending();
            if !applied && start.elapsed().as_millis() > (wait as u128) * 2 / 3 {
                applied = true;
                apply(&kv);
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        apply(&kv);
        let Some(a) = app_opt() else { return };
        // Simulated input on the hidden window (never the real mouse):
        // clicks=x,y|x,y (client px), dbl=x,y, keys=40|40|46 (virtual-key codes)
        if let Some(c) = kv.get("clicks") {
            for pt in c.split('|') {
                let v: Vec<i32> = pt.split(',').filter_map(|n| n.trim().parse().ok()).collect();
                if v.len() == 2 {
                    a.paint();
                    a.on_lbutton_down(v[0], v[1], false);
                    a.on_lbutton_up(v[0], v[1]);
                }
            }
        }
        if let Some(c) = kv.get("dbl") {
            let v: Vec<i32> = c.split(',').filter_map(|n| n.trim().parse().ok()).collect();
            if v.len() == 2 {
                a.paint();
                a.on_lbutton_down(v[0], v[1], false);
                a.on_lbutton_up(v[0], v[1]);
                a.on_lbutton_down(v[0], v[1], true);
                a.on_lbutton_up(v[0], v[1]);
            }
        }
        if let Some(k) = kv.get("keys") {
            for code in k.split('|').filter_map(|n| n.trim().parse::<u16>().ok()) {
                a.paint();
                a.on_key(code);
            }
        }
        a.paint();
        write_bmp(out, a);
        DestroyWindow(hwnd);
    }
}

fn apply(kv: &std::collections::HashMap<String, String>) {
    let Some(a) = app_opt() else { return };
    // The render window has no saved preferences of its own: keep them neutral.
    a.settings.nav_expanded = true;
    if kv.get("light").map(|v| v == "1").unwrap_or(false) && a.th.dark {
        a.th = super::Theme::load_variant(true);
    }
    if let Some(p) = kv.get("page") {
        let page = match p.as_str() {
            "perf" => Page::Perf,
            "details" => Page::Details,
            "settings" => Page::Settings,
            "history" => Page::History,
            "startup" => Page::Startup,
            "users" => Page::Users,
            "services" => Page::Services,
            _ => Page::Procs,
        };
        a.set_page(page);
    }
    if let Some(s) = kv.get("search") {
        a.search = s.clone();
    }
    if let Some(v) = kv.get("perf").and_then(|v| v.parse::<usize>().ok()) {
        a.perf_sel = v;
    }
    if let Some(v) = kv.get("logical") {
        a.settings.cpu_logical = v == "1";
    }
    if let Some(v) = kv.get("hint") {
        a.settings.hint_dismissed = v == "0";
    }
    #[cfg(feature = "demo")]
    if kv.contains_key("demo") {
        super::demo::install(a);
    }
    a.rebuild_rows();
    let page = a.page;
    if let Some(table) = a.lists.table(page) {
        if let Some(i) = kv.get("sel").and_then(|v| v.parse::<usize>().ok()) {
            table.select(i);
        }
        if kv.get("expand").is_some() && table.row(0).is_some_and(|r| !table.expanded.contains(&r.id)) {
            table.toggle(0);
        }
    }
    if let Some(n) = kv.get("expand").and_then(|v| v.parse::<usize>().ok()) {
        let keys: Vec<_> =
            a.procs.rows.iter().filter(|r| r.expandable).take(n).filter_map(|r| r.group.clone()).collect();
        for k in keys {
            a.expanded.insert(k);
        }
        a.rebuild_rows();
    }
    if let Some(i) = kv.get("sel").and_then(|v| v.parse::<usize>().ok()) {
        let page = a.page;
        super::procs::select_row(a, page, i);
    }
    if let Some(i) = kv.get("hoverrow").and_then(|v| v.parse::<usize>().ok()) {
        a.hover = super::Hit::Row(a.page, i);
    }
}

fn write_bmp(path: &str, a: &mut super::App) {
    let w = a.cv.w as usize;
    let h = a.cv.h as usize;
    let row = (w * 3 + 3) & !3;
    let mut data = Vec::with_capacity(54 + row * h);
    let size = (54 + row * h) as u32;
    data.extend_from_slice(b"BM");
    data.extend_from_slice(&size.to_le_bytes());
    data.extend_from_slice(&[0, 0, 0, 0]);
    data.extend_from_slice(&54u32.to_le_bytes());
    data.extend_from_slice(&40u32.to_le_bytes());
    data.extend_from_slice(&(w as i32).to_le_bytes());
    data.extend_from_slice(&(h as i32).to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&24u16.to_le_bytes());
    data.extend_from_slice(&[0u8; 24]);
    // Composite premultiplied pixels over the Mica tint seen in the user's screenshot.
    let mica = [0x29u32, 0x1du32, 0x23u32]; // b, g, r of #231d29
    unsafe { windows_sys::Win32::Graphics::Gdi::GdiFlush() };
    let px = unsafe { std::slice::from_raw_parts(a.cv.px, w * h) };
    for y in (0..h).rev() {
        let mut line = Vec::with_capacity(row);
        for x in 0..w {
            let p = px[y * w + x];
            let al = (p >> 24) & 0xFF;
            let r = (p >> 16) & 0xFF;
            let g = (p >> 8) & 0xFF;
            let b = p & 0xFF;
            let inv = 255 - al;
            let c = |src: u32, m: u32| -> u8 { (src + m * inv / 255).min(255) as u8 };
            line.push(c(b, mica[0]));
            line.push(c(g, mica[1]));
            line.push(c(r, mica[2]));
        }
        line.resize(row, 0);
        data.extend_from_slice(&line);
    }
    let _ = std::fs::write(path, data);
}

fn ft(f: &windows_sys::Win32::Foundation::FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

fn proc_times() -> (u64, u64) {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    unsafe {
        let mut c = std::mem::zeroed();
        let mut e = std::mem::zeroed();
        let mut k = std::mem::zeroed();
        let mut u = std::mem::zeroed();
        GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        (ft(&c), ft(&k) + ft(&u))
    }
}

fn now_ft() -> u64 {
    let mut f = unsafe { std::mem::zeroed() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime(&mut f) };
    ft(&f)
}

/// Hidden-window benchmark: paints every refresh exactly like a visible window would and
/// reports start-up latency and steady-state cost of the whole process.
fn bench(out: &str, secs: u64) {
    use std::fmt::Write as _;
    let (created, _) = proc_times();
    let mut first_paint_ms: Option<f64> = None;
    let mut last_seq = 0u64;
    let mut paints = 0u32;
    let mut paint_ms_total = 0f64;
    let mut sample_ms_total = 0f64;
    let mut samples = 0u32;
    let mut cpu_start: Option<(std::time::Instant, u64)> = None;
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < secs + 5 {
        super::window::dispatch_pending();
        if let Some(a) = app_opt() {
            let seq = a.snap.as_ref().map(|s| s.seq).unwrap_or(0);
            if seq != last_seq {
                last_seq = seq;
                let t = crate::util::qpc_now();
                a.paint();
                unsafe { windows_sys::Win32::Graphics::Gdi::GdiFlush() };
                let pm = crate::util::qpc_ms(t, crate::util::qpc_now());
                if first_paint_ms.is_none() && a.snap.as_ref().map(|s| !s.procs.is_empty()).unwrap_or(false) {
                    first_paint_ms = Some((now_ft() - created) as f64 / 10_000.0);
                }
                if cpu_start.is_some() {
                    paints += 1;
                    paint_ms_total += pm;
                    if let Some(s) = a.snap.as_ref() {
                        samples += 1;
                        sample_ms_total += s.sample_ms as f64;
                    }
                }
            }
        }
        if cpu_start.is_none() && start.elapsed().as_secs() >= 5 {
            cpu_start = Some((std::time::Instant::now(), proc_times().1));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (t0, c0) = cpu_start.unwrap();
    let el = t0.elapsed().as_secs_f64();
    let cpu_ms_per_s = (proc_times().1 - c0) as f64 / 10_000.0 / el;
    let mut pmc: windows_sys::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    pmc.cb = std::mem::size_of_val(&pmc) as u32;
    unsafe {
        windows_sys::Win32::System::ProcessStatus::GetProcessMemoryInfo(
            windows_sys::Win32::System::Threading::GetCurrentProcess(),
            &mut pmc as *mut _ as *mut _,
            pmc.cb,
        )
    };
    let mut o = String::new();
    let _ = writeln!(o, "first_data_painted_ms_since_process_start: {:.1}", first_paint_ms.unwrap_or(-1.0));
    let _ = writeln!(o, "cpu_ms_per_s (whole process, incl. painting): {:.2}", cpu_ms_per_s);
    let _ = writeln!(o, "avg_sample_ms: {:.2}", if samples > 0 { sample_ms_total / samples as f64 } else { 0.0 });
    let _ = writeln!(
        o,
        "avg_paint_ms: {:.2} ({} paints)",
        if paints > 0 { paint_ms_total / paints as f64 } else { 0.0 },
        paints
    );
    if let Some(a) = app_opt() {
        let n = a.paint_stats[4].max(1.0);
        let _ = writeln!(
            o,
            "paint phases avg ms: chrome {:.2} panel {:.2} page {:.2} alpha {:.2}",
            a.paint_stats[0] / n,
            a.paint_stats[1] / n,
            a.paint_stats[2] / n,
            a.paint_stats[3] / n
        );
    }
    let _ = writeln!(o, "working_set_mb: {:.1}", pmc.WorkingSetSize as f64 / 1048576.0);
    let _ = writeln!(o, "private_mb: {:.1}", pmc.PrivateUsage as f64 / 1048576.0);
    let _ = std::fs::write(out, o);
}
