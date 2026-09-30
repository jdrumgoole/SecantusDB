//! `pg_type`'s physical and I/O columns (`typlen`, `typbyval`, `typalign`,
//! `typstorage`, `typcategory`, `typispreferred`, `typinput` ...): for a
//! built-in type as PostgreSQL 15 records it (`pg_type_facts.tsv`, dumped
//! from `pg_type`), and for a user enum / composite / range / domain as
//! PostgreSQL gives every type of that kind.

use std::collections::HashMap;
use std::sync::OnceLock;

use bson::Bson;

const FACTS: &str = include_str!("pg_type_facts.tsv");

/// The columns, in the file's order after `oid`.
pub(crate) const COLUMNS: [&str; 14] = [
    "typlen",
    "typbyval",
    "typalign",
    "typstorage",
    "typcategory",
    "typispreferred",
    "typinput",
    "typoutput",
    "typreceive",
    "typsend",
    "typmodin",
    "typmodout",
    "typanalyze",
    "typsubscript",
];

fn facts() -> &'static HashMap<i64, Vec<String>> {
    static F: OnceLock<HashMap<i64, Vec<String>>> = OnceLock::new();
    F.get_or_init(|| {
        FACTS
            .lines()
            .filter_map(|l| {
                let mut it = l.split('\t');
                let oid = it.next()?.parse().ok()?;
                Some((oid, it.map(str::to_string).collect()))
            })
            .collect()
    })
}

fn typed(column: &str, v: &str) -> Bson {
    match column {
        "typlen" => Bson::Int32(v.parse().unwrap_or(-1)),
        "typbyval" | "typispreferred" => Bson::Boolean(v == "true"),
        _ => Bson::String(v.to_string()),
    }
}

/// A built-in type's value for `column`.
pub(crate) fn builtin(oid: i64, column: &str) -> Option<Bson> {
    let i = COLUMNS.iter().position(|c| *c == column)?;
    let row = facts().get(&oid)?;
    Some(typed(column, row.get(i)?))
}

/// A user type's value for `column` by its `typtype` (`e` enum, `c`
/// composite, `r` range, `m` multirange, `d` domain over `base`, `p` shell).
pub(crate) fn by_kind(typtype: &str, base: Option<i64>, column: &str) -> Option<Bson> {
    let v = match (typtype, column) {
        ("d", c) => {
            // A domain is its base type's representation, with its own I/O
            // entry points.
            return match c {
                "typinput" => Some(Bson::String("domain_in".into())),
                "typreceive" => Some(Bson::String("domain_recv".into())),
                "typispreferred" => Some(Bson::Boolean(false)),
                _ => base.and_then(|b| builtin(b, c)),
            };
        }
        ("e", "typlen") => "4",
        ("e", "typbyval") => "true",
        ("e", "typalign") => "i",
        ("e", "typstorage") => "p",
        ("e", "typcategory") => "E",
        ("e", "typinput") => "enum_in",
        ("e", "typoutput") => "enum_out",
        ("e", "typreceive") => "enum_recv",
        ("e", "typsend") => "enum_send",
        ("c", "typlen") => "-1",
        ("c", "typbyval") => "false",
        ("c", "typalign") => "d",
        ("c", "typstorage") => "x",
        ("c", "typcategory") => "C",
        ("c", "typinput") => "record_in",
        ("c", "typoutput") => "record_out",
        ("c", "typreceive") => "record_recv",
        ("c", "typsend") => "record_send",
        ("r" | "m", "typlen") => "-1",
        ("r" | "m", "typbyval") => "false",
        ("r" | "m", "typalign") => "i",
        ("r" | "m", "typstorage") => "x",
        ("r" | "m", "typcategory") => "R",
        ("r", "typinput") => "range_in",
        ("r", "typoutput") => "range_out",
        ("r", "typreceive") => "range_recv",
        ("r", "typsend") => "range_send",
        ("r", "typanalyze") => "range_typanalyze",
        ("m", "typinput") => "multirange_in",
        ("m", "typoutput") => "multirange_out",
        ("m", "typreceive") => "multirange_recv",
        ("m", "typsend") => "multirange_send",
        ("m", "typanalyze") => "multirange_typanalyze",
        ("p", "typlen") => "4",
        ("p", "typbyval") => "true",
        ("p", "typalign") => "i",
        ("p", "typstorage") => "p",
        ("p", "typcategory") => "P",
        ("p", "typinput") => "shell_in",
        ("p", "typoutput") => "shell_out",
        (_, "typispreferred") => "false",
        (
            _,
            "typinput" | "typoutput" | "typreceive" | "typsend" | "typmodin" | "typmodout"
            | "typanalyze" | "typsubscript",
        ) => "-",
        _ => return None,
    };
    Some(typed(column, v))
}
