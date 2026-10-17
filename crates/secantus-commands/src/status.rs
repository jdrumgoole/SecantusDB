//! The counters behind `serverStatus`'s `asserts`, `opLatencies`,
//! `globalLock.activeClients`, `security` and `metrics.{commands, document,
//! operation, query}`, and the rules that move them.
//!
//! Every rule here was measured against mongod 8.2.11 (2026-10-15), by
//! running an operation and reading which of its counters moved:
//!
//! - `metrics.commands.<name>.total` counts each run of a command, `failed`
//!   each one that answered `ok: 0`; a name mongod does not know counts in
//!   `<UNKNOWN>`. `update` / `findAndModify` / `bulkWrite` also count the
//!   statements that carry `arrayFilters` and the ones that are a pipeline;
//!   `create` / `collMod` count a `validator` (and its `$jsonSchema`).
//! - `metrics.document`: `inserted` is what an insert put in, `updated` what
//!   an update MATCHED (a no-op update still counts, an upsert's insert does
//!   not count at all), `deleted` what a delete removed, `returned` the
//!   documents a reply hands back (a cursor's batch, a `count`'s one, a
//!   `distinct`'s values, a `findAndModify`'s document).
//! - `opLatencies`: `find` / `getMore` / `aggregate` / `count` / `distinct`
//!   are reads, the four write commands and `bulkWrite` are writes, the rest
//!   are commands; microseconds, and one op per command.
//! - `asserts.user` counts each error a client is told of: a failed command,
//!   and each write error of one that did not fail. A command mongod does
//!   not know is not one. The counter is mongod's 30-bit one, and `rollovers`
//!   counts its wraps.
//! - `metrics.query.{update,delete}ManyCount` count the multi-document
//!   statements that RAN (an ordered batch stopped before one does not count
//!   it), and `updateDeleteMany*` their documents and milliseconds.
//!
//! - A command whose fields do not parse (a wrong type, an unknown field) is
//!   not a run of it: mongod times it as a command and counts nothing else
//!   but the assertion.
//!
//! Where this differs from mongod: mongod raises some errors twice inside
//! (an invalid sort, an index build, an unordered insert's duplicate), so
//! its `asserts.user` can move by two where this one moves by one; an
//! `aggregate` whose pipeline does not parse is not a run of `aggregate`
//! there and is here; `$out` / `$merge` writes are not counted in
//! `metrics.document`; a `bulkWrite`'s multi-document update counts its
//! documents in `updateDeleteManyDocumentsTotalCount` here and not there.
//!
//! The section a counter of mongod's would need a mechanism this server does
//! not have (a plan cache, query settings that reject) is not reported: a
//! zero standing in for something not counted is a lie. (`locks` joined the
//! reported sections when the lock manager landed — RM-034, 2026-10-17.)

use std::sync::atomic::{AtomicI64, Ordering::Relaxed};
use std::time::Duration;

use bson::{doc, Bson, Document};

use crate::mongod_commands::COMMANDS;

/// The three kinds of operation `opLatencies` (and `activeClients`) splits
/// commands into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Read,
    Write,
    Command,
}

impl OpKind {
    pub fn of(name: &str) -> OpKind {
        match name {
            "find" | "getMore" | "aggregate" | "count" | "distinct" => OpKind::Read,
            "insert" | "update" | "delete" | "findAndModify" | "findandmodify" | "bulkWrite" => {
                OpKind::Write
            }
            _ => OpKind::Command,
        }
    }
}

/// The authentication mechanisms this server runs, in mongod's order.
pub const MECHANISMS: [&str; 3] = ["MONGODB-X509", "SCRAM-SHA-1", "SCRAM-SHA-256"];

fn mechanism_index(name: &str) -> Option<usize> {
    MECHANISMS.iter().position(|m| *m == name)
}

#[derive(Default)]
struct CommandSlot {
    total: AtomicI64,
    failed: AtomicI64,
    array_filters: AtomicI64,
    pipeline: AtomicI64,
    validator_total: AtomicI64,
    validator_failed: AtomicI64,
    validator_json_schema: AtomicI64,
}

/// mongod's assertion counters wrap at 2^30.
const ASSERT_ROLLOVER: i64 = 1 << 30;

/// Server-wide `serverStatus` counters. Every bump is a relaxed atomic add:
/// they sit on the path of every command.
pub struct StatusCounters {
    commands: Box<[CommandSlot]>,
    unknown: AtomicI64,
    /// `(micros, ops)` for reads, writes, commands.
    latency: [[AtomicI64; 2]; 3],
    /// deleted, inserted, returned, updated.
    document: [AtomicI64; 4],
    asserts_user: AtomicI64,
    max_time_expired: AtomicI64,
    /// deleteManyCount, updateDeleteManyDocumentsMaxCount,
    /// updateDeleteManyDocumentsTotalCount, updateDeleteManyDurationMaxMs,
    /// updateDeleteManyDurationTotalMs, updateManyCount.
    many: [AtomicI64; 6],
    /// Reads and writes running right now.
    active: [AtomicI64; 2],
    /// retriedCommandsCount, retriedStatementsCount.
    retried: [AtomicI64; 2],
    sasl_supported_mechs: AtomicI64,
    /// Per mechanism: authentications received, and succeeded.
    auth: [[AtomicI64; 2]; 3],
    auth_micros: AtomicI64,
}

impl std::fmt::Debug for StatusCounters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StatusCounters")
    }
}

impl Default for StatusCounters {
    fn default() -> Self {
        StatusCounters {
            commands: COMMANDS.iter().map(|_| CommandSlot::default()).collect(),
            unknown: AtomicI64::new(0),
            latency: Default::default(),
            document: Default::default(),
            asserts_user: AtomicI64::new(0),
            max_time_expired: AtomicI64::new(0),
            many: Default::default(),
            active: Default::default(),
            retried: Default::default(),
            sasl_supported_mechs: AtomicI64::new(0),
            auth: Default::default(),
            auth_micros: AtomicI64::new(0),
        }
    }
}

/// Marks a read or write as running until it is dropped, so a command that
/// panics does not stay "active" for ever.
pub struct Active<'a> {
    slot: Option<&'a AtomicI64>,
}

impl Drop for Active<'_> {
    fn drop(&mut self) {
        if let Some(slot) = self.slot {
            slot.fetch_sub(1, Relaxed);
        }
    }
}

/// The `COMMANDS` row a request names: its own name, or the canonical name
/// of a lower-case alias (`ismaster`, `findandmodify`).
fn command_slot(name: &str) -> Option<usize> {
    COMMANDS
        .binary_search_by(|(known, _, _)| (*known).cmp(name))
        .ok()
        .or_else(|| {
            COMMANDS
                .iter()
                .position(|(known, _, _)| known.eq_ignore_ascii_case(name))
        })
}

fn len_of(doc: &Document, path: &[&str]) -> i64 {
    let mut at = doc;
    for key in &path[..path.len() - 1] {
        match at.get_document(key) {
            Ok(d) => at = d,
            Err(_) => return 0,
        }
    }
    at.get_array(path[path.len() - 1])
        .map_or(0, |a| a.len() as i64)
}

fn int_field(doc: &Document, key: &str) -> i64 {
    crate::util::as_i64_field(doc, key)
}

impl StatusCounters {
    /// Count `kind` as running until the guard drops.
    pub fn activate(&self, kind: OpKind) -> Active<'_> {
        let slot = match kind {
            OpKind::Read => &self.active[0],
            OpKind::Write => &self.active[1],
            OpKind::Command => return Active { slot: None },
        };
        slot.fetch_add(1, Relaxed);
        Active { slot: Some(slot) }
    }

    /// Count one finished command: `request` was answered with `reply`, plus
    /// `pending` documents handed to the wire outside the reply.
    pub fn record(&self, request: &Document, reply: &Document, pending: usize, elapsed: Duration) {
        let Some((name, _)) = request.iter().next() else {
            return;
        };
        let name = name.as_str();
        let ok = reply.get_f64("ok").unwrap_or(0.0) == 1.0;
        let code = reply.get_i32("code").unwrap_or(0);
        let micros = elapsed.as_micros().min(i64::MAX as u128) as i64;
        // A command that did not parse never ran: mongod times it as a
        // command and does not count it as a run of its name.
        let parsed = !crate::util::did_not_parse(reply);
        let kind = match if parsed {
            OpKind::of(name)
        } else {
            OpKind::Command
        } {
            OpKind::Read => 0,
            OpKind::Write => 1,
            OpKind::Command => 2,
        };
        self.latency[kind][0].fetch_add(micros, Relaxed);
        self.latency[kind][1].fetch_add(1, Relaxed);

        if !ok && code == 59 {
            self.unknown.fetch_add(1, Relaxed);
            return;
        }
        if let Some(i) = command_slot(name).filter(|_| parsed) {
            self.count_command(&self.commands[i], name, request, ok);
        }
        if !ok {
            self.assert_user(1);
            if code == 50 {
                self.max_time_expired.fetch_add(1, Relaxed);
            }
            return;
        }
        self.assert_user(len_of(reply, &["writeErrors"]));
        self.count_documents(name, request, reply, pending as i64);
        let retried = reply
            .get_array("retriedStmtIds")
            .map(|ids| ids.len() as i64)
            .unwrap_or_else(|_| i64::from(reply.contains_key("retriedStmtId")));
        if retried > 0 {
            self.retried[0].fetch_add(1, Relaxed);
            self.retried[1].fetch_add(retried, Relaxed);
        }
    }

    fn count_command(&self, slot: &CommandSlot, name: &str, request: &Document, ok: bool) {
        slot.total.fetch_add(1, Relaxed);
        if !ok {
            slot.failed.fetch_add(1, Relaxed);
        }
        let statements: Vec<(bool, bool)> = match name {
            "update" => request
                .get_array("updates")
                .map(|all| {
                    all.iter()
                        .filter_map(Bson::as_document)
                        .map(|s| {
                            (
                                s.contains_key("arrayFilters"),
                                matches!(s.get("u"), Some(Bson::Array(_))),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            "findAndModify" | "findandmodify" => vec![(
                request.contains_key("arrayFilters"),
                matches!(request.get("update"), Some(Bson::Array(_))),
            )],
            "bulkWrite" => request
                .get_array("ops")
                .map(|all| {
                    all.iter()
                        .filter_map(Bson::as_document)
                        .map(|s| {
                            (
                                s.contains_key("arrayFilters"),
                                matches!(s.get("updateMods"), Some(Bson::Array(_))),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        for (array_filters, pipeline) in statements {
            if array_filters {
                slot.array_filters.fetch_add(1, Relaxed);
            }
            if pipeline {
                slot.pipeline.fetch_add(1, Relaxed);
            }
        }
        if matches!(name, "create" | "collMod" | "collmod") {
            if let Ok(validator) = request.get_document("validator") {
                slot.validator_total.fetch_add(1, Relaxed);
                if !ok {
                    slot.validator_failed.fetch_add(1, Relaxed);
                }
                if validator.contains_key("$jsonSchema") {
                    slot.validator_json_schema.fetch_add(1, Relaxed);
                }
            }
        }
    }

    fn count_documents(&self, name: &str, request: &Document, reply: &Document, pending: i64) {
        let [deleted, inserted, returned, updated] = &self.document;
        match name {
            "insert" => {
                inserted.fetch_add(int_field(reply, "n"), Relaxed);
            }
            "update" => {
                let upserted = len_of(reply, &["upserted"]);
                updated.fetch_add(int_field(reply, "n") - upserted, Relaxed);
            }
            "delete" => {
                deleted.fetch_add(int_field(reply, "n"), Relaxed);
            }
            "findAndModify" | "findandmodify" => {
                let last = reply.get_document("lastErrorObject").ok();
                let n = last.map_or(0, |l| int_field(l, "n"));
                if matches!(request.get("remove"), Some(Bson::Boolean(true))) {
                    deleted.fetch_add(n, Relaxed);
                } else if last.is_some_and(|l| l.get_bool("updatedExisting").unwrap_or(false)) {
                    updated.fetch_add(n, Relaxed);
                }
                if matches!(reply.get("value"), Some(Bson::Document(_))) {
                    returned.fetch_add(1, Relaxed);
                }
            }
            "bulkWrite" => {
                inserted.fetch_add(int_field(reply, "nInserted"), Relaxed);
                updated.fetch_add(int_field(reply, "nMatched"), Relaxed);
                deleted.fetch_add(int_field(reply, "nDeleted"), Relaxed);
                returned.fetch_add(len_of(reply, &["cursor", "firstBatch"]) + pending, Relaxed);
            }
            "find" | "aggregate" => {
                returned.fetch_add(len_of(reply, &["cursor", "firstBatch"]) + pending, Relaxed);
            }
            "getMore" => {
                returned.fetch_add(len_of(reply, &["cursor", "nextBatch"]) + pending, Relaxed);
            }
            "count" => {
                returned.fetch_add(1, Relaxed);
            }
            "distinct" => {
                returned.fetch_add(len_of(reply, &["values"]), Relaxed);
            }
            _ => {}
        }
    }

    fn assert_user(&self, n: i64) {
        if n > 0 {
            self.asserts_user.fetch_add(n, Relaxed);
        }
    }

    /// One multi-document update (`update: true`) or delete statement ran,
    /// over `documents` documents, in `elapsed`.
    pub fn many_statement(&self, update: bool, documents: u64, elapsed: Duration) {
        let documents = documents.min(i64::MAX as u64) as i64;
        let millis = elapsed.as_millis().min(i64::MAX as u128) as i64;
        let [delete_many, docs_max, docs_total, millis_max, millis_total, update_many] = &self.many;
        if update {
            update_many.fetch_add(1, Relaxed);
        } else {
            delete_many.fetch_add(1, Relaxed);
        }
        docs_max.fetch_max(documents, Relaxed);
        docs_total.fetch_add(documents, Relaxed);
        millis_max.fetch_max(millis, Relaxed);
        millis_total.fetch_add(millis, Relaxed);
    }

    /// A `hello` asked for a user's `saslSupportedMechs`.
    pub fn sasl_supported_mechs_received(&self) {
        self.sasl_supported_mechs.fetch_add(1, Relaxed);
    }

    /// An authentication with `mechanism` began.
    pub fn authentication_received(&self, mechanism: &str) {
        if let Some(i) = mechanism_index(mechanism) {
            self.auth[i][0].fetch_add(1, Relaxed);
        }
    }

    /// An authentication with `mechanism` ended after `elapsed`, and whether
    /// it succeeded.
    pub fn authentication_ended(&self, mechanism: &str, succeeded: bool, elapsed: Duration) {
        if let Some(i) = mechanism_index(mechanism) {
            if succeeded {
                self.auth[i][1].fetch_add(1, Relaxed);
            }
        }
        let micros = elapsed.as_micros().min(i64::MAX as u128) as i64;
        self.auth_micros.fetch_add(micros, Relaxed);
    }

    /// Reads and writes running right now.
    pub fn active_clients(&self) -> (i64, i64) {
        (self.active[0].load(Relaxed), self.active[1].load(Relaxed))
    }

    /// `asserts`: only the user assertions, which are the ones this server
    /// raises; mongod's `regular` / `warning` / `msg` / `tripwire` count its
    /// internal invariants.
    pub fn asserts(&self) -> Document {
        let user = self.asserts_user.load(Relaxed);
        doc! {
            "user": (user % ASSERT_ROLLOVER) as i32,
            "rollovers": (user / ASSERT_ROLLOVER) as i32,
        }
    }

    /// `opLatencies`, with the transactions' `(micros, ops)`.
    pub fn op_latencies(&self, transactions: Option<(i64, i64)>) -> Document {
        let entry = |slot: &[AtomicI64; 2]| {
            doc! {
                "latency": Bson::Int64(slot[0].load(Relaxed)),
                "ops": Bson::Int64(slot[1].load(Relaxed)),
            }
        };
        let mut out = doc! {
            "commands": entry(&self.latency[2]),
            "reads": entry(&self.latency[0]),
            "writes": entry(&self.latency[1]),
        };
        if let Some((micros, ops)) = transactions {
            out.insert(
                "transactions",
                doc! { "latency": Bson::Int64(micros), "ops": Bson::Int64(ops) },
            );
        }
        out
    }

    /// `metrics.commands`: every command this server runs, in mongod's
    /// (byte) order, after the commands it does not know.
    pub fn commands(&self) -> Document {
        let mut out = doc! { "<UNKNOWN>": Bson::Int64(self.unknown.load(Relaxed)) };
        for (i, (name, _, _)) in COMMANDS.iter().enumerate() {
            if crate::lookup(name).is_none() {
                continue;
            }
            let slot = &self.commands[i];
            let load = |a: &AtomicI64| Bson::Int64(a.load(Relaxed));
            let entry = match *name {
                "update" | "findAndModify" | "bulkWrite" => doc! {
                    "arrayFilters": load(&slot.array_filters),
                    "failed": load(&slot.failed),
                    "pipeline": load(&slot.pipeline),
                    "total": load(&slot.total),
                },
                "create" | "collMod" => doc! {
                    "failed": load(&slot.failed),
                    "total": load(&slot.total),
                    "validator": {
                        "failed": load(&slot.validator_failed),
                        "jsonSchema": load(&slot.validator_json_schema),
                        "total": load(&slot.validator_total),
                    },
                },
                _ => doc! { "failed": load(&slot.failed), "total": load(&slot.total) },
            };
            out.insert(*name, entry);
        }
        out
    }

    /// `metrics.document`.
    pub fn document(&self) -> Document {
        let [deleted, inserted, returned, updated] = &self.document;
        doc! {
            "deleted": Bson::Int64(deleted.load(Relaxed)),
            "inserted": Bson::Int64(inserted.load(Relaxed)),
            "returned": Bson::Int64(returned.load(Relaxed)),
            "updated": Bson::Int64(updated.load(Relaxed)),
        }
    }

    /// `metrics.operation`: the operations a `maxTimeMS` ended.
    pub fn operation(&self) -> Document {
        doc! { "killedDueToMaxTimeMSExpired": Bson::Int64(self.max_time_expired.load(Relaxed)) }
    }

    /// `metrics.query`: the multi-document updates and deletes.
    pub fn query(&self) -> Document {
        let names = [
            "deleteManyCount",
            "updateDeleteManyDocumentsMaxCount",
            "updateDeleteManyDocumentsTotalCount",
            "updateDeleteManyDurationMaxMs",
            "updateDeleteManyDurationTotalMs",
            "updateManyCount",
        ];
        names
            .iter()
            .zip(&self.many)
            .map(|(name, n)| (name.to_string(), Bson::Int64(n.load(Relaxed))))
            .collect()
    }

    /// `transactions.retriedCommandsCount` / `retriedStatementsCount`.
    pub fn retried(&self) -> (i64, i64) {
        (self.retried[0].load(Relaxed), self.retried[1].load(Relaxed))
    }

    /// `security`: the authentications this server ran, by mechanism.
    pub fn security(&self) -> Document {
        let mut mechanisms = Document::new();
        for (name, slot) in MECHANISMS.iter().zip(&self.auth) {
            mechanisms.insert(
                *name,
                doc! {
                    "authenticate": {
                        "received": Bson::Int64(slot[0].load(Relaxed)),
                        "successful": Bson::Int64(slot[1].load(Relaxed)),
                    },
                },
            );
        }
        doc! {
            "authentication": {
                "saslSupportedMechsReceived": Bson::Int64(self.sasl_supported_mechs.load(Relaxed)),
                "mechanisms": mechanisms,
                "totalAuthenticationTimeMicros": Bson::Int64(self.auth_micros.load(Relaxed)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_counts_by_its_canonical_name() {
        let s = StatusCounters::default();
        s.record(
            &doc! {"findandmodify": "c", "update": [{"$set": {"a": 1}}]},
            &doc! {"lastErrorObject": {"n": 1, "updatedExisting": true}, "value": {"_id": 1}, "ok": 1.0},
            0,
            Duration::from_micros(5),
        );
        let fam = s.commands().get_document("findAndModify").unwrap().clone();
        assert_eq!(fam.get_i64("total").unwrap(), 1);
        assert_eq!(fam.get_i64("pipeline").unwrap(), 1);
        let d = s.document();
        assert_eq!(
            (
                d.get_i64("updated").unwrap(),
                d.get_i64("returned").unwrap()
            ),
            (1, 1)
        );
    }

    #[test]
    fn an_unknown_command_is_not_a_user_assertion() {
        let s = StatusCounters::default();
        let reply = doc! {"ok": 0.0, "code": 59, "codeName": "CommandNotFound", "errmsg": "x"};
        s.record(&doc! {"nosuch": 1}, &reply, 0, Duration::ZERO);
        assert_eq!(s.commands().get_i64("<UNKNOWN>").unwrap(), 1);
        assert_eq!(s.asserts().get_i32("user").unwrap(), 0);
    }

    #[test]
    fn the_user_assertion_counter_rolls_over_as_mongods_does() {
        let s = StatusCounters::default();
        s.assert_user(ASSERT_ROLLOVER + 3);
        assert_eq!(s.asserts(), doc! {"user": 3, "rollovers": 1});
    }

    #[test]
    fn an_active_guard_releases_on_drop() {
        let s = StatusCounters::default();
        {
            let _r = s.activate(OpKind::Read);
            let _w = s.activate(OpKind::Write);
            let _c = s.activate(OpKind::Command);
            assert_eq!(s.active_clients(), (1, 1));
        }
        assert_eq!(s.active_clients(), (0, 0));
    }
}
