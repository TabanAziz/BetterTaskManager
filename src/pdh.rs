// Thin PDH (performance counter) wrapper. Used for what has no cheaper API:
// GPU engine utilisation per process, GPU memory, physical-disk activity and the
// CPU "% Processor Utility"/"% Processor Performance" values Task Manager shows.

use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
    PDH_MORE_DATA,
};

use crate::util::{from_wide_ptr, wide};

const PDH_FMT_NOCAP100: u32 = 0x0000_8000;
const PDH_CSTATUS_VALID_DATA: u32 = 0;
const PDH_CSTATUS_NEW_DATA: u32 = 1;

pub struct Pdh {
    query: isize,
    buf: Vec<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Counter(isize);

impl Pdh {
    pub fn open() -> Option<Pdh> {
        let mut q = 0isize;
        let st = unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut q) };
        if st != 0 {
            return None;
        }
        Some(Pdh { query: q, buf: Vec::new() })
    }

    pub fn add(&mut self, path: &str) -> Option<Counter> {
        let w = wide(path);
        let mut c = 0isize;
        let st = unsafe { PdhAddEnglishCounterW(self.query, w.as_ptr(), 0, &mut c) };
        if st != 0 {
            return None;
        }
        Some(Counter(c))
    }

    pub fn collect(&self) -> bool {
        unsafe { PdhCollectQueryData(self.query) == 0 }
    }

    pub fn value(&self, c: Counter) -> Option<f64> {
        let mut v: PDH_FMT_COUNTERVALUE = unsafe { std::mem::zeroed() };
        let st = unsafe {
            PdhGetFormattedCounterValue(c.0, PDH_FMT_DOUBLE | PDH_FMT_NOCAP100, std::ptr::null_mut(), &mut v)
        };
        if st != 0 || (v.CStatus != PDH_CSTATUS_VALID_DATA && v.CStatus != PDH_CSTATUS_NEW_DATA) {
            return None;
        }
        Some(unsafe { v.Anonymous.doubleValue })
    }

    /// All instances of a wildcard counter: (instance name, value).
    pub fn array(&mut self, c: Counter, out: &mut Vec<(String, f64)>) -> bool {
        out.clear();
        let mut size = 0u32;
        let mut count = 0u32;
        let st = unsafe {
            PdhGetFormattedCounterArrayW(
                c.0,
                PDH_FMT_DOUBLE | PDH_FMT_NOCAP100,
                &mut size,
                &mut count,
                std::ptr::null_mut(),
            )
        };
        if st != PDH_MORE_DATA || size == 0 {
            return false;
        }
        let words = (size as usize).div_ceil(8) + 1;
        if self.buf.len() < words {
            self.buf.resize(words, 0);
        }
        let items = self.buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        let st = unsafe {
            PdhGetFormattedCounterArrayW(c.0, PDH_FMT_DOUBLE | PDH_FMT_NOCAP100, &mut size, &mut count, items)
        };
        if st != 0 {
            return false;
        }
        out.reserve(count as usize);
        for i in 0..count as usize {
            let it = unsafe { &*items.add(i) };
            if it.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA && it.FmtValue.CStatus != PDH_CSTATUS_NEW_DATA {
                continue;
            }
            let name = unsafe { from_wide_ptr(it.szName) };
            out.push((name, unsafe { it.FmtValue.Anonymous.doubleValue }));
        }
        true
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}
