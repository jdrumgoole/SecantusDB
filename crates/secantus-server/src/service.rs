//! `secantusd-mdb service install | uninstall | status`: run the server as a
//! per-user background service under the operating system's own service
//! manager, the way mongod's packages run mongod (a systemd unit on Linux,
//! the launchd plist `brew services` writes on macOS). The server itself
//! stays a foreground process; the service manager starts it, captures its
//! output and stops it with SIGTERM, which closes WiredTiger cleanly.
//!
//! Everything lives under `~/.secantus`:
//!
//! | path | what |
//! | --- | --- |
//! | `~/.secantus/bin/secantusd-mdb` | the binary the service runs, copied there by `install` |
//! | `~/.secantus/data` | the store, unless the install names another `--storage-path` / `--dbpath` |
//! | `~/.secantus/secantusd-mdb.log` | the server's output |
//! | `~/.secantus/secantusd.toml` | the config file the server already auto-discovers (optional) |
//!
//! The service file itself goes where the service manager looks:
//! `~/Library/LaunchAgents/com.secantusdb.secantusd-mdb.plist` (a launchd
//! LaunchAgent) or `~/.config/systemd/user/secantusd-mdb.service` (a systemd
//! user unit). `uninstall` removes the service file and the copied binary
//! and keeps the data, the config and the log.
//!
//! Like mongod's own unit and Homebrew's plist, the service does not restart
//! the server when it exits: a database that crashed stays down where its
//! log can be read, rather than being restarted over the cause.
//!
//! Lives here, WT-free, so the files it writes are unit-tested in the clean
//! workspace; the `secantusd-mdb` binary only dispatches to [`run`].

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::args::{parse_args, Parsed};

/// The launchd label, reverse-DNS of secantusdb.com.
pub const LAUNCHD_LABEL: &str = "com.secantusdb.secantusd-mdb";
/// The systemd user unit's name.
pub const SYSTEMD_UNIT: &str = "secantusd-mdb.service";
/// The open-file limit the service raises: mongod's packaged unit and
/// Homebrew's mongod plist both set 64000. launchd's own default is 256, too
/// few for a store with a table per collection and per index.
pub const OPEN_FILES: u32 = 64000;
/// Seconds the service manager waits for a clean stop before SIGKILL:
/// systemd's default. launchd's own default is 20, which a large final
/// checkpoint can outlast.
pub const STOP_TIMEOUT_SECS: u32 = 90;
/// How long `install` waits for the server to report it is listening.
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// The line the server prints once it accepts connections.
const LISTENING: &str = "secantusd-mdb listening on ";

/// The service flags that name a file or directory: made absolute at install
/// time, since the service runs in `~/.secantus`, not where `install` ran.
const PATH_FLAGS: [&str; 7] = [
    "--config",
    "--storage-path",
    "--dbpath",
    "--tls-cert-file",
    "--tls-key-file",
    "--tls-ca-file",
    "--oplog-archive-dir",
];

const HELP: &str = "\
Usage:
  secantusd-mdb service install [--dry-run] [SERVER OPTIONS...]
  secantusd-mdb service uninstall [--dry-run]
  secantusd-mdb service status

Run secantusd-mdb as a background service for this user, started at login,
under the operating system's service manager: a launchd LaunchAgent on macOS,
a systemd user unit on Linux.

  install    Copy this binary to ~/.secantus/bin, write the service file and
             start the service, then wait until the server is listening.
             SERVER OPTIONS are the flags the service runs the server with
             (see secantusd-mdb --help); without --storage-path or --dbpath
             the store is ~/.secantus/data. ~/.secantus/secantusd.toml, if
             present, is read as usual. Installing again replaces the service
             (a new binary, new options) and keeps the data.
  uninstall  Stop the service and remove the service file and the copied
             binary. The data, the config file and the log are kept.
  status     Report whether the service is installed and running, where it
             listens, and where its files are. Exits 0 when it is running and
             3 when it is not (the LSB status code).

  --dry-run  Print what install or uninstall would do, and do nothing.

The server's output goes to ~/.secantus/secantusd-mdb.log. The service does
not restart a server that exits: read the log, then run install again.
On Linux a user service stops at logout unless lingering is enabled:
  loginctl enable-linger $USER
";

/// The service manager a platform has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    /// macOS: a launchd LaunchAgent in the user's GUI domain.
    Launchd,
    /// Linux: a systemd user unit.
    Systemd,
}

impl Manager {
    /// The manager on this platform, or why there is none.
    pub fn current() -> Result<Self, String> {
        if cfg!(target_os = "macos") {
            Ok(Manager::Launchd)
        } else if cfg!(target_os = "linux") {
            Ok(Manager::Systemd)
        } else {
            Err(
                "secantusd-mdb service supports launchd (macOS) and systemd (Linux); \
                 on this platform run secantusd-mdb under your own service manager"
                    .to_string(),
            )
        }
    }

    fn name(self) -> &'static str {
        match self {
            Manager::Launchd => "launchd",
            Manager::Systemd => "systemd",
        }
    }
}

/// Where the service's files live, all derived from the user's home.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// `~/.secantus`: the service's working directory.
    pub root: PathBuf,
    /// The binary the service runs.
    pub binary: PathBuf,
    /// The default store.
    pub data: PathBuf,
    /// The server's output.
    pub log: PathBuf,
    /// The service file the manager reads.
    pub service_file: PathBuf,
}

impl Layout {
    /// The layout for `manager` under `home`. `xdg_config` is
    /// `$XDG_CONFIG_HOME`, which moves systemd's user unit directory.
    pub fn new(manager: Manager, home: &Path, xdg_config: Option<&Path>) -> Self {
        let root = home.join(".secantus");
        let service_file = match manager {
            Manager::Launchd => home
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{LAUNCHD_LABEL}.plist")),
            Manager::Systemd => xdg_config
                .map(Path::to_path_buf)
                .unwrap_or_else(|| home.join(".config"))
                .join("systemd")
                .join("user")
                .join(SYSTEMD_UNIT),
        };
        Layout {
            binary: root.join("bin").join("secantusd-mdb"),
            data: root.join("data"),
            log: root.join("secantusd-mdb.log"),
            root,
            service_file,
        }
    }
}

/// Escape `s` for a plist `<string>`.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The launchd LaunchAgent that runs `binary args...`.
pub fn launchd_plist(layout: &Layout, args: &[String]) -> String {
    let mut program = format!(
        "        <string>{}</string>\n",
        xml_escape(&layout.binary.to_string_lossy())
    );
    for a in args {
        program.push_str(&format!("        <string>{}</string>\n", xml_escape(a)));
    }
    let root = xml_escape(&layout.root.to_string_lossy());
    let log = xml_escape(&layout.log.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `secantusd-mdb service install`; remove with `secantusd-mdb service uninstall`. -->
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{program}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
    <key>WorkingDirectory</key>
    <string>{root}</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>ExitTimeOut</key>
    <integer>{STOP_TIMEOUT_SECS}</integer>
    <key>HardResourceLimits</key>
    <dict>
        <key>NumberOfFiles</key>
        <integer>{OPEN_FILES}</integer>
    </dict>
    <key>SoftResourceLimits</key>
    <dict>
        <key>NumberOfFiles</key>
        <integer>{OPEN_FILES}</integer>
    </dict>
</dict>
</plist>
"#
    )
}

/// Escape `s` for a systemd unit value: `%` starts a specifier and `$` an
/// environment substitution, so both are doubled.
fn systemd_value(s: &str) -> String {
    s.replace('%', "%%").replace('$', "$$")
}

/// Quote one `ExecStart=` word when systemd would otherwise split or
/// unescape it.
fn systemd_word(s: &str) -> String {
    let v = systemd_value(s);
    if !v.is_empty()
        && !v
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | ';'))
    {
        return v;
    }
    let mut out = String::from("\"");
    for c in v.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The systemd user unit that runs `binary args...`.
pub fn systemd_unit(layout: &Layout, args: &[String]) -> String {
    let mut exec = systemd_word(&layout.binary.to_string_lossy());
    for a in args {
        exec.push(' ');
        exec.push_str(&systemd_word(a));
    }
    let root = systemd_value(&layout.root.to_string_lossy());
    let log = systemd_value(&layout.log.to_string_lossy());
    format!(
        "\
# Written by `secantusd-mdb service install`; remove with `secantusd-mdb service uninstall`.
[Unit]
Description=SecantusDB MongoDB-compatible server (secantusd-mdb)
Documentation=https://secantusdb.com/docs/rust/

[Service]
Type=simple
ExecStart={exec}
WorkingDirectory={root}
StandardOutput=append:{log}
StandardError=append:{log}
KillSignal=SIGTERM
TimeoutStopSec={STOP_TIMEOUT_SECS}
LimitNOFILE={OPEN_FILES}

[Install]
WantedBy=default.target
"
    )
}

/// What `service` was asked to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// `install [--dry-run] [SERVER OPTIONS...]`.
    Install { args: Vec<String>, dry_run: bool },
    /// `uninstall [--dry-run]`.
    Uninstall { dry_run: bool },
    /// `status`.
    Status,
    /// `--help`, or no action.
    Help,
}

/// Parse the words after `service`.
pub fn parse_action(words: &[String]) -> Result<Action, String> {
    let Some((verb, rest)) = words.split_first() else {
        return Ok(Action::Help);
    };
    let dry_run = rest.iter().any(|w| w == "--dry-run");
    let rest: Vec<String> = rest.iter().filter(|w| *w != "--dry-run").cloned().collect();
    match verb.as_str() {
        "--help" | "-h" | "help" => Ok(Action::Help),
        "install" => {
            match parse_args(&rest)? {
                Parsed::Run(run) if run.repair => {
                    return Err(
                        "service install runs a server; --repair is not a server option"
                            .to_string(),
                    )
                }
                Parsed::Run(_) => {}
                Parsed::Help(_) | Parsed::Version(_) => {
                    return Err(
                        "service install takes the server's options (see secantusd-mdb --help)"
                            .to_string(),
                    )
                }
            }
            Ok(Action::Install {
                args: rest,
                dry_run,
            })
        }
        "uninstall" | "status" if !rest.is_empty() => Err(format!(
            "service {verb} takes no options but --dry-run, got {rest:?}"
        )),
        "uninstall" => Ok(Action::Uninstall { dry_run }),
        "status" if dry_run => Err("service status has no --dry-run".to_string()),
        "status" => Ok(Action::Status),
        other => Err(format!(
            "unknown service action {other:?}: expected install, uninstall or status"
        )),
    }
}

/// The server options the service runs with: `args` with every path made
/// absolute against `cwd`, and the default store added when none is named.
pub fn service_args(args: &[String], cwd: &Path, layout: &Layout) -> Vec<String> {
    let absolute = |v: &str| -> String {
        let p = Path::new(v);
        if p.is_absolute() {
            v.to_string()
        } else {
            cwd.join(p).to_string_lossy().into_owned()
        }
    };
    let mut out = Vec::with_capacity(args.len() + 2);
    let mut store_named = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v)),
            _ => (arg.as_str(), None),
        };
        if matches!(flag, "--storage-path" | "--dbpath") {
            store_named = true;
        }
        if PATH_FLAGS.contains(&flag) {
            match inline {
                Some(v) => out.push(format!("{flag}={}", absolute(v))),
                None => {
                    out.push(arg.clone());
                    if let Some(v) = args.get(i + 1) {
                        out.push(absolute(v));
                        i += 1;
                    }
                }
            }
        } else {
            out.push(arg.clone());
        }
        i += 1;
    }
    if !store_named {
        out.push("--storage-path".to_string());
        out.push(layout.data.to_string_lossy().into_owned());
    }
    out
}

/// One step of an install or uninstall, printed by `--dry-run` and run
/// otherwise.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Create a directory and its parents.
    MakeDir(PathBuf),
    /// Copy the running binary to the service's binary path.
    CopyBinary { from: PathBuf, to: PathBuf },
    /// Write a file.
    Write { path: PathBuf, contents: String },
    /// Remove a file, if it exists.
    Remove(PathBuf),
    /// Run a command; `may_fail` when failing only means there was nothing
    /// to stop.
    Run { argv: Vec<String>, may_fail: bool },
}

impl std::fmt::Display for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Step::MakeDir(p) => write!(f, "mkdir -p {}", p.display()),
            Step::CopyBinary { from, to } => write!(f, "cp {} {}", from.display(), to.display()),
            Step::Write { path, contents } => {
                write!(f, "write {}:\n{}", path.display(), contents)
            }
            Step::Remove(p) => write!(f, "rm -f {}", p.display()),
            Step::Run { argv, .. } => write!(f, "{}", argv.join(" ")),
        }
    }
}

fn run_argv(argv: &[&str], may_fail: bool) -> Step {
    Step::Run {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        may_fail,
    }
}

/// launchd's domain for this user's services.
fn launchd_domain(uid: &str) -> String {
    format!("gui/{uid}")
}

/// The steps that install the service.
pub fn install_steps(
    manager: Manager,
    layout: &Layout,
    exe: &Path,
    args: &[String],
    uid: &str,
) -> Vec<Step> {
    let mut steps = vec![
        Step::MakeDir(layout.root.clone()),
        Step::MakeDir(layout.binary.parent().expect("bin dir").to_path_buf()),
    ];
    if args
        .windows(2)
        .any(|w| w[0] == "--storage-path" && Path::new(&w[1]) == layout.data)
    {
        steps.push(Step::MakeDir(layout.data.clone()));
    }
    if exe != layout.binary {
        steps.push(Step::CopyBinary {
            from: exe.to_path_buf(),
            to: layout.binary.clone(),
        });
    }
    steps.push(Step::MakeDir(
        layout
            .service_file
            .parent()
            .expect("service dir")
            .to_path_buf(),
    ));
    match manager {
        Manager::Launchd => {
            let domain = launchd_domain(uid);
            let target = format!("{domain}/{LAUNCHD_LABEL}");
            let plist = layout.service_file.to_string_lossy().into_owned();
            // An installed service is stopped first: install replaces it.
            steps.push(run_argv(&["launchctl", "bootout", &target], true));
            steps.push(Step::Write {
                path: layout.service_file.clone(),
                contents: launchd_plist(layout, args),
            });
            steps.push(run_argv(
                &["launchctl", "bootstrap", &domain, &plist],
                false,
            ));
        }
        Manager::Systemd => {
            steps.push(Step::Write {
                path: layout.service_file.clone(),
                contents: systemd_unit(layout, args),
            });
            steps.push(run_argv(&["systemctl", "--user", "daemon-reload"], false));
            steps.push(run_argv(
                &["systemctl", "--user", "enable", SYSTEMD_UNIT],
                false,
            ));
            // restart, not start: install replaces a running service.
            steps.push(run_argv(
                &["systemctl", "--user", "restart", SYSTEMD_UNIT],
                false,
            ));
        }
    }
    steps
}

/// The steps that remove the service, keeping data, config and log.
pub fn uninstall_steps(manager: Manager, layout: &Layout, uid: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    match manager {
        Manager::Launchd => {
            let target = format!("{}/{LAUNCHD_LABEL}", launchd_domain(uid));
            steps.push(run_argv(&["launchctl", "bootout", &target], true));
            steps.push(Step::Remove(layout.service_file.clone()));
        }
        Manager::Systemd => {
            steps.push(run_argv(
                &["systemctl", "--user", "disable", "--now", SYSTEMD_UNIT],
                true,
            ));
            steps.push(Step::Remove(layout.service_file.clone()));
            steps.push(run_argv(&["systemctl", "--user", "daemon-reload"], false));
        }
    }
    steps.push(Step::Remove(layout.binary.clone()));
    steps
}

/// Run `secantusd-mdb service <words>`; returns the process exit code. A bad
/// command line is reported here and exits 2, as the server's own options
/// do; an `Err` is an operation that failed (exit 1).
pub fn run(words: &[String]) -> Result<u8, String> {
    let action = match parse_action(words) {
        Ok(action) => action,
        Err(msg) => {
            eprintln!("secantusd-mdb service: {msg}");
            eprintln!("Try 'secantusd-mdb service --help' for usage.");
            return Ok(2);
        }
    };
    if action == Action::Help {
        print!("{HELP}");
        return Ok(0);
    }
    let manager = Manager::current()?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set: the service lives under ~/.secantus")?;
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let layout = Layout::new(manager, &home, xdg.as_deref());
    match action {
        Action::Install { args, dry_run } => install(manager, &layout, &args, dry_run),
        Action::Uninstall { dry_run } => uninstall(manager, &layout, dry_run),
        Action::Status => status(manager, &layout),
        Action::Help => unreachable!("handled above"),
    }
}

fn current_uid() -> Result<String, String> {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| format!("running `id -u`: {e}"))?;
    if !out.status.success() {
        return Err("`id -u` failed".to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn execute(step: &Step) -> Result<(), String> {
    match step {
        Step::MakeDir(p) => {
            std::fs::create_dir_all(p).map_err(|e| format!("creating {}: {e}", p.display()))
        }
        Step::CopyBinary { from, to } => {
            // Copy beside the target and rename over it: a new inode, so a
            // running copy of the old binary is not overwritten in place.
            let tmp = to.with_extension("new");
            std::fs::copy(from, &tmp)
                .map_err(|e| format!("copying {} to {}: {e}", from.display(), tmp.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| format!("making {} executable: {e}", tmp.display()))?;
            }
            std::fs::rename(&tmp, to)
                .map_err(|e| format!("moving {} to {}: {e}", tmp.display(), to.display()))
        }
        Step::Write { path, contents } => {
            std::fs::write(path, contents).map_err(|e| format!("writing {}: {e}", path.display()))
        }
        Step::Remove(p) => match std::fs::remove_file(p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("removing {}: {e}", p.display())),
        },
        Step::Run { argv, may_fail } => {
            let out = Command::new(&argv[0])
                .args(&argv[1..])
                .output()
                .map_err(|e| format!("running {}: {e}", argv[0]))?;
            if out.status.success() || *may_fail {
                Ok(())
            } else {
                Err(format!(
                    "`{}` failed ({}): {}",
                    argv.join(" "),
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        }
    }
}

fn install(
    manager: Manager,
    layout: &Layout,
    args: &[String],
    dry_run: bool,
) -> Result<u8, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("reading the current directory: {e}"))?;
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("finding this binary: {e}"))?;
    let args = service_args(args, &cwd, layout);
    let uid = current_uid()?;
    let steps = install_steps(manager, layout, &exe, &args, &uid);
    if dry_run {
        for step in &steps {
            println!("{step}");
        }
        return Ok(0);
    }
    // Where the log ends now: the new server's lines start after it.
    let log_start = std::fs::metadata(&layout.log).map(|m| m.len()).unwrap_or(0);
    for step in &steps {
        execute(step)?;
    }
    match wait_listening(layout, log_start, READY_TIMEOUT) {
        Some(address) => {
            println!(
                "secantusd-mdb is running as a {} service, listening on {address}",
                manager.name()
            );
            println!("  service file: {}", layout.service_file.display());
            println!("  binary:       {}", layout.binary.display());
            println!("  log:          {}", layout.log.display());
            println!("Check it with `secantusd-mdb service status`; remove it with `secantusd-mdb service uninstall`.");
            if manager == Manager::Systemd {
                println!("A user service stops at logout unless lingering is on: loginctl enable-linger $USER");
            }
            Ok(0)
        }
        None => Err(format!(
            "the service did not report it was listening within {}s; the end of {}:\n{}",
            READY_TIMEOUT.as_secs(),
            layout.log.display(),
            log_tail(&layout.log, log_start)
        )),
    }
}

/// Wait for the server to print its listening line past `from` in the log,
/// and return the address it names.
fn wait_listening(layout: &Layout, from: u64, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(address) = listening_address(&read_from(&layout.log, from)) {
            return Some(address);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The address in the last listening line of `log`.
pub fn listening_address(log: &str) -> Option<String> {
    log.lines()
        .rev()
        .find_map(|l| l.strip_prefix(LISTENING).map(|a| a.trim().to_string()))
}

fn read_from(path: &Path, from: u64) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    // A log shorter than before was replaced: read it whole.
    let start = if len < from { 0 } else { from };
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(start)).is_ok() {
        let _ = f.read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn log_tail(path: &Path, from: u64) -> String {
    let text = read_from(path, from);
    let lines: Vec<&str> = text.lines().collect();
    let tail = &lines[lines.len().saturating_sub(20)..];
    if tail.is_empty() {
        "(nothing)".to_string()
    } else {
        tail.join("\n")
    }
}

fn uninstall(manager: Manager, layout: &Layout, dry_run: bool) -> Result<u8, String> {
    let uid = current_uid()?;
    let steps = uninstall_steps(manager, layout, &uid);
    if dry_run {
        for step in &steps {
            println!("{step}");
        }
        return Ok(0);
    }
    let installed = layout.service_file.exists();
    for step in &steps {
        execute(step)?;
    }
    if installed {
        println!("secantusd-mdb service removed.");
    } else {
        println!("secantusd-mdb service was not installed.");
    }
    println!(
        "Kept: {} (the data, any secantusd.toml and the log).",
        layout.root.display()
    );
    Ok(0)
}

/// What the service manager says about the service.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    /// Running now.
    pub running: bool,
    /// Its process id, when running.
    pub pid: Option<u32>,
}

/// Read `launchctl print`'s output.
pub fn parse_launchd_state(out: &str) -> State {
    let mut running = false;
    let mut pid = None;
    for line in out.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("state = ") {
            running = v.trim() == "running";
        } else if let Some(v) = line.strip_prefix("pid = ") {
            pid = v.trim().parse().ok();
        }
    }
    State {
        running,
        pid: pid.filter(|_| running),
    }
}

/// Read `systemctl --user show -p ActiveState -p MainPID`'s output.
pub fn parse_systemd_state(out: &str) -> State {
    let mut running = false;
    let mut pid = None;
    for line in out.lines() {
        if let Some(v) = line.strip_prefix("ActiveState=") {
            running = v.trim() == "active";
        } else if let Some(v) = line.strip_prefix("MainPID=") {
            pid = v.trim().parse::<u32>().ok().filter(|p| *p != 0);
        }
    }
    State {
        running,
        pid: pid.filter(|_| running),
    }
}

fn query_state(manager: Manager) -> Result<State, String> {
    let out = match manager {
        Manager::Launchd => {
            let target = format!("{}/{LAUNCHD_LABEL}", launchd_domain(&current_uid()?));
            Command::new("launchctl").args(["print", &target]).output()
        }
        Manager::Systemd => Command::new("systemctl")
            .args([
                "--user",
                "show",
                "-p",
                "ActiveState",
                "-p",
                "MainPID",
                SYSTEMD_UNIT,
            ])
            .output(),
    }
    .map_err(|e| format!("asking {}: {e}", manager.name()))?;
    if !out.status.success() {
        // launchctl print fails for a service that is not loaded.
        return Ok(State {
            running: false,
            pid: None,
        });
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(match manager {
        Manager::Launchd => parse_launchd_state(&text),
        Manager::Systemd => parse_systemd_state(&text),
    })
}

fn status(manager: Manager, layout: &Layout) -> Result<u8, String> {
    if !layout.service_file.exists() {
        println!("secantusd-mdb service: not installed");
        println!("Install it with `secantusd-mdb service install [SERVER OPTIONS...]`.");
        return Ok(3);
    }
    let state = query_state(manager)?;
    let address = listening_address(&read_from(&layout.log, 0));
    if state.running {
        let pid = state.pid.map(|p| format!(", pid {p}")).unwrap_or_default();
        println!("secantusd-mdb service: running ({}{pid})", manager.name());
        if let Some(a) = address {
            println!("  listening on: {a}");
        }
    } else {
        println!(
            "secantusd-mdb service: installed, not running ({})",
            manager.name()
        );
    }
    println!("  service file: {}", layout.service_file.display());
    println!("  binary:       {}", layout.binary.display());
    println!("  log:          {}", layout.log.display());
    Ok(if state.running { 0 } else { 3 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(ws: &[&str]) -> Vec<String> {
        ws.iter().map(|s| s.to_string()).collect()
    }

    fn mac() -> Layout {
        Layout::new(Manager::Launchd, Path::new("/Users/ada"), None)
    }

    fn linux() -> Layout {
        Layout::new(Manager::Systemd, Path::new("/home/ada"), None)
    }

    #[test]
    fn the_layout_lives_under_dot_secantus() {
        let l = mac();
        assert_eq!(l.root, Path::new("/Users/ada/.secantus"));
        assert_eq!(
            l.binary,
            Path::new("/Users/ada/.secantus/bin/secantusd-mdb")
        );
        assert_eq!(l.data, Path::new("/Users/ada/.secantus/data"));
        assert_eq!(l.log, Path::new("/Users/ada/.secantus/secantusd-mdb.log"));
        assert_eq!(
            l.service_file,
            Path::new("/Users/ada/Library/LaunchAgents/com.secantusdb.secantusd-mdb.plist")
        );
        assert_eq!(
            linux().service_file,
            Path::new("/home/ada/.config/systemd/user/secantusd-mdb.service")
        );
        let xdg = Layout::new(
            Manager::Systemd,
            Path::new("/home/ada"),
            Some(Path::new("/cfg")),
        );
        assert_eq!(
            xdg.service_file,
            Path::new("/cfg/systemd/user/secantusd-mdb.service")
        );
    }

    #[test]
    fn parse_actions() {
        assert_eq!(parse_action(&[]).unwrap(), Action::Help);
        assert_eq!(parse_action(&words(&["--help"])).unwrap(), Action::Help);
        assert_eq!(
            parse_action(&words(&["install", "--port", "27018", "--dry-run"])).unwrap(),
            Action::Install {
                args: words(&["--port", "27018"]),
                dry_run: true
            }
        );
        assert_eq!(
            parse_action(&words(&["uninstall"])).unwrap(),
            Action::Uninstall { dry_run: false }
        );
        assert_eq!(parse_action(&words(&["status"])).unwrap(), Action::Status);
        // The server's own validation applies to install's options.
        assert!(parse_action(&words(&["install", "--port", "x"])).is_err());
        assert!(parse_action(&words(&["install", "--bogus"])).is_err());
        assert!(parse_action(&words(&["install", "--repair"])).is_err());
        assert!(parse_action(&words(&["install", "--help"])).is_err());
        assert!(parse_action(&words(&["status", "--port", "1"])).is_err());
        assert!(parse_action(&words(&["status", "--dry-run"])).is_err());
        assert!(parse_action(&words(&["start"])).is_err());
    }

    #[test]
    fn service_args_default_the_store_and_make_paths_absolute() {
        let l = mac();
        let cwd = Path::new("/work");
        assert_eq!(
            service_args(&words(&["--port", "27018"]), cwd, &l),
            words(&[
                "--port",
                "27018",
                "--storage-path",
                "/Users/ada/.secantus/data"
            ])
        );
        assert_eq!(
            service_args(&words(&["--dbpath", "db", "--config=conf/s.toml"]), cwd, &l),
            words(&["--dbpath", "/work/db", "--config=/work/conf/s.toml"])
        );
        assert_eq!(
            service_args(&words(&["--storage-path=/abs/db"]), cwd, &l),
            words(&["--storage-path=/abs/db"])
        );
    }

    #[test]
    fn the_plist_runs_the_copied_binary_with_mongods_limits() {
        let l = mac();
        let p = launchd_plist(&l, &words(&["--port", "27018", "--storage-path", "/a&b"]));
        assert!(p.contains("<string>com.secantusdb.secantusd-mdb</string>"));
        assert!(p.contains(
            "<string>/Users/ada/.secantus/bin/secantusd-mdb</string>\n        <string>--port</string>\n        <string>27018</string>"
        ));
        assert!(p.contains("<string>/a&amp;b</string>"));
        assert!(p.contains("<key>KeepAlive</key>\n    <false/>"));
        assert!(p.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(p.contains("<key>ExitTimeOut</key>\n    <integer>90</integer>"));
        assert_eq!(p.matches("<integer>64000</integer>").count(), 2);
        assert_eq!(
            p.matches("<string>/Users/ada/.secantus/secantusd-mdb.log</string>")
                .count(),
            2
        );
    }

    #[test]
    fn the_unit_quotes_what_systemd_would_split() {
        let l = Layout::new(Manager::Systemd, Path::new("/home/a da"), None);
        let u = systemd_unit(
            &l,
            &words(&["--port", "27018", "--storage-path", "/x/100%"]),
        );
        assert!(u.contains(
            "ExecStart=\"/home/a da/.secantus/bin/secantusd-mdb\" --port 27018 --storage-path /x/100%%\n"
        ));
        assert!(u.contains("StandardOutput=append:/home/a da/.secantus/secantusd-mdb.log\n"));
        assert!(u.contains("LimitNOFILE=64000\n"));
        assert!(u.contains("TimeoutStopSec=90\n"));
        assert!(u.contains("WantedBy=default.target\n"));
        assert!(!u.contains("Restart="));
        assert_eq!(systemd_word("a\"b"), "\"a\\\"b\"");
        assert_eq!(systemd_word("$HOME"), "$$HOME");
    }

    #[test]
    fn install_replaces_and_uninstall_keeps_the_data() {
        let l = mac();
        let args = service_args(&[], Path::new("/w"), &l);
        let steps = install_steps(
            Manager::Launchd,
            &l,
            Path::new("/tmp/secantusd-mdb"),
            &args,
            "501",
        );
        assert!(steps.contains(&Step::MakeDir(l.data.clone())));
        assert!(steps.contains(&Step::CopyBinary {
            from: PathBuf::from("/tmp/secantusd-mdb"),
            to: l.binary.clone()
        }));
        let runs: Vec<String> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Run { argv, .. } => Some(argv.join(" ")),
                _ => None,
            })
            .collect();
        assert_eq!(
            runs,
            [
                "launchctl bootout gui/501/com.secantusdb.secantusd-mdb",
                "launchctl bootstrap gui/501 /Users/ada/Library/LaunchAgents/com.secantusdb.secantusd-mdb.plist",
            ]
        );
        // Installing from the installed binary copies nothing.
        let again = install_steps(Manager::Launchd, &l, &l.binary.clone(), &args, "501");
        assert!(!again.iter().any(|s| matches!(s, Step::CopyBinary { .. })));

        let lx = linux();
        let un = uninstall_steps(Manager::Systemd, &lx, "1000");
        assert!(un.contains(&Step::Remove(lx.service_file.clone())));
        assert!(un.contains(&Step::Remove(lx.binary.clone())));
        assert!(!un
            .iter()
            .any(|s| matches!(s, Step::Remove(p) if p == &lx.data || p == &lx.log)));
    }

    #[test]
    fn states_and_the_listening_line() {
        let s = parse_launchd_state("\tstate = running\n\tpid = 4242\n");
        assert_eq!(
            s,
            State {
                running: true,
                pid: Some(4242)
            }
        );
        let s = parse_launchd_state("\tstate = not running\n");
        assert_eq!(
            s,
            State {
                running: false,
                pid: None
            }
        );
        let s = parse_systemd_state("MainPID=77\nActiveState=active\n");
        assert_eq!(
            s,
            State {
                running: true,
                pid: Some(77)
            }
        );
        let s = parse_systemd_state("MainPID=0\nActiveState=inactive\n");
        assert_eq!(
            s,
            State {
                running: false,
                pid: None
            }
        );
        assert_eq!(
            listening_address("x\nsecantusd-mdb listening on 127.0.0.1:1\ny\nsecantusd-mdb listening on 127.0.0.1:2\n"),
            Some("127.0.0.1:2".to_string())
        );
        assert_eq!(listening_address("nothing"), None);
    }
}
