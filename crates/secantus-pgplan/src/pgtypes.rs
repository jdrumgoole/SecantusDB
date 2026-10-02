//! The builtin type catalog: names, oids, array oids.
//!
//! One table serves three consumers -- `to_regtype()`, the `regtype` cast, and
//! the `pg_type` virtual table -- so they cannot disagree about what a type is
//! called or numbered. Oids are PostgreSQL's own, measured from `pg_type` on
//! PG 14, not invented.

/// (typname, oid, typarray). `typdelim` is `,` for every type here except
/// `box` -- see [`typdelim`].
pub const BUILTIN_TYPES: &[(&str, i64, i64)] = &[
    ("bool", 16, 1000),
    ("bytea", 17, 1001),
    ("char", 18, 1002),
    ("name", 19, 1003),
    ("int8", 20, 1016),
    ("int2", 21, 1005),
    ("int2vector", 22, 1006),
    ("int4", 23, 1007),
    ("regproc", 24, 1008),
    ("text", 25, 1009),
    ("oid", 26, 1028),
    ("oidvector", 30, 1013),
    ("json", 114, 199),
    ("xml", 142, 143),
    ("float4", 700, 1021),
    ("float8", 701, 1022),
    ("aclitem", 1033, 1034),
    ("box", 603, 1020),
    ("point", 600, 1017),
    ("lseg", 601, 1018),
    ("path", 602, 1019),
    ("polygon", 604, 1027),
    ("line", 628, 629),
    ("circle", 718, 719),
    ("bpchar", 1042, 1014),
    ("varchar", 1043, 1015),
    ("date", 1082, 1182),
    ("time", 1083, 1183),
    ("timestamp", 1114, 1115),
    ("timestamptz", 1184, 1185),
    ("interval", 1186, 1187),
    ("timetz", 1266, 1270),
    ("numeric", 1700, 1231),
    ("regclass", 2205, 2210),
    ("regproc", 24, 1008),
    ("regprocedure", 2202, 2207),
    ("regnamespace", 4089, 4090),
    ("regrole", 4096, 4097),
    ("regcollation", 4191, 4192),
    ("regtype", 2206, 2211),
    ("uuid", 2950, 2951),
    ("refcursor", 1790, 2201),
    ("money", 790, 791),
    ("inet", 869, 1041),
    ("cidr", 650, 651),
    ("bit", 1560, 1561),
    ("varbit", 1562, 1563),
    ("tsvector", 3614, 3643),
    ("tsquery", 3615, 3645),
    ("regconfig", 3734, 3735),
    ("jsonpath", 4072, 4073),
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

/// The character that separates the elements of an array of this type in its
/// text form: `;` for `box` (whose own text is full of commas), `,` for every
/// other type.
pub fn typdelim(element_type: &str) -> char {
    if element_type == "box" {
        ';'
    } else {
        ','
    }
}

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

/// The internal name for a scalar OR ARRAY type oid (`int4`, `int4[]`).
pub fn type_name_of_oid(oid: i64) -> Option<String> {
    name_of_oid(oid).map(str::to_string).or_else(|| {
        BUILTIN_TYPES
            .iter()
            .find(|(_, _, a)| *a == oid)
            .map(|(t, _, _)| format!("{t}[]"))
    })
}

/// The internal name for an oid, or `None`.
pub fn name_of_oid(oid: i64) -> Option<&'static str> {
    BUILTIN_TYPES
        .iter()
        .find(|(_, o, _)| *o == oid)
        .map(|(t, _, _)| *t)
}

/// Every type name PostgreSQL 14 has in `pg_catalog` (arrays aside, which
/// are `<name>[]` of these), measured from `pg_type`. A column declared with
/// a name that is neither one of these nor a user type is `42704`.
const CATALOG_TYPE_NAMES: &[&str] = &[
    "aclitem",
    "any",
    "anyarray",
    "anycompatible",
    "anycompatiblearray",
    "anycompatiblemultirange",
    "anycompatiblenonarray",
    "anycompatiblerange",
    "anyelement",
    "anyenum",
    "anymultirange",
    "anynonarray",
    "anyrange",
    "bit",
    "bool",
    "box",
    "bpchar",
    "bytea",
    "char",
    "cid",
    "cidr",
    "circle",
    "cstring",
    "date",
    "datemultirange",
    "daterange",
    "event_trigger",
    "fdw_handler",
    "float4",
    "float8",
    "gtsvector",
    "index_am_handler",
    "inet",
    "int2",
    "int2vector",
    "int4",
    "int4multirange",
    "int4range",
    "int8",
    "int8multirange",
    "int8range",
    "internal",
    "interval",
    "json",
    "jsonb",
    "jsonpath",
    "language_handler",
    "line",
    "lseg",
    "macaddr",
    "macaddr8",
    "money",
    "name",
    "numeric",
    "nummultirange",
    "numrange",
    "oid",
    "oidvector",
    "path",
    "pg_lsn",
    "pg_snapshot",
    "point",
    "polygon",
    "record",
    "refcursor",
    "regclass",
    "regcollation",
    "regconfig",
    "regdictionary",
    "regnamespace",
    "regoper",
    "regoperator",
    "regproc",
    "regprocedure",
    "regrole",
    "regtype",
    "table_am_handler",
    "text",
    "tid",
    "time",
    "timestamp",
    "timestamptz",
    "timetz",
    "trigger",
    "tsm_handler",
    "tsmultirange",
    "tsquery",
    "tsrange",
    "tstzmultirange",
    "tstzrange",
    "tsvector",
    "txid_snapshot",
    "unknown",
    "uuid",
    "varbit",
    "varchar",
    "void",
    "xid",
    "xid8",
    "xml",
];

/// Whether `name` (an array's element name, or a scalar's) is a built-in
/// type, in any spelling PostgreSQL accepts.
pub fn is_builtin_type(name: &str) -> bool {
    let base = name.trim_end_matches("[]").trim();
    let base = base.strip_prefix("pg_catalog.").unwrap_or(base);
    let base = base.trim_matches('"');
    CATALOG_TYPE_NAMES.contains(&base)
        || matches!(base, "bit varying" | "serial" | "bigserial" | "smallserial")
        || oid_of_name(base).is_some()
}
