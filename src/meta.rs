// Per-executable metadata: friendly name (FileDescription), publisher (CompanyName) and
// the app icon. Version info is read once per path; icons are extracted on a separate
// low-priority thread so neither the sampler nor the UI ever waits for the disk.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::System::Threading::{
    CreateEventW, SetEvent, SetThreadPriority, WaitForSingleObject, THREAD_PRIORITY_BELOW_NORMAL,
};
use windows_sys::Win32::UI::Shell::{
    SHDefExtractIconW, SHGetStockIconInfo, SHGSI_ICON, SHGSI_SMALLICON, SHSTOCKICONINFO, SIID_APPLICATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, PostMessageW};

use crate::util::{from_wide, wide};

pub const WM_APP_META: u32 = 0x8000 + 2;

pub struct Meta {
    pub path: String,
    pub description: String,
    pub company: String,
    mtime: u64,
    icon: AtomicIsize,
    icon_size: AtomicI32,
    queued: AtomicBool,
}

impl Meta {
    /// HICON (0 while not yet loaded or unavailable).
    pub fn icon(&self) -> isize {
        let h = self.icon.load(Ordering::Acquire);
        if h > 0 {
            h
        } else {
            0
        }
    }
}

impl Drop for Meta {
    fn drop(&mut self) {
        let icon = *self.icon.get_mut();
        if icon > 0 {
            unsafe { DestroyIcon(icon as _) };
        }
    }
}

// Every icon costs three of the 10,000 GDI objects a process may have, and updaters,
// installers and builds keep producing new paths: forget executables nothing refers to.
const KEEP_EXECUTABLES: usize = 600;
const TRIM_STEP: usize = 100;
const REMEMBER_NAMES: usize = 5000;

fn trim(map: &mut HashMap<String, Arc<Meta>>, limit: &AtomicUsize) {
    if map.len() < limit.load(Ordering::Relaxed) {
        return;
    }
    map.retain(|_, meta| Arc::strong_count(meta) > 1);
    limit.store((map.len() + TRIM_STEP).max(KEEP_EXECUTABLES), Ordering::Relaxed);
}

struct Loader {
    map: Mutex<HashMap<String, Arc<Meta>>>,
    trim_at: AtomicUsize,
    /// Names remembered from the previous run: lower-case path -> (mtime, description, company).
    disk: Mutex<HashMap<String, (u64, String, String)>>,
    dirty: AtomicBool,
    queue: Mutex<VecDeque<Arc<Meta>>>,
    event: isize,
    notify: AtomicIsize,
    icon_size: AtomicI32,
    default_icon: AtomicIsize,
}

fn loader() -> &'static Loader {
    static L: OnceLock<Loader> = OnceLock::new();
    L.get_or_init(|| {
        let ev = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) } as isize;
        let l = Loader {
            map: Mutex::new(HashMap::with_capacity(512)),
            trim_at: AtomicUsize::new(KEEP_EXECUTABLES),
            disk: Mutex::new(HashMap::new()),
            dirty: AtomicBool::new(false),
            queue: Mutex::new(VecDeque::new()),
            event: ev,
            notify: AtomicIsize::new(0),
            icon_size: AtomicI32::new(16),
            default_icon: AtomicIsize::new(0),
        };
        std::thread::Builder::new().name("icons".into()).stack_size(256 << 10).spawn(icon_thread).ok();
        l
    })
}

pub fn set_notify_window(hwnd: HWND) {
    loader().notify.store(hwnd as isize, Ordering::Release);
}

/// Icon edge length in physical pixels (changes with DPI). Re-queues loaded icons.
pub fn set_icon_size(px: i32) {
    let l = loader();
    if l.icon_size.swap(px, Ordering::AcqRel) == px {
        return;
    }
    let all: Vec<Arc<Meta>> = l.map.lock().unwrap().values().cloned().collect();
    let mut q = l.queue.lock().unwrap();
    for m in all {
        if m.icon_size.load(Ordering::Acquire) != px && !m.queued.swap(true, Ordering::AcqRel) {
            q.push_back(m);
        }
    }
    drop(q);
    unsafe { SetEvent(l.event as _) };
}

/// Generic icon if the icon thread has loaded it already (never blocks painting).
pub fn default_icon_if_loaded() -> isize {
    loader().default_icon.load(Ordering::Acquire)
}

/// Generic application icon used until (or instead of) a real one.
pub fn default_icon() -> isize {
    let l = loader();
    let h = l.default_icon.load(Ordering::Acquire);
    if h != 0 {
        return h;
    }
    unsafe {
        let mut sii: SHSTOCKICONINFO = std::mem::zeroed();
        sii.cbSize = std::mem::size_of::<SHSTOCKICONINFO>() as u32;
        if SHGetStockIconInfo(SIID_APPLICATION, SHGSI_ICON | SHGSI_SMALLICON, &mut sii) >= 0 {
            let v = sii.hIcon as isize;
            l.default_icon.store(v, Ordering::Release);
            return v;
        }
    }
    0
}

pub fn is_cached(path: &str) -> bool {
    loader().map.lock().unwrap().contains_key(&path.to_lowercase())
}

/// True when the version resource must actually be read from disk.
pub fn needs_read(path: &str) -> bool {
    let k = path.to_lowercase();
    let l = loader();
    !l.map.lock().unwrap().contains_key(&k) && !l.disk.lock().unwrap().contains_key(&k)
}

/// Wake the icon thread after a batch of `get_quiet` calls.
pub fn kick() {
    unsafe { SetEvent(loader().event as _) };
}

/// Like `get`, without waking the icon thread for every single path.
pub fn get_quiet(path: &str) -> Arc<Meta> {
    get_impl(path, false)
}

/// Metadata for a DOS path; reads the version resource on first use (cheap, cached).
pub fn get(path: &str) -> Arc<Meta> {
    get_impl(path, true)
}

fn get_impl(path: &str, wake: bool) -> Arc<Meta> {
    let l = loader();
    let key = path.to_lowercase();
    if let Some(m) = l.map.lock().unwrap().get(&key) {
        return m.clone();
    }
    // Names from the previous run are used immediately; the icon thread re-checks the
    // file's timestamp later and drops stale entries (so start-up does no file I/O here).
    let cached = l.disk.lock().unwrap().get(&key).map(|c| (c.0, c.1.clone(), c.2.clone()));
    let (mtime, description, company) = match cached {
        Some(c) => c,
        None => {
            l.dirty.store(true, Ordering::Release);
            let (d, c) = read_version_strings(path);
            (file_mtime(path), d, c)
        }
    };
    let m = Arc::new(Meta {
        path: path.to_string(),
        description,
        company,
        mtime,
        icon: AtomicIsize::new(0),
        icon_size: AtomicI32::new(0),
        queued: AtomicBool::new(true),
    });
    {
        let mut map = l.map.lock().unwrap();
        if let Some(existing) = map.get(&key) {
            return existing.clone();
        }
        trim(&mut map, &l.trim_at);
        map.insert(key, m.clone());
    }
    l.queue.lock().unwrap().push_back(m.clone());
    if wake {
        unsafe { SetEvent(l.event as _) };
    }
    m
}

fn file_mtime(path: &str) -> u64 {
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileAttributesExW, GetFileExInfoStandard, WIN32_FILE_ATTRIBUTE_DATA,
    };
    let w = wide(path);
    let mut d: WIN32_FILE_ATTRIBUTE_DATA = unsafe { std::mem::zeroed() };
    if unsafe { GetFileAttributesExW(w.as_ptr(), GetFileExInfoStandard, &mut d as *mut _ as *mut c_void) } == 0 {
        return 0;
    }
    ((d.ftLastWriteTime.dwHighDateTime as u64) << 32) | d.ftLastWriteTime.dwLowDateTime as u64
}

fn cache_file() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(std::path::PathBuf::from(base).join(crate::product::DATA_DIR).join("namecache.tsv"))
}

/// Load the name cache written by the previous run (one small file read).
pub fn load_disk_cache() {
    let Some(f) = cache_file() else { return };
    let Ok(text) = std::fs::read_to_string(&f) else { return };
    let mut m = HashMap::with_capacity(512);
    for line in text.lines() {
        let mut it = line.split('\t');
        let (Some(p), Some(t), Some(d), Some(c)) = (it.next(), it.next(), it.next(), it.next()) else { continue };
        if let Ok(t) = t.parse::<u64>() {
            m.insert(p.to_string(), (t, d.to_string(), c.to_string()));
        }
    }
    *loader().disk.lock().unwrap() = m;
}

/// Persist names when new executables were seen (called from the sampler thread).
pub fn save_disk_cache_if_dirty() {
    if crate::shell::elevated() {
        return;
    }
    let l = loader();
    if !l.dirty.swap(false, Ordering::AcqRel) {
        return;
    }
    let Some(f) = cache_file() else { return };
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let mut out = String::with_capacity(64 * 1024);
    {
        let mut disk = l.disk.lock().unwrap();
        if disk.len() > REMEMBER_NAMES {
            disk.clear();
        }
        for (k, m) in l.map.lock().unwrap().iter() {
            if m.mtime != 0 {
                disk.insert(k.clone(), (m.mtime, clean(&m.description), clean(&m.company)));
            }
        }
        for (k, (t, d, c)) in disk.iter() {
            out.push_str(&format!("{}\t{}\t{}\t{}\n", k, t, d, c));
        }
    }
    if let Some(dir) = f.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = f.with_extension("tmp");
    if std::fs::write(&tmp, out).is_ok() {
        let _ = std::fs::rename(&tmp, &f);
    }
}

fn icon_thread() {
    unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        SetThreadPriority(windows_sys::Win32::System::Threading::GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
    let l = loader();
    default_icon();
    post(l);
    loop {
        unsafe { WaitForSingleObject(l.event as _, 0xFFFF_FFFF) };
        let mut loaded_any = false;
        let mut last_post = crate::util::qpc_now();
        loop {
            let next = l.queue.lock().unwrap().pop_front();
            let Some(m) = next else { break };
            let size = l.icon_size.load(Ordering::Acquire);
            if m.icon_size.load(Ordering::Acquire) == 0 && m.mtime != 0 && file_mtime(&m.path) != m.mtime {
                // Executable changed since the name was cached: re-read on next start.
                l.disk.lock().unwrap().remove(&m.path.to_lowercase());
                l.dirty.store(true, Ordering::Release);
            }
            let h = extract_icon(&m.path, size);
            let old = m.icon.swap(if h != 0 { h } else { -1 }, Ordering::AcqRel);
            if old > 0 {
                unsafe { DestroyIcon(old as _) };
            }
            m.icon_size.store(size, Ordering::Release);
            m.queued.store(false, Ordering::Release);
            loaded_any = true;
            // Repaint in batches (at most every 60 ms) while a burst is loading.
            let now = crate::util::qpc_now();
            if crate::util::qpc_ms(last_post, now) > 60.0 {
                post(l);
                last_post = now;
                loaded_any = false;
            }
        }
        if loaded_any {
            post(l);
        }
    }
}

fn post(l: &Loader) {
    let hwnd = l.notify.load(Ordering::Acquire);
    if hwnd != 0 {
        unsafe { PostMessageW(hwnd as HWND, WM_APP_META, 0, 0) };
    }
}

fn extract_icon(path: &str, size: i32) -> isize {
    if path.is_empty() {
        return 0;
    }
    let w = wide(path);
    let mut large = std::ptr::null_mut();
    let mut small = std::ptr::null_mut();
    let sizes = ((size as u32) & 0xFFFF) | (((size as u32) & 0xFFFF) << 16);
    let hr = unsafe { SHDefExtractIconW(w.as_ptr(), 0, 0, &mut large, &mut small, sizes) };
    if hr == 0 && !large.is_null() {
        if !small.is_null() && small != large {
            unsafe { DestroyIcon(small) };
        }
        return large as isize;
    }
    0
}

fn read_version_strings(path: &str) -> (String, String) {
    if path.is_empty() {
        return (String::new(), String::new());
    }
    let w = wide(path);
    unsafe {
        let mut dummy = 0u32;
        let size = GetFileVersionInfoSizeW(w.as_ptr(), &mut dummy);
        if size == 0 {
            return (String::new(), String::new());
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(w.as_ptr(), 0, size, data.as_mut_ptr() as *mut c_void) == 0 {
            return (String::new(), String::new());
        }
        let mut langs: Vec<(u16, u16)> = Vec::new();
        let sub = wide("\\VarFileInfo\\Translation");
        let mut p: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        if VerQueryValueW(data.as_ptr() as *const c_void, sub.as_ptr(), &mut p, &mut len) != 0 && !p.is_null() {
            let n = len as usize / 4;
            let pairs = std::slice::from_raw_parts(p as *const u16, n * 2);
            for i in 0..n {
                langs.push((pairs[i * 2], pairs[i * 2 + 1]));
            }
        }
        for fallback in [(0x0409u16, 0x04B0u16), (0x0409, 0x04E4), (0x0000, 0x04B0), (0x0407, 0x04B0)] {
            if !langs.contains(&fallback) {
                langs.push(fallback);
            }
        }
        let query = |lang: (u16, u16), name: &str| -> Option<String> {
            let q = wide(&format!("\\StringFileInfo\\{:04x}{:04x}\\{}", lang.0, lang.1, name));
            let mut p: *mut c_void = std::ptr::null_mut();
            let mut len = 0u32;
            if VerQueryValueW(data.as_ptr() as *const c_void, q.as_ptr(), &mut p, &mut len) != 0
                && !p.is_null()
                && len > 0
            {
                let s = from_wide(std::slice::from_raw_parts(p as *const u16, len as usize));
                let s = s.trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
            None
        };
        let mut desc = String::new();
        let mut comp = String::new();
        for &l in &langs {
            if desc.is_empty() {
                if let Some(s) = query(l, "FileDescription") {
                    desc = s;
                }
            }
            if comp.is_empty() {
                if let Some(s) = query(l, "CompanyName") {
                    comp = s;
                }
            }
            if !desc.is_empty() && !comp.is_empty() {
                break;
            }
        }
        (desc, comp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetGuiResources};

    fn gdi_objects() -> u32 {
        unsafe { GetGuiResources(GetCurrentProcess(), 0) }
    }

    fn with_icon(path: &str) -> Arc<Meta> {
        let icon = extract_icon(path, 16);
        assert!(icon > 0, "no icon for {path}");
        Arc::new(Meta {
            path: path.to_string(),
            description: String::new(),
            company: String::new(),
            mtime: 0,
            icon: AtomicIsize::new(icon),
            icon_size: AtomicI32::new(16),
            queued: AtomicBool::new(false),
        })
    }

    #[test]
    fn forgotten_executables_give_their_icons_back() {
        let exe = format!("{}/notepad.exe", crate::shell::system_directory());
        drop(with_icon(&exe)); // whatever the shell allocates once
        let before = gdi_objects();

        let limit = AtomicUsize::new(KEEP_EXECUTABLES);
        let mut map = HashMap::new();
        let in_use = with_icon(&exe);
        map.insert("in use".to_string(), in_use.clone());
        for i in 0..1500 {
            trim(&mut map, &limit);
            map.insert(i.to_string(), with_icon(&exe));
            assert!(map.len() <= KEEP_EXECUTABLES + TRIM_STEP);
        }
        assert!(map.contains_key("in use") && in_use.icon() > 0);
        drop(map);
        drop(in_use);
        let after = gdi_objects();
        assert!(after <= before + 12, "GDI objects {before} -> {after}");
    }
}
