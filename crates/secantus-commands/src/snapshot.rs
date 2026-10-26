//! Snapshot reads at a point in time (RM-210): `readConcern: {level:
//! "snapshot"}` outside a transaction, with or without `atClusterTime`, and
//! the `getMore`s of the cursors such a read opens. mongod reads them all at
//! one timestamp from WiredTiger's history; so does this server, on a store
//! that keeps one (its data tables are not logged, see
//! `Storage::snapshot_clock`).

use std::cell::Cell;
use std::time::Duration;

use bson::{Bson, Document};

use crate::{CommandContext, CommandError};

thread_local! {
    /// The timestamp the command running on this thread reads at.
    static READ_AT: Cell<Option<bson::Timestamp>> = const { Cell::new(None) };
}

/// The timestamp the command running on this thread reads at, if any: what a
/// cursor it opens keeps for its `getMore`s, and what its reply reports.
pub(crate) fn current() -> Option<bson::Timestamp> {
    READ_AT.with(|c| c.get())
}

/// The commands a snapshot read concern makes read at a timestamp.
fn reads_at(name: &str) -> bool {
    matches!(name, "find" | "aggregate" | "distinct")
}

/// The snapshot read concern of a command outside a transaction, if it has
/// one.
pub(crate) fn snapshot_concern(doc: &Document) -> Option<&Document> {
    if doc.get("autocommit") == Some(&Bson::Boolean(false)) {
        return None;
    }
    let rc = doc.get_document("readConcern").ok()?;
    (rc.get_str("level").ok() == Some("snapshot")).then_some(rc)
}

/// The timestamp `doc` reads at: its `atClusterTime`, else every write
/// committed so far -- or, for a `getMore`, the time its cursor was opened
/// at. `None` when it reads the latest data.
///
/// An `atClusterTime` waits until every write at or below it has committed,
/// so a read at a time answers the same however often it is made. A
/// transaction here takes its timestamps as its statements run, not at
/// commit as mongod's does, so a read at a time after one of them waits for
/// the transaction to end (RM-314); when `maxTimeMS` or `killOp` ends the
/// wait the read fails rather than read what that commit would change.
fn read_point(
    doc: &Document,
    name: &str,
    ctx: &CommandContext,
) -> Result<Option<bson::Timestamp>, CommandError> {
    if name == "getMore" {
        let id = doc.get("getMore").and_then(crate::util::as_i64);
        return Ok(id.and_then(|id| ctx.cursors.as_ref()?.read_at(id)));
    }
    if !reads_at(name) {
        return Ok(None);
    }
    let Some(rc) = snapshot_concern(doc) else {
        return Ok(None);
    };
    let Some(storage) = ctx.storage.as_ref() else {
        return Ok(None);
    };
    let Some((committed, _)) = storage.snapshot_clock() else {
        return Ok(None);
    };
    let at = match rc.get("atClusterTime") {
        Some(Bson::Timestamp(t)) => *t,
        _ => return Ok(Some(committed)),
    };
    while storage.snapshot_clock().is_some_and(|(c, _)| c < at) {
        if crate::ops::current_killed() {
            return Err(CommandError::new(
                11601,
                "Interrupted",
                "operation was interrupted",
            ));
        }
        secantus_core::deadline::check_now().map_err(|_| {
            CommandError::new(50, "MaxTimeMSExpired", "operation exceeded time limit")
        })?;
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(Some(at))
}

/// Run `f` -- a command's handler -- at the timestamp `doc` reads at, if it
/// reads at one; that timestamp comes back beside the result, unless the
/// reply should not report it: mongod's `find` of a namespace that was
/// neither a collection nor a view at that time reads nothing, and says
/// nothing of when (measured 8.2.11, 2026-10-25; `aggregate` and `distinct`
/// still report it).
pub(crate) fn run<T>(
    doc: &Document,
    name: &str,
    ctx: &mut CommandContext,
    f: impl FnOnce(&mut CommandContext) -> Result<T, CommandError>,
) -> (Result<T, CommandError>, Option<bson::Timestamp>) {
    let at = match read_point(doc, name, ctx) {
        Ok(Some(at)) => at,
        Ok(None) => return (f(ctx), None),
        Err(e) => return (Err(e), None),
    };
    let Some(storage) = ctx.storage.clone() else {
        return (f(ctx), None);
    };
    struct Restore(Option<bson::Timestamp>);
    impl Drop for Restore {
        fn drop(&mut self) {
            READ_AT.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(READ_AT.with(|c| c.replace(Some(at))));
    let mut f = Some(f);
    let mut out = None;
    let mut reported = true;
    let ran = storage.read_at(at, &mut || {
        if let Some(f) = f.take() {
            out = Some(f(ctx));
        }
        if name == "find" {
            if let Ok(coll) = doc.get_str("find") {
                let db = ctx.db_name.clone();
                reported = storage.collection_exists(&db, coll).unwrap_or(true)
                    || crate::views::is_view(storage.as_ref(), &db, coll);
            }
        }
    });
    let result = match (ran, out) {
        (Err(e), _) => Err(crate::util::command_error(e)),
        (Ok(()), Some(out)) => out,
        (Ok(()), None) => Err(CommandError::new(
            1,
            "InternalError",
            "snapshot read did not run",
        )),
    };
    (result, reported.then_some(at))
}

/// Report the time a snapshot read read at, where mongod reports it: in the
/// cursor document of a cursor reply, else just before `ok`.
pub(crate) fn place_at_cluster_time(reply: &mut Document, ts: bson::Timestamp) {
    match reply.get_mut("cursor") {
        Some(Bson::Document(cur)) => {
            if !cur.contains_key("atClusterTime") {
                cur.insert("atClusterTime", Bson::Timestamp(ts));
            }
        }
        _ => {
            if !reply.contains_key("atClusterTime") {
                let mut out = Document::new();
                for (k, v) in std::mem::take(reply) {
                    if k == "ok" {
                        out.insert("atClusterTime", Bson::Timestamp(ts));
                    }
                    out.insert(k, v);
                }
                *reply = out;
            }
        }
    }
}

/// mongod's checks of an `atClusterTime`, in its order: a null one, one
/// later than any time this server has handed out, and one older than the
/// history kept (239). A store that keeps no history reads only the latest
/// data, and refuses one in the past by name rather than answer it as of
/// now.
pub(crate) fn check_at(rc: &Document, ctx: &CommandContext) -> Result<(), CommandError> {
    let Some(Bson::Timestamp(at)) = rc.get("atClusterTime") else {
        return Ok(());
    };
    if at.time == 0 {
        return Err(CommandError::new(
            72,
            "InvalidOptions",
            "atClusterTime cannot be a null timestamp",
        ));
    }
    let Some(storage) = ctx.storage.as_ref() else {
        return Ok(());
    };
    let current = storage.peek_cluster_time();
    if *at > current {
        return Err(CommandError::new(
            72,
            "InvalidOptions",
            format!(
                "readConcern atClusterTime value must not be greater than the current \
                 clusterTime. Requested clusterTime: {{ ts: Timestamp({}, {}) }}; current \
                 clusterTime: {{ ts: Timestamp({}, {}) }}",
                at.time, at.increment, current.time, current.increment
            ),
        ));
    }
    let too_old = || {
        CommandError::new(
            239,
            "SnapshotTooOld",
            format!(
                "Read timestamp Timestamp({}, {}) is older than the oldest available timestamp.",
                at.time, at.increment
            ),
        )
    };
    match storage.snapshot_clock() {
        Some((_, oldest)) => {
            if *at < oldest {
                return Err(too_old());
            }
        }
        None => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            if u64::from(at.time) + 300 < now {
                return Err(too_old());
            }
            if *at < current {
                return Err(CommandError::new(
                    246,
                    "SnapshotUnavailable",
                    format!(
                        "Cannot read at Timestamp({}, {}): this store's data tables are \
                         logged, and keep no history to read a past time from. A store \
                         secantusd-rs creates keeps it.",
                        at.time, at.increment
                    ),
                ));
            }
        }
    }
    Ok(())
}
