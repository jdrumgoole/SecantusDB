//! CLI argument parsing for the standalone `secantusd-rs` binary (R7).
//!
//! Lives here — not in the bin crate — so it is WT-free and unit-testable in
//! the clean workspace (`cargo test -p secantus-server`). The WT-linked
//! `crates/secantusdb` bin consumes [`parse_args`], resolves the TOML config
//! (see [`crate::config`]), and maps the [`ResolvedConfig`] onto
//! `secantus_storage::Storage` + `secantus_server::bind`.
//!
//! Mirrors `src/secantus/cli.py`'s flag surface. Every value-bearing flag is
//! optional at the CLI layer: a flag the user did **not** pass stays `None` in
//! the emitted [`ConfigOverrides`], so it can defer to the TOML file / built-in
//! defaults (the precedence rule enforced in [`crate::config::resolve`]).
//! Boolean flags (`--auth`, `--standalone`, `--sync-on-commit`,
//! `--tls-require-client-cert`) are `store_true`: the CLI can only flip them to
//! `true` (TOML can set either value). Hand-rolled (no `clap`) to keep the
//! dependency tree flat; both `--flag value` and `--flag=value` spellings work.

use std::path::PathBuf;

use crate::config::{validate_log_level, ConfigOverrides, ResolvedConfig};
use crate::{ServerConfig, TlsOptions};

/// The raw result of parsing the command line: the config-file path (if
/// `--config` was passed) and the set of overrides the user actually typed.
/// The bin crate hands this to [`crate::config::resolve`] to layer the TOML
/// file underneath and produce a [`ResolvedConfig`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedRun {
    /// The `--config PATH` value, if passed (disables auto-discovery).
    pub config_path: Option<PathBuf>,
    /// The CLI flags the user actually passed (highest precedence).
    pub overrides: ConfigOverrides,
}

/// A fully-resolved run configuration: where to bind, the WiredTiger home, and
/// the storage/oplog/logging knobs. Built from a [`ResolvedConfig`] by
/// [`CliArgs::from_resolved`] after TOML + CLI precedence has been applied. The
/// `--standalone` → `replica_set_name` derivation happens here (post-precedence)
/// so a TOML `[server] standalone = true/false` is honoured and a CLI
/// `--standalone` forces STANDALONE.
#[derive(Debug, Clone, PartialEq)]
pub struct CliArgs {
    pub host: String,
    pub port: u16,
    pub storage_path: String,
    pub replica_set_name: Option<String>,
    pub require_auth: bool,
    pub tls: Option<CliTls>,
    /// PITR v2: archive pruned oplog rows to this directory (off by default).
    pub oplog_archive_dir: Option<String>,
    pub log_level: String,
    pub cache_size: String,
    pub log_file_max: String,
    pub session_max: u32,
    pub sync_on_commit: bool,
    pub noop_heartbeat_seconds: f64,
    pub oplog_retention_seconds: f64,
    pub oplog_max_entries: usize,
    pub ttl_sweep_seconds: f64,
    /// Storage write-path modes (`--oplog-async` / `--oplog-nonlogged` /
    /// `--data-nonlogged` / `--checkpoint-seconds`). `None` defers to the
    /// matching `SECANTUS_*` env var via `StorageOptions`.
    pub oplog_async: Option<bool>,
    pub oplog_nonlogged: Option<bool>,
    pub data_nonlogged: Option<bool>,
    pub checkpoint_seconds: Option<u64>,
    /// Admission control: cap on concurrent engine writes (0 / None = off).
    pub write_tickets: Option<usize>,
}

/// TLS options in plain-data form (the lib's [`TlsOptions`] is not `PartialEq`,
/// which the parser tests want).
#[derive(Debug, Clone, PartialEq)]
pub struct CliTls {
    pub cert_file: String,
    pub key_file: String,
    pub ca_file: Option<String>,
    pub require_client_cert: bool,
}

impl CliArgs {
    /// Build resolved run args from a [`ResolvedConfig`] (TOML + CLI already
    /// layered). Applies the TLS pairing rules and derives `replica_set_name`
    /// from `standalone`. Returns an error string if the TLS options are
    /// inconsistent (mirrors `server.py`'s constructor checks).
    pub fn from_resolved(cfg: &ResolvedConfig) -> Result<Self, String> {
        let tls = build_tls(
            cfg.tls_cert_file.clone(),
            cfg.tls_key_file.clone(),
            cfg.tls_ca_file.clone(),
            cfg.tls_require_client_cert,
        )?;
        Ok(CliArgs {
            host: cfg.host.clone(),
            port: cfg.port,
            storage_path: cfg.storage_path.clone(),
            replica_set_name: if cfg.standalone {
                None
            } else {
                Some("secantus".to_string())
            },
            require_auth: cfg.auth,
            tls,
            oplog_archive_dir: cfg.oplog_archive_dir.clone(),
            log_level: cfg.log_level.clone(),
            cache_size: cfg.cache_size.clone(),
            log_file_max: cfg.log_file_max.clone(),
            session_max: cfg.session_max,
            sync_on_commit: cfg.sync_on_commit,
            noop_heartbeat_seconds: cfg.noop_heartbeat_seconds,
            oplog_retention_seconds: cfg.oplog_retention_seconds,
            oplog_max_entries: cfg.oplog_max_entries,
            ttl_sweep_seconds: cfg.ttl_sweep_seconds,
            oplog_async: cfg.oplog_async,
            oplog_nonlogged: cfg.oplog_nonlogged,
            data_nonlogged: cfg.data_nonlogged,
            checkpoint_seconds: cfg.checkpoint_seconds,
            write_tickets: cfg.write_tickets,
        })
    }

    /// The `ServerConfig` for `bind`.
    pub fn server_config(&self) -> ServerConfig {
        ServerConfig {
            replica_set_name: self.replica_set_name.clone(),
            require_auth: self.require_auth,
            tls: self.tls.as_ref().map(|t| TlsOptions {
                cert_file: t.cert_file.clone(),
                key_file: t.key_file.clone(),
                ca_file: t.ca_file.clone(),
                require_client_cert: t.require_client_cert,
            }),
            ..ServerConfig::default()
        }
    }

    /// The `host:port` string for `bind`.
    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Apply the TLS pairing rules (cert+key both-or-neither; CA / mandatory-mTLS
/// need cert+key; mandatory-mTLS needs a CA). Shared between CLI-only parsing
/// and the full resolved-config path.
fn build_tls(
    cert_file: Option<String>,
    key_file: Option<String>,
    ca_file: Option<String>,
    require_client_cert: bool,
) -> Result<Option<CliTls>, String> {
    let tls = match (cert_file, key_file) {
        (Some(cert_file), Some(key_file)) => Some(CliTls {
            cert_file,
            key_file,
            ca_file,
            require_client_cert,
        }),
        (None, None) => {
            if ca_file.is_some() {
                return Err("--tls-ca-file requires --tls-cert-file and --tls-key-file".to_string());
            }
            if require_client_cert {
                return Err(
                    "--tls-require-client-cert requires --tls-cert-file and --tls-key-file"
                        .to_string(),
                );
            }
            None
        }
        _ => return Err("--tls-cert-file and --tls-key-file must be passed together".to_string()),
    };
    if let Some(t) = &tls {
        if t.require_client_cert && t.ca_file.is_none() {
            return Err("--tls-require-client-cert requires --tls-ca-file".to_string());
        }
    }
    Ok(tls)
}

/// Outcome of parsing: run the server (with the raw CLI overrides + config
/// path), or print a text and exit cleanly.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Run(Box<ParsedRun>),
    /// `--help`: the usage text to print to stdout.
    Help(String),
    /// `--version`: the version line to print to stdout.
    Version(String),
}

/// Parse `args` (NOT including the binary name, i.e. `env::args().skip(1)`).
///
/// Emits a [`ParsedRun`] carrying only the flags the user passed — the TOML
/// file is layered underneath by [`crate::config::resolve`] afterwards.
///
/// Errors are user-facing strings; the bin prints them to stderr with the
/// usage hint and exits 2 (argparse's exit code for bad args).
pub fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut config_path: Option<PathBuf> = None;
    let mut o = ConfigOverrides::default();
    // TLS bits are parsed piecemeal, then validated by the resolved-config path;
    // but a CLI-only inconsistency (e.g. `--tls-cert-file` alone) must still be
    // caught here so a bad command line fails before TOML is even read.
    let mut tls_cert_file: Option<String> = None;
    let mut tls_key_file: Option<String> = None;
    let mut tls_ca_file: Option<String> = None;
    let mut tls_require_client_cert = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        // Split --flag=value into (--flag, Some(value)).
        let (flag, inline): (&str, Option<String>) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        // A value-bearing flag takes its inline form or the next arg.
        let mut take_value = |name: &str| -> Result<String, String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => iter
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("{name} requires a value")),
            }
        };

        match flag {
            "--help" | "-h" => return Ok(Parsed::Help(usage())),
            "--version" => {
                return Ok(Parsed::Version(format!(
                    "secantusd-rs {}",
                    env!("CARGO_PKG_VERSION")
                )))
            }
            "--config" => config_path = Some(PathBuf::from(take_value("--config")?)),
            "--host" => o.host = Some(take_value("--host")?),
            "--port" => {
                let raw = take_value("--port")?;
                o.port =
                    Some(raw.parse::<u16>().map_err(|_| {
                        format!("--port expects an integer in 0..=65535, got {raw:?}")
                    })?);
            }
            "--storage-path" => o.storage_path = Some(take_value("--storage-path")?),
            "--log-level" => {
                let lvl = take_value("--log-level")?;
                validate_log_level(&lvl, "secantusd-rs").map_err(|_| {
                    format!("--log-level must be one of DEBUG/INFO/WARNING/ERROR, got {lvl:?}")
                })?;
                o.log_level = Some(lvl);
            }
            "--cache-size" => o.cache_size = Some(take_value("--cache-size")?),
            "--log-file-max" => o.log_file_max = Some(take_value("--log-file-max")?),
            "--write-tickets" => {
                let raw = take_value("--write-tickets")?;
                o.write_tickets = Some(
                    raw.parse::<usize>()
                        .map_err(|_| format!("--write-tickets: {raw:?} is not a number"))?,
                );
            }
            "--session-max" => {
                let raw = take_value("--session-max")?;
                o.session_max = Some(raw.parse::<u32>().map_err(|_| {
                    format!("--session-max expects a non-negative integer, got {raw:?}")
                })?);
            }
            "--sync-on-commit" => o.sync_on_commit = Some(true),
            "--oplog-async" => o.oplog_async = Some(true),
            "--oplog-nonlogged" => o.oplog_nonlogged = Some(true),
            "--data-nonlogged" => o.data_nonlogged = Some(true),
            "--checkpoint-seconds" => {
                let raw = take_value("--checkpoint-seconds")?;
                o.checkpoint_seconds = Some(raw.parse::<u64>().map_err(|_| {
                    format!("--checkpoint-seconds expects a non-negative integer, got {raw:?}")
                })?);
            }
            "--noop-heartbeat-seconds" => {
                let raw = take_value("--noop-heartbeat-seconds")?;
                o.noop_heartbeat_seconds = Some(raw.parse::<f64>().map_err(|_| {
                    format!("--noop-heartbeat-seconds expects a number, got {raw:?}")
                })?);
            }
            "--oplog-retention-seconds" => {
                let raw = take_value("--oplog-retention-seconds")?;
                o.oplog_retention_seconds = Some(raw.parse::<f64>().map_err(|_| {
                    format!("--oplog-retention-seconds expects a number, got {raw:?}")
                })?);
            }
            "--oplog-max-entries" => {
                let raw = take_value("--oplog-max-entries")?;
                o.oplog_max_entries = Some(raw.parse::<usize>().map_err(|_| {
                    format!("--oplog-max-entries expects a non-negative integer, got {raw:?}")
                })?);
            }
            "--auth" => o.auth = Some(true),
            "--standalone" => o.standalone = Some(true),
            "--tls-cert-file" => tls_cert_file = Some(take_value("--tls-cert-file")?),
            "--tls-key-file" => tls_key_file = Some(take_value("--tls-key-file")?),
            "--tls-ca-file" => tls_ca_file = Some(take_value("--tls-ca-file")?),
            "--tls-require-client-cert" => tls_require_client_cert = true,
            "--oplog-archive-dir" => o.oplog_archive_dir = Some(take_value("--oplog-archive-dir")?),
            other => return Err(format!("unknown argument: {other}")),
        }
        // Reject `--auth=yes`-style inline values on boolean / no-value flags.
        if inline.is_some()
            && matches!(
                flag,
                "--auth"
                    | "--standalone"
                    | "--sync-on-commit"
                    | "--oplog-async"
                    | "--oplog-nonlogged"
                    | "--data-nonlogged"
                    | "--tls-require-client-cert"
                    | "--help"
                    | "--version"
            )
        {
            return Err(format!("{flag} does not take a value"));
        }
    }

    // Validate the CLI-only TLS combination up front (a bad command line fails
    // before TOML is read). The resolved-config path re-validates the merged
    // result (a TOML-supplied cert/key still gets checked).
    let cli_tls = build_tls(
        tls_cert_file.clone(),
        tls_key_file.clone(),
        tls_ca_file.clone(),
        tls_require_client_cert,
    )?;
    if let Some(t) = cli_tls {
        o.tls_cert_file = Some(t.cert_file);
        o.tls_key_file = Some(t.key_file);
        o.tls_ca_file = t.ca_file;
        if t.require_client_cert {
            o.tls_require_client_cert = Some(true);
        }
    }

    Ok(Parsed::Run(Box::new(ParsedRun {
        config_path,
        overrides: o,
    })))
}

/// The `--help` text. Mirrors the wording of `src/secantus/cli.py`.
pub fn usage() -> String {
    format!(
        "\
secantusd-rs {} — standalone single-node MongoDB-compatible server (Rust)

Flags override values in secantusd.toml; secantusd.toml overrides built-in
defaults.

USAGE:
    secantusd-rs [OPTIONS]

OPTIONS:
    --config PATH                Path to a secantusd.toml config file. When
                                 omitted, auto-discovers ./secantusd.toml,
                                 ~/.secantus/secantusd.toml,
                                 /etc/secantus/secantusd.toml (and the legacy
                                 secantusdb.toml at each). Passing this flag
                                 disables auto-discovery.
    --host HOST                  Bind address (default: 127.0.0.1)
    --port PORT                  Bind port; 0 picks an ephemeral port and
                                 prints it on startup (default: 27017)
    --storage-path PATH          WiredTiger home directory; created if missing,
                                 reopened intact across restarts
                                 (default: ./secantus-data)
    --log-level LEVEL            One of DEBUG/INFO/WARNING/ERROR (default: INFO)
    --auth                       Require SCRAM-SHA-256 authentication for
                                 non-handshake commands
    --standalone                 Drop the single-node replica-set advertisement
                                 from the hello reply (drivers see a STANDALONE
                                 topology; change streams need the default)
    --cache-size SIZE            WiredTiger cache size, unit-suffixed string like
                                 '256M', '1G', '8G' (default: 4G)
    --log-file-max SIZE          WiredTiger WAL log file_max, unit-suffixed like
                                 '128MB', '1GB', '2GB' (default: 2GB; 2GB is WT's
                                 cap. Bigger = fewer log rotations under write
                                 load = higher throughput; files are sparse.)
    --session-max N              WiredTiger session_max — concurrent WT session
                                 cap (default: 1000)
    --write-tickets N            Admission control: cap on writes concurrently
                                 inside the storage engine; further writers
                                 queue OUTSIDE it. 0 = unlimited (default).
                                 Bounds the p99.9 tail under write saturation,
                                 which unbounded concurrency does not — see
                                 tasks/backlog.md. Start near the core count.
    --sync-on-commit             Fsync the WT log on every transaction commit
                                 (closes the writeConcern j:true durability gap
                                 at a throughput cost; off by default)
    --oplog-async                Persist oplog entries via a background drainer
                                 pool instead of inside each write's commit
                                 (higher write throughput; change-stream events
                                 become visible when drained. Default: off, or
                                 SECANTUS_OPLOG_ASYNC)
    --oplog-nonlogged            Create oplog tables log=(enabled=false) —
                                 checkpoint-durable only. Measurement/ephemeral
                                 use; a crash can lose the oplog tail (default:
                                 off, or SECANTUS_OPLOG_NONLOGGED)
    --data-nonlogged             mongod's split: WAL-log only the oplog; data
                                 tables recover by oplog replay from the last
                                 stable checkpoint. Create-time for fresh
                                 stores; an existing store keeps its recorded
                                 mode (default: off, or SECANTUS_DATA_NONLOGGED)
    --checkpoint-seconds S       Stable-checkpoint cadence for --data-nonlogged
                                 (default: 60, or SECANTUS_CHECKPOINT_SECONDS)
    --noop-heartbeat-seconds S   Emit a periodic {{op:'n'}} oplog heartbeat every
                                 S seconds so quiet change-stream cursors keep
                                 their resume token inside the retention window.
                                 0 = disabled (default)
    --oplog-retention-seconds S  Oplog wall-clock retention; entries older than
                                 this are pruned opportunistically (default: 3600)
    --oplog-max-entries N        Oplog count cap; whichever bound hits first
                                 prunes the oldest entries (default: 100000)
    --oplog-archive-dir DIR      PITR v2: archive pruned oplog rows here before
                                 they are dropped, so recovery can reach a time
                                 before the live oplog floor (off by default)
    --tls-cert-file PATH         PEM server certificate chain (with
                                 --tls-key-file, enables TLS)
    --tls-key-file PATH          PEM private key matching --tls-cert-file
    --tls-ca-file PATH           PEM CA bundle to verify client certs (mTLS)
    --tls-require-client-cert    Reject clients without a valid X.509 cert;
                                 requires --tls-ca-file
    --version                    Print the version and exit
    -h, --help                   Print this help and exit
",
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(words: &[&str]) -> Result<Parsed, String> {
        let owned: Vec<String> = words.iter().map(|s| s.to_string()).collect();
        parse_args(&owned)
    }

    /// Parse into a [`ParsedRun`], then resolve with no TOML file so the result
    /// reflects CLI-over-defaults (the common test path).
    fn run(words: &[&str]) -> CliArgs {
        let pr = match parse(words).expect("parse should succeed") {
            Parsed::Run(pr) => pr,
            other => panic!("expected Run, got {other:?}"),
        };
        let mut cfg = ResolvedConfig::default();
        pr.overrides.apply_to_for_test(&mut cfg);
        CliArgs::from_resolved(&cfg).expect("resolved config should be valid")
    }

    // A tiny test shim so we don't need to expose `ConfigOverrides::apply_to`.
    impl ConfigOverrides {
        fn apply_to_for_test(&self, base: &mut ResolvedConfig) {
            // Re-use the real precedence application via config::resolve would
            // need a file; instead round-trip through resolve with no file by
            // constructing directly. Simplest: mirror the public resolve path.
            let (cfg, _src) = crate::config::resolve(None, self).expect("resolve without file");
            *base = cfg;
        }
    }

    #[test]
    fn defaults() {
        let a = run(&[]);
        assert_eq!(a.host, "127.0.0.1");
        assert_eq!(a.port, 27017);
        assert_eq!(a.storage_path, "./secantus-data");
        assert_eq!(a.replica_set_name.as_deref(), Some("secantus"));
        assert!(!a.require_auth);
        assert!(a.tls.is_none());
        assert_eq!(a.log_level, "INFO");
        assert_eq!(a.cache_size, "4G");
        assert_eq!(a.session_max, 1000);
        assert!(!a.sync_on_commit);
        assert_eq!(a.noop_heartbeat_seconds, 0.0);
        assert_eq!(a.oplog_retention_seconds, 3600.0);
        assert_eq!(a.oplog_max_entries, 100_000);
        assert_eq!(a.ttl_sweep_seconds, 60.0);
    }

    #[test]
    fn space_and_equals_forms() {
        let a = run(&["--host", "0.0.0.0", "--port=27018", "--storage-path=/tmp/x"]);
        assert_eq!(a.host, "0.0.0.0");
        assert_eq!(a.port, 27018);
        assert_eq!(a.storage_path, "/tmp/x");
    }

    #[test]
    fn port_zero_is_ephemeral() {
        assert_eq!(run(&["--port", "0"]).port, 0);
    }

    #[test]
    fn bad_port_rejected() {
        assert!(parse(&["--port", "notaport"]).is_err());
        assert!(parse(&["--port", "70000"]).is_err());
        assert!(parse(&["--port"]).is_err());
    }

    #[test]
    fn auth_and_standalone_flags() {
        let a = run(&["--auth", "--standalone"]);
        assert!(a.require_auth);
        assert_eq!(a.replica_set_name, None);
    }

    #[test]
    fn boolean_flag_rejects_inline_value() {
        assert!(parse(&["--auth=yes"]).is_err());
        assert!(parse(&["--standalone=1"]).is_err());
        assert!(parse(&["--sync-on-commit=1"]).is_err());
        assert!(parse(&["--oplog-async=1"]).is_err());
        assert!(parse(&["--oplog-nonlogged=1"]).is_err());
        assert!(parse(&["--data-nonlogged=1"]).is_err());
    }

    #[test]
    fn storage_mode_flags_default_to_env_deferral() {
        // No flag passed → None → StorageOptions defers to the SECANTUS_* env.
        let a = run(&[]);
        assert_eq!(a.oplog_async, None);
        assert_eq!(a.oplog_nonlogged, None);
        assert_eq!(a.data_nonlogged, None);
        assert_eq!(a.checkpoint_seconds, None);
    }

    #[test]
    fn storage_mode_flags_resolve() {
        let a = run(&[
            "--oplog-async",
            "--oplog-nonlogged",
            "--data-nonlogged",
            "--checkpoint-seconds",
            "15",
        ]);
        assert_eq!(a.oplog_async, Some(true));
        assert_eq!(a.oplog_nonlogged, Some(true));
        assert_eq!(a.data_nonlogged, Some(true));
        assert_eq!(a.checkpoint_seconds, Some(15));
    }

    #[test]
    fn bad_checkpoint_seconds_rejected() {
        assert!(parse(&["--checkpoint-seconds", "-5"]).is_err());
        assert!(parse(&["--checkpoint-seconds", "abc"]).is_err());
        assert!(parse(&["--checkpoint-seconds"]).is_err());
    }

    #[test]
    fn unknown_flag_rejected() {
        let err = parse(&["--bogus"]).unwrap_err();
        assert!(err.contains("--bogus"), "{err}");
    }

    #[test]
    fn new_scalar_flags_resolve() {
        let a = run(&[
            "--log-level",
            "DEBUG",
            "--cache-size",
            "512M",
            "--session-max",
            "200",
            "--sync-on-commit",
            "--noop-heartbeat-seconds",
            "1.5",
            "--oplog-retention-seconds",
            "60",
            "--oplog-max-entries",
            "500",
        ]);
        assert_eq!(a.log_level, "DEBUG");
        assert_eq!(a.cache_size, "512M");
        assert_eq!(a.session_max, 200);
        assert!(a.sync_on_commit);
        assert_eq!(a.noop_heartbeat_seconds, 1.5);
        assert_eq!(a.oplog_retention_seconds, 60.0);
        assert_eq!(a.oplog_max_entries, 500);
    }

    #[test]
    fn bad_log_level_rejected() {
        assert!(parse(&["--log-level", "TRACE"]).is_err());
    }

    #[test]
    fn bad_numeric_flags_rejected() {
        assert!(parse(&["--session-max", "-1"]).is_err());
        assert!(parse(&["--oplog-max-entries", "x"]).is_err());
        assert!(parse(&["--noop-heartbeat-seconds", "abc"]).is_err());
    }

    #[test]
    fn config_flag_captured() {
        let pr = match parse(&["--config", "/tmp/foo.toml"]).unwrap() {
            Parsed::Run(pr) => pr,
            other => panic!("expected Run, got {other:?}"),
        };
        assert_eq!(
            pr.config_path.as_deref(),
            Some(PathBuf::from("/tmp/foo.toml").as_path())
        );
    }

    #[test]
    fn tls_pairing_enforced() {
        assert!(parse(&["--tls-cert-file", "c.pem"]).is_err());
        assert!(parse(&["--tls-key-file", "k.pem"]).is_err());
        assert!(parse(&["--tls-ca-file", "ca.pem"]).is_err());
        assert!(parse(&["--tls-require-client-cert"]).is_err());
        assert!(parse(&[
            "--tls-cert-file",
            "c.pem",
            "--tls-key-file",
            "k.pem",
            "--tls-require-client-cert",
        ])
        .is_err());
    }

    #[test]
    fn tls_full_set() {
        let a = run(&[
            "--tls-cert-file",
            "c.pem",
            "--tls-key-file",
            "k.pem",
            "--tls-ca-file",
            "ca.pem",
            "--tls-require-client-cert",
        ]);
        let cfg = a.server_config();
        assert!(cfg.tls.is_some());
        let t = a.tls.expect("tls should be configured");
        assert_eq!(t.cert_file, "c.pem");
        assert_eq!(t.key_file, "k.pem");
        assert_eq!(t.ca_file.as_deref(), Some("ca.pem"));
        assert!(t.require_client_cert);
    }

    #[test]
    fn help_and_version() {
        assert!(matches!(parse(&["--help"]).unwrap(), Parsed::Help(_)));
        assert!(matches!(parse(&["-h"]).unwrap(), Parsed::Help(_)));
        match parse(&["--version"]).unwrap() {
            Parsed::Version(v) => assert!(v.starts_with("secantusd-rs ")),
            other => panic!("expected Version, got {other:?}"),
        }
    }

    #[test]
    fn bind_addr_formats() {
        assert_eq!(run(&["--port", "0"]).bind_addr(), "127.0.0.1:0");
        assert_eq!(
            run(&["--host", "0.0.0.0", "--port", "27018"]).bind_addr(),
            "0.0.0.0:27018"
        );
    }
}
