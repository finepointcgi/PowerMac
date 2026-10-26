//! CPU / memory / network counters (Mach, sysctl, getifaddrs) and battery
//! details (IOKit power sources). Any thread.
#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_void, CStr};

use objc2::rc::Retained;

use super::ffi::*;
use crate::widgets::stats::{CpuTicks, NetCounters};
use crate::widgets::BatteryInfo;

extern "C" {
    fn mach_host_self() -> u32;
    fn host_statistics(host: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
    fn host_statistics64(host: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
    fn sysctlbyname(name: *const c_char, oldp: *mut c_void, oldlenp: *mut usize, newp: *mut c_void, newlen: usize) -> i32;
    fn getifaddrs(ifap: *mut *mut IfAddrs) -> i32;
    fn freeifaddrs(ifa: *mut IfAddrs);
}

const HOST_CPU_LOAD_INFO: i32 = 3;
const HOST_VM_INFO64: i32 = 4;
const AF_LINK: u8 = 18;

#[repr(C)]
struct IfAddrs {
    next: *mut IfAddrs,
    name: *const c_char,
    flags: u32,
    addr: *const SockAddr,
    netmask: *const c_void,
    dstaddr: *const c_void,
    data: *const c_void,
}

#[repr(C)]
struct SockAddr {
    len: u8,
    family: u8,
}

/// `struct if_data` up to the byte counters.
#[repr(C)]
struct IfData {
    head: [u8; 8],
    mtu: u32,
    metric: u32,
    baudrate: u32,
    ipackets: u32,
    ierrors: u32,
    opackets: u32,
    oerrors: u32,
    collisions: u32,
    ibytes: u32,
    obytes: u32,
}

#[repr(C)]
#[derive(Default)]
struct VmStats64 {
    free_count: u32,
    active_count: u32,
    inactive_count: u32,
    wire_count: u32,
    zero_fill_count: u64,
    reactivations: u64,
    pageins: u64,
    pageouts: u64,
    faults: u64,
    cow_faults: u64,
    lookups: u64,
    hits: u64,
    purges: u64,
    purgeable_count: u32,
    speculative_count: u32,
    decompressions: u64,
    compressions: u64,
    swapins: u64,
    swapouts: u64,
    compressor_page_count: u32,
    throttled_count: u32,
    external_page_count: u32,
    internal_page_count: u32,
    total_uncompressed_pages_in_compressor: u64,
}

fn host() -> u32 {
    use std::sync::OnceLock;
    static H: OnceLock<u32> = OnceLock::new();
    *H.get_or_init(|| unsafe { mach_host_self() })
}

pub fn cpu_ticks() -> Option<CpuTicks> {
    let mut t = [0u32; 4];
    let mut n: u32 = 4;
    let r = unsafe { host_statistics(host(), HOST_CPU_LOAD_INFO, t.as_mut_ptr() as *mut i32, &mut n) };
    (r == 0).then(|| CpuTicks { user: t[0] as u64, system: t[1] as u64, idle: t[2] as u64, nice: t[3] as u64 })
}

fn sysctl<T: Default + Copy>(name: &CStr) -> Option<T> {
    let mut v = T::default();
    let mut len = std::mem::size_of::<T>();
    let r = unsafe { sysctlbyname(name.as_ptr(), &mut v as *mut T as *mut c_void, &mut len, std::ptr::null_mut(), 0) };
    (r == 0 && len == std::mem::size_of::<T>()).then_some(v)
}

/// Memory in use (app + wired + compressed, like Activity Monitor's
/// "Memory Used") as a fraction, and the pressure level.
pub fn memory() -> Option<(f64, i32)> {
    let total: u64 = sysctl(c"hw.memsize")?;
    let page: u64 = sysctl::<u64>(c"hw.pagesize").or_else(|| sysctl::<u32>(c"hw.pagesize").map(|v| v as u64))?;
    let mut vm = VmStats64::default();
    let mut n = (std::mem::size_of::<VmStats64>() / 4) as u32;
    let r = unsafe { host_statistics64(host(), HOST_VM_INFO64, &mut vm as *mut VmStats64 as *mut i32, &mut n) };
    if r != 0 || total == 0 {
        return None;
    }
    let app = vm.internal_page_count.saturating_sub(vm.purgeable_count) as u64;
    let used = (app + vm.wire_count as u64 + vm.compressor_page_count as u64) * page;
    let level: i32 = sysctl(c"kern.memorystatus_vm_pressure_level").unwrap_or(1);
    Some(((used as f64 / total as f64).clamp(0.0, 1.0), level))
}

/// Byte counters of the interfaces that carry traffic (32-bit, wrapping).
pub fn net_counters() -> Vec<(String, NetCounters)> {
    let mut out = vec![];
    unsafe {
        let mut head: *mut IfAddrs = std::ptr::null_mut();
        if getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut p = head;
        while !p.is_null() {
            let ifa = &*p;
            if !ifa.addr.is_null() && (*ifa.addr).family == AF_LINK && !ifa.data.is_null() && !ifa.name.is_null() {
                let name = CStr::from_ptr(ifa.name).to_string_lossy().into_owned();
                if crate::widgets::stats::counts_interface(&name) {
                    let d = &*(ifa.data as *const IfData);
                    out.push((name, NetCounters { rx: d.ibytes as u64, tx: d.obytes as u64 }));
                }
            }
            p = ifa.next;
        }
        freeifaddrs(head);
    }
    out
}

/// The internal battery, if there is one.
pub fn battery() -> Option<BatteryInfo> {
    unsafe {
        let blob = IOPSCopyPowerSourcesInfo();
        if blob.is_null() {
            return None;
        }
        let list = IOPSCopyPowerSourcesList(blob);
        let mut out = None;
        if !list.is_null() {
            for i in 0..CFArrayGetCount(list) {
                let d = IOPSGetPowerSourceDescription(blob, CFArrayGetValueAtIndex(list, i));
                if d.is_null() {
                    continue;
                }
                let val = |k: &str| -> CFTypeRef {
                    let key = ns(k);
                    CFDictionaryGetValue(d, Retained::as_ptr(&key) as CFTypeRef)
                };
                let num = |k: &str| -> Option<i64> {
                    let v = val(k);
                    if v.is_null() {
                        return None;
                    }
                    let mut o: i64 = 0;
                    (CFNumberGetValue(v, 4, &mut o as *mut i64 as *mut c_void) != 0).then_some(o)
                };
                let string = |k: &str| -> Option<String> {
                    let v = val(k);
                    (!v.is_null()).then(|| (*(v as *const objc2_foundation::NSString)).to_string())
                };
                if string("Type").is_some_and(|t| t != "InternalBattery") {
                    continue;
                }
                let (Some(cur), Some(max)) = (num("Current Capacity"), num("Max Capacity")) else { continue };
                if max <= 0 {
                    continue;
                }
                let charging = {
                    let v = val("Is Charging");
                    !v.is_null() && CFBooleanGetValue(v) != 0
                };
                let on_ac = string("Power Source State").is_some_and(|s| s == "AC Power");
                let mins = |k: &str| num(k).filter(|m| *m > 0 && *m < 24 * 60).map(|m| m as u32);
                out = Some(BatteryInfo {
                    percent: ((cur * 100) / max).clamp(0, 100) as u8,
                    on_ac,
                    charging,
                    minutes_to_empty: if on_ac { None } else { mins("Time to Empty") },
                    minutes_to_full: if charging { mins("Time to Full Charge") } else { None },
                });
                break;
            }
            CFRelease(list);
        }
        CFRelease(blob);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_are_readable() {
        let a = cpu_ticks().expect("cpu ticks");
        std::thread::sleep(std::time::Duration::from_millis(60));
        let b = cpu_ticks().unwrap();
        assert!(b.idle + b.user + b.system >= a.idle + a.user + a.system);
        let (used, level) = memory().expect("memory");
        assert!(used > 0.01 && used <= 1.0, "{used}");
        assert!([1, 2, 4].contains(&level), "{level}");
        // Every Mac has at least one counted interface (en0 / bridge-less).
        let net = net_counters();
        assert!(net.iter().all(|(n, _)| crate::widgets::stats::counts_interface(n)));
        assert_eq!(std::mem::size_of::<VmStats64>(), 152);
        // battery() may be None on desktops; it must not crash.
        let _ = battery();
    }
}
