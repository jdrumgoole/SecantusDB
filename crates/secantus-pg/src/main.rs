//! `secantusd-pg` -- the standalone PostgreSQL-wire server (P1 slice).
//!
//! A CLI wrapper, nothing more: argument parsing, the readiness line, and the
//! signal wait. The accept loop, the shutdown drain, and the storage ownership
//! that makes the close-checkpoint run all live in `secantus_pg::bind`,
//! which the embedded Python handle (`_secantus_server`'s `PgServer`) calls
//! too -- so there is one serve path, not two.

// The binary's allocator. Planning and running a statement allocate heavily
// (the parse tree, the plan, each row's BSON), and on Linux glibc's malloc was
// about a sixth of the server's CPU time for a prepared write. The LIBRARY
// sets no allocator: a test that embeds `PgServer` keeps its own.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::sync::mpsc;
use std::sync::Arc;

use secantus_pg::{
    bind_with, ClientCert, ClientName, DatabaseRegistry, Reloader, ServerOptions, TlsConfig,
};

/// Unblock SIGINT / SIGTERM in the calling thread and give them their
/// default disposition, undoing whatever the parent left. POSIX only:
/// Windows has neither a signal mask nor inherited dispositions, and its
/// console control events reach the `ctrlc` handler as they are.
#[cfg(unix)]
fn reset_stop_signals() {
    // SAFETY: plain libc calls on a zeroed sigset, made before any other
    // thread exists; their failure modes are an invalid signal number, which
    // these are not.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for sig in [libc::SIGINT, libc::SIGTERM] {
            libc::sigaddset(&mut set, sig);
            libc::signal(sig, libc::SIG_DFL);
        }
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
    }
}

#[cfg(not(unix))]
fn reset_stop_signals() {}

/// `--version` output: the version, and the source tree it was built from --
/// or, for a build from a packaged crate (no git tree), `source: crates.io`.
fn version_text() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let origin = option_env!("SECANTUS_SOURCE_ORIGIN").unwrap_or("");
    match (option_env!("SECANTUS_SOURCE_TREE").unwrap_or(""), origin) {
        ("", "") => format!("secantusd-pg {version}\n"),
        ("", origin) => format!("secantusd-pg {version}\nsource: {origin}\n"),
        (tree, _) => format!("secantusd-pg {version}\ntree: {tree}\n"),
    }
}

// NOT `#[tokio::main]`: `bind` owns the runtime, and `RunningPgServer::stop`
// drops it -- which panics if it happens inside a runtime context. `main` is a
// plain blocking function that waits on a signal.
/// Set by SIGHUP; the reload itself is no work for a signal handler.
#[cfg(unix)]
static RELOAD_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn request_reload(_: libc::c_int) {
    RELOAD_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Re-read the login files on SIGHUP, as PostgreSQL does on `pg_ctl reload`.
/// Only a server started with TLS or a `pg_hba.conf` has anything to
/// re-read; one without takes the signal and carries on, as a postmaster
/// does -- SIGHUP never stops PostgreSQL. Windows has no SIGHUP, and
/// `select pg_reload_conf()` is the way there.
#[cfg(unix)]
fn watch_for_reload(reloader: Reloader) {
    let has_files = reloader.has_files();
    // SAFETY: `signal` with a valid signal number and a handler that only
    // stores to an atomic, which is async-signal-safe. This replaces the
    // stop handler `ctrlc` installed for SIGHUP and leaves its others.
    unsafe {
        libc::signal(
            libc::SIGHUP,
            request_reload as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
    let watcher = std::thread::Builder::new()
        .name("secantus-pg-reload".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if RELOAD_REQUESTED.swap(false, std::sync::atomic::Ordering::SeqCst) {
                if !has_files {
                    eprintln!("secantusd-pg: received SIGHUP; there are no files to reload");
                    continue;
                }
                match reloader.reload() {
                    Ok(()) => eprintln!("secantusd-pg: configuration reloaded"),
                    Err(e) => eprintln!("secantusd-pg: configuration was not reloaded: {e}"),
                }
            }
        });
    if let Err(e) = watcher {
        eprintln!("secantusd-pg: SIGHUP will not reload the TLS files: {e}");
    }
}

#[cfg(not(unix))]
fn watch_for_reload(_reloader: Reloader) {}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // FIRST, before any thread exists (a thread inherits the mask it is
    // spawned with): take back SIGINT and SIGTERM. A process inherits its
    // parent's blocked-signal mask and any SIG_IGN disposition across exec --
    // a shell starts a background job with SIGINT ignored, and a supervisor
    // that blocks SIGTERM around its own fork leaves it blocked in the child.
    // Measured 2026-10-05: launched with SIGTERM blocked, `secantusd-pg`
    // ignored SIGTERM entirely (the `ctrlc` handler below was installed but
    // the signal was never delivered) and stopped only on SIGINT.
    reset_stop_signals();
    // `secantusd-pg [<home> [<addr>]] [--database NAME]...`: every
    // `--database` is one more name a client may connect to without a
    // `CREATE DATABASE` first; the builtin `postgres` / `template1` always are.
    let mut home: Option<String> = None;
    let mut addr: Option<String> = None;
    let mut databases: Vec<String> = Vec::new();
    let mut tls_cert_file: Option<String> = None;
    let mut tls_key_file: Option<String> = None;
    let mut tls_ca_file: Option<String> = None;
    let mut tls_client_cert: Option<ClientCert> = None;
    let mut tls_require = false;
    let mut tls_crl_file: Option<String> = None;
    let mut tls_key_passphrase_command: Option<String> = None;
    let mut tls_ident_file: Option<String> = None;
    let mut tls_user_map: Option<String> = None;
    let mut tls_client_name: Option<ClientName> = None;
    let mut hba_file: Option<String> = None;
    let mut ident_file: Option<String> = None;
    let mut trust_unknown_users = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        // `--version` / `--help` BEFORE the positional fallthrough. Without
        // them, `--version` fell through to `home` and the server tried to open
        // a WiredTiger database in a directory called `--version`, reporting
        // `WT_TRY_SALVAGE: database corruption detected` -- an alarming answer
        // to a question every binary is expected to answer. The release
        // workflow sanity-checks the artifact with `--version`, so this is load
        // bearing rather than a nicety.
        if arg == "--version" || arg == "-V" {
            // Two lines: the version for "what did I install?", and the source
            // tree for a bug report. The crate version moves at release
            // cadence, so hundreds of commits share one string and it cannot
            // identify a build on its own. The tree line is OMITTED, not
            // blank, when built without git.
            print!("{}", version_text());
            return Ok(());
        } else if arg == "--help" || arg == "-h" {
            println!(
                "secantusd-pg {} -- standalone PostgreSQL-wire server (SecantusDB)\n\
                 \n\
                 USAGE:\n    \
                 secantusd-pg [<storage-path> [<host:port>]] [--database NAME]...\n                \
                 [--tls-cert-file PATH --tls-key-file PATH [--tls-require]\n                 \
                 [--tls-key-passphrase-command CMD]\n                 \
                 [--tls-ca-file PATH [--tls-client-cert MODE] [--tls-crl-file PATH]\n                  \
                 [--tls-client-name CN|DN]\n                  \
                 [--tls-ident-file PATH --tls-user-map NAME]]]\n                \
                 [--trust-unknown-users]\n                \
                 [--hba-file PATH [--ident-file PATH]]\n\
                 \n\
                 ARGS:\n    \
                 <storage-path>  WiredTiger home directory (default: ./secantus-pg-data)\n    \
                 <host:port>     Bind address (default: 127.0.0.1:25434). Port 0 picks\n                    \
                 an ephemeral port and prints the one it bound.\n\
                 \n\
                 OPTIONS:\n    \
                 --database NAME  A database a client may connect to without CREATE\n                     \
                 DATABASE first. Repeatable. `postgres` and `template1`\n                     \
                 always exist.\n    \
                 --tls-cert-file PATH  PEM certificate chain. With --tls-key-file, a\n                     \
                 client that asks for TLS gets it; one that does not is\n                     \
                 still served in the clear. Without the pair the server\n                     \
                 answers a TLS request with \"not supported\".\n    \
                 --tls-key-file PATH   PEM private key for that certificate.\n    \
                 --tls-key-passphrase-command CMD  A shell command printing the\n                     \
                 passphrase of an encrypted PKCS#8 key\n                     \
                 (ssl_passphrase_command; %p is the prompt).\n    \
                 --tls-require    Refuse a connection that is not TLS (a pg_hba.conf\n                     \
                 with only `hostssl` lines).\n    \
                 --tls-ca-file PATH    PEM certificate authorities. A TLS client is\n                     \
                 asked for a certificate, and one it sends must be\n                     \
                 signed by one of these.\n    \
                 --tls-client-cert MODE  What a client must send. `verify-ca`: a\n                     \
                 certificate. `verify-full`: one whose common name is\n                     \
                 the user name, beside the password. `cert`: the same,\n                     \
                 and no password is asked for. Each needs\n                     \
                 --tls-ca-file and refuses a connection that is not TLS.\n    \
                 --tls-crl-file PATH   PEM revocation lists (ssl_crl_file). A client\n                     \
                 certificate one of them names is refused, and so is\n                     \
                 one whose CA has no list here or an expired one.\n    \
                 --tls-client-name CN|DN  What of the certificate names the user:\n                     \
                 its common name (the default) or its whole subject,\n                     \
                 as in `CN=alice,O=Example`. With verify-full or cert.\n    \
                 --tls-ident-file PATH  A pg_ident.conf. With --tls-user-map NAME\n                     \
                 and `--tls-client-cert cert`, a certificate logs in as\n                     \
                 the roles that map gives it.\n    \
                 --tls-user-map NAME   The map in --tls-ident-file to use.\n    \
                 --hba-file PATH  A pg_hba.conf. Its lines decide every login: the\n                     \
                 first that matches a connection's type, database, user\n                     \
                 and address says how it must prove who it is, and a\n                     \
                 connection no line matches is refused. In place of\n                     \
                 --tls-require, --tls-client-cert, --tls-client-name,\n                     \
                 --tls-user-map and --trust-unknown-users.\n    \
                 --ident-file PATH  The pg_ident.conf for a map= in the pg_hba.conf.\n    \
                 --trust-unknown-users  Let a user there is no role for connect\n                     \
                 without a password, as a role with no password does.\n                     \
                 By default such a user is asked for one, which\n                     \
                 fails, as PostgreSQL refuses a role that does not\n                     \
                 exist. (--refuse-unknown-users, the old opt-in, is\n                     \
                 accepted and changes nothing.)\n    \
                 -V, --version    Print version and exit\n    \
                 -h, --help       Print this help and exit",
                env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        } else if arg == "--database" {
            let name = args.next().ok_or("--database needs a name")?;
            databases.push(name);
        } else if let Some(name) = arg.strip_prefix("--database=") {
            databases.push(name.to_string());
        } else if arg == "--tls-cert-file" {
            tls_cert_file = Some(args.next().ok_or("--tls-cert-file needs a path")?);
        } else if let Some(path) = arg.strip_prefix("--tls-cert-file=") {
            tls_cert_file = Some(path.to_string());
        } else if arg == "--tls-key-file" {
            tls_key_file = Some(args.next().ok_or("--tls-key-file needs a path")?);
        } else if let Some(path) = arg.strip_prefix("--tls-key-file=") {
            tls_key_file = Some(path.to_string());
        } else if arg == "--tls-ca-file" {
            tls_ca_file = Some(args.next().ok_or("--tls-ca-file needs a path")?);
        } else if let Some(path) = arg.strip_prefix("--tls-ca-file=") {
            tls_ca_file = Some(path.to_string());
        } else if arg == "--tls-client-cert" {
            let mode = args.next().ok_or("--tls-client-cert needs a mode")?;
            tls_client_cert = Some(
                mode.parse()
                    .map_err(|e| format!("--tls-client-cert: {e}"))?,
            );
        } else if let Some(mode) = arg.strip_prefix("--tls-client-cert=") {
            tls_client_cert = Some(
                mode.parse()
                    .map_err(|e| format!("--tls-client-cert: {e}"))?,
            );
        } else if arg == "--tls-key-passphrase-command" {
            tls_key_passphrase_command = Some(
                args.next()
                    .ok_or("--tls-key-passphrase-command needs a command")?,
            );
        } else if let Some(command) = arg.strip_prefix("--tls-key-passphrase-command=") {
            tls_key_passphrase_command = Some(command.to_string());
        } else if arg == "--tls-crl-file" {
            tls_crl_file = Some(args.next().ok_or("--tls-crl-file needs a path")?);
        } else if let Some(path) = arg.strip_prefix("--tls-crl-file=") {
            tls_crl_file = Some(path.to_string());
        } else if arg == "--tls-ident-file" {
            tls_ident_file = Some(args.next().ok_or("--tls-ident-file needs a path")?);
        } else if let Some(path) = arg.strip_prefix("--tls-ident-file=") {
            tls_ident_file = Some(path.to_string());
        } else if arg == "--tls-user-map" {
            tls_user_map = Some(args.next().ok_or("--tls-user-map needs a map name")?);
        } else if let Some(name) = arg.strip_prefix("--tls-user-map=") {
            tls_user_map = Some(name.to_string());
        } else if arg == "--tls-client-name" {
            let name = args.next().ok_or("--tls-client-name needs CN or DN")?;
            tls_client_name = Some(
                name.parse()
                    .map_err(|e| format!("--tls-client-name: {e}"))?,
            );
        } else if let Some(name) = arg.strip_prefix("--tls-client-name=") {
            tls_client_name = Some(
                name.parse()
                    .map_err(|e| format!("--tls-client-name: {e}"))?,
            );
        } else if arg == "--hba-file" {
            hba_file = Some(args.next().ok_or("--hba-file needs a path")?);
        } else if let Some(path) = arg.strip_prefix("--hba-file=") {
            hba_file = Some(path.to_string());
        } else if arg == "--ident-file" {
            ident_file = Some(args.next().ok_or("--ident-file needs a path")?);
        } else if let Some(path) = arg.strip_prefix("--ident-file=") {
            ident_file = Some(path.to_string());
        } else if arg == "--tls-require" {
            tls_require = true;
        } else if arg == "--trust-unknown-users" {
            trust_unknown_users = true;
        } else if arg == "--refuse-unknown-users" {
            // The default since 0.1.0-beta.17; accepted so a script that
            // asked for it still starts.
        } else if arg.starts_with("--") {
            // Never a storage path: an unknown or misspelt option fell through
            // to `home` and the server came up over a directory named after
            // it -- for a TLS option, in the clear.
            return Err(format!("unknown option: {arg}").into());
        } else if home.is_none() {
            home = Some(arg);
        } else if addr.is_none() {
            addr = Some(arg);
        } else {
            return Err(format!("unexpected argument: {arg}").into());
        }
    }
    // Both or neither, checked before anything is opened: half a pair must
    // not start a server that then serves without TLS.
    // The other TLS options say what a TLS connection is held to, so each
    // needs the pair; and a client certificate needs a CA to be checked by.
    if tls_cert_file.is_none() && tls_key_file.is_none() {
        for (given, option) in [
            (tls_ca_file.is_some(), "--tls-ca-file"),
            (tls_client_cert.is_some(), "--tls-client-cert"),
            (tls_require, "--tls-require"),
            (tls_crl_file.is_some(), "--tls-crl-file"),
            (
                tls_key_passphrase_command.is_some(),
                "--tls-key-passphrase-command",
            ),
            (tls_ident_file.is_some(), "--tls-ident-file"),
            (tls_user_map.is_some(), "--tls-user-map"),
            (tls_client_name.is_some(), "--tls-client-name"),
        ] {
            if given {
                return Err(format!("{option} needs --tls-cert-file and --tls-key-file").into());
            }
        }
    }
    if tls_client_cert.is_some() && tls_ca_file.is_none() {
        return Err("--tls-client-cert needs --tls-ca-file".into());
    }
    let tls = match (tls_cert_file, tls_key_file) {
        (Some(cert), Some(key)) => {
            let mut tls = TlsConfig::new(cert, key).require_tls(tls_require);
            if let Some(ca) = tls_ca_file {
                tls = tls.ca_file(ca);
            }
            if let Some(crl) = tls_crl_file {
                tls = tls.crl_file(crl);
            }
            if let Some(command) = tls_key_passphrase_command {
                tls = tls.key_passphrase_command(command);
            }
            if let Some(name) = tls_client_name {
                tls = tls.client_name(name);
            }
            match (tls_ident_file, tls_user_map) {
                (Some(file), Some(map)) => tls = tls.user_map(file, map),
                (Some(_), None) => return Err("--tls-ident-file needs --tls-user-map".into()),
                (None, Some(_)) => return Err("--tls-user-map needs --tls-ident-file".into()),
                (None, None) => {}
            }
            Some(tls.client_cert(tls_client_cert.unwrap_or_default()))
        }
        (None, None) => None,
        (Some(_), None) => return Err("--tls-cert-file needs --tls-key-file".into()),
        (None, Some(_)) => return Err("--tls-key-file needs --tls-cert-file".into()),
    };
    let home = home.unwrap_or_else(|| "./secantus-pg-data".into());
    let addr = addr.unwrap_or_else(|| "127.0.0.1:25434".into());
    let databases = Arc::new(DatabaseRegistry::new("postgres", databases));

    // Create the home if it is missing, which is what `secantusd-rs` does
    // (`--storage-path … created if missing`). Without it a first run against a
    // fresh path failed inside WiredTiger with
    // `WiredTiger.lock: handle-open: open: No such file or directory` and a
    // `WT_TRY_SALVAGE: database corruption detected` -- a frightening answer to
    // "I pointed it at a new directory". Found by the release smoke test, which
    // is the first thing to drive this binary the way a new user would.
    std::fs::create_dir_all(&home)
        .map_err(|e| format!("could not create storage path {home}: {e}"))?;

    // Install the stop handler BEFORE opening storage or announcing
    // readiness. It used to be installed after the "listening on" line, so a
    // SIGTERM sent the moment a harness saw that line hit the default
    // disposition `reset_stop_signals` had just restored and killed the
    // process outright -- no `stop`, no close-checkpoint (caught by CI's
    // `stops_on_sigterm_when_the_parent_left_it_ignored`, exit status 15).
    // A stop requested during startup is honoured once the server is up.
    let (tx, rx) = mpsc::channel::<()>();
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })?;

    let storage = secantus_pg::open_storage(&home)?;
    // The error as a sentence: an unusable certificate is the likeliest thing
    // to go wrong here, and the `Debug` form buries which file it was.
    let options = ServerOptions::default()
        .tls(tls)
        .trust_unknown_users(trust_unknown_users);
    let options = match hba_file {
        Some(path) => options.hba_file(path),
        None => options,
    };
    let options = match ident_file {
        Some(path) => options.ident_file(path),
        None => options,
    };
    let mut server = bind_with(&addr, storage, databases, &options).map_err(|e| e.to_string())?;

    // SIGHUP's reload handler goes in BEFORE the readiness line, for the
    // reason the stop handler does: until it is installed SIGHUP is still
    // `ctrlc`'s (its `termination` feature takes SIGHUP too), so a SIGHUP
    // sent the moment a harness saw the line stopped the server (CI's
    // `sighup_does_not_stop_a_server_with_nothing_to_reload`, on Linux).
    watch_for_reload(server.reloader());

    // One line, flushed, so a harness can wait for readiness. It reports the
    // address the listener actually BOUND, not the one requested, so that
    // `127.0.0.1:0` is usable: the kernel names the port and this line is how
    // the caller learns it. A harness that instead probes for a free port and
    // passes it in cannot be made race-free -- the probe socket must close
    // before the child binds.
    println!(
        "secantusd-pg listening on {} storage={home}",
        server.address()
    );

    // Block until SIGINT or SIGTERM, then stop cleanly so WiredTiger closes.
    // WITHOUT THIS the process dies with no checkpoint and every acknowledged
    // write since the last one is lost -- measured 2026-08-31: a SIGTERM after
    // CREATE TABLE + INSERT left the catalog document and the rows both gone,
    // while the client had been told the writes succeeded. `stop` drains the
    // connections and drops the last `Arc<Storage>`, which is where the
    // close-checkpoint runs. The handler was installed above, before startup.
    let _ = rx.recv();

    server.stop();

    // The instrumented stage of the release build only (the `pgo-instrument`
    // feature; the symbols exist only under `-Cprofile-generate`). On the
    // arm64 macOS release runner the profiling runtime does not write its
    // profile as the process exits, which the MongoDB binary's build
    // established, so the profile is written here, to the path in
    // `SECANTUS_PGO_OUT`.
    #[cfg(feature = "pgo-instrument")]
    if let Ok(path) = std::env::var("SECANTUS_PGO_OUT") {
        if let Ok(cpath) = std::ffi::CString::new(path) {
            extern "C" {
                fn __llvm_profile_set_filename(name: *const std::os::raw::c_char);
                fn __llvm_profile_write_file() -> std::os::raw::c_int;
            }
            unsafe {
                __llvm_profile_set_filename(cpath.as_ptr());
                __llvm_profile_write_file();
            }
        }
    }
    Ok(())
}
