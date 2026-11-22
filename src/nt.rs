// Native (ntdll / gdi32 D3DKMT) declarations that windows-sys does not cover, and the
// zero-copy parser for SystemProcessInformation. One NtQuerySystemInformation call
// returns CPU times, memory, I/O, handle/thread counts and parent PIDs for every
// process, so the sampler never has to open a handle just to list processes.

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{HANDLE, LUID};

pub const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC0000004u32 as i32;
pub const STATUS_BUFFER_TOO_SMALL: i32 = 0xC0000023u32 as i32;

pub const SYSTEM_PROCESS_INFORMATION_CLASS: u32 = 5;
pub const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION_CLASS: u32 = 8;
pub const SYSTEM_PROCESS_ID_INFORMATION_CLASS: u32 = 88;
pub const SYSTEM_FULL_PROCESS_INFORMATION_CLASS: u32 = 148;

pub const PROCESS_POWER_THROTTLING_STATE_CLASS: u32 = 77;
pub const PROCESS_NETWORK_IO_COUNTERS_CLASS: u32 = 114;

#[link(name = "ntdll", kind = "raw-dylib")]
extern "system" {
    pub fn NtQuerySystemInformation(class: u32, buf: *mut c_void, len: u32, ret: *mut u32) -> i32;
    pub fn NtQueryInformationProcess(h: HANDLE, class: u32, buf: *mut c_void, len: u32, ret: *mut u32) -> i32;
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ProcessorPerf {
    pub idle: i64,
    pub kernel: i64,
    pub user: i64,
    pub dpc: i64,
    pub interrupt: i64,
    pub interrupt_count: u32,
    pub _pad: u32,
}

/// Per-logical-processor idle/kernel/user/DPC/interrupt times (100 ns units).
pub fn processor_times(n: usize) -> Vec<ProcessorPerf> {
    let mut v = vec![ProcessorPerf::default(); n.max(1)];
    let mut ret = 0u32;
    let st = unsafe {
        NtQuerySystemInformation(
            SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION_CLASS,
            v.as_mut_ptr() as *mut c_void,
            (std::mem::size_of::<ProcessorPerf>() * v.len()) as u32,
            &mut ret,
        )
    };
    if st < 0 {
        v.clear();
    } else {
        v.truncate(ret as usize / std::mem::size_of::<ProcessorPerf>());
    }
    v
}

/// Growable, reusable buffer for NtQuerySystemInformation process snapshots.
pub struct ProcBuf {
    buf: Vec<u64>, // u64 for 8-byte alignment
    len: usize,
}

impl ProcBuf {
    pub fn new() -> Self {
        ProcBuf { buf: vec![0u64; (2 << 20) / 8], len: 0 }
    }

    pub fn empty() -> Self {
        ProcBuf { buf: Vec::new(), len: 0 }
    }

    /// Fill with the given information class (5 = normal, 148 = full/elevated).
    pub fn query(&mut self, class: u32) -> bool {
        for _ in 0..8 {
            let mut ret = 0u32;
            let bytes = self.buf.len() * 8;
            let st = unsafe {
                NtQuerySystemInformation(class, self.buf.as_mut_ptr() as *mut c_void, bytes as u32, &mut ret)
            };
            if st == STATUS_INFO_LENGTH_MISMATCH || st == STATUS_BUFFER_TOO_SMALL {
                let want = (ret as usize).max(bytes) + (256 << 10);
                self.buf = vec![0u64; want / 8 + 1];
                continue;
            }
            if st < 0 {
                self.len = 0;
                return false;
            }
            self.len = ret as usize;
            return true;
        }
        false
    }

    pub fn iter(&self, full: bool) -> ProcIter<'_> {
        ProcIter {
            base: self.buf.as_ptr() as *const u8,
            len: self.len,
            off: 0,
            done: self.len == 0,
            full,
            _p: std::marker::PhantomData,
        }
    }
}

/// A view onto one SYSTEM_PROCESS_INFORMATION record (x64 layout).
#[derive(Clone, Copy)]
pub struct ProcView<'a> {
    p: *const u8,
    full: bool,
    _p: std::marker::PhantomData<&'a ()>,
}

pub struct ProcIter<'a> {
    base: *const u8,
    len: usize,
    off: usize,
    done: bool,
    full: bool,
    _p: std::marker::PhantomData<&'a ()>,
}

impl<'a> Iterator for ProcIter<'a> {
    type Item = ProcView<'a>;
    fn next(&mut self) -> Option<ProcView<'a>> {
        if self.done || self.off + 0x100 > self.len {
            return None;
        }
        let p = unsafe { self.base.add(self.off) };
        let next = unsafe { rd_u32(p, 0) } as usize;
        if next == 0 {
            self.done = true;
        } else {
            self.off += next;
        }
        Some(ProcView { p, full: self.full, _p: std::marker::PhantomData })
    }
}

#[inline]
unsafe fn rd_u32(p: *const u8, off: usize) -> u32 {
    std::ptr::read_unaligned(p.add(off) as *const u32)
}
#[inline]
unsafe fn rd_u64(p: *const u8, off: usize) -> u64 {
    std::ptr::read_unaligned(p.add(off) as *const u64)
}
#[inline]
unsafe fn rd_i32(p: *const u8, off: usize) -> i32 {
    std::ptr::read_unaligned(p.add(off) as *const i32)
}

const THREAD_INFO_SIZE: usize = 0x50;
const EXT_THREAD_INFO_SIZE: usize = 0x88;

impl<'a> ProcView<'a> {
    pub fn threads(&self) -> u32 {
        unsafe { rd_u32(self.p, 0x04) }
    }
    pub fn private_ws(&self) -> u64 {
        unsafe { rd_u64(self.p, 0x08) }
    }
    pub fn create_time(&self) -> u64 {
        unsafe { rd_u64(self.p, 0x20) }
    }
    pub fn user_time(&self) -> u64 {
        unsafe { rd_u64(self.p, 0x28) }
    }
    pub fn kernel_time(&self) -> u64 {
        unsafe { rd_u64(self.p, 0x30) }
    }
    pub fn image_name(&self) -> String {
        unsafe {
            let len = std::ptr::read_unaligned(self.p.add(0x38) as *const u16) as usize / 2;
            let buf = std::ptr::read_unaligned(self.p.add(0x40) as *const *const u16);
            if buf.is_null() || len == 0 {
                return String::new();
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(buf, len))
        }
    }
    pub fn base_priority(&self) -> i32 {
        unsafe { rd_i32(self.p, 0x48) }
    }
    pub fn pid(&self) -> u32 {
        unsafe { rd_u64(self.p, 0x50) as u32 }
    }
    pub fn ppid(&self) -> u32 {
        unsafe { rd_u64(self.p, 0x58) as u32 }
    }
    pub fn handles(&self) -> u32 {
        unsafe { rd_u32(self.p, 0x60) }
    }
    pub fn session(&self) -> u32 {
        unsafe { rd_u32(self.p, 0x64) }
    }
    pub fn working_set(&self) -> u64 {
        unsafe { rd_u64(self.p, 0x90) }
    }
    pub fn private_bytes(&self) -> u64 {
        unsafe { rd_u64(self.p, 0xC8) }
    }
    pub fn paged_pool(&self) -> u64 {
        unsafe { rd_u64(self.p, 0xA0) }
    }
    pub fn nonpaged_pool(&self) -> u64 {
        unsafe { rd_u64(self.p, 0xB0) }
    }
    /// (read, write, other) transfer byte counters.
    pub fn io_bytes(&self) -> (u64, u64, u64) {
        unsafe { (rd_u64(self.p, 0xE8), rd_u64(self.p, 0xF0), rd_u64(self.p, 0xF8)) }
    }
    /// True when every thread is suspended (Task Manager's "Angehalten").
    pub fn all_threads_suspended(&self) -> bool {
        let n = self.threads() as usize;
        if n == 0 {
            return false;
        }
        let stride = if self.full { EXT_THREAD_INFO_SIZE } else { THREAD_INFO_SIZE };
        for i in 0..n {
            let t = unsafe { self.p.add(0x100 + i * stride) };
            let state = unsafe { rd_u32(t, 0x44) };
            let reason = unsafe { rd_u32(t, 0x48) };
            if !(state == 5 && reason == 5) {
                return false;
            }
        }
        true
    }
    fn ext(&self) -> Option<*const u8> {
        if !self.full {
            return None;
        }
        let n = self.threads() as usize;
        Some(unsafe { self.p.add(0x100 + n * EXT_THREAD_INFO_SIZE) })
    }
    /// Physical disk bytes (read, written) attributed to the process. Elevated only.
    pub fn disk_bytes(&self) -> Option<(u64, u64)> {
        self.ext().map(|e| unsafe { (rd_u64(e, 0x00), rd_u64(e, 0x08)) })
    }
    /// Raw SID of the process owner (elevated snapshot only).
    pub fn user_sid(&self) -> Option<&'a [u8]> {
        let e = self.ext()?;
        let off = unsafe { rd_u32(e, 0x34) } as usize;
        if off == 0 {
            return None;
        }
        unsafe {
            let sid = self.p.add(off);
            let sub = *sid.add(1) as usize;
            if *sid != 1 || sub > 15 {
                return None;
            }
            Some(std::slice::from_raw_parts(sid, 8 + 4 * sub))
        }
    }
}

// ---------------------------------------------------------------------------
// Per-process queries on an already opened handle (PROCESS_QUERY_LIMITED_INFORMATION).

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct NetCounters {
    pub bytes_in: u64,
    pub bytes_out: u64,
}

pub fn process_net(h: HANDLE) -> Option<NetCounters> {
    let mut c = NetCounters::default();
    let mut ret = 0u32;
    let st = unsafe {
        NtQueryInformationProcess(h, PROCESS_NETWORK_IO_COUNTERS_CLASS, &mut c as *mut _ as *mut c_void, 16, &mut ret)
    };
    if st >= 0 {
        Some(c)
    } else {
        None
    }
}

/// EcoQoS ("Effizienzmodus") state: ControlMask & StateMask both carry EXECUTION_SPEED.
pub fn process_eco(h: HANDLE) -> bool {
    #[repr(C)]
    struct State {
        version: u32,
        control: u32,
        state: u32,
    }
    let mut s = State { version: 1, control: 0, state: 0 };
    let mut ret = 0u32;
    let st = unsafe {
        NtQueryInformationProcess(
            h,
            PROCESS_POWER_THROTTLING_STATE_CLASS,
            &mut s as *mut _ as *mut c_void,
            12,
            &mut ret,
        )
    };
    st >= 0 && (s.control & 1) != 0 && (s.state & 1) != 0
}

/// NT image path for any PID without opening the process
/// (e.g. "\Device\HarddiskVolume3\Windows\System32\svchost.exe").
pub fn image_path_by_pid(pid: u32) -> Option<String> {
    #[repr(C)]
    struct Info {
        pid: usize,
        length: u16,
        max_length: u16,
        _pad: u32,
        buffer: *mut u16,
    }
    let mut buf = vec![0u16; 520];
    for _ in 0..3 {
        let mut info = Info {
            pid: pid as usize,
            length: 0,
            max_length: (buf.len() * 2) as u16,
            _pad: 0,
            buffer: buf.as_mut_ptr(),
        };
        let st = unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESS_ID_INFORMATION_CLASS,
                &mut info as *mut _ as *mut c_void,
                std::mem::size_of::<Info>() as u32,
                std::ptr::null_mut(),
            )
        };
        if st == STATUS_INFO_LENGTH_MISMATCH && (info.max_length as usize) > buf.len() * 2 {
            buf = vec![0u16; info.max_length as usize / 2 + 1];
            continue;
        }
        if st < 0 || info.length == 0 {
            return None;
        }
        return Some(String::from_utf16_lossy(&buf[..info.length as usize / 2]));
    }
    None
}

// ---------------------------------------------------------------------------
// D3DKMT (gdi32) for GPU adapter details: name, memory sizes, temperature.

pub type D3DKMT_HANDLE = u32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3DKMT_ADAPTERINFO {
    pub hAdapter: D3DKMT_HANDLE,
    pub AdapterLuid: LUID,
    pub NumOfSources: u32,
    pub bPrecisePresentRegionsPreferred: i32,
}

#[repr(C)]
pub struct D3DKMT_ENUMADAPTERS2 {
    pub NumAdapters: u32,
    pub pAdapters: *mut D3DKMT_ADAPTERINFO,
}

#[repr(C)]
pub struct D3DKMT_QUERYADAPTERINFO {
    pub hAdapter: D3DKMT_HANDLE,
    pub Type: u32,
    pub pPrivateDriverData: *mut c_void,
    pub PrivateDriverDataSize: u32,
}

#[repr(C)]
pub struct D3DKMT_CLOSEADAPTER {
    pub hAdapter: D3DKMT_HANDLE,
}

pub const KMTQAITYPE_GETSEGMENTSIZE: u32 = 3;
pub const KMTQAITYPE_ADAPTERADDRESS: u32 = 6;
pub const KMTQAITYPE_ADAPTERREGISTRYINFO: u32 = 8;
pub const KMTQAITYPE_ADAPTERTYPE: u32 = 15;
pub const KMTQAITYPE_UMD_DRIVER_VERSION: u32 = 18;
pub const KMTQAITYPE_PHYSICALADAPTERPNPKEY: u32 = 41;
pub const KMTQAITYPE_ADAPTERPERFDATA: u32 = 62;

#[repr(C)]
#[derive(Default)]
pub struct D3DKMT_SEGMENTSIZEINFO {
    pub DedicatedVideoMemorySize: u64,
    pub DedicatedSystemMemorySize: u64,
    pub SharedSystemMemorySize: u64,
}

#[repr(C)]
#[derive(Default)]
pub struct D3DKMT_ADAPTERADDRESS {
    pub BusNumber: u32,
    pub DeviceNumber: u32,
    pub FunctionNumber: u32,
}

#[repr(C)]
pub struct D3DKMT_ADAPTERREGISTRYINFO {
    pub AdapterString: [u16; 260],
    pub BiosString: [u16; 260],
    pub DacType: [u16; 260],
    pub ChipType: [u16; 260],
}

#[repr(C)]
#[derive(Default)]
pub struct D3DKMT_ADAPTER_PERFDATA {
    pub PhysicalAdapterIndex: u32,
    pub MemoryFrequency: u64,
    pub MaxMemoryFrequency: u64,
    pub MaxMemoryFrequencyOC: u64,
    pub MemoryBandwidth: u64,
    pub PCIEBandwidth: u64,
    pub FanRPM: u32,
    pub Power: u32,
    pub Temperature: u32,
    pub PowerStateOverride: u8,
}

#[repr(C)]
pub struct D3DKMT_QUERY_PHYSICAL_ADAPTER_PNP_KEY {
    pub PhysicalAdapterIndex: u32,
    pub PnPKeyType: u32,
    pub pDest: *mut u16,
    pub pCchDest: *mut u32,
}

#[link(name = "gdi32", kind = "raw-dylib")]
extern "system" {
    pub fn D3DKMTEnumAdapters2(p: *mut D3DKMT_ENUMADAPTERS2) -> i32;
    pub fn D3DKMTQueryAdapterInfo(p: *mut D3DKMT_QUERYADAPTERINFO) -> i32;
    pub fn D3DKMTCloseAdapter(p: *const D3DKMT_CLOSEADAPTER) -> i32;
}

/// # Safety
/// `data` must point to a structure of `size` bytes matching `kind`.
pub unsafe fn kmt_query(h: D3DKMT_HANDLE, kind: u32, data: *mut c_void, size: u32) -> bool {
    let mut q =
        D3DKMT_QUERYADAPTERINFO { hAdapter: h, Type: kind, pPrivateDriverData: data, PrivateDriverDataSize: size };
    D3DKMTQueryAdapterInfo(&mut q) >= 0
}
