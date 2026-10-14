//! `getDefaultRWConcern` / `setDefaultRWConcern`: the cluster-wide default
//! read and write concern, kept where mongod keeps it.
//!
//! mongod persists the defaults as the one document
//! `config.settings {_id: "ReadWriteConcernDefaults"}` and reads them back
//! from there on every start, so this server stores the same document in
//! the same collection: it survives a restart, `dbHash` on `config` sees it,
//! and a tool that reads `config.settings` finds what it finds on mongod.
//! Every shape and message below was measured on a one-member replica set
//! of mongod 8.2.11 (2026-10-14, `tools/probes/admin_commands.py`).
//!
//! What the write of the settings document is held to is the default in
//! force BEFORE the command: a default of `w: 3` makes the next
//! `setDefaultRWConcern` fail with `UnsatisfiableWriteConcern` while still
//! storing its new value, as mongod does (`crate::dispatch` attaches the
//! error; `set_default_rw_concern` only writes).

use bson::{doc, Bson, Document};

use crate::util::as_i64;
use crate::{CommandContext, CommandError, HandlerResult};

/// The collection and `_id` mongod uses.
pub(crate) const SETTINGS_DB: &str = "config";
pub(crate) const SETTINGS_COLL: &str = "settings";
const SETTINGS_ID: &str = "ReadWriteConcernDefaults";

fn type_mismatch(msg: String) -> CommandError {
    CommandError::new(14, "TypeMismatch", msg)
}

fn bad_value(msg: impl Into<String>) -> CommandError {
    CommandError::new(2, "BadValue", msg)
}

fn failed_to_parse(msg: impl Into<String>) -> CommandError {
    CommandError::new(9, "FailedToParse", msg)
}

fn unknown_field(path: &str, field: &str) -> CommandError {
    CommandError::new(
        40415,
        "IDLUnknownField",
        format!("BSON field '{path}.{field}' is an unknown field."),
    )
}

fn type_name(v: &Bson) -> &'static str {
    secantus_core::query::bson_type_name(v)
}

/// The stored settings document, if any.
pub(crate) fn stored(ctx: &CommandContext) -> Result<Option<Document>, CommandError> {
    let storage = ctx.storage()?;
    let rows = storage
        .find(
            SETTINGS_DB,
            SETTINGS_COLL,
            &doc! {"_id": SETTINGS_ID},
            None,
            None,
        )
        .map_err(crate::util::command_error)?;
    Ok(rows
        .first()
        .and_then(|bytes| crate::bsondoc::from_bytes(bytes).ok()))
}

/// The default write concern a write with none of its own is held to:
/// the stored one, or `None` for mongod's implicit `w: "majority"` (which
/// a one-member set always satisfies).
pub(crate) fn default_write_concern(ctx: &CommandContext) -> Option<Document> {
    if let Some(p) = &ctx.server_params {
        if let Some(cached) = p.rw_defaults_cached() {
            return cached.and_then(|d| d.get_document("defaultWriteConcern").ok().cloned());
        }
    }
    let settings = stored(ctx).ok()?;
    if let Some(p) = &ctx.server_params {
        p.cache_rw_defaults(settings.clone());
    }
    settings.and_then(|d| d.get_document("defaultWriteConcern").ok().cloned())
}

/// A command that may have written `config.settings` behind the cache's
/// back ran: read the document again next time.
pub(crate) fn forget_cached(ctx: &CommandContext) {
    if let Some(p) = &ctx.server_params {
        p.forget_rw_defaults();
    }
}

/// The reply both commands give: what is in force, where it came from, and
/// when it was last set.
fn reply(settings: Option<&Document>, in_memory: Option<bool>) -> Document {
    let read = settings
        .and_then(|d| d.get_document("defaultReadConcern").ok())
        .cloned();
    let write = settings
        .and_then(|d| d.get_document("defaultWriteConcern").ok())
        .cloned();
    let mut out = doc! {
        "defaultReadConcern": read.clone().unwrap_or_else(|| doc! {"level": "local"}),
        "defaultWriteConcern": write.clone().unwrap_or_else(|| doc! {"w": "majority", "wtimeout": 0_i32}),
    };
    let mut loaded = crate::handshake::process_started();
    if let Some(d) = settings {
        if let Some(t) = d.get("updateOpTime") {
            out.insert("updateOpTime", t.clone());
        }
        if let Some(t) = d.get("updateWallClockTime") {
            out.insert("updateWallClockTime", t.clone());
            if let Bson::DateTime(set_at) = t {
                if *set_at > loaded {
                    loaded = *set_at;
                }
            }
        }
    }
    out.insert(
        "defaultWriteConcernSource",
        if write.is_some() {
            "global"
        } else {
            "implicit"
        },
    );
    out.insert(
        "defaultReadConcernSource",
        if read.is_some() { "global" } else { "implicit" },
    );
    // Echoed only when asked for: `inMemory: false` is not answered.
    if in_memory == Some(true) {
        out.insert("inMemory", true);
    }
    out.insert("localUpdateWallClockTime", loaded);
    out.insert("ok", 1.0);
    out
}

/// `getDefaultRWConcern`: the defaults in force. `inMemory` must be a bool
/// (and this server has nothing else to answer from); any other field is
/// unknown.
pub fn get_default_rw_concern(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let mut in_memory = None;
    for (key, value) in doc {
        match key.as_str() {
            "getDefaultRWConcern" => {}
            "inMemory" => {
                in_memory = match value {
                    Bson::Null => None,
                    Bson::Boolean(b) => Some(*b),
                    v => {
                        return Err(type_mismatch(format!(
                            "BSON field 'getDefaultRWConcern.inMemory' is the wrong type '{}', \
                             expected type 'bool'",
                            type_name(v)
                        )))
                    }
                }
            }
            k if k.starts_with('$') || crate::argtypes::is_generic_field(k) => {}
            k => return Err(unknown_field("getDefaultRWConcern", k)),
        }
    }
    let settings = stored(ctx)?;
    Ok(reply(settings.as_ref(), in_memory))
}

/// The read concern as `setDefaultRWConcern` parses it: the IDL's checks
/// (field names, types, the `level` enumeration, `atClusterTime` only with
/// `snapshot`), with the default-specific refusals left to `check_read`.
/// `None` is an empty document, or a null `level`: the default is unset.
fn parse_read(rc: &Document) -> Result<Option<Document>, CommandError> {
    let mut level = None;
    let mut extra: Vec<&str> = Vec::new();
    for (key, value) in rc {
        match key.as_str() {
            "level" => match value {
                Bson::Null => {}
                Bson::String(s) => {
                    if !matches!(
                        s.as_str(),
                        "local" | "majority" | "linearizable" | "available" | "snapshot"
                    ) {
                        return Err(bad_value(format!(
                            "Enumeration value '{s}' for field 'readConcern.level' is not a \
                             valid value."
                        )));
                    }
                    level = Some(s.clone());
                }
                v => {
                    return Err(type_mismatch(format!(
                        "BSON field 'readConcern.level' is the wrong type '{}', expected type \
                         'string'",
                        type_name(v)
                    )))
                }
            },
            "afterClusterTime" | "atClusterTime" => {
                if !matches!(value, Bson::Timestamp(_)) {
                    return Err(type_mismatch(format!(
                        "\"{key}\" had the wrong type. Expected 17, found {}",
                        type_name(value)
                    )));
                }
                extra.push(key);
            }
            "afterOpTime" => extra.push(key),
            "provenance" => match value {
                Bson::String(s)
                    if matches!(
                        s.as_str(),
                        "clientSupplied" | "implicitDefault" | "customDefault"
                    ) =>
                {
                    extra.push(key)
                }
                Bson::String(s) => {
                    return Err(bad_value(format!(
                        "Enumeration value '{s}' for field 'readConcern.provenance' is not a \
                         valid value."
                    )))
                }
                v => {
                    return Err(type_mismatch(format!(
                        "BSON field 'readConcern.provenance' is the wrong type '{}', expected \
                         type 'string'",
                        type_name(v)
                    )))
                }
            },
            k => return Err(unknown_field("readConcern", k)),
        }
    }
    if extra.contains(&"atClusterTime") && level.as_deref() != Some("snapshot") {
        return Err(CommandError::new(
            72,
            "InvalidOptions",
            "atClusterTime field can be set only if level is equal to snapshot",
        ));
    }
    let Some(level) = level else {
        if extra.is_empty() {
            return Ok(None);
        }
        return Ok(Some(doc! {"level": "local", "extra": extra.join(",")}));
    };
    let mut out = doc! {"level": level};
    if !extra.is_empty() {
        out.insert("extra", extra.join(","));
    }
    Ok(Some(out))
}

/// What a read concern may not be as a default: a level that needs a
/// transaction or a single node's own clock, or any timing field.
fn check_read(rc: &Document) -> Result<(), CommandError> {
    let level = rc.get_str("level").unwrap_or("local");
    if matches!(level, "snapshot" | "linearizable") {
        return Err(bad_value(format!(
            "level: '{level}' is not suitable for the default read concern"
        )));
    }
    for field in rc.get_str("extra").unwrap_or("").split(',') {
        match field {
            "afterClusterTime" | "afterOpTime" => {
                return Err(bad_value(format!(
                    "'{field}' is not suitable for the default read concern"
                )))
            }
            "provenance" => {
                return Err(bad_value(
                    "'provenance' must be unset in default read concern",
                ))
            }
            _ => {}
        }
    }
    Ok(())
}

/// A parsed default write concern: what is stored, plus what the
/// post-parse checks need to know.
struct WriteConcern {
    stored: Document,
    w_zero: bool,
    unknown_mode: Option<String>,
    provenance: bool,
}

/// The write concern as `WriteConcernOptions::parse` reads it: field names
/// and types, `w`'s kind and range, `j` against `fsync`, and that a
/// non-empty document names `w`. `None` is the empty document.
fn parse_write(wc: &Document) -> Result<Option<WriteConcern>, CommandError> {
    if wc.is_empty() {
        return Ok(None);
    }
    let mut w: Option<Bson> = None;
    let mut j: Option<bool> = None;
    let mut fsync = false;
    let mut wtimeout = 0_i32;
    let mut provenance = false;
    let mut w_zero = false;
    let mut unknown_mode = None;
    for (key, value) in wc {
        match key.as_str() {
            "w" => {
                w = Some(match value {
                    Bson::Null => Bson::String(String::new()),
                    Bson::String(s) => Bson::String(s.clone()),
                    Bson::Document(tags) => {
                        if tags.is_empty() {
                            return Err(failed_to_parse("tagged write concern requires tags"));
                        }
                        Bson::Document(
                            tags.iter()
                                .map(|(k, v)| (k.clone(), Bson::Int64(as_i64(v).unwrap_or(0))))
                                .collect(),
                        )
                    }
                    Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => {
                        let n = crate::write_concern_w_number(value);
                        if !(0..=50).contains(&n) {
                            return Err(failed_to_parse(format!(
                                "w has to be a non-negative number and not greater than 50; \
                                 found: {n}"
                            )));
                        }
                        w_zero = n == 0;
                        Bson::Int32(n as i32)
                    }
                    v => {
                        return Err(failed_to_parse(format!(
                            "w has to be a number, string, or object; found: {}",
                            type_name(v)
                        )))
                    }
                });
                if let Some(Bson::String(mode)) = &w {
                    if mode != "majority" {
                        unknown_mode = Some(mode.clone());
                    }
                }
            }
            "j" => match value {
                Bson::Null => {}
                Bson::Boolean(b) => j = Some(*b),
                v if as_i64(v).is_some() => j = Some(as_i64(v) != Some(0)),
                v => {
                    return Err(type_mismatch(format!(
                        "BSON field 'WriteConcernOptions.j' is the wrong type '{}', expected \
                         types '[bool, double, long, decimal, int]'",
                        type_name(v)
                    )))
                }
            },
            "wtimeout" => {
                // A non-number is read as 0; a number must fit an int.
                if let Some(n) = as_i64(value).filter(|_| !matches!(value, Bson::Boolean(_))) {
                    wtimeout = i32::try_from(n)
                        .map_err(|_| failed_to_parse("wtimeout must be a 32-bit integer"))?;
                }
            }
            "fsync" => fsync = crate::util::truthy(value),
            "getLastError" => {}
            "provenance" => match value {
                Bson::String(s)
                    if matches!(
                        s.as_str(),
                        "clientSupplied"
                            | "implicitDefault"
                            | "customDefault"
                            | "internalWriteDefault"
                    ) =>
                {
                    provenance = true
                }
                Bson::String(s) => {
                    return Err(bad_value(format!(
                        "Enumeration value '{s}' for field 'WriteConcernOptions.provenance' is \
                         not a valid value."
                    )))
                }
                v => {
                    return Err(type_mismatch(format!(
                        "BSON field 'WriteConcernOptions.provenance' is the wrong type '{}', \
                         expected type 'string'",
                        type_name(v)
                    )))
                }
            },
            k => return Err(unknown_field("WriteConcernOptions", k)),
        }
    }
    if j == Some(true) && fsync {
        return Err(failed_to_parse(
            "fsync and j options cannot be used together",
        ));
    }
    let Some(w) = w else {
        return Err(bad_value("Default write concern must have 'w' field."));
    };
    let mut stored = doc! {"w": w};
    if let Some(j) = j {
        stored.insert("j", j);
    }
    stored.insert("wtimeout", wtimeout);
    if fsync {
        stored.insert("fsync", true);
    }
    Ok(Some(WriteConcern {
        stored,
        w_zero,
        unknown_mode,
        provenance,
    }))
}

/// What a write concern may not be as a default.
fn check_write(wc: &WriteConcern) -> Result<(), CommandError> {
    if let Some(mode) = &wc.unknown_mode {
        return Err(CommandError::new(
            79,
            "UnknownReplWriteConcern",
            format!("No write concern mode named '{mode}' found in replica set configuration"),
        ));
    }
    if wc.w_zero {
        return Err(bad_value(
            "Unacknowledged write concern is not suitable for the default write concern",
        ));
    }
    if wc.provenance {
        return Err(bad_value(
            "'provenance' must be unset in default write concern",
        ));
    }
    Ok(())
}

/// A field of the command, as given: absent or null, empty, or a value.
enum Given<T> {
    Absent,
    Unset,
    Value(T),
}

/// `setDefaultRWConcern`: store the defaults and answer with what is now in
/// force. The fields are read in the order they were sent, as mongod's IDL
/// reads them, so the first problem in the document is the one reported.
pub fn set_default_rw_concern(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let mut read: Given<Document> = Given::Absent;
    let mut write: Given<WriteConcern> = Given::Absent;
    for (key, value) in doc {
        match key.as_str() {
            "setDefaultRWConcern" => {}
            "defaultReadConcern" => {
                read = match value {
                    Bson::Null => Given::Absent,
                    Bson::Document(d) => match parse_read(d)? {
                        Some(rc) => Given::Value(rc),
                        None => Given::Unset,
                    },
                    v => {
                        return Err(type_mismatch(format!(
                            "BSON field 'setDefaultRWConcern.defaultReadConcern' is the wrong \
                             type '{}', expected type 'object'",
                            type_name(v)
                        )))
                    }
                }
            }
            "defaultWriteConcern" => {
                write = match value {
                    Bson::Null => Given::Absent,
                    Bson::Document(d) => match parse_write(d)? {
                        Some(wc) => Given::Value(wc),
                        None => Given::Unset,
                    },
                    v => {
                        return Err(type_mismatch(format!(
                            "BSON field 'setDefaultRWConcern.defaultWriteConcern' is the wrong \
                             type '{}', expected type 'object'",
                            type_name(v)
                        )))
                    }
                }
            }
            k if k.starts_with('$') || crate::argtypes::is_generic_field(k) => {}
            k => return Err(unknown_field("setDefaultRWConcern", k)),
        }
    }
    if matches!(read, Given::Absent) && matches!(write, Given::Absent) {
        return Err(bad_value(
            "At least one of the \"defaultReadConcern\" or \"defaultWriteConcern\" fields must \
             be present",
        ));
    }
    if let Given::Value(rc) = &read {
        check_read(rc)?;
    }
    if let Given::Value(wc) = &write {
        check_write(wc)?;
    }
    let before = stored(ctx)?;
    let had_write = before
        .as_ref()
        .is_some_and(|d| d.contains_key("defaultWriteConcern"));
    if matches!(write, Given::Unset) && had_write {
        return Err(CommandError::new(
            20,
            "IllegalOperation",
            "The global default write concern cannot be unset once it is set.",
        ));
    }

    let mut settings = doc! {"_id": SETTINGS_ID};
    let read_level = match read {
        Given::Value(rc) => Some(rc.get_str("level").unwrap_or("local").to_string()),
        Given::Unset => None,
        Given::Absent => before
            .as_ref()
            .and_then(|d| d.get_document("defaultReadConcern").ok())
            .and_then(|rc| rc.get_str("level").ok())
            .map(str::to_string),
    };
    if let Some(level) = read_level {
        settings.insert("defaultReadConcern", doc! {"level": level});
    }
    let write_doc = match write {
        Given::Value(wc) => Some(wc.stored),
        Given::Unset => None,
        Given::Absent => before
            .as_ref()
            .and_then(|d| d.get_document("defaultWriteConcern").ok())
            .cloned(),
    };
    if let Some(wc) = write_doc {
        settings.insert("defaultWriteConcern", wc);
    }
    // The time of the write: minted first so the document carries it, as
    // mongod's does the opTime of its own write.
    let storage = ctx.storage()?;
    settings.insert("updateOpTime", storage.current_cluster_time());
    settings.insert("updateWallClockTime", bson::DateTime::now());
    storage
        .update_matching(
            SETTINGS_DB,
            SETTINGS_COLL,
            &doc! {"_id": SETTINGS_ID},
            &settings,
            false,
            true,
        )
        .map_err(crate::util::command_error)?;
    if let Some(p) = &ctx.server_params {
        p.cache_rw_defaults(Some(settings.clone()));
    }
    Ok(reply(Some(&settings), None))
}
