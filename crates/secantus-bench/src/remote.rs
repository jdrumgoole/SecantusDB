//! `ssh` / `scp` plumbing, and the fan-out helper the two client droplets need.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::BenchResult;

/// Where the harness keeps its own `known_hosts` and scratch scripts.
pub fn state_dir() -> PathBuf {
    let base = std::env::var("SECANTUS_BENCH_STATE")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("bench/.do-state"));
    let _ = std::fs::create_dir_all(&base);
    base
}

pub fn known_hosts() -> PathBuf {
    let path = state_dir().join("known_hosts");
    if !path.exists() {
        let _ = std::fs::File::create(&path);
    }
    path
}

pub fn default_ssh_key() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    for name in ["id_ed25519", "id_rsa"] {
        let candidate = Path::new(&home).join(".ssh").join(name);
        if candidate.exists() && Path::new(&format!("{}.pub", candidate.display())).exists() {
            return candidate;
        }
    }
    Path::new(&home).join(".ssh").join("id_ed25519")
}

fn ssh_opts(key: &Path) -> Vec<String> {
    vec![
        "-i".into(),
        key.display().to_string(),
        "-o".into(),
        format!("UserKnownHostsFile={}", known_hosts().display()),
        // A benchmark droplet is cattle: its host key changes every time it is
        // rebuilt, and the known_hosts above is the harness's own, so
        // accept-new beats a prompt no script can answer.
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=15".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "LogLevel=ERROR".into(),
    ]
}

pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Forget a recycled IP's host key so `accept-new` can learn the new one.
pub fn forget_host(ip: &str) {
    let _ = Command::new("ssh-keygen")
        .args(["-q", "-R", ip, "-f", &known_hosts().display().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub fn ssh_raw(key: &Path, ip: &str, command: &str) -> BenchResult<Output> {
    let mut cmd = Command::new("ssh");
    cmd.args(ssh_opts(key));
    cmd.arg(format!("root@{ip}"));
    cmd.arg(command);
    let out = cmd
        .output()
        .map_err(|e| format!("spawning ssh failed: {e}"))?;
    Ok(Output {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    })
}

/// Run a command and fail loudly, quoting both streams.
///
/// A remote step that failed but was allowed to continue produces a benchmark
/// of something other than what was asked for, so the default is to stop.
pub fn ssh(key: &Path, ip: &str, command: &str, echo: bool) -> BenchResult<Output> {
    let out = ssh_raw(key, ip, command)?;
    if out.status != 0 {
        return Err(format!(
            "ssh root@{ip} failed ({}): {command}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.status, out.stdout, out.stderr
        ));
    }
    if echo {
        for line in out.stdout.trim().lines() {
            println!("  [{ip}] {line}");
        }
    }
    Ok(out)
}

pub fn scp_to(key: &Path, ip: &str, local: &Path, remote: &str) -> BenchResult<()> {
    let mut cmd = Command::new("scp");
    cmd.args(ssh_opts(key));
    cmd.arg(local.display().to_string());
    cmd.arg(format!("root@{ip}:{remote}"));
    let out = cmd
        .output()
        .map_err(|e| format!("spawning scp failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "scp {} -> {ip}:{remote} failed: {}",
            local.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

pub fn scp_from(key: &Path, ip: &str, remote: &str, local: &Path) -> BenchResult<()> {
    if let Some(parent) = local.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut cmd = Command::new("scp");
    cmd.args(ssh_opts(key));
    cmd.arg(format!("root@{ip}:{remote}"));
    cmd.arg(local.display().to_string());
    let out = cmd
        .output()
        .map_err(|e| format!("spawning scp failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "scp {ip}:{remote} -> {} failed: {}",
            local.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// Ship a script and run it with an explicit environment.
///
/// Values travel through the environment rather than being interpolated into
/// the script text, so a URL or a git ref containing shell metacharacters can
/// never become a command.
pub fn remote_script(
    key: &Path,
    ip: &str,
    script: &str,
    env: &[(&str, String)],
    echo: bool,
) -> BenchResult<Output> {
    let path = state_dir().join(format!("script-{ip}.sh"));
    let mut file =
        std::fs::File::create(&path).map_err(|e| format!("writing {}: {e}", path.display()))?;
    file.write_all(script.as_bytes())
        .map_err(|e| format!("writing script: {e}"))?;
    drop(file);
    scp_to(key, ip, &path, "/tmp/secantus-deploy.sh")?;
    let _ = std::fs::remove_file(&path);
    let exports: Vec<String> = env
        .iter()
        .map(|(k, v)| format!("{k}={}", shell_quote(v)))
        .collect();
    ssh(
        key,
        ip,
        &format!("{} bash /tmp/secantus-deploy.sh", exports.join(" ")),
        echo,
    )
}

/// POSIX single-quote quoting: everything inside is literal, and an embedded
/// quote is closed, escaped, and reopened.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Run `f` over `items` concurrently, preserving order.
///
/// The two client droplets must be driven simultaneously or the run measures
/// two sequential single-client tests, so every client-facing step goes
/// through here rather than a loop.
pub fn in_parallel<T, R, F>(items: &[T], f: F) -> Vec<BenchResult<R>>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> BenchResult<R> + Sync + Send,
{
    thread::scope(|scope| {
        let handles: Vec<_> = items.iter().map(|item| scope.spawn(|| f(item))).collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("worker thread panicked".to_string()))
            })
            .collect()
    })
}

/// Poll `f` until it returns true or `timeout` elapses.
pub fn wait_until<F: FnMut() -> bool>(mut f: F, timeout: Duration, interval: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if f() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(interval);
    }
}

/// Parse `ping -q` summary output: `rtt min/avg/max/mdev = 0.2/0.3/0.6/0.04 ms`.
pub fn parse_ping(stdout: &str) -> Option<crate::report::Rtt> {
    for line in stdout.lines() {
        if !line.contains("min/avg/max") {
            continue;
        }
        let tail = line.split('=').next_back()?.trim();
        let numbers = tail.split_whitespace().next()?;
        let parts: Vec<&str> = numbers.split('/').collect();
        if parts.len() < 4 {
            continue;
        }
        return Some(crate::report::Rtt {
            min_ms: parts[0].parse().ok()?,
            avg_ms: parts[1].parse().ok()?,
            max_ms: parts[2].parse().ok()?,
            mdev_ms: parts[3].parse().ok()?,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quoting_neutralises_metacharacters() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("a b;rm -rf /"), "'a b;rm -rf /'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn ping_summary_is_parsed() {
        let out = "20 packets transmitted, 20 received, 0% packet loss, time 3805ms\n\
                   rtt min/avg/max/mdev = 0.201/0.310/0.605/0.042 ms";
        let rtt = parse_ping(out).unwrap();
        assert_eq!(rtt.min_ms, 0.201);
        assert_eq!(rtt.avg_ms, 0.310);
        assert_eq!(rtt.mdev_ms, 0.042);
    }

    #[test]
    fn ping_without_a_summary_yields_none() {
        assert!(parse_ping("ping: connect: Network is unreachable").is_none());
    }

    #[test]
    fn in_parallel_preserves_order_and_reports_errors() {
        let items = vec![1u32, 2, 3];
        let out = in_parallel(&items, |n| {
            if *n == 2 {
                Err("no".into())
            } else {
                Ok(n * 10)
            }
        });
        assert_eq!(out[0].as_ref().unwrap(), &10);
        assert!(out[1].is_err());
        assert_eq!(out[2].as_ref().unwrap(), &30);
    }
}
