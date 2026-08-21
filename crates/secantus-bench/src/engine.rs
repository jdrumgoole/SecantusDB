//! The two database engines under comparison.
//!
//! The whole point of this harness is a like-for-like number: the same
//! workload, from the same client machines, over the same network, against the
//! same hardware — with only the database swapped. So an engine is reduced to
//! the few things that actually differ: what to install, how to launch it, and
//! where it keeps its data.
//!
//! Everything else is deliberately identical. Both engines bind the server
//! droplet's private IP on the same port (so the firewall rule and the client
//! command line never change), both get the same WiredTiger cache size, and
//! both start from an empty data directory. They run **sequentially**, never
//! side by side, because two databases sharing four cores would measure
//! contention rather than either engine.

use crate::BenchResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// The SecantusDB Rust server, `secantusd-rs`.
    Secantus,
    /// A real MongoDB Community server, `mongod`.
    Mongod,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Engine::Secantus => "secantusdb",
            Engine::Mongod => "mongod",
        }
    }

    /// The systemd unit the harness installs and drives.
    pub fn service(self) -> &'static str {
        match self {
            Engine::Secantus => "secantus-bench",
            Engine::Mongod => "secantus-bench-mongod",
        }
    }

    /// Data directory. Separate per engine so switching engines never inherits
    /// the other's files, and so `--keep-data` means what it says for each.
    pub fn data_dir(self) -> &'static str {
        match self {
            Engine::Secantus => "/var/lib/secantus-bench",
            Engine::Mongod => "/var/lib/secantus-bench-mongod",
        }
    }

    /// The process name the resource sampler tracks.
    pub fn process(self) -> &'static str {
        match self {
            Engine::Secantus => "secantusd-rs",
            Engine::Mongod => "mongod",
        }
    }

    pub fn parse(name: &str) -> BenchResult<Engine> {
        match name {
            "secantus" | "secantusdb" | "secantusd-rs" => Ok(Engine::Secantus),
            "mongod" | "mongodb" => Ok(Engine::Mongod),
            other => Err(format!(
                "unknown engine {other:?} (expected: secantus | mongod | both)"
            )),
        }
    }

    /// `--engine` accepts one engine or `both`. `both` runs SecantusDB first
    /// so a failure in the comparison arm still leaves the primary number.
    pub fn parse_list(spec: &str) -> BenchResult<Vec<Engine>> {
        if spec == "both" {
            return Ok(vec![Engine::Secantus, Engine::Mongod]);
        }
        let mut out = Vec::new();
        for part in spec.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let engine = Engine::parse(part)?;
            if !out.contains(&engine) {
                out.push(engine);
            }
        }
        if out.is_empty() {
            return Err("--engine must name at least one engine".to_string());
        }
        Ok(out)
    }

    /// The full `ExecStart` line, given the bind address and cache size.
    ///
    /// The two are configured as equivalently as their flags allow: same bind,
    /// same port, same data path, same WiredTiger cache.
    pub fn exec_start(self, bind: &str, port: u16, cache_size: &str, extra: &str) -> String {
        let mut cmd = match self {
            Engine::Secantus => format!(
                "{} --host {bind} --port {port} --storage-path {} --cache-size {cache_size} \
                 --log-level INFO",
                crate::SERVER_BIN,
                self.data_dir()
            ),
            Engine::Mongod => format!(
                "/usr/bin/mongod --bind_ip {bind} --port {port} --dbpath {} \
                 --wiredTigerCacheSizeGB {}",
                self.data_dir(),
                cache_gb(cache_size)
            ),
        };
        if !extra.is_empty() {
            cmd.push(' ');
            cmd.push_str(extra);
        }
        cmd
    }

    /// Per-engine durability flag, so `--sync-on-commit` means the same thing
    /// on both sides rather than only applying to one.
    pub fn sync_on_commit_flag(self) -> &'static str {
        match self {
            Engine::Secantus => "--sync-on-commit",
            // mongod's equivalent knob: journal every commit rather than on
            // the default ~100ms interval.
            Engine::Mongod => "--journalCommitInterval 1",
        }
    }
}

/// mongod takes its cache as a number of GB, SecantusDB as a unit-suffixed
/// string. Convert so one `--cache-size` drives both.
pub fn cache_gb(cache_size: &str) -> String {
    let trimmed = cache_size.trim();
    let (digits, suffix) = trimmed.split_at(
        trimmed
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(trimmed.len()),
    );
    let value: f64 = digits.parse().unwrap_or(1.0);
    let gb = match suffix.trim().to_ascii_uppercase().as_str() {
        "G" | "GB" | "" => value,
        "M" | "MB" => value / 1024.0,
        "K" | "KB" => value / 1024.0 / 1024.0,
        _ => value,
    };
    // mongod rejects a cache below 0.25 GB.
    let gb = if gb < 0.25 { 0.25 } else { gb };
    if (gb - gb.round()).abs() < 1e-9 {
        format!("{}", gb.round() as i64)
    } else {
        format!("{gb:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_runs_secantus_first() {
        // SecantusDB is the primary number; if the comparison arm fails, the
        // run that matters has already happened.
        assert_eq!(
            Engine::parse_list("both").unwrap(),
            [Engine::Secantus, Engine::Mongod]
        );
    }

    #[test]
    fn engine_names_and_aliases_parse() {
        for name in ["secantus", "secantusdb", "secantusd-rs"] {
            assert_eq!(Engine::parse(name).unwrap(), Engine::Secantus);
        }
        for name in ["mongod", "mongodb"] {
            assert_eq!(Engine::parse(name).unwrap(), Engine::Mongod);
        }
        assert!(Engine::parse("postgres").is_err());
    }

    #[test]
    fn a_list_dedups_and_keeps_order() {
        assert_eq!(
            Engine::parse_list("mongod,secantus,mongod").unwrap(),
            [Engine::Mongod, Engine::Secantus]
        );
        assert!(Engine::parse_list("").is_err());
    }

    #[test]
    fn the_engines_never_share_a_data_directory_or_service() {
        assert_ne!(Engine::Secantus.data_dir(), Engine::Mongod.data_dir());
        assert_ne!(Engine::Secantus.service(), Engine::Mongod.service());
        assert_ne!(Engine::Secantus.process(), Engine::Mongod.process());
    }

    #[test]
    fn both_engines_bind_the_same_address_and_port() {
        // The firewall rule and the client command line must not depend on
        // which engine is running.
        for engine in [Engine::Secantus, Engine::Mongod] {
            let cmd = engine.exec_start("10.0.0.2", 27017, "4G", "");
            assert!(cmd.contains("10.0.0.2"), "{cmd}");
            assert!(cmd.contains("27017"), "{cmd}");
            assert!(cmd.contains(engine.data_dir()), "{cmd}");
        }
    }

    #[test]
    fn the_same_cache_size_reaches_both_engines() {
        assert!(Engine::Secantus
            .exec_start("h", 1, "4G", "")
            .contains("--cache-size 4G"));
        assert!(Engine::Mongod
            .exec_start("h", 1, "4G", "")
            .contains("--wiredTigerCacheSizeGB 4"));
    }

    #[test]
    fn cache_sizes_convert_to_mongods_gb_form() {
        assert_eq!(cache_gb("4G"), "4");
        assert_eq!(cache_gb("1G"), "1");
        assert_eq!(cache_gb("512M"), "0.50");
        assert_eq!(cache_gb("8"), "8");
        // mongod refuses anything under a quarter GB.
        assert_eq!(cache_gb("64M"), "0.25");
    }

    #[test]
    fn extra_flags_are_appended() {
        let cmd = Engine::Secantus.exec_start("h", 1, "1G", "--standalone");
        assert!(cmd.ends_with("--standalone"));
    }
}
