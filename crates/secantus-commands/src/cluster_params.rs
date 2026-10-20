//! Cluster parameters: `setClusterParameter`, `getClusterParameter` and the
//! one parameter that changes what this server does, `defaultMaxTimeMS`
//! (RM-219).
//!
//! mongod 8.2.11 keeps each parameter that has been set as a document in
//! `config.clusterParameters`, `{_id: <name>, clusterParameterTime, ...the
//! fields as sent}`, and reads it back parsed: a number field of the
//! `safeInt64` kind comes back as a long, a `null` or missing field as its
//! default. A parameter never set is its default, stamped with the null
//! timestamp on a set member. A document written to the collection directly
//! is read the same way, and deleting one returns the parameter to its
//! default -- this server reads the collection on every `getClusterParameter`,
//! so both hold here too.
//!
//! Every rule and message below was measured on a one-member replica set and
//! a standalone (2026-10-20). Of the fifteen parameters, only
//! `defaultMaxTimeMS` acts on a single node: it is the time limit of a read
//! (`find`, `aggregate` without `$out` / `$merge`, `count`, `distinct`) that
//! names no `maxTimeMS`, for an authenticated user without the
//! `bypassDefaultMaxTimeMS` privilege -- with authorization off, every user
//! has it, so the parameter does nothing (measured: a query of 150 ms over
//! `readOperations: 1` answered as an admin and failed with 50 as a
//! `readWrite` user). The others are kept and reported, as mongod keeps
//! them; what they govern (sharding, queryable encryption, search, the
//! serverless change-stream collection) is not on a single node.

use std::cmp::Ordering;

use bson::{doc, Bson, Document};

use crate::{CommandContext, CommandError, HandlerResult};

const PARAMS_DB: &str = "config";
const PARAMS_COLL: &str = "clusterParameters";

/// How a field is parsed.
#[derive(Clone, Copy)]
enum Kind {
    Bool,
    /// An int32 that must be above zero.
    PositiveInt,
    /// A double, with mongod's bounds.
    Double {
        gt: Option<f64>,
        ge: Option<f64>,
        le: Option<f64>,
    },
    /// mongod's `safeInt64`: any number, a double truncated, a decimal
    /// rounded half to even, both saturated. `types` is the expected-types
    /// list as mongod names it for this field.
    SafeInt64 {
        types: &'static str,
        check: Check,
    },
    /// A duration in milliseconds: a whole number that fits an int64.
    Duration,
}

#[derive(Clone, Copy)]
enum Check {
    /// `>= 0`, reported as a bound.
    NonNegative,
    /// `> 0`, reported as `changeStreams` reports it.
    Positive,
}

#[derive(Clone, Copy)]
enum Default {
    Bool(bool),
    Int(i32),
    Long(i64),
    Double(f64),
}

impl Default {
    fn bson(self) -> Bson {
        match self {
            Default::Bool(b) => Bson::Boolean(b),
            Default::Int(i) => Bson::Int32(i),
            Default::Long(i) => Bson::Int64(i),
            Default::Double(d) => Bson::Double(d),
        }
    }
}

struct Field {
    name: &'static str,
    kind: Kind,
    default: Default,
}

struct Param {
    name: &'static str,
    fields: &'static [Field],
}

const fn bool_field(name: &'static str, default: bool) -> Field {
    Field {
        name,
        kind: Kind::Bool,
        default: Default::Bool(default),
    }
}

const fn positive_int(name: &'static str, default: i32) -> Field {
    Field {
        name,
        kind: Kind::PositiveInt,
        default: Default::Int(default),
    }
}

/// The name `changeStreamOptions`, parsed by its own rules (`change_stream_options`).
const CHANGE_STREAM_OPTIONS: &str = "changeStreamOptions";

/// mongod 8.2.11's fifteen cluster parameters, in its name order.
const PARAMS: &[Param] = &[
    Param {
        name: "addOrRemoveShardInProgress",
        fields: &[bool_field("inProgress", false)],
    },
    Param {
        name: CHANGE_STREAM_OPTIONS,
        fields: &[],
    },
    Param {
        name: "changeStreams",
        fields: &[Field {
            name: "expireAfterSeconds",
            kind: Kind::SafeInt64 {
                types: "[decimal, double, long, int]",
                check: Check::Positive,
            },
            default: Default::Long(3600),
        }],
    },
    Param {
        name: "configServerReadPreferenceForCatalogQueries",
        fields: &[bool_field("mustAlwaysUseNearest", false)],
    },
    Param {
        name: "ddlLockOptimisticRecoveryWaitTimeout",
        fields: &[Field {
            name: "timeInMs",
            kind: Kind::Duration,
            default: Default::Long(100),
        }],
    },
    Param {
        name: "defaultMaxTimeMS",
        fields: &[Field {
            name: "readOperations",
            kind: Kind::SafeInt64 {
                types: "[int, decimal, long, double]",
                check: Check::NonNegative,
            },
            default: Default::Long(0),
        }],
    },
    Param {
        name: "fleAllowTotalTagOverheadToExceedBSONLimit",
        fields: &[bool_field("shouldOverride", false)],
    },
    Param {
        name: "fleCompactionOptions",
        fields: &[
            positive_int("maxCompactionSize", 268_435_456),
            positive_int("maxAnchorCompactionSize", 268_435_456),
            positive_int("maxESCEntriesPerCompactionDelete", 350_000),
        ],
    },
    Param {
        name: "fleDisableSubstringPreviewParameterLimits",
        fields: &[bool_field("shouldOverride", false)],
    },
    Param {
        name: "internalQueryCutoffForSampleFromRandomCursor",
        fields: &[Field {
            name: "sampleCutoff",
            kind: Kind::Double {
                gt: Some(0.0),
                ge: None,
                le: Some(1.0),
            },
            default: Default::Double(0.05),
        }],
    },
    Param {
        name: "internalSearchOptions",
        fields: &[
            Field {
                name: "oversubscriptionFactor",
                kind: Kind::Double {
                    gt: None,
                    ge: Some(1.0),
                    le: None,
                },
                default: Default::Double(1.064),
            },
            Field {
                name: "batchSizeGrowthFactor",
                kind: Kind::Double {
                    gt: None,
                    ge: Some(1.0),
                    le: None,
                },
                default: Default::Double(1.5),
            },
        ],
    },
    Param {
        name: "internalVectorSearchStoredSource",
        fields: &[bool_field("enabled", true)],
    },
    Param {
        name: "onlyTargetDataOwningShardsForMultiWrites",
        fields: &[bool_field("enabled", false)],
    },
    Param {
        name: "pauseMigrationsDuringMultiUpdates",
        fields: &[bool_field("enabled", false)],
    },
    Param {
        name: "shardedClusterCardinalityForDirectConns",
        fields: &[bool_field("hasTwoOrMoreShards", false)],
    },
];

fn type_name(v: &Bson) -> &'static str {
    secantus_core::query::bson_type_name(v)
}

fn error(code: i32, msg: impl Into<String>) -> CommandError {
    CommandError::new(code, crate::util::error_code_name(code), msg)
}

/// A number as mongod's bound messages render it.
fn render_number(v: &Bson) -> String {
    match v {
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(d) => secantus_core::expressions::format_double_g(*d),
        Bson::Decimal128(d) => d.to_string(),
        other => format!("{other}"),
    }
}

/// mongod's `safeInt64` of a number: `None` for a non-number.
fn safe_int64(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) => Some(saturate_f64(*d)),
        Bson::Decimal128(_) => secantus_core::decimal_to_i64_saturating(v),
        _ => None,
    }
}

fn saturate_f64(d: f64) -> i64 {
    if d.is_nan() {
        0
    } else if d >= 9_223_372_036_854_775_808.0 {
        i64::MAX
    } else if d < -9_223_372_036_854_775_808.0 {
        i64::MIN
    } else {
        d.trunc() as i64
    }
}

/// A duration field's value, or mongod's error for it.
fn duration(field: &str, v: &Bson) -> Result<i64, CommandError> {
    let whole = |d: &Bson| format!("{field}: {}", render_number(d));
    match v {
        Bson::Int32(i) => Ok(i64::from(*i)),
        Bson::Int64(i) => Ok(*i),
        Bson::Double(d) => {
            if d.is_nan() {
                Err(error(
                    9,
                    format!("Expected an integer, but found NaN in: {}", whole(v)),
                ))
            } else if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(d) {
                Err(error(
                    9,
                    format!("Cannot represent as a 64-bit integer: {}", whole(v)),
                ))
            } else if d.fract() != 0.0 {
                Err(error(9, format!("Expected an integer: {}", whole(v))))
            } else {
                Ok(*d as i64)
            }
        }
        Bson::Decimal128(_) => secantus_core::decimal_to_i64_exact(v).ok_or_else(|| {
            error(
                9,
                format!("Cannot represent as a 64-bit integer: {}", whole(v)),
            )
        }),
        other => Err(error(
            2,
            format!("Duration value must be numeric, got: {}", type_name(other)),
        )),
    }
}

/// Check one field of a value being set. `prefix` is mongod's
/// `Failed parsing ClusterServerParameter '<name>' :: caused by :: `.
fn check_field(f: &Field, v: &Bson, prefix: &str) -> Result<(), CommandError> {
    let wrong = |expected: &str| {
        error(
            14,
            format!(
                "{prefix}BSON field 'ClusterServerParameter.{}' is the wrong type '{}', \
                 expected {expected}",
                f.name,
                type_name(v)
            ),
        )
    };
    let bound = |op: &str, limit: &str, actual: String| {
        error(
            2,
            format!(
                "{prefix}BSON field '{}' value must be {op} {limit}, actual value '{actual}'",
                f.name
            ),
        )
    };
    match f.kind {
        Kind::Bool => match v {
            Bson::Boolean(_) => Ok(()),
            _ => Err(wrong("type 'bool'")),
        },
        Kind::PositiveInt => match v {
            Bson::Int32(i) if *i > 0 => Ok(()),
            Bson::Int32(i) => Err(bound(">", "0", i.to_string())),
            _ => Err(wrong("type 'int'")),
        },
        Kind::Double { gt, ge, le } => {
            let Bson::Double(d) = v else {
                return Err(wrong("type 'double'"));
            };
            let shown = || secantus_core::expressions::format_double_g(*d);
            let limit = |l: f64| secantus_core::expressions::format_double_g(l);
            if let Some(l) = gt {
                if d.partial_cmp(&l) != Some(Ordering::Greater) {
                    return Err(bound(">", &limit(l), shown()));
                }
            }
            if let Some(l) = ge {
                if !matches!(d.partial_cmp(&l), Some(Ordering::Greater | Ordering::Equal)) {
                    return Err(bound(">=", &limit(l), shown()));
                }
            }
            if let Some(l) = le {
                if !matches!(d.partial_cmp(&l), Some(Ordering::Less | Ordering::Equal)) {
                    return Err(bound("<=", &limit(l), shown()));
                }
            }
            Ok(())
        }
        Kind::SafeInt64 { types, check } => {
            let Some(n) = safe_int64(v) else {
                return Err(wrong(&format!("types '{types}'")));
            };
            match check {
                Check::NonNegative if n < 0 => Err(bound(">=", "0", n.to_string())),
                // The only message here without the prefix.
                Check::Positive if n <= 0 => Err(error(
                    2,
                    format!("Expected a positive integer for '{}' field", f.name),
                )),
                _ => Ok(()),
            }
        }
        Kind::Duration => duration(f.name, v).map(|_| ()).map_err(|mut e| {
            e.errmsg = format!("{prefix}{}", e.errmsg);
            e
        }),
    }
}

/// A field's value as `getClusterParameter` reports it.
fn parsed_field(f: &Field, v: Option<&Bson>) -> Bson {
    let Some(v) = v.filter(|v| !matches!(v, Bson::Null)) else {
        return f.default.bson();
    };
    match f.kind {
        Kind::SafeInt64 { .. } => safe_int64(v).map(Bson::Int64),
        Kind::Duration => duration(f.name, v).ok().map(Bson::Int64),
        _ => Some(v.clone()),
    }
    .unwrap_or_else(|| f.default.bson())
}

/// Check a value for an ordinary parameter, in its fields' order, as
/// mongod's parser walks it.
fn check_value(p: &Param, value: &Document) -> Result<(), CommandError> {
    let prefix = format!(
        "Failed parsing ClusterServerParameter '{}' :: caused by :: ",
        p.name
    );
    let mut duplicate_time = false;
    for (k, v) in value {
        if k == "clusterParameterTime" {
            // mongod appends its own time and parses the result, so a field
            // of that name is a duplicate of the one it adds.
            duplicate_time = true;
            continue;
        }
        let Some(f) = p.fields.iter().find(|f| f.name == k) else {
            return Err(error(
                40415,
                format!("{prefix}BSON field 'ClusterServerParameter.{k}' is an unknown field."),
            ));
        };
        // A null is the default, except for a duration, which must be a
        // number.
        if !matches!(v, Bson::Null) || matches!(f.kind, Kind::Duration) {
            check_field(f, v, &prefix)?;
        }
    }
    if duplicate_time {
        return Err(error(
            40413,
            format!(
                "{prefix}BSON field 'ClusterServerParameter.clusterParameterTime' is a \
                 duplicate field"
            ),
        ));
    }
    Ok(())
}

/// `changeStreamOptions`'s own parser: `{preAndPostImages: {expireAfterSeconds:
/// "off" | <positive number>}}`. Its messages run mongod's prefix into the
/// cause with no separator, as mongod's do.
fn check_change_stream_options(value: &Document) -> Result<(), CommandError> {
    const PREFIX: &str = "Failed parsing new changeStreamOptions value";
    let bad = |msg: String| error(2, msg);
    let images = match value.get("preAndPostImages") {
        None => {
            return Err(bad(format!(
                "{PREFIX}BSON field 'changeStreamOptions.preAndPostImages' is missing but a \
                 required field"
            )))
        }
        Some(Bson::Document(d)) => d,
        Some(_) => {
            return Err(bad(format!(
                "{PREFIX}invalid parameter: expected an object (preAndPostImages)"
            )))
        }
    };
    if let Some(k) = value.keys().find(|k| *k != "preAndPostImages") {
        return Err(bad(format!(
            "{PREFIX}BSON field 'changeStreamOptions.{k}' is an unknown field."
        )));
    }
    let mut expire = None;
    for (k, v) in images {
        if k != "expireAfterSeconds" {
            return Err(bad(format!(
                "{PREFIX}BSON field 'changeStreamOptions.preAndPostImages.{k}' is an unknown \
                 field."
            )));
        }
        expire = Some(v);
    }
    match expire {
        // mongod names the field without its parent here.
        None => Err(bad(format!(
            "{PREFIX}BSON field 'changeStreamOptions.expireAfterSeconds' is missing but a \
             required field"
        ))),
        Some(Bson::String(s)) if s == "off" => Ok(()),
        Some(Bson::String(_)) => Err(bad(
            "Non-numeric value of 'expireAfterSeconds' should be 'off'".to_string(),
        )),
        Some(v) => match safe_int64(v) {
            Some(n) if n > 0 => Ok(()),
            Some(_) => Err(bad(
                "Numeric value of 'expireAfterSeconds' should be positive".to_string(),
            )),
            None => Err(bad(format!(
                "{PREFIX}BSON field 'changeStreamOptions.preAndPostImages.expireAfterSeconds' \
                 is the wrong type '{}', expected types '[string, long]'",
                type_name(v)
            ))),
        },
    }
}

/// `changeStreamOptions` as `getClusterParameter` reports it.
fn parsed_change_stream_options(stored: Option<&Document>) -> Document {
    let expire = stored
        .and_then(|d| d.get_document("preAndPostImages").ok())
        .and_then(|d| d.get("expireAfterSeconds"))
        .and_then(|v| match v {
            Bson::String(s) => Some(Bson::String(s.clone())),
            other => safe_int64(other).map(Bson::Int64),
        })
        .unwrap_or_else(|| Bson::String("off".into()));
    doc! {"preAndPostImages": {"expireAfterSeconds": expire}}
}

/// A parameter's fields as `getClusterParameter` reports them, from its
/// stored document if it has one.
fn parsed(p: &Param, stored: Option<&Document>) -> Document {
    if p.name == CHANGE_STREAM_OPTIONS {
        return parsed_change_stream_options(stored);
    }
    let mut out = Document::new();
    for f in p.fields {
        out.insert(f.name, parsed_field(f, stored.and_then(|d| d.get(f.name))));
    }
    out
}

/// Every stored parameter document, by name.
fn stored(ctx: &CommandContext) -> Result<Vec<Document>, CommandError> {
    let rows = ctx
        .storage()?
        .find(PARAMS_DB, PARAMS_COLL, &Document::new(), None, None)
        .map_err(crate::util::command_error)?;
    Ok(rows
        .iter()
        .filter_map(|raw| crate::bsondoc::from_bytes(raw).ok())
        .collect())
}

fn stored_one(ctx: &CommandContext, name: &str) -> Result<Option<Document>, CommandError> {
    let rows = ctx
        .storage()?
        .find(PARAMS_DB, PARAMS_COLL, &doc! {"_id": name}, None, None)
        .map_err(crate::util::command_error)?;
    Ok(rows
        .first()
        .and_then(|raw| crate::bsondoc::from_bytes(raw).ok()))
}

/// `setClusterParameter`.
pub fn set_cluster_parameter(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let arg = match doc.get("setClusterParameter") {
        Some(Bson::Document(d)) => d,
        None | Some(Bson::Null) => {
            return Err(error(
                40414,
                "BSON field 'setClusterParameter.setClusterParameter' is missing but a \
                 required field",
            ))
        }
        Some(other) => {
            return Err(error(
                14,
                format!(
                    "BSON field 'setClusterParameter.setClusterParameter' is the wrong type \
                     '{}', expected type 'object'",
                    type_name(other)
                ),
            ))
        }
    };
    let mut entries = arg.iter();
    let (name, value) = match (entries.next(), entries.next()) {
        (None, _) => return Err(error(4, "No cluster parameter provided")),
        (Some(_), Some(_)) => {
            return Err(error(
                72,
                "setClusterParameter only supports setting exactly one parameter",
            ))
        }
        (Some(one), None) => one,
    };
    let Some(param) = PARAMS.iter().find(|p| p.name == name) else {
        return Err(error(4, format!("Unknown server parameter: {name}")));
    };
    let Bson::Document(value) = value else {
        return Err(error(2, "Cluster parameter value must be an object"));
    };
    if value.contains_key("_id") {
        return Err(error(2, "Can't have multiple _id fields in one document"));
    }
    if param.name == CHANGE_STREAM_OPTIONS {
        check_change_stream_options(value)?;
    } else {
        check_value(param, value)?;
    }
    let storage = ctx.storage()?;
    let mut row = doc! {"_id": name, "clusterParameterTime": storage.current_cluster_time()};
    row.extend(value.clone());
    storage
        .update_matching(
            PARAMS_DB,
            PARAMS_COLL,
            &doc! {"_id": name},
            &row,
            false,
            true,
        )
        .map_err(crate::util::command_error)?;
    Ok(doc! {"ok": 1.0})
}

/// The names `getClusterParameter` asks for: `None` for `"*"`.
fn requested_names(doc: &Document) -> Result<Option<Vec<String>>, CommandError> {
    let wrong = |v: &Bson| {
        error(
            14,
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
                return Err(error(
                    2,
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
        return Err(error(
            13,
            "getClusterParameter may only be run against the admin database.",
        ));
    }
    let chosen: Vec<&Param> = match &names {
        None => PARAMS.iter().collect(),
        Some(names) => names
            .iter()
            .map(|name| {
                PARAMS
                    .iter()
                    .find(|p| p.name == name)
                    .ok_or_else(|| error(4, format!("Unknown server parameter: {name}")))
            })
            .collect::<Result<_, _>>()?,
    };
    let rows = stored(ctx)?;
    let set_member = ctx.replica_set_name.is_some();
    let out: Vec<Bson> = chosen
        .into_iter()
        .map(|p| {
            let row = rows.iter().find(|r| r.get_str("_id") == Ok(p.name));
            let mut d = doc! {"_id": p.name};
            // A set member stamps each parameter with the time it was set,
            // the null timestamp for one never set; a standalone leaves the
            // time out.
            if set_member {
                let time = row
                    .and_then(|r| r.get("clusterParameterTime"))
                    .filter(|t| matches!(t, Bson::Timestamp(_)))
                    .cloned()
                    .unwrap_or(Bson::Timestamp(bson::Timestamp {
                        time: 0,
                        increment: 0,
                    }));
                d.insert("clusterParameterTime", time);
            }
            d.extend(parsed(p, row));
            Bson::Document(d)
        })
        .collect();
    Ok(doc! {"clusterParameters": out, "ok": 1.0})
}

/// The reads `defaultMaxTimeMS` limits: `find`, an `aggregate` that writes
/// nothing, `count` and `distinct` (measured: an `aggregate` with `$out`, the
/// write commands and `findAndModify` ran past it).
fn is_limited_read(doc: &Document) -> bool {
    match doc.keys().next().map(String::as_str) {
        Some("find" | "count" | "distinct") => true,
        Some("aggregate") => !doc
            .get_array("pipeline")
            .ok()
            .and_then(|p| p.last())
            .and_then(Bson::as_document)
            .is_some_and(|s| s.contains_key("$out") || s.contains_key("$merge")),
        _ => false,
    }
}

/// The time limit `defaultMaxTimeMS` puts on `doc`, in milliseconds, or 0
/// for none: only a limited read with no `maxTimeMS` of its own (even 0, which
/// is no limit), run by an authenticated user who lacks
/// `bypassDefaultMaxTimeMS`.
pub(crate) fn default_read_budget(doc: &Document, ctx: &CommandContext) -> i64 {
    if !ctx.require_auth || doc.contains_key("maxTimeMS") || !is_limited_read(doc) {
        return 0;
    }
    let authenticated = ctx.conn_auth.as_ref().is_some_and(|a| {
        a.lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_authenticated()
    });
    if !authenticated {
        return 0;
    }
    let bypass = crate::rbac::effective(ctx).map_or(true, |az| {
        az.is_authorized(
            &crate::privileges::Resource::Cluster,
            &["bypassDefaultMaxTimeMS"],
        )
    });
    if bypass {
        return 0;
    }
    let param = PARAMS
        .iter()
        .find(|p| p.name == "defaultMaxTimeMS")
        .expect("defaultMaxTimeMS is a cluster parameter");
    let Ok(row) = stored_one(ctx, param.name) else {
        return 0;
    };
    parsed(param, row.as_ref())
        .get("readOperations")
        .and_then(crate::util::as_i64)
        .unwrap_or(0)
        .max(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(s: &str) -> Bson {
        Bson::Decimal128(bson::Decimal128::from_str(s).unwrap())
    }

    /// mongod 8.2.11's `getClusterParameter` after setting each value.
    #[test]
    fn safe_int64_truncates_rounds_and_saturates() {
        for (v, want) in [
            (Bson::Double(5.5), 5),
            (Bson::Double(-0.5), 0),
            (Bson::Double(f64::NAN), 0),
            (Bson::Double(1e20), i64::MAX),
            (Bson::Double(-1e20), i64::MIN),
            (Bson::Double(f64::INFINITY), i64::MAX),
            (dec("7.9"), 8),
            (dec("5.7"), 6),
            (dec("0.4"), 0),
            (dec("NaN"), 0),
            (dec("1E+30"), i64::MAX),
            (Bson::Int32(7), 7),
        ] {
            assert_eq!(safe_int64(&v), Some(want), "{v:?}");
        }
        assert_eq!(safe_int64(&Bson::String("s".into())), None);
    }

    /// mongod 8.2.11's answers for `ddlLockOptimisticRecoveryWaitTimeout`.
    #[test]
    fn duration_takes_only_whole_numbers() {
        assert_eq!(duration("t", &Bson::Double(5.0)).unwrap(), 5);
        assert_eq!(duration("t", &dec("5")).unwrap(), 5);
        assert_eq!(duration("t", &dec("-0")).unwrap(), 0);
        for (v, code, msg) in [
            (Bson::Double(5.5), 9, "Expected an integer: t: 5.5"),
            (
                Bson::Double(f64::NAN),
                9,
                "Expected an integer, but found NaN in: t: nan",
            ),
            (
                Bson::Double(1e21),
                9,
                "Cannot represent as a 64-bit integer: t: 1e+21",
            ),
            (
                dec("7.9"),
                9,
                "Cannot represent as a 64-bit integer: t: 7.9",
            ),
            (Bson::Null, 2, "Duration value must be numeric, got: null"),
        ] {
            let e = duration("t", &v).unwrap_err();
            assert_eq!((e.code, e.errmsg.as_str()), (code, msg), "{v:?}");
        }
    }
}
