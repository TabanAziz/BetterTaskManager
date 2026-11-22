//! Users: signed-in sessions. Usage is summed from the live process snapshot by the page.

use windows_sys::Win32::NetworkManagement::NetManagement as net;
use windows_sys::Win32::System::RemoteDesktop as wts;

use crate::util::{from_wide_ptr, wide};

#[derive(Clone)]
pub struct Session {
    pub id: u32,
    /// The account's e-mail for Microsoft accounts (as in Task Manager), else its user name.
    pub name: String,
    pub disconnected: bool,
}

pub fn load() -> Vec<Session> {
    let mut found = Vec::new();
    unsafe {
        let mut list: *mut wts::WTS_SESSION_INFOW = std::ptr::null_mut();
        let mut count = 0u32;
        if wts::WTSEnumerateSessionsW(std::ptr::null_mut(), 0, 1, &mut list, &mut count) == 0 {
            return found;
        }
        for info in std::slice::from_raw_parts(list, count as usize) {
            let user = query(info.SessionId, wts::WTSUserName);
            if user.is_empty() {
                continue; // service and listener sessions
            }
            let domain = query(info.SessionId, wts::WTSDomainName);
            found.push(Session {
                id: info.SessionId,
                name: account_mail(&domain, &user).unwrap_or(user),
                disconnected: info.State == wts::WTSDisconnected,
            });
        }
        wts::WTSFreeMemory(list.cast());
    }
    found
}

unsafe fn query(session: u32, what: wts::WTS_INFO_CLASS) -> String {
    let mut text: *mut u16 = std::ptr::null_mut();
    let mut bytes = 0u32;
    if wts::WTSQuerySessionInformationW(std::ptr::null_mut(), session, what, &mut text, &mut bytes) == 0 {
        return String::new();
    }
    let value = from_wide_ptr(text);
    wts::WTSFreeMemory(text.cast());
    value
}

/// Only for accounts of this PC: asking a domain controller could block for seconds.
fn account_mail(domain: &str, user: &str) -> Option<String> {
    let computer = std::env::var("COMPUTERNAME").ok()?;
    if !domain.eq_ignore_ascii_case(&computer) && !domain.eq_ignore_ascii_case("MicrosoftAccount") {
        return None;
    }
    let user = wide(user);
    unsafe {
        let mut buffer: *mut u8 = std::ptr::null_mut();
        if net::NetUserGetInfo(std::ptr::null(), user.as_ptr(), 24, &mut buffer) != 0 || buffer.is_null() {
            return None;
        }
        let info = &*(buffer as *const net::USER_INFO_24);
        let mail = (info.usri24_internet_identity != 0).then(|| from_wide_ptr(info.usri24_internet_principal_name));
        net::NetApiBufferFree(buffer as *const _);
        mail.filter(|m| !m.is_empty())
    }
}

pub fn disconnect(session: u32) -> bool {
    unsafe { wts::WTSDisconnectSession(std::ptr::null_mut(), session, 0) != 0 }
}

pub fn sign_off(session: u32) -> bool {
    unsafe { wts::WTSLogoffSession(std::ptr::null_mut(), session, 0) != 0 }
}
