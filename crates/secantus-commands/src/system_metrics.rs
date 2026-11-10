//! mongod's Linux `systemMetrics` FTDC collector (RM-424): the machine's
//! CPU, memory, network, disk, mount, VM, file-handle and pressure counters,
//! read from `/proc` each sample, as `ftdc_system_stats_linux.cpp` and
//! `procparser.cpp` (8.2.1) read them -- the same files, keys, units, BSON
//! number types and document order, and a section that cannot be read keeps
//! mongod's `error` string in its place. macOS mongod has no such collector,
//! so neither does this server off Linux.

use bson::{doc, Bson, Document};

/// `/proc/stat`'s lines mongod keeps.
const CPU_KEYS: &[&str] = &[
    "btime",
    "cpu",
    "ctxt",
    "processes",
    "procs_blocked",
    "procs_running",
];

/// `/proc/meminfo`'s lines mongod keeps.
const MEM_KEYS: &[&str] = &[
    "MemAvailable",
    "MemTotal",
    "MemFree",
    "Cached",
    "Dirty",
    "Buffers",
    "SwapTotal",
    "SwapCached",
    "SwapFree",
    "Active",
    "Inactive",
    "Active(anon)",
    "Inactive(anon)",
    "Active(file)",
    "Inactive(file)",
    "AnonHugePages",
];

/// The `/proc/net/netstat` and `/proc/net/snmp` prefixes mongod keeps.
const NETSTAT_KEYS: &[&str] = &["Tcp:", "Ip:", "TcpExt:", "IpExt:"];

/// `/proc/vmstat`'s lines mongod keeps.
const VM_KEYS: &[&str] = &[
    "balloon_deflate",
    "balloon_inflate",
    "nr_mlock",
    "numa_pages_migrated",
    "pgfault",
    "pgmajfault",
    "pswpin",
    "pswpout",
    "nr_anon_transparent_hugepages",
    "thp_fault_alloc",
    "thp_collapse_alloc",
    "thp_fault_fallback",
    "thp_swpout",
];

/// `/proc/net/sockstat`'s lines, and each one's keys, mongod keeps.
const SOCKSTAT_KEYS: &[(&str, &[&str])] = &[
    ("sockets", &["used"]),
    ("TCP", &["inuse", "orphan", "tw", "alloc"]),
];

/// `/proc/stat`'s `cpu` line, in ticks, named as mongod names it in ms.
const CPU_FIELDS: &[&str] = &[
    "user_ms",
    "nice_ms",
    "system_ms",
    "idle_ms",
    "iowait_ms",
    "irq_ms",
    "softirq_ms",
    "steal_ms",
    "guest_ms",
    "guest_nice_ms",
];

/// `/proc/diskstats`'s first eleven counters, as mongod names them.
const DISK_FIELDS: &[&str] = &[
    "reads",
    "reads_merged",
    "read_sectors",
    "read_time_ms",
    "writes",
    "writes_merged",
    "write_sectors",
    "write_time_ms",
    "io_in_progress",
    "io_time_ms",
    "io_queued_ms",
];

/// A failed read or parse: mongod's `Status::toString()`, `Code: reason`.
type Status = Result<(), String>;

fn failed(code: &str, reason: impl std::fmt::Display) -> Status {
    Err(format!("{code}: {reason}"))
}

/// `Status::addContext`.
fn with_context(status: Status, context: &str) -> Status {
    status.map_err(|e| match e.split_once(": ") {
        Some((code, reason)) => format!("{code}: {context} :: caused by :: {reason}"),
        None => e,
    })
}

/// `SystemMetricsCollector::processStatusErrors`.
fn note(out: &mut Document, status: Status) {
    if let Err(e) = status {
        out.insert("error", e);
    }
}

/// `BSONObjBuilder::appendNumber(long long)`: an int when it fits.
fn number(n: i64) -> Bson {
    match i32::try_from(n) {
        Ok(n) => Bson::Int32(n),
        Err(_) => Bson::Int64(n),
    }
}

/// `NumberParser` into a `uint64_t`.
fn unsigned(token: &str) -> Option<u64> {
    token.parse().ok()
}

/// Split as boost's `split_iterator` with `token_compress_on` does on a
/// line that starts with no delimiter.
fn tokens(line: &str, delimiter: impl Fn(char) -> bool) -> Vec<&str> {
    line.split(delimiter).filter(|t| !t.is_empty()).collect()
}

fn lines(data: &str) -> impl Iterator<Item = &str> {
    data.split('\n').filter(|l| !l.is_empty())
}

/// `parseGenericStats`: each line whose first token is a wanted key and
/// which has a value after it.
fn generic<'a>(
    keys: &[&str],
    data: &'a str,
    delimiter: impl Fn(char) -> bool + Copy,
    mut take: impl FnMut(&'a str, &[&'a str]),
) -> Status {
    let mut found = false;
    for line in lines(data) {
        let parts = tokens(line, delimiter);
        if parts.len() < 2 {
            continue;
        }
        if keys.is_empty() || keys.contains(&parts[0]) {
            found = true;
            take(parts[0], &parts[1..]);
        }
    }
    if found {
        Ok(())
    } else {
        failed("NoSuchKey", "Failed to find any keys in string")
    }
}

/// `parseProcStat`: the `cpu` line's ten tick counts in milliseconds, the
/// others as counts.
fn parse_stat(data: &str, ticks_per_second: i64, out: &mut Document) -> Status {
    generic(
        CPU_KEYS,
        data,
        |c| c == ' ',
        |key, values| {
            if key == "cpu" {
                for (name, v) in CPU_FIELDS.iter().zip(values) {
                    let ticks = unsigned(v).unwrap_or(0) as i64;
                    out.insert(*name, ticks as f64 / (ticks_per_second as f64 / 1000.0));
                }
            } else {
                out.insert(key, number(unsigned(values[0]).unwrap_or(0) as i64));
            }
        },
    )
}

/// `parseProcMemInfo`: a value in `kB` is named with `_kb`.
fn parse_meminfo(data: &str, out: &mut Document) -> Status {
    generic(
        MEM_KEYS,
        data,
        |c| c == ' ' || c == ':',
        |key, values| {
            let n = number(unsigned(values[0]).unwrap_or(0) as i64);
            match values.get(1) {
                Some(&"kB") => out.insert(format!("{key}_kb"), n),
                _ => out.insert(key, n),
            };
        },
    )
}

/// `parseProcVMStat`.
fn parse_vmstat(data: &str, out: &mut Document) -> Status {
    generic(
        VM_KEYS,
        data,
        |c| c == ' ',
        |key, values| {
            out.insert(key, number(unsigned(values[0]).unwrap_or(0) as i64));
        },
    )
}

/// `parseProcNetstat`: alternate lines of names and values, each pair
/// prefixed (`TcpExt:`), every name kept as `prefix` + name. A value that
/// is not an unsigned number (`Tcp: MaxConn -1`) is left out.
fn parse_netstat(data: &str, out: &mut Document) -> Status {
    let mut found = false;
    let mut names: Vec<&str> = Vec::new();
    for (n, line) in lines(data).enumerate() {
        let parts = tokens(line, |c| c == ' ');
        if n % 2 == 0 {
            names = parts;
            continue;
        }
        let Some((prefix, _)) = names.split_first() else {
            continue;
        };
        if parts.is_empty() || !NETSTAT_KEYS.contains(prefix) {
            continue;
        }
        for (name, value) in names.iter().zip(&parts).skip(1) {
            if let Some(v) = unsigned(value) {
                out.insert(format!("{prefix}{name}"), number(v as i64));
                found = true;
            }
        }
    }
    if found {
        Ok(())
    } else {
        failed("NoSuchKey", "Failed to find any keys in netstats string")
    }
}

/// `parseProcSockstat`: a document per kept line, of its kept keys.
fn parse_sockstat(data: &str, out: &mut Document) -> Status {
    let mut found = false;
    for line in lines(data) {
        let parts = tokens(line, |c| c == ' ' || c == ':');
        let Some((&line_key, rest)) = parts.split_first() else {
            continue;
        };
        let Some((_, wanted)) = SOCKSTAT_KEYS.iter().find(|(k, _)| *k == line_key) else {
            continue;
        };
        let mut sub = Document::new();
        let mut status = Ok(());
        for pair in rest.chunks(2) {
            if !wanted.contains(&pair[0]) {
                continue;
            }
            let value = pair.get(1).copied().unwrap_or("");
            match value.parse::<i64>() {
                Ok(v) => {
                    sub.insert(pair[0], number(v));
                    found = true;
                }
                Err(_) => {
                    status = failed(
                        "FailedToParse",
                        format!("Couldn't parse '{value}' to number"),
                    );
                    break;
                }
            }
        }
        out.insert(line_key, sub);
        status?;
    }
    if found {
        Ok(())
    } else {
        failed("NoSuchKey", "Failed to find any keys in sockStats string")
    }
}

/// `parseProcDiskStats`: each named disk with any activity, its first
/// eleven counters.
fn parse_diskstats(disks: &[String], data: &str, out: &mut Document) -> Status {
    let mut found = false;
    for line in lines(data) {
        let parts = tokens(line, |c| c == ' ');
        if parts.len() < 3 {
            continue;
        }
        let disk = parts[2];
        if !disks.is_empty() && !disks.iter().any(|d| d == disk) {
            continue;
        }
        found = true;
        let stats: Vec<u64> = parts[3..]
            .iter()
            .take(DISK_FIELDS.len())
            .map(|v| unsigned(v).unwrap_or(0))
            .collect();
        if stats.iter().any(|&v| v != 0) {
            let mut sub = Document::new();
            for (name, v) in DISK_FIELDS.iter().zip(&stats) {
                sub.insert(*name, number(*v as i64));
            }
            out.insert(disk, sub);
        }
    }
    if found {
        Ok(())
    } else {
        failed("NoSuchKey", "Failed to find any keys in diskstats string")
    }
}

/// The mount point of a `/proc/self/mountinfo` line, as mongod's
/// `\d+ \d+ \d+:\d+ \S+ (\S+)` takes it.
fn mount_point(line: &str) -> Option<&str> {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mut parts = line.split(' ');
    let (id, parent, dev, _root, point) = (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
    );
    let (major, minor) = dev.split_once(':')?;
    (digits(id) && digits(parent) && digits(major) && digits(minor) && !point.is_empty())
        .then_some(point)
}

/// `parseProcSelfMountStats`: each mount point's `capacity`, `available`
/// and `free` bytes, as `boost::filesystem::space` reads them, when it
/// answers and has any capacity.
fn parse_mounts(
    data: &str,
    out: &mut Document,
    space: impl Fn(&str) -> Option<(u64, u64, u64)>,
) -> Status {
    for line in data.lines() {
        let Some(point) = mount_point(line) else {
            continue;
        };
        if let Some((capacity, available, free)) = space(point) {
            if capacity != 0 {
                out.insert(
                    point,
                    doc! {
                        "capacity": number(capacity as i64),
                        "available": number(available as i64),
                        "free": number(free as i64),
                    },
                );
            }
        }
    }
    Ok(())
}

/// `parseProcSysFsFileNr` for the handles in use, the file's first number.
fn parse_file_nr(data: &str, out: &mut Document) -> Status {
    let Some(first) = tokens(data, |c| c == ' ' || c == '\t' || c == '\n')
        .into_iter()
        .next()
    else {
        return failed("FailedToParse", "Couldn't find first token");
    };
    match unsigned(first) {
        Some(v) => {
            out.insert("sys_file_handles_in_use", number(v as i64));
            Ok(())
        }
        None => failed("FailedToParse", "Couldn't parse first token to number"),
    }
}

/// `parseProcPressure`: each `some` / `full` line's `total`, in
/// microseconds, as a double.
fn parse_pressure(data: &str, out: &mut Document) -> Status {
    for line in lines(data) {
        let parts = tokens(line, |c| c == ' ' || c == '=');
        if parts.len() < 2 {
            continue;
        }
        let share = parts[0];
        if share != "some" && share != "full" {
            return failed("FailedToParse", "Couldn't find the share of time");
        }
        let Some(at) = parts[1..].iter().position(|p| *p == "total") else {
            return failed("NoSuchKey", "Failed to find 'total' token");
        };
        let Some(value) = parts.get(at + 2) else {
            return failed("FailedToParse", "No value found for 'total' token");
        };
        match value.parse::<f64>() {
            Ok(v) => {
                out.insert(share, doc! {"totalMicros": v});
            }
            Err(_) => {
                return failed(
                    "FailedToParse",
                    format!("Couldn't parse '{value}' to number"),
                );
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::io::Read;
    use std::sync::OnceLock;

    /// `readFileAsString`'s failures, worded as mongod words them.
    fn read(path: &str) -> Result<String, String> {
        let mut file = std::fs::File::open(path).map_err(|e| {
            format!(
                "FileOpenFailed: Failed to open file {path} with error: {}",
                strerror(&e)
            )
        })?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| {
            format!(
                "FileStreamFailed: Failed to read file {path} with error: {}",
                strerror(&e)
            )
        })?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// `errorMessage(ec)`: the C library's text, without Rust's
    /// ` (os error N)`.
    fn strerror(e: &std::io::Error) -> String {
        let text = e.to_string();
        match text.rfind(" (os error ") {
            Some(at) => text[..at].to_string(),
            None => text,
        }
    }

    fn file(path: &str, parse: impl FnOnce(&str) -> Status) -> Status {
        parse(&read(path)?)
    }

    /// `findPhysicalDisks`: the block devices with a `device` directory,
    /// found once, when mongod builds the collector.
    fn physical_disks() -> &'static [String] {
        static DISKS: OnceLock<Vec<String>> = OnceLock::new();
        DISKS.get_or_init(|| {
            let Ok(dir) = std::fs::read_dir("/sys/block") else {
                return Vec::new();
            };
            dir.flatten()
                .filter(|e| e.path().join("device").is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
    }

    /// `boost::filesystem::space`: `statvfs`'s blocks in fragment-size
    /// units.
    fn space(path: &str) -> Option<(u64, u64, u64)> {
        let cpath = std::ffi::CString::new(path).ok()?;
        // SAFETY: `statvfs` fills the zeroed struct from a NUL-terminated
        // path and touches nothing else.
        let vfs = unsafe {
            let mut vfs: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(cpath.as_ptr(), &mut vfs) != 0 {
                return None;
            }
            vfs
        };
        let frsize = vfs.f_frsize as u64;
        Some((
            vfs.f_blocks as u64 * frsize,
            vfs.f_bavail as u64 * frsize,
            vfs.f_bfree as u64 * frsize,
        ))
    }

    /// `ProcessInfo::getNumCoresAvailableToProcess`: the CPUs in this
    /// process's affinity mask.
    fn cores_available_to_process() -> Option<i32> {
        // SAFETY: `sched_getaffinity` fills the zeroed set it is given.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) == 0 {
                Some(libc::CPU_COUNT(&set))
            } else {
                None
            }
        }
    }

    pub(crate) fn collect() -> Document {
        let mut out = Document::new();

        let mut cpu = doc! {"num_logical_cores": crate::admission::execution::logical_cores()};
        if let Some(n) = cores_available_to_process() {
            cpu.insert("num_cores_available_to_process", n);
        }
        // SAFETY: `sysconf` reads a system constant.
        // `c_long` is i64 on a 64-bit target, narrower on others.
        #[allow(clippy::unnecessary_cast)]
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as i64;
        let status = file("/proc/stat", |d| {
            with_context(parse_stat(d, ticks, &mut cpu), "Parsing /proc/stat")
        });
        note(&mut cpu, status);
        out.insert("cpu", cpu);

        let mut memory = Document::new();
        let status = file("/proc/meminfo", |d| {
            with_context(parse_meminfo(d, &mut memory), "Parsing /proc/meminfo")
        });
        note(&mut memory, status);
        out.insert("memory", memory);

        // SAFETY: `PR_GET_THP_DISABLE` reads this process's flag.
        let thp_disabled = unsafe { libc::prctl(libc::PR_GET_THP_DISABLE, 0, 0, 0, 0) };
        if thp_disabled >= 0 {
            out.insert(
                "status",
                doc! {"process_opting_into_THP_if_enabled": i32::from(thp_disabled == 0)},
            );
        }

        let mut netstat = Document::new();
        for path in ["/proc/net/netstat", "/proc/net/snmp"] {
            let status = file(path, |d| parse_netstat(d, &mut netstat));
            note(&mut netstat, status);
        }
        out.insert("netstat", netstat);

        let mut sockstat = Document::new();
        let status = file("/proc/net/sockstat", |d| parse_sockstat(d, &mut sockstat));
        note(&mut sockstat, status);
        out.insert("sockstat", sockstat);

        let disks = physical_disks();
        if !disks.is_empty() {
            let mut stats = Document::new();
            let status = file("/proc/diskstats", |d| parse_diskstats(disks, d, &mut stats));
            note(&mut stats, status);
            out.insert("disks", stats);
        }

        let mut mounts = Document::new();
        let status = file("/proc/self/mountinfo", |d| {
            parse_mounts(d, &mut mounts, space)
        });
        note(&mut mounts, status);
        out.insert("mounts", mounts);

        let mut vmstat = Document::new();
        let status = file("/proc/vmstat", |d| {
            with_context(parse_vmstat(d, &mut vmstat), "Parsing /proc/vmstat")
        });
        note(&mut vmstat, status);
        out.insert("vmstat", vmstat);

        let mut files = Document::new();
        let status = file("/proc/sys/fs/file-nr", |d| parse_file_nr(d, &mut files));
        note(&mut files, status);
        out.insert("files", files);

        let mut pressure = Document::new();
        for kind in ["cpu", "memory", "io"] {
            let status = read(&format!("/proc/pressure/{kind}")).and_then(|d| {
                let mut sub = Document::new();
                let status = parse_pressure(&d, &mut sub);
                pressure.insert(kind, sub);
                status
            });
            note(&mut pressure, status);
        }
        out.insert("pressure", pressure);

        out
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux::collect;

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "cpu  41801 9179 32206 831134223 34279 0 947 0 0 0\n\
        cpu0 2977 450 2475 69253074 1959 0 116 0 0 0\n\
        intr 54084718 135 2\n\
        ctxt 190305514\n\
        btime 1463584038\n\
        processes 47438\n\
        procs_running 1\n\
        procs_blocked 0\n\
        softirq 102690251 8 26697410\n";

    #[test]
    fn stat_keeps_the_cpu_line_in_ms_and_the_named_counts() {
        let mut out = Document::new();
        parse_stat(STAT, 100, &mut out).unwrap();
        let keys: Vec<&str> = out.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "user_ms",
                "nice_ms",
                "system_ms",
                "idle_ms",
                "iowait_ms",
                "irq_ms",
                "softirq_ms",
                "steal_ms",
                "guest_ms",
                "guest_nice_ms",
                "ctxt",
                "btime",
                "processes",
                "procs_running",
                "procs_blocked",
            ]
        );
        assert_eq!(out.get("user_ms"), Some(&Bson::Double(418010.0)));
        assert_eq!(out.get("ctxt"), Some(&Bson::Int32(190305514)));
        assert_eq!(out.get("procs_blocked"), Some(&Bson::Int32(0)));
    }

    #[test]
    fn a_short_cpu_line_names_only_its_fields() {
        let mut out = Document::new();
        parse_stat("cpu 1 2 3 4 5 6 7 8\n", 100, &mut out).unwrap();
        assert_eq!(out.len(), 8);
        assert!(!out.contains_key("guest_ms"));
    }

    #[test]
    fn a_file_without_wanted_keys_is_an_error_with_mongods_context() {
        let mut out = Document::new();
        let status = with_context(parse_stat("intr 1\n", 100, &mut out), "Parsing /proc/stat");
        assert_eq!(
            status.unwrap_err(),
            "NoSuchKey: Parsing /proc/stat :: caused by :: Failed to find any keys in string"
        );
    }

    #[test]
    fn meminfo_names_kilobyte_values_with_kb() {
        let data = "MemTotal:       12294392 kB\nMemFree:         3652612 kB\n\
            HugePages_Total:       0\nActive(anon):    4975441 kB\n";
        let mut out = Document::new();
        parse_meminfo(data, &mut out).unwrap();
        assert_eq!(
            out,
            doc! {"MemTotal_kb": 12294392, "MemFree_kb": 3652612, "Active(anon)_kb": 4975441}
        );
    }

    #[test]
    fn netstat_prefixes_names_and_leaves_out_negative_values() {
        let data = "TcpExt: SyncookiesSent SyncookiesRecv\nTcpExt: 3437 5938\n\
            MPTcpExt: MPCapableSYNRX\nMPTcpExt: 4\n\
            Tcp: RtoAlgorithm MaxConn ActiveOpens\nTcp: 1 -1 9000000000\n";
        let mut out = Document::new();
        parse_netstat(data, &mut out).unwrap();
        assert_eq!(
            out,
            doc! {
                "TcpExt:SyncookiesSent": 3437,
                "TcpExt:SyncookiesRecv": 5938,
                "Tcp:RtoAlgorithm": 1,
                "Tcp:ActiveOpens": 9000000000_i64,
            }
        );
    }

    #[test]
    fn sockstat_keeps_sockets_used_and_four_tcp_counts() {
        let data = "sockets: used 290\nTCP: inuse 8 orphan 0 tw 2 alloc 10 mem 1\n\
            UDP: inuse 3 mem 2\n";
        let mut out = Document::new();
        parse_sockstat(data, &mut out).unwrap();
        assert_eq!(
            out,
            doc! {
                "sockets": {"used": 290},
                "TCP": {"inuse": 8, "orphan": 0, "tw": 2, "alloc": 10},
            }
        );
    }

    #[test]
    fn diskstats_keeps_named_active_disks_and_eleven_counters() {
        let data = "   8       0 sda 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15\n\
            8       1 sda1 138 37 8642 315 3 0 18 14 0 292 329\n\
            11       0 sr0 0 0 0 0 0 0 0 0 0 0 0\n";
        let disks = ["sda".to_string(), "sr0".to_string()];
        let mut out = Document::new();
        parse_diskstats(&disks, data, &mut out).unwrap();
        let keys: Vec<&str> = out.keys().map(String::as_str).collect();
        assert_eq!(keys, ["sda"]);
        let sda = out.get_document("sda").unwrap();
        assert_eq!(sda.len(), 11);
        assert_eq!(sda.get("io_queued_ms"), Some(&Bson::Int32(11)));
    }

    #[test]
    fn mounts_report_space_of_each_mount_point_with_capacity() {
        let data = "36 35 98:0 /mnt1 /mnt2 rw,noatime master:1 - ext3 /dev/root rw\n\
            37 35 0:5 / /proc rw - proc proc rw\n\
            garbage\n";
        let mut out = Document::new();
        parse_mounts(data, &mut out, |p| match p {
            "/mnt2" => Some((10_000_000_000, 4, 5)),
            _ => Some((0, 0, 0)),
        })
        .unwrap();
        assert_eq!(
            out,
            doc! {"/mnt2": {"capacity": 10_000_000_000_i64, "available": 4, "free": 5}}
        );
    }

    #[test]
    fn file_nr_keeps_the_handles_in_use() {
        let mut out = Document::new();
        parse_file_nr("1632\t0\t9223372036854775807\n", &mut out).unwrap();
        assert_eq!(out, doc! {"sys_file_handles_in_use": 1632});
    }

    #[test]
    fn pressure_keeps_each_shares_total_as_a_double() {
        let data = "some avg10=0.00 avg60=0.00 avg300=0.14 total=1434509127\n\
            full avg10=0.00 avg60=0.00 avg300=0.14 total=1035574668\n";
        let mut out = Document::new();
        parse_pressure(data, &mut out).unwrap();
        assert_eq!(
            out,
            doc! {
                "some": {"totalMicros": 1434509127.0},
                "full": {"totalMicros": 1035574668.0},
            }
        );
        let mut bad = Document::new();
        assert_eq!(
            parse_pressure("half total=1\n", &mut bad).unwrap_err(),
            "FailedToParse: Couldn't find the share of time"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_linux_collector_reads_each_section() {
        let out = collect();
        for key in [
            "cpu", "memory", "netstat", "sockstat", "mounts", "vmstat", "files", "pressure",
        ] {
            assert!(out.contains_key(key), "{key} missing from {out}");
        }
        assert!(out.get_document("cpu").unwrap().contains_key("user_ms"));
    }
}
