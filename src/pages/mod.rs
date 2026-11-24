//! Data behind the App history, Startup apps, Users and Services pages.
//!
//! Nothing here runs until one of those pages is opened, and everything runs on a
//! short-lived worker thread, so start-up and the live pages pay nothing for them.

pub mod history;
pub mod packages;
pub mod services;
pub mod startup;
pub mod users;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Posted when a feed has new data.
pub const WM_APP_LIST: u32 = 0x8000 + 4;

pub enum Data {
    History(history::History),
    Startup(Vec<startup::Entry>),
    Users(Vec<users::Session>),
    Services(Result<Vec<services::Service>, String>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Feed {
    History,
    Startup,
    Users,
    Services,
}

static BUSY: [AtomicBool; 4] = [const { AtomicBool::new(false) }; 4];
static READY: Mutex<Vec<Data>> = Mutex::new(Vec::new());

/// Starts loading unless that feed is already loading.
pub fn request(feed: Feed, notify: HWND) {
    let busy = &BUSY[feed as usize];
    if busy.swap(true, Ordering::AcqRel) {
        return;
    }
    let window = notify as isize;
    let spawned = std::thread::Builder::new().name("page data".into()).spawn(move || {
        let data = match feed {
            Feed::History => Data::History(history::load(|apps| {
                enqueue(Data::History(history::History::Loading(apps)));
                unsafe { PostMessageW(window as HWND, WM_APP_LIST, 0, 0) };
            })),
            Feed::Startup => Data::Startup(startup::load()),
            Feed::Users => Data::Users(users::load()),
            Feed::Services => Data::Services(services::load()),
        };
        enqueue(data);
        BUSY[feed as usize].store(false, Ordering::Release);
        unsafe { PostMessageW(window as HWND, WM_APP_LIST, 0, 0) };
    });
    if spawned.is_err() {
        busy.store(false, Ordering::Release);
    }
}

fn enqueue(data: Data) {
    if let Ok(mut ready) = READY.lock() {
        ready.push(data);
    }
}

pub fn take() -> Vec<Data> {
    READY.lock().map(|mut ready| std::mem::take(&mut *ready)).unwrap_or_default()
}

pub fn file_stem(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    match file.rfind('.') {
        Some(dot) if dot > 0 => file[..dot].to_string(),
        _ => file.to_string(),
    }
}

/// Reads the real system. Run with `cargo test live_ -- --ignored --nocapture`.
#[cfg(test)]
mod live {
    use super::*;
    use std::time::Instant;

    #[test]
    #[ignore]
    fn live_feeds() {
        let t = Instant::now();
        let (history::History::Complete(apps) | history::History::Incomplete(apps)) = history::load(|_| {}) else {
            panic!("Windows history unavailable");
        };
        let total: u64 = apps.iter().map(|a| a.received + a.sent).sum();
        let named = apps.iter().filter(|a| !a.name.is_empty()).count();
        println!("history: {} apps, {named} named, {} MB, {:?}", apps.len(), total >> 20, t.elapsed());
        for app in apps.iter().take(8) {
            println!("   {} | rx {} tx {}", app.name, app.received >> 20, app.sent >> 20);
        }

        let t = Instant::now();
        let entries = startup::load();
        println!("startup: {} entries, {:?}", entries.len(), t.elapsed());
        for e in &entries {
            println!("   {:<34} | {:<30} | {:?} on={}", e.name, e.publisher, e.source, e.enabled);
        }

        let t = Instant::now();
        let sessions = users::load();
        let with_mail = sessions.iter().filter(|s| s.name.contains('@')).count();
        println!("users: {} sessions, {with_mail} with account mail, {:?}", sessions.len(), t.elapsed());

        let t = Instant::now();
        let list = services::load().expect("services");
        let running = list.iter().filter(|s| s.state == services::State::Running).count();
        let grouped = list.iter().filter(|s| !s.group.is_empty()).count();
        println!("services: {} total, {running} running, {grouped} grouped, {:?}", list.len(), t.elapsed());
        for s in list.iter().take(4) {
            println!("   {} | {} | {} | {}", s.name, s.pid, s.display, s.group);
        }
        assert!(!apps.is_empty() && !entries.is_empty() && !sessions.is_empty() && running > 0);
    }
}
