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
            ("relacl", "text[]"),
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
        ],
        "pg_type" => &[("typcollation", "oid"), ("typelem", "oid")],
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
            d
        })
        .collect()
    }

    pub(crate) fn pg_policy_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        let render = |v: Option<&str>| -> Bson {
            match v {
                None => Bson::Null,
                Some(e) => Bson::String(
                    secantus_pgplan::generation_expression(e).unwrap_or_else(|| e.to_string()),
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
                d.insert(f("polqual"), render(p.get_str("using").ok()));
                d.insert(f("polwithcheck"), render(p.get_str("check").ok()));
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
    pub(crate) fn function_sigs(&self) -> Vec<(i64, String, String)> {
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
                let variadic = d.get_bool("variadic").unwrap_or(false);
                let n = types.len();
                let args: Vec<String> = types
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
                        if name.is_empty() {
                            format!("{prefix}{ty}")
                        } else {
                            format!("{prefix}{name} {ty}")
                        }
                    })
                    .collect();
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
                (oid, args.join(", "), result)
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
            .map(|(o, _, _)| o)
            .collect();
        self.object_comments()
            .into_iter()
            .map(|(oid, subid, text)| {
                // The catalog the object lives in, as `pg_description` keys it.
                let classoid = if extensions.contains(&oid) {
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
                            "relhasrules" => Bson::Boolean(kind == "v"),
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
                            "reloptions" | "relacl" => Bson::Null,
                            _ => Bson::Int64(0),
                        }
                    }
                    ("pg_index", c) => match c {
                        "indnkeyatts" => get(row, "indnatts").unwrap_or(Bson::Int32(0)),
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
                            "attcollation" => Bson::Int64(type_collation(type_oid)),
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
                            "attstorage" => Bson::String(
                                if matches!(
                                    type_oid,
                                    16 | 18
                                        | 20
                                        | 21
                                        | 23
                                        | 26
                                        | 700
                                        | 701
                                        | 1082
                                        | 1083
                                        | 1114
                                        | 1184
                                        | 1186
                                        | 2950
                                        | 19
                                ) {
                                    "p".into()
                                } else {
                                    "x".into()
                                },
                            ),
                            "attcompression" => Bson::String(String::new()),
                            "attndims" | "attinhcount" => Bson::Int32(0),
                            "attstattarget" => Bson::Int32(-1),
                            "attislocal" => Bson::Boolean(true),
                            _ => Bson::Boolean(false),
                        }
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
