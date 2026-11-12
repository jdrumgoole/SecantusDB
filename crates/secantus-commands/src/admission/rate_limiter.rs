//! mongod's connection establishment rate limiter (RM-355).
//!
//! A port of mongod 8.2.11's `admission::RateLimiter` (`db/admission/
//! rate_limiter.cpp`), the thin wrapper it puts around folly's
//! `TokenBucket`, and of the `SessionEstablishmentRateLimiter`
//! (`transport/session_establishment_rate_limiter.cpp`) every ingress
//! session passes through before its first command while
//! `ingressConnectionEstablishmentRateLimiterEnabled` is on.
//!
//! The bucket holds `ratePerSec x burstCapacitySecs` tokens and fills at
//! `ratePerSec`. A connection takes one; with none it borrows against tokens
//! still to come and sleeps until its token is due, while no more than
//! `maxQueueDepth` connections sleep, and past that it is rejected. Its
//! statistics are `serverStatus.queues.ingressSessionEstablishment`, and
//! `serverStatus.connections` carries `queuedForEstablishment` and
//! `establishmentRateLimit`.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use bson::{doc, Bson, Document};

/// The parameters mongod's limiter reads (`transport_options.idl`).
pub const ENABLED: &str = "ingressConnectionEstablishmentRateLimiterEnabled";
pub const RATE_PER_SEC: &str = "ingressConnectionEstablishmentRatePerSec";
pub const BURST_CAPACITY_SECS: &str = "ingressConnectionEstablishmentBurstCapacitySecs";
pub const MAX_QUEUE_DEPTH: &str = "ingressConnectionEstablishmentMaxQueueDepth";

/// How often a connection asleep in the queue checks that its client is
/// still there: mongod's `OperationContext` checks a session it was told to
/// kill on disconnect once a second (`_schedulePeriodicClientConnectedCheck`).
const CLIENT_CHECK_PERIOD: Duration = Duration::from_secs(1);

/// The clock the bucket reads, in seconds (mongod's tick source; folly's
/// bucket takes `nowInSeconds` from its caller).
fn now_in_seconds() -> f64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// folly's `TokenBucket`: a rate, a burst size, and the time at which the
/// bucket would have held no tokens. The balance at `now` is the tokens
/// generated since then, capped at the burst; it goes below zero while
/// consumers have borrowed tokens not yet generated.
#[derive(Debug)]
struct TokenBucket {
    rate: f64,
    burst_size: f64,
    zero_time: f64,
}

impl TokenBucket {
    fn balance(&self, now: f64) -> f64 {
        ((now - self.zero_time) * self.rate).min(self.burst_size)
    }

    fn available(&self, now: f64) -> f64 {
        self.balance(now).max(0.0)
    }

    /// `consume`: take `n` tokens if the bucket holds them.
    fn consume(&mut self, n: f64, now: f64) -> bool {
        let tokens = self.balance(now);
        if tokens < n {
            return false;
        }
        self.zero_time = now - (tokens - n) / self.rate;
        true
    }

    /// `consumeWithBorrowNonBlocking`: take `n` tokens, borrowing what the
    /// bucket does not hold, and answer how long the caller must sleep
    /// until its borrowed tokens have been generated. `None` when `n` is
    /// more than the bucket can ever hold.
    fn consume_with_borrow(&mut self, n: f64, now: f64) -> Option<f64> {
        if self.burst_size < n {
            return None;
        }
        // `consumePartial`: what the bucket holds, unless it is in debt.
        let tokens = self.balance(now);
        let mut owed = n;
        if tokens >= 0.0 {
            let consumed = tokens.min(n);
            self.zero_time = now - (tokens - consumed) / self.rate;
            owed -= consumed;
        }
        if owed > 0.0 {
            // `returnTokensImpl(-owed)`: borrow the rest.
            self.zero_time += owed / self.rate;
            return Some((self.zero_time - now).max(0.0));
        }
        Some(0.0)
    }

    fn return_tokens(&mut self, n: f64) {
        self.zero_time -= n / self.rate;
    }

    /// `reset`: a new rate and burst size, keeping the tokens available now.
    fn reset(&mut self, rate: f64, burst_size: f64, now: f64) {
        let available = self.available(now);
        self.rate = rate;
        self.burst_size = burst_size;
        self.zero_time = now - available / rate;
    }
}

/// mongod's `MovingAverage`: an exponential moving average, with no value
/// until its first sample.
#[derive(Debug)]
struct MovingAverage {
    alpha: f64,
    average: Mutex<Option<f64>>,
}

impl MovingAverage {
    fn add_sample(&self, sample: f64) {
        let mut average = self.average.lock().unwrap_or_else(|e| e.into_inner());
        *average = Some(match *average {
            None => sample,
            Some(previous) => self.alpha * sample + (1.0 - self.alpha) * previous,
        });
    }

    fn get(&self) -> Option<f64> {
        *self.average.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// mongod's `RateLimiter::Stats`.
#[derive(Debug)]
struct Stats {
    added_to_queue: AtomicI64,
    removed_from_queue: AtomicI64,
    interrupted_in_queue: AtomicI64,
    rejected_admissions: AtomicI64,
    successful_admissions: AtomicI64,
    exempted_admissions: AtomicI64,
    attempted_admissions: AtomicI64,
    average_time_queued_micros: MovingAverage,
}

/// Why a token was not acquired (mongod's `RateLimitExceeded`, or the
/// interruption that woke a sleeper).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// The queue was full: mongod's 462 `RateLimitExceeded`.
    Rejected(String),
    /// The caller was interrupted while it slept in the queue.
    Interrupted,
}

/// mongod's `admission::RateLimiter`.
#[derive(Debug)]
pub struct RateLimiter {
    name: String,
    bucket: Mutex<TokenBucket>,
    max_queue_depth: AtomicI64,
    queued: AtomicI64,
    stats: Stats,
}

impl RateLimiter {
    /// A limiter issuing `rate_per_sec` tokens a second, holding
    /// `burst_capacity_secs` seconds of them, full at construction (mongod
    /// sets the bucket's zero time `burst / rate` seconds in the past).
    pub fn new(
        rate_per_sec: f64,
        burst_capacity_secs: f64,
        max_queue_depth: i64,
        name: &str,
    ) -> Self {
        let burst_size = rate_per_sec * burst_capacity_secs;
        RateLimiter {
            name: name.to_string(),
            bucket: Mutex::new(TokenBucket {
                rate: rate_per_sec,
                burst_size,
                zero_time: now_in_seconds() - burst_size / rate_per_sec,
            }),
            max_queue_depth: AtomicI64::new(max_queue_depth),
            queued: AtomicI64::new(0),
            stats: Stats {
                added_to_queue: AtomicI64::new(0),
                removed_from_queue: AtomicI64::new(0),
                interrupted_in_queue: AtomicI64::new(0),
                rejected_admissions: AtomicI64::new(0),
                successful_admissions: AtomicI64::new(0),
                exempted_admissions: AtomicI64::new(0),
                attempted_admissions: AtomicI64::new(0),
                average_time_queued_micros: MovingAverage {
                    alpha: 0.2,
                    average: Mutex::new(None),
                },
            },
        }
    }

    fn bucket(&self) -> std::sync::MutexGuard<'_, TokenBucket> {
        self.bucket.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// mongod's `enqueue`: a place in the queue, unless it is full.
    fn enqueue(&self) -> Result<(), Refused> {
        let max_depth = self.max_queue_depth.load(Ordering::Relaxed);
        let mut expected = self.queued.load(Ordering::SeqCst);
        loop {
            if expected >= max_depth {
                return Err(Refused::Rejected(format!(
                    "Rate limiter '{}' maximum queue depth ({max_depth}) exceeded",
                    self.name
                )));
            }
            match self.queued.compare_exchange(
                expected,
                expected + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Ok(()),
                Err(now) => expected = now,
            }
        }
    }

    /// mongod's `acquireToken`: take a token, or sleep in the queue until
    /// the one borrowed is due. `interrupted` is polled every
    /// [`CLIENT_CHECK_PERIOD`] of the sleep; when it answers `true` the
    /// borrowed token goes back and the caller is refused.
    pub fn acquire_token(&self, interrupted: &mut dyn FnMut() -> bool) -> Result<(), Refused> {
        self.stats
            .attempted_admissions
            .fetch_add(1, Ordering::Relaxed);
        let wait_secs = self
            .bucket()
            .consume_with_borrow(1.0, now_in_seconds())
            .unwrap_or(0.0);
        // mongod sleeps whole milliseconds, truncated (`doubleToMillis`).
        let nap = Duration::from_millis((wait_secs * 1000.0) as u64);
        if nap > Duration::ZERO {
            let deadline = Instant::now() + nap;
            if let Err(refused) = self.enqueue() {
                self.bucket().return_tokens(1.0);
                self.stats
                    .rejected_admissions
                    .fetch_add(1, Ordering::Relaxed);
                return Err(refused);
            }
            self.stats.added_to_queue.fetch_add(1, Ordering::Relaxed);
            let slept = self.sleep_until(deadline, interrupted);
            self.stats
                .removed_from_queue
                .fetch_add(1, Ordering::Relaxed);
            self.queued.fetch_sub(1, Ordering::SeqCst);
            if slept.is_err() {
                self.stats
                    .interrupted_in_queue
                    .fetch_add(1, Ordering::Relaxed);
                self.bucket().return_tokens(1.0);
                return Err(Refused::Interrupted);
            }
        }
        self.stats
            .successful_admissions
            .fetch_add(1, Ordering::Relaxed);
        self.stats
            .average_time_queued_micros
            .add_sample(wait_secs * 1_000_000.0);
        Ok(())
    }

    fn sleep_until(
        &self,
        deadline: Instant,
        interrupted: &mut dyn FnMut() -> bool,
    ) -> Result<(), ()> {
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(());
            }
            std::thread::sleep((deadline - now).min(CLIENT_CHECK_PERIOD));
            if Instant::now() < deadline && interrupted() {
                return Err(());
            }
        }
    }

    /// mongod's `tryAcquireToken`: a token only if one is there now.
    pub fn try_acquire_token(&self) -> Result<(), Refused> {
        self.stats
            .attempted_admissions
            .fetch_add(1, Ordering::Relaxed);
        if !self.bucket().consume(1.0, now_in_seconds()) {
            self.stats
                .rejected_admissions
                .fetch_add(1, Ordering::Relaxed);
            return Err(Refused::Rejected(format!(
                "Rate limiter '{}' rate exceeded",
                self.name
            )));
        }
        self.stats
            .successful_admissions
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// mongod's `recordExemption`: an admission granted without a token.
    pub fn record_exemption(&self) {
        self.stats
            .exempted_admissions
            .fetch_add(1, Ordering::Relaxed);
    }

    /// mongod's `updateRateParameters`.
    pub fn update_rate_parameters(&self, rate_per_sec: f64, burst_capacity_secs: f64) {
        self.bucket().reset(
            rate_per_sec,
            rate_per_sec * burst_capacity_secs,
            now_in_seconds(),
        );
    }

    pub fn set_max_queue_depth(&self, max_queue_depth: i64) {
        self.max_queue_depth
            .store(max_queue_depth, Ordering::Relaxed);
    }

    /// The tokens in the bucket now.
    pub fn tokens_available(&self) -> f64 {
        self.bucket().available(now_in_seconds())
    }

    /// The callers asleep in the queue.
    pub fn queued(&self) -> i64 {
        self.queued.load(Ordering::SeqCst)
    }

    pub fn rejected(&self) -> i64 {
        self.stats.rejected_admissions.load(Ordering::Relaxed)
    }

    pub fn exempted(&self) -> i64 {
        self.stats.exempted_admissions.load(Ordering::Relaxed)
    }

    /// mongod's `appendStats`, field for field (measured 8.2.11: every
    /// count a long, the average and the tokens doubles).
    pub fn stats(&self) -> Document {
        let get = |a: &AtomicI64| Bson::Int64(a.load(Ordering::Relaxed));
        let s = &self.stats;
        let mut out = doc! {
            "addedToQueue": get(&s.added_to_queue),
            "removedFromQueue": get(&s.removed_from_queue),
            "interruptedInQueue": get(&s.interrupted_in_queue),
            "rejectedAdmissions": get(&s.rejected_admissions),
            "exemptedAdmissions": get(&s.exempted_admissions),
            "successfulAdmissions": get(&s.successful_admissions),
            "attemptedAdmissions": get(&s.attempted_admissions),
        };
        if let Some(average) = s.average_time_queued_micros.get() {
            out.insert("averageTimeQueuedMicros", average);
        }
        // FTDC cannot take infinity, so mongod caps the count at INT64_MAX.
        out.insert(
            "totalAvailableTokens",
            self.tokens_available().min(i64::MAX as f64),
        );
        out
    }
}

/// mongod's `SessionEstablishmentRateLimiter`: the [`RateLimiter`] a new
/// ingress session waits on, built with the parameters' values and kept up
/// to date as they are set.
#[derive(Debug)]
pub struct SessionEstablishmentRateLimiter {
    limiter: RateLimiter,
    /// The rate and burst last set, which an update of either one passes
    /// back with the other (mongod reads the other parameter's global).
    rate_per_sec: Mutex<(f64, f64)>,
    interrupted_due_to_client_disconnect: AtomicI64,
}

impl Default for SessionEstablishmentRateLimiter {
    /// With the parameters' defaults: `INT_MAX` a second, `DBL_MAX`
    /// seconds of burst, no queue.
    fn default() -> Self {
        let rate = f64::from(i32::MAX);
        let burst = f64::MAX;
        SessionEstablishmentRateLimiter {
            limiter: RateLimiter::new(rate, burst, 0, "SessionEstablishmentRateLimiter"),
            rate_per_sec: Mutex::new((rate, burst)),
            interrupted_due_to_client_disconnect: AtomicI64::new(0),
        }
    }
}

impl SessionEstablishmentRateLimiter {
    /// Apply a parameter `setParameter` (or startup) set, as its
    /// `on_update` hook does. Other names are not the limiter's.
    pub fn apply(&self, name: &str, value: &Bson) {
        let number = match value {
            Bson::Int32(n) => f64::from(*n),
            Bson::Int64(n) => *n as f64,
            Bson::Double(d) => *d,
            _ => return,
        };
        match name {
            RATE_PER_SEC | BURST_CAPACITY_SECS => {
                let mut params = self.rate_per_sec.lock().unwrap_or_else(|e| e.into_inner());
                if name == RATE_PER_SEC {
                    params.0 = number;
                } else {
                    params.1 = number;
                }
                self.limiter.update_rate_parameters(params.0, params.1);
            }
            MAX_QUEUE_DEPTH => self.limiter.set_max_queue_depth(number as i64),
            _ => {}
        }
    }

    /// mongod's `throttleIfNeeded`: take a token or sleep until one is due.
    /// `disconnected` says whether the client has gone; one that has gone
    /// while it slept is counted and refused.
    pub fn throttle_if_needed(
        &self,
        disconnected: &mut dyn FnMut() -> bool,
    ) -> Result<(), Refused> {
        let mut gone = false;
        let result = self.limiter.acquire_token(&mut || {
            gone = disconnected();
            gone
        });
        if result == Err(Refused::Interrupted) && gone {
            self.interrupted_due_to_client_disconnect
                .fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    /// `serverStatus.queues.ingressSessionEstablishment`.
    pub fn stats_queues(&self) -> Document {
        self.limiter.stats()
    }

    /// What `serverStatus.connections` carries of the limiter (mongod's
    /// `appendStatsConnections`): `queuedForEstablishment` and
    /// `establishmentRateLimit`.
    pub fn append_stats_connections(&self, out: &mut Document) {
        out.insert("queuedForEstablishment", Bson::Int64(self.limiter.queued()));
        out.insert(
            "establishmentRateLimit",
            doc! {
                "rejected": Bson::Int64(self.limiter.rejected()),
                "exempted": Bson::Int64(self.limiter.exempted()),
                "interruptedDueToClientDisconnect": Bson::Int64(
                    self.interrupted_due_to_client_disconnect.load(Ordering::Relaxed),
                ),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the defaults the bucket is bottomless, and mongod prints its
    /// tokens as INT64_MAX (measured 8.2.11: 9.223372036854776e18).
    #[test]
    fn the_default_limiter_reports_int64_max_tokens() {
        let limiter = SessionEstablishmentRateLimiter::default();
        let stats = limiter.stats_queues();
        assert_eq!(stats.get_f64("totalAvailableTokens"), Ok(i64::MAX as f64));
        assert!(!stats.contains_key("averageTimeQueuedMicros"));
        assert_eq!(limiter.throttle_if_needed(&mut || false), Ok(()));
        assert_eq!(
            limiter.stats_queues().get_f64("averageTimeQueuedMicros"),
            Ok(0.0)
        );
    }

    /// A rate of one a second and a second of burst: one token, then a
    /// connection with no queue to wait in is rejected, mongod's words.
    #[test]
    fn past_the_burst_with_no_queue_a_connection_is_rejected() {
        let limiter = SessionEstablishmentRateLimiter::default();
        limiter.apply(RATE_PER_SEC, &Bson::Int32(1));
        limiter.apply(BURST_CAPACITY_SECS, &Bson::Double(1.0));
        assert_eq!(limiter.throttle_if_needed(&mut || false), Ok(()));
        assert_eq!(
            limiter.throttle_if_needed(&mut || false),
            Err(Refused::Rejected(
                "Rate limiter 'SessionEstablishmentRateLimiter' maximum queue depth (0) exceeded"
                    .to_string()
            ))
        );
        let stats = limiter.stats_queues();
        assert_eq!(stats.get_i64("attemptedAdmissions"), Ok(2));
        assert_eq!(stats.get_i64("successfulAdmissions"), Ok(1));
        assert_eq!(stats.get_i64("rejectedAdmissions"), Ok(1));
        // The rejected connection's borrowed token went back.
        assert!(limiter.limiter.tokens_available() < 1.0);
        assert!(limiter.limiter.bucket().balance(now_in_seconds()) > -0.5);
    }

    /// With room in the queue a connection sleeps until its token is due,
    /// counted in and out of the queue.
    #[test]
    fn a_connection_with_room_in_the_queue_waits_its_turn() {
        let limiter = SessionEstablishmentRateLimiter::default();
        limiter.apply(RATE_PER_SEC, &Bson::Int32(20));
        limiter.apply(BURST_CAPACITY_SECS, &Bson::Double(0.05));
        limiter.apply(MAX_QUEUE_DEPTH, &Bson::Int32(1));
        assert_eq!(limiter.throttle_if_needed(&mut || false), Ok(()));
        let started = Instant::now();
        assert_eq!(limiter.throttle_if_needed(&mut || false), Ok(()));
        assert!(started.elapsed() >= Duration::from_millis(40));
        let stats = limiter.stats_queues();
        assert_eq!(stats.get_i64("addedToQueue"), Ok(1));
        assert_eq!(stats.get_i64("removedFromQueue"), Ok(1));
        assert_eq!(stats.get_i64("successfulAdmissions"), Ok(2));
        assert!(stats.get_f64("averageTimeQueuedMicros").unwrap() > 0.0);
    }

    /// A client that goes while it waits leaves the queue, counted as
    /// interrupted by its disconnect, and its token goes back.
    #[test]
    fn a_client_that_disconnects_in_the_queue_is_counted() {
        let limiter = SessionEstablishmentRateLimiter::default();
        limiter.apply(RATE_PER_SEC, &Bson::Int32(1));
        limiter.apply(BURST_CAPACITY_SECS, &Bson::Double(1.0));
        limiter.apply(MAX_QUEUE_DEPTH, &Bson::Int32(1));
        assert_eq!(limiter.throttle_if_needed(&mut || false), Ok(()));
        // A token borrowed by a connection already asleep: the next one's is
        // ~2 s away, and its client goes at the first check, after ~1 s.
        limiter.limiter.bucket().return_tokens(-1.0);
        assert_eq!(
            limiter.throttle_if_needed(&mut || true),
            Err(Refused::Interrupted)
        );
        let stats = limiter.stats_queues();
        assert_eq!(stats.get_i64("interruptedInQueue"), Ok(1));
        assert_eq!(stats.get_i64("successfulAdmissions"), Ok(1));
        let mut connections = Document::new();
        limiter.append_stats_connections(&mut connections);
        assert_eq!(
            connections
                .get_document("establishmentRateLimit")
                .unwrap()
                .get_i64("interruptedDueToClientDisconnect"),
            Ok(1)
        );
        assert_eq!(connections.get_i64("queuedForEstablishment"), Ok(0));
    }
}
