// Small shared helpers: UTF-16 strings, locale-aware number formatting, timing.

use std::sync::OnceLock;

use windows_sys::Win32::Globalization::{GetLocaleInfoEx, LOCALE_SDECIMAL, LOCALE_STHOUSAND};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// NUL-terminated UTF-16 for Win32 calls.
pub fn wide(text: &str) -> Vec<u16> {
    let mut units = Vec::with_capacity(text.len() + 1);
    units.extend(text.encode_utf16());
    units.push(0);
    units
}

/// Text up to the first NUL (or the whole buffer).
pub fn from_wide(units: &[u16]) -> String {
    let text = units.split(|&unit| unit == 0).next().unwrap_or_default();
    char::decode_utf16(text.iter().copied()).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
}

/// # Safety
/// `p` must be null or point to a NUL-terminated UTF-16 string.
pub unsafe fn from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0usize;
    while *p.add(n) != 0 {
        n += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
}

struct Seps {
    dec: String,
    thou: String,
}

fn seps() -> &'static Seps {
    static S: OnceLock<Seps> = OnceLock::new();
    S.get_or_init(|| unsafe {
        let mut buf = [0u16; 8];
        let n = GetLocaleInfoEx(std::ptr::null(), LOCALE_SDECIMAL, buf.as_mut_ptr(), 8);
        let dec = if n > 1 { from_wide(&buf) } else { ",".into() };
        let mut buf2 = [0u16; 8];
        let n2 = GetLocaleInfoEx(std::ptr::null(), LOCALE_STHOUSAND, buf2.as_mut_ptr(), 8);
        let thou = if n2 > 1 { from_wide(&buf2) } else { ".".into() };
        Seps { dec, thou }
    })
}

/// Format with a fixed number of decimals using the user's decimal and grouping separators,
/// e.g. 3096.04 -> "3 096,0" on de-AT.
pub fn fmt_num(v: f64, decimals: usize) -> String {
    let s = seps();
    let v = if v.is_finite() { v } else { 0.0 };
    let txt = format!("{:.*}", decimals, v.abs());
    let (int_part, frac) = match txt.find('.') {
        Some(i) => (&txt[..i], Some(&txt[i + 1..])),
        None => (&txt[..], None),
    };
    let mut out = String::with_capacity(txt.len() + 4);
    if v < 0.0 && txt.chars().any(|c| c != '0' && c != '.') {
        out.push('-');
    }
    let digits = int_part.as_bytes();
    for (i, &d) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push_str(&s.thou);
        }
        out.push(d as char);
    }
    if let Some(f) = frac {
        out.push_str(&s.dec);
        out.push_str(f);
    }
    out
}

pub fn fmt_int(v: u64) -> String {
    fmt_num(v as f64, 0)
}

/// Task Manager style: zero stays "0", tiny non-zero values show as the smallest step.
pub fn fmt_min_step(v: f64, decimals: usize) -> String {
    let step = 10f64.powi(-(decimals as i32));
    if v <= 0.0 {
        "0".to_string()
    } else if v < step {
        fmt_num(step, decimals)
    } else {
        fmt_num(v, decimals)
    }
}

/// Bytes -> "1,2 GB" / "512 MB" / "34 KB".
pub fn fmt_bytes(b: f64) -> String {
    const K: f64 = 1024.0;
    if b >= K * K * K * K {
        format!("{} TB", fmt_num(b / (K * K * K * K), 1))
    } else if b >= K * K * K {
        format!("{} GB", fmt_num(b / (K * K * K), 1))
    } else if b >= K * K {
        format!("{} MB", fmt_num(b / (K * K), 0))
    } else {
        format!("{} KB", fmt_num(b / K, 0))
    }
}

/// Bits per second -> "64,0 Kbit/s" / "12,3 Mbit/s".
pub fn fmt_bits(bps: f64) -> String {
    if bps >= 1e9 {
        format!("{} Gbps", fmt_num(bps / 1e9, 1))
    } else if bps >= 1e6 {
        format!("{} Mbps", fmt_num(bps / 1e6, 1))
    } else {
        format!("{} Kbps", fmt_num(bps / 1e3, 0))
    }
}

/// Bytes per second for disk throughput -> "1,2 MB/s" / "312 KB/s".
pub fn fmt_bytes_rate(b: f64) -> String {
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{} GB/s", fmt_num(b / (1024.0 * 1024.0 * 1024.0), 1))
    } else if b >= 1024.0 * 1024.0 {
        format!("{} MB/s", fmt_num(b / (1024.0 * 1024.0), 1))
    } else {
        format!("{} KB/s", fmt_num(b / 1024.0, 0))
    }
}

pub fn qpc_now() -> i64 {
    let mut v = 0i64;
    unsafe { QueryPerformanceCounter(&mut v) };
    v
}

pub fn qpc_freq() -> i64 {
    static F: OnceLock<i64> = OnceLock::new();
    *F.get_or_init(|| {
        let mut f = 0i64;
        unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    })
}

pub fn qpc_ms(from: i64, to: i64) -> f64 {
    (to - from) as f64 * 1000.0 / qpc_freq() as f64
}

/// Seconds -> "1:06:13:33" (days:hh:mm:ss) like Task Manager's uptime.
pub fn fmt_uptime(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{}:{:02}:{:02}:{:02}", d, h, m, s)
}

/// Start-up tracing (BTM_TRACE=1): milliseconds since process creation.
pub fn trace(label: &str) {
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| cfg!(feature = "trace")) {
        return;
    }
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let ft = |f: &windows_sys::Win32::Foundation::FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    unsafe {
        let mut c = std::mem::zeroed();
        let mut e = std::mem::zeroed();
        let mut k = std::mem::zeroed();
        let mut u = std::mem::zeroed();
        GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        let mut now = std::mem::zeroed();
        windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime(&mut now);
        let ms = (ft(&now) as i64 - ft(&c) as i64) as f64 / 10_000.0;
        use std::io::Write;
        if let Ok(mut f) =
            std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("btm-trace.log"))
        {
            let _ = writeln!(f, "{:8.1} ms  {}", ms, label);
        }
    }
}
