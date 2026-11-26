//! The logical replication launcher (RP-1096), as PostgreSQL 15's
//! `ApplyLauncherMain` (`launcher.c`) runs: a background process with no
//! database that heap-scans `pg_subscription` (`get_subscription_list`) as
//! it starts, waits on its latch for `DEFAULT_NAPTIME_PER_CYCLE` (180 s),
//! and scans again each time it wakes. Its latch is set as it starts, so it
//! scans twice at once; a reload (the postmaster's SIGHUP, which every
//! child gets) sets it again. A subscription's commit sets it only for an
//! enabled subscription (`ApplyLauncherWakeupAtCommit`), which this server
//! never has: `ENABLE` and `enabled = true` are refused (`subscriptions.rs`).
//!
//! It never reports its statistics while it runs (it calls no
//! `pgstat_report_stat`), so its scans reach the shared counts only as it
//! exits (`pgstat_shutdown_hook`), at a clean shutdown: across a restart
//! `pg_stat_get_numscans('pg_subscription')` moves by the run before's
//! scans. Here its counts are a backend's pending ones, under a pid no
//! client backend has, which the shutdown's `flush_all` folds in.
//!
//! No apply worker is ever started: an enabled subscription is refused, so
//! the list it scans holds none to start (and no worker connects to a
//! publisher here).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use secantus_storage::Storage;

use crate::{DatabaseRegistry, PgHandler};

/// `DEFAULT_NAPTIME_PER_CYCLE`: the longest the launcher sleeps.
const NAPTIME: Duration = Duration::from_secs(180);

/// The longest the server's start waits for the launcher's start scans.
const START_WAIT: Duration = Duration::from_secs(10);

/// How often a sleeping launcher looks at the stop flag.
const STOP_POLL: Duration = Duration::from_millis(50);

/// The launcher's pid in the statistics: no client backend's (pgwire's
/// are positive), so its pending counts are never another session's.
pub(crate) const LAUNCHER_PID: i32 = -2;

/// The launcher's latch (`MyLatch`): set by a reload, reset as it wakes.
#[derive(Debug)]
pub(crate) struct Latch {
    set: Mutex<bool>,
    cv: Condvar,
}

impl Default for Latch {
    /// Set: the launcher's start leaves its latch set, so it scans again
    /// at once.
    fn default() -> Self {
        Latch {
            set: Mutex::new(true),
            cv: Condvar::new(),
        }
    }
}

impl Latch {
    /// `SetLatch`.
    pub(crate) fn set(&self) {
        *self.set.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.cv.notify_all();
    }

    /// Wake a sleeper without setting the latch: it looks at the stop flag.
    fn nudge(&self) {
        self.cv.notify_all();
    }

    /// `WaitLatch(WL_LATCH_SET | WL_TIMEOUT)` then `ResetLatch`: back when
    /// the latch is set, `timeout` passes, or `stop` is raised.
    fn wait(&self, timeout: Duration, stop: &AtomicBool) {
        let deadline = Instant::now() + timeout;
        let mut set = self.set.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if *set || stop.load(Ordering::SeqCst) {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let nap = (deadline - now).min(STOP_POLL);
            set = self
                .cv
                .wait_timeout(set, nap)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *set = false;
    }
}

/// The running launcher, stopped before the statistics are written.
pub(crate) struct Launcher {
    latch: Arc<Latch>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Launcher {
    /// Stop it and wait: its scans are pending until it exits.
    pub(crate) fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.latch.nudge();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// `ApplyLauncherRegister` and its start: none when
/// `max_logical_replication_workers` is 0, as in PostgreSQL.
pub(crate) fn launch(
    storage: &Arc<Storage>,
    databases: Arc<DatabaseRegistry>,
    latch: Arc<Latch>,
    stop: Arc<AtomicBool>,
) -> Option<Launcher> {
    let workers = launcher_handler(storage, &databases)
        .setting_value("max_logical_replication_workers")
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(4);
    if workers == 0 {
        return None;
    }
    let weak = Arc::downgrade(storage);
    let (l, s) = (latch.clone(), stop.clone());
    let (started, up) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("secantus-pg-logical-launcher".into())
        .stack_size(crate::server::WORKER_STACK_BYTES)
        .spawn(move || main_loop(weak, databases, l, s, started))
        .ok()?;
    // Its start's two scans are made before a client is served: a
    // PostgreSQL launcher makes them in a few milliseconds, before a
    // client's first statement, where this one's first pays for loading
    // `initdb`'s catalog heaps (which the first backend would otherwise).
    // A reload sent while the start's latch was still set would be one
    // wake with it, and a scan short.
    let _ = up.recv_timeout(START_WAIT);
    Some(Launcher {
        latch,
        stop,
        thread: Some(thread),
    })
}

/// `ApplyLauncherMain`'s loop.
fn main_loop(
    storage: Weak<Storage>,
    databases: Arc<DatabaseRegistry>,
    latch: Arc<Latch>,
    stop: Arc<AtomicBool>,
    started: std::sync::mpsc::Sender<()>,
) {
    for wake in 0u64.. {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        {
            let Some(store) = storage.upgrade() else {
                return;
            };
            scan_subscriptions(&store, &databases);
        }
        if wake == 1 {
            let _ = started.send(());
        }
        latch.wait(NAPTIME, &stop);
    }
}

/// The launcher's session: no database, the server's configuration file
/// in force (read afresh at each wake, as its SIGHUP's
/// `ProcessConfigFile` does), and its own pid in the statistics.
fn launcher_handler(storage: &Arc<Storage>, databases: &Arc<DatabaseRegistry>) -> PgHandler {
    let handler = PgHandler::new(storage.clone(), databases.clone());
    handler
        .backend_pid
        .store(LAUNCHER_PID, std::sync::atomic::Ordering::Relaxed);
    let _ = handler.stat_db_oid.set(0);
    handler.start_config_layer();
    // With no database and no user it takes the file's settings alone:
    // `process_settings` runs only for a database's backend.
    for e in handler.file_settings_in_force() {
        let name = crate::canonical_setting(&e.name);
        let _ = handler.apply_setting_from(&name, &e.value, crate::GucSource::ConfigFile);
    }
    handler
}

/// `get_subscription_list`: a heap scan of `pg_subscription`, which returns
/// every row and reads every block.
fn scan_subscriptions(storage: &Arc<Storage>, databases: &Arc<DatabaseRegistry>) {
    let handler = launcher_handler(storage, databases);
    if !handler.tracking() {
        return;
    }
    let rows = handler.all_subscription_rows();
    handler
        .stats()
        .seq_scan(crate::subscriptions::PG_SUBSCRIPTION, rows);
    let mut be = handler.catcache.lock().unwrap_or_else(|e| e.into_inner());
    handler.catalog_index_pages(
        &mut be,
        vec![crate::catcache::PageScan::Seq(
            crate::subscriptions::PG_SUBSCRIPTION,
        )],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_latch_is_set_and_a_wait_resets_it() {
        let latch = Latch::default();
        let stop = AtomicBool::new(false);
        let t = Instant::now();
        latch.wait(Duration::from_secs(5), &stop);
        assert!(t.elapsed() < Duration::from_secs(1));
        let t = Instant::now();
        latch.wait(Duration::from_millis(120), &stop);
        assert!(t.elapsed() >= Duration::from_millis(120));
    }

    #[test]
    fn a_set_latch_wakes_a_sleeper() {
        let latch = Arc::new(Latch::default());
        let stop = Arc::new(AtomicBool::new(false));
        latch.wait(Duration::ZERO, &stop);
        let (l, s) = (latch.clone(), stop.clone());
        let t = Instant::now();
        let sleeper = std::thread::spawn(move || l.wait(Duration::from_secs(30), &s));
        std::thread::sleep(Duration::from_millis(100));
        latch.set();
        sleeper.join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_stop_ends_the_wait() {
        let latch = Arc::new(Latch::default());
        let stop = Arc::new(AtomicBool::new(false));
        latch.wait(Duration::ZERO, &stop);
        let (l, s) = (latch.clone(), stop.clone());
        let t = Instant::now();
        let sleeper = std::thread::spawn(move || l.wait(Duration::from_secs(30), &s));
        std::thread::sleep(Duration::from_millis(100));
        stop.store(true, Ordering::SeqCst);
        latch.nudge();
        sleeper.join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(5));
    }
}
