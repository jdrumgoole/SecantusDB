//! `autoCompact`: WiredTiger's background compaction server, as WiredTiger
//! 11.3 runs it for mongod 8.x, over the vendored WiredTiger 11.2 that has
//! no such server.
//!
//! mongod's `autoCompact` only signals WiredTiger (`WT_SESSION::compact`
//! with `background=true`); the work is WiredTiger's `compact-server` thread
//! (11.3's `src/conn/conn_compact.c`), modelled here line for line where
//! 11.2 allows:
//!
//! * The server walks every `file:` in WiredTiger's metadata in key order,
//!   one file per step, and after a full pass waits 10 seconds (or a
//!   signal). It skips a file in the exclude list (mongod excludes the
//!   oplog: here its shard tables), a file of 1 MB or less, and a file it
//!   compacted within the last hour whose last pass gave nothing back or
//!   rewrote less than the moving average -- and with `runOnce` it compacts
//!   every file once and stops.
//! * A file is compacted as a foreground `compact` with `free_space_target`
//!   ([`super::compact::compact_table`]): 11.3's background compaction runs
//!   the same per-file compaction, without the leading checkpoint.
//! * It pauses while the cache is near its eviction triggers (dirty above
//!   20% of the cache, or in use above 95%), as 11.3 throttles itself.
//! * Enabling it while it runs with a different configuration is refused
//!   (11.3's `WT_BACKGROUND_COMPACT_ALREADY_RUNNING`, which mongod reports as
//!   23 AlreadyInitialized); enabling it again as it is, or disabling it
//!   when it is off, does nothing.
//! * Its counters are 11.3's `background-compact` statistics, which mongod
//!   reports in `serverStatus.wiredTiger`.
//!
//! Where 11.2 cannot follow: a file's compaction cannot be interrupted, so
//! disabling the server lets the file in hand finish (11.3 interrupts it and
//! counts `background compact interrupted`, never counted here), and 11.2
//! does not count the bytes a compaction rewrote, so the moving average
//! that decides the skip is of the bytes each pass gave back.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use secantus_wt::{Connection, Session, WtError};

use crate::compact::{compact_table, megabytes, DEFAULT_FREE_SPACE_TARGET_MB};
use crate::{Result, StorageError};

/// WiredTiger 11.3's `WT_BACKGROUND_COMPACT_WAIT_TIME`: the pause after a
/// full pass over the files.
const FULL_ITERATION_WAIT: Duration = Duration::from_secs(10);
/// `WT_BACKGROUND_COMPACT_MAX_SKIP_TIME`: a file skipped for poor results is
/// tried again after this long.
const MAX_FILE_SKIP: Duration = Duration::from_secs(60 * 60);
/// `WT_BACKGROUND_COMPACT_MAX_IDLE_TIME`: a file's record is forgotten once
/// it has not been compacted for this long.
const MAX_FILE_IDLE: Duration = Duration::from_secs(24 * 60 * 60);
/// Files of this size or less are never compacted (`WT_MEGABYTE`).
const SMALL_FILE: u64 = 1 << 20;

/// The `serverStatus.wiredTiger["background-compact"]` counters, in
/// WiredTiger 11.3's order.
pub const STATISTICS: [&str; 12] = [
    "background-compact: background compact failed calls",
    "background-compact: background compact failed calls due to cache pressure",
    "background-compact: background compact interrupted",
    "background-compact: background compact moving average of bytes rewritten",
    "background-compact: background compact recovered bytes",
    "background-compact: background compact running",
    "background-compact: background compact skipped file as it is part of the exclude list",
    "background-compact: background compact skipped file as not meeting requirements for compaction",
    "background-compact: background compact sleeps due to cache pressure",
    "background-compact: background compact successful calls",
    "background-compact: background compact timeout",
    "background-compact: number of files tracked by background compaction",
];

#[derive(Default)]
struct Counters {
    failed: AtomicI64,
    failed_cache_pressure: AtomicI64,
    interrupted: AtomicI64,
    ema: AtomicI64,
    recovered: AtomicI64,
    running: AtomicI64,
    excluded: AtomicI64,
    skipped: AtomicI64,
    sleeps_cache_pressure: AtomicI64,
    succeeded: AtomicI64,
    timeout: AtomicI64,
    tracked: AtomicI64,
}

/// What the server knows of a file it has compacted (11.3's
/// `WT_BACKGROUND_COMPACT_STAT`).
struct FileRecord {
    prev_compact_time: Instant,
    prev_compact_success: bool,
    bytes_rewritten: u64,
}

/// The server's configuration and the request the next pass picks up.
#[derive(Default)]
struct State {
    /// Whether compaction is on (11.3's `background_compact.running`).
    running: bool,
    /// The configuration it runs with, with the on/off toggle stripped: what
    /// a second enable is compared against.
    config: Option<Config>,
    /// A change the server has not yet acted on.
    signalled: bool,
    /// The store is closing.
    stop: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Config {
    free_space_target: u64,
    run_once: bool,
}

/// How `autoCompact` ended, when it did not fail in the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoCompactOutcome {
    /// Turned on or off, or already as asked.
    Done,
    /// On already, with another configuration (mongod: 23 AlreadyInitialized).
    AlreadyRunning,
    /// A store held in memory (mongod: 20 IllegalOperation, "autoCompact()
    /// cannot be executed for in-memory configurations").
    InMemory,
    /// The server has not yet taken up the previous change (11.3's `EBUSY`,
    /// "Background compact is busy processing a previous command", which
    /// mongod reports as 314 ObjectIsBusy). Carries the platform's text for
    /// `EBUSY`, which mongod's message quotes.
    Busy(String),
}

pub(crate) struct AutoCompactor {
    conn: Arc<Connection>,
    home: String,
    state: Arc<(Mutex<State>, Condvar)>,
    counters: Arc<Counters>,
    /// Held while a file compacts (the store's checkpoint gate).
    gate: Arc<Mutex<()>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl AutoCompactor {
    pub(crate) fn new(conn: Arc<Connection>, home: String, gate: Arc<Mutex<()>>) -> Self {
        AutoCompactor {
            conn,
            home,
            state: Arc::new((Mutex::new(State::default()), Condvar::new())),
            counters: Arc::new(Counters::default()),
            gate,
            thread: Mutex::new(None),
        }
    }

    /// `autoCompact: enable`, with `runOnce` and `freeSpaceTargetMB`.
    pub(crate) fn signal(
        &self,
        enable: bool,
        run_once: bool,
        free_space_target_mb: Option<i64>,
    ) -> Result<AutoCompactOutcome> {
        let (lock, cv) = &*self.state;
        let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
        // 11.3 waits for nothing: a change the server has not acted on yet
        // refuses the next one, whatever it asks.
        if st.signalled {
            let text = std::io::Error::from_raw_os_error(16).to_string();
            let text = text.split(" (os error").next().unwrap_or(&text).to_string();
            return Ok(AutoCompactOutcome::Busy(text));
        }
        let config = Config {
            free_space_target: megabytes(
                free_space_target_mb.unwrap_or(DEFAULT_FREE_SPACE_TARGET_MB),
            ),
            run_once,
        };
        if enable && st.running && st.config.as_ref() != Some(&config) {
            return Ok(AutoCompactOutcome::AlreadyRunning);
        }
        if enable == st.running {
            return Ok(AutoCompactOutcome::Done);
        }
        if enable {
            st.config = Some(config);
        }
        st.running = enable;
        st.signalled = true;
        drop(st);
        if enable {
            self.ensure_thread()?;
        }
        cv.notify_all();
        Ok(AutoCompactOutcome::Done)
    }

    fn ensure_thread(&self) -> Result<()> {
        let mut slot = self.thread.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return Ok(());
        }
        let server = Server {
            conn: self.conn.clone(),
            home: self.home.clone(),
            state: self.state.clone(),
            counters: self.counters.clone(),
            gate: self.gate.clone(),
            files: HashMap::new(),
            ema: 0,
        };
        let handle = std::thread::Builder::new()
            .name("secantus-compact-server".into())
            .spawn(move || server.run())
            .map_err(|e| {
                StorageError::Internal(format!("could not start background compaction: {e}"))
            })?;
        *slot = Some(handle);
        Ok(())
    }

    /// Stop the server, letting a file in hand finish. Called before the
    /// connection closes.
    pub(crate) fn shutdown(&self) -> Result<()> {
        {
            let (lock, cv) = &*self.state;
            let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
            st.stop = true;
            st.running = false;
            cv.notify_all();
        }
        let handle = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = handle {
            h.join().map_err(|_| {
                StorageError::Internal("the background compaction server panicked".into())
            })?;
        }
        Ok(())
    }

    /// The `background-compact` statistics, as `(description, value)`.
    pub(crate) fn statistics(&self) -> Vec<(String, i64)> {
        let c = &self.counters;
        let values = [
            &c.failed,
            &c.failed_cache_pressure,
            &c.interrupted,
            &c.ema,
            &c.recovered,
            &c.running,
            &c.excluded,
            &c.skipped,
            &c.sleeps_cache_pressure,
            &c.succeeded,
            &c.timeout,
            &c.tracked,
        ];
        STATISTICS
            .iter()
            .zip(values)
            .map(|(d, v)| (d.to_string(), v.load(Ordering::Relaxed)))
            .collect()
    }
}

/// The server thread's own state (11.3's `__background_compact_server`).
struct Server {
    conn: Arc<Connection>,
    home: String,
    state: Arc<(Mutex<State>, Condvar)>,
    counters: Arc<Counters>,
    gate: Arc<Mutex<()>>,
    files: HashMap<String, FileRecord>,
    ema: u64,
}

impl Server {
    fn run(mut self) {
        if let Err(e) = self.serve() {
            // 11.3 panics the connection here ("compact server error"). A
            // failure that is not one of compaction's ordinary refusals is
            // a storage error: said loudly, and the server stops.
            eprintln!("secantus-storage: background compaction stopped on an error: {e:?}");
            let (lock, _) = &*self.state;
            let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
            st.running = false;
            st.signalled = false;
        }
        self.counters.running.store(0, Ordering::Relaxed);
        self.counters.tracked.store(0, Ordering::Relaxed);
    }

    fn serve(&mut self) -> Result<()> {
        let session = self.conn.open_session()?;
        let mut uri = String::new();
        let (mut full_iteration, mut cache_pressure, mut running) = (false, false, false);
        let mut config = Config {
            free_space_target: megabytes(DEFAULT_FREE_SPACE_TARGET_MB),
            run_once: false,
        };
        self.counters.running.store(0, Ordering::Relaxed);
        loop {
            if full_iteration && config.run_once {
                let (lock, _) = &*self.state;
                let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
                st.running = false;
                running = false;
                self.counters.running.store(0, Ordering::Relaxed);
            }
            if !running || full_iteration || cache_pressure {
                if uri.is_empty() || full_iteration {
                    full_iteration = false;
                    uri = "file:".to_string();
                    if config.run_once {
                        self.forget_all();
                    } else {
                        self.forget_stale();
                    }
                }
                if cache_pressure {
                    self.counters
                        .sleeps_cache_pressure
                        .fetch_add(1, Ordering::Relaxed);
                    cache_pressure = false;
                }
                let (lock, cv) = &*self.state;
                let st = lock.lock().unwrap_or_else(|e| e.into_inner());
                if !st.stop && !st.signalled {
                    drop(
                        cv.wait_timeout(st, FULL_ITERATION_WAIT)
                            .unwrap_or_else(|e| e.into_inner()),
                    );
                }
            }
            let signalled = {
                let state = self.state.clone();
                let (lock, _) = &*state;
                let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
                if st.stop {
                    return Ok(());
                }
                running = st.running;
                if let Some(c) = &st.config {
                    config = c.clone();
                }
                std::mem::take(&mut st.signalled)
            };
            if signalled {
                if running && config.run_once {
                    uri = "file:".to_string();
                }
                if !running {
                    self.forget_all();
                }
                self.counters
                    .running
                    .store(i64::from(running), Ordering::Relaxed);
            }
            if !running {
                continue;
            }
            cache_pressure = cache_under_pressure(&session)?;
            if cache_pressure {
                continue;
            }
            let Some(next) = self.next_uri(&session, &uri, &config)? else {
                full_iteration = true;
                continue;
            };
            uri = next;
            let start = self.size_of(&uri)?;
            let outcome = {
                let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
                compact_table(&session, &uri, config.free_space_target)
            };
            match outcome {
                Ok(()) => {
                    self.counters.succeeded.fetch_add(1, Ordering::Relaxed);
                    let end = self.size_of(&uri)?;
                    self.record(&uri, start, end);
                }
                Err(StorageError::Wt(e)) if silenced(&e) => {
                    self.counters.failed.fetch_add(1, Ordering::Relaxed);
                    if crate::compact::is_busy(&e) && cache_under_pressure(&session)? {
                        self.counters
                            .failed_cache_pressure
                            .fetch_add(1, Ordering::Relaxed);
                    } else if e.code == ETIMEDOUT {
                        self.counters.timeout.fetch_add(1, Ordering::Relaxed);
                    }
                    self.record(&uri, start, start);
                }
                Err(e) => {
                    self.counters.failed.fetch_add(1, Ordering::Relaxed);
                    return Err(e);
                }
            }
        }
    }

    /// The next `file:` after `after` worth compacting (11.3's
    /// `__background_compact_find_next_uri`), counting the ones skipped.
    fn next_uri(
        &mut self,
        session: &Session,
        after: &str,
        config: &Config,
    ) -> Result<Option<String>> {
        let cursor = session.open_cursor("metadata:", None)?;
        cursor.set_key_s(after);
        let mut more = match cursor.search_near() {
            Ok(exact) if exact <= 0 => cursor.next()?,
            Ok(_) => true,
            Err(e) if e.is_not_found() => false,
            Err(e) => return Err(e.into()),
        };
        while more {
            let key = cursor.get_key_s()?;
            if !key.starts_with("file:") {
                return Ok(None);
            }
            if !self.should_skip(&key, config)? {
                return Ok(Some(key));
            }
            more = cursor.next()?;
        }
        Ok(None)
    }

    /// 11.3's `__background_compact_should_skip`.
    fn should_skip(&mut self, uri: &str, config: &Config) -> Result<bool> {
        let name = uri.trim_start_matches("file:");
        // WiredTiger's own files (its history store, its metadata) are not
        // a session's to compact.
        if name.starts_with("WiredTiger") {
            return Ok(true);
        }
        if excluded(name) {
            self.counters.excluded.fetch_add(1, Ordering::Relaxed);
            return Ok(true);
        }
        let size = match std::fs::metadata(std::path::Path::new(&self.home).join(name)) {
            Ok(m) => m.len(),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok(true)
            }
            Err(e) => {
                return Err(StorageError::Internal(format!(
                    "could not read the size of {name}: {e}"
                )))
            }
        };
        if size <= SMALL_FILE {
            self.counters.skipped.fetch_add(1, Ordering::Relaxed);
            return Ok(true);
        }
        let Some(record) = self.files.get(uri) else {
            return Ok(false);
        };
        if config.run_once || record.prev_compact_time.elapsed() >= MAX_FILE_SKIP {
            return Ok(false);
        }
        if !record.prev_compact_success || record.bytes_rewritten < self.ema {
            self.counters.skipped.fetch_add(1, Ordering::Relaxed);
            return Ok(true);
        }
        Ok(false)
    }

    /// The size of a `file:` URI's file, 0 once it is gone.
    fn size_of(&self, uri: &str) -> Result<u64> {
        let path = std::path::Path::new(&self.home).join(uri.trim_start_matches("file:"));
        match std::fs::metadata(&path) {
            Ok(m) => Ok(m.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(StorageError::Internal(format!(
                "could not read the size of {}: {e}",
                path.display()
            ))),
        }
    }

    /// 11.3's `__wt_background_compact_end`: what a pass gave back.
    fn record(&mut self, uri: &str, start: u64, end: u64) {
        let recovered = start.saturating_sub(end);
        let success = end < start;
        if success {
            self.counters
                .recovered
                .fetch_add(recovered as i64, Ordering::Relaxed);
            self.ema = (0.1 * recovered as f64 + 0.9 * self.ema as f64) as u64;
            self.counters.ema.store(self.ema as i64, Ordering::Relaxed);
        }
        self.files.insert(
            uri.to_string(),
            FileRecord {
                prev_compact_time: Instant::now(),
                prev_compact_success: success,
                bytes_rewritten: recovered,
            },
        );
        self.counters
            .tracked
            .store(self.files.len() as i64, Ordering::Relaxed);
    }

    fn forget_all(&mut self) {
        self.files.clear();
        self.counters.tracked.store(0, Ordering::Relaxed);
    }

    fn forget_stale(&mut self) {
        self.files
            .retain(|_, r| r.prev_compact_time.elapsed() < MAX_FILE_IDLE);
        self.counters
            .tracked
            .store(self.files.len() as i64, Ordering::Relaxed);
    }
}

/// The files `autoCompact` leaves alone: the oplog's, as mongod excludes its
/// oplog table.
fn excluded(file: &str) -> bool {
    file == "secantus_oplog.wt" || file.starts_with("secantus_oplog_sh")
}

const ENOENT: i32 = 2;
const ETIMEDOUT: i32 = if cfg!(target_os = "linux") { 110 } else { 60 };

/// The refusals 11.3's server counts as a failed call and goes on from:
/// `EBUSY`, `ENOENT` (the file went), `ETIMEDOUT` and `WT_ROLLBACK`.
fn silenced(e: &WtError) -> bool {
    crate::compact::is_busy(e) || e.code == ENOENT || e.code == ETIMEDOUT || e.is_rollback()
}

/// Whether the cache is near an eviction trigger (11.3's
/// `__wt_evict_dirty_needed || __wt_evict_clean_needed`, at WiredTiger's
/// default triggers: dirty content over 20% of the cache, or the cache over
/// 95% full).
fn cache_under_pressure(session: &Session) -> Result<bool> {
    let cursor = session.open_cursor("statistics:", None)?;
    let (mut max, mut inuse, mut dirty) = (0i64, 0i64, 0i64);
    while cursor.next()? {
        let (d, v) = cursor.get_value_stat()?;
        match d.as_str() {
            "cache: maximum bytes configured" => max = v,
            "cache: bytes currently in the cache" => inuse = v,
            "cache: tracked dirty bytes in the cache" => dirty = v,
            _ => {}
        }
    }
    if max <= 0 {
        return Ok(false);
    }
    Ok(dirty * 100 > max * 20 || inuse * 100 > max * 95)
}
