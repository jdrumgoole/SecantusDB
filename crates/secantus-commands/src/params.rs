//! `setParameter` — the runtime-settable half of the server parameters.
//!
//! [`get_parameter`](crate::serverinfo::get_parameter) reports the parameters
//! this server has; this module is what lets a client CHANGE the ones mongod
//! lets you change at runtime, and refuse the rest the way mongod refuses them.
//!
//! Every code and message below was measured against mongod 8.2.11 on
//! 2026-09-29 (a standalone and a single-node replica set answered identically):
//!
//! | request | answer |
//! | --- | --- |
//! | a settable parameter | `ok: 1` plus `was`, the PREVIOUS value |
//! | a parameter that exists but is startup-only | 20 `IllegalOperation` "not allowed to change [X] at runtime" |
//! | a name the server does not register | 72 `InvalidOptions` "attempted to set unrecognized parameter [X], use help:true to see options " |
//! | no parameter at all | 72 `InvalidOptions` "no option found to set, use help:true to see options " |
//! | a settable parameter, wrong type | 2 `BadValue` "Invalid value for X: ..." |
//! | any of the above outside `admin` | 13 `Unauthorized` "setParameter may only be run against the admin database." |
//!
//! **We deliberately do NOT register a parameter we do not honour.** The
//! `ingressConnectionEstablishment*` family that mongo-go-driver's
//! `TestConnectionPoolBackpressure` sets is real on mongod and tunes a
//! connection rate limiter SecantusDB has no equivalent of. Accepting those
//! names would let a client ask for rate limiting and silently get none, which
//! is the "half-implemented feature that silently diverges" this project
//! prefers an honest refusal to. A server that does not register them answers
//! 72, and so do we.

use std::collections::BTreeMap;
use std::sync::Mutex;

use bson::{Bson, Document};

use crate::{CommandContext, CommandError, HandlerResult};

/// Trailing space included — mongod emits one, and a driver that compares the
/// message verbatim sees it.
const HELP_SUFFIX: &str = ", use help:true to see options ";

/// MongoDB's *generic command arguments* — the envelope every command may
/// carry — which are NOT parameter names.
///
/// Filtering only `$`-prefixed keys is not enough, and the gap is not
/// theoretical: `pymongo` attaches an `lsid` to every command it sends, so
/// `{setParameter: 1, logLevel: 0}` arrived as three keys and the handler
/// rejected the whole call with "unrecognized parameter [lsid]". Every real
/// driver call would have failed while a unit test built from a bare document
/// passed. Found by the differential probe on 2026-09-29.
const GENERIC_ARGS: &[&str] = &[
    "lsid",
    "txnNumber",
    "autocommit",
    "startTransaction",
    "stmtId",
    "readConcern",
    "writeConcern",
    "maxTimeMS",
    "comment",
    "apiVersion",
    "apiStrict",
    "apiDeprecationErrors",
];

/// Whether `key` is envelope rather than a parameter the client wants to set.
pub(crate) fn is_generic_arg(key: &str) -> bool {
    key.starts_with('$') || GENERIC_ARGS.contains(&key)
}

/// Parameters a client may change at runtime, with the type each accepts.
///
/// Keep this in lockstep with `serverinfo::parameters`: a name that is
/// settable here must be reported there, or `getParameter` will not show the
/// value a client just set.
///
/// The two kinds behave very differently, and both were measured rather than
/// assumed (8.2.11, 2026-09-29) because the obvious guess is wrong for each:
///
///   * `LogLevel` COERCES any number or bool, truncating toward zero
///     (`1.9` -> `1`, `-0.5` -> `0`, `true` -> `1`) and CLAMPING the top end
///     (`6` and `99` both store `5`). It rejects only string / array /
///     document / null. A negative that does not truncate to zero (`-1`) is
///     rejected rather than clamped.
///   * `Bool` accepts **every** BSON type and stores its truthiness. `null`,
///     `0`, `0.0` and `false` are false; an EMPTY STRING, an array and a
///     document are all true.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// mongod's `logLevel`: a clamped 0..=5 integer with wide coercion.
    LogLevel,
    /// A boolean parameter, which mongod coerces from any BSON value.
    Bool,
    /// A whole number of seconds greater than zero (`ttlMonitorSleepSecs`).
    PositiveInt,
    /// Any 64-bit whole number (`cursorTimeoutMillis`): a bool or a number,
    /// truncated toward zero.
    Int64,
    /// A 32-bit whole number of at least the given value
    /// (`transactionLifetimeLimitSeconds` 1, the SCRAM iteration counts 5000).
    Int32AtLeast(i32),
}

/// `(name, kind)` for every runtime-settable parameter.
const SETTABLE: &[(&str, ParamKind)] = &[
    ("logLevel", ParamKind::LogLevel),
    // Refuse a query the planner would answer with a collection scan
    // (`crate::notablescan`); read on every query, so it is also kept in an
    // atomic beside the stored value.
    ("notablescan", ParamKind::Bool),
    ("quiet", ParamKind::Bool),
    // The TTL monitor, which these reach in the storage layer as well as
    // being stored here: a test sets the sleep to 1 to see a document expire
    // without waiting a minute.
    ("ttlMonitorEnabled", ParamKind::Bool),
    ("ttlMonitorSleepSecs", ParamKind::PositiveInt),
    // The two registries' limits: `cursorTimeoutMillis` for an idle cursor,
    // `transactionLifetimeLimitSeconds` for an idle transaction. Each is
    // changed in the registry as well as stored here.
    ("cursorTimeoutMillis", ParamKind::Int64),
    (
        "transactionLifetimeLimitSeconds",
        ParamKind::Int32AtLeast(1),
    ),
    // The iteration counts `createUser` / `updateUser` write into the
    // credentials they build from then on (`auth::scram_credentials`); a
    // credential already stored keeps its own count. mongod 8.2.11 takes
    // either at 5000 or more (measured 2026-10-20).
    ("scramIterationCount", ParamKind::Int32AtLeast(5000)),
    ("scramSHA256IterationCount", ParamKind::Int32AtLeast(5000)),
];

/// mongod's highest log level; `setParameter` clamps rather than refusing.
const MAX_LOG_LEVEL: i32 = 5;

/// When a session was last used, and whose it is (the SHA-256 of
/// `user@db`).
type SessionUse = (bson::DateTime, Vec<u8>);

/// Server-wide store for the settable parameters' current values.
///
/// Only holds the ones that have been CHANGED; an unset parameter reads its
/// default from `known_params`, so the two cannot drift apart by construction.
#[derive(Default)]
pub struct ServerParams {
    values: Mutex<BTreeMap<String, Bson>>,
    /// Sticky: a user has existed on this server since it started. mongod closes
    /// its localhost exception for the life of the PROCESS once any user
    /// exists -- deleting every user does not reopen it until a restart
    /// (measured 8.2.11, 2026-09-30).
    users_seen: std::sync::atomic::AtomicBool,
    /// Bumped by every user and role command, so that a connection's cached
    /// effective privileges (`rbac::effective`) are recomputed after one.
    user_cache_generation: std::sync::atomic::AtomicU64,
    /// The sessions commands have come in, each with when it was last used
    /// and whose it is (the SHA-256 of `user@db`, `session::user_digest`):
    /// what `$listLocalSessions` lists. One unused for the session timeout
    /// (30 minutes) is forgotten.
    sessions: Mutex<BTreeMap<Vec<u8>, SessionUse>>,
    /// The `config.settings` read/write-concern defaults document, read once
    /// and kept so a write need not look it up (`crate::rwconcern`). The
    /// outer `None` is "not read yet"; the inner one is "no document".
    rw_defaults: Mutex<Option<Option<Document>>>,
    /// `notablescan`, read by every query: kept here as well as in `values`
    /// so the read takes no lock.
    notablescan: std::sync::atomic::AtomicBool,
    /// The feature compatibility version, once read from
    /// `admin.system.version` (`cluster_admin::current_fcv`) or set.
    fcv: Mutex<Option<String>>,
}

impl ServerParams {
    pub fn new() -> Self {
        Self::default()
    }

    /// A user or role changed: every connection recomputes its privileges
    /// on its next command (mongod's `invalidateUserCache`).
    pub fn invalidate_user_cache(&self) {
        self.user_cache_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn user_cache_generation(&self) -> u64 {
        self.user_cache_generation
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether a query the planner would answer with a collection scan is
    /// refused (the `notablescan` parameter).
    pub fn notablescan(&self) -> bool {
        self.notablescan.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Record that a user exists (see `users_seen`).
    pub fn mark_users_seen(&self) {
        self.users_seen
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// A command came in the session `id`, run by the user whose digest is
    /// `uid`.
    pub fn note_session(&self, id: &[u8], uid: Vec<u8>) {
        if let Ok(mut sessions) = self.sessions.lock() {
            let now = bson::DateTime::now();
            let oldest = now.timestamp_millis() - 30 * 60 * 1000;
            sessions.retain(|_, (used, _)| used.timestamp_millis() >= oldest);
            sessions.insert(id.to_vec(), (now, uid));
        }
    }

    /// The sessions seen and not timed out: the id, when each was last
    /// used, and whose it is.
    pub fn sessions(&self) -> Vec<(Vec<u8>, bson::DateTime, Vec<u8>)> {
        let oldest = bson::DateTime::now().timestamp_millis() - 30 * 60 * 1000;
        self.sessions.lock().map_or_else(
            |_| Vec::new(),
            |sessions| {
                sessions
                    .iter()
                    .filter(|(_, (used, _))| used.timestamp_millis() >= oldest)
                    .map(|(id, (used, uid))| (id.clone(), *used, uid.clone()))
                    .collect()
            },
        )
    }

    /// The feature compatibility version, if it has been read or set.
    pub fn fcv(&self) -> Option<String> {
        self.fcv.lock().ok()?.clone()
    }

    pub fn set_fcv(&self, version: &str) {
        if let Ok(mut guard) = self.fcv.lock() {
            *guard = Some(version.to_string());
        }
    }

    /// Whether a user has existed since the server started.
    pub fn users_seen(&self) -> bool {
        self.users_seen.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The cached read/write-concern defaults document: `None` when it has
    /// not been read since the last change to `config.settings`.
    pub fn rw_defaults_cached(&self) -> Option<Option<Document>> {
        self.rw_defaults.lock().ok()?.clone()
    }

    pub fn cache_rw_defaults(&self, settings: Option<Document>) {
        if let Ok(mut guard) = self.rw_defaults.lock() {
            *guard = Some(settings);
        }
    }

    pub fn forget_rw_defaults(&self) {
        if let Ok(mut guard) = self.rw_defaults.lock() {
            *guard = None;
        }
    }

    /// The current value, or `None` when it has never been set.
    pub fn get(&self, name: &str) -> Option<Bson> {
        self.values.lock().ok()?.get(name).cloned()
    }

    /// The SCRAM-SHA-1 and SCRAM-SHA-256 iteration counts `setParameter`
    /// set, each `None` while it holds its default.
    pub fn scram_iterations(&self) -> (Option<u32>, Option<u32>) {
        let count = |name| match self.get(name) {
            Some(Bson::Int32(n)) => u32::try_from(n).ok(),
            _ => None,
        };
        (
            count("scramIterationCount"),
            count("scramSHA256IterationCount"),
        )
    }

    /// Store `value`, returning nothing — the caller already holds the previous
    /// value for `was`.
    fn set(&self, name: &str, value: Bson) {
        if let Ok(mut guard) = self.values.lock() {
            guard.insert(name.to_string(), value);
        }
    }

    /// Overlay the changed values onto a `known_params`-shaped document.
    pub fn overlay(&self, params: &mut Document) {
        if let Ok(guard) = self.values.lock() {
            for (k, v) in guard.iter() {
                params.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Whether a client may change `name` while the server runs.
pub(crate) fn settable(name: &str) -> bool {
    kind_of(name).is_some()
}

fn kind_of(name: &str) -> Option<ParamKind> {
    SETTABLE.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
}

/// The value mongod would STORE for `value`, or `None` when it refuses it.
fn coerce(kind: ParamKind, value: &Bson) -> Option<Bson> {
    match kind {
        ParamKind::LogLevel => {
            let n: f64 = match value {
                Bson::Int32(i) => *i as f64,
                Bson::Int64(i) => *i as f64,
                Bson::Double(d) => *d,
                Bson::Boolean(b) => {
                    if *b {
                        1.0
                    } else {
                        0.0
                    }
                }
                _ => return None,
            };
            // Truncate toward zero first, THEN reject a negative: -0.5 lands on
            // 0 and is accepted, while -1 is refused.
            let truncated = n.trunc();
            if truncated < 0.0 {
                return None;
            }
            Some(Bson::Int32(
                (truncated as i64).min(MAX_LOG_LEVEL as i64) as i32
            ))
        }
        ParamKind::Int64 => integral(value, false).ok().map(Bson::Int64),
        ParamKind::Int32AtLeast(min) => match integral(value, true) {
            Ok(n) if n >= i64::from(min) => Some(Bson::Int32(n as i32)),
            _ => None,
        },
        ParamKind::PositiveInt => match whole(value) {
            Some(n) if n > 0 => Some(Bson::Int32(n.min(i64::from(i32::MAX)) as i32)),
            _ => None,
        },
        // Accepts every BSON type; only these four are falsey.
        ParamKind::Bool => Some(Bson::Boolean(
            !matches!(
                value,
                Bson::Boolean(false) | Bson::Null | Bson::Int32(0) | Bson::Int64(0)
            ) && *value != Bson::Double(0.0),
        )),
    }
}

/// A number truncated toward zero, or `None` for anything that is not one.
fn whole(value: &Bson) -> Option<i64> {
    match value {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) if d.is_finite() => Some(d.trunc() as i64),
        _ => None,
    }
}

/// A value as mongod coerces it to a whole number for a parameter: a bool
/// is 0 or 1, a number is truncated toward zero, and each failure has its
/// own words (measured 8.2.11, 2026-10-15).
fn integral(value: &Bson, int32: bool) -> Result<i64, &'static str> {
    let n = match value {
        Bson::Boolean(b) => return Ok(i64::from(*b)),
        Bson::Int32(i) => return Ok(i64::from(*i)),
        Bson::Int64(i) if int32 && i32::try_from(*i).is_err() => {
            return Err("Out of bounds coercing to integral value")
        }
        Bson::Int64(i) => return Ok(*i),
        Bson::Double(d) => *d,
        Bson::Decimal128(d) => d
            .to_string()
            .parse::<f64>()
            .map_err(|_| "Unable to coerce value to integral type")?,
        _ => return Err("Unable to coerce value to integral type"),
    };
    if !n.is_finite() {
        return Err("Unable to coerce NaN/Inf to integral type");
    }
    let n = n.trunc();
    let (low, high) = if int32 {
        (f64::from(i32::MIN), f64::from(i32::MAX))
    } else {
        (i64::MIN as f64, i64::MAX as f64)
    };
    if n < low || n >= high + 1.0 {
        return Err("Out of bounds coercing to integral value");
    }
    Ok(n as i64)
}

/// mongod's words for a value `coerce` refused (measured 8.2.11).
fn refusal(kind: ParamKind, name: &str, value: &Bson) -> String {
    let min = match kind {
        ParamKind::Int64 => Some(None),
        ParamKind::Int32AtLeast(min) => Some(Some(min)),
        _ => None,
    };
    if let Some(min) = min {
        return match integral(value, min.is_some()) {
            Err(why) => format!("Failed validating {name}: {why}"),
            Ok(n) => format!(
                "Invalid value for parameter {name}: {n} is not greater than or equal to {}",
                min.unwrap_or(1)
            ),
        };
    }
    match (kind, whole(value)) {
        (ParamKind::PositiveInt, Some(n)) => {
            format!("Invalid value for parameter {name}: {n} is not greater than 0")
        }
        (ParamKind::PositiveInt, None) => {
            format!("Failed validating {name}: Unable to coerce value to integral type")
        }
        _ => format!("Invalid value for {name}: {}", render_element(name, value)),
    }
}

/// mongod renders the offending element shell-style in its "Invalid value"
/// message -- `logLevel: "nope"`, `logLevel: [ 1, 2 ]`, `logLevel: { a: 1 }`,
/// `logLevel: null` -- so a driver comparing the message verbatim sees the
/// spacing too.
fn render_element(name: &str, value: &Bson) -> String {
    format!("{name}: {}", render_value(value))
}

fn render_value(value: &Bson) -> String {
    match value {
        Bson::String(s) => format!("{s:?}"),
        Bson::Null => "null".to_string(),
        Bson::Boolean(b) => b.to_string(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(d) => {
            if d.fract() == 0.0 && d.is_finite() {
                format!("{d:.1}")
            } else {
                d.to_string()
            }
        }
        Bson::Array(a) => {
            if a.is_empty() {
                "[]".to_string()
            } else {
                let inner: Vec<String> = a.iter().map(render_value).collect();
                format!("[ {} ]", inner.join(", "))
            }
        }
        Bson::Document(d) => {
            if d.is_empty() {
                "{}".to_string()
            } else {
                let inner: Vec<String> = d.iter().map(|(k, v)| render_element(k, v)).collect();
                format!("{{ {} }}", inner.join(", "))
            }
        }
        other => format!("{other:?}"),
    }
}

/// `setParameter` — change a runtime-settable server parameter.
pub fn set_parameter(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    if ctx.db_name != "admin" {
        return Err(CommandError::new(
            13,
            "Unauthorized",
            "setParameter may only be run against the admin database.",
        ));
    }
    // Everything but the command name and the `$`-prefixed wire metadata is a
    // parameter the client wants to set.
    let requested: Vec<(&String, &Bson)> = doc
        .iter()
        .filter(|(k, _)| k.as_str() != "setParameter" && !is_generic_arg(k))
        .collect();
    if requested.is_empty() {
        return Err(CommandError::new(
            72,
            "InvalidOptions",
            format!("no option found to set{HELP_SUFFIX}"),
        ));
    }

    let defaults = crate::serverinfo::known_params(ctx.failpoints.is_some());
    // mongod validates every named parameter BEFORE applying any of them, so a
    // batch that names one bad parameter changes nothing.
    for (name, value) in &requested {
        // A parameter this server reports and does not let a client change.
        // Answering 20 and not 72 matters: 72 claims the parameter does not
        // exist, and `getParameter` would immediately contradict that. (mongod
        // lets some of these be changed while it runs; this server does not,
        // and says so in mongod's words for a parameter it will not change.)
        if !settable(name) && defaults.contains_key(name.as_str()) {
            return Err(CommandError::new(
                20,
                "IllegalOperation",
                format!("not allowed to change [{name}] at runtime"),
            ));
        }
        let Some(kind) = kind_of(name) else {
            return Err(CommandError::new(
                72,
                "InvalidOptions",
                format!("attempted to set unrecognized parameter [{name}]{HELP_SUFFIX}"),
            ));
        };
        if coerce(kind, value).is_none() {
            return Err(CommandError::new(2, "BadValue", refusal(kind, name, value)));
        }
    }

    let store = ctx.server_params.clone();
    let mut out = Document::new();
    for (i, (name, value)) in requested.iter().enumerate() {
        let previous = crate::serverinfo::ttl_monitor_param(ctx, name)
            .or_else(|| store.as_ref().and_then(|s| s.get(name)))
            .or_else(|| defaults.get(name.as_str()).cloned())
            .unwrap_or(Bson::Null);
        // Store what mongod would store, not what the client sent, so
        // `getParameter` reports the coerced value (`logLevel: 99` reads back
        // as 5, `quiet: "yes"` as true). The setting takes effect whether or
        // not there is a store to remember it in.
        if let Some(coerced) = coerce(kind_of(name).expect("validated above"), value) {
            if let Ok(storage) = ctx.storage() {
                match (name.as_str(), &coerced) {
                    ("ttlMonitorSleepSecs", Bson::Int32(n)) => {
                        storage.set_ttl_monitor(Some(i64::from(*n)), None)
                    }
                    ("ttlMonitorEnabled", Bson::Boolean(on)) => {
                        storage.set_ttl_monitor(None, Some(*on))
                    }
                    _ => {}
                }
            }
            match (name.as_str(), &coerced) {
                ("cursorTimeoutMillis", Bson::Int64(ms)) => {
                    if let Ok(cursors) = ctx.cursors() {
                        cursors.set_cursor_timeout_millis(*ms);
                    }
                }
                ("transactionLifetimeLimitSeconds", Bson::Int32(secs)) => {
                    if let Some(transactions) = &ctx.transactions {
                        transactions.set_lifetime_seconds(f64::from(*secs));
                    }
                }
                ("notablescan", Bson::Boolean(on)) => {
                    if let Some(s) = store.as_ref() {
                        s.notablescan
                            .store(*on, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                _ => {}
            }
            if let Some(s) = store.as_ref() {
                s.set(name, coerced);
            }
        }
        // mongod reports a single `was`, the FIRST named parameter's previous
        // value, even when several are set in one call (measured: setting
        // `logLevel` and `quiet` together reported `was: 0`, logLevel's).
        if i == 0 {
            out.insert("was", previous);
        }
    }
    out.insert("ok", 1.0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn ctx() -> CommandContext {
        let mut c = CommandContext::new(1);
        c.db_name = "admin".to_string();
        c.server_params = Some(std::sync::Arc::new(ServerParams::new()));
        c
    }

    #[test]
    fn sets_a_settable_parameter_and_reports_the_previous_value() {
        let mut c = ctx();
        let r = set_parameter(&doc! {"setParameter": 1, "logLevel": 3_i32}, &mut c).unwrap();
        assert_eq!(r.get_i32("was").unwrap(), 0);
        // and it sticks
        let r2 = set_parameter(&doc! {"setParameter": 1, "logLevel": 5_i32}, &mut c).unwrap();
        assert_eq!(r2.get_i32("was").unwrap(), 3);
    }

    /// The SCRAM iteration counts (mongod 8.2.11, 2026-10-20): settable at
    /// runtime, an int32 of at least 5000, a double truncated.
    #[test]
    fn the_scram_iteration_counts_are_settable_from_5000() {
        let mut c = ctx();
        for name in ["scramIterationCount", "scramSHA256IterationCount"] {
            assert!(settable(name));
            let r = set_parameter(&doc! {"setParameter": 1, name: 12000.7}, &mut c).unwrap();
            assert!(r.contains_key("was"));
            assert_eq!(
                c.server_params.as_ref().unwrap().get(name),
                Some(Bson::Int32(12000))
            );
            let e = set_parameter(&doc! {"setParameter": 1, name: 4999}, &mut c).unwrap_err();
            assert_eq!(
                (e.code, e.errmsg),
                (
                    2,
                    format!(
                        "Invalid value for parameter {name}: 4999 is not greater than or equal to 5000"
                    )
                )
            );
            let e =
                set_parameter(&doc! {"setParameter": 1, name: 1_i64 << 31}, &mut c).unwrap_err();
            assert_eq!(
                e.errmsg,
                format!("Failed validating {name}: Out of bounds coercing to integral value")
            );
        }
        assert_eq!(
            c.server_params.as_ref().unwrap().scram_iterations(),
            (Some(12000), Some(12000))
        );
    }

    /// The TTL monitor's two parameters (mongod 8.2.11, 2026-10-10). Both
    /// were "unrecognized", so a test could not shorten the monitor's sleep.
    #[test]
    fn the_ttl_monitor_parameters_are_settable() {
        let mut c = ctx();
        let r = set_parameter(
            &doc! {"setParameter": 1, "ttlMonitorSleepSecs": 1_i32},
            &mut c,
        )
        .unwrap();
        assert_eq!(r.get_i32("was").unwrap(), 60);
        let r = set_parameter(
            &doc! {"setParameter": 1, "ttlMonitorEnabled": false},
            &mut c,
        )
        .unwrap();
        assert!(r.get_bool("was").unwrap());

        let e = set_parameter(
            &doc! {"setParameter": 1, "ttlMonitorSleepSecs": "x"},
            &mut c,
        )
        .unwrap_err();
        assert_eq!(e.code, 2);
        assert_eq!(
            e.errmsg,
            "Failed validating ttlMonitorSleepSecs: Unable to coerce value to integral type"
        );
        let e = set_parameter(
            &doc! {"setParameter": 1, "ttlMonitorSleepSecs": 0_i32},
            &mut c,
        )
        .unwrap_err();
        assert_eq!(e.code, 2);
        assert_eq!(
            e.errmsg,
            "Invalid value for parameter ttlMonitorSleepSecs: 0 is not greater than 0"
        );
    }

    #[test]
    fn unrecognized_parameter_is_invalid_options() {
        let mut c = ctx();
        let e = set_parameter(&doc! {"setParameter": 1, "nope": 1_i32}, &mut c).unwrap_err();
        assert_eq!(e.code, 72);
        assert_eq!(
            e.errmsg,
            "attempted to set unrecognized parameter [nope], use help:true to see options "
        );
    }

    #[test]
    fn startup_only_parameter_is_illegal_operation_not_unrecognized() {
        // 72 would claim the parameter does not exist, which `getParameter`
        // reporting it would immediately contradict.
        let mut c = ctx();
        let e = set_parameter(
            &doc! {"setParameter": 1, "enableTestCommands": true},
            &mut c,
        )
        .unwrap_err();
        assert_eq!(e.code, 20);
        assert_eq!(
            e.errmsg,
            "not allowed to change [enableTestCommands] at runtime"
        );
    }

    #[test]
    fn the_session_envelope_is_not_mistaken_for_a_parameter() {
        // pymongo attaches `lsid` to every command; before this filter existed
        // that made EVERY real driver call fail with "unrecognized parameter
        // [lsid]" while a unit test built from a bare document passed.
        let mut c = ctx();
        let r = set_parameter(
            &doc! {
                "setParameter": 1,
                "logLevel": 2_i32,
                "lsid": doc! {"id": "x"},
                "$db": "admin",
                "$clusterTime": doc! {},
                "writeConcern": doc! {"w": 1_i32},
            },
            &mut c,
        )
        .expect("the envelope must be ignored");
        assert_eq!(r.get_i32("was").unwrap(), 0);
        assert_eq!(
            c.server_params.as_ref().unwrap().get("logLevel"),
            Some(Bson::Int32(2))
        );
    }

    #[test]
    fn an_envelope_only_call_still_reports_no_option_found() {
        // ... and the envelope must not be counted as "a parameter was named".
        let mut c = ctx();
        let e =
            set_parameter(&doc! {"setParameter": 1, "lsid": doc! {"id": "x"}}, &mut c).unwrap_err();
        assert_eq!(e.code, 72);
        assert_eq!(
            e.errmsg,
            "no option found to set, use help:true to see options "
        );
    }

    #[test]
    fn no_parameter_named_at_all() {
        let mut c = ctx();
        let e = set_parameter(&doc! {"setParameter": 1}, &mut c).unwrap_err();
        assert_eq!(e.code, 72);
        assert_eq!(
            e.errmsg,
            "no option found to set, use help:true to see options "
        );
    }

    #[test]
    fn wrong_type_is_bad_value_and_renders_the_element_shell_style() {
        // The exact strings mongod 8.2.11 emits, measured 2026-09-29 -- the
        // spacing inside the array and document braces included.
        for (value, rendered) in [
            (Bson::String("nope".into()), "logLevel: \"nope\""),
            (
                Bson::Array(vec![Bson::Int32(1), Bson::Int32(2)]),
                "logLevel: [ 1, 2 ]",
            ),
            (Bson::Document(doc! {"a": 1_i32}), "logLevel: { a: 1 }"),
            (Bson::Null, "logLevel: null"),
        ] {
            let mut c = ctx();
            let e =
                set_parameter(&doc! {"setParameter": 1, "logLevel": value}, &mut c).unwrap_err();
            assert_eq!(e.code, 2);
            // The parameter name really does appear TWICE -- once in the
            // sentence and once as the rendered element's key.
            assert_eq!(e.errmsg, format!("Invalid value for logLevel: {rendered}"));
        }
    }

    #[test]
    fn log_level_coerces_and_clamps_rather_than_refusing() {
        // Every one of these is ACCEPTED by mongod; the obvious guess (an int
        // parameter takes only ints) is wrong four ways.
        for (input, stored) in [
            (Bson::Int32(3), 3_i32),
            (Bson::Double(1.9), 1),  // truncates toward zero
            (Bson::Double(-0.5), 0), // ... so a small negative lands on zero
            (Bson::Boolean(true), 1),
            (Bson::Boolean(false), 0),
            (Bson::Int32(6), 5), // clamped, not refused
            (Bson::Int32(99), 5),
        ] {
            let mut c = ctx();
            set_parameter(&doc! {"setParameter": 1, "logLevel": input.clone()}, &mut c)
                .unwrap_or_else(|e| panic!("{input:?} should be accepted: {}", e.errmsg));
            assert_eq!(
                c.server_params.as_ref().unwrap().get("logLevel"),
                Some(Bson::Int32(stored)),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn a_negative_log_level_that_does_not_truncate_to_zero_is_refused() {
        let mut c = ctx();
        let e = set_parameter(&doc! {"setParameter": 1, "logLevel": -1_i32}, &mut c).unwrap_err();
        assert_eq!(e.code, 2);
        assert_eq!(e.errmsg, "Invalid value for logLevel: logLevel: -1");
    }

    #[test]
    fn a_bool_parameter_accepts_every_type_and_stores_truthiness() {
        // mongod refuses NOTHING here -- an empty string, an array and a
        // document are all true; only false / null / 0 / 0.0 are false.
        for (input, stored) in [
            (Bson::Boolean(true), true),
            (Bson::Boolean(false), false),
            (Bson::Int32(1), true),
            (Bson::Int32(0), false),
            (Bson::Double(0.0), false),
            (Bson::String("yes".into()), true),
            (Bson::String(String::new()), true),
            (Bson::Null, false),
            (Bson::Array(vec![Bson::Int32(1)]), true),
            (Bson::Document(doc! {"a": 1_i32}), true),
        ] {
            let mut c = ctx();
            set_parameter(&doc! {"setParameter": 1, "quiet": input.clone()}, &mut c)
                .unwrap_or_else(|e| panic!("{input:?} should be accepted: {}", e.errmsg));
            assert_eq!(
                c.server_params.as_ref().unwrap().get("quiet"),
                Some(Bson::Boolean(stored)),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn only_on_the_admin_database() {
        let mut c = ctx();
        c.db_name = "test".to_string();
        let e = set_parameter(&doc! {"setParameter": 1, "logLevel": 0_i32}, &mut c).unwrap_err();
        assert_eq!(e.code, 13);
        assert_eq!(
            e.errmsg,
            "setParameter may only be run against the admin database."
        );
    }

    #[test]
    fn a_bad_name_in_a_batch_changes_nothing() {
        // mongod validates the whole batch first; a partially applied set would
        // leave the server in a state the client never asked for.
        let mut c = ctx();
        let e = set_parameter(
            &doc! {"setParameter": 1, "logLevel": 4_i32, "nope": 1_i32},
            &mut c,
        )
        .unwrap_err();
        assert_eq!(e.code, 72);
        assert_eq!(c.server_params.as_ref().unwrap().get("logLevel"), None);
    }

    #[test]
    fn notablescan_is_read_without_the_lock() {
        let mut c = ctx();
        let params = c.server_params.clone().unwrap();
        assert!(!params.notablescan());
        let r = set_parameter(&doc! {"setParameter": 1, "notablescan": "yes"}, &mut c).unwrap();
        assert!(!r.get_bool("was").unwrap());
        assert!(params.notablescan());
        assert_eq!(params.get("notablescan"), Some(Bson::Boolean(true)));
        set_parameter(&doc! {"setParameter": 1, "notablescan": Bson::Null}, &mut c).unwrap();
        assert!(!params.notablescan());
    }

    #[test]
    fn overlay_shows_the_changed_value_to_get_parameter() {
        let mut c = ctx();
        set_parameter(&doc! {"setParameter": 1, "quiet": true}, &mut c).unwrap();
        let mut params = crate::serverinfo::known_params(false);
        c.server_params.as_ref().unwrap().overlay(&mut params);
        assert!(params.get_bool("quiet").unwrap());
    }
}
