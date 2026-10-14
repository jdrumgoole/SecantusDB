//! The fields every command takes, checked the way mongod's parser checks them.
//!
//! Measured on mongod 8.2.11 with `tools/probes/diagnostic_commands.py`
//! (2026-10-13). Before this, a wrong-typed `$readPreference`, `$clusterTime`
//! or `apiStrict` was read as absent, and a command given a field it does not
//! have (`{ping: 1, bogus: 1}`) ran as though the field were not there.
//!
//! Three rules, each measured per command and not assumed:
//!
//! - The generic fields are typed on EVERY command, `hello` included.
//! - Most commands refuse a field they do not know (40415). A dozen do not:
//!   `hello`, `serverStatus`, `whatsmyuri`, `startSession`, `validate`,
//!   `getParameter`, `setParameter`, `top`, `currentOp`, `killOp`,
//!   `replSetGetStatus`, `killSessions`, `listCommands`, `dbHash`. The strict
//!   ones are listed here by name, so a command nobody measured stays loose.
//! - `apiStrict` and `apiDeprecationErrors` are bools and nothing else, and
//!   either one without `apiVersion` is refused.

use bson::{Bson, Document};
use secantus_core::query::bson_type_name;

use crate::CommandError;

/// The fields mongod's parser takes on any command (`ping` given each name in
/// turn, 8.2.11). `$db` is the wire's own.
pub(crate) const GENERIC: &[&str] = &[
    "$audit",
    "$client",
    "$clusterTime",
    "$configServerState",
    "$configTime",
    "$db",
    "$oplogQueryData",
    "$queryOptions",
    "$readPreference",
    "$replData",
    "$topologyTime",
    "allowImplicitCollectionCreation",
    "apiDeprecationErrors",
    "apiStrict",
    "apiVersion",
    "autocommit",
    "clientOperationKey",
    "comment",
    "coordinator",
    "databaseVersion",
    "expectPrefix",
    "help",
    "lsid",
    "maxTimeMS",
    "maxTimeMSOpOnly",
    "mayBypassWriteBlocking",
    "rawData",
    "readConcern",
    "requestGossipRoutingCache",
    "shardVersion",
    "startOrContinueTransaction",
    "startTransaction",
    "stmtId",
    "tracking_info",
    "txnNumber",
    "txnRetryCounter",
    "usesDefaultMaxTimeMS",
    "versionContext",
    "writeConcern",
];

/// Commands measured to refuse a `$`-field they do not know. Their own
/// handlers already refuse an unknown plain field, or `OWN_FIELDS` does.
const STRICT: &[&str] = &[
    "abortTransaction",
    "aggregate",
    "buildInfo",
    "buildinfo",
    "bulkWrite",
    "collMod",
    "collStats",
    "collstats",
    "commitTransaction",
    "compact",
    "connectionStatus",
    "count",
    "create",
    "createIndexes",
    "dataSize",
    "datasize",
    "dbStats",
    "dbstats",
    "delete",
    "distinct",
    "drop",
    "dropDatabase",
    "dropIndexes",
    "endSessions",
    "explain",
    "features",
    "find",
    "findAndModify",
    "findandmodify",
    "fsync",
    "getCmdLineOpts",
    "getLog",
    "getMore",
    "hostInfo",
    "insert",
    "killCursors",
    "listCollections",
    "listDatabases",
    "listIndexes",
    "logout",
    "mapReduce",
    "mapreduce",
    "ping",
    "profile",
    "refreshSessions",
    "renameCollection",
    "rolesInfo",
    "update",
    "usersInfo",
];

/// The whole field list of the strict commands whose handlers do not check
/// it themselves (each name sent to mongod with a value no field takes).
fn own_fields(command: &str) -> Option<&'static [&'static str]> {
    Some(match command {
        "ping" | "buildInfo" | "buildinfo" | "hostInfo" | "getLog" | "getCmdLineOpts"
        | "logout" => &[],
        "connectionStatus" => &["showPrivileges"],
        "fsync" => &["lock", "fsyncLockAcquisitionTimeoutMillis"],
        "profile" => &["slowms", "sampleRate", "filter"],
        "usersInfo" => &[
            "showCredentials",
            "showCustomData",
            "showPrivileges",
            "showAuthenticationRestrictions",
            "filter",
        ],
        "rolesInfo" => &[
            "showPrivileges",
            "showBuiltinRoles",
            "showAuthenticationRestrictions",
        ],
        "dataSize" | "datasize" => &["keyPattern", "min", "max", "estimate"],
        "features" => &["oidReset"],
        "createIndexes" => &[
            "v",
            "indexes",
            "ignoreUnknownIndexOptions",
            "commitQuorum",
            "returnOnStart",
            "isTimeseriesNamespace",
            "collectionUUID",
        ],
        "dropIndexes" => &["index", "isTimeseriesNamespace", "collectionUUID"],
        "mapReduce" | "mapreduce" => &[
            "map",
            "reduce",
            "out",
            "query",
            "sort",
            "collation",
            "limit",
            "finalize",
            "scope",
            "jsMode",
            "verbose",
            "bypassDocumentValidation",
        ],
        "compact" => &["force", "freeSpaceTargetMB", "dryRun"],
        _ => return None,
    })
}

/// The name mongod's parser gives a command in a message about its fields.
pub(crate) fn struct_name(command: &str) -> &str {
    match command {
        "find" => "FindCommandRequest",
        "buildinfo" => "buildInfo",
        "collstats" => "collStats",
        "datasize" => "dataSize",
        "dbstats" => "dbStats",
        "findandmodify" => "findAndModify",
        "mapreduce" => "mapReduce",
        other => other,
    }
}

fn wrong_type(path: &str, value: &Bson, expected: &str) -> CommandError {
    CommandError::new(
        14,
        "TypeMismatch",
        format!(
            "BSON field '{path}' is the wrong type '{}', expected {expected}",
            bson_type_name(value)
        ),
    )
}

fn missing(path: &str) -> CommandError {
    CommandError::new(
        40414,
        "IDLFailedToParse",
        format!("BSON field '{path}' is missing but a required field"),
    )
}

fn unknown(path: &str) -> CommandError {
    CommandError::new(
        40415,
        "IDLUnknownField",
        format!("BSON field '{path}' is an unknown field."),
    )
}

/// Check the generic fields of `doc`, in the order they were sent, and then
/// that a strict command was given no field it does not know.
pub(crate) fn check(doc: &Document, command: &str) -> Result<(), CommandError> {
    let name = struct_name(command);
    // `distinct`'s parser calls itself by another name in this one message.
    let unknown_in = if command == "distinct" {
        "distinctCommandRequest"
    } else {
        name
    };
    let strict = STRICT.contains(&command);
    let own = own_fields(command);
    let mut api_version = false;
    let mut api_flag = false;
    for (i, (key, value)) in doc.iter().enumerate() {
        if i == 0 {
            continue;
        }
        let path = format!("{name}.{key}");
        if matches!(value, Bson::Null) {
            // A null is an absent field to this parser, for every field
            // checked here.
            if !GENERIC.contains(&key.as_str()) && key.starts_with('$') && strict {
                return Err(unknown(&format!("{unknown_in}.{key}")));
            }
            continue;
        }
        match (key.as_str(), value) {
            ("apiVersion", Bson::String(_)) => api_version = true,
            ("apiVersion", v) => return Err(wrong_type(&path, v, "type 'string'")),
            ("apiStrict" | "apiDeprecationErrors", Bson::Boolean(_)) => api_flag = true,
            ("apiStrict" | "apiDeprecationErrors", v) => {
                return Err(wrong_type(&path, v, "type 'bool'"))
            }
            ("$readPreference", Bson::Document(pref)) => read_preference(pref)?,
            ("$clusterTime", Bson::Document(time)) => {
                for (field, v) in time {
                    match (field.as_str(), v) {
                        ("clusterTime", Bson::Timestamp(_)) | ("signature", Bson::Document(_)) => {}
                        ("clusterTime", v) => {
                            return Err(wrong_type(
                                &format!("{path}.clusterTime"),
                                v,
                                "type 'timestamp'",
                            ))
                        }
                        ("signature", v) => {
                            return Err(wrong_type(
                                &format!("{path}.signature"),
                                v,
                                "type 'object'",
                            ))
                        }
                        _ => {}
                    }
                }
                if !time.contains_key("clusterTime") {
                    return Err(missing(&format!("{path}.clusterTime")));
                }
            }
            ("$client", client @ Bson::Document(_)) => {
                crate::handshake::validate_client_metadata(client, false)?
            }
            ("$audit" | "$configServerState", Bson::Document(_)) => {}
            (
                "$readPreference" | "$clusterTime" | "$client" | "$audit" | "$configServerState",
                v,
            ) => return Err(wrong_type(&path, v, "type 'object'")),
            (key, _) if GENERIC.contains(&key) => {}
            (key, _) if strict && key.starts_with('$') => {
                return Err(unknown(&format!("{unknown_in}.{key}")))
            }
            (key, _) => {
                if own.is_some_and(|fields| !fields.contains(&key)) {
                    return Err(unknown(&format!("{unknown_in}.{key}")));
                }
            }
        }
    }
    if api_flag && !api_version {
        return Err(CommandError::new(
            4886600,
            "Location4886600",
            "Provided apiStrict and/or apiDeprecationErrors without passing apiVersion",
        ));
    }
    Ok(())
}

/// `$readPreference`, as mongod reads it on a command sent to itself: the
/// document is parsed and its combinations checked, and then any mode is
/// served, because the node the command reached is the one that answers.
fn read_preference(pref: &Document) -> Result<(), CommandError> {
    let bad_value = |message: &str| CommandError::new(2, "BadValue", message);
    let mut mode = None;
    let mut tagged = false;
    let mut staleness = None;
    for (key, value) in pref {
        let path = format!("readPreference.{key}");
        match (key.as_str(), value) {
            (_, Bson::Null) => {}
            ("mode", Bson::String(name)) => {
                if !matches!(
                    name.as_str(),
                    "primary" | "primaryPreferred" | "secondary" | "secondaryPreferred" | "nearest"
                ) {
                    return Err(bad_value(&format!(
                        "Enumeration value '{name}' for field 'readPreference.mode' is not a \
                         valid value."
                    )));
                }
                mode = Some(name.as_str());
            }
            ("mode", v) => return Err(wrong_type(&path, v, "type 'string'")),
            ("tags", Bson::Array(tags)) => {
                tagged = tags
                    .iter()
                    .any(|tag| tag.as_document().is_none_or(|d| !d.is_empty()));
            }
            ("tags", v) => return Err(wrong_type(&path, v, "type 'array'")),
            ("maxStalenessSeconds", v) => match crate::util::as_i64(v) {
                Some(seconds) => staleness = Some(seconds),
                None => return Err(wrong_type(&path, v, "types '[double, int, long, decimal]'")),
            },
            ("hedge", Bson::Document(_)) => {}
            ("hedge", v) => return Err(wrong_type(&path, v, "type 'object'")),
            _ => {}
        }
    }
    let Some(mode) = mode else {
        return Err(missing("readPreference.mode"));
    };
    if mode == "primary" {
        if tagged {
            return Err(bad_value(
                "Only empty tags are allowed with primary read preference",
            ));
        }
        if staleness.is_some_and(|s| s != 0) {
            return Err(bad_value(
                "maxStalenessSeconds can not be set for the primary mode",
            ));
        }
    } else if staleness.is_some_and(|s| s != 0 && s < 90) {
        return Err(CommandError::new(
            160,
            "MaxStalenessOutOfRange",
            "maxStalenessSeconds value cannot be less than 90",
        ));
    }
    Ok(())
}

/// The reply to an OP_MSG whose `$db` is absent or not a string, or `None`
/// when it is fine. An unknown command is left to say so itself.
pub fn db_field_problem(request: &Document) -> Option<Document> {
    let command = crate::command_name(request);
    crate::lookup(command)?;
    let path = format!("{}.$db", struct_name(command));
    match request.get("$db") {
        Some(Bson::String(_)) => None,
        None | Some(Bson::Null) => Some(missing(&path).into_reply()),
        Some(v) => Some(wrong_type(&path, v, "type 'string'").into_reply()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn refusal(doc: Document) -> (i32, String) {
        let name = doc.keys().next().unwrap().clone();
        let e = check(&doc, &name).unwrap_err();
        (e.code, e.errmsg)
    }

    #[test]
    fn a_strict_command_refuses_a_field_it_does_not_know() {
        assert_eq!(
            refusal(doc! {"ping": 1, "bogus": 1}),
            (40415, "BSON field 'ping.bogus' is an unknown field.".into())
        );
        assert_eq!(
            refusal(doc! {"find": "c", "$bogus": 1}).1,
            "BSON field 'FindCommandRequest.$bogus' is an unknown field."
        );
        assert_eq!(
            refusal(doc! {"distinct": "c", "$bogus": 1}).1,
            "BSON field 'distinctCommandRequest.$bogus' is an unknown field."
        );
        // The loose commands take anything, and so does a generic field.
        assert!(check(&doc! {"serverStatus": 1, "$bogus": 1}, "serverStatus").is_ok());
        assert!(check(&doc! {"ping": 1, "comment": 1, "$queryOptions": {}}, "ping").is_ok());
    }

    #[test]
    fn the_api_fields_are_typed_and_need_a_version() {
        assert_eq!(refusal(doc! {"ping": 1, "apiVersion": 1}).0, 14);
        assert_eq!(
            refusal(doc! {"ping": 1, "apiVersion": "1", "apiStrict": 0}).1,
            "BSON field 'ping.apiStrict' is the wrong type 'int', expected type 'bool'"
        );
        assert_eq!(refusal(doc! {"ping": 1, "apiStrict": false}).0, 4886600);
        assert!(check(
            &doc! {"ping": 1, "apiVersion": null, "apiStrict": null},
            "ping"
        )
        .is_ok());
    }

    #[test]
    fn a_read_preference_is_parsed_even_though_any_mode_is_served() {
        let with = |pref: Document| refusal(doc! {"find": "c", "$readPreference": pref});
        assert_eq!(with(doc! {}).0, 40414);
        assert_eq!(with(doc! {"mode": "bogus"}).0, 2);
        assert_eq!(with(doc! {"mode": "primary", "tags": [{"a": "b"}]}).0, 2);
        assert_eq!(
            with(doc! {"mode": "secondary", "maxStalenessSeconds": 1}).0,
            160
        );
        assert_eq!(
            with(doc! {"mode": "primary", "maxStalenessSeconds": 90}).0,
            2
        );
        let fine = doc! {"find": "c", "$readPreference": {"mode": "secondary", "tags": [{}]}};
        assert!(check(&fine, "find").is_ok());
        assert_eq!(
            refusal(doc! {"find": "c", "$readPreference": 1}).1,
            "BSON field 'FindCommandRequest.$readPreference' is the wrong type 'int', expected \
             type 'object'"
        );
    }

    #[test]
    fn a_cluster_time_needs_its_timestamp() {
        assert_eq!(refusal(doc! {"ping": 1, "$clusterTime": {}}).0, 40414);
        assert_eq!(refusal(doc! {"hello": 1, "$clusterTime": 1}).0, 14);
    }
}
