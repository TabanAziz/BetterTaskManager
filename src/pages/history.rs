//! App history: traffic per app over the last 30 days, as recorded by Windows.
//!
//! Uses the documented WinRT call `IConnectionProfile3::GetAttributedNetworkUsageAsync`,
//! which needs no elevation. The few interfaces involved are called through their
//! vtables, which avoids pulling in a WinRT projection.

use std::cell::OnceCell;
use std::cmp::Reverse;
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

pub enum History {
    Loading(Vec<AppUsage>),
    Complete(Vec<AppUsage>),
    Incomplete(Vec<AppUsage>),
    Unavailable,
}

const STATICS: GUID = GUID::from_u128(0x5074f851_950d_4165_9c15_365619481eea);
const PROFILE3: GUID = GUID::from_u128(0x578c2528_4cd9_4161_8045_201cfd5b115c);
const ASYNC_INFO: GUID = GUID::from_u128(0x00000036_0000_0000_c000_000000000046);

// Vtable slots. IInspectable takes the first six.
const QUERY_INTERFACE: usize = 0;
const RELEASE: usize = 2;
const STATICS_GET_PROFILES: usize = 6;
const PROFILE_CONNECTIVITY: usize = 7;
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

    fn get_u64(&self, slot: usize) -> Option<u64> {
        let mut out = 0;
        let hr = unsafe {
            let get: unsafe extern "system" fn(Raw, *mut u64) -> i32 = self.method(slot);
            get(self.0, &mut out)
        };
        (hr >= 0).then_some(out)
    }

    fn get_text(&self, slot: usize) -> Option<String> {
        unsafe {
            let mut text: Raw = std::ptr::null_mut();
            let get: unsafe extern "system" fn(Raw, *mut Raw) -> i32 = self.method(slot);
            if get(self.0, &mut text) < 0 {
                return None;
            }
            let mut len = 0u32;
            let chars = rt::WindowsGetStringRawBuffer(text as _, &mut len);
            let owned = if chars.is_null() || len == 0 {
                String::new()
            } else {
                String::from_utf16_lossy(std::slice::from_raw_parts(chars, len as usize))
            };
            rt::WindowsDeleteString(text as _);
            Some(owned)
        }
    }

    fn items(&self) -> Option<impl ExactSizeIterator<Item = Option<Object>> + '_> {
        let count = self.get_u32(VIEW_SIZE)?;
        Some((0..count).map(|index| {
            let mut out = std::ptr::null_mut();
            let hr = unsafe {
                let get: unsafe extern "system" fn(Raw, u32, *mut Raw) -> i32 = self.method(VIEW_GET_AT);
                get(self.0, index, &mut out)
            };
            Object::new(hr, out)
        }))
    }

    fn usage(&self) -> Option<(String, u64, u64)> {
        Some((self.get_text(USAGE_ID)?, self.get_u64(USAGE_RECEIVED)?, self.get_u64(USAGE_SENT)?))
    }
}

/// NetworkUsageStates with both filters on "don't care".
#[repr(C)]
#[derive(Clone, Copy)]
struct UsageStates {
    roaming: i32,
    shared: i32,
}

pub fn load(publish: impl FnMut(Vec<AppUsage>)) -> History {
    unsafe {
        let started = rt::RoInitialize(rt::RO_INIT_MULTITHREADED);
        let result = query(publish);
        if started >= 0 {
            rt::RoUninitialize();
        }
        result.unwrap_or(History::Unavailable)
    }
}

fn query(mut publish: impl FnMut(Vec<AppUsage>)) -> Option<History> {
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

    // Ask for connected profiles first. Disconnected profiles can still have
    // traffic recorded in the same 30-day window.
    let profile_list = factory.get_object(STATICS_GET_PROFILES)?;
    let items = profile_list.items()?;
    if items.len() == 0 {
        return Some(History::Complete(Vec::new()));
    }
    let mut failed = false;
    let mut profiles = Vec::new();
    for item in items {
        if let Some(profile) = item {
            profiles.push(profile);
        } else {
            failed = true;
        }
    }
    profiles.sort_by_cached_key(|profile| Reverse(profile.get_u32(PROFILE_CONNECTIVITY).unwrap_or(0)));
    let mut pending = Vec::new();
    for profile in profiles {
        let Some(profile) = profile.cast(&PROFILE3) else {
            failed = true;
            continue;
        };
        let mut raw = std::ptr::null_mut();
        let hr = unsafe {
            let start: unsafe extern "system" fn(Raw, i64, i64, UsageStates, *mut Raw) -> i32 =
                profile.method(PROFILE3_GET_USAGE);
            start(profile.0, from, to, UsageStates { roaming: 0, shared: 0 }, &mut raw)
        };
        let Some(operation) = Object::new(hr, raw) else {
            failed = true;
            continue;
        };
        let Some(info) = operation.cast(&ASYNC_INFO) else {
            failed = true;
            continue;
        };
        pending.push((operation, info));
    }
    if pending.is_empty() {
        return None;
    }
    let deadline = Instant::now() + Duration::from_secs(25);
    let devices = OnceCell::new();
    let packages = OnceCell::new();
    let mut totals: HashMap<String, AppUsage> = HashMap::new();
    let mut last_publish: Option<Instant> = None;
    let mut changed = false;
    let mut answered = false;
    while !pending.is_empty() && Instant::now() < deadline {
        let mut waiting = Vec::new();
        for (operation, info) in pending {
            match info.get_u32(INFO_STATUS) {
                Some(0) => waiting.push((operation, info)), // Started
                Some(1) => {
                    let Some(list) = operation.get_object(OPERATION_RESULTS) else {
                        failed = true;
                        continue;
                    };
                    let Some(items) = list.items() else {
                        failed = true;
                        continue;
                    };
                    // A profile with no recorded traffic is a valid answer.
                    answered |= items.len() == 0;
                    for item in items {
                        let Some(item) = item else {
                            failed = true;
                            continue;
                        };
                        let Some((id, received, sent)) = item.usage() else {
                            failed = true;
                            continue;
                        };
                        answered = true;
                        let total = totals.entry(id).or_insert_with_key(|id| {
                            name_app(id, item.get_text(USAGE_NAME).unwrap_or_default(), &devices, &packages)
                        });
                        total.received += received;
                        total.sent += sent;
                        changed = true;
                    }
                }
                _ => failed = true,
            }
        }
        pending = waiting;
        if !pending.is_empty() {
            // Show the first results immediately, then batch the remaining updates.
            if changed && last_publish.is_none_or(|last| last.elapsed() >= Duration::from_millis(500)) {
                publish(merge_apps(totals.values().cloned()));
                last_publish = Some(Instant::now());
                changed = false;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }
    answered.then(|| {
        let apps = merge_apps(totals.into_values());
        if failed || !pending.is_empty() {
            History::Incomplete(apps)
        } else {
            History::Complete(apps)
        }
    })
}

fn name_app(
    id: &str,
    given: String,
    devices: &OnceCell<DeviceMap>,
    packages: &OnceCell<super::packages::Packages>,
) -> AppUsage {
    let mut app = AppUsage { key: id.to_lowercase(), name: given, meta: None, received: 0, sent: 0 };
    if id.starts_with('\\') {
        let path = devices.get_or_init(DeviceMap::new).to_dos(id).unwrap_or_else(|| id.to_string());
        let info = meta::get(&path);
        app.name = if info.description.is_empty() { super::file_stem(&path) } else { info.description.clone() };
        app.key = path.to_lowercase();
        app.meta = Some(info);
    } else if id.is_empty() {
        app.name = "System".into();
        app.key = "system".into();
    } else {
        let packages = packages.get_or_init(super::packages::Packages::scan);
        if packages.knows(id) {
            app.name = packages.describe(id).name;
        } else if app.name.is_empty() {
            app.name = id.to_string();
        }
    }
    app
}

fn merge_apps(totals: impl IntoIterator<Item = AppUsage>) -> Vec<AppUsage> {
    let mut apps: HashMap<String, AppUsage> = HashMap::new();
    for app in totals {
        apps.entry(app.key.clone())
            .and_modify(|known| {
                known.received += app.received;
                known.sent += app.sent;
            })
            .or_insert(app);
    }
    let mut list: Vec<AppUsage> = apps.into_values().collect();
    list.sort_by_key(|app| Reverse(app.received + app.sent));
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "system" fn release_stub(_: Raw) -> u32 {
        0
    }

    #[test]
    fn vector_reads_distinguish_empty_vectors_from_missing_items() {
        #[repr(C)]
        struct View {
            vtable: *const *const c_void,
            size: Option<u32>,
        }

        unsafe extern "system" fn size(this: Raw, out: *mut u32) -> i32 {
            let Some(size) = (*this.cast::<View>()).size else { return -1 };
            *out = size;
            0
        }
        unsafe extern "system" fn missing_item(_: Raw, _: u32, _: *mut Raw) -> i32 {
            -1
        }

        let mut methods = [std::ptr::null(); VIEW_SIZE + 1];
        methods[RELEASE] = release_stub as *const c_void;
        methods[VIEW_GET_AT] = missing_item as *const c_void;
        methods[VIEW_SIZE] = size as *const c_void;
        let read = |size| {
            let mut raw = View { vtable: methods.as_ptr(), size };
            let view = Object(std::ptr::from_mut(&mut raw).cast());
            view.items().map(|items| items.map(|item| item.is_some()).collect::<Vec<_>>())
        };

        assert_eq!(read(None), None);
        assert_eq!(read(Some(0)), Some(vec![]));
        assert_eq!(read(Some(2)), Some(vec![false, false]));
    }

    #[test]
    fn an_unreadable_counter_is_distinct_from_zero_traffic() {
        unsafe extern "system" fn zero(_: Raw, out: *mut u64) -> i32 {
            *out = 0;
            0
        }
        unsafe extern "system" fn missing_counter(_: Raw, _: *mut u64) -> i32 {
            -1
        }
        unsafe extern "system" fn system_id(_: Raw, out: *mut Raw) -> i32 {
            *out = std::ptr::null_mut(); // A valid empty HSTRING represents System.
            0
        }

        let mut methods = [std::ptr::null(); USAGE_ID + 1];
        methods[RELEASE] = release_stub as *const c_void;
        methods[USAGE_ID] = system_id as *const c_void;
        methods[USAGE_RECEIVED] = zero as *const c_void;
        methods[USAGE_SENT] = zero as *const c_void;

        let read = |vtable: &[*const c_void]| {
            let mut header = vtable.as_ptr();
            let item = Object(std::ptr::from_mut(&mut header).cast());
            item.usage()
        };
        assert_eq!(read(&methods), Some((String::new(), 0, 0)));
        methods[USAGE_SENT] = missing_counter as *const c_void;
        assert_eq!(read(&methods), None);
    }

    #[test]
    #[ignore = "uses the real Windows 30-day history on a machine with multiple profiles"]
    fn live_history_first_rows() {
        let start = Instant::now();
        let mut first = None;
        let final_result = load(|rows| {
            first.get_or_insert_with(|| (start.elapsed(), rows));
        });
        let complete_at = start.elapsed();
        let History::Complete(apps) = final_result else { panic!("Windows did not return all profiles") };
        let (first_at, first_rows) = first.expect("publish nonempty rows while more profiles are loading");
        assert!(!first_rows.is_empty());

        let final_totals: HashMap<_, _> = apps.iter().map(|app| (app.key.as_str(), (app.received, app.sent))).collect();
        for app in &first_rows {
            assert!(final_totals
                .get(app.key.as_str())
                .is_some_and(|&(received, sent)| received >= app.received && sent >= app.sent));
        }
        println!("history: {} rows in {first_at:?}, {} rows in {complete_at:?}", first_rows.len(), apps.len());
    }
}
