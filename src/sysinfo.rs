// Hardware facts that rarely change: CPU topology and caches, RAM modules (SMBIOS),
// physical disks, GPU adapters and network adapters. Gathered on the sampler thread
// after the first process list is on screen, so it never delays start-up.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
    IP_ADAPTER_ADDRESSES_LH,
};
use windows_sys::Win32::NetworkManagement::WiFi::{
    WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle, WlanQueryInterface,
    WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST,
};
use windows_sys::Win32::Networking::WinSock::{
    RtlIpv4AddressToStringW, RtlIpv6AddressToStringW, AF_INET, AF_INET6, IN6_ADDR, IN_ADDR, SOCKADDR_IN, SOCKADDR_IN6,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetDiskFreeSpaceExW, GetFileAttributesW, QueryDosDeviceW, FILE_SHARE_READ, FILE_SHARE_WRITE,
    INVALID_FILE_ATTRIBUTES, OPEN_EXISTING,
};
use windows_sys::Win32::System::Power::CallNtPowerInformation;
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows_sys::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, GetPhysicallyInstalledSystemMemory, GetSystemFirmwareTable, RelationAll,
};
use windows_sys::Win32::System::Threading::IsProcessorFeaturePresent;
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::model::{CpuInfo, DiskInfo, GpuInfo, MemInfo, NetInfo};
use crate::nt::*;
use crate::util::{from_wide, wide};

pub fn reg_string(hive: windows_sys::Win32::System::Registry::HKEY, key: &str, value: &str) -> Option<String> {
    let (key, value) = (wide(key), wide(value));
    let read = |into: *mut c_void, bytes: &mut u32| unsafe {
        let kind = std::ptr::null_mut();
        RegGetValueW(hive, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, kind, into, bytes) == 0
    };
    let mut bytes = 0u32;
    if !read(std::ptr::null_mut(), &mut bytes) {
        return None;
    }
    let mut buf: Vec<u16> = std::iter::repeat_n(0, bytes.div_ceil(2) as usize).collect();
    if !read(buf.as_mut_ptr().cast(), &mut bytes) {
        return None;
    }
    Some(from_wide(&buf).trim().to_string())
}

// ---------------------------------------------------------------- CPU

pub fn cpu_static() -> CpuInfo {
    let mut c = CpuInfo {
        name: reg_string(
            HKEY_LOCAL_MACHINE,
            "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0",
            "ProcessorNameString",
        )
        .unwrap_or_default(),
        ..Default::default()
    };
    // Topology + caches.
    unsafe {
        let mut len = 0u32;
        GetLogicalProcessorInformationEx(RelationAll, std::ptr::null_mut(), &mut len);
        if len > 0 {
            let mut buf = vec![0u8; len as usize];
            if GetLogicalProcessorInformationEx(RelationAll, buf.as_mut_ptr() as *mut _, &mut len) != 0 {
                let mut off = 0usize;
                while off + 8 <= len as usize {
                    let rel = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
                    let size = u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap()) as usize;
                    if size < 8 + 24 || off + size > buf.len() {
                        break;
                    }
                    let body = &buf[off + 8..off + size];
                    match rel {
                        0 => {
                            // ProcessorCore: count logical processors in its group masks.
                            c.cores += 1;
                            let group_count = u16::from_le_bytes([body[22], body[23]]) as usize;
                            for g in 0..group_count.max(1) {
                                let mo = 24 + g * 16;
                                if mo + 8 <= body.len() {
                                    let mask = u64::from_le_bytes(body[mo..mo + 8].try_into().unwrap());
                                    c.logical += mask.count_ones();
                                }
                            }
                        }
                        2 => {
                            let level = body[0];
                            let cache_size = u32::from_le_bytes(body[4..8].try_into().unwrap()) as u64;
                            match level {
                                1 => c.l1 += cache_size,
                                2 => c.l2 += cache_size,
                                3 => c.l3 += cache_size,
                                _ => {}
                            }
                        }
                        3 => c.sockets += 1,
                        _ => {}
                    }
                    off += size;
                }
            }
        }
    }
    // Base frequency from the power manager (MaxMhz == nominal clock).
    unsafe {
        let n = c.logical.max(1) as usize;
        let mut ppi = vec![[0u32; 6]; n];
        if CallNtPowerInformation(11, std::ptr::null(), 0, ppi.as_mut_ptr() as *mut c_void, (n * 24) as u32) == 0 {
            c.base_mhz = ppi[0][1];
        }
    }
    // Virtualisation: firmware-enabled VT-x/AMD-V, or a running hypervisor (Hyper-V/VBS
    // hides the VMX bit from the root partition but Task Manager still reports "enabled").
    let fw = unsafe { IsProcessorFeaturePresent(21) } != 0;
    #[cfg(target_arch = "x86_64")]
    let hv = (std::arch::x86_64::__cpuid(1).ecx >> 31) & 1 == 1;
    #[cfg(not(target_arch = "x86_64"))]
    let hv = false;
    c.virtualization = Some(fw || hv);
    c
}

// ---------------------------------------------------------------- Memory (SMBIOS)

/// A little-endian word, 0 where an older (shorter) SMBIOS record has no such field.
fn word(record: &[u8], at: usize) -> u32 {
    match record.get(at..at + 2) {
        Some(b) => u32::from(u16::from_le_bytes([b[0], b[1]])),
        None => 0,
    }
}

pub fn mem_static(total_phys: u64) -> MemInfo {
    let mut m = MemInfo::default();
    unsafe {
        let mut installed_kb = 0u64;
        if GetPhysicallyInstalledSystemMemory(&mut installed_kb) != 0 {
            let inst = installed_kb * 1024;
            m.hw_reserved = inst.saturating_sub(total_phys);
        }
        let sig = u32::from_be_bytes(*b"RSMB");
        let size = GetSystemFirmwareTable(sig, 0, std::ptr::null_mut(), 0);
        if size == 0 {
            return m;
        }
        let mut buf = vec![0u8; size as usize];
        if GetSystemFirmwareTable(sig, 0, buf.as_mut_ptr(), size) != size {
            return m;
        }
        if buf.len() < 8 {
            return m;
        }
        let table_len = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
        let data = &buf[8..(8 + table_len).min(buf.len())];
        let mut off = 0usize;
        let mut speeds = Vec::new();
        while off + 4 <= data.len() {
            let ty = data[off];
            let len = data[off + 1] as usize;
            if len < 4 || off + len > data.len() {
                break;
            }
            let s = &data[off..off + len];
            if ty == 17 && len >= 0x15 {
                m.slots_total += 1;
                let sz = u16::from_le_bytes([s[0x0C], s[0x0D]]);
                let installed = sz != 0 && sz != 0xFFFF;
                if installed {
                    m.slots_used += 1;
                    let ff = s[0x0E];
                    if m.form_factor.is_empty() {
                        m.form_factor = match ff {
                            0x09 => "DIMM",
                            0x0D => "SODIMM",
                            0x0B => "RIMM",
                            0x0F => "FB-DIMM",
                            0x10 => "Die",
                            _ => "",
                        }
                        .to_string();
                    }
                    let speed = word(s, 0x15);
                    let conf = word(s, 0x20);
                    let v = if conf != 0 && conf != 0xFFFF { conf } else { speed };
                    if v != 0 && v != 0xFFFF {
                        speeds.push(v);
                    }
                }
            }
            if ty == 127 {
                break;
            }
            // Skip the formatted area, then the string-set (ends with a double NUL).
            let mut p = off + len;
            while p + 1 < data.len() && !(data[p] == 0 && data[p + 1] == 0) {
                p += 1;
            }
            off = p + 2;
        }
        m.speed_mts = speeds.into_iter().max().unwrap_or(0);
    }
    m
}

// ---------------------------------------------------------------- Disks

const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;

pub fn disk_static(index: u32, letters: &str) -> DiskInfo {
    let mut d = DiskInfo { index, letters: letters.to_string(), ..Default::default() };
    let path = wide(&format!("\\\\.\\PhysicalDrive{}", index));
    unsafe {
        let h = CreateFileW(
            path.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if h != INVALID_HANDLE_VALUE {
            let mut out = vec![0u8; 1024];
            let mut ret = 0u32;
            let q: [u32; 3] = [0, 0, 0]; // StorageDeviceProperty, PropertyStandardQuery
            let mut bus = 0u32;
            let mut removable = false;
            if DeviceIoControl(
                h,
                IOCTL_STORAGE_QUERY_PROPERTY,
                q.as_ptr() as *const c_void,
                12,
                out.as_mut_ptr() as *mut c_void,
                out.len() as u32,
                &mut ret,
                std::ptr::null_mut(),
            ) != 0
                && ret >= 36
            {
                let rd = |o: usize| u32::from_le_bytes(out[o..o + 4].try_into().unwrap()) as usize;
                let cstr = |o: usize| -> String {
                    if o == 0 || o >= ret as usize {
                        return String::new();
                    }
                    let end = out[o..ret as usize].iter().position(|&b| b == 0).map(|e| o + e).unwrap_or(ret as usize);
                    String::from_utf8_lossy(&out[o..end]).trim().to_string()
                };
                removable = out[10] != 0;
                let vendor = cstr(rd(12));
                let product = cstr(rd(16));
                bus = rd(28) as u32;
                d.model = if !vendor.is_empty() && !product.to_lowercase().starts_with(&vendor.to_lowercase()) {
                    format!("{} {}", vendor, product)
                } else {
                    product
                };
            }
            // Seek penalty -> HDD; none -> SSD.
            let q2: [u32; 3] = [7, 0, 0];
            let mut sp = [0u8; 12];
            let mut ssd = None;
            if DeviceIoControl(
                h,
                IOCTL_STORAGE_QUERY_PROPERTY,
                q2.as_ptr() as *const c_void,
                12,
                sp.as_mut_ptr() as *mut c_void,
                12,
                &mut ret,
                std::ptr::null_mut(),
            ) != 0
                && ret >= 9
            {
                ssd = Some(sp[8] == 0);
            }
            let mut geo = [0u8; 256];
            if DeviceIoControl(
                h,
                IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
                std::ptr::null(),
                0,
                geo.as_mut_ptr() as *mut c_void,
                geo.len() as u32,
                &mut ret,
                std::ptr::null_mut(),
            ) != 0
                && ret >= 32
            {
                d.capacity = u64::from_le_bytes(geo[24..32].try_into().unwrap());
            }
            CloseHandle(h);
            let bus_name = match bus {
                1 => "SCSI",
                3 => "ATA",
                7 => "USB",
                8 => "RAID",
                10 => "SAS",
                11 => "SATA",
                12 => "SD",
                13 => "MMC",
                14 | 15 => "Virtual",
                16 => "Storage Spaces",
                17 => "NVMe",
                _ => "",
            };
            let base = if removable && bus == 7 {
                "Removable"
            } else {
                match ssd {
                    Some(true) => "SSD",
                    Some(false) => "HDD",
                    None => "Disk",
                }
            };
            d.kind = if bus_name.is_empty() { base.to_string() } else { format!("{} ({})", base, bus_name) };
        }
        // Formatted size, system disk, page file from the drive letters on this disk.
        let sysdrive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()).to_uppercase();
        for l in letters.split_whitespace() {
            let root = wide(&format!("{}\\", l));
            let mut total = 0u64;
            if GetDiskFreeSpaceExW(root.as_ptr(), std::ptr::null_mut(), &mut total, std::ptr::null_mut()) != 0 {
                d.formatted += total;
            }
            if l.eq_ignore_ascii_case(&sysdrive) {
                d.system = true;
            }
            let pf = wide(&format!("{}\\pagefile.sys", l));
            if GetFileAttributesW(pf.as_ptr()) != INVALID_FILE_ATTRIBUTES {
                d.pagefile = true;
            }
        }
    }
    d
}

// ---------------------------------------------------------------- GPUs

pub struct GpuAdapter {
    pub handle: D3DKMT_HANDLE,
    pub luid: u64,
    pub info: GpuInfo,
}

pub fn luid_key(high: i32, low: u32) -> u64 {
    ((high as u32 as u64) << 32) | low as u64
}

pub fn gpus_static() -> Vec<GpuAdapter> {
    let mut out = Vec::new();
    unsafe {
        let mut e = D3DKMT_ENUMADAPTERS2 { NumAdapters: 0, pAdapters: std::ptr::null_mut() };
        if D3DKMTEnumAdapters2(&mut e) < 0 || e.NumAdapters == 0 {
            return out;
        }
        let mut list: Vec<D3DKMT_ADAPTERINFO> = vec![std::mem::zeroed(); e.NumAdapters as usize];
        e.pAdapters = list.as_mut_ptr();
        if D3DKMTEnumAdapters2(&mut e) < 0 {
            return out;
        }
        list.truncate(e.NumAdapters as usize);
        let mut idx = 0u32;
        for a in list {
            let mut ty = 0u32;
            let has_type = kmt_query(a.hAdapter, KMTQAITYPE_ADAPTERTYPE, &mut ty as *mut _ as *mut c_void, 4);
            let render = ty & 1 != 0;
            let software = ty & 4 != 0;
            if has_type && (!render || software) {
                D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER { hAdapter: a.hAdapter });
                continue;
            }
            let mut reg: D3DKMT_ADAPTERREGISTRYINFO = std::mem::zeroed();
            let name = if kmt_query(
                a.hAdapter,
                KMTQAITYPE_ADAPTERREGISTRYINFO,
                &mut reg as *mut _ as *mut c_void,
                std::mem::size_of::<D3DKMT_ADAPTERREGISTRYINFO>() as u32,
            ) {
                from_wide(&reg.AdapterString)
            } else {
                String::new()
            };
            if name.contains("Microsoft Basic Render") {
                D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER { hAdapter: a.hAdapter });
                continue;
            }
            let mut seg = D3DKMT_SEGMENTSIZEINFO::default();
            kmt_query(a.hAdapter, KMTQAITYPE_GETSEGMENTSIZE, &mut seg as *mut _ as *mut c_void, 24);
            let mut addr = D3DKMT_ADAPTERADDRESS::default();
            let location = if kmt_query(a.hAdapter, KMTQAITYPE_ADAPTERADDRESS, &mut addr as *mut _ as *mut c_void, 12) {
                format!("PCI bus {}, device {}, function {}", addr.BusNumber, addr.DeviceNumber, addr.FunctionNumber)
            } else {
                String::new()
            };
            let mut ver = 0i64;
            let driver_version =
                if kmt_query(a.hAdapter, KMTQAITYPE_UMD_DRIVER_VERSION, &mut ver as *mut _ as *mut c_void, 8)
                    && ver != 0
                {
                    let v = ver as u64;
                    format!("{}.{}.{}.{}", (v >> 48) & 0xFFFF, (v >> 32) & 0xFFFF, (v >> 16) & 0xFFFF, v & 0xFFFF)
                } else {
                    String::new()
                };
            // Driver date (and a better name) from the adapter's software registry key.
            let mut driver_date = String::new();
            let mut key = vec![0u16; 512];
            let mut cch = key.len() as u32;
            let mut pnp = D3DKMT_QUERY_PHYSICAL_ADAPTER_PNP_KEY {
                PhysicalAdapterIndex: 0,
                PnPKeyType: 2,
                pDest: key.as_mut_ptr(),
                pCchDest: &mut cch,
            };
            if kmt_query(
                a.hAdapter,
                KMTQAITYPE_PHYSICALADAPTERPNPKEY,
                &mut pnp as *mut _ as *mut c_void,
                std::mem::size_of::<D3DKMT_QUERY_PHYSICAL_ADAPTER_PNP_KEY>() as u32,
            ) {
                let k = from_wide(&key);
                let rel = k.to_ascii_uppercase().find("\\SYSTEM\\").map(|i| k[i + 1..].to_string()).unwrap_or_default();
                if !rel.is_empty() {
                    if let Some(dd) = reg_string(HKEY_LOCAL_MACHINE, &rel, "DriverDate") {
                        // "M-D-YYYY" -> "D.M.YYYY"
                        let parts: Vec<&str> = dd.split('-').collect();
                        driver_date =
                            if parts.len() == 3 { format!("{}.{}.{}", parts[1], parts[0], parts[2]) } else { dd };
                    }
                }
            }
            let luid = luid_key(a.AdapterLuid.HighPart, a.AdapterLuid.LowPart);
            out.push(GpuAdapter {
                handle: a.hAdapter,
                luid,
                info: GpuInfo {
                    index: idx,
                    luid,
                    name,
                    ded_total: seg.DedicatedVideoMemorySize,
                    shared_total: seg.SharedSystemMemorySize,
                    driver_version,
                    driver_date,
                    location,
                    ..Default::default()
                },
            });
            idx += 1;
        }
    }
    out
}

/// GPU temperature in °C if the driver reports it (WDDM 2.4+ perf data).
pub fn gpu_temperature(h: D3DKMT_HANDLE) -> Option<f32> {
    let mut p = D3DKMT_ADAPTER_PERFDATA::default();
    let ok = unsafe {
        kmt_query(
            h,
            KMTQAITYPE_ADAPTERPERFDATA,
            &mut p as *mut _ as *mut c_void,
            std::mem::size_of::<D3DKMT_ADAPTER_PERFDATA>() as u32,
        )
    };
    if ok && p.Temperature > 0 && p.Temperature < 2000 {
        Some(p.Temperature as f32 / 10.0)
    } else {
        None
    }
}

// ---------------------------------------------------------------- Network adapters

pub fn net_adapters() -> Vec<NetInfo> {
    let mut out = Vec::new();
    unsafe {
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size = 32u32 << 10;
        let mut buf: Vec<u64> = Vec::new();
        for _ in 0..4 {
            buf = vec![0u64; size as usize / 8 + 1];
            let r = GetAdaptersAddresses(
                0,
                flags,
                std::ptr::null(),
                buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH,
                &mut size,
            );
            if r == 111 {
                continue; // ERROR_BUFFER_OVERFLOW
            }
            if r != 0 {
                return out;
            }
            break;
        }
        let wlan = wlan_info();
        // Task Manager hides adapters without a physical medium (Hyper-V vEthernet, WAN
        // miniports); VPN adapters that present as 802.3 stay visible.
        let mut medium: std::collections::HashMap<u64, u32> = std::collections::HashMap::new();
        {
            use windows_sys::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
            let mut t: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
            if GetIfTable2(&mut t) == 0 && !t.is_null() {
                let rows = std::slice::from_raw_parts((*t).Table.as_ptr(), (*t).NumEntries as usize);
                for r in rows {
                    medium.insert(r.InterfaceLuid.Value, r.PhysicalMediumType as u32);
                }
                FreeMibTable(t as *const c_void);
            }
        }
        let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !p.is_null() {
            let a = &*p;
            p = a.Next;
            let up = a.OperStatus == 1;
            let (title, ctype) = match a.IfType {
                6 => ("Ethernet", "Ethernet"),
                71 => ("Wi-Fi", "Wi-Fi"),
                243 | 244 => ("Cellular", "Cellular"),
                _ => continue,
            };
            if !up || medium.get(&a.Luid.Value).copied().unwrap_or(1) == 0 {
                continue;
            }
            let mut n = NetInfo {
                luid: a.Luid.Value,
                title: title.to_string(),
                alias: crate::util::from_wide_ptr(a.FriendlyName),
                description: crate::util::from_wide_ptr(a.Description),
                conn_type: ctype.to_string(),
                link_bps: a.ReceiveLinkSpeed.max(a.TransmitLinkSpeed),
                ..Default::default()
            };
            let mut ua = a.FirstUnicastAddress;
            let mut v6_link = String::new();
            while !ua.is_null() {
                let u = &*ua;
                ua = u.Next;
                let sa = u.Address.lpSockaddr;
                if sa.is_null() {
                    continue;
                }
                let fam = (*sa).sa_family;
                let mut s = [0u16; 64];
                if fam == AF_INET && n.ipv4.is_empty() {
                    let sin = &*(sa as *const SOCKADDR_IN);
                    RtlIpv4AddressToStringW(&sin.sin_addr as *const IN_ADDR, s.as_mut_ptr());
                    n.ipv4 = from_wide(&s);
                } else if fam == AF_INET6 {
                    let sin6 = &*(sa as *const SOCKADDR_IN6);
                    RtlIpv6AddressToStringW(&sin6.sin6_addr as *const IN6_ADDR, s.as_mut_ptr());
                    let txt = from_wide(&s);
                    if txt.to_ascii_lowercase().starts_with("fe80") {
                        if v6_link.is_empty() {
                            v6_link = txt;
                        }
                    } else if n.ipv6.is_empty() {
                        n.ipv6 = txt;
                    }
                }
            }
            if n.ipv6.is_empty() {
                n.ipv6 = v6_link;
            }
            if a.IfType == 71 {
                let name = std::ffi::CStr::from_ptr(a.AdapterName as *const i8).to_string_lossy().to_lowercase();
                if let Some(w) = wlan.iter().find(|w| name.contains(&w.guid)) {
                    n.ssid = w.ssid.clone();
                    n.signal = w.signal;
                    if !w.phy.is_empty() {
                        n.conn_type = w.phy.clone();
                    }
                }
            }
            out.push(n);
        }
    }
    out
}

struct WlanConn {
    guid: String,
    ssid: String,
    signal: Option<u32>,
    phy: String,
}

fn wlan_info() -> Vec<WlanConn> {
    let mut out = Vec::new();
    unsafe {
        let mut ver = 0u32;
        let mut h = std::ptr::null_mut();
        if WlanOpenHandle(2, std::ptr::null(), &mut ver, &mut h) != 0 {
            return out;
        }
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        if WlanEnumInterfaces(h, std::ptr::null(), &mut list) == 0 && !list.is_null() {
            let n = (*list).dwNumberOfItems as usize;
            let items = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), n);
            for it in items {
                let g = it.InterfaceGuid;
                let guid = format!(
                    "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
                    g.data1,
                    g.data2,
                    g.data3,
                    g.data4[0],
                    g.data4[1],
                    g.data4[2],
                    g.data4[3],
                    g.data4[4],
                    g.data4[5],
                    g.data4[6],
                    g.data4[7]
                );
                let mut size = 0u32;
                let mut data: *mut c_void = std::ptr::null_mut();
                let mut conn = WlanConn { guid, ssid: String::new(), signal: None, phy: String::new() };
                if WlanQueryInterface(h, &g, 7, std::ptr::null(), &mut size, &mut data, std::ptr::null_mut()) == 0
                    && !data.is_null()
                {
                    let ca = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                    let aa = &ca.wlanAssociationAttributes;
                    let l = (aa.dot11Ssid.uSSIDLength as usize).min(32);
                    conn.ssid = String::from_utf8_lossy(&aa.dot11Ssid.ucSSID[..l]).to_string();
                    if conn.ssid.is_empty() {
                        conn.ssid = from_wide(&ca.strProfileName);
                    }
                    conn.signal = Some(aa.wlanSignalQuality);
                    conn.phy = match aa.dot11PhyType {
                        4 => "802.11a",
                        5 => "802.11b",
                        6 => "802.11g",
                        7 => "802.11n",
                        8 => "802.11ac",
                        9 => "802.11ad",
                        10 => "802.11ax",
                        11 => "802.11be",
                        _ => "",
                    }
                    .to_string();
                    WlanFreeMemory(data);
                }
                out.push(conn);
            }
            WlanFreeMemory(list as *const c_void);
        }
        WlanCloseHandle(h, std::ptr::null());
    }
    out
}

// ---------------------------------------------------------------- NT -> DOS paths

pub struct DeviceMap {
    map: Vec<(String, String)>, // ("\device\harddiskvolume3", "C:")
}

impl DeviceMap {
    pub fn new() -> Self {
        let mut map = Vec::new();
        for l in b'A'..=b'Z' {
            let drive = format!("{}:", l as char);
            let w = wide(&drive);
            let mut buf = [0u16; 512];
            let n = unsafe { QueryDosDeviceW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
            if n > 0 {
                let target = from_wide(&buf).to_lowercase();
                if !target.is_empty() {
                    map.push((target, drive));
                }
            }
        }
        DeviceMap { map }
    }

    pub fn to_dos(&self, nt: &str) -> Option<String> {
        let lower = nt.to_lowercase();
        for (dev, drive) in &self.map {
            if lower.starts_with(dev.as_str()) && lower.as_bytes().get(dev.len()) == Some(&b'\\') {
                return Some(format!("{}{}", drive, &nt[dev.len()..]));
            }
        }
        if lower.starts_with("\\device\\mup\\") {
            return Some(format!("\\\\{}", &nt[12..]));
        }
        if lower.starts_with("\\??\\") {
            return Some(nt[4..].to_string());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn short_smbios_records_do_not_panic() {
        let smbios_2_1 = [0u8; 0x15]; // has no speed field
        assert_eq!(super::word(&smbios_2_1, 0x15), 0);
        assert_eq!(super::word(&smbios_2_1, 0x20), 0);
    }
}
