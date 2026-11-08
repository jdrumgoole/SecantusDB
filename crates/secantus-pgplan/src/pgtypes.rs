//! The builtin type catalog: names, oids, array oids.
//!
//! One table serves three consumers -- `to_regtype()`, the `regtype` cast, and
//! the `pg_type` virtual table -- so they cannot disagree about what a type is
//! called or numbered. Oids are PostgreSQL's own, measured from `pg_type` on
//! PG 14, not invented.

/// (typname, oid, typarray). Every base, pseudo, range and multirange type
/// in PostgreSQL 16.15's `pg_catalog` (measured 2026-09-23, `typnamespace =
/// 11`, the `_`-prefixed array rows excluded -- they derive from `typarray`).
/// A pseudo type carries `typarray` 0: it has no array type, and `void[]`
/// resolves to nothing. `typdelim` is `,` for every type here except `box`
/// -- see [`typdelim`].
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
    ("tid", 27, 1010),
    ("xid", 28, 1011),
    ("cid", 29, 1012),
    ("oidvector", 30, 1013),
    ("pg_ddl_command", 32, 0),
    ("json", 114, 199),
    ("xml", 142, 143),
    ("pg_node_tree", 194, 0),
    ("table_am_handler", 269, 0),
    ("index_am_handler", 325, 0),
    ("point", 600, 1017),
    ("lseg", 601, 1018),
    ("path", 602, 1019),
    ("box", 603, 1020),
    ("polygon", 604, 1027),
    ("line", 628, 629),
    ("cidr", 650, 651),
    ("float4", 700, 1021),
    ("float8", 701, 1022),
    ("unknown", 705, 0),
    ("circle", 718, 719),
    ("macaddr8", 774, 775),
    ("money", 790, 791),
    ("macaddr", 829, 1040),
    ("inet", 869, 1041),
    ("aclitem", 1033, 1034),
    ("bpchar", 1042, 1014),
    ("varchar", 1043, 1015),
    ("date", 1082, 1182),
    ("time", 1083, 1183),
    ("timestamp", 1114, 1115),
    ("timestamptz", 1184, 1185),
    ("interval", 1186, 1187),
    ("timetz", 1266, 1270),
    ("bit", 1560, 1561),
    ("varbit", 1562, 1563),
    ("numeric", 1700, 1231),
    ("refcursor", 1790, 2201),
    ("regprocedure", 2202, 2207),
    ("regoper", 2203, 2208),
    ("regoperator", 2204, 2209),
    ("regclass", 2205, 2210),
    ("regtype", 2206, 2211),
    ("record", 2249, 2287),
    ("cstring", 2275, 1263),
    ("any", 2276, 0),
    ("anyarray", 2277, 0),
    ("void", 2278, 0),
    ("trigger", 2279, 0),
    ("language_handler", 2280, 0),
    ("internal", 2281, 0),
    ("anyelement", 2283, 0),
    ("anynonarray", 2776, 0),
    ("uuid", 2950, 2951),
    ("txid_snapshot", 2970, 2949),
    ("fdw_handler", 3115, 0),
    ("pg_lsn", 3220, 3221),
    ("tsm_handler", 3310, 0),
    ("pg_ndistinct", 3361, 0),
    ("pg_dependencies", 3402, 0),
    ("anyenum", 3500, 0),
    ("tsvector", 3614, 3643),
    ("tsquery", 3615, 3645),
    ("gtsvector", 3642, 3644),
    ("regconfig", 3734, 3735),
    ("regdictionary", 3769, 3770),
    ("jsonb", 3802, 3807),
    ("anyrange", 3831, 0),
    ("event_trigger", 3838, 0),
    ("int4range", 3904, 3905),
    ("numrange", 3906, 3907),
    ("tsrange", 3908, 3909),
    ("tstzrange", 3910, 3911),
    ("daterange", 3912, 3913),
    ("int8range", 3926, 3927),
    ("jsonpath", 4072, 4073),
    ("regnamespace", 4089, 4090),
    ("regrole", 4096, 4097),
    ("regcollation", 4191, 4192),
    ("int4multirange", 4451, 6150),
    ("nummultirange", 4532, 6151),
    ("tsmultirange", 4533, 6152),
    ("tstzmultirange", 4534, 6153),
    ("datemultirange", 4535, 6155),
    ("int8multirange", 4536, 6157),
    ("anymultirange", 4537, 0),
    ("anycompatiblemultirange", 4538, 0),
    ("pg_brin_bloom_summary", 4600, 0),
    ("pg_brin_minmax_multi_summary", 4601, 0),
    ("pg_mcv_list", 5017, 0),
    ("pg_snapshot", 5038, 5039),
    ("xid8", 5069, 271),
    ("anycompatible", 5077, 0),
    ("anycompatiblearray", 5078, 0),
    ("anycompatiblenonarray", 5079, 0),
    ("anycompatiblerange", 5080, 0),
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
            .map(|(_, _, arr)| *arr)
            .filter(|arr| *arr != 0);
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
