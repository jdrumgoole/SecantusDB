//! CLI argument parsing for the standalone `secantusdb` binary (R7).
//!
//! Lives here — not in the bin crate — so it is WT-free and unit-testable in
//! the clean workspace (`cargo test -p secantus-server`). The WT-linked
//! `crates/secantusdb` bin consumes [`parse_args`] and maps the result onto
//! `secantus_storage::Storage` + `secantus_server::bind`.
//!
//! Mirrors the subset of `src/secantus/cli.py`'s flags the Rust server
//! supports today: `--host`, `--port`, `--storage-path`, `--auth`,
//! `--standalone`, and the four `--tls-*` options. Hand-rolled (no `clap`)
//! to keep the dependency tree flat; both `--flag value` and `--flag=value`
//! spellings are accepted.

use crate::{ServerConfig, TlsOptions};

/// Defaults matching `src/secantus/config.py`'s `SecantusConfig`.
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 27017;
const DEFAULT_STORAGE_PATH: &str = "./secantus-data";

/// Parsed CLI: where to bind, where the WiredTiger home lives, and the
/// [`ServerConfig`] handed to `bind`.
#[derive(Debug, Clone, PartialEq)]
pub struct CliArgs {
    pub host: String,
    pub port: u16,
    pub storage_path: String,
    pub replica_set_name: Option<String>,
    pub require_auth: bool,
    pub tls: Option<CliTls>,
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

/// Outcome of parsing: run the server, or print a text and exit cleanly.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Run(CliArgs),
    /// `--help`: the usage text to print to stdout.
    Help(String),
    /// `--version`: the version line to print to stdout.
    Version(String),
}

/// Parse `args` (NOT including the binary name, i.e. `env::args().skip(1)`).
///
/// Errors are user-facing strings; the bin prints them to stderr with the
/// usage hint and exits 2 (argparse's exit code for bad args).
pub fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut host = DEFAULT_HOST.to_string();
    let mut port = DEFAULT_PORT;
    let mut storage_path = DEFAULT_STORAGE_PATH.to_string();
    let mut auth = false;
    let mut standalone = false;
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
                    "secantusdb {}",
                    env!("CARGO_PKG_VERSION")
                )))
            }
            "--host" => host = take_value("--host")?,
            "--port" => {
                let raw = take_value("--port")?;
                port = raw
                    .parse::<u16>()
                    .map_err(|_| format!("--port expects an integer in 0..=65535, got {raw:?}"))?;
            }
            "--storage-path" => storage_path = take_value("--storage-path")?,
            "--auth" => auth = true,
            "--standalone" => standalone = true,
            "--tls-cert-file" => tls_cert_file = Some(take_value("--tls-cert-file")?),
            "--tls-key-file" => tls_key_file = Some(take_value("--tls-key-file")?),
            "--tls-ca-file" => tls_ca_file = Some(take_value("--tls-ca-file")?),
            "--tls-require-client-cert" => tls_require_client_cert = true,
            other => return Err(format!("unknown argument: {other}")),
        }
        // Reject `--auth=yes`-style inline values on boolean flags.
        if inline.is_some()
            && matches!(
                flag,
                "--auth" | "--standalone" | "--tls-require-client-cert" | "--help" | "--version"
            )
        {
            return Err(format!("{flag} does not take a value"));
        }
    }

    // TLS pairing rules (matching server.py / the embedded handle): cert+key
    // both-or-neither; the CA file and mandatory-mTLS flag need cert+key; the
    // mandatory-mTLS flag needs a CA to verify against.
    let tls = match (tls_cert_file, tls_key_file) {
        (Some(cert_file), Some(key_file)) => Some(CliTls {
            cert_file,
            key_file,
            ca_file: tls_ca_file,
            require_client_cert: tls_require_client_cert,
        }),
        (None, None) => {
            if tls_ca_file.is_some() {
                return Err("--tls-ca-file requires --tls-cert-file and --tls-key-file".to_string());
            }
            if tls_require_client_cert {
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

    Ok(Parsed::Run(CliArgs {
        host,
        port,
        storage_path,
        replica_set_name: if standalone {
            None
        } else {
            Some("secantus".to_string())
        },
        require_auth: auth,
        tls,
    }))
}

/// The `--help` text. Mirrors the wording of `src/secantus/cli.py` for the
/// flags the Rust server supports.
pub fn usage() -> String {
    format!(
        "\
secantusdb {} — standalone single-node MongoDB-compatible server (Rust)

USAGE:
    secantusdb [OPTIONS]

OPTIONS:
    --host HOST                  Bind address (default: {DEFAULT_HOST})
    --port PORT                  Bind port; 0 picks an ephemeral port and
                                 prints it on startup (default: {DEFAULT_PORT})
    --storage-path PATH          WiredTiger home directory; created if missing,
                                 reopened intact across restarts
                                 (default: {DEFAULT_STORAGE_PATH})
    --auth                       Require SCRAM-SHA-256 authentication for
                                 non-handshake commands
    --standalone                 Drop the single-node replica-set advertisement
                                 from the hello reply (drivers see a STANDALONE
                                 topology; change streams need the default)
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

    fn run(words: &[&str]) -> CliArgs {
        match parse(words).expect("parse should succeed") {
            Parsed::Run(a) => a,
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn defaults() {
        let a = run(&[]);
        assert_eq!(a.host, DEFAULT_HOST);
        assert_eq!(a.port, DEFAULT_PORT);
        assert_eq!(a.storage_path, DEFAULT_STORAGE_PATH);
        assert_eq!(a.replica_set_name.as_deref(), Some("secantus"));
        assert!(!a.require_auth);
        assert!(a.tls.is_none());
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
    }

    #[test]
    fn unknown_flag_rejected() {
        let err = parse(&["--bogus"]).unwrap_err();
        assert!(err.contains("--bogus"), "{err}");
    }

    #[test]
    fn tls_pairing_enforced() {
        assert!(parse(&["--tls-cert-file", "c.pem"]).is_err());
        assert!(parse(&["--tls-key-file", "k.pem"]).is_err());
        assert!(parse(&["--tls-ca-file", "ca.pem"]).is_err());
        assert!(parse(&["--tls-require-client-cert"]).is_err());
        // require-client-cert without a CA is also an error even with cert+key.
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
            Parsed::Version(v) => assert!(v.starts_with("secantusdb ")),
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
