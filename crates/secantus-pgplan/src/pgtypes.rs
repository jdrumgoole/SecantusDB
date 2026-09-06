//! The builtin type catalog: names, oids, array oids.
//!
//! One table serves three consumers -- `to_regtype()`, the `regtype` cast, and
//! the `pg_type` virtual table -- so they cannot disagree about what a type is
//! called or numbered. Oids are PostgreSQL's own, measured from `pg_type` on
//! PG 14, not invented.

/// (typname, oid, typarray). `typdelim` is `,` for every type here.
pub const BUILTIN_TYPES: &[(&str, i64, i64)] = &[
    ("bool", 16, 1000),
    ("bytea", 17, 1001),
    ("char", 18, 1002),
    ("name", 19, 1003),
    ("int8", 20, 1016),
    ("int2", 21, 1005),
    ("int4", 23, 1007),
    ("regproc", 24, 1008),
    ("text", 25, 1009),
    ("oid", 26, 1028),
    ("json", 114, 199),
    ("float4", 700, 1021),
    ("float8", 701, 1022),
    ("bpchar", 1042, 1014),
    ("varchar", 1043, 1015),
    ("date", 1082, 1182),
    ("time", 1083, 1183),
    ("timestamp", 1114, 1115),
    ("timestamptz", 1184, 1185),
    ("interval", 1186, 1187),
    ("timetz", 1266, 1270),
    ("numeric", 1700, 1231),
    ("regtype", 2206, 2211),
    ("uuid", 2950, 2951),
    ("jsonb", 3802, 3807),
    ("int4range", 3904, 3905),
    ("numrange", 3906, 3907),
    ("tsrange", 3908, 3909),
    ("tstzrange", 3910, 3911),
    ("daterange", 3912, 3913),
    ("int8range", 3926, 3927),
    ("int4multirange", 4451, 6150),
    ("nummultirange", 4532, 6151),
    ("tsmultirange", 4533, 6152),
    ("tstzmultirange", 4534, 6153),
    ("datemultirange", 4535, 6155),
    ("int8multirange", 4536, 6157),
];

/// The oid for a type NAME, in any spelling PostgreSQL itself accepts --
/// `int4` and `integer`, `varchar` and `character varying`. `None` for a name
/// this catalog does not have, which is what `to_regtype` answers NULL for.
pub fn oid_of_name(name: &str) -> Option<i64> {
    // A QUOTED identifier resolves too -- `to_regtype('"text"')` is 25 on
    // PostgreSQL, and psycopg's `TypeInfo.fetch(conn, sql.Identifier(...))`
    // sends exactly that. Unlike the bare spelling it is CASE-SENSITIVE, so
    // the quotes strip without the lowercasing.
    let trimmed = name.trim();
    if let Some(inner) = trimmed.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        return BUILTIN_TYPES
            .iter()
            .find(|(t, _, _)| *t == inner)
            .map(|(_, oid, _)| *oid);
    }
    // An ARRAY name resolves to the element's typarray oid.
    if let Some(element) = trimmed.strip_suffix("[]") {
        let element_oid = oid_of_name(element)?;
        return BUILTIN_TYPES
            .iter()
            .find(|(_, o, _)| *o == element_oid)
            .map(|(_, _, arr)| *arr);
    }
    let n = trimmed.to_ascii_lowercase();
    let internal = match n.as_str() {
        "integer" | "int" => "int4",
        "smallint" => "int2",
        "bigint" => "int8",
        "real" => "float4",
        "double precision" => "float8",
        "boolean" => "bool",
        "character varying" => "varchar",
        "character" => "bpchar",
        "decimal" => "numeric",
        "time without time zone" => "time",
        "timestamp without time zone" => "timestamp",
        "timestamp with time zone" => "timestamptz",
        "time with time zone" => "timetz",
        other => other,
    };
    BUILTIN_TYPES
        .iter()
        .find(|(t, _, _)| *t == internal)
        .map(|(_, oid, _)| *oid)
}

/// The internal name for an oid, or `None`.
pub fn name_of_oid(oid: i64) -> Option<&'static str> {
    BUILTIN_TYPES
        .iter()
        .find(|(_, o, _)| *o == oid)
        .map(|(t, _, _)| *t)
}
