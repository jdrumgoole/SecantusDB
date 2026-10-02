//! The catalog columns clients read that the virtual catalogs do not compute
//! one by one -- psql's `\d` alone reads `pg_class.reltoastrelid`,
//! `pg_index.indisvalid`, `pg_attribute.attcollation`, `pg_type.typcollation`
//! and `pg_collation`. Each gets the value PostgreSQL 15 gives such a row,
//! filled in after the catalog's own columns, so a row the builder already
//! set is left alone.

use bson::{Bson, Document};
use secantus_pgcatalog::{Column, TableDef};

use crate::PgHandler;

/// The `default` collation, and the one a `name` column carries (`C`).
const DEFAULT_COLLATION: i64 = 100;
const C_COLLATION: i64 = 950;

/// The extra `(column, type)` pairs each catalog carries.
pub(crate) fn extra_columns(name: &str) -> &'static [(&'static str, &'static str)] {
    match name {
        "pg_class" => &[
            ("reltype", "oid"),
            ("reloftype", "oid"),
            ("relam", "oid"),
            ("relfilenode", "oid"),
            ("reltablespace", "oid"),
            ("relpages", "int4"),
            ("relallvisible", "int4"),
            ("reltoastrelid", "oid"),
            ("relisshared", "bool"),
            ("relchecks", "int2"),
            ("relhasrules", "bool"),
            ("relhastriggers", "bool"),
            ("relhassubclass", "bool"),
            ("relreplident", secantus_pgplan::QUOTED_CHAR),
            ("relispopulated", "bool"),
            ("relrewrite", "oid"),
            ("reloptions", "text[]"),
            ("relacl", "aclitem[]"),
        ],
        "pg_index" => &[
            ("indnkeyatts", "int2"),
            ("indisvalid", "bool"),
            ("indisready", "bool"),
            ("indislive", "bool"),
            ("indcheckxmin", "bool"),
            ("indimmediate", "bool"),
            ("indisreplident", "bool"),
            ("indnullsnotdistinct", "bool"),
            ("indexprs", "text"),
            ("indpred", "text"),
            ("indclass", "oidvector"),
            ("indoption", "int2vector"),
            ("indcollation", "oidvector"),
        ],
        "pg_attribute" => &[
            ("attidentity", secantus_pgplan::QUOTED_CHAR),
            ("attcollation", "oid"),
            ("attlen", "int2"),
            ("attndims", "int4"),
            ("attislocal", "bool"),
            ("attinhcount", "int4"),
            ("attstattarget", "int4"),
            ("atthasmissing", "bool"),
            ("attstorage", secantus_pgplan::QUOTED_CHAR),
            ("attcompression", secantus_pgplan::QUOTED_CHAR),
            ("attacl", "aclitem[]"),
            ("attoptions", "text[]"),
            ("attfdwoptions", "text[]"),
        ],
        "pg_type" => &[
            ("typcollation", "oid"),
            ("typelem", "oid"),
            ("typlen", "int2"),
            ("typbyval", "bool"),
            ("typalign", secantus_pgplan::QUOTED_CHAR),
            ("typstorage", secantus_pgplan::QUOTED_CHAR),
            ("typcategory", secantus_pgplan::QUOTED_CHAR),
            ("typispreferred", "bool"),
            ("typisdefined", "bool"),
            ("typinput", "regproc"),
            ("typoutput", "regproc"),
            ("typreceive", "regproc"),
            ("typsend", "regproc"),
            ("typmodin", "regproc"),
            ("typmodout", "regproc"),
            ("typanalyze", "regproc"),
            ("typsubscript", "regproc"),
            ("typndims", "int4"),
            ("typacl", "text[]"),
        ],
        "pg_proc" => &[
            ("proparallel", secantus_pgplan::QUOTED_CHAR),
            ("proacl", "text[]"),
            ("procost", "float4"),
            ("prorows", "float4"),
            ("proleakproof", "bool"),
            ("prosupport", "oid"),
            ("proconfig", "text[]"),
            ("provariadic", "oid"),
            ("prosqlbody", "text"),
            ("proargmodes", "\"char\"[]"),
            ("proallargtypes", "oid[]"),
        ],
        "pg_extension" => &[
            ("extowner", "oid"),
            ("extnamespace", "oid"),
            ("extconfig", "text[]"),
            ("extcondition", "text[]"),
        ],
        _ => &[],
    }
}

/// `pg_collation`: PostgreSQL's built-in collations.
pub(crate) fn pg_collation_def() -> TableDef {
    TableDef::new(
        "pg_collation",
        vec![
            Column::new("oid", "oid", false),
            Column::new("collname", "name", false),
            Column::new("collnamespace", "oid", false),
            Column::new("collowner", "oid", false),
            Column::new("collprovider", secantus_pgplan::QUOTED_CHAR, false),
            Column::new("collisdeterministic", "bool", false),
            Column::new("collencoding", "int4", false),
            Column::new("collcollate", "text", false),
            Column::new("collctype", "text", false),
            Column::new("colliculocale", "text", false),
        ],
    )
}

/// `pg_am`: the access methods.
pub(crate) fn pg_am_def() -> TableDef {
    TableDef::new(
        "pg_am",
        vec![
            Column::new("oid", "oid", false),
            Column::new("amname", "name", false),
            Column::new("amhandler", "text", false),
            Column::new("amtype", secantus_pgplan::QUOTED_CHAR, false),
        ],
    )
}

/// `pg_policy`: row-level security policies, as psql's `\d` reads them.
pub(crate) fn pg_policy_def() -> TableDef {
    TableDef::new(
        "pg_policy",
        vec![
            Column::new("oid", "oid", false),
            Column::new("polname", "name", false),
            Column::new("polrelid", "oid", false),
            Column::new("polcmd", secantus_pgplan::QUOTED_CHAR, false),
            Column::new("polpermissive", "bool", false),
            Column::new("polroles", "oid[]", false),
            Column::new("polqual", "text", false),
            Column::new("polwithcheck", "text", false),
        ],
    )
}

/// `pg_depend`: kept empty -- no client reads a dependency this server
/// would record differently from "none" (psql asks it about internal
/// triggers only).
pub(crate) fn pg_depend_def() -> TableDef {
    TableDef::new(
        "pg_depend",
        vec![
            Column::new("classid", "oid", false),
            Column::new("objid", "oid", false),
            Column::new("objsubid", "int4", false),
            Column::new("refclassid", "oid", false),
            Column::new("refobjid", "oid", false),
            Column::new("refobjsubid", "int4", false),
            Column::new("deptype", secantus_pgplan::QUOTED_CHAR, false),
        ],
    )
}

/// `pg_publication_namespace` (PostgreSQL 15's `FOR TABLES IN SCHEMA`):
/// empty, as no publication here names a schema.
pub(crate) fn pg_publication_namespace_def() -> TableDef {
    TableDef::new(
        "pg_publication_namespace",
        vec![
            Column::new("oid", "oid", false),
            Column::new("pnpubid", "oid", false),
            Column::new("pnnspid", "oid", false),
        ],
    )
}

/// `pg_sequence`: each sequence's parameters, keyed by its relation oid.
pub(crate) fn pg_sequence_def() -> TableDef {
    TableDef::new(
        "pg_sequence",
        vec![
            Column::new("seqrelid", "oid", false),
            Column::new("seqtypid", "oid", false),
            Column::new("seqstart", "int8", false),
            Column::new("seqincrement", "int8", false),
            Column::new("seqmax", "int8", false),
            Column::new("seqmin", "int8", false),
            Column::new("seqcache", "int8", false),
            Column::new("seqcycle", "bool", false),
        ],
    )
}

/// The description `COMMENT ON EXTENSION` gives each extension this server
/// installs, as PostgreSQL's control files do.
pub(crate) fn extension_description(name: &str) -> Option<&'static str> {
    Some(match name {
        "plpgsql" => "PL/pgSQL procedural language",
        "hstore" => "data type for storing sets of (key, value) pairs",
        "pg_trgm" => "text similarity measurement and index searching based on trigrams",
        "pgcrypto" => "cryptographic functions",
        "btree_gist" => "support for indexing common datatypes in GiST",
        "btree_gin" => "support for indexing common datatypes in GIN",
        "uuid-ossp" => "generate universally unique identifiers (UUIDs)",
        "citext" => "data type for case-insensitive character strings",
        "postgis" => "PostGIS geometry and geography spatial types and functions",
        "cube" => "data type for multidimensional cubes",
        "earthdistance" => "calculate great-circle distances on the surface of the Earth",
        "tablefunc" => "functions that manipulate whole tables, including crosstab",
        "unaccent" => "text search dictionary that removes accents",
        "fuzzystrmatch" => "determine similarities and distance between strings",
        _ => return None,
    })
}

/// `pg_description`: the comments `obj_description` / `col_description`
/// read, as a relation (psql joins it).
pub(crate) fn pg_description_def() -> TableDef {
    TableDef::new(
        "pg_description",
        vec![
            Column::new("objoid", "oid", false),
            Column::new("classoid", "oid", false),
            Column::new("objsubid", "int4", false),
            Column::new("description", "text", false),
        ],
    )
}

/// The default btree operator class of each type: `(opclass oid, name,
/// input type oid)`, from PostgreSQL 15's `pg_opclass`.
const DEFAULT_OPCLASSES: &[(i64, &str, i64)] = &[
    (424, "bool_ops", 16),
    (426, "bpchar_ops", 1042),
    (428, "bytea_ops", 17),
    (429, "char_ops", 18),
    (1978, "int4_ops", 23),
    (1979, "int2_ops", 21),
    (1980, "int8_ops", 20),
    (1981, "oid_ops", 26),
    (1970, "float4_ops", 700),
    (3123, "float8_ops", 701),
    (3125, "numeric_ops", 1700),
    (3126, "text_ops", 25),
    (3122, "date_ops", 1082),
    (3128, "timestamp_ops", 1114),
    (3127, "timestamptz_ops", 1184),
    (3129, "time_ops", 1083),
    (3130, "interval_ops", 1186),
    (2968, "uuid_ops", 2950),
    (3124, "jsonb_ops", 3802),
    (3121, "name_ops", 19),
];

/// `pg_opclass`: the default btree operator classes.
pub(crate) fn pg_opclass_def() -> TableDef {
    TableDef::new(
        "pg_opclass",
        vec![
            Column::new("oid", "oid", false),
            Column::new("opcmethod", "oid", false),
            Column::new("opcname", "name", false),
            Column::new("opcnamespace", "oid", false),
            Column::new("opcowner", "oid", false),
            Column::new("opcfamily", "oid", false),
            Column::new("opcintype", "oid", false),
            Column::new("opcdefault", "bool", false),
            Column::new("opckeytype", "oid", false),
        ],
    )
}

/// A type's default btree opclass oid (a `varchar` key uses `text_ops`).
fn default_opclass(type_oid: i64) -> i64 {
    let t = if type_oid == 1043 { 25 } else { type_oid };
    DEFAULT_OPCLASSES
        .iter()
        .find(|(_, _, ty)| *ty == t)
        .map_or(0, |(o, _, _)| *o)
}

/// A type's collation: `default` for the collatable string types, `C` for
/// `name`, none (0) for the rest.
fn type_collation(type_oid: i64) -> i64 {
    match type_oid {
        25 | 1042 | 1043 => DEFAULT_COLLATION,
        19 => C_COLLATION,
        _ => 0,
    }
}

impl PgHandler {
    pub(crate) fn pg_collation_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        [
            (
                DEFAULT_COLLATION,
                "default",
                "d",
                Bson::Null,
                Bson::Null,
                -1,
            ),
            (C_COLLATION, "C", "c", "C".into(), "C".into(), -1),
            (951, "POSIX", "c", "POSIX".into(), "POSIX".into(), -1),
            (962, "ucs_basic", "c", "C".into(), "C".into(), 6),
        ]
        .into_iter()
        .map(|(oid, name, provider, collate, ctype, encoding)| {
            let mut d = Document::new();
            d.insert(f("oid"), Bson::Int64(oid));
            d.insert(f("collname"), name);
            d.insert(f("collnamespace"), Bson::Int64(11));
            d.insert(f("collowner"), Bson::Int64(10));
            d.insert(f("collprovider"), provider);
            d.insert(f("collisdeterministic"), true);
            d.insert(f("collencoding"), Bson::Int32(encoding));
            d.insert(f("collcollate"), collate);
            d.insert(f("collctype"), ctype);
            d.insert(f("colliculocale"), Bson::Null);
            d
        })
        .collect()
    }

    pub(crate) fn pg_policy_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        let render = |table: &str, v: Option<&str>| -> Bson {
            match v {
                None => Bson::Null,
                Some(e) => Bson::String(
                    self.lookup(table)
                        .and_then(|t| secantus_pgplan::ruleutils::expr_def(e, &t))
                        .or_else(|| secantus_pgplan::generation_expression(e))
                        .unwrap_or_else(|| e.to_string()),
                ),
            }
        };
        let roles = self.roles().unwrap_or_default();
        self.policy_docs()
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let table = p.get_str("table").unwrap_or_default();
                let mut d = Document::new();
                d.insert(f("oid"), Bson::Int64(0x3000_0000 + i as i64));
                d.insert(f("polname"), p.get_str("name").unwrap_or_default());
                d.insert(
                    f("polrelid"),
                    Bson::Int64(self.relation_oid(table).unwrap_or(0)),
                );
                d.insert(
                    f("polcmd"),
                    match p.get_str("command").unwrap_or("ALL") {
                        "SELECT" => "r",
                        "INSERT" => "a",
                        "UPDATE" => "w",
                        "DELETE" => "d",
                        _ => "*",
                    },
                );
                d.insert(f("polpermissive"), p.get_bool("permissive").unwrap_or(true));
                // PUBLIC is role 0.
                let polroles: Vec<Bson> = p
                    .get_array("roles")
                    .map(|a| {
                        a.iter()
                            .map(|r| {
                                let r = r.as_str().unwrap_or_default();
                                Bson::Int64(if r.eq_ignore_ascii_case("public") {
                                    0
                                } else {
                                    roles.iter().find(|x| x.name == r).map_or(0, |x| x.oid)
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_else(|_| vec![Bson::Int64(0)]);
                d.insert(f("polroles"), polroles);
                d.insert(f("polqual"), render(table, p.get_str("using").ok()));
                d.insert(f("polwithcheck"), render(table, p.get_str("check").ok()));
                d
            })
            .collect()
    }

    pub(crate) fn pg_sequence_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        self.all_sequence_docs()
            .unwrap_or_default()
            .iter()
            .map(|s| {
                let int = |k: &str, dflt: i64| {
                    s.get_i64(k)
                        .or_else(|_| s.get_i32(k).map(i64::from))
                        .unwrap_or(dflt)
                };
                let name = s.get_str("_id").unwrap_or_default();
                let max = int("max_value", i64::MAX);
                // A serial's sequence is typed by its column's width.
                let typ = if max == i64::from(i32::MAX) {
                    23
                } else if max == i64::from(i16::MAX) {
                    21
                } else {
                    20
                };
                let mut d = Document::new();
                d.insert(f("seqrelid"), Bson::Int64(Self::sequence_oid(name)));
                d.insert(f("seqtypid"), Bson::Int64(typ));
                d.insert(f("seqstart"), Bson::Int64(int("start", 1)));
                d.insert(f("seqincrement"), Bson::Int64(int("increment", 1)));
                d.insert(f("seqmax"), Bson::Int64(max));
                d.insert(f("seqmin"), Bson::Int64(int("min_value", 1)));
                d.insert(f("seqcache"), Bson::Int64(int("cache", 1)));
                d.insert(f("seqcycle"), s.get_bool("cycle").unwrap_or(false));
                d
            })
            .collect()
    }

    /// Every function's `(oid, pg_get_function_arguments,
    /// pg_get_function_result)` text.
    pub(crate) fn function_sigs(&self) -> Vec<(i64, String, String, String)> {
        self.type_catalog_docs(Self::FUNCTION_COLLECTION)
            .unwrap_or_default()
            .iter()
            .map(|d| {
                let strings = |key: &str| -> Vec<String> {
                    d.get_array(key)
                        .map(|a| {
                            a.iter()
                                .map(|v| v.as_str().unwrap_or_default().to_string())
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let names = strings("params");
                let types = strings("param_types");
                let defaults: Vec<Option<String>> = d
                    .get_array("param_defaults")
                    .map(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                let variadic = d.get_bool("variadic").unwrap_or(false);
                let n = types.len();
                // `(with DEFAULTs, without)`: `pg_get_function_arguments`
                // prints a parameter's default as ruleutils does -- a string
                // literal typed, `'x'::text` -- and the identity form omits it.
                let pairs: Vec<(String, String)> = types
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let ty = self.display_type_name(t);
                        let name = names.get(i).map(String::as_str).unwrap_or("");
                        let prefix = if variadic && i + 1 == n {
                            "VARIADIC "
                        } else {
                            ""
                        };
                        let bare = if name.is_empty() {
                            format!("{prefix}{ty}")
                        } else {
                            format!("{prefix}{name} {ty}")
                        };
                        let default = match defaults.get(i).cloned().flatten() {
                            None => String::new(),
                            Some(sql) if sql.starts_with('\'') && sql.ends_with('\'') => {
                                format!(" DEFAULT {sql}::{ty}")
                            }
                            Some(sql) => format!(
                                " DEFAULT {}",
                                secantus_pgplan::generation_expression(&sql).unwrap_or(sql)
                            ),
                        };
                        (format!("{bare}{default}"), bare)
                    })
                    .collect();
                let args: Vec<String> = pairs.iter().map(|(a, _)| a.clone()).collect();
                let identity: Vec<String> = pairs.iter().map(|(_, b)| b.clone()).collect();
                let ret = d.get_str("return_tag").unwrap_or("void");
                let result = if d.get_bool("returns_trigger").unwrap_or(false) {
                    "trigger".to_string()
                } else if d.get_bool("is_table").unwrap_or(false) {
                    let cols: Vec<String> = d
                        .get_array("table_columns")
                        .map(|a| {
                            a.iter()
                                .filter_map(|c| {
                                    let c = c.as_document()?;
                                    Some(format!(
                                        "{} {}",
                                        c.get_str("name").ok()?,
                                        self.display_type_name(c.get_str("type_tag").ok()?)
                                    ))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    format!("TABLE({})", cols.join(", "))
                } else if d.get_bool("returns_set").unwrap_or(false) {
                    format!("SETOF {}", self.display_type_name(ret))
                } else {
                    self.display_type_name(ret)
                };
                let oid = Self::index_oid(&format!("fn:{}", d.get_str("_id").unwrap_or_default()));
                (oid, args.join(", "), identity.join(", "), result)
            })
            .collect()
    }

    pub(crate) fn pg_description_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        let extensions: Vec<i64> = (0..self.extensions().unwrap_or_default().len())
            .map(|i| 13_826 + i as i64)
            .collect();
        let relations: Vec<i64> = self.relations().into_iter().map(|(_, o, _)| o).collect();
        let functions: Vec<i64> = self
            .function_sigs()
            .into_iter()
            .map(|(o, _, _, _)| o)
            .collect();
        let constraints: Vec<i64> = self
            .constraint_comments()
            .into_iter()
            .map(|(o, _)| o)
            .collect();
        self.object_comments()
            .into_iter()
            .map(|(oid, subid, text)| {
                // The catalog the object lives in, as `pg_description` keys it.
                let classoid = if constraints.contains(&oid) {
                    2606
                } else if extensions.contains(&oid) {
                    3079
                } else if relations.contains(&oid) {
                    1259
                } else if functions.contains(&oid) {
                    1255
                } else {
                    1247
                };
                let mut d = Document::new();
                d.insert(f("objoid"), Bson::Int64(oid));
                d.insert(f("classoid"), Bson::Int64(classoid));
                d.insert(f("objsubid"), Bson::Int32(subid));
                d.insert(f("description"), text);
                d
            })
            .collect()
    }

    /// Every function's `(oid, pg_get_functiondef text)`, laid out as
    /// ruleutils lays it out.
    pub(crate) fn function_defs(&self) -> Vec<(i64, String)> {
        let sigs = self.function_sigs();
        self.type_catalog_docs(Self::FUNCTION_COLLECTION)
            .unwrap_or_default()
            .iter()
            .filter_map(|d| {
                let oid = Self::index_oid(&format!("fn:{}", d.get_str("_id").unwrap_or_default()));
                let (_, args, _, result) = sigs.iter().find(|(o, ..)| *o == oid)?;
                let mut text = format!(
                    "CREATE OR REPLACE FUNCTION public.{}({args})\n RETURNS {result}\n LANGUAGE {}\n",
                    d.get_str("name").unwrap_or_default(),
                    d.get_str("language").unwrap_or("sql"),
                );
                match d.get_str("volatility").unwrap_or("volatile") {
                    "immutable" => text.push_str(" IMMUTABLE\n"),
                    "stable" => text.push_str(" STABLE\n"),
                    _ => {}
                }
                if d.get_bool("strict").unwrap_or(false) {
                    text.push_str(" STRICT\n");
                }
                text.push_str(&format!(
                    "AS $function${}$function$\n",
                    d.get_str("body").unwrap_or_default()
                ));
                Some((oid, text))
            })
            .collect()
    }

    pub(crate) fn pg_opclass_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        DEFAULT_OPCLASSES
            .iter()
            .map(|(oid, name, ty)| {
                let mut d = Document::new();
                d.insert(f("oid"), Bson::Int64(*oid));
                d.insert(f("opcmethod"), Bson::Int64(403));
                d.insert(f("opcname"), *name);
                d.insert(f("opcnamespace"), Bson::Int64(11));
                d.insert(f("opcowner"), Bson::Int64(10));
                d.insert(f("opcfamily"), Bson::Int64(0));
                d.insert(f("opcintype"), Bson::Int64(*ty));
                d.insert(f("opcdefault"), true);
                d.insert(f("opckeytype"), Bson::Int64(0));
                d
            })
            .collect()
    }

    pub(crate) fn pg_am_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        [
            (2, "heap", "heap_tableam_handler", "t"),
            (403, "btree", "bthandler", "i"),
            (405, "hash", "hashhandler", "i"),
            (783, "gist", "gisthandler", "i"),
            (2742, "gin", "ginhandler", "i"),
            (4000, "spgist", "spghandler", "i"),
            (3580, "brin", "brinhandler", "i"),
        ]
        .into_iter()
        .map(|(oid, name, handler, kind)| {
            let mut d = Document::new();
            d.insert(f("oid"), Bson::Int64(oid));
            d.insert(f("amname"), name);
            d.insert(f("amhandler"), handler);
            d.insert(f("amtype"), kind);
            d
        })
        .collect()
    }

    /// Fill the extra columns of `name`'s rows with PostgreSQL's values.
    pub(crate) fn fill_catalog_columns(&self, name: &str, def: &TableDef, rows: &mut [Document]) {
        let extra = extra_columns(name);
        if extra.is_empty() {
            return;
        }
        let field = |c: &str| def.field_of(c);
        let get = |d: &Document, c: &str| field(c).and_then(|f| d.get(&f).cloned());
        let int = |v: Option<Bson>| match v {
            Some(Bson::Int32(i)) => i64::from(i),
            Some(Bson::Int64(i)) => i,
            _ => 0,
        };
        let text = |v: Option<Bson>| match v {
            Some(Bson::String(s)) => s,
            _ => String::new(),
        };
        // Per table: (oid, def), for the columns that depend on it.
        let tables: Vec<(i64, TableDef)> = match name {
            "pg_class" | "pg_attribute" => self
                .all_table_defs()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|t| Some((self.relation_oid(&t.name)?, t)))
                .collect(),
            _ => Vec::new(),
        };
        let table_by_oid = |oid: i64| tables.iter().find(|(o, _)| *o == oid).map(|(_, t)| t);
        // Per index: each key's type oid and whether it sorts DESC.
        let index_keys: Vec<(i64, Vec<(i64, bool)>)> = if name == "pg_index" {
            self.index_relations()
                .into_iter()
                .map(|ix| {
                    let stored = self
                        .storage
                        .list_indexes(self.db(), &ix.table.name)
                        .unwrap_or_default()
                        .into_iter()
                        .find(|d| d.get_str("name") == Ok(ix.name.as_str()));
                    let directions: Vec<bool> = stored
                        .and_then(|d| d.get_document("key").ok().cloned())
                        .map(|k| {
                            k.values()
                                .map(|v| matches!(v, Bson::Int32(-1)) || v.as_f64() == Some(-1.0))
                                .collect()
                        })
                        .unwrap_or_default();
                    let keys = ix
                        .keys
                        .iter()
                        .enumerate()
                        .map(|(i, k)| {
                            let ty = usize::try_from(*k)
                                .ok()
                                .filter(|k| *k > 0)
                                .and_then(|k| ix.table.columns.get(k - 1))
                                .and_then(|c| secantus_pgplan::pgtypes::oid_of_name(&c.pg_type))
                                .unwrap_or(0);
                            (ty, directions.get(i).copied().unwrap_or(false))
                        })
                        .collect();
                    (ix.oid, keys)
                })
                .collect()
        } else {
            Vec::new()
        };
        let parents: Vec<String> = if name == "pg_class" {
            tables
                .iter()
                .flat_map(|(_, t)| {
                    crate::partition::parent_of(t)
                        .map(str::to_string)
                        .into_iter()
                        .chain(Self::inherited_parents(t))
                })
                .collect()
        } else {
            Vec::new()
        };
        let matviews: Vec<(String, bool)> = if name == "pg_class" {
            self.matviews()
                .unwrap_or_default()
                .into_iter()
                .map(|(n, _, populated)| (n, populated))
                .collect()
        } else {
            Vec::new()
        };
        for row in rows.iter_mut() {
            for (column, _) in extra {
                let Some(f) = field(column) else {
                    continue;
                };
                if row.contains_key(&f) {
                    continue;
                }
                let value: Bson = match (name, *column) {
                    ("pg_class", c) => {
                        let kind = text(get(row, "relkind"));
                        let oid = int(get(row, "oid"));
                        let relname = text(get(row, "relname"));
                        let table = table_by_oid(oid);
                        match c {
                            "relam" => Bson::Int64(match kind.as_str() {
                                "r" | "m" | "t" => 2,
                                "i" | "I" => 403,
                                _ => 0,
                            }),
                            "relfilenode" => Bson::Int64(match kind.as_str() {
                                "v" | "p" | "I" | "c" | "f" => 0,
                                _ => oid,
                            }),
                            "relchecks" => {
                                Bson::Int32(table.map_or(0, |t| t.check_constraints.len() as i32))
                            }
                            // A view is its `_RETURN` rule.
                            "relhasrules" => Bson::Boolean(kind == "v" || self.has_rules(&relname)),
                            // A FOREIGN KEY is enforced by RI triggers on
                            // BOTH tables, so either side has triggers --
                            // which is what makes psql print its FK footers.
                            "relhastriggers" => Bson::Boolean(table.is_some_and(|t| {
                                !t.foreign_keys.is_empty()
                                    || tables.iter().any(|(_, o)| {
                                        o.foreign_keys.iter().any(|k| k.ref_table == t.name)
                                    })
                                    || self.has_triggers(&relname).unwrap_or(false)
                            })),
                            "relhassubclass" => Bson::Boolean(parents.contains(&relname)),
                            "relreplident" => {
                                Bson::String(if matches!(kind.as_str(), "r" | "m" | "p") {
                                    "d".into()
                                } else {
                                    "n".into()
                                })
                            }
                            "relispopulated" => Bson::Boolean(
                                matviews
                                    .iter()
                                    .find(|(n, _)| *n == relname)
                                    .is_none_or(|(_, p)| *p),
                            ),
                            "relisshared" => Bson::Boolean(false),
                            "relpages" | "relallvisible" => Bson::Int32(0),
                            "relacl" => self.relation_acl(&relname, &kind),
                            "reloptions" => Bson::Null,
                            _ => Bson::Int64(0),
                        }
                    }
                    ("pg_index", c) => match c {
                        "indclass" | "indoption" | "indcollation" => {
                            let oid = int(get(row, "indexrelid"));
                            let keys = index_keys
                                .iter()
                                .find(|(o, _)| *o == oid)
                                .map(|(_, k)| k.clone())
                                .unwrap_or_default();
                            Bson::Array(
                                keys.iter()
                                    .map(|(ty, desc)| match c {
                                        "indclass" => Bson::Int64(default_opclass(*ty)),
                                        // DESC implies NULLS FIRST: both bits.
                                        "indoption" => Bson::Int32(if *desc { 3 } else { 0 }),
                                        _ => Bson::Int64(type_collation(*ty)),
                                    })
                                    .collect(),
                            )
                        }
                        // The KEY columns, which leave out the INCLUDE ones.
                        "indnkeyatts" => {
                            let oid = int(get(row, "indexrelid"));
                            match index_keys.iter().find(|(o, _)| *o == oid) {
                                Some((_, k)) => Bson::Int32(k.len() as i32),
                                None => get(row, "indnatts").unwrap_or(Bson::Int32(0)),
                            }
                        }
                        "indcheckxmin" | "indisreplident" | "indnullsnotdistinct" => {
                            Bson::Boolean(false)
                        }
                        "indexprs" | "indpred" => Bson::Null,
                        _ => Bson::Boolean(true),
                    },
                    ("pg_attribute", c) => {
                        let type_oid = int(get(row, "atttypid"));
                        match c {
                            "attidentity" => {
                                let table = table_by_oid(int(get(row, "attrelid")));
                                let attname = text(get(row, "attname"));
                                let identity = table
                                    .and_then(|t| t.column(&attname))
                                    .and_then(|col| col.identity.clone());
                                Bson::String(match identity.as_deref() {
                                    Some("always") | Some("a") => "a".into(),
                                    Some(_) => "d".into(),
                                    None => String::new(),
                                })
                            }
                            // A column's own COLLATE, else its type's.
                            "attcollation" => {
                                let table = table_by_oid(int(get(row, "attrelid")));
                                let attname = text(get(row, "attname"));
                                let declared =
                                    table.and_then(|t| t.column(&attname)).and_then(|col| {
                                        col.extra.get_str("collation").ok().map(str::to_string)
                                    });
                                match declared {
                                    Some(name) => Bson::Int64(
                                        secantus_pgplan::regobj::collation_names()
                                            .into_iter()
                                            .find(|(n, _)| *n == name)
                                            .map_or_else(|| type_collation(type_oid), |(_, o)| o),
                                    ),
                                    None => Bson::Int64(type_collation(type_oid)),
                                }
                            }
                            "attlen" => Bson::Int32(match type_oid {
                                16 | 18 => 1,
                                21 => 2,
                                23 | 26 | 700 | 1082 => 4,
                                20 | 701 | 1114 | 1184 | 1083 => 8,
                                19 => 64,
                                2950 => 16,
                                1186 => 16,
                                _ => -1,
                            }),
                            // A fixed-width type is stored `p`lain, a
                            // varlena one `x` (extended), as `typstorage`.
                            "attstorage" => Bson::String(match type_oid {
                                16 | 18 | 20 | 21 | 23 | 26 | 700 | 701 | 1082 | 1083 | 1114
                                | 1184 | 1186 | 2950 | 19 => "p".into(),
                                // numeric, inet and cidr are kept inline.
                                1700 | 869 | 650 => "m".into(),
                                _ => "x".into(),
                            }),
                            "attcompression" => Bson::String(String::new()),
                            "attacl" => match table_by_oid(int(get(row, "attrelid"))) {
                                Some(t) => self.column_acl(t, &text(get(row, "attname"))),
                                None => Bson::Null,
                            },
                            "attoptions" | "attfdwoptions" => Bson::Null,
                            "attndims" | "attinhcount" => Bson::Int32(0),
                            "attstattarget" => Bson::Int32(-1),
                            "attislocal" => Bson::Boolean(true),
                            _ => Bson::Boolean(false),
                        }
                    }
                    ("pg_type", "typisdefined") => Bson::Boolean(true),
                    ("pg_type", "typndims") => Bson::Int32(0),
                    ("pg_type", "typacl") => Bson::Null,
                    ("pg_type", c) if crate::pg_type_facts::COLUMNS.contains(&c) => {
                        let oid = int(get(row, "oid"));
                        crate::pg_type_facts::builtin(oid, c)
                            .or_else(|| {
                                let kind = text(get(row, "typtype"));
                                let base = int(get(row, "typbasetype"));
                                crate::pg_type_facts::by_kind(&kind, (base > 0).then_some(base), c)
                            })
                            .unwrap_or(Bson::Null)
                    }
                    ("pg_type", "typcollation") => {
                        Bson::Int64(type_collation(int(get(row, "oid"))))
                    }
                    // An array type's element; 0 for any other type.
                    ("pg_type", "typelem") => Bson::Int64(
                        secantus_pgplan::pgtypes::type_name_of_oid(int(get(row, "oid")))
                            .and_then(|n| {
                                n.strip_suffix("[]")
                                    .and_then(secantus_pgplan::pgtypes::oid_of_name)
                            })
                            .unwrap_or(0),
                    ),
                    ("pg_proc", c) => match c {
                        "proparallel" => Bson::String("u".into()),
                        "procost" => Bson::Double(100.0),
                        "prorows" => Bson::Double(
                            if matches!(get(row, "proretset"), Some(Bson::Boolean(true))) {
                                1000.0
                            } else {
                                0.0
                            },
                        ),
                        "proleakproof" => Bson::Boolean(false),
                        "prosupport" | "provariadic" => Bson::Int64(0),
                        _ => Bson::Null,
                    },
                    ("pg_extension", c) => match c {
                        "extowner" => Bson::Int64(10),
                        // plpgsql lives in pg_catalog, an installed extension
                        // in the schema it was created in (public).
                        "extnamespace" => Bson::Int64(if text(get(row, "extname")) == "plpgsql" {
                            11
                        } else {
                            2200
                        }),
                        _ => Bson::Null,
                    },
                    _ => continue,
                };
                row.insert(f, value);
            }
        }
    }
}
