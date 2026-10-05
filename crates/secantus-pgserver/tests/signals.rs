//! The standalone binary stops on SIGTERM even when its parent left SIGTERM
//! blocked or ignored -- both survive exec, and a server a supervisor cannot
//! stop with SIGTERM is a defect (measured 2026-10-05: it ignored SIGTERM and
//! stopped only on SIGINT).
//!
//! POSIX only: Windows has no signal mask or inherited dispositions, so there
//! is nothing to reproduce there and the file compiles to nothing.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Start the binary with SIGTERM (and SIGINT) blocked or ignored in the
/// child before exec, send SIGTERM, and require a clean exit.
fn stops_on_sigterm(inherit: fn()) {
    let home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_secantusd-pg"));
    cmd.arg(home.path())
        .arg("127.0.0.1:0")
        .stdout(Stdio::piped());
    // SAFETY: `inherit` makes only async-signal-safe libc calls.
    unsafe {
        cmd.pre_exec(move || {
            inherit();
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.contains("listening on"), "did not start: {line:?}");

    // SAFETY: signalling our own child by pid.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "exited badly on SIGTERM: {status:?}");
            return;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("secantusd-pg did not stop on SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn stops_on_sigterm_when_the_parent_left_it_blocked() {
    stops_on_sigterm(|| unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    });
}

#[test]
fn stops_on_sigterm_when_the_parent_left_it_ignored() {
    stops_on_sigterm(|| unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    });
}
