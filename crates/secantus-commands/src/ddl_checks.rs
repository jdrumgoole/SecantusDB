//! What mongod refuses when it PARSES a DDL command, before it looks at the
//! catalog: a database name it cannot hold, and `create` options of the
//! wrong type or shape. Each answer was measured on mongod 8.2.11 with
//! `tools/probes/ddl_admin_commands.py`.

use crate::CommandError;
use bson::{Bson, Document};
use secantus_core::query::bson_type_name;

fn invalid_namespace(message: String) -> CommandError {
    CommandError::new(73, "InvalidNamespace", message)
}

/// mongod's refusal of a database name, for the command being run.
///
/// Every one of these was accepted, and a write created the database: `a.b`,
/// `a b`, the empty name. mongod words the refusal by what the command names:
/// a collection-scoped command reports the namespace, a database-scoped one
/// the database.
pub(crate) fn invalid_database_name(db: &str, doc: &Document) -> Option<CommandError> {
    if db.len() > 63 {
        return Some(invalid_namespace(format!(
            "db name must be at most 63 characters, found: {}",
            db.len()
        )));
    }
    if db.contains('.') {
        return Some(invalid_namespace(format!(
            "'.' is an invalid character in a db name: {db}"
        )));
    }
    let (name, target) = doc.iter().next()?;
    // A user or role command's first value names no collection.
    let coll = match target {
        Bson::String(coll) if !crate::umc::is_user_or_role_command(name) => Some(coll.as_str()),
        _ => None,
    };
    if db.is_empty() || db.contains(['/', '\\', ' ', '"']) {
        return Some(invalid_namespace(match coll {
            Some(coll) => format!("Invalid namespace specified: {db}.{coll}"),
            None => format!("Invalid database name: '{db}'"),
        }));
    }
    // A `$` is refused only where the command would create the namespace.
    if db.contains('$')
        && matches!(
            name.as_str(),
            "insert"
                | "update"
                | "delete"
                | "findAndModify"
                | "findandmodify"
                | "create"
                | "createIndexes"
        )
    {
        if let Some(coll) = coll {
            return Some(invalid_namespace(format!("Invalid namespace: {db}.{coll}")));
        }
    }
    None
}

/// A document as mongod prints one in a message: `{ a: 1 }`.
fn spaced(d: &Document) -> String {
    spaced_value(&Bson::Document(d.clone()))
}

/// A value as mongod prints one in a message. An array prints as the
/// document it is in BSON, keyed by position: `{ 0: { $match: {} } }`.
pub(crate) fn spaced_value(v: &Bson) -> String {
    let entries: Vec<(String, &Bson)> = match v {
        Bson::Document(d) => d.iter().map(|(k, x)| (k.clone(), x)).collect(),
        Bson::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, x)| (i.to_string(), x))
            .collect(),
        other => return secantus_core::aggregate::render_value_compact(other),
    };
    if entries.is_empty() {
        return "{}".to_string();
    }
    let parts: Vec<String> = entries
        .iter()
        .map(|(k, x)| format!("{k}: {}", spaced_value(x)))
        .collect();
    format!("{{ {} }}", parts.join(", "))
}

fn wrong_type(path: &str, v: &Bson, expected: &str) -> CommandError {
    CommandError::new(
        14,
        "TypeMismatch",
        format!(
            "BSON field '{path}' is the wrong type '{}', expected type '{expected}'",
            bson_type_name(v)
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

/// A field that must be a document when it is given at all.
fn object<'a>(
    doc: &'a Document,
    cmd: &str,
    field: &str,
) -> Result<Option<&'a Document>, CommandError> {
    match doc.get(field) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::Document(d)) => Ok(Some(d)),
        Some(other) => Err(wrong_type(&format!("{cmd}.{field}"), other, "object")),
    }
}

/// `changeStreamPreAndPostImages`, on `create` or `collMod`: a document with
/// a boolean `enabled` and nothing else.
pub(crate) fn pre_images(doc: &Document, cmd: &str) -> Result<(), CommandError> {
    let field = "changeStreamPreAndPostImages";
    let Some(spec) = object(doc, cmd, field)? else {
        return Ok(());
    };
    let path = format!("{cmd}.{field}");
    match spec.get("enabled") {
        None => return Err(missing(&format!("{path}.enabled"))),
        Some(Bson::Boolean(_)) => {}
        Some(other) => return Err(wrong_type(&format!("{path}.enabled"), other, "bool")),
    }
    match spec.keys().find(|k| *k != "enabled") {
        Some(extra) => Err(unknown(&format!("{path}.{extra}"))),
        None => Ok(()),
    }
}

/// A field that must be a string when it is given at all.
pub(crate) fn string(doc: &Document, cmd: &str, field: &str) -> Result<(), CommandError> {
    match doc.get(field) {
        None | Some(Bson::Null | Bson::String(_)) => Ok(()),
        Some(other) => Err(wrong_type(&format!("{cmd}.{field}"), other, "string")),
    }
}

/// The options of `create` mongod refuses on sight.
pub(crate) fn create(doc: &Document) -> Result<(), CommandError> {
    string(doc, "create", "validationLevel")?;
    string(doc, "create", "validationAction")?;
    pre_images(doc, "create")?;
    crate::argtypes::require_number(doc, "expireAfterSeconds", "create.expireAfterSeconds")?;
    object(doc, "create", "indexOptionDefaults")?;
    if doc.contains_key("autoIndexId") {
        return Err(unknown("create.autoIndexId"));
    }
    if doc.contains_key("temp") {
        return Err(CommandError::new(
            72,
            "InvalidOptions",
            "the 'temp' field is an invalid option",
        ));
    }
    if let Some(fields) = object(doc, "create", "encryptedFields")? {
        if !fields.contains_key("fields") {
            return Err(missing("create.encryptedFields.fields"));
        }
    }
    if doc.contains_key("recordIdsReplicated") {
        return Err(CommandError::new(
            115,
            "CommandNotSupported",
            "'recordIdsReplicated' option may not be run without \
             featureFlagRecordIdsReplicated enabled",
        ));
    }
    if let Some(engines) = object(doc, "create", "storageEngine")? {
        if let Some(other) = engines.keys().find(|k| *k != "wiredTiger") {
            return Err(CommandError::new(
                72,
                "InvalidOptions",
                format!("{other} is not a registered storage engine for this server"),
            ));
        }
    }
    if let Some(id_index) = object(doc, "create", "idIndex")? {
        let key = id_index.get_document("key").ok();
        let is_id = key
            .is_some_and(|k| k.len() == 1 && k.get("_id").and_then(crate::util::as_i64) == Some(1));
        if let (Some(key), false) = (key, is_id) {
            return Err(CommandError::new(
                2,
                "BadValue",
                format!(
                    "The field 'key' for an _id index must be {{_id: 1}}, but got {}",
                    spaced(key)
                ),
            ));
        }
    }
    match doc.get("clusteredIndex") {
        None | Some(Bson::Null) => {}
        Some(Bson::Document(spec)) => {
            if !spec.contains_key("unique") {
                return Err(missing("create.clusteredIndex.unique"));
            }
            if !matches!(doc.get("size"), None | Some(Bson::Null)) {
                return Err(CommandError::new(
                    6_049_200,
                    "Location6049200",
                    "'size' field for capped collections is not allowed on clustered \
                     collections. Did you mean 'capped: true' with 'expireAfterSeconds'?",
                ));
            }
        }
        Some(_) => {
            return Err(CommandError::new(
                5_979_703,
                "Location5979703",
                "The 'clusteredIndex' legacy format {clusteredIndex: <bool>} is only \
                 supported for specific internal collections and vice versa",
            ))
        }
    }
    if let Some(Bson::Array(stages)) = doc.get("pipeline") {
        if let Some((i, other)) = stages
            .iter()
            .enumerate()
            .find(|(_, s)| !matches!(s, Bson::Document(_)))
        {
            return Err(wrong_type(&format!("create.pipeline.{i}"), other, "object"));
        }
    }
    Ok(())
}

/// The options of `collMod` mongod refuses on sight.
pub(crate) fn coll_mod(doc: &Document) -> Result<(), CommandError> {
    string(doc, "collMod", "validationLevel")?;
    string(doc, "collMod", "validationAction")?;
    pre_images(doc, "collMod")?;
    match doc.get("expireAfterSeconds") {
        None
        | Some(
            Bson::Int32(_)
            | Bson::Int64(_)
            | Bson::Double(_)
            | Bson::Decimal128(_)
            | Bson::String(_),
        ) => {}
        Some(other) => {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'collMod.expireAfterSeconds' is the wrong type '{}', expected \
                     types '[long, string]'",
                    bson_type_name(other)
                ),
            ))
        }
    }
    if let Some(Bson::Document(index)) = doc.get("index") {
        if index.contains_key("name") && index.contains_key("keyPattern") {
            return Err(CommandError::new(
                72,
                "InvalidOptions",
                "Cannot specify both key pattern and name.",
            ));
        }
    }
    Ok(())
}

/// What `collMod` refuses once it knows the collection it is changing.
pub(crate) fn coll_mod_on(doc: &Document, options: &Document) -> Option<CommandError> {
    let refused = |m: &str| Some(CommandError::new(72, "InvalidOptions", m));
    let dry = doc.get("dryRun").is_some_and(crate::util::truthy);
    let converts = doc
        .get_document("index")
        .is_ok_and(|i| i.contains_key("unique"));
    if dry && !converts {
        return refused("dry run mode requires an valid index modification.");
    }
    if options.contains_key("timeseries") {
        return None;
    }
    match doc.get("expireAfterSeconds") {
        None => None,
        Some(_) if !options.contains_key("clusteredIndex") => {
            refused("'expireAfterSeconds' option is only supported on collections clustered by _id")
        }
        Some(v) if crate::util::as_i64(v).is_some_and(|n| n < 0) => {
            refused("TTL index 'expireAfterSeconds' option cannot be less than 0")
        }
        Some(_) => None,
    }
}

/// What `renameCollection` refuses about its two namespaces before it looks
/// for either. An empty or dotless namespace, a rename onto itself and a
/// rename onto `system.x` all used to be accepted.
pub(crate) fn rename(src: &str, to: &str) -> Option<CommandError> {
    let whole = |ns: &str| {
        ns.split_once('.')
            .is_some_and(|(db, coll)| !db.is_empty() && !coll.is_empty())
    };
    for (which, ns) in [("source", src), ("target", to)] {
        if !whole(ns) {
            let shown = ns.strip_suffix('.').unwrap_or(ns);
            return Some(invalid_namespace(format!(
                "Invalid {which} namespace: {shown}"
            )));
        }
    }
    let illegal = |m: String| Some(CommandError::new(20, "IllegalOperation", m));
    if src == to {
        return illegal("Can't rename a collection to itself".to_string());
    }
    if src == "local.oplog.rs" {
        return illegal("can't rename live oplog while replicating".to_string());
    }
    for (which, ns) in [("source", src), ("target", to)] {
        let (db, coll) = ns.split_once('.')?;
        if let Some(e) = crate::admin::invalid_write_namespace(db, coll) {
            return illegal(format!("error with {which} namespace: {}", e.errmsg));
        }
    }
    None
}

/// The whole namespace is held to 255 bytes.
pub(crate) fn namespace_too_long(db: &str, coll: &str) -> Option<CommandError> {
    let ns = format!("{db}.{coll}");
    (ns.len() > 255).then(|| {
        invalid_namespace(format!(
            "Fully qualified namespace is too long. Namespace: {ns} Max: 255"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn refused(db: &str, cmd: Document) -> String {
        invalid_database_name(db, &cmd)
            .map(|e| e.errmsg)
            .unwrap_or_default()
    }

    #[test]
    fn database_names_are_held_to_mongods_rules() {
        let find = doc! {"find": "c"};
        assert_eq!(
            refused("a.b", find.clone()),
            "'.' is an invalid character in a db name: a.b"
        );
        assert_eq!(refused("", find.clone()), "Invalid namespace specified: .c");
        assert_eq!(
            refused("a b", find.clone()),
            "Invalid namespace specified: a b.c"
        );
        assert_eq!(
            refused("a b", doc! {"ping": 1}),
            "Invalid database name: 'a b'"
        );
        assert_eq!(
            refused(&"x".repeat(64), find.clone()),
            "db name must be at most 63 characters, found: 64"
        );
        // A `$` is refused only where the namespace would be created.
        assert_eq!(refused("$x", find), "");
        assert_eq!(
            refused("$x", doc! {"create": "c"}),
            "Invalid namespace: $x.c"
        );
        assert_eq!(refused("ok_name-1", doc! {"create": "c"}), "");
    }

    #[test]
    fn create_options_are_parsed_before_anything_is_created() {
        let code = |d: Document| create(&d).err().map(|e| e.code);
        assert_eq!(code(doc! {"create": "c", "validationLevel": 1}), Some(14));
        assert_eq!(
            code(doc! {"create": "c", "changeStreamPreAndPostImages": {}}),
            Some(40414)
        );
        assert_eq!(
            code(doc! {"create": "c", "changeStreamPreAndPostImages": {"enabled": 1}}),
            Some(14)
        );
        assert_eq!(code(doc! {"create": "c", "autoIndexId": true}), Some(40415));
        assert_eq!(
            code(doc! {"create": "c", "clusteredIndex": true}),
            Some(5_979_703)
        );
        assert_eq!(
            code(doc! {"create": "c", "storageEngine": {"nope": {}}}),
            Some(72)
        );
        assert_eq!(
            code(doc! {"create": "c", "changeStreamPreAndPostImages": {"enabled": true}}),
            None
        );
    }
}
