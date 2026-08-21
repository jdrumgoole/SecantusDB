//! TOML-based configuration loader for the `secantusd-rs` daemon.
//!
//! A faithful Rust port of `src/secantus/config.py` (the oracle). The config
//! file is a thin convenience over the CLI flag surface plus a handful of
//! previously-hard-coded WiredTiger / oplog knobs. Passing **no** `--config`
//! and having **no** `secantusd.toml` on the auto-discovery path leaves the
//! original behaviour untouched.
//!
//! Precedence (low → high):
//!
//! ```text
//! ResolvedConfig defaults  <  TOML file values  <  explicit CLI flag
//! ```
//!
//! Auto-discovery path order (first hit wins), walked only when `--config`
//! was not passed. The legacy `secantusdb.toml` is probed at each location
//! immediately after the new name, so old-named configs keep working:
//!
//! 1. `./secantusd.toml`                (cwd — per-checkout)
//! 2. `~/.secantus/secantusd.toml`      (per-user)
//! 3. `/etc/secantus/secantusd.toml`    (system-wide)
//!
//! This module is WiredTiger-free (it only parses text and resolves values),
//! so it lives in the clean-workspace `secantus-server` crate next to the arg
//! parser and is unit-testable via `cargo test -p secantus-server`.

use std::path::{Path, PathBuf};

/// The valid `log_level` choices (mirrors cli.py's `choices=`).
pub const LOG_LEVELS: [&str; 4] = ["DEBUG", "INFO", "WARNING", "ERROR"];

/// A fully-resolved daemon configuration. Field defaults match
/// `SecantusConfig` in `config.py` exactly, so a `ResolvedConfig::default()`
/// (no file, no flags) behaves identically to what `secantusd-rs` with zero
/// arguments used to do.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedConfig {
    // ---- [server] ----------------------------------------------------
    pub host: String,
    pub port: u16,
    pub storage_path: String,
    pub log_level: String,
    pub auth: bool,
    /// `false` ⇒ advertise as single-node replica set (the default that lets
    /// pymongo's change-stream topology checks pass). `true` ⇒ STANDALONE.
    pub standalone: bool,

    // ---- [oplog] -----------------------------------------------------
    pub oplog_retention_seconds: f64,
    pub oplog_max_entries: usize,
    pub oplog_archive_dir: Option<String>,
    pub noop_heartbeat_seconds: f64,

    // ---- [storage] ---------------------------------------------------
    pub cache_size: String,
    /// WiredTiger WAL log `file_max` (unit-suffixed, e.g. "2GB"). The standalone
    /// daemon defaults to 2GB — the 128MB WT default forced constant log-file
    /// rotation under a multi-writer write load (a measured ~+13-19% throughput
    /// loss at 4-8 writers). 2GB is WT's hard cap; `prealloc=false` keeps the log
    /// files sparse so a small workload still costs only what it writes.
    pub log_file_max: String,
    pub session_max: u32,
    pub ttl_sweep_seconds: f64,
    pub sync_on_commit: bool,
    /// Rust-server storage write-path modes (no Python-daemon counterpart —
    /// `config.py` has no equivalents). `None` defers to the matching
    /// `SECANTUS_*` env var via `StorageOptions`; a set value wins for this
    /// daemon only.
    pub oplog_async: Option<bool>,
    pub oplog_nonlogged: Option<bool>,
    pub data_nonlogged: Option<bool>,
    pub checkpoint_seconds: Option<u64>,
    pub write_tickets: Option<usize>,

    // ---- [tls] -------------------------------------------------------
    pub tls_cert_file: Option<String>,
    pub tls_key_file: Option<String>,
    pub tls_ca_file: Option<String>,
    pub tls_require_client_cert: bool,
}

impl Default for ResolvedConfig {
    fn default() -> Self {
        ResolvedConfig {
            host: "127.0.0.1".to_string(),
            port: 27017,
            storage_path: "./secantus-data".to_string(),
            log_level: "INFO".to_string(),
            auth: false,
            standalone: false,
            oplog_retention_seconds: 3600.0,
            oplog_max_entries: 100_000,
            oplog_archive_dir: None,
            noop_heartbeat_seconds: 0.0,
            cache_size: "4G".to_string(),
            log_file_max: "2GB".to_string(),
            session_max: 1000,
            ttl_sweep_seconds: 60.0,
            sync_on_commit: false,
            oplog_async: None,
            oplog_nonlogged: None,
            data_nonlogged: None,
            checkpoint_seconds: None,
            write_tickets: None,
            tls_cert_file: None,
            tls_key_file: None,
            tls_ca_file: None,
            tls_require_client_cert: false,
        }
    }
}

/// Overrides sourced from a TOML file or the CLI: `Some` = "this value was
/// set", `None` = "not set, defer to the lower-precedence layer". The two
/// layers (TOML then CLI) are applied on top of [`ResolvedConfig::default`] in
/// that order by [`resolve`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigOverrides {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub storage_path: Option<String>,
    pub log_level: Option<String>,
    pub auth: Option<bool>,
    pub standalone: Option<bool>,
    pub oplog_retention_seconds: Option<f64>,
    pub oplog_max_entries: Option<usize>,
    pub oplog_archive_dir: Option<String>,
    pub noop_heartbeat_seconds: Option<f64>,
    pub cache_size: Option<String>,
    pub log_file_max: Option<String>,
    pub session_max: Option<u32>,
    pub ttl_sweep_seconds: Option<f64>,
    pub sync_on_commit: Option<bool>,
    pub oplog_async: Option<bool>,
    pub oplog_nonlogged: Option<bool>,
    pub data_nonlogged: Option<bool>,
    pub checkpoint_seconds: Option<u64>,
    pub write_tickets: Option<usize>,
    pub tls_cert_file: Option<String>,
    pub tls_key_file: Option<String>,
    pub tls_ca_file: Option<String>,
    pub tls_require_client_cert: Option<bool>,
}

impl ConfigOverrides {
    /// Apply the `Some` fields on top of `base`, mutating it in place. `None`
    /// fields leave the base untouched (the precedence rule).
    fn apply_to(&self, base: &mut ResolvedConfig) {
        macro_rules! set {
            ($field:ident) => {
                if let Some(v) = &self.$field {
                    base.$field = v.clone();
                }
            };
        }
        macro_rules! set_copy {
            ($field:ident) => {
                if let Some(v) = self.$field {
                    base.$field = v;
                }
            };
        }
        set!(host);
        set_copy!(port);
        set!(storage_path);
        set!(log_level);
        set_copy!(auth);
        set_copy!(standalone);
        set_copy!(oplog_retention_seconds);
        set_copy!(oplog_max_entries);
        // oplog_archive_dir is Option-valued; `Some(dir)` sets it.
        if let Some(v) = &self.oplog_archive_dir {
            base.oplog_archive_dir = Some(v.clone());
        }
        set_copy!(noop_heartbeat_seconds);
        set!(cache_size);
        set!(log_file_max);
        set_copy!(session_max);
        set_copy!(ttl_sweep_seconds);
        set_copy!(sync_on_commit);
        // Option-valued in the base too: `Some(v)` sets, `None` defers.
        if let Some(v) = self.oplog_async {
            base.oplog_async = Some(v);
        }
        if let Some(v) = self.oplog_nonlogged {
            base.oplog_nonlogged = Some(v);
        }
        if let Some(v) = self.data_nonlogged {
            base.data_nonlogged = Some(v);
        }
        if let Some(v) = self.write_tickets {
            base.write_tickets = Some(v);
        }
        if let Some(v) = self.checkpoint_seconds {
            base.checkpoint_seconds = Some(v);
        }
        if let Some(v) = &self.tls_cert_file {
            base.tls_cert_file = Some(v.clone());
        }
        if let Some(v) = &self.tls_key_file {
            base.tls_key_file = Some(v.clone());
        }
        if let Some(v) = &self.tls_ca_file {
            base.tls_ca_file = Some(v.clone());
        }
        set_copy!(tls_require_client_cert);
    }
}

/// Auto-discovery candidates, in order. The launcher walks this list only when
/// `--config` was not passed. Each location is probed for the new
/// `secantusd.toml` first, then the legacy `secantusdb.toml`, so configs written
/// for the old daemon name keep working while the new name wins on a tie.
fn auto_discovery_paths() -> Vec<PathBuf> {
    const NAMES: [&str; 2] = ["secantusd.toml", "secantusdb.toml"];
    let mut paths = Vec::new();
    for name in NAMES {
        paths.push(PathBuf::from(name));
    }
    if let Some(home) = home_dir() {
        for name in NAMES {
            paths.push(home.join(".secantus").join(name));
        }
    }
    for name in NAMES {
        paths.push(PathBuf::from(format!("/etc/secantus/{name}")));
    }
    paths
}

/// The user's home directory, from `$HOME` (POSIX) or `%USERPROFILE%`
/// (Windows). Avoids pulling in a crate just for this.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Walk the auto-discovery list and return the first existing path, or `None`.
pub fn discover_config_path() -> Option<PathBuf> {
    auto_discovery_paths().into_iter().find(|p| p.is_file())
}

/// Load and resolve the TOML config, then layer the CLI overrides on top.
///
/// * `explicit_path` — the value of `--config`, if passed. When `Some`, the
///   file **must** exist (missing → error) and auto-discovery is skipped.
/// * `cli` — the overrides the user actually typed on the command line.
///
/// Returns `(config, source_path)`, where `source_path` is the TOML file that
/// was loaded (for a "loaded config from X" log line), or `None` if none was.
pub fn resolve(
    explicit_path: Option<&Path>,
    cli: &ConfigOverrides,
) -> Result<(ResolvedConfig, Option<PathBuf>), String> {
    let (toml_overrides, source) = load_toml(explicit_path)?;
    let mut cfg = ResolvedConfig::default();
    toml_overrides.apply_to(&mut cfg);
    cli.apply_to(&mut cfg);
    Ok((cfg, source))
}

/// Resolve the TOML file (explicit or discovered) and parse it into overrides.
fn load_toml(explicit_path: Option<&Path>) -> Result<(ConfigOverrides, Option<PathBuf>), String> {
    let path = match explicit_path {
        Some(p) => {
            if !p.is_file() {
                return Err(format!("config file not found: {}", p.display()));
            }
            p.to_path_buf()
        }
        None => match discover_config_path() {
            Some(p) => p,
            None => return Ok((ConfigOverrides::default(), None)),
        },
    };
    let overrides = parse_file(&path)?;
    Ok((overrides, Some(path)))
}

/// Read and parse a TOML config file into [`ConfigOverrides`]. Mirrors
/// `config.py::_parse` — strict on unknown tables and unknown keys.
pub fn parse_file(path: &Path) -> Result<ConfigOverrides, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{}: cannot read config: {e}", path.display()))?;
    parse_str(&text, &path.display().to_string())
}

/// Parse a TOML string into [`ConfigOverrides`]. `label` is used in error
/// messages (a path, or "<config>" in tests). Faithfully replicates
/// `config.py::_parse`: only the four known tables and their allowed keys are
/// accepted; anything else is a hard error.
pub fn parse_str(text: &str, label: &str) -> Result<ConfigOverrides, String> {
    let value: toml::Value =
        toml::from_str(text).map_err(|e| format!("{label}: invalid TOML: {e}"))?;
    let table = match value {
        toml::Value::Table(t) => t,
        _ => return Err(format!("{label}: top level must be a table")),
    };

    // Known top-level tables (mirrors config.py's _TABLE_FIELDS keys).
    const KNOWN_TABLES: [&str; 4] = ["server", "oplog", "storage", "tls"];
    let mut unknown_tables: Vec<String> = table
        .keys()
        .filter(|k| !KNOWN_TABLES.contains(&k.as_str()))
        .cloned()
        .collect();
    if !unknown_tables.is_empty() {
        unknown_tables.sort();
        return Err(format!(
            "{label}: unknown top-level table(s): {unknown_tables:?} (valid: {:?})",
            {
                let mut v = KNOWN_TABLES.to_vec();
                v.sort();
                v
            }
        ));
    }

    let mut out = ConfigOverrides::default();

    // ---- [server] ----------------------------------------------------
    if let Some(server) = get_table(&table, "server", label)? {
        for (key, val) in server {
            match key.as_str() {
                "host" => out.host = Some(as_string(val, "server", key, label)?),
                "port" => out.port = Some(as_u16(val, "server", key, label)?),
                "storage_path" => out.storage_path = Some(as_string(val, "server", key, label)?),
                "log_level" => {
                    let lvl = as_string(val, "server", key, label)?;
                    validate_log_level(&lvl, label)?;
                    out.log_level = Some(lvl);
                }
                "auth" => out.auth = Some(as_bool(val, "server", key, label)?),
                "standalone" => out.standalone = Some(as_bool(val, "server", key, label)?),
                other => return Err(unknown_key("server", other, label)),
            }
        }
    }

    // ---- [oplog] -----------------------------------------------------
    // NOTE: `archive_dir` is intentionally NOT allowed here — config.py has a
    // vestigial rename entry for it but the allowed-key set excludes it, so a
    // `[oplog] archive_dir` MUST be rejected as an unknown key. Replicated.
    if let Some(oplog) = get_table(&table, "oplog", label)? {
        for (key, val) in oplog {
            match key.as_str() {
                "retention_seconds" => {
                    out.oplog_retention_seconds = Some(as_f64(val, "oplog", key, label)?)
                }
                "max_entries" => out.oplog_max_entries = Some(as_usize(val, "oplog", key, label)?),
                "noop_heartbeat_seconds" => {
                    out.noop_heartbeat_seconds = Some(as_f64(val, "oplog", key, label)?)
                }
                other => return Err(unknown_key("oplog", other, label)),
            }
        }
    }

    // ---- [storage] ---------------------------------------------------
    if let Some(storage) = get_table(&table, "storage", label)? {
        for (key, val) in storage {
            match key.as_str() {
                "cache_size" => out.cache_size = Some(as_string(val, "storage", key, label)?),
                "log_file_max" => out.log_file_max = Some(as_string(val, "storage", key, label)?),
                "session_max" => out.session_max = Some(as_u32(val, "storage", key, label)?),
                "ttl_sweep_seconds" => {
                    out.ttl_sweep_seconds = Some(as_f64(val, "storage", key, label)?)
                }
                "sync_on_commit" => out.sync_on_commit = Some(as_bool(val, "storage", key, label)?),
                "oplog_async" => out.oplog_async = Some(as_bool(val, "storage", key, label)?),
                "oplog_nonlogged" => {
                    out.oplog_nonlogged = Some(as_bool(val, "storage", key, label)?)
                }
                "data_nonlogged" => out.data_nonlogged = Some(as_bool(val, "storage", key, label)?),
                "write_tickets" => {
                    out.write_tickets = Some(as_u64(val, "storage", key, label)? as usize)
                }
                "checkpoint_seconds" => {
                    out.checkpoint_seconds = Some(as_u64(val, "storage", key, label)?)
                }
                other => return Err(unknown_key("storage", other, label)),
            }
        }
    }

    // ---- [tls] -------------------------------------------------------
    if let Some(tls) = get_table(&table, "tls", label)? {
        for (key, val) in tls {
            match key.as_str() {
                "cert_file" => out.tls_cert_file = Some(as_string(val, "tls", key, label)?),
                "key_file" => out.tls_key_file = Some(as_string(val, "tls", key, label)?),
                "ca_file" => out.tls_ca_file = Some(as_string(val, "tls", key, label)?),
                "require_client_cert" => {
                    out.tls_require_client_cert = Some(as_bool(val, "tls", key, label)?)
                }
                other => return Err(unknown_key("tls", other, label)),
            }
        }
    }

    Ok(out)
}

/// Validate a `log_level` against the allowed choices (both TOML and CLI go
/// through here). Mirrors argparse's `choices=` rejection.
pub fn validate_log_level(level: &str, label: &str) -> Result<(), String> {
    if LOG_LEVELS.contains(&level) {
        Ok(())
    } else {
        Err(format!(
            "{label}: invalid log_level {level:?} (valid: {LOG_LEVELS:?})"
        ))
    }
}

// --- small helpers --------------------------------------------------------

fn get_table<'a>(
    root: &'a toml::value::Table,
    name: &str,
    label: &str,
) -> Result<Option<&'a toml::value::Table>, String> {
    match root.get(name) {
        None => Ok(None),
        Some(toml::Value::Table(t)) => Ok(Some(t)),
        Some(other) => Err(format!(
            "{label}: [{name}] must be a table, not {}",
            other.type_str()
        )),
    }
}

fn unknown_key(table: &str, key: &str, label: &str) -> String {
    format!("{label}: unknown key [{table}].{key:?}")
}

fn as_string(v: &toml::Value, table: &str, key: &str, label: &str) -> Result<String, String> {
    v.as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{label}: [{table}].{key} must be a string"))
}

fn as_bool(v: &toml::Value, table: &str, key: &str, label: &str) -> Result<bool, String> {
    v.as_bool()
        .ok_or_else(|| format!("{label}: [{table}].{key} must be a boolean"))
}

/// Accept a TOML integer OR float for a seconds field (mirrors config.py's
/// permissive `float` fields).
fn as_f64(v: &toml::Value, table: &str, key: &str, label: &str) -> Result<f64, String> {
    if let Some(f) = v.as_float() {
        Ok(f)
    } else if let Some(i) = v.as_integer() {
        Ok(i as f64)
    } else {
        Err(format!("{label}: [{table}].{key} must be a number"))
    }
}

fn as_integer(v: &toml::Value, table: &str, key: &str, label: &str) -> Result<i64, String> {
    v.as_integer()
        .ok_or_else(|| format!("{label}: [{table}].{key} must be an integer"))
}

fn as_u16(v: &toml::Value, table: &str, key: &str, label: &str) -> Result<u16, String> {
    let i = as_integer(v, table, key, label)?;
    u16::try_from(i)
        .map_err(|_| format!("{label}: [{table}].{key} out of range for a port (0..=65535)"))
}

fn as_u32(v: &toml::Value, table: &str, key: &str, label: &str) -> Result<u32, String> {
    let i = as_integer(v, table, key, label)?;
    u32::try_from(i).map_err(|_| format!("{label}: [{table}].{key} out of range (0..=4294967295)"))
}

fn as_u64(v: &toml::Value, table: &str, key: &str, label: &str) -> Result<u64, String> {
    let i = as_integer(v, table, key, label)?;
    u64::try_from(i).map_err(|_| format!("{label}: [{table}].{key} must be a non-negative integer"))
}

fn as_usize(v: &toml::Value, table: &str, key: &str, label: &str) -> Result<usize, String> {
    let i = as_integer(v, table, key, label)?;
    usize::try_from(i)
        .map_err(|_| format!("{label}: [{table}].{key} must be a non-negative integer"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Write `text` to a fresh temp file and return its path. Uses an explicit
    /// path in the temp dir (never an auto-discovery location) so parallel test
    /// runs don't collide and never trip auto-discovery.
    fn temp_toml(text: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("secantus-cfg-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secantusd.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    fn parse(text: &str) -> Result<ConfigOverrides, String> {
        parse_str(text, "<config>")
    }

    #[test]
    fn defaults_match_config_py() {
        let c = ResolvedConfig::default();
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.port, 27017);
        assert_eq!(c.storage_path, "./secantus-data");
        assert_eq!(c.log_level, "INFO");
        assert!(!c.auth);
        assert!(!c.standalone);
        assert_eq!(c.oplog_retention_seconds, 3600.0);
        assert_eq!(c.oplog_max_entries, 100_000);
        assert_eq!(c.oplog_archive_dir, None);
        assert_eq!(c.noop_heartbeat_seconds, 0.0);
        assert_eq!(c.cache_size, "4G");
        assert_eq!(c.session_max, 1000);
        assert_eq!(c.ttl_sweep_seconds, 60.0);
        assert!(!c.sync_on_commit);
        assert!(!c.tls_require_client_cert);
    }

    #[test]
    fn precedence_default_lt_toml_lt_cli() {
        // TOML sets cache_size + port; CLI overrides only port.
        let toml_over = parse("[server]\nport = 5000\n[storage]\ncache_size = \"512M\"\n").unwrap();
        let mut cfg = ResolvedConfig::default();
        toml_over.apply_to(&mut cfg);
        // TOML applied.
        assert_eq!(cfg.port, 5000);
        assert_eq!(cfg.cache_size, "512M");
        // Now CLI overrides port only.
        let cli = ConfigOverrides {
            port: Some(6000),
            ..Default::default()
        };
        cli.apply_to(&mut cfg);
        assert_eq!(cfg.port, 6000); // CLI wins
        assert_eq!(cfg.cache_size, "512M"); // TOML still stands
        assert_eq!(cfg.host, "127.0.0.1"); // default untouched
    }

    #[test]
    fn resolve_explicit_path_applies_toml() {
        let path = temp_toml("[storage]\ncache_size = \"700M\"\n");
        let (cfg, source) = resolve(Some(&path), &ConfigOverrides::default()).unwrap();
        assert_eq!(cfg.cache_size, "700M");
        assert_eq!(source.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn resolve_explicit_missing_file_errors() {
        let missing = std::env::temp_dir().join("secantus-does-not-exist-xyz.toml");
        let err = resolve(Some(&missing), &ConfigOverrides::default()).unwrap_err();
        assert!(err.contains("config file not found"), "{err}");
    }

    #[test]
    fn unknown_key_rejected() {
        let err = parse("[storage]\ncache_seize = \"1G\"\n").unwrap_err();
        assert!(err.contains("unknown key"), "{err}");
        assert!(err.contains("cache_seize"), "{err}");
    }

    #[test]
    fn unknown_table_rejected() {
        let err = parse("[bogus]\nx = 1\n").unwrap_err();
        assert!(err.contains("unknown top-level table"), "{err}");
        assert!(err.contains("bogus"), "{err}");
    }

    #[test]
    fn vestigial_oplog_archive_dir_rejected() {
        // config.py has a rename entry for oplog.archive_dir but excludes it
        // from the allowed set, so it must be rejected as unknown.
        let err = parse("[oplog]\narchive_dir = \"/tmp/x\"\n").unwrap_err();
        assert!(err.contains("unknown key"), "{err}");
        assert!(err.contains("archive_dir"), "{err}");
    }

    #[test]
    fn log_level_validation() {
        assert!(parse("[server]\nlog_level = \"DEBUG\"\n").is_ok());
        let err = parse("[server]\nlog_level = \"TRACE\"\n").unwrap_err();
        assert!(err.contains("invalid log_level"), "{err}");
    }

    #[test]
    fn renames_applied() {
        let o = parse(
            "[oplog]\nretention_seconds = 120\nmax_entries = 42\n\
             [tls]\ncert_file = \"c.pem\"\nkey_file = \"k.pem\"\n\
             ca_file = \"ca.pem\"\nrequire_client_cert = true\n",
        )
        .unwrap();
        assert_eq!(o.oplog_retention_seconds, Some(120.0));
        assert_eq!(o.oplog_max_entries, Some(42));
        assert_eq!(o.tls_cert_file.as_deref(), Some("c.pem"));
        assert_eq!(o.tls_key_file.as_deref(), Some("k.pem"));
        assert_eq!(o.tls_ca_file.as_deref(), Some("ca.pem"));
        assert_eq!(o.tls_require_client_cert, Some(true));
    }

    #[test]
    fn seconds_accepts_int_or_float() {
        let a = parse("[oplog]\nretention_seconds = 90\n").unwrap();
        assert_eq!(a.oplog_retention_seconds, Some(90.0));
        let b = parse("[oplog]\nretention_seconds = 90.5\n").unwrap();
        assert_eq!(b.oplog_retention_seconds, Some(90.5));
    }

    #[test]
    fn storage_knobs_resolve() {
        let o = parse(
            "[storage]\ncache_size = \"2G\"\nsession_max = 200\n\
             ttl_sweep_seconds = 30\nsync_on_commit = true\n",
        )
        .unwrap();
        assert_eq!(o.cache_size.as_deref(), Some("2G"));
        assert_eq!(o.session_max, Some(200));
        assert_eq!(o.ttl_sweep_seconds, Some(30.0));
        assert_eq!(o.sync_on_commit, Some(true));
    }

    #[test]
    fn storage_mode_keys_parse() {
        let o = parse(
            "[storage]\noplog_async = true\noplog_nonlogged = true\n\
             data_nonlogged = false\ncheckpoint_seconds = 30\n",
        )
        .unwrap();
        assert_eq!(o.oplog_async, Some(true));
        assert_eq!(o.oplog_nonlogged, Some(true));
        assert_eq!(o.data_nonlogged, Some(false));
        assert_eq!(o.checkpoint_seconds, Some(30));
    }

    #[test]
    fn storage_mode_keys_reject_bad_types() {
        assert!(parse("[storage]\noplog_async = \"yes\"\n").is_err());
        assert!(parse("[storage]\ncheckpoint_seconds = -1\n").is_err());
        assert!(parse("[storage]\ncheckpoint_seconds = 1.5\n").is_err());
    }

    #[test]
    fn standalone_from_toml() {
        let o = parse("[server]\nstandalone = true\n").unwrap();
        assert_eq!(o.standalone, Some(true));
        let mut cfg = ResolvedConfig::default();
        o.apply_to(&mut cfg);
        assert!(cfg.standalone);
        // and false is honoured (distinct from "unset")
        let o2 = parse("[server]\nstandalone = false\n").unwrap();
        assert_eq!(o2.standalone, Some(false));
    }

    #[test]
    fn malformed_toml_errors() {
        let err = parse("this is not = = valid toml").unwrap_err();
        assert!(err.contains("invalid TOML"), "{err}");
    }

    #[test]
    fn non_table_section_errors() {
        let err = parse("server = 5\n").unwrap_err();
        assert!(err.contains("must be a table"), "{err}");
    }

    #[test]
    fn wrong_type_rejected() {
        let err = parse("[server]\nport = \"nope\"\n").unwrap_err();
        assert!(err.contains("integer"), "{err}");
    }
}
