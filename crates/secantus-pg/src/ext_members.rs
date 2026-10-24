//! An extension's member objects in the catalogs (RP-105): the functions,
//! types, operators, casts and operator classes and families its script
//! makes, with its families' operators and support functions, as
//! PostgreSQL 15.19's `CREATE EXTENSION` leaves them
//! (`pg15_extension_rows.txt`, written by
//! `tools/probes/pg15_extension_rows_dump.py`).
//!
//! Installing an extension copies its rows into the database, each oid the
//! dump drew moved to one of the database's own: a type the extension
//! already makes here (`citext`, `hstore`) keeps its oid, the rest take a
//! block from the oid counter. The rows' schema is the extension's, their
//! owner the role that installed it. `pg_depend` lists each member's `e`
//! row from what the install recorded.

use std::collections::HashMap;

use bson::{doc, Bson, Document};
use pgwire::error::PgWireResult;
use secantus_pgcatalog::TableDef;

use crate::{catalog_meta, PgHandler};

const ROWS: &str = include_str!("pg15_extension_rows.txt");

/// Where an installed extension's rows are kept: one document per
/// extension, `{_id: name, rows: {catalog: text}, members: [[classid,
/// oid]]}`.
pub(crate) const COLLECTION: &str = "__sql_extension_rows__";

/// The catalogs an extension's rows appear in.
pub(crate) const CATALOGS: &[&str] = &[
    "pg_type",
    "pg_proc",
    "pg_operator",
    "pg_cast",
    "pg_opfamily",
    "pg_opclass",
    "pg_amop",
    "pg_amproc",
];

/// Each catalog's `pg_class` oid, for its members' `pg_depend.classid`.
fn classid(catalog: &str) -> Option<i64> {
    Some(match catalog {
        "pg_type" => 1247,
        "pg_proc" => 1255,
        "pg_operator" => 2617,
        "pg_cast" => 2605,
        "pg_opfamily" => 2753,
        "pg_opclass" => 2616,
        _ => return None,
    })
}

/// The dumped blocks of `extension`: `(catalog, its rows' COPY text)`.
fn dumped(extension: &str) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut current: Option<&str> = None;
    for line in ROWS.split_inclusive('\n') {
        if let Some(name) = line.strip_prefix("\\extension ") {
            current = Some(name.trim());
            continue;
        }
        if current != Some(extension) {
            continue;
        }
        if line.trim_end() == "\\signatures" {
            out.push(("signatures", String::new()));
            continue;
        }
        if let Some(rel) = line.strip_prefix("\\relation ") {
            let rel = rel.trim();
            let rel = rel.strip_prefix("pg_catalog.").unwrap_or(rel);
            if let Some(c) = CATALOGS.iter().find(|c| **c == rel) {
                out.push((c, String::new()));
            }
            continue;
        }
        if let Some((_, text)) = out.last_mut() {
            text.push_str(line);
        }
    }
    out
}

/// A dumped text field: `\\`, `\t` and `\n` read back.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Is a column of type `type_oid` an oid, or a list of them?
fn holds_oids(type_oid: i64) -> bool {
    matches!(
        type_oid,
        // oid, regproc, regprocedure, regoper, regoperator, regclass,
        // regtype, oidvector, oid[]
        26 | 24 | 2202 | 2203 | 2204 | 2205 | 2206 | 30 | 1028
    )
}

/// `text` with every number in `map` replaced by its new value.
fn remap_numbers(text: &str, map: &HashMap<i64, i64>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut num = String::new();
    let flush = |num: &mut String, out: &mut String| {
        if !num.is_empty() {
            match num.parse::<i64>().ok().and_then(|n| map.get(&n)) {
                Some(new) => out.push_str(&new.to_string()),
                None => out.push_str(num),
            }
            num.clear();
        }
    };
    for c in text.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            flush(&mut num, &mut out);
            out.push(c);
        }
    }
    flush(&mut num, &mut out);
    out
}

impl PgHandler {
    /// `CREATE EXTENSION`: the extension's rows into the database, in
    /// `schema`, its own types (`types`, already made) kept at their oids.
    pub(crate) fn install_extension_members(
        &self,
        extension: &str,
        schema: &str,
        types: &[(String, i64)],
    ) -> PgWireResult<()> {
        let blocks = dumped(extension);
        if blocks.is_empty() {
            return Ok(());
        }
        // The dump's own objects, by oid: a row's first column.
        let mut drawn: Vec<i64> = Vec::new();
        let mut kept: HashMap<i64, i64> = HashMap::new();
        for (catalog, text) in blocks.iter().filter(|(c, _)| *c != "signatures") {
            for line in text.lines() {
                let mut cols = line.split('\t');
                let Some(oid) = cols.next().and_then(|o| o.parse::<i64>().ok()) else {
                    continue;
                };
                if *catalog == "pg_type" {
                    let name = cols.next().unwrap_or_default();
                    let base = name.strip_prefix('_').unwrap_or(name);
                    if let Some((_, own)) = types.iter().find(|(t, _)| t == base) {
                        let own = if name.starts_with('_') {
                            own + Self::USER_TYPE_ARRAY_OID_OFFSET
                        } else {
                            *own
                        };
                        kept.insert(oid, own);
                        continue;
                    }
                }
                drawn.push(oid);
            }
        }
        drawn.sort_unstable();
        drawn.dedup();
        let first = self.mint_composite_oids(drawn.len())?;
        let mut map: HashMap<i64, i64> = drawn
            .iter()
            .enumerate()
            .map(|(i, o)| (*o, first + i as i64))
            .collect();
        map.extend(kept.iter().map(|(o, n)| (*o, *n)));
        // A new type's array is its oid and USER_TYPE_ARRAY_OID_OFFSET, as
        // every user type's array is here; and the types themselves, for
        // the planner's `regtype`.
        let mut new_types: Vec<Bson> = Vec::new();
        if let Some((_, text)) = blocks.iter().find(|(c, _)| *c == "pg_type") {
            let by_name: HashMap<&str, i64> = text
                .lines()
                .filter_map(|l| {
                    let mut c = l.split('\t');
                    let o = c.next()?.parse::<i64>().ok()?;
                    Some((c.next()?, o))
                })
                .collect();
            for (name, old) in &by_name {
                if kept.contains_key(old) {
                    continue;
                }
                match name.strip_prefix('_') {
                    Some(element) => {
                        if let Some(e) = by_name.get(element).and_then(|e| map.get(e)).copied() {
                            map.insert(*old, e + Self::USER_TYPE_ARRAY_OID_OFFSET);
                        }
                    }
                    None => new_types.push(Bson::Array(vec![
                        Bson::String((*name).to_string()),
                        Bson::Int64(map.get(old).copied().unwrap_or(*old)),
                    ])),
                }
            }
        }
        // Each of its types' names, for its functions' `regprocedure`.
        let mut type_names: HashMap<i64, String> = HashMap::new();
        for (catalog, text) in &blocks {
            if *catalog != "pg_type" {
                continue;
            }
            for line in text.lines() {
                let mut cols = line.split('\t');
                let (Some(oid), Some(name)) =
                    (cols.next().and_then(|o| o.parse::<i64>().ok()), cols.next())
                else {
                    continue;
                };
                let shown = match name.strip_prefix('_') {
                    Some(element) => format!("{element}[]"),
                    None => name.to_string(),
                };
                type_names.insert(map.get(&oid).copied().unwrap_or(oid), shown);
            }
        }
        let type_name = |oid: i64| -> String {
            type_names
                .get(&oid)
                .cloned()
                .unwrap_or_else(|| secantus_pgplan::regtype_text(oid))
        };
        let mut procs: Vec<Bson> = Vec::new();
        let namespace = self.namespace_oid(schema);
        let owner = self.role_oid_of(&self.current_role_name());
        let mut rows = Document::new();
        let mut members: Vec<Bson> = Vec::new();
        let mut sigs: Vec<Bson> = Vec::new();
        let mut defs: Vec<Bson> = Vec::new();
        for (catalog, text) in &blocks {
            if *catalog == "signatures" {
                // `oid<TAB>arguments<TAB>identity<TAB>result<TAB>definition`,
                // its oid moved.
                for line in text.lines() {
                    let f: Vec<&str> = line.split('\t').collect();
                    let (Some(old), [_, a, i, r, d]) =
                        (f.first().and_then(|o| o.parse::<i64>().ok()), f.as_slice())
                    else {
                        continue;
                    };

                    let new = map.get(&old).copied().unwrap_or(old);
                    sigs.push(Bson::Array(vec![
                        Bson::Int64(new),
                        Bson::String(unescape(a)),
                        Bson::String(unescape(i)),
                        Bson::String(unescape(r)),
                    ]));
                    defs.push(Bson::Array(vec![
                        Bson::Int64(new),
                        Bson::String(unescape(d)),
                    ]));
                }
                continue;
            }
            let columns = catalog_meta::system_relation_columns(catalog).unwrap_or_default();
            let mut out = String::new();
            for line in text.lines() {
                let fields: Vec<&str> = line.split('\t').collect();
                let Some(old) = fields.first().and_then(|o| o.parse::<i64>().ok()) else {
                    continue;
                };
                // A type the extension already makes here is listed already.
                if *catalog == "pg_type" && kept.contains_key(&old) {
                    continue;
                }
                let fields: Vec<String> = fields
                    .iter()
                    .zip(columns.iter())
                    .map(|(value, (name, type_oid, _))| {
                        if *value == "\\N" || !holds_oids(*type_oid) {
                            return (*value).to_string();
                        }
                        if name.ends_with("namespace") && *value == "2200" {
                            return namespace.to_string();
                        }
                        if name.ends_with("owner") && *value == "10" {
                            return owner.to_string();
                        }
                        remap_numbers(value, &map)
                    })
                    .collect();
                if *catalog == "pg_proc" {
                    // `(name, oid, argument types)`, as `regprocedure` reads
                    // and prints it.
                    let at = |col: &str| columns.iter().position(|(n, _, _)| n == col);
                    if let (Some(n), Some(a)) = (at("proname"), at("proargtypes")) {
                        let args: Vec<String> = fields
                            .get(a)
                            .map(|v| {
                                v.split_whitespace()
                                    .filter_map(|o| o.parse::<i64>().ok())
                                    .map(type_name)
                                    .collect()
                            })
                            .unwrap_or_default();
                        let name = fields.get(n).cloned().unwrap_or_default();
                        let name = if schema == "public" {
                            name
                        } else {
                            secantus_pgplan::schemas::relation_key(schema, &name)
                        };
                        procs.push(Bson::Array(vec![
                            Bson::String(name),
                            Bson::Int64(map.get(&old).copied().unwrap_or(old)),
                            Bson::String(args.join(",")),
                        ]));
                    }
                }
                out.push_str(&fields.join("\t"));
                out.push('\n');
                // A member, as `pg_depend` lists it: not an array type, and
                // not an operator family's member (which belongs to the
                // family).
                let is_array =
                    *catalog == "pg_type" && fields.get(1).is_some_and(|n| n.starts_with('_'));
                if let (Some(class), false) = (classid(catalog), is_array) {
                    members.push(Bson::Array(vec![
                        Bson::Int64(class),
                        Bson::Int64(map.get(&old).copied().unwrap_or(old)),
                    ]));
                }
            }
            rows.insert(*catalog, out);
        }
        self.ensure_collection(COLLECTION)?;
        let d = doc! {
            "_id": extension,
            "rows": rows,
            "members": members,
            "sigs": sigs,
            "defs": defs,
            "procs": procs,
            "types": new_types,
            "schema": schema,
        };
        self.put(COLLECTION, extension, d)
    }

    /// `DROP EXTENSION`: its rows go with it.
    pub(crate) fn drop_extension_members(&self, extension: &str) -> PgWireResult<()> {
        if self.catalog_doc(COLLECTION, extension).is_some() {
            self.delete_type_doc(COLLECTION, extension)?;
        }
        Ok(())
    }

    /// The installed extensions' rows of `catalog`.
    pub(crate) fn extension_member_rows(&self, catalog: &str, def: &TableDef) -> Vec<Document> {
        let mut out = Vec::new();
        for d in self.catalog_docs(COLLECTION) {
            if let Ok(text) = d.get_document("rows").and_then(|r| r.get_str(catalog)) {
                out.extend(self.static_rows_of(catalog, def, text));
            }
        }
        out
    }

    /// The installed extensions' functions' signatures, `(oid, arguments,
    /// identity arguments, result)`, as `pg_get_function_*` answer them.
    pub(crate) fn extension_function_sigs(&self) -> Vec<(i64, String, String, String)> {
        let mut out = Vec::new();
        for d in self.catalog_docs(COLLECTION) {
            for s in d.get_array("sigs").map(|a| a.to_vec()).unwrap_or_default() {
                let Bson::Array(f) = s else { continue };
                let text = |i: usize| {
                    f.get(i)
                        .and_then(Bson::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                if let Some(oid) = f.first().and_then(crate::bson_i64) {
                    out.push((oid, text(1), text(2), text(3)));
                }
            }
        }
        out
    }

    /// The installed extensions' functions' `pg_get_functiondef` (an
    /// aggregate's as `!` and its name, which it refuses).
    pub(crate) fn extension_function_defs(&self) -> Vec<(i64, String)> {
        let mut out = Vec::new();
        for d in self.catalog_docs(COLLECTION) {
            for p in d.get_array("defs").map(|a| a.to_vec()).unwrap_or_default() {
                let Bson::Array(f) = p else { continue };
                if let (Some(o), Some(Bson::String(t))) =
                    (f.first().and_then(crate::bson_i64), f.get(1))
                {
                    out.push((o, t.clone()));
                }
            }
        }
        out
    }

    /// The installed extensions' functions, `(name, oid, argument types)`,
    /// for `regproc` / `regprocedure`.
    pub(crate) fn extension_procs(&self) -> Vec<(String, i64, String)> {
        let mut out = Vec::new();
        for d in self.catalog_docs(COLLECTION) {
            for p in d.get_array("procs").map(|a| a.to_vec()).unwrap_or_default() {
                let Bson::Array(f) = p else { continue };
                if let (Some(Bson::String(n)), Some(o), Some(Bson::String(a))) =
                    (f.first(), f.get(1).and_then(crate::bson_i64), f.get(2))
                {
                    out.push((n.clone(), o, a.clone()));
                }
            }
        }
        out
    }

    /// The types the installed extensions make beyond the ones this server
    /// makes itself, `(resolution name, oid)`.
    pub(crate) fn extension_new_types(&self) -> Vec<(String, i64)> {
        let mut out = Vec::new();
        for d in self.catalog_docs(COLLECTION) {
            let schema = d.get_str("schema").unwrap_or("public").to_string();
            for t in d.get_array("types").map(|a| a.to_vec()).unwrap_or_default() {
                let Bson::Array(f) = t else { continue };
                if let (Some(Bson::String(n)), Some(o)) =
                    (f.first(), f.get(1).and_then(crate::bson_i64))
                {
                    out.push((Self::type_resolution(&schema, n), o));
                }
            }
        }
        out
    }

    /// Each installed extension's members, `(classid, oid)`, by name.
    pub(crate) fn extension_members(&self) -> Vec<(String, Vec<(i64, i64)>)> {
        self.catalog_docs(COLLECTION)
            .iter()
            .map(|d| {
                let members = d
                    .get_array("members")
                    .map(|a| {
                        a.iter()
                            .filter_map(|m| match m {
                                Bson::Array(p) => Some((
                                    crate::bson_i64(p.first()?)?,
                                    crate::bson_i64(p.get(1)?)?,
                                )),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                (d.get_str("_id").unwrap_or_default().to_string(), members)
            })
            .collect()
    }
}
