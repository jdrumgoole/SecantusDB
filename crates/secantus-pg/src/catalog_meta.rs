//! PostgreSQL's own relations as `information_schema` lists them: every
//! `pg_catalog` and `information_schema` table and view, and their columns,
//! measured from PostgreSQL 15.19 (`catalog_relations.tsv`,
//! `catalog_columns.tsv`). A client that browses the system schemas through
//! `information_schema` sees what it would see on PostgreSQL.

use bson::{Bson, Document};
use secantus_pgcatalog::TableDef;

const RELATIONS: &str = include_str!("catalog_relations.tsv");
const COLUMNS: &str = include_str!("catalog_columns.tsv");

fn cell(s: &str) -> Option<&str> {
    (s != "\\N").then_some(s)
}

fn text(s: &str) -> Bson {
    cell(s).map_or(Bson::Null, |v| Bson::String(v.to_string()))
}

fn number(s: &str) -> Bson {
    cell(s)
        .and_then(|v| v.parse::<i32>().ok())
        .map_or(Bson::Null, Bson::Int32)
}

/// `information_schema.tables` rows for the system schemas.
pub(crate) fn relation_rows(def: &TableDef, db: &str) -> Vec<Document> {
    let f = |name: &str| def.field_of(name).expect("column");
    RELATIONS
        .lines()
        .filter_map(|l| {
            let [schema, name, kind] = l.split('\t').collect::<Vec<_>>()[..] else {
                return None;
            };
            let mut d = Document::new();
            d.insert(f("table_catalog"), db);
            d.insert(f("table_schema"), schema);
            d.insert(f("table_name"), name);
            d.insert(f("table_type"), kind);
            for k in [
                "self_referencing_column_name",
                "reference_generation",
                "user_defined_type_catalog",
                "user_defined_type_schema",
                "user_defined_type_name",
                "commit_action",
            ] {
                if let Some(field) = def.field_of(k) {
                    d.insert(field, Bson::Null);
                }
            }
            // Every table, and the two system views PostgreSQL 15.19 finds
            // auto-updatable.
            let insertable =
                kind == "BASE TABLE" || matches!(name, "pg_group" | "pg_stat_database_conflicts");
            if let Some(field) = def.field_of("is_insertable_into") {
                d.insert(field, if insertable { "YES" } else { "NO" });
            }
            if let Some(field) = def.field_of("is_typed") {
                d.insert(field, "NO");
            }
            Some(d)
        })
        .collect()
}

/// `information_schema.columns` rows for the system schemas.
pub(crate) fn column_rows(def: &TableDef, db: &str) -> Vec<Document> {
    let has = |name: &str| def.field_of(name);
    COLUMNS
        .lines()
        .filter_map(|l| {
            let v: Vec<&str> = l.split('\t').collect();
            let [schema, table, column, ordinal, default, nullable, data_type, charlen, precision, scale, dtprec, udt, identity, collation, generated, domain, udt_schema] =
                v[..]
            else {
                return None;
            };
            let mut d = Document::new();
            let mut put = |k: &str, b: Bson| {
                if let Some(field) = has(k) {
                    d.insert(field, b);
                }
            };
            put("table_catalog", Bson::String(db.to_string()));
            put("table_schema", text(schema));
            put("table_name", text(table));
            put("column_name", text(column));
            put("ordinal_position", number(ordinal));
            put("column_default", text(default));
            put("is_nullable", text(nullable));
            put("data_type", text(data_type));
            put("character_maximum_length", number(charlen));
            put("numeric_precision", number(precision));
            put("numeric_scale", number(scale));
            put("datetime_precision", number(dtprec));
            put("udt_catalog", Bson::String(db.to_string()));
            put("udt_schema", text(udt_schema));
            put("udt_name", text(udt));
            put("is_identity", text(identity));
            put("identity_generation", Bson::Null);
            put(
                "collation_catalog",
                if cell(collation).is_some() {
                    Bson::String(db.to_string())
                } else {
                    Bson::Null
                },
            );
            put(
                "collation_schema",
                if cell(collation).is_some() {
                    Bson::String("pg_catalog".into())
                } else {
                    Bson::Null
                },
            );
            put("collation_name", text(collation));
            put("domain_catalog", if cell(domain).is_some() { Bson::String(db.to_string()) } else { Bson::Null });
            put("domain_schema", if cell(domain).is_some() { Bson::String("information_schema".into()) } else { Bson::Null });
            put("domain_name", text(domain));
            put("is_generated", text(generated));
            put("generation_expression", Bson::Null);
            // The columns the dump does not carry, derived as for a table's.
            let mut derived = Document::new();
            crate::info_columns::fill(
                &mut derived,
                def,
                udt,
                -1,
                ordinal.parse().unwrap_or(0),
                udt_schema,
                db,
            );
            for (k, v) in derived {
                if !d.contains_key(&k) {
                    d.insert(k, v);
                }
            }
            Some(d)
        })
        .collect()
}

const CLASSES: &str = include_str!("pg_class_system.tsv");

/// `pg_class` rows for PostgreSQL 15.19's own relations -- every table,
/// view, index and TOAST relation in `pg_catalog`, `information_schema` and
/// `pg_toast` (`pg_class_system.tsv`) -- under this server's namespace oids.
pub(crate) fn class_rows(def: &TableDef, namespace_oid: impl Fn(&str) -> i64) -> Vec<Document> {
    let f = |name: &str| def.field_of(name);
    let mut out = Vec::new();
    for line in CLASSES.lines() {
        let c: Vec<&str> = line.split('\t').collect();
        let [oid, relname, nsp, kind, natts, hasindex, shared, reltype, relam, toast, persistence, hasrules, checks, filenode, tablespace] =
            c.as_slice()
        else {
            continue;
        };
        let int = |s: &str| s.parse::<i64>().unwrap_or(0);
        let mut d = Document::new();
        let mut put = |name: &str, v: Bson| {
            if let Some(field) = f(name) {
                d.insert(field, v);
            }
        };
        put("oid", Bson::Int64(int(oid)));
        put("relname", Bson::String((*relname).to_string()));
        put("relnamespace", Bson::Int64(namespace_oid(nsp)));
        put("relkind", Bson::String((*kind).to_string()));
        put("relnatts", Bson::Int32(int(natts) as i32));
        put("relhasindex", Bson::Boolean(*hasindex == "1"));
        put("relisshared", Bson::Boolean(*shared == "1"));
        put("reltype", Bson::Int64(int(reltype)));
        put("relam", Bson::Int64(int(relam)));
        put("reltoastrelid", Bson::Int64(int(toast)));
        put("relpersistence", Bson::String((*persistence).to_string()));
        put("relhasrules", Bson::Boolean(*hasrules == "1"));
        put("relchecks", Bson::Int32(int(checks) as i32));
        put("relfilenode", Bson::Int64(int(filenode)));
        put("reltablespace", Bson::Int64(int(tablespace)));
        put("reltuples", Bson::Double(-1.0));
        put("relowner", Bson::Int64(10));
        put("relrowsecurity", Bson::Boolean(false));
        put("relforcerowsecurity", Bson::Boolean(false));
        put("relispartition", Bson::Boolean(false));
        put("relpartbound", Bson::Null);
        put("relhastriggers", Bson::Boolean(false));
        put("relhassubclass", Bson::Boolean(false));
        put("relacl", system_relacl(relname, nsp, kind));
        out.push(d);
    }
    out
}

/// `relacl` of one of PostgreSQL's own relations, as initdb leaves it
/// (15.19, bootstrap superuser `postgres`): every catalog table and view
/// readable by PUBLIC, except the few holding secrets or server state,
/// which only the superuser (or `pg_read_all_stats`) reads, and
/// `pg_settings`, which PUBLIC may also UPDATE (`SET`). Indexes, TOAST
/// relations and sequences carry none.
fn system_relacl(relname: &str, nsp: &str, kind: &str) -> Bson {
    if !matches!(kind, "r" | "v") || nsp == "pg_toast" {
        return Bson::Null;
    }
    let owner = "postgres=arwdDxt/postgres";
    let items: Vec<String> = match relname {
        "pg_authid"
        | "pg_config"
        | "pg_file_settings"
        | "pg_hba_file_rules"
        | "pg_ident_file_mappings"
        | "pg_largeobject"
        | "pg_replication_origin_status"
        | "pg_shadow"
        | "pg_statistic"
        | "pg_statistic_ext_data"
        | "pg_subscription"
        | "pg_user_mapping"
            if nsp == "pg_catalog" =>
        {
            vec![owner.into()]
        }
        "pg_backend_memory_contexts" | "pg_shmem_allocations" if nsp == "pg_catalog" => {
            vec![owner.into(), "pg_read_all_stats=r/postgres".into()]
        }
        "pg_settings" if nsp == "pg_catalog" => vec![owner.into(), "=rw/postgres".into()],
        _ => vec![owner.into(), "=r/postgres".into()],
    };
    Bson::Array(items.into_iter().map(Bson::String).collect())
}

/// Install PostgreSQL's own relations in the planner, once: `'pg_statistic'
/// ::regclass`, `'information_schema.tables'::regclass` and their rendering.
pub(crate) fn install_system_relations() {
    static DONE: std::sync::Once = std::sync::Once::new();
    DONE.call_once(|| {
        secantus_pgplan::set_system_relations(
            CLASSES
                .lines()
                .filter_map(|l| {
                    let c: Vec<&str> = l.split('\t').collect();
                    Some((
                        c.get(2)?.to_string(),
                        c.get(1)?.to_string(),
                        c.first()?.parse().ok()?,
                    ))
                })
                .collect(),
        );
    });
}

/// The lines of `pg_class_system.tsv`.
pub(crate) fn system_class_lines() -> std::str::Lines<'static> {
    CLASSES.lines()
}

/// The lines of `pg_index_system.tsv`.
pub(crate) fn system_index_lines() -> std::str::Lines<'static> {
    INDEXES.lines()
}

const INDEXES: &str = include_str!("pg_index_system.tsv");

/// `pg_index` rows for PostgreSQL 15.19's own indexes (`pg_index_system.tsv`,
/// dumped with `indexrelid < 16384`; the same 162 indexes
/// `pg_class_system.tsv` lists): indexrelid, indrelid, indnatts,
/// indnkeyatts, the ten flags in catalog order, indkey, indcollation,
/// indclass, indoption.
/// How many of PostgreSQL's own indexes are on relation `oid`.
pub(crate) fn system_index_count(oid: i64) -> i64 {
    let key = oid.to_string();
    INDEXES
        .lines()
        .filter(|l| l.split('\t').nth(1) == Some(key.as_str()))
        .count() as i64
}

pub(crate) fn index_rows(def: &TableDef) -> Vec<Document> {
    let f = |name: &str| def.field_of(name);
    let mut out = Vec::new();
    for l in INDEXES.lines() {
        let c: Vec<&str> = l.split('\t').collect();
        let [indexrelid, indrelid, natts, nkeyatts, unique, nnd, primary, exclusion, immediate, clustered, valid, checkxmin, ready, live, replident, key, collation, class, option] =
            c.as_slice()
        else {
            continue;
        };
        let int = |s: &str| s.parse::<i64>().unwrap_or(0);
        let vector = |s: &str, wide: bool| {
            Bson::Array(
                s.split_whitespace()
                    .map(|v| {
                        if wide {
                            Bson::Int64(int(v))
                        } else {
                            Bson::Int32(int(v) as i32)
                        }
                    })
                    .collect(),
            )
        };
        let mut d = Document::new();
        let mut put = |name: &str, v: Bson| {
            if let Some(field) = f(name) {
                d.insert(field, v);
            }
        };
        put("indexrelid", Bson::Int64(int(indexrelid)));
        put("indrelid", Bson::Int64(int(indrelid)));
        put("indnatts", Bson::Int32(int(natts) as i32));
        put("indnkeyatts", Bson::Int32(int(nkeyatts) as i32));
        for (name, v) in [
            ("indisunique", unique),
            ("indnullsnotdistinct", nnd),
            ("indisprimary", primary),
            ("indisexclusion", exclusion),
            ("indimmediate", immediate),
            ("indisclustered", clustered),
            ("indisvalid", valid),
            ("indcheckxmin", checkxmin),
            ("indisready", ready),
            ("indislive", live),
            ("indisreplident", replident),
        ] {
            put(name, Bson::Boolean(*v == "1"));
        }
        put("indkey", vector(key, false));
        put("indcollation", vector(collation, true));
        put("indclass", vector(class, true));
        put("indoption", vector(option, false));
        put("indexprs", Bson::Null);
        put("indpred", Bson::Null);
        out.push(d);
    }
    out
}

/// One of PostgreSQL's own indexes as `pg_indexes` / `pg_get_indexdef`
/// print it.
pub(crate) struct SystemIndex {
    pub oid: i64,
    pub schema: String,
    pub table: String,
    pub name: String,
    /// The table is shared across databases: its tablespace is `pg_global`.
    pub shared: bool,
    pub def: String,
}

/// Every system index's definition, built from the dumped catalogs: all
/// are btree over plain columns with default operator classes and options
/// (checked against PostgreSQL 15.19's `pg_get_indexdef`).
pub(crate) fn system_indexes() -> &'static [SystemIndex] {
    static OUT: std::sync::OnceLock<Vec<SystemIndex>> = std::sync::OnceLock::new();
    OUT.get_or_init(|| {
        let classes: Vec<Vec<&str>> = CLASSES.lines().map(|l| l.split('\t').collect()).collect();
        let class = |oid: &str| classes.iter().find(|c| c.first() == Some(&oid));
        let column = |rel: &str, num: &str| {
            ATTRIBUTES.lines().find_map(|l| {
                let c: Vec<&str> = l.split('\t').collect();
                (c.first() == Some(&rel) && c.get(3) == Some(&num)).then(|| c[1].to_string())
            })
        };
        INDEXES
            .lines()
            .filter_map(|l| {
                let c: Vec<&str> = l.split('\t').collect();
                let (index, table) = (class(c.first()?)?, class(c.get(1)?)?);
                let columns: Vec<String> = c
                    .get(15)?
                    .split_whitespace()
                    .map(|n| column(c[1], n))
                    .collect::<Option<_>>()?;
                let unique = if c.get(4) == Some(&"1") {
                    "UNIQUE "
                } else {
                    ""
                };
                Some(SystemIndex {
                    oid: c[0].parse().ok()?,
                    schema: table[2].to_string(),
                    table: table[1].to_string(),
                    name: index[1].to_string(),
                    shared: table[6] == "1",
                    def: format!(
                        "CREATE {unique}INDEX {} ON {}.{} USING btree ({})",
                        index[1],
                        table[2],
                        table[1],
                        columns.join(", ")
                    ),
                })
            })
            .collect()
    })
}

const ATTRIBUTES: &str = include_str!("pg_attribute_system.tsv");

/// `(relation oid, attnum)` of a column of one of PostgreSQL's own
/// relations, by the name this server gives the relation (`pg_class`,
/// `information_schema.tables`) and the column's name.
pub(crate) fn system_column_source(relation: &str, column: &str) -> Option<(i64, i16)> {
    static MAP: std::sync::OnceLock<std::collections::HashMap<(String, String), (i64, i16)>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| {
        let mut names = std::collections::HashMap::new();
        for line in CLASSES.lines() {
            let c: Vec<&str> = line.split('\t').collect();
            let (Some(oid), Some(rel), Some(nsp)) = (c.first(), c.get(1), c.get(2)) else {
                continue;
            };
            let key = match *nsp {
                "pg_catalog" => (*rel).to_string(),
                "information_schema" => format!("information_schema.{rel}"),
                _ => continue,
            };
            names.insert(oid.to_string(), key);
        }
        let mut map = std::collections::HashMap::new();
        for line in ATTRIBUTES.lines() {
            let c: Vec<&str> = line.split('\t').collect();
            let (Some(rel), Some(att), Some(num)) = (c.first(), c.get(1), c.get(3)) else {
                continue;
            };
            let (Some(name), Ok(oid), Ok(num)) = (names.get(*rel), rel.parse(), num.parse()) else {
                continue;
            };
            if num > 0 {
                map.insert((name.clone(), (*att).to_string()), (oid, num));
            }
        }
        map
    })
    .get(&(relation.to_string(), column.to_string()))
    .copied()
}

/// `pg_attribute` rows for PostgreSQL's own relations' columns -- tables,
/// views, indexes and TOAST relations -- dumped from PostgreSQL 15
/// (`pg_attribute_system.tsv`: attrelid, attname, the column's type with an
/// `information_schema` domain smashed to its base type (what this server
/// computes the column as), attnum, attnotnull, atttypmod, atthasdef, then
/// the column's declared type and its collation as PostgreSQL 15.19 records
/// them).
pub(crate) fn attribute_rows(def: &TableDef) -> Vec<Document> {
    let f = |name: &str| def.field_of(name);
    let mut out = Vec::new();
    for l in ATTRIBUTES.lines() {
        let [relid, name, _base, num, notnull, typmod, hasdef, ty, collation] =
            l.split('\t').collect::<Vec<_>>()[..]
        else {
            continue;
        };
        let int = |s: &str| s.parse::<i64>().unwrap_or(0);
        let mut d = Document::new();
        let mut put = |k: &str, b: Bson| {
            if let Some(field) = f(k) {
                d.insert(field, b);
            }
        };
        put("attrelid", Bson::Int64(int(relid)));
        put("attname", Bson::String(name.to_string()));
        put("atttypid", Bson::Int64(int(ty)));
        put("attnum", Bson::Int32(int(num) as i32));
        put("attisdropped", Bson::Boolean(false));
        put("attnotnull", Bson::Boolean(notnull == "1"));
        put("atttypmod", Bson::Int32(int(typmod) as i32));
        put("atthasdef", Bson::Boolean(hasdef == "1"));
        put("attcollation", Bson::Int64(int(collation)));
        put("attgenerated", Bson::String(String::new()));
        out.push(d);
    }
    out
}

/// PostgreSQL 15's `pg_get_keywords()`, dumped from the reference server:
/// `word`, `catcode`, `barelabel`, `catdesc`, `baredesc`.
const KEYWORDS: &str = include_str!("pg_keywords.tsv");

/// `pg_get_keywords()`, served as a relation of that name (the planner
/// rewrites the FROM call to it).
pub(crate) fn pg_get_keywords_def() -> TableDef {
    TableDef::new(
        "pg_get_keywords",
        vec![
            secantus_pgcatalog::Column::new("word", "text", false),
            secantus_pgcatalog::Column::new("catcode", secantus_pgplan::QUOTED_CHAR, false),
            secantus_pgcatalog::Column::new("barelabel", "bool", false),
            secantus_pgcatalog::Column::new("catdesc", "text", false),
            secantus_pgcatalog::Column::new("baredesc", "text", false),
        ],
    )
}

pub(crate) fn pg_get_keywords_rows(def: &TableDef) -> Vec<Document> {
    let f = |c: &str| def.field_of(c).expect("column");
    KEYWORDS
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let (word, code, bare, catdesc, baredesc) = (
                parts.next()?,
                parts.next()?,
                parts.next()?,
                parts.next()?,
                parts.next()?,
            );
            let mut d = Document::new();
            d.insert(f("word"), word);
            d.insert(f("catcode"), code);
            d.insert(f("barelabel"), bare == "t");
            d.insert(f("catdesc"), catdesc);
            d.insert(f("baredesc"), baredesc);
            Some(d)
        })
        .collect()
}

/// `(name, type oid, typmod)` for each column of one of PostgreSQL's own
/// relations, in attnum order, by the name this server gives it (`pg_amop`,
/// `information_schema.sql_features`).
pub(crate) fn system_relation_columns(relation: &str) -> Option<Vec<(String, i64, i32)>> {
    let (nsp, rel) = match relation.split_once('.') {
        Some(("information_schema", r)) => ("information_schema", r),
        Some(_) => return None,
        None => ("pg_catalog", relation),
    };
    let oid = CLASSES.lines().find_map(|line| {
        let c: Vec<&str> = line.split('\t').collect();
        (c.get(1) == Some(&rel) && c.get(2) == Some(&nsp)).then(|| c[0].to_string())
    })?;
    let mut cols: Vec<(i32, String, i64, i32)> = ATTRIBUTES
        .lines()
        .filter_map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            if c.first() != Some(&oid.as_str()) {
                return None;
            }
            let num: i32 = c.get(3)?.parse().ok()?;
            (num > 0).then(|| {
                (
                    num,
                    c[1].to_string(),
                    c[2].parse().unwrap_or(25),
                    c.get(5).and_then(|t| t.parse().ok()).unwrap_or(-1),
                )
            })
        })
        .collect();
    cols.sort_by_key(|c| c.0);
    Some(cols.into_iter().map(|(_, n, t, m)| (n, t, m)).collect())
}

/// A system relation's column name by number (`pg_attribute_system.tsv`).
pub(crate) fn system_attribute_name(relid: i64, attnum: i64) -> Option<String> {
    let (relid, attnum) = (relid.to_string(), attnum.to_string());
    ATTRIBUTES.lines().find_map(|l| {
        let c: Vec<&str> = l.split('\t').collect();
        (c.first() == Some(&relid.as_str()) && c.get(3) == Some(&attnum.as_str()))
            .then(|| c[1].to_string())
    })
}
