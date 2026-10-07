//! Schema-qualified relations in the catalogs.
//!
//! A relation outside `public` is stored under `schema.name` (the key the
//! Python server uses too; see `secantus_pgplan::schemas`). The catalog rows
//! are built from those keys, so each row is fixed up here once: the name
//! columns lose the `schema.` prefix and the row's schema / namespace columns
//! name the schema instead of `public`.

use bson::{Bson, Document};
use secantus_pgcatalog::TableDef;

use crate::PgHandler;

/// Columns that carry a relation-derived name.
const NAME_COLUMNS: &[&str] = &[
    "relname",
    "typname",
    "conname",
    "tablename",
    "viewname",
    "matviewname",
    "sequencename",
    "indexname",
    "table_name",
    "sequence_name",
    "constraint_name",
];

/// Columns naming the row's schema as text.
const SCHEMA_TEXT_COLUMNS: &[&str] = &[
    "schemaname",
    "table_schema",
    "constraint_schema",
    "sequence_schema",
];

/// Columns naming the row's schema by namespace oid.
const SCHEMA_OID_COLUMNS: &[&str] = &["relnamespace", "typnamespace", "connamespace"];

impl PgHandler {
    /// Split every `schema.name` the rows carry into its schema and name.
    pub(crate) fn schema_qualify_rows(&self, def: &TableDef, rows: &mut [Document]) {
        let spaces: Vec<(String, i64)> = self
            .namespaces()
            .into_iter()
            .filter(|(n, _)| {
                !matches!(
                    n.as_str(),
                    "public" | "pg_catalog" | "information_schema" | "pg_toast"
                )
            })
            .collect();
        if spaces.is_empty() {
            return;
        }
        let names: Vec<String> = NAME_COLUMNS
            .iter()
            .filter_map(|c| def.field_of(c))
            .collect();
        if names.is_empty() {
            return;
        }
        let texts: Vec<String> = SCHEMA_TEXT_COLUMNS
            .iter()
            .filter_map(|c| def.field_of(c))
            .collect();
        let oids: Vec<String> = SCHEMA_OID_COLUMNS
            .iter()
            .filter_map(|c| def.field_of(c))
            .collect();
        let split = |v: &str| -> Option<(usize, String)> {
            let (prefix, body) = match v.strip_prefix('_') {
                Some(rest) => ("_", rest),
                None => ("", v),
            };
            let (s, n) = body.split_once('.')?;
            let i = spaces.iter().position(|(name, _)| name == s)?;
            Some((i, format!("{prefix}{n}")))
        };
        for row in rows.iter_mut() {
            let mut found: Option<usize> = None;
            for f in &names {
                let Some(Bson::String(v)) = row.get(f) else {
                    continue;
                };
                if let Some((i, bare)) = split(v) {
                    if found.is_none_or(|j| j == i) {
                        found = Some(i);
                        row.insert(f.clone(), bare);
                    }
                }
            }
            let Some(i) = found else {
                continue;
            };
            let (schema, oid) = &spaces[i];
            for f in &texts {
                if matches!(row.get(f), Some(Bson::String(s)) if s == "public") {
                    row.insert(f.clone(), schema.as_str());
                }
            }
            for f in &oids {
                let public = match row.get(f) {
                    Some(Bson::Int64(o)) => *o == Self::PUBLIC_NAMESPACE_OID,
                    Some(Bson::Int32(o)) => i64::from(*o) == Self::PUBLIC_NAMESPACE_OID,
                    _ => false,
                };
                if public {
                    row.insert(f.clone(), Bson::Int64(*oid));
                }
            }
        }
    }
}

impl PgHandler {
    /// The relations stored in schema `schema`, as `(kind, key)` in the
    /// order a CASCADE drops them: views, then tables, then sequences.
    pub(crate) fn schema_relations(
        &self,
        schema: &str,
    ) -> pgwire::error::PgWireResult<Vec<(&'static str, String)>> {
        let prefix = format!("{schema}.");
        let mut out: Vec<(&'static str, String)> = Vec::new();
        for (v, _) in self.views()? {
            if v.starts_with(&prefix) {
                out.push(("view", v));
            }
        }
        for t in self.all_table_defs()? {
            if t.name.starts_with(&prefix) {
                out.push(("table", t.name));
            }
        }
        for d in self.all_sequence_docs()? {
            if let Ok(id) = d.get_str("_id") {
                if id.starts_with(&prefix) {
                    out.push(("sequence", id.to_string()));
                }
            }
        }
        Ok(out)
    }

    /// Drop what `schema_relations` found, each with CASCADE so a view or a
    /// foreign key reaching into the schema goes with it.
    pub(crate) fn drop_schema_relations(
        &self,
        relations: &[(&'static str, String)],
    ) -> pgwire::error::PgWireResult<()> {
        for (kind, key) in relations {
            let quoted = format!("\"{}\"", key.replace('"', "\"\""));
            let sql = match *kind {
                "view" => format!("DROP VIEW IF EXISTS {quoted} CASCADE"),
                "table" => format!("DROP TABLE IF EXISTS {quoted} CASCADE"),
                _ => format!("DROP SEQUENCE IF EXISTS {quoted}"),
            };
            let stmt =
                secantus_pgplan::plan(&sql, &|n| self.lookup(n)).map_err(|e| Self::err(&e))?;
            self.execute_inner(stmt, 0)?;
        }
        Ok(())
    }
}

impl PgHandler {
    /// An execution error with each quoted `"schema.name"` catalog key shown
    /// as PostgreSQL names the object -- by its bare name (`relation "t"
    /// already exists`, `constraint "t_pkey"`, `table "t" does not exist`
    /// from a DROP). A `relation ... does not exist` keeps the
    /// qualification: it echoes the name the statement was written with.
    pub(crate) fn unqualify_error(
        &self,
        e: pgwire::error::PgWireError,
    ) -> pgwire::error::PgWireError {
        let pgwire::error::PgWireError::UserError(mut info) = e else {
            return e;
        };
        if info.code == "42P01" && info.message.starts_with("relation ") {
            return pgwire::error::PgWireError::UserError(info);
        }
        info.message = unqualify_text(&info.message);
        if let Some(d) = info.detail.take() {
            info.detail = Some(unqualify_text(&d));
        }
        pgwire::error::PgWireError::UserError(info)
    }
}

/// `text` with every `"schema.name"` whose schema is a user schema reduced
/// to `"name"`.
pub(crate) fn unqualify_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('"') {
        out.push_str(&rest[..=start]);
        rest = &rest[start + 1..];
        let Some(end) = rest.find('"') else {
            break;
        };
        let quoted = &rest[..end];
        let (schema, name) = secantus_pgplan::schemas::split_key(quoted);
        if schema != "public" {
            out.push_str(&name);
        } else {
            out.push_str(quoted);
        }
        out.push('"');
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}
