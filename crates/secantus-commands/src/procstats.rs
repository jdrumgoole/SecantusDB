//! The process's own figures for `serverStatus.mem` and `extra_info`, read
//! from the operating system as mongod reads them: the resident and virtual
//! size in megabytes, and the page faults that needed I/O.
//!
//! Measured against mongod 8.2.11 on macOS (2026-10-15): `mem` is
//! `{bits, resident, virtual, supported}` as int32 megabytes (`ps` agrees
//! with both), and `extra_info` is `{note, page_faults}`, where a freshly
//! started mongod reports 1 -- the faults that read a page in, not every
//! fault. mongod's `secureAlloc*` fields describe its secure allocator,
//! which this server does not have, and are not reported.

use bson::{doc, Bson, Document};

/// Resident and virtual size of this process, in bytes.
#[cfg(target_os = "macos")]
fn memory_bytes() -> Option<(u64, u64)> {
    // <mach/task_info.h>, `#pragma pack(4)`.
    #[repr(C, packed(4))]
    #[derive(Default)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: [i32; 2],
        system_time: [i32; 2],
        policy: i32,
        suspend_count: i32,
    }
    const MACH_TASK_BASIC_INFO: i32 = 20;
    extern "C" {
        static mach_task_self_: u32;
        fn task_info(task: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
    }
    let mut info = MachTaskBasicInfo::default();
    let mut count = (std::mem::size_of::<MachTaskBasicInfo>() / 4) as u32;
    // SAFETY: `info` is the structure the flavour names, and `count` its size
    // in `natural_t` units, as the call requires.
    let rc = unsafe {
        task_info(
            mach_task_self_,
            MACH_TASK_BASIC_INFO,
            &mut info as *mut MachTaskBasicInfo as *mut i32,
            &mut count,
        )
    };
    (rc == 0).then_some((info.resident_size, info.virtual_size))
}

#[cfg(target_os = "linux")]
fn memory_bytes() -> Option<(u64, u64)> {
    // `/proc/self/statm`: total and resident size, in pages.
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let mut fields = statm.split_whitespace().map(|f| f.parse::<u64>().ok());
    let (size, resident) = (fields.next()??, fields.next()??);
    // SAFETY: `sysconf` has no preconditions.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page = u64::try_from(page).ok()?;
    Some((resident * page, size * page))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn memory_bytes() -> Option<(u64, u64)> {
    None
}

fn megabytes(bytes: u64) -> Bson {
    let mb = bytes / (1024 * 1024);
    i32::try_from(mb).map_or(Bson::Int64(mb as i64), Bson::Int32)
}

/// `serverStatus.mem`, or `None` where this platform's figures are not read.
pub fn mem() -> Option<Document> {
    let (resident, virtual_size) = memory_bytes()?;
    Some(doc! {
        "bits": (std::mem::size_of::<usize>() * 8) as i32,
        "resident": megabytes(resident),
        "virtual": megabytes(virtual_size),
        "supported": true,
    })
}

/// `serverStatus.extra_info`: the page faults that read a page in
/// (`getrusage`'s `ru_majflt`).
#[cfg(unix)]
pub fn extra_info() -> Option<Document> {
    // SAFETY: `getrusage` fills the zeroed structure it is given.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        (libc::getrusage(libc::RUSAGE_SELF, &mut usage) == 0).then_some(usage)
    }?;
    let faults = usage.ru_majflt as i64;
    Some(doc! {
        "note": "fields vary by platform",
        "page_faults": i32::try_from(faults).map_or(Bson::Int64(faults), Bson::Int32),
    })
}

#[cfg(not(unix))]
pub fn extra_info() -> Option<Document> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn the_process_has_a_resident_size() {
        let m = mem().expect("mem");
        assert!(m.get_i32("resident").unwrap() > 0, "{m}");
        assert!(m.get_i32("virtual").unwrap() >= m.get_i32("resident").unwrap());
        assert_eq!(m.get_i32("bits").unwrap(), 64);
    }

    #[test]
    #[cfg(unix)]
    fn extra_info_says_its_fields_vary() {
        let e = extra_info().expect("extra_info");
        assert_eq!(e.get_str("note").unwrap(), "fields vary by platform");
        assert!(e.contains_key("page_faults"));
    }
}
