//! The replica-set, lock, feature, cluster-parameter and plan-cache
//! commands a tool sends a one-member set: `replSetInitiate`,
//! `appendOplogNote`, `replSetResizeOplog`, `lockInfo`, `features`,
//! `getClusterParameter`, `planCacheClear`, `planCacheClearFilters` and
//! `planCacheListFilters`.
//!
//! Every reply and message is what mongod 8.2.11 answered (2026-10-15), as
//! a replica-set member and, for a server run without a set name, as a
//! standalone. Each command says only what is true of this server:
//!
//! - `features` has no `js` block, because there is no script engine.
//! - `getClusterParameter` reports the cluster parameters whose default is a
//!   fact about this server (no shards, no default `maxTimeMS`, pre-images
//!   that go with the oplog). It does not have the rest, so it answers 4 for
//!   them, as mongod does for a name it does not have.
//! - There is no plan cache and no index filter here, so clearing either
//!   is true and the filter list is empty. `planCacheSetFilter` is not a
//!   command: a filter would be accepted and never applied.
//! - `replSetResizeOplog` changes the oplog's cap. `minRetentionHours` above
//!   zero is refused, because this oplog drops an entry older than its
//!   retention window whatever its size.

use bson::{doc, Bson, Document};

use crate::{CommandContext, CommandError, HandlerResult};

/// The oplog cap is counted in entries; `collStats` reports it as 16 KiB an
/// entry (`Storage::get_collection_options`), so a size in megabytes is 64
/// entries a megabyte.
const ENTRIES_PER_MB: f64 = 64.0;

fn type_name(v: &Bson) -> &'static str {
    secantus_core::query::bson_type_name(v)
}

fn unknown_field(command: &str, field: &str) -> CommandError {
    CommandError::new(
        40415,
        "IDLUnknownField",
        format!("BSON field '{command}.{field}' is an unknown field."),
    )
}

/// Refuse any field but the command's own, the generic ones and `allowed`.
fn only_fields(doc: &Document, command: &str, allowed: &[&str]) -> Result<(), CommandError> {
    for key in doc.keys() {
        if key == command
            || allowed.contains(&key.as_str())
            || key.starts_with('$')
            || crate::argtypes::is_generic_field(key)
        {
            continue;
        }
        return Err(unknown_field(command, key));
    }
    Ok(())
}

fn no_replication(message: &str) -> CommandError {
    CommandError::new(76, "NoReplicationEnabled", message)
}

fn storage_error(e: crate::storage::StorageError) -> CommandError {
    CommandError::new(1, "InternalError", format!("{e:?}"))
}

/// `replSetInitiate`: a set member has its configuration already.
pub fn repl_set_initiate(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    if ctx.replica_set_name.is_none() {
        return Err(no_replication(
            "This node was not started with replication enabled.",
        ));
    }
    Err(CommandError::new(
        23,
        "AlreadyInitialized",
        "already initialized",
    ))
}

/// `appendOplogNote`: a no-op oplog entry, `{op: "n", ns: "", o: data}`.
pub fn append_oplog_note(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    if ctx.replica_set_name.is_none() {
        return Err(no_replication(
            "Must have replication set up to run \"appendOplogNote\"",
        ));
    }
    let data = match doc.get("data") {
        Some(Bson::Document(d)) => d.clone(),
        Some(other) => {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "\"data\" had the wrong type. Expected object, found {}",
                    type_name(other)
                ),
            ))
        }
        None => {
            return Err(CommandError::new(
                4,
                "NoSuchKey",
                "Missing expected field \"data\"",
            ))
        }
    };
    ctx.storage()?
        .append_oplog_note(data)
        .map_err(storage_error)?;
    Ok(doc! {"ok": 1.0})
}

/// A number as C++'s default stream prints it (`%g`, six significant
/// digits): `990`, `989.5`, `1.07374e+09`. mongod's range messages for
/// `replSetResizeOplog` quote both bound and value this way.
fn render_g(x: f64) -> String {
    if x == 0.0 {
        return "0".into();
    }
    let exp = x.abs().log10().floor() as i32;
    if !(-5..6).contains(&exp) {
        let s = format!("{x:.5e}");
        let (mantissa, e) = s.split_once('e').expect("exponent form");
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        let e: i32 = e.parse().expect("exponent");
        let sign = if e < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", e.abs());
    }
    let decimals = (5 - exp).max(0) as usize;
    let s = format!("{x:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// A number field of `replSetResizeOplog`: `None` when absent or null; a
/// NaN reads as 0, as mongod reads it.
fn resize_number(doc: &Document, field: &str) -> Result<Option<f64>, CommandError> {
    let n = match doc.get(field) {
        None | Some(Bson::Null) => return Ok(None),
        Some(Bson::Int32(i)) => f64::from(*i),
        Some(Bson::Int64(i)) => *i as f64,
        Some(Bson::Double(d)) => *d,
        Some(Bson::Decimal128(d)) => d.to_string().parse::<f64>().unwrap_or(0.0),
        Some(other) => {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'replSetResizeOplog.{field}' is the wrong type '{}', expected \
                     types '[double, decimal, int, long]'",
                    type_name(other)
                ),
            ))
        }
    };
    Ok(Some(if n.is_nan() { 0.0 } else { n }))
}

fn out_of_range(field: &str, relation: &str, bound: f64, value: f64) -> CommandError {
    CommandError::new(
        2,
        "BadValue",
        format!(
            "BSON field '{field}' value must be {relation} {}, actual value '{}'",
            render_g(bound),
            render_g(value)
        ),
    )
}

/// `replSetResizeOplog`: `size` in megabytes, 990 to 1 PB.
pub fn repl_set_resize_oplog(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    if ctx.replica_set_name.is_none() {
        return Err(CommandError::new(
            26,
            "NamespaceNotFound",
            "oplog does not exist",
        ));
    }
    only_fields(doc, "replSetResizeOplog", &["size", "minRetentionHours"])?;
    let size = resize_number(doc, "size")?;
    let hours = resize_number(doc, "minRetentionHours")?;
    if let Some(mb) = size {
        if mb < 990.0 {
            return Err(out_of_range("size", ">=", 990.0, mb));
        }
        if mb > 1_073_741_824.0 {
            return Err(out_of_range("size", "<=", 1_073_741_824.0, mb));
        }
    }
    if let Some(h) = hours {
        if h < 0.0 {
            return Err(out_of_range("minRetentionHours", ">=", 0.0, h));
        }
        if h > 0.0 {
            return Err(CommandError::new(
                2,
                "BadValue",
                "minRetentionHours is not supported by this server: its oplog drops an entry \
                 older than its retention window whatever the oplog's size",
            ));
        }
    }
    if let Some(mb) = size {
        let entries = (mb * ENTRIES_PER_MB).round().min(usize::MAX as f64) as usize;
        ctx.storage()?
            .set_oplog_max_entries(entries)
            .map_err(storage_error)?;
    }
    Ok(doc! {"ok": 1.0})
}

/// `lockInfo`: the locks operations hold or wait for. This server has none
/// of mongod's lock manager, so the list is empty.
pub fn lock_info(doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    only_fields(doc, "lockInfo", &[])?;
    Ok(doc! {"lockInfo": [], "ok": 1.0})
}

/// The `oidMachine` mongod reports: the first four bytes of the process's
/// ObjectId value, read little-endian (an ObjectId the server mints carries
/// the same five bytes; measured 8.2.11).
fn oid_machine() -> i64 {
    i64::from(secantus_core::oid::machine())
}

/// `features`: no `js` block, as a `mongod --noscripting` answers (there is
/// no script engine), and the ObjectId machine value. `oidReset` replaces
/// the process value and reports the old one too (mongod 8.2.11).
pub fn features(doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    only_fields(doc, "features", &["oidReset"])?;
    if doc.get("oidReset").is_some_and(crate::util::truthy) {
        let (old, now) = secantus_core::oid::reset();
        return Ok(doc! {
            "oidMachineOld": i64::from(old),
            "oidMachine": i64::from(now),
            "ok": 1.0,
        });
    }
    Ok(doc! {"oidMachine": oid_machine(), "ok": 1.0})
}

/// The cluster parameters this server has: each default is a fact about
/// it, in mongod's name order.
fn cluster_parameters() -> Vec<Document> {
    vec![
        doc! {"_id": "addOrRemoveShardInProgress", "inProgress": false},
        doc! {"_id": "changeStreamOptions", "preAndPostImages": {"expireAfterSeconds": "off"}},
        doc! {"_id": "defaultMaxTimeMS", "readOperations": 0_i64},
        doc! {"_id": "onlyTargetDataOwningShardsForMultiWrites", "enabled": false},
        doc! {"_id": "pauseMigrationsDuringMultiUpdates", "enabled": false},
        doc! {"_id": "shardedClusterCardinalityForDirectConns", "hasTwoOrMoreShards": false},
    ]
}

/// The names `getClusterParameter` asks for: `"*"` for every one.
fn requested_names(doc: &Document) -> Result<Option<Vec<String>>, CommandError> {
    let wrong = |v: &Bson| {
        CommandError::new(
            14,
            "TypeMismatch",
            format!(
                "BSON field 'getClusterParameter.getClusterParameter' is the wrong type '{}', \
                 expected types '[string]'",
                type_name(v)
            ),
        )
    };
    match doc.get("getClusterParameter") {
        Some(Bson::String(s)) if s == "*" => Ok(None),
        Some(Bson::String(s)) => Ok(Some(vec![s.clone()])),
        Some(Bson::Array(names)) => {
            if names.is_empty() {
                return Err(CommandError::new(
                    2,
                    "BadValue",
                    "Must supply at least one cluster server parameter name to \
                     getClusterParameter",
                ));
            }
            names
                .iter()
                .map(|n| match n {
                    Bson::String(s) => Ok(s.clone()),
                    other => Err(wrong(other)),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
        }
        Some(other) => Err(wrong(other)),
        None => Err(wrong(&Bson::Null)),
    }
}

/// `getClusterParameter`. Its argument is read before the database is: a
/// wrong type is 14 from any database, and only then is a database other
/// than `admin` refused.
pub fn get_cluster_parameter(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let names = requested_names(doc)?;
    if ctx.db_name != "admin" {
        return Err(CommandError::new(
            13,
            "Unauthorized",
            "getClusterParameter may only be run against the admin database.",
        ));
    }
    let all = cluster_parameters();
    let set_member = ctx.replica_set_name.is_some();
    let stamp = |mut p: Document| {
        // A set member stamps each parameter with the time it was last set;
        // none has been, so it is the null timestamp. A standalone has no
        // such time and leaves it out.
        if set_member {
            let id = p.remove("_id").unwrap_or(Bson::Null);
            let mut out = doc! {"_id": id, "clusterParameterTime": Bson::Timestamp(bson::Timestamp { time: 0, increment: 0 })};
            out.extend(p);
            out
        } else {
            p
        }
    };
    let chosen: Vec<Bson> = match names {
        None => all.into_iter().map(|p| Bson::Document(stamp(p))).collect(),
        Some(names) => {
            let mut out = Vec::new();
            for name in names {
                let Some(p) = all.iter().find(|p| p.get_str("_id") == Ok(name.as_str())) else {
                    return Err(CommandError::new(
                        4,
                        "NoSuchKey",
                        format!("Unknown server parameter: {name}"),
                    ));
                };
                out.push(Bson::Document(stamp(p.clone())));
            }
            out
        }
    };
    Ok(doc! {"clusterParameters": chosen, "ok": 1.0})
}

/// The collection a plan-cache command names, checked as mongod checks it:
/// a non-string is 73, a view is 166, a collection that does not exist is
/// fine.
fn plan_cache_target(
    doc: &Document,
    command: &str,
    ctx: &mut CommandContext,
) -> Result<(), CommandError> {
    let coll = match doc.get(command) {
        Some(Bson::String(s)) => s.clone(),
        Some(other) => {
            return Err(CommandError::new(
                73,
                "InvalidNamespace",
                format!("Collection name has invalid type {}", type_name(other)),
            ))
        }
        None => return Ok(()),
    };
    let db = ctx.db_name.clone();
    if crate::views::is_view(ctx.storage()?, &db, &coll) {
        return Err(CommandError::new(
            166,
            "CommandNotSupportedOnView",
            format!("Namespace {db}.{coll} is a view, not a collection"),
        ));
    }
    Ok(())
}

/// `planCacheClear` / `planCacheClearFilters`: nothing to clear.
pub fn plan_cache_clear(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let command = doc.keys().next().cloned().unwrap_or_default();
    plan_cache_target(doc, &command, ctx)?;
    Ok(doc! {"ok": 1.0})
}

/// `planCacheListFilters`: no filter can be set, so there is none.
pub fn plan_cache_list_filters(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    plan_cache_target(doc, "planCacheListFilters", ctx)?;
    Ok(doc! {"filters": [], "ok": 1.0})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_as_cpp_streams_print_them() {
        assert_eq!(render_g(990.0), "990");
        assert_eq!(render_g(989.5), "989.5");
        assert_eq!(render_g(5.0), "5");
        assert_eq!(render_g(-1.0), "-1");
        assert_eq!(render_g(0.0), "0");
        assert_eq!(render_g(1_073_741_824.0), "1.07374e+09");
        assert_eq!(render_g(1_073_741_825.0), "1.07374e+09");
    }

    #[test]
    fn oid_machine_is_the_process_value() {
        let bytes = secantus_core::oid::new().bytes();
        let want = i64::from(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]));
        assert_eq!(oid_machine(), want);
    }
}
