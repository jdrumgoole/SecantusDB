//! What `hostInfo` reports about the machine, read from the operating
//! system: `sysctl` on macOS, `/proc` and `sysconf` on Linux. A figure a
//! platform cannot give is `None`, and the reply leaves it out.

use bson::Document;

pub(crate) struct Host {
    pub hostname: String,
    /// `uname -m`: `arm64` on Apple silicon, `x86_64`, `aarch64` on Linux.
    pub machine: String,
    pub mem_size_mb: Option<i64>,
    pub mem_limit_mb: Option<i64>,
    pub logical_cores: i32,
    pub available_cores: i32,
    pub physical_cores: Option<i32>,
    pub sockets: Option<i32>,
    pub os_type: String,
    pub os_name: String,
    pub os_version: String,
    pub extra: Document,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn uname() -> (String, String) {
    // SAFETY: `uname` fills the zeroed struct; each field is NUL-terminated.
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut u) != 0 {
            return (String::new(), std::env::consts::ARCH.to_string());
        }
        let field = |f: &[libc::c_char]| {
            std::ffi::CStr::from_ptr(f.as_ptr())
                .to_string_lossy()
                .into_owned()
        };
        (field(&u.nodename), field(&u.machine))
    }
}

fn available_cores() -> i32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as i32)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    fn sysctl_bytes(name: &str) -> Option<Vec<u8>> {
        let cname = std::ffi::CString::new(name).ok()?;
        let mut len: libc::size_t = 0;
        // SAFETY: a null buffer asks for the length; the second call fills
        // a buffer of that length.
        unsafe {
            if libc::sysctlbyname(
                cname.as_ptr(),
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            ) != 0
            {
                return None;
            }
            let mut buf = vec![0u8; len];
            if libc::sysctlbyname(
                cname.as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            ) != 0
            {
                return None;
            }
            buf.truncate(len);
            Some(buf)
        }
    }

    fn sysctl_int(name: &str) -> Option<i64> {
        let b = sysctl_bytes(name)?;
        match b.len() {
            4 => Some(i64::from(i32::from_ne_bytes(b[..4].try_into().ok()?))),
            8 => Some(i64::from_ne_bytes(b[..8].try_into().ok()?)),
            _ => None,
        }
    }

    fn sysctl_string(name: &str) -> Option<String> {
        let b = sysctl_bytes(name)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        Some(String::from_utf8_lossy(&b[..end]).into_owned())
    }

    pub(super) fn read() -> Host {
        let (nodename, machine) = uname();
        let mem = sysctl_int("hw.memsize").map(|b| b / (1024 * 1024));
        let logical = sysctl_int("hw.logicalcpu").map_or_else(available_cores, |n| n as i32);
        let mut extra = Document::new();
        if let Some(v) = sysctl_string("kern.version") {
            extra.insert("versionString", v);
        }
        extra.insert("alwaysFullSync", 0_i32);
        extra.insert("nfsAsync", 0_i32);
        if let Some(v) = sysctl_string("hw.model") {
            extra.insert("model", v);
        }
        if let Some(v) = sysctl_string("machdep.cpu.brand_string") {
            extra.insert("cpuString", v);
        }
        if let Some(v) = sysctl_int("hw.pagesize") {
            extra.insert("pageSize", v as i32);
        }
        Host {
            hostname: nodename,
            machine,
            mem_size_mb: mem,
            mem_limit_mb: mem,
            logical_cores: logical,
            available_cores: available_cores(),
            physical_cores: sysctl_int("hw.physicalcpu").map(|n| n as i32),
            sockets: sysctl_int("hw.packages").map(|n| n as i32),
            os_type: "Darwin".into(),
            os_name: "Mac OS X".into(),
            os_version: sysctl_string("kern.osrelease").unwrap_or_default(),
            extra,
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    fn meminfo_mb() -> Option<i64> {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
        let kb: i64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb / 1024)
    }

    /// Distinct `(physical id, core id)` pairs and distinct `physical id`s.
    fn cpuinfo() -> (Option<i32>, Option<i32>) {
        let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") else {
            return (None, None);
        };
        let mut cores = std::collections::BTreeSet::new();
        let mut sockets = std::collections::BTreeSet::new();
        let mut physical = None;
        for line in text.lines() {
            let mut parts = line.splitn(2, ':');
            let (Some(k), Some(v)) = (parts.next(), parts.next()) else {
                continue;
            };
            match k.trim() {
                "physical id" => {
                    physical = Some(v.trim().to_string());
                    sockets.insert(v.trim().to_string());
                }
                "core id" => {
                    cores.insert((physical.clone(), v.trim().to_string()));
                }
                _ => {}
            }
        }
        let count = |n: usize| (n > 0).then_some(n as i32);
        (count(cores.len()), count(sockets.len()))
    }

    fn os_name() -> String {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|t| {
                t.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "Linux".into())
    }

    pub(super) fn read() -> Host {
        let (nodename, machine) = uname();
        let (physical, sockets) = cpuinfo();
        // SAFETY: sysconf has no preconditions.
        let online = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
        let release = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let mem = meminfo_mb();
        Host {
            hostname: nodename,
            machine,
            mem_size_mb: mem,
            mem_limit_mb: mem,
            logical_cores: if online > 0 {
                online as i32
            } else {
                available_cores()
            },
            available_cores: available_cores(),
            physical_cores: physical,
            sockets,
            os_type: "Linux".into(),
            os_name: os_name(),
            os_version: release,
            extra: Document::new(),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::*;

    pub(super) fn read() -> Host {
        Host {
            hostname: std::env::var("COMPUTERNAME").unwrap_or_default(),
            machine: std::env::consts::ARCH.into(),
            mem_size_mb: None,
            mem_limit_mb: None,
            logical_cores: available_cores(),
            available_cores: available_cores(),
            physical_cores: None,
            sockets: None,
            os_type: std::env::consts::OS.into(),
            os_name: std::env::consts::OS.into(),
            os_version: String::new(),
            extra: Document::new(),
        }
    }
}

impl Host {
    pub(crate) fn read() -> Host {
        platform::read()
    }
}
