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

const ATTRIBUTES: &str = include_str!("pg_attribute_system.tsv");

/// `pg_attribute` rows for PostgreSQL's own relations' columns -- tables,
/// views, indexes and TOAST relations -- dumped from PostgreSQL 15
/// (`pg_attribute_system.tsv`: attrelid, attname, atttypid, attnum,
/// attnotnull, atttypmod, atthasdef). An `information_schema` column of one
/// of that schema's domains reports the domain's base type.
pub(crate) fn attribute_rows(def: &TableDef) -> Vec<Document> {
    let f = |name: &str| def.field_of(name);
    let mut out = Vec::new();
    for l in ATTRIBUTES.lines() {
        let [relid, name, ty, num, notnull, typmod, hasdef] = l.split('\t').collect::<Vec<_>>()[..]
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
