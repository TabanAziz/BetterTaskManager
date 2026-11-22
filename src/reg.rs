//! Small registry helpers for the Startup, Services and App history pages.

use windows_sys::Win32::System::Registry as r;

use crate::util::{from_wide, wide};

pub use r::{HKEY, HKEY_CURRENT_USER as USER, HKEY_LOCAL_MACHINE as MACHINE};

pub struct Value {
    pub name: String,
    pub kind: u32,
    pub data: Vec<u8>,
}

impl Value {
    pub fn text(&self) -> Option<String> {
        if self.kind != r::REG_SZ && self.kind != r::REG_EXPAND_SZ {
            return None;
        }
        let units: Vec<u16> = self.data.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        Some(from_wide(&units))
    }

    pub fn dword(&self) -> Option<u32> {
        (self.kind == r::REG_DWORD).then_some(())?;
        Some(u32::from_le_bytes(self.data.get(..4)?.try_into().ok()?))
    }
}

struct Key(r::HKEY);

impl Key {
    fn read(hive: HKEY, path: &str) -> Option<Key> {
        let path = wide(path);
        let mut key = std::ptr::null_mut();
        let status = unsafe { r::RegOpenKeyExW(hive, path.as_ptr(), 0, r::KEY_READ, &mut key) };
        (status == 0).then_some(Key(key))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { r::RegCloseKey(self.0) };
    }
}

pub fn subkeys(hive: HKEY, path: &str) -> Vec<String> {
    let Some(key) = Key::read(hive, path) else { return Vec::new() };
    let mut found = Vec::new();
    let mut name = [0u16; 256]; // key names are at most 255 characters
    for index in 0.. {
        let mut len = name.len() as u32;
        let none = std::ptr::null_mut();
        let status = unsafe {
            r::RegEnumKeyExW(key.0, index, name.as_mut_ptr(), &mut len, none as _, none, none as _, none as _)
        };
        if status != 0 {
            break;
        }
        found.push(from_wide(&name[..len as usize]));
    }
    found
}

pub fn values(hive: HKEY, path: &str) -> Vec<Value> {
    const MORE_DATA: u32 = 234;
    let Some(key) = Key::read(hive, path) else { return Vec::new() };
    let mut found = Vec::new();
    let mut name = vec![0u16; 16_384];
    let mut data = vec![0u8; 4096];
    let mut index = 0;
    loop {
        let (mut name_len, mut data_len, mut kind) = (name.len() as u32, data.len() as u32, 0u32);
        let status = unsafe {
            r::RegEnumValueW(
                key.0,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                std::ptr::null_mut(),
                &mut kind,
                data.as_mut_ptr(),
                &mut data_len,
            )
        };
        match status {
            0 => {
                let data = data[..data_len as usize].to_vec();
                found.push(Value { name: from_wide(&name[..name_len as usize]), kind, data });
                index += 1;
            }
            MORE_DATA if (data_len as usize) > data.len() => data.resize(data_len as usize, 0),
            _ => break,
        }
    }
    found
}

pub fn string(hive: HKEY, path: &str, name: &str) -> Option<String> {
    crate::sysinfo::reg_string(hive, path, name)
}

fn set(hive: HKEY, path: &str, name: &str, kind: u32, data: &[u8]) -> Result<(), u32> {
    let (path, name) = (wide(path), wide(name));
    let status = unsafe {
        r::RegSetKeyValueW(hive, path.as_ptr(), name.as_ptr(), kind, data.as_ptr().cast(), data.len() as u32)
    };
    if status == 0 {
        Ok(())
    } else {
        Err(status)
    }
}

pub fn set_binary(hive: HKEY, path: &str, name: &str, data: &[u8]) -> Result<(), u32> {
    set(hive, path, name, r::REG_BINARY, data)
}

pub fn set_dword(hive: HKEY, path: &str, name: &str, value: u32) -> Result<(), u32> {
    set(hive, path, name, r::REG_DWORD, &value.to_le_bytes())
}

pub fn set_string(hive: HKEY, path: &str, name: &str, value: &str) -> Result<(), u32> {
    let bytes: Vec<u8> = wide(value).into_iter().flat_map(u16::to_le_bytes).collect();
    set(hive, path, name, r::REG_SZ, &bytes)
}
