//! App history: traffic per app over the last 30 days, as recorded by Windows.
//!
//! Uses the documented WinRT call `IConnectionProfile3::GetAttributedNetworkUsageAsync`,
//! which needs no elevation. The few interfaces involved are called through their
//! vtables, which avoids pulling in a WinRT projection.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;

use crate::meta::{self, Meta};
use crate::sysinfo::DeviceMap;
use crate::util::wide;

/// The few combase functions needed (windows-sys has no WinRT module). Delay-loaded.
mod rt {
    use std::ffi::c_void;
    use windows_sys::core::GUID;

    pub type Str = *mut c_void;
    pub const RO_INIT_MULTITHREADED: i32 = 1;

    #[link(name = "combase", kind = "raw-dylib")]
    extern "system" {
        pub fn RoInitialize(kind: i32) -> i32;
        pub fn RoUninitialize();
        pub fn RoGetActivationFactory(class: Str, iid: *const GUID, factory: *mut *mut c_void) -> i32;
        pub fn WindowsCreateString(text: *const u16, len: u32, out: *mut Str) -> i32;
        pub fn WindowsDeleteString(text: Str) -> i32;
        pub fn WindowsGetStringRawBuffer(text: Str, len: *mut u32) -> *const u16;
    }
}

pub const DAYS: i64 = 30;

#[derive(Clone)]
pub struct AppUsage {
    /// Lower-case executable path or package family: joins with running processes.
    pub key: String,
    pub name: String,
    pub meta: Option<Arc<Meta>>,
    pub received: u64,
    pub sent: u64,
}

const STATICS: GUID = GUID::from_u128(0x5074f851_950d_4165_9c15_365619481eea);
const PROFILE3: GUID = GUID::from_u128(0x578c2528_4cd9_4161_8045_201cfd5b115c);
const ASYNC_INFO: GUID = GUID::from_u128(0x00000036_0000_0000_c000_000000000046);

// Vtable slots. IInspectable takes the first six.
const QUERY_INTERFACE: usize = 0;
const RELEASE: usize = 2;
const STATICS_GET_PROFILES: usize = 6;
const PROFILE3_GET_USAGE: usize = 6;
const INFO_STATUS: usize = 7;
const OPERATION_RESULTS: usize = 8;
const VIEW_GET_AT: usize = 6;
const VIEW_SIZE: usize = 7;
const USAGE_SENT: usize = 6;
const USAGE_RECEIVED: usize = 7;
const USAGE_ID: usize = 8;
const USAGE_NAME: usize = 9;

type Raw = *mut c_void;

/// An owned interface pointer.
struct Object(Raw);

impl Drop for Object {
    fn drop(&mut self) {
        unsafe {
            let release: unsafe extern "system" fn(Raw) -> u32 = self.method(RELEASE);
            release(self.0);
        }
    }
}

impl Object {
    /// # Safety
    /// `F` must be the signature of that slot.
    unsafe fn method<F: Copy>(&self, slot: usize) -> F {
        let vtable = *(self.0 as *const *const *const c_void);
        std::mem::transmute_copy(&*vtable.add(slot))
    }

    fn new(hr: i32, raw: Raw) -> Option<Object> {
        (hr >= 0 && !raw.is_null()).then(|| Object(raw))
    }

    fn cast(&self, iid: &GUID) -> Option<Object> {
        let mut out = std::ptr::null_mut();
        let hr = unsafe {
            let query: unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> i32 = self.method(QUERY_INTERFACE);
            query(self.0, iid, &mut out)
        };
        Object::new(hr, out)
    }

    fn get_object(&self, slot: usize) -> Option<Object> {
        let mut out = std::ptr::null_mut();
        let hr = unsafe {
            let get: unsafe extern "system" fn(Raw, *mut Raw) -> i32 = self.method(slot);
            get(self.0, &mut out)
        };
        Object::new(hr, out)
    }

    fn get_u32(&self, slot: usize) -> Option<u32> {
        let mut out = 0;
        let hr = unsafe {
            let get: unsafe extern "system" fn(Raw, *mut u32) -> i32 = self.method(slot);
            get(self.0, &mut out)
        };
        (hr >= 0).then_some(out)
    }

    fn get_u64(&self, slot: usize) -> u64 {
        let mut out = 0;
        let hr = unsafe {
            let get: unsafe extern "system" fn(Raw, *mut u64) -> i32 = self.method(slot);
            get(self.0, &mut out)
        };
        if hr >= 0 {
            out
        } else {
            0
        }
    }

    fn get_text(&self, slot: usize) -> String {
        unsafe {
            let mut text: Raw = std::ptr::null_mut();
            let get: unsafe extern "system" fn(Raw, *mut Raw) -> i32 = self.method(slot);
            if get(self.0, &mut text) < 0 {
                return String::new();
            }
            let mut len = 0u32;
            let chars = rt::WindowsGetStringRawBuffer(text as _, &mut len);
            let owned = if chars.is_null() || len == 0 {
                String::new()
            } else {
                String::from_utf16_lossy(std::slice::from_raw_parts(chars, len as usize))
            };
            rt::WindowsDeleteString(text as _);
            owned
        }
    }

    fn items(&self) -> Vec<Object> {
        let count = self.get_u32(VIEW_SIZE).unwrap_or(0);
        (0..count)
            .filter_map(|index| {
                let mut out = std::ptr::null_mut();
                let hr = unsafe {
                    let get: unsafe extern "system" fn(Raw, u32, *mut Raw) -> i32 = self.method(VIEW_GET_AT);
                    get(self.0, index, &mut out)
                };
                Object::new(hr, out)
            })
            .collect()
    }
}

/// NetworkUsageStates with both filters on "don't care".
#[repr(C)]
#[derive(Clone, Copy)]
struct UsageStates {
    roaming: i32,
    shared: i32,
}

pub fn load() -> Result<Vec<AppUsage>, String> {
    unsafe {
        let started = rt::RoInitialize(rt::RO_INIT_MULTITHREADED);
        let result = query().ok_or_else(|| "Windows did not return its network usage history.".to_string());
        if started >= 0 {
            rt::RoUninitialize();
        }
        Ok(name_apps(result?))
    }
}

/// Bytes (received, sent) per attribution id and name.
fn query() -> Option<HashMap<String, (String, u64, u64)>> {
    let class = wide("Windows.Networking.Connectivity.NetworkInformation");
    let factory = unsafe {
        let mut id: rt::Str = std::ptr::null_mut();
        if rt::WindowsCreateString(class.as_ptr(), class.len() as u32 - 1, &mut id) < 0 {
            return None;
        }
        let mut raw = std::ptr::null_mut();
        let hr = rt::RoGetActivationFactory(id, &STATICS, &mut raw);
        rt::WindowsDeleteString(id);
        Object::new(hr, raw)?
    };

    let mut now = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    unsafe { GetSystemTimeAsFileTime(&mut now) };
    let to = ((u64::from(now.dwHighDateTime) << 32) | u64::from(now.dwLowDateTime)) as i64;
    let from = to - DAYS * 86_400 * 10_000_000;

    // One query per connection profile (every Wi-Fi, Ethernet, VPN ...), all at once.
    let profiles = factory.get_object(STATICS_GET_PROFILES)?;
    let mut pending: Vec<(Object, Object)> = Vec::new();
    for profile in profiles.items() {
        let Some(profile) = profile.cast(&PROFILE3) else { continue };
        let mut raw = std::ptr::null_mut();
        let hr = unsafe {
            let start: unsafe extern "system" fn(Raw, i64, i64, UsageStates, *mut Raw) -> i32 =
                profile.method(PROFILE3_GET_USAGE);
            start(profile.0, from, to, UsageStates { roaming: 0, shared: 0 }, &mut raw)
        };
        let Some(operation) = Object::new(hr, raw) else { continue };
        if let Some(info) = operation.cast(&ASYNC_INFO) {
            pending.push((operation, info));
        }
    }
    if pending.is_empty() {
        return None;
    }

    let deadline = Instant::now() + Duration::from_secs(25);
    let mut totals: HashMap<String, (String, u64, u64)> = HashMap::new();
    let mut answered = false;
    while !pending.is_empty() && Instant::now() < deadline {
        let mut waiting = Vec::new();
        for (operation, info) in pending {
            match info.get_u32(INFO_STATUS) {
                Some(0) => waiting.push((operation, info)), // Started
                Some(1) => {
                    let Some(list) = operation.get_object(OPERATION_RESULTS) else { continue };
                    answered = true;
                    for item in list.items() {
                        let id = item.get_text(USAGE_ID);
                        let total = totals.entry(id).or_insert_with(|| (item.get_text(USAGE_NAME), 0, 0));
                        total.1 += item.get_u64(USAGE_RECEIVED);
                        total.2 += item.get_u64(USAGE_SENT);
                    }
                }
                _ => {} // cancelled or failed
            }
        }
        pending = waiting;
        if !pending.is_empty() {
            std::thread::sleep(Duration::from_millis(40));
        }
    }
    answered.then_some(totals)
}

fn name_apps(totals: HashMap<String, (String, u64, u64)>) -> Vec<AppUsage> {
    let devices = DeviceMap::new();
    let packages = super::packages::Packages::scan();
    let mut apps: HashMap<String, AppUsage> = HashMap::new();
    for (id, (given, received, sent)) in totals {
        let mut app = AppUsage { key: id.to_lowercase(), name: given, meta: None, received, sent };
        if id.starts_with('\\') {
            let path = devices.to_dos(&id).unwrap_or_else(|| id.clone());
            let info = meta::get(&path);
            app.name = if info.description.is_empty() { super::file_stem(&path) } else { info.description.clone() };
            app.key = path.to_lowercase();
            app.meta = Some(info);
        } else if id.is_empty() {
            app.name = "System".into();
            app.key = "system".into();
        } else if packages.knows(&id) {
            app.name = packages.describe(&id).name;
        } else if app.name.is_empty() {
            app.name = id;
        }
        apps.entry(app.key.clone())
            .and_modify(|known| {
                known.received += app.received;
                known.sent += app.sent;
            })
            .or_insert(app);
    }
    let mut list: Vec<AppUsage> = apps.into_values().collect();
    list.sort_by_key(|app| std::cmp::Reverse(app.received + app.sent));
    list
}
