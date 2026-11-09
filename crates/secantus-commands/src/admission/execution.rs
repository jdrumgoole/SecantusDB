//! mongod's execution admission control (RM-355): the read and write
//! ticket pools an operation takes a ticket from as it takes the global
//! lock, and the throughput probing that sizes them.
//!
//! A port of mongod 8.2.11's `initializeExecutionControl`
//! (`db/admission/execution_control_init.cpp`), `TicketHolderManager`
//! (`ticketholder_manager.cpp`) and `ThroughputProbing`
//! (`throughput_probing.cpp`). Two pools of the gradual resize policy, each
//! counting a ticket held past `delinquentAcquisitionIntervalMillis` as
//! delinquent:
//!
//! - with `storageEngineConcurrencyAdjustmentAlgorithm: throughputProbing`
//!   (the default) a monitor runs every
//!   `storageEngineConcurrencyAdjustmentIntervalMillis` and moves the pools'
//!   sizes to where throughput is best, starting from twice the logical
//!   cores split by `throughputProbingReadWriteRatio`;
//! - with `fixedConcurrentTransactions` (which a concurrency given at
//!   startup implies) the pools are `storageEngineConcurrentReadTransactions`
//!   and `...WriteTransactions` tickets, 128 when not given, resized only
//!   by `setParameter`.
//!
//! Where an operation takes and returns its tickets is the lock manager's
//! (`lock_manager`, mongod's `Locker::_acquireTicket`).

use std::sync::atomic::{AtomicI32, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use bson::{doc, Bson, Document};

use super::{ResizePolicy, TicketHolder};
use crate::CommandError;

pub const ALGORITHM: &str = "storageEngineConcurrencyAdjustmentAlgorithm";
pub const INTERVAL_MILLIS: &str = "storageEngineConcurrencyAdjustmentIntervalMillis";
pub const WRITE_TRANSACTIONS: &str = "storageEngineConcurrentWriteTransactions";
pub const READ_TRANSACTIONS: &str = "storageEngineConcurrentReadTransactions";
/// The deprecated names of the two concurrencies, which mongod keeps as
/// aliases of the same values.
pub const WT_WRITE_TRANSACTIONS: &str = "wiredTigerConcurrentWriteTransactions";
pub const WT_READ_TRANSACTIONS: &str = "wiredTigerConcurrentReadTransactions";
pub const READ_MAX_QUEUE_DEPTH: &str = "storageEngineReadMaxQueueDepth";
pub const WRITE_MAX_QUEUE_DEPTH: &str = "storageEngineWriteMaxQueueDepth";
pub const STEP_MULTIPLE: &str = "throughputProbingStepMultiple";
pub const INITIAL_CONCURRENCY: &str = "throughputProbingInitialConcurrency";
pub const MIN_CONCURRENCY: &str = "throughputProbingMinConcurrency";
pub const MAX_CONCURRENCY: &str = "throughputProbingMaxConcurrency";
pub const READ_WRITE_RATIO: &str = "throughputProbingReadWriteRatio";
pub const MOVING_AVERAGE_WEIGHT: &str = "throughputProbingConcurrencyMovingAverageWeight";
pub const STALL_DETECTION_TIMEOUT_MS: &str = "throughputProbingStallDetectionTimeoutMs";
pub const DELINQUENT_INTERVAL_MILLIS: &str = "delinquentAcquisitionIntervalMillis";

pub const THROUGHPUT_PROBING: &str = "throughputProbing";
pub const FIXED_CONCURRENT_TRANSACTIONS: &str = "fixedConcurrentTransactions";

/// A pool's size when the concurrency was not given (mongod's
/// `DEFAULT_TICKETS_VALUE`).
const DEFAULT_TICKETS: i32 = 128;
/// A pool's queue depth (mongod's `TicketHolder::kDefaultMaxQueueDepth`,
/// `DEFAULT_MAX_CONN`).
pub const DEFAULT_MAX_QUEUE_DEPTH: i32 = 1_000_000;

/// mongod 8.2.11's defaults for the parameters, `(name, value)`.
pub fn defaults() -> [(&'static str, Bson); 16] {
    [
        (ALGORITHM, Bson::String(THROUGHPUT_PROBING.to_string())),
        (INTERVAL_MILLIS, Bson::Int32(100)),
        (READ_TRANSACTIONS, Bson::Int32(0)),
        (WRITE_TRANSACTIONS, Bson::Int32(0)),
        (WT_READ_TRANSACTIONS, Bson::Int32(0)),
        (WT_WRITE_TRANSACTIONS, Bson::Int32(0)),
        (READ_MAX_QUEUE_DEPTH, Bson::Int32(DEFAULT_MAX_QUEUE_DEPTH)),
        (WRITE_MAX_QUEUE_DEPTH, Bson::Int32(DEFAULT_MAX_QUEUE_DEPTH)),
        (STEP_MULTIPLE, Bson::Double(0.1)),
        (INITIAL_CONCURRENCY, Bson::Int32(0)),
        (MIN_CONCURRENCY, Bson::Int32(4)),
        (MAX_CONCURRENCY, Bson::Int32(128)),
        (READ_WRITE_RATIO, Bson::Double(0.5)),
        (MOVING_AVERAGE_WEIGHT, Bson::Double(0.2)),
        (STALL_DETECTION_TIMEOUT_MS, Bson::Int32(60_000)),
        (DELINQUENT_INTERVAL_MILLIS, Bson::Int32(30)),
    ]
}

/// The value of `name` in `values`, else its default.
fn value_of(values: &dyn Fn(&str) -> Option<Bson>, name: &str) -> Bson {
    values(name).unwrap_or_else(|| {
        defaults()
            .into_iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v)
            .unwrap_or(Bson::Null)
    })
}

fn int_of(values: &dyn Fn(&str) -> Option<Bson>, name: &str) -> i32 {
    match value_of(values, name) {
        Bson::Int32(n) => n,
        Bson::Int64(n) => n as i32,
        Bson::Double(d) => d as i32,
        _ => 0,
    }
}

fn double_of(values: &dyn Fn(&str) -> Option<Bson>, name: &str) -> f64 {
    match value_of(values, name) {
        Bson::Double(d) => d,
        Bson::Int32(n) => f64::from(n),
        Bson::Int64(n) => n as f64,
        _ => 0.0,
    }
}

/// Whether `name` is one of execution control's parameters.
pub fn is_parameter(name: &str) -> bool {
    defaults().iter().any(|(n, _)| *n == name)
}

/// The other name of a concurrency (mongod keeps the deprecated
/// `wiredTiger*` names as aliases of the same value).
pub fn alias(name: &str) -> Option<&'static str> {
    match name {
        WRITE_TRANSACTIONS => Some(WT_WRITE_TRANSACTIONS),
        WT_WRITE_TRANSACTIONS => Some(WRITE_TRANSACTIONS),
        READ_TRANSACTIONS => Some(WT_READ_TRANSACTIONS),
        WT_READ_TRANSACTIONS => Some(READ_TRANSACTIONS),
        _ => None,
    }
}

fn is_double(name: &str) -> bool {
    matches!(
        name,
        STEP_MULTIPLE | READ_WRITE_RATIO | MOVING_AVERAGE_WEIGHT
    )
}

/// A bound as mongod's IDL validators word it: `{value} is not {relation}
/// {bound}`, both printed as `%g` prints them.
fn bound(name: &str, value: f64, relation: &str, limit: f64) -> String {
    let g = |d: f64| crate::explain::debug_string::g(d, 6);
    format!(
        "Invalid value for parameter {name}: {} is not {relation} {}",
        g(value),
        g(limit)
    )
}

/// mongod 8.2.11's validators for the parameters (measured 2026-11-09):
/// `Ok` or the reason it refuses `value`. `current` gives the other
/// parameters' values, which the probing concurrencies are checked against;
/// `test_commands` lifts the concurrencies' floor of 5.
fn validate(
    name: &str,
    value: &Bson,
    test_commands: bool,
    current: &dyn Fn(&str) -> Option<Bson>,
) -> Result<(), String> {
    let n = match value {
        Bson::Int32(n) => f64::from(*n),
        Bson::Double(d) => *d,
        Bson::String(s) if name == ALGORITHM => {
            return if s == THROUGHPUT_PROBING || s == FIXED_CONCURRENT_TRANSACTIONS {
                Ok(())
            } else {
                Err(format!(
                    "Enumeration value '{s}' for field '{ALGORITHM}' is not a valid value."
                ))
            };
        }
        _ => return Ok(()),
    };
    let at_least = |min: f64| {
        if n >= min {
            Ok(())
        } else {
            Err(bound(name, n, "greater than or equal to", min))
        }
    };
    let at_most = |max: f64| {
        if n <= max {
            Ok(())
        } else {
            Err(bound(name, n, "less than or equal to", max))
        }
    };
    let above = |min: f64| {
        if n > min {
            Ok(())
        } else {
            Err(bound(name, n, "greater than", min))
        }
    };
    let min = f64::from(int_of(current, MIN_CONCURRENCY));
    let max = f64::from(int_of(current, MAX_CONCURRENCY));
    match name {
        WRITE_TRANSACTIONS | WT_WRITE_TRANSACTIONS | READ_TRANSACTIONS | WT_READ_TRANSACTIONS => {
            if !test_commands && n < 5.0 {
                let kind = if matches!(name, WRITE_TRANSACTIONS | WT_WRITE_TRANSACTIONS) {
                    "write"
                } else {
                    "read"
                };
                return Err(format!(
                    "Concurrent {kind} transactions limit must be greater than or equal to 5."
                ));
            }
            Ok(())
        }
        READ_MAX_QUEUE_DEPTH | WRITE_MAX_QUEUE_DEPTH => at_least(0.0),
        INTERVAL_MILLIS | DELINQUENT_INTERVAL_MILLIS => at_least(10.0),
        STEP_MULTIPLE => at_least(0.01).and_then(|()| at_most(0.5)),
        READ_WRITE_RATIO => at_least(0.0).and_then(|()| at_most(1.0)),
        MOVING_AVERAGE_WEIGHT => above(0.0).and_then(|()| at_most(1.0)),
        STALL_DETECTION_TIMEOUT_MS => above(0.0),
        INITIAL_CONCURRENCY if n < min => Err(
            "Throughput probing initial concurrency cannot be less than minimum concurrency".into(),
        ),
        INITIAL_CONCURRENCY if n > max => Err(
            "Throughput probing initial concurrency cannot be greater than maximum concurrency"
                .into(),
        ),
        MIN_CONCURRENCY if n < 1.0 => {
            Err("Throughput probing minimum concurrency cannot be less than 1".into())
        }
        MIN_CONCURRENCY if n > max => Err(
            "Throughput probing minimum concurrency cannot be greater than maximum concurrency"
                .into(),
        ),
        MAX_CONCURRENCY if n < min => Err(
            "Throughput probing maximum concurrency cannot be less than minimum concurrency".into(),
        ),
        _ => Ok(()),
    }
}

/// The value `setParameter` stores for `name`, or mongod's reason for
/// refusing it: a whole-number parameter coerces as mongod's do (a bool is 0
/// or 1, a fraction truncated), a fractional one takes any number but not a
/// bool, and then the parameter's validator runs.
pub fn coerce_runtime(
    name: &str,
    value: &Bson,
    test_commands: bool,
    current: &dyn Fn(&str) -> Option<Bson>,
) -> Result<Bson, String> {
    let coerced = if is_double(name) {
        let d = match value {
            Bson::Int32(i) => f64::from(*i),
            Bson::Int64(i) => *i as f64,
            Bson::Double(d) => *d,
            Bson::Decimal128(d) => d.to_string().parse().unwrap_or(f64::NAN),
            _ => {
                return Err(format!(
                    "Failed validating {name}: Unable to coerce value to correct type"
                ))
            }
        };
        Bson::Double(d)
    } else {
        let n = crate::params::integral(value, true)
            .map_err(|why| format!("Failed validating {name}: {why}"))?;
        Bson::Int32(n as i32)
    };
    validate(name, &coerced, test_commands, current)?;
    Ok(coerced)
}

/// The value `--setParameter name=raw` gives `name` at startup, or
/// mongod's reason for refusing it.
pub fn parse_startup(
    name: &str,
    raw: &str,
    test_commands: bool,
    current: &dyn Fn(&str) -> Option<Bson>,
) -> Result<Bson, String> {
    let value = if name == ALGORITHM {
        Bson::String(raw.to_string())
    } else if is_double(name) {
        match raw.parse::<f64>() {
            Ok(d) => Bson::Double(d),
            Err(_) => {
                let digits = raw
                    .trim_start_matches(['-', '+'])
                    .starts_with(|c: char| c.is_ascii_digit() || c == '.');
                return Err(if digits {
                    "Did not consume whole string.".to_string()
                } else {
                    "Did not consume any digits".to_string()
                });
            }
        }
    } else {
        let digits = raw.strip_prefix(['-', '+']).unwrap_or(raw);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err("Did not consume whole string.".to_string());
        }
        match raw.parse::<i32>() {
            Ok(n) => Bson::Int32(n),
            Err(_) => return Err("Value is out of range".to_string()),
        }
    };
    validate(name, &value, test_commands, current)?;
    Ok(value)
}

/// The probing parameters mongod lets change while it runs, read by each
/// round of the monitor.
#[derive(Debug)]
struct ProbingParams {
    step_multiple: AtomicU64,
    max_concurrency: AtomicI32,
    read_write_ratio: AtomicU64,
    moving_average_weight: AtomicU64,
    stall_detection_timeout_ms: AtomicI32,
}

fn load_f64(a: &AtomicU64) -> f64 {
    f64::from_bits(a.load(Ordering::Relaxed))
}

fn store_f64(a: &AtomicU64, v: f64) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

/// mongod's `ThroughputProbing::Stats`.
#[derive(Debug, Default)]
struct ProbeStats {
    times_decreased: AtomicI64,
    times_increased: AtomicI64,
    total_amount_decreased: AtomicI64,
    total_amount_increased: AtomicI64,
    resize_duration_micros: AtomicI64,
    times_probed_stable: AtomicI64,
    times_probed_up: AtomicI64,
    times_probed_down: AtomicI64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbingState {
    Stable,
    Up,
    Down,
}

#[derive(Debug)]
struct Probe {
    state: ProbingState,
    stable_concurrency: f64,
    stable_throughput: f64,
    /// The operations finished at the last round; below zero before the
    /// first.
    prev_finished: i64,
    timer: Instant,
}

/// mongod's `ThroughputProbing`: each round compares the throughput (the
/// operations that returned their tickets, per microsecond) with the stable
/// baseline, and moves both pools' sizes up, down or back.
pub struct ThroughputProbing {
    read: Arc<TicketHolder>,
    write: Arc<TicketHolder>,
    params: Arc<ProbingParams>,
    min_concurrency: i32,
    probe: Mutex<Probe>,
    stats: ProbeStats,
    logs: Option<Arc<crate::logbuf::LogBuffer>>,
}

/// An exponential moving average weighing `new_value` by `weight`.
fn exp_moving_average(average: f64, new_value: f64, weight: f64) -> f64 {
    (new_value * weight) + (average * (1.0 - weight))
}

impl ThroughputProbing {
    fn new(
        read: Arc<TicketHolder>,
        write: Arc<TicketHolder>,
        params: Arc<ProbingParams>,
        min_concurrency: i32,
        initial_concurrency: i32,
        cores: i32,
        logs: Option<Arc<crate::logbuf::LogBuffer>>,
    ) -> Self {
        let max = params.max_concurrency.load(Ordering::Relaxed);
        let stable_concurrency = if initial_concurrency != 0 {
            initial_concurrency
        } else {
            (cores.saturating_mul(2)).clamp(min_concurrency * 2, max.saturating_mul(2))
        };
        let probing = ThroughputProbing {
            read,
            write,
            params,
            min_concurrency,
            probe: Mutex::new(Probe {
                state: ProbingState::Stable,
                stable_concurrency: f64::from(stable_concurrency),
                stable_throughput: 0.0,
                prev_finished: -1,
                timer: Instant::now(),
            }),
            stats: ProbeStats::default(),
            logs,
        };
        {
            let probe = probing.probe.lock().unwrap_or_else(|e| e.into_inner());
            probing.reset_concurrency(&probe);
        }
        probing
    }

    /// mongod's `_run`, one round, as of `now`.
    fn run(&self, now: Instant) {
        let mut probe = self.probe.lock().unwrap_or_else(|e| e.into_inner());
        let finished = self.read.num_finished_processing() + self.write.num_finished_processing();
        if probe.prev_finished < 0 {
            probe.prev_finished = finished;
            probe.timer = now;
            return;
        }
        let elapsed = now.saturating_duration_since(probe.timer).as_micros();
        if elapsed == 0 {
            // No time has passed by this clock: wait for the next round.
            return;
        }
        let throughput = (finished - probe.prev_finished) as f64 / elapsed as f64;
        match probe.state {
            ProbingState::Stable => self.probe_stable(&mut probe, throughput),
            ProbingState::Up => self.probe_up(&mut probe, throughput),
            ProbingState::Down => self.probe_down(&mut probe, throughput),
        }
        // Fresh values after the adjustment, for a better cause and effect.
        probe.prev_finished =
            self.read.num_finished_processing() + self.write.num_finished_processing();
        probe.timer = Instant::now().max(now);
    }

    fn max_concurrency(&self) -> i32 {
        self.params.max_concurrency.load(Ordering::Relaxed)
    }

    fn probe_stable(&self, probe: &mut Probe, throughput: f64) {
        probe.stable_throughput = throughput;
        let (read_total, write_total) = (self.read.outof(), self.write.outof());
        let read_peak = self.read.get_and_reset_peak_used();
        let write_peak = self.write.get_and_reset_peak_used();
        let max = self.max_concurrency();
        if (read_total < max && read_peak >= read_total)
            || (write_total < max && write_peak >= write_total)
        {
            // A pool is exhausted: try more concurrency.
            probe.state = ProbingState::Up;
            self.increase_concurrency(probe);
        } else if read_peak > self.min_concurrency || write_peak > self.min_concurrency {
            // Neither is: try just below the use there is.
            probe.state = ProbingState::Down;
            self.decrease_concurrency(probe);
        }
        self.stats
            .times_probed_stable
            .fetch_add(1, Ordering::Relaxed);
    }

    /// The probe up or down moved throughput: when it rose, the stable
    /// concurrency moves toward the probed one by the moving average.
    /// Answers the stable concurrency it replaced, when it did.
    fn settle(&self, probe: &mut Probe, throughput: f64) -> Option<f64> {
        let improved = throughput > probe.stable_throughput;
        let old = probe.stable_concurrency;
        if improved {
            let concurrency = f64::from(self.read.outof() + self.write.outof());
            probe.stable_concurrency = exp_moving_average(
                probe.stable_concurrency,
                concurrency,
                load_f64(&self.params.moving_average_weight),
            );
            probe.stable_throughput = throughput;
        }
        probe.state = ProbingState::Stable;
        self.reset_concurrency(probe);
        improved.then_some(old)
    }

    fn probe_up(&self, probe: &mut Probe, throughput: f64) {
        if let Some(old) = self.settle(probe, throughput) {
            self.stats.times_increased.fetch_add(1, Ordering::Relaxed);
            let now = f64::from(self.read.outof() + self.write.outof());
            self.stats
                .total_amount_increased
                .fetch_add((now - old) as i64, Ordering::Relaxed);
        }
        self.stats.times_probed_up.fetch_add(1, Ordering::Relaxed);
    }

    fn probe_down(&self, probe: &mut Probe, throughput: f64) {
        if let Some(old) = self.settle(probe, throughput) {
            self.stats.times_decreased.fetch_add(1, Ordering::Relaxed);
            let now = f64::from(self.read.outof() + self.write.outof());
            self.stats
                .total_amount_decreased
                .fetch_add((old - now) as i64, Ordering::Relaxed);
        }
        self.stats.times_probed_down.fetch_add(1, Ordering::Relaxed);
    }

    /// mongod's `newReadWriteConcurrencies`: the stable concurrency scaled
    /// by `step` and split by the read/write ratio, each pool within the
    /// minimum and maximum.
    fn concurrencies(&self, stable: f64, step: f64) -> (i32, i32) {
        let read_pct = load_f64(&self.params.read_write_ratio);
        let (min, max) = (self.min_concurrency, self.max_concurrency());
        let clamp = |pct: f64| ((stable * pct * step).round() as i32).clamp(min, max.max(min));
        (clamp(read_pct), clamp(1.0 - read_pct))
    }

    fn reset_concurrency(&self, probe: &Probe) {
        let (read, write) = self.concurrencies(probe.stable_concurrency, 1.0);
        self.resize(&self.read, read, "reads");
        self.resize(&self.write, write, "writes");
    }

    fn increase_concurrency(&self, probe: &Probe) {
        let step = 1.0 + load_f64(&self.params.step_multiple);
        let (mut read, mut write) = self.concurrencies(probe.stable_concurrency, step);
        if read == self.read.outof() {
            read += 1;
        }
        if write == self.write.outof() {
            write += 1;
        }
        self.resize(&self.read, read, "reads");
        self.resize(&self.write, write, "writes");
    }

    fn decrease_concurrency(&self, probe: &Probe) {
        let step = 1.0 - load_f64(&self.params.step_multiple);
        let (mut read, mut write) = self.concurrencies(probe.stable_concurrency, step);
        if read == self.read.outof() {
            read -= 1;
        }
        if write == self.write.outof() {
            write -= 1;
        }
        self.resize(&self.read, read, "reads");
        self.resize(&self.write, write, "writes");
    }

    /// mongod's `_resize`: a shrink that cannot take its tickets back
    /// within `throughputProbingStallDetectionTimeoutMs` is a stall, logged
    /// as mongod logs it.
    fn resize(&self, holder: &TicketHolder, tickets: i32, pool: &str) {
        let started = Instant::now();
        let finished_before = holder.num_finished_processing();
        let timeout = self
            .params
            .stall_detection_timeout_ms
            .load(Ordering::Relaxed)
            .max(1);
        let deadline = started + Duration::from_millis(timeout as u64);
        let resized = holder.resize(tickets, Some(deadline));
        let elapsed = started.elapsed();
        self.stats
            .resize_duration_micros
            .fetch_add(elapsed.as_micros() as i64, Ordering::Relaxed);
        if !resized {
            if let Some(logs) = &self.logs {
                logs.log(
                    crate::logbuf::LogEntry::new(
                        "W",
                        "STORAGE",
                        8373000,
                        "ThroughputProbingTicketHolderMonitor",
                        "Unable to acquire a ticket within deadline, which indicates the system \
                         is stalled",
                    )
                    .attr(doc! {
                        "ticketPool": pool,
                        "total": holder.outof(),
                        "target": tickets,
                        "throughput": holder.num_finished_processing() - finished_before,
                        "queued": holder.queued(),
                        "stalled": format!("{} ms", elapsed.as_millis()),
                    }),
                );
            }
        }
    }

    /// `serverStatus.queues.execution.monitor`: every count a long.
    fn stats(&self) -> Document {
        let get = |a: &AtomicI64| a.load(Ordering::Relaxed);
        let s = &self.stats;
        doc! {
            "timesDecreased": get(&s.times_decreased),
            "timesIncreased": get(&s.times_increased),
            "totalAmountDecreased": get(&s.total_amount_decreased),
            "totalAmountIncreased": get(&s.total_amount_increased),
            "resizeDurationMicros": get(&s.resize_duration_micros),
            "timesProbedStable": get(&s.times_probed_stable),
            "timesProbedUp": get(&s.times_probed_up),
            "timesProbedDown": get(&s.times_probed_down),
        }
    }
}

/// The server's execution control: its read and write pools, and the
/// monitor sizing them when probing (mongod's `TicketHolderManager` and its
/// two kinds).
pub struct ExecutionControl {
    read: Arc<TicketHolder>,
    write: Arc<TicketHolder>,
    params: Arc<ProbingParams>,
    probing: Option<Arc<ThroughputProbing>>,
}

/// mongod's `ProcessInfo::getNumLogicalCores`: `sysconf(_SC_NPROCESSORS_ONLN)`
/// on POSIX (`processinfo_linux.cpp`, `processinfo_osx.mm`).
#[cfg(unix)]
pub fn logical_cores() -> i32 {
    // SAFETY: `sysconf` reads a system constant and touches no memory of ours.
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if n > 0 {
        n as i32
    } else {
        1
    }
}

/// mongod's `ProcessInfo::getNumLogicalCores` off POSIX: on Windows it is
/// `GetSystemInfo`'s `dwNumberOfProcessors` (`processinfo_windows.cpp`),
/// which is what `available_parallelism` reads there. `libc` is a
/// unix-only dependency of this crate.
#[cfg(not(unix))]
pub fn logical_cores() -> i32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as i32)
}

impl ExecutionControl {
    /// mongod's `initializeExecutionControl`, with the parameters as
    /// `values` holds them (each `None` at its default), and the monitor
    /// started when it probes.
    pub fn start(
        values: &dyn Fn(&str) -> Option<Bson>,
        logs: Option<Arc<crate::logbuf::LogBuffer>>,
    ) -> Arc<Self> {
        let control = Self::build(values, logical_cores(), logs);
        if let Some(probing) = &control.probing {
            let interval = int_of(values, INTERVAL_MILLIS).max(10) as u64;
            spawn_monitor(Arc::downgrade(probing), Duration::from_millis(interval));
        }
        control
    }

    /// The control as `start` builds it, with no monitor running: what a
    /// test drives by hand.
    pub(crate) fn build(
        values: &dyn Fn(&str) -> Option<Bson>,
        cores: i32,
        logs: Option<Arc<crate::logbuf::LogBuffer>>,
    ) -> Arc<Self> {
        let read_transactions = int_of(values, READ_TRANSACTIONS);
        let write_transactions = int_of(values, WRITE_TRANSACTIONS);
        // A concurrency given at startup fixes the pools.
        let user_set = read_transactions != 0 || write_transactions != 0;
        let probing = !user_set && value_of(values, ALGORITHM).as_str() == Some(THROUGHPUT_PROBING);
        let tickets = |n: i32| if n == 0 { DEFAULT_TICKETS } else { n };
        let delinquent_after = Some(Duration::from_millis(
            int_of(values, DELINQUENT_INTERVAL_MILLIS).max(0) as u64,
        ));
        let pool = |n: i32, depth: &str| {
            Arc::new(TicketHolder::with_policy(
                tickets(n),
                int_of(values, depth),
                delinquent_after,
                ResizePolicy::Gradual,
                probing,
            ))
        };
        let read = pool(read_transactions, READ_MAX_QUEUE_DEPTH);
        let write = pool(write_transactions, WRITE_MAX_QUEUE_DEPTH);
        let params = Arc::new(ProbingParams {
            step_multiple: AtomicU64::new(double_of(values, STEP_MULTIPLE).to_bits()),
            max_concurrency: AtomicI32::new(int_of(values, MAX_CONCURRENCY)),
            read_write_ratio: AtomicU64::new(double_of(values, READ_WRITE_RATIO).to_bits()),
            moving_average_weight: AtomicU64::new(
                double_of(values, MOVING_AVERAGE_WEIGHT).to_bits(),
            ),
            stall_detection_timeout_ms: AtomicI32::new(int_of(values, STALL_DETECTION_TIMEOUT_MS)),
        });
        let probing = probing.then(|| {
            Arc::new(ThroughputProbing::new(
                read.clone(),
                write.clone(),
                params.clone(),
                int_of(values, MIN_CONCURRENCY),
                int_of(values, INITIAL_CONCURRENCY),
                cores,
                logs,
            ))
        });
        Arc::new(ExecutionControl {
            read,
            write,
            params,
            probing,
        })
    }

    /// The pool a global lock in a shared mode takes from (`reader`), or
    /// the one an intent-exclusive one does.
    pub fn holder(&self, reader: bool) -> &Arc<TicketHolder> {
        if reader {
            &self.read
        } else {
            &self.write
        }
    }

    /// Whether the pools' sizes are the user's to change (mongod's
    /// `supportsRuntimeSizeAdjustment`): not while probing sizes them.
    pub fn supports_runtime_size_adjustment(&self) -> bool {
        self.probing.is_none()
    }

    /// `serverStatus.queues.execution`.
    pub fn server_status(&self) -> Document {
        let mut out = doc! {
            "write": self.write.stats(),
            "read": self.read.stats(),
        };
        if let Some(probing) = &self.probing {
            out.insert("monitor", probing.stats());
        }
        out
    }

    /// Apply a parameter `setParameter` has stored (mongod's `on_update`
    /// callbacks): a concurrency is refused while probing sizes the pools,
    /// after it was stored, as mongod refuses it.
    pub fn apply(&self, name: &str, value: &Bson) -> Result<(), CommandError> {
        let int = || match value {
            Bson::Int32(n) => Some(*n),
            _ => None,
        };
        let double = || match value {
            Bson::Double(d) => Some(*d),
            _ => None,
        };
        match name {
            WRITE_TRANSACTIONS
            | WT_WRITE_TRANSACTIONS
            | READ_TRANSACTIONS
            | WT_READ_TRANSACTIONS => {
                let writes = matches!(name, WRITE_TRANSACTIONS | WT_WRITE_TRANSACTIONS);
                if !self.supports_runtime_size_adjustment() {
                    return Err(CommandError::new(
                        20,
                        "IllegalOperation",
                        format!(
                            "Cannot modify concurrent {} transactions limit when it is being \
                             dynamically adjusted",
                            if writes { "write" } else { "read" }
                        ),
                    ));
                }
                if let Some(n) = int() {
                    self.holder(!writes).resize(n, None);
                }
            }
            READ_MAX_QUEUE_DEPTH => {
                if let Some(n) = int() {
                    self.read.set_max_queue_depth(n);
                }
            }
            WRITE_MAX_QUEUE_DEPTH => {
                if let Some(n) = int() {
                    self.write.set_max_queue_depth(n);
                }
            }
            STEP_MULTIPLE => {
                if let Some(d) = double() {
                    store_f64(&self.params.step_multiple, d);
                }
            }
            READ_WRITE_RATIO => {
                if let Some(d) = double() {
                    store_f64(&self.params.read_write_ratio, d);
                }
            }
            MOVING_AVERAGE_WEIGHT => {
                if let Some(d) = double() {
                    store_f64(&self.params.moving_average_weight, d);
                }
            }
            MAX_CONCURRENCY => {
                if let Some(n) = int() {
                    self.params.max_concurrency.store(n, Ordering::Relaxed);
                }
            }
            STALL_DETECTION_TIMEOUT_MS => {
                if let Some(n) = int() {
                    self.params
                        .stall_detection_timeout_ms
                        .store(n, Ordering::Relaxed);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// mongod's `ThroughputProbingTicketHolderMonitor` job: a round every
/// `interval`, for as long as the server's control lives.
fn spawn_monitor(probing: Weak<ThroughputProbing>, interval: Duration) {
    let spawned = std::thread::Builder::new()
        .name("ThroughputProbingTicketHolderMonitor".to_string())
        .spawn(move || loop {
            std::thread::sleep(interval);
            let Some(probing) = probing.upgrade() else {
                return;
            };
            probing.run(Instant::now());
        });
    // Without its thread the pools keep their initial sizes, as mongod's
    // do until its first round.
    drop(spawned);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::AdmissionContext;

    fn settings(pairs: &[(&str, Bson)]) -> impl Fn(&str) -> Option<Bson> {
        let pairs: Vec<(String, Bson)> = pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.clone()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn probing_starts_at_twice_the_cores_split_by_the_ratio() {
        // mongod 8.2.11 on this 8-core Mac: 8 read and 8 write tickets.
        let control = ExecutionControl::build(&settings(&[]), 8, None);
        assert_eq!((control.read.outof(), control.write.outof()), (8, 8));
        assert!(!control.supports_runtime_size_adjustment());
        let status = control.server_status();
        assert_eq!(
            status.keys().collect::<Vec<_>>(),
            ["write", "read", "monitor"]
        );
        // Two cores still give the minimum of four each.
        let small = ExecutionControl::build(&settings(&[]), 2, None);
        assert_eq!((small.read.outof(), small.write.outof()), (4, 4));
        // And a ratio favours one side.
        let reads = ExecutionControl::build(
            &settings(&[(READ_WRITE_RATIO, Bson::Double(0.75))]),
            8,
            None,
        );
        assert_eq!((reads.read.outof(), reads.write.outof()), (12, 4));
    }

    #[test]
    fn a_concurrency_given_at_startup_fixes_the_pools() {
        let control =
            ExecutionControl::build(&settings(&[(WRITE_TRANSACTIONS, Bson::Int32(10))]), 8, None);
        assert_eq!((control.read.outof(), control.write.outof()), (128, 10));
        assert!(control.supports_runtime_size_adjustment());
        assert_eq!(
            control.server_status().keys().collect::<Vec<_>>(),
            ["write", "read"]
        );
        control.apply(READ_TRANSACTIONS, &Bson::Int32(7)).unwrap();
        assert_eq!(control.read.outof(), 7);
        let fixed = ExecutionControl::build(
            &settings(&[(
                ALGORITHM,
                Bson::String(FIXED_CONCURRENT_TRANSACTIONS.into()),
            )]),
            8,
            None,
        );
        assert_eq!((fixed.read.outof(), fixed.write.outof()), (128, 128));
    }

    #[test]
    fn probing_refuses_a_concurrency() {
        let control = ExecutionControl::build(&settings(&[]), 8, None);
        let e = control
            .apply(WT_WRITE_TRANSACTIONS, &Bson::Int32(10))
            .unwrap_err();
        assert_eq!(
            (e.code, e.errmsg.as_str()),
            (
                20,
                "Cannot modify concurrent write transactions limit when it is being dynamically \
                 adjusted"
            )
        );
        assert_eq!(control.write.outof(), 8);
    }

    /// Hold `n` tickets of `holder` at once, then return them.
    fn use_tickets(holder: &Arc<TicketHolder>, n: usize) {
        let ctx = Arc::new(AdmissionContext::default());
        let held: Vec<_> = (0..n).filter_map(|_| holder.try_acquire(&ctx)).collect();
        drop(held);
    }

    #[test]
    fn an_exhausted_pool_probes_up_and_keeps_what_raised_throughput() {
        let control = ExecutionControl::build(&settings(&[]), 8, None);
        let probing = control.probing.clone().unwrap();
        let t0 = Instant::now();
        probing.run(t0);
        // Every read ticket in use: probe up by the step, 10% of 8 -> 9.
        use_tickets(&control.read, 8);
        probing.run(t0 + Duration::from_millis(100));
        assert_eq!(probing.probe.lock().unwrap().state, ProbingState::Up);
        assert_eq!((control.read.outof(), control.write.outof()), (9, 9));
        // More finished per microsecond than the baseline: the stable
        // concurrency moves a fifth of the way to the 18 probed.
        use_tickets(&control.read, 9);
        use_tickets(&control.read, 9);
        let t1 = probing.probe.lock().unwrap().timer;
        probing.run(t1 + Duration::from_millis(100));
        let probe = probing.probe.lock().unwrap();
        assert_eq!(probe.state, ProbingState::Stable);
        assert!((probe.stable_concurrency - 16.4).abs() < 1e-9);
        drop(probe);
        // round(16.4 x 0.5) = 8 each.
        assert_eq!((control.read.outof(), control.write.outof()), (8, 8));
        let monitor = probing.stats();
        assert_eq!(monitor.get_i64("timesProbedStable"), Ok(1));
        assert_eq!(monitor.get_i64("timesProbedUp"), Ok(1));
        assert_eq!(monitor.get_i64("timesIncreased"), Ok(1));
    }

    #[test]
    fn a_pool_used_above_the_minimum_probes_down() {
        let control = ExecutionControl::build(&settings(&[]), 8, None);
        let probing = control.probing.clone().unwrap();
        let t0 = Instant::now();
        probing.run(t0);
        use_tickets(&control.write, 6);
        probing.run(t0 + Duration::from_millis(100));
        assert_eq!(probing.probe.lock().unwrap().state, ProbingState::Down);
        // round(16 x 0.5 x 0.9) = 7.
        assert_eq!((control.read.outof(), control.write.outof()), (7, 7));
        // Nothing finished since: throughput fell, so back to stable.
        let t1 = probing.probe.lock().unwrap().timer;
        probing.run(t1 + Duration::from_millis(100));
        assert_eq!((control.read.outof(), control.write.outof()), (8, 8));
        let monitor = probing.stats();
        assert_eq!(monitor.get_i64("timesProbedDown"), Ok(1));
        assert_eq!(monitor.get_i64("timesDecreased"), Ok(0));
    }

    #[test]
    fn an_idle_server_stays_where_it_started() {
        let control = ExecutionControl::build(&settings(&[]), 8, None);
        let probing = control.probing.clone().unwrap();
        let t0 = Instant::now();
        for i in 0..5 {
            probing.run(t0 + Duration::from_millis(100 * i));
        }
        assert_eq!((control.read.outof(), control.write.outof()), (8, 8));
        assert_eq!(probing.stats().get_i64("timesProbedStable"), Ok(4));
    }

    #[test]
    fn a_shrink_waits_for_the_tickets_to_come_back() {
        let holder = Arc::new(TicketHolder::with_policy(
            2,
            10,
            None,
            ResizePolicy::Gradual,
            false,
        ));
        let ctx = Arc::new(AdmissionContext::default());
        let (a, b) = (holder.try_acquire(&ctx), holder.try_acquire(&ctx));
        // None comes back in time: the shrink stops part-way.
        assert!(!holder.resize(1, Some(Instant::now() + Duration::from_millis(20))));
        assert_eq!(holder.outof(), 2);
        let h = holder.clone();
        let shrink = std::thread::spawn(move || h.resize(1, None));
        std::thread::sleep(Duration::from_millis(20));
        drop(a);
        assert!(shrink.join().unwrap());
        assert_eq!((holder.outof(), holder.available()), (1, 0));
        drop(b);
        assert_eq!(holder.available(), 1);
        // Growing hands the tickets out at once.
        assert!(holder.resize(3, None));
        assert_eq!((holder.outof(), holder.available()), (3, 3));
    }

    #[test]
    fn a_ticket_held_past_the_interval_is_delinquent_in_the_operation_too() {
        let control = ExecutionControl::build(
            &settings(&[(DELINQUENT_INTERVAL_MILLIS, Bson::Int32(10))]),
            8,
            None,
        );
        let ctx = Arc::new(AdmissionContext::default());
        let ticket = control.read.try_acquire(&ctx);
        std::thread::sleep(Duration::from_millis(15));
        drop(ticket);
        assert_eq!(ctx.delinquent_acquisitions(), 1);
        assert!(ctx.max_acquisition_delinquency_millis() >= 15);
        let read = control.server_status();
        let normal = read
            .get_document("read")
            .unwrap()
            .get_document("normalPriority")
            .unwrap();
        assert_eq!(normal.get_i32("totalDelinquentAcquisitions"), Ok(1));
    }
}
