//! The replica-set, lock, feature and plan-cache
//! commands a tool sends a one-member set: `replSetInitiate`,
//! `appendOplogNote`, `replSetResizeOplog`, `lockInfo`, `features` and
//! `planCacheClear`. The cluster parameters are `cluster_params`.
//!
//! Every reply and message is what mongod 8.2.11 answered (2026-10-15), as
//! a replica-set member and, for a server run without a set name, as a
//! standalone. Each command says only what is true of this server:
//!
//! - `features` has no `js` block, because there is no script engine.
//! - `planCacheClear` clears the plan cache `explain::plan_cache` keeps.
//!   Index filters are `index_filters`.
//! - `replSetResizeOplog` changes the oplog's cap, and `minRetentionHours`
//!   keeps every entry younger than it whatever the cap.

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
    }
    if let Some(mb) = size {
        let entries = (mb * ENTRIES_PER_MB).round().min(usize::MAX as f64) as usize;
        ctx.storage()?
            .set_oplog_max_entries(entries)
            .map_err(storage_error)?;
    }
    // No entry younger than this goes, whatever the size (mongod 8.2.11).
    if let Some(h) = hours {
        ctx.storage()?
            .set_oplog_min_retention_seconds((h * 3600.0).round() as i64)
            .map_err(storage_error)?;
    }
    Ok(doc! {"ok": 1.0})
}

fn config_error(code: i32, msg: impl Into<String>) -> CommandError {
    CommandError::new(code, crate::util::error_code_name(code), msg)
}

/// `replSetReconfig`: a new configuration for the one-member set, kept in
/// `local.system.replset` where `replSetGetConfig` reads it. Each check and
/// message is mongod 8.2.11's (measured 2026-10-20). A configuration with
/// more members than this server is refused: SecantusDB is a single-node
/// set, and mongod would go on to replicate to them.
pub fn repl_set_reconfig(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let (Some(set_name), Some((host, port))) =
        (ctx.replica_set_name.clone(), ctx.server_address.clone())
    else {
        return Err(config_error(76, "not running with --replSet"));
    };
    let Some(Bson::Document(new)) = doc.get("replSetReconfig") else {
        return Err(config_error(2, "no configuration specified"));
    };
    let force = doc.get("force").is_some_and(crate::util::truthy);
    const FIELDS: &[&str] = &[
        "_id",
        "version",
        "term",
        "members",
        "protocolVersion",
        "writeConcernMajorityJournalDefault",
        "settings",
        "configsvr",
    ];
    for key in new.keys() {
        if !FIELDS.contains(&key.as_str()) {
            return Err(config_error(
                93,
                format!("BSON field 'ReplSetConfig.{key}' is an unknown field."),
            ));
        }
    }
    for required in ["_id", "version", "members"] {
        if !new.contains_key(required) {
            return Err(config_error(
                93,
                format!("BSON field 'ReplSetConfig.{required}' is missing but a required field"),
            ));
        }
    }
    let name = new.get_str("_id").unwrap_or_default();
    if name != set_name {
        return Err(config_error(
            93,
            format!(
                "Rejecting reconfig where new config set name differs from command line set \
                 name, new config set name: {name}, old config set name: {set_name}"
            ),
        ));
    }
    let storage = ctx.storage()?;
    let host_port = format!("{host}:{port}");
    let old = crate::serverinfo::stored_replset_config(storage, &set_name).unwrap_or_else(|| {
        crate::serverinfo::replset_config(
            &set_name,
            &host_port,
            crate::handshake::hello_process_id(),
        )
    });
    let members = new
        .get_array("members")
        .map_err(|_| config_error(93, "BSON field 'ReplSetConfig.members' is the wrong type"))?;
    if members.is_empty() || members.len() > 50 {
        return Err(config_error(
            103,
            format!(
                "Replica set configuration contains {} members, but must have at least 1 and no \
                 more than  50",
                members.len()
            ),
        ));
    }
    if members.len() > 1 {
        return Err(config_error(
            103,
            "SecantusDB is a single-node replica set: a configuration names exactly one member, \
             this server",
        ));
    }
    let Bson::Document(given) = &members[0] else {
        return Err(config_error(
            93,
            "BSON field 'ReplSetConfig.members' is the wrong type",
        ));
    };
    let mut member = doc! {
        "_id": 0_i32,
        "host": "",
        "arbiterOnly": false,
        "buildIndexes": true,
        "hidden": false,
        "priority": 1.0,
        "tags": {},
        "secondaryDelaySecs": 0_i64,
        "votes": 1_i32,
    };
    for (k, v) in given {
        member.insert(k.clone(), v.clone());
    }
    let number = |v: Option<&Bson>| {
        v.and_then(|b| match b {
            Bson::Int32(i) => Some(f64::from(*i)),
            Bson::Int64(i) => Some(*i as f64),
            Bson::Double(d) => Some(*d),
            _ => None,
        })
    };
    let old_id = old
        .get_array("members")
        .ok()
        .and_then(|m| m.first())
        .and_then(Bson::as_document)
        .and_then(|m| m.get("_id"))
        .cloned()
        .unwrap_or(Bson::Int32(0));
    // A member under another `_id` is a new member, and a new member has
    // no vote until it has caught up: the set would have none.
    // A member without a vote may not be electable (measured).
    if number(member.get("votes")) == Some(0.0) && number(member.get("priority")) != Some(0.0) {
        return Err(config_error(
            93,
            format!(
                "member: {} :: caused by :: priority must be 0 when non-voting (votes:0)",
                secantus_core::update::render_shell(&Bson::Document(member.clone()))
            ),
        ));
    }
    if number(member.get("_id")) != number(Some(&old_id))
        || number(member.get("votes")) == Some(0.0)
    {
        return Err(config_error(
            103,
            "Replica set configuration contains 0 voting members, but must be at least 1 and no \
             more than 7",
        ));
    }
    if number(member.get("priority")) == Some(0.0) || member.get_bool("arbiterOnly") == Ok(true) {
        return Err(config_error(
            103,
            "Replica set configuration must contain at least one non-arbiter member with \
             priority > 0",
        ));
    }
    let member_host = member.get_str("host").unwrap_or_default().to_string();
    let mine = [
        host_port.clone(),
        format!("localhost:{port}"),
        format!("127.0.0.1:{port}"),
    ];
    let version = number(new.get("version")).unwrap_or(0.0) as i64;
    let old_version = old
        .get_i32("version")
        .map(i64::from)
        .or_else(|_| old.get_i64("version"))
        .unwrap_or(1);
    let term = old
        .get_i32("term")
        .map(i64::from)
        .or_else(|_| old.get_i64("term"))
        .unwrap_or(1);
    if !mine.contains(&member_host) {
        return Err(config_error(
            74,
            format!(
                "No host described in new configuration with {{version: {version}, term: \
                 {term}}} for replica set {set_name} maps to this node"
            ),
        ));
    }
    if !force && version <= old_version {
        return Err(config_error(
            103,
            format!(
                "New replica set configuration version and term must be greater than old, but \
                 {{version: {version}, term: {term}}} is not greater than {{version: \
                 {old_version}, term: {term}}} for replica set {set_name}"
            ),
        ));
    }
    // The settings start from their defaults, not from the old ones: a
    // configuration without `settings` puts each back (measured).
    let old_set_id = old
        .get_document("settings")
        .ok()
        .and_then(|s| s.get_object_id("replicaSetId").ok());
    let mut settings = crate::serverinfo::replset_config(
        &set_name,
        &host_port,
        old_set_id.unwrap_or_else(crate::handshake::hello_process_id),
    )
    .get_document("settings")
    .cloned()
    .unwrap_or_default();
    if let Ok(given) = new.get_document("settings") {
        if let (Ok(newer), Some(older)) = (given.get_object_id("replicaSetId"), old_set_id) {
            if newer != older {
                return Err(config_error(
                    103,
                    format!(
                        "New and old configurations differ in replica set ID; old was {older}, \
                         and new is {newer}"
                    ),
                ));
            }
        }
        // The settings mongod knows; others are not kept (measured).
        for (k, v) in given {
            if settings.contains_key(k) && k != "replicaSetId" {
                settings.insert(k.clone(), v.clone());
            }
        }
    }
    let config = doc! {
        "_id": set_name.clone(),
        "version": version as i32,
        "term": term as i32,
        "members": [member],
        "protocolVersion": 1_i64,
        "writeConcernMajorityJournalDefault": new
            .get_bool("writeConcernMajorityJournalDefault")
            .unwrap_or(true),
        "settings": settings,
    };
    storage
        .update_matching(
            "local",
            "system.replset",
            &doc! {"_id": set_name},
            &config,
            false,
            true,
        )
        .map_err(storage_error)?;
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

/// `planCacheClear`: the collection's plan cache, or one query shape's
/// entry in it (`explain::plan_cache`).
pub fn plan_cache_clear(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let command = doc.keys().next().cloned().unwrap_or_default();
    plan_cache_target(doc, &command, ctx)?;
    if let Some(Bson::String(coll)) = doc.get(&command) {
        let db = ctx.db_name.clone();
        crate::explain::plan_cache::clear_command(ctx.storage()?, &db, coll, doc)?;
    }
    Ok(doc! {"ok": 1.0})
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
