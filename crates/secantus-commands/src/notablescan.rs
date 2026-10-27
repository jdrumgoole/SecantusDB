//! The `notablescan` server parameter: refuse a query that would scan a
//! whole collection.
//!
//! mongod decides it in its query planner. Set, a query whose plan reads the
//! collection rather than an index fails with 291 `NoQueryExecutionPlans`.
//! Here the plan is the one `explain` derives from mongod's planning rules
//! (`crate::explain::plans_table_scan`), asked just before the read.
//!
//! What mongod 8.2.11 does (measured 2026-10-15, a one-member replica set):
//!
//! - `find`, `count`, `distinct`, `aggregate` (the query its leading stages
//!   make), `update`, `delete`, `findAndModify` and `explain` of each are
//!   refused. A write's refusal is the statement's `writeErrors` entry.
//! - An EMPTY filter is never refused, sorted or not; nor is a read of a
//!   collection that does not exist, of `admin`, `local` or `config`, or of
//!   a `system.*` collection. A filter that only looks empty (`{$comment:
//!   "x"}`, `{$and: [{}]}`) is refused.
//! - A hinted index is used, whatever the filter. A `{$natural: ±1}` hint
//!   with a filter is refused in words of its own.
//!
//! The message: mongod prints its parse of the query between the namespace
//! and the reason (`ns=db.cTree: b $eq 1\nSort: {}\nProj: {}\n planner
//! returned error ...`), reproduced by `explain::debug_string` (2026-10-20,
//! RM-035): what each command shows of its paging, projection and collation
//! is the read's [`Shown`].
//!
//! Not refused here, though mongod refuses them: a `$match` mongod moves to
//! the front of a pipeline from behind another stage (`$project`, `$unwind`,
//! `$addFields`, through a `$group`'s key), and the reads a `$lookup`,
//! `$graphLookup` or `$unionWith` makes of another collection. And a query
//! that names a field of an index the planner does not read (hashed, text,
//! geo, wildcard) is let through, because the planner cannot say whether
//! mongod would use that index.

use bson::{Bson, Document};
use secantus_core::collation::Collation;

use crate::explain::TableScan;
use crate::storage::Storage;
use crate::{CommandContext, CommandError};

/// One read, as the planner is asked about it.
pub(crate) struct Read<'a> {
    pub coll: &'a str,
    pub filter: &'a Document,
    /// The sort the plan has to return (a find's, a findAndModify's).
    pub sort: Option<&'a Document>,
    pub hint: Option<&'a Bson>,
    pub collation: Option<&'a Collation>,
    /// What the refusal prints of the read besides its filter and sort.
    pub shown: Shown<'a>,
}

/// What mongod's refusal prints of a read besides its filter and sort,
/// measured per command (8.2.11, 2026-10-20): a `find`'s projection, skip,
/// limit and collation as given; `findAndModify`'s `limit=1` when it
/// updates; an aggregate's resolved collation, `simple` when it names none.
/// `count`, `distinct`, `update` and `delete` print none of it.
#[derive(Default, Clone)]
pub(crate) struct Shown<'a> {
    pub projection: Option<&'a Document>,
    pub skip: Option<i64>,
    pub limit: Option<i64>,
    pub collation: Option<Document>,
    /// A `find`'s `batchSize`.
    pub batch_size: Option<i64>,
}

impl Read<'_> {
    /// The query mongod's planner says it was asked, for a refusal of this
    /// read of `db`.
    pub(crate) fn canonical(&self, db: &str) -> crate::explain::CanonicalQuery<'_> {
        crate::explain::CanonicalQuery {
            ns: format!("{db}.{}", self.coll),
            filter: self.filter,
            sort: self.sort,
            projection: self.shown.projection,
            skip: self.shown.skip,
            limit: self.shown.limit,
            collation: self.shown.collation.as_ref(),
            batch_size: self.shown.batch_size,
        }
    }

    /// Refuse a hint naming no index, in mongod's planner's words.
    pub(crate) fn check_hint(&self, storage: &dyn Storage, db: &str) -> Result<(), CommandError> {
        crate::util::validate_hint(storage, db, self.coll, self.hint, &self.canonical(db))
    }
}

impl<'a> Shown<'a> {
    /// An aggregate's: the collation it runs under, resolved.
    pub(crate) fn aggregate(collation: Option<&Collation>) -> Shown<'a> {
        Shown {
            collation: Some(
                collation.map_or_else(|| bson::doc! {"locale": "simple"}, |c| c.spec().clone()),
            ),
            ..Shown::default()
        }
    }
}

/// Whether the parameter is set on this server.
pub(crate) fn enabled(ctx: &CommandContext) -> bool {
    ctx.server_params.as_ref().is_some_and(|p| p.notablescan())
}

/// mongod never refuses these: an empty query, and the internal databases
/// and system collections.
fn exempt(db: &str, coll: &str, filter: &Document) -> bool {
    filter.is_empty() || matches!(db, "admin" | "local" | "config") || coll.starts_with("system.")
}

/// mongod's refusal of a read of `db.coll` with `filter`.
pub(crate) fn refusal(
    db: &str,
    coll: &str,
    filter: &Document,
    sort: Option<&Document>,
    shown: &Shown,
    why: TableScan,
) -> CommandError {
    let reason = match why {
        TableScan::Unindexed => "No indexed plans available, and running with 'notablescan' 2",
        TableScan::Natural => {
            "could not force a collection scan with a $natural hint :: caused by :: not allowed \
             to output a collection scan because 'notablescan' is enabled"
        }
    };
    let query = crate::explain::CanonicalQuery {
        ns: format!("{db}.{coll}"),
        filter,
        sort,
        projection: shown.projection,
        skip: shown.skip,
        limit: shown.limit,
        collation: shown.collation.as_ref(),
        batch_size: shown.batch_size,
    };
    CommandError::new(291, "NoQueryExecutionPlans", query.planner_error(reason))
}

/// The fields `filter` tests, at any depth of `$and` / `$or` / `$nor`, and
/// the fields its `$expr`s read (into `in_expr`).
fn tested_fields<'a>(filter: &'a Document, out: &mut Vec<&'a str>, in_expr: &mut Vec<&'a str>) {
    for (key, value) in filter {
        if !key.starts_with('$') {
            out.push(key);
            continue;
        }
        if key == "$expr" {
            expr_fields(value, in_expr);
            continue;
        }
        if let Bson::Array(items) = value {
            for item in items {
                if let Bson::Document(d) = item {
                    tested_fields(d, out, in_expr);
                }
            }
        }
    }
}

/// The fields an `$expr` compares with a constant, which mongod rewrites
/// into predicates an index can serve (`$_internalExprEq` and its kin):
/// `{$eq|$gt|$gte|$lt|$lte: ["$path", <constant or $$var>]}`, either way
/// round, alone or under an `$and`. A comparison through another
/// expression (`{$add: ["$a", 1]}`), or by `$ne`, is not rewritten.
fn expr_fields<'a>(expr: &'a Bson, out: &mut Vec<&'a str>) {
    let Bson::Document(d) = expr else {
        return;
    };
    let Some((op, arg)) = d.iter().next().filter(|_| d.len() == 1) else {
        return;
    };
    let field = |v: &'a Bson| match v {
        Bson::String(s) if s.starts_with('$') && !s.starts_with("$$") => Some(&s[1..]),
        _ => None,
    };
    let constant = |v: &Bson| match v {
        Bson::String(s) => !s.starts_with('$') || s.starts_with("$$"),
        Bson::Document(d) => d
            .keys()
            .next()
            .is_none_or(|k| k == "$literal" || !k.starts_with('$')),
        _ => true,
    };
    match (op.as_str(), arg) {
        ("$and", Bson::Array(items)) => items.iter().for_each(|i| expr_fields(i, out)),
        ("$eq" | "$gt" | "$gte" | "$lt" | "$lte", Bson::Array(pair)) if pair.len() == 2 => {
            match (field(&pair[0]), field(&pair[1])) {
                (Some(path), None) if constant(&pair[1]) => out.push(path),
                (None, Some(path)) if constant(&pair[0]) => out.push(path),
                _ => {}
            }
        }
        _ => {}
    }
}

/// Whether the planner cannot say if mongod would read an index for
/// `filter`, so the read is let through:
///
/// - `$text`, and a field of an index the planner does not read (any key
///   that is not 1 or -1: hashed, geo, text, wildcard);
/// - an `$expr` that compares an index's leading field with a constant:
///   mongod rewrites that into a predicate the index can serve
///   ([`expr_fields`]) and the planner does not.
fn beyond_the_planner(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    filter: &Document,
) -> Result<bool, CommandError> {
    if filter.contains_key("$text") {
        return Ok(true);
    }
    let indexes = storage
        .list_indexes(db, coll)
        .map_err(crate::util::command_error)?;
    let (mut fields, mut in_expr) = (Vec::new(), Vec::new());
    tested_fields(filter, &mut fields, &mut in_expr);
    let top = |path: &str| path.split('.').next().unwrap_or(path).to_string();
    for spec in indexes {
        let Ok(key) = spec.get_document("key") else {
            continue;
        };
        // The planner does not see a hidden index, and neither does mongod.
        if spec.get_bool("hidden").unwrap_or(false) {
            continue;
        }
        // mongod does not serve a rewritten `$expr` from a sparse index
        // (measured: refused with one on the field).
        let sparse = spec.get_bool("sparse").unwrap_or(false);
        if !sparse
            && key
                .keys()
                .next()
                .is_some_and(|lead| in_expr.iter().any(|f| top(f) == top(lead)))
        {
            return Ok(true);
        }
        let plain = key.values().all(|v| {
            matches!(crate::util::as_i64(v), Some(1 | -1)) && !matches!(v, Bson::String(_))
        });
        if plain {
            continue;
        }
        let wildcard = key.keys().any(|k| k == "$**" || k.ends_with(".$**"));
        if wildcard || key.keys().any(|k| fields.iter().any(|f| top(f) == top(k))) {
            return Ok(true);
        }
    }
    Ok(false)
}

thread_local! {
    /// The namespace of the aggregate this thread runs while `notablescan`
    /// is set: a stage that reads another collection is checked too.
    static AGGREGATE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// While held, the stages of the aggregate on `ns` check their reads of
/// other collections (see [`check_foreign`]).
pub(crate) struct Aggregate(Option<String>);

impl Drop for Aggregate {
    fn drop(&mut self) {
        let previous = self.0.take();
        AGGREGATE.with(|a| *a.borrow_mut() = previous);
    }
}

/// Mark the aggregate on `ns` as running under `notablescan` (or not).
pub(crate) fn aggregate(ctx: &CommandContext, ns: &str) -> Aggregate {
    aggregate_on(enabled(ctx).then(|| ns.to_string()))
}

/// [`aggregate`] for a cursor that ran under `notablescan` when it opened
/// (`Some(ns)`), as each of its `getMore`s runs its stages.
pub(crate) fn aggregate_on(ns: Option<String>) -> Aggregate {
    Aggregate(AGGREGATE.with(|a| std::mem::replace(&mut *a.borrow_mut(), ns)))
}

/// A stage's read of `db.<read.coll>` inside an aggregate: refused when the
/// aggregate runs under `notablescan` and the read's plan scans the
/// collection, in mongod's words for an executor error.
pub(crate) fn check_foreign(
    storage: &dyn Storage,
    db: &str,
    read: &Read,
) -> Result<(), CommandError> {
    let Some(ns) = AGGREGATE.with(|a| a.borrow().clone()) else {
        return Ok(());
    };
    check_in(storage, db, read).map_err(|mut e| {
        e.errmsg = format!(
            "Executor error during aggregate command on namespace: {ns} :: caused by :: {}",
            e.errmsg
        );
        e
    })
}

/// Refuse `read` when `notablescan` is set and mongod's plan for it scans
/// the collection.
pub(crate) fn check(ctx: &CommandContext, read: &Read) -> Result<(), CommandError> {
    if !enabled(ctx) {
        return Ok(());
    }
    check_in(ctx.storage()?, &ctx.db_name, read)
}

/// [`check`] of a read of `db`, for a caller that has read the parameter
/// and holds no context.
pub(crate) fn check_in(storage: &dyn Storage, db: &str, read: &Read) -> Result<(), CommandError> {
    if exempt(db, read.coll, read.filter) {
        return Ok(());
    }
    // A view's read is its pipeline's, checked where the pipeline runs.
    if !storage.collection_exists(db, read.coll).unwrap_or(false)
        || crate::views::is_view(storage, db, read.coll)
    {
        return Ok(());
    }
    let why = crate::explain::plans_table_scan(
        storage,
        db,
        read.coll,
        read.filter,
        read.sort,
        read.hint,
        read.collation,
    )?;
    refuse_planned(storage, db, read, why)
}

/// The refusal of `read` of `db` whose plan is `why` (`None`: it reads an
/// index).
pub(crate) fn refuse_planned(
    storage: &dyn Storage,
    db: &str,
    read: &Read,
    why: Option<TableScan>,
) -> Result<(), CommandError> {
    let (coll, filter) = (read.coll, read.filter);
    let Some(why) = why else {
        return Ok(());
    };
    // A `$near` without its index is refused in words of its own, by the
    // read; with one it reads it.
    if crate::near::find_near(filter).ok().flatten().is_some() {
        return Ok(());
    }
    // A time-series collection is read through `system.buckets.<name>`,
    // which mongod exempts with every `system.*` collection (measured).
    if exempt(db, coll, filter)
        || storage
            .get_collection_options(db, coll)
            .is_ok_and(|o| o.contains_key("timeseries"))
    {
        return Ok(());
    }
    if why == TableScan::Unindexed && beyond_the_planner(storage, db, coll, filter)? {
        return Ok(());
    }
    Err(refusal(db, coll, filter, read.sort, &read.shown, why))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn the_exemptions_mongod_makes() {
        assert!(exempt("db", "c", &doc! {}));
        assert!(exempt("admin", "c", &doc! {"b": 1}));
        assert!(exempt("local", "oplog.rs", &doc! {"b": 1}));
        assert!(exempt("config", "c", &doc! {"b": 1}));
        assert!(exempt("db", "system.js", &doc! {"b": 1}));
        // A filter that only looks empty is refused (measured).
        assert!(!exempt("db", "c", &doc! {"$comment": "x"}));
        assert!(!exempt("db", "c", &doc! {"$and": [{}]}));
    }

    #[test]
    fn the_words_are_mongods_closing_words() {
        let e = refusal(
            "d",
            "c",
            &doc! {"b": 1},
            None,
            &Shown::default(),
            TableScan::Unindexed,
        );
        assert_eq!(e.code, 291);
        assert_eq!(e.code_name, "NoQueryExecutionPlans");
        assert_eq!(
            e.errmsg,
            "error processing query: ns=d.cTree: b $eq 1\nSort: {}\nProj: {}\n planner \
             returned error :: caused by :: No indexed plans available, and running with \
             'notablescan' 2"
        );
        let e = refusal(
            "d",
            "c",
            &doc! {"b": 1},
            None,
            &Shown::default(),
            TableScan::Natural,
        );
        assert!(e.errmsg.ends_with(
            "could not force a collection scan with a $natural hint :: caused by :: not allowed \
             to output a collection scan because 'notablescan' is enabled"
        ));
    }

    #[test]
    fn the_fields_a_filter_tests() {
        let f = doc! {
            "a": 1,
            "$or": [{"b.c": 1}, {"$and": [{"d": 1}]}],
            "$expr": {"$and": [{"$eq": ["$e", 1]}, {"$gt": ["$$v", "$f"]}, {"$ne": ["$g", 1]}, {"$eq": [{"$add": ["$h", 1]}, 2]}]},
        };
        let (mut out, mut in_expr) = (Vec::new(), Vec::new());
        tested_fields(&f, &mut out, &mut in_expr);
        assert_eq!(out, vec!["a", "b.c", "d"]);
        assert_eq!(in_expr, vec!["e", "f"]);
    }
}
