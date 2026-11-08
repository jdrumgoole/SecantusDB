//! What depends on a relation's row type, for DROP TABLE, DROP VIEW and
//! DROP MATERIALIZED VIEW (RP-546).
//!
//! A relation's row type and that type's array are types like any other, and
//! PostgreSQL records what names them in `pg_depend`: `ProcedureCreate` a
//! row from a routine to every type its signature names (each parameter's,
//! OUT and TABLE columns included, and the result's), `CREATE CAST` one from
//! the cast to its source and target, and from the cast to the function
//! implementing it. So a plain DROP TABLE is 2BP01 while one exists, its
//! DETAIL naming each (`function f(t) depends on type t`), and DROP TABLE
//! ... CASCADE drops them with the table.
//!
//! The DETAIL follows `pg_depend`'s order. `heap_create_with_catalog` makes
//! the row type and then its array with the table, so the array is the row
//! type's first dependant and the row type the table's: what names the
//! array comes first, then what names the row type, each in creation order,
//! and all of them ahead of the table's other dependants (the inheritance
//! children, the foreign keys referencing it, its views), each made after
//! the table (measured on PostgreSQL 15.19).
//!
//! A routine's creation order is its oid, drawn from the oid counter as
//! `ProcedureCreate` draws it. A cast's oid is derived from its types, not
//! drawn, so the casts follow the routines rather than falling between
//! them where PostgreSQL's creation order puts them.

use super::*;

thread_local! {
    /// An internal drop of a relation whose row type outlives it, or whose
    /// row type's dependants its statement has already settled.
    static KEEP_ROW_TYPE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f`, an internal DROP TABLE that leaves what names the relation's
/// row type alone: a table becoming a view (`CREATE RULE "_RETURN"`) keeps
/// its row type, and DROP MATERIALIZED VIEW checks its own before it
/// removes anything, under its own name.
pub(crate) fn keeping_row_type<T>(f: impl FnOnce() -> T) -> T {
    let before = KEEP_ROW_TYPE.with(|k| k.replace(true));
    let out = f();
    KEEP_ROW_TYPE.with(|k| k.set(before));
    out
}

/// The routines, casts and transforms over a table's row type or its
/// array, as DROP TABLE reports and drops them.
#[derive(Default)]
pub(crate) struct RowTypeDependents {
    /// `(object, what it depends on)`, as the 2BP01 DETAIL names each.
    pub(crate) lines: Vec<(String, String)>,
    /// The routines' catalog keys.
    functions: Vec<String>,
    /// The casts' catalog keys.
    casts: Vec<String>,
    /// The transforms' catalog keys.
    transforms: Vec<String>,
}

impl RowTypeDependents {
    /// Take in `more`, what names another relation's row type that this
    /// statement drops, so a CASCADE drops it too, and answer its DETAIL
    /// lines (RP-568). An object named twice is left for [`last_reached`]
    /// to settle.
    pub(crate) fn absorb(&mut self, more: RowTypeDependents) -> Vec<(String, String)> {
        for (mine, theirs) in [
            (&mut self.functions, more.functions),
            (&mut self.casts, more.casts),
            (&mut self.transforms, more.transforms),
        ] {
            for key in theirs {
                if !mine.contains(&key) {
                    mine.push(key);
                }
            }
        }
        more.lines
    }
}

/// A DROP's DETAIL pairs `(object, what it depends on)` with each object
/// named once, at its LAST place (RP-568). `findDependentObjects` visits a
/// relation's dependants newest first and records each object the first
/// time it reaches it, and `reportDependentObjects` reads that record
/// backwards: so the lines come out oldest first, and an object two of
/// them reach (`f(v1, v2)`, `f(t) returns v`) under the newer one -- as
/// measured on PostgreSQL 15.19 (`function f(v1,v2) depends on type v2`).
pub(crate) fn last_reached(lines: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        if !lines[i + 1..].iter().any(|(o, _)| *o == line.0) {
            out.push(line.clone());
        }
    }
    out
}

/// `getObjectDescription` of a routine: `function`, a procedure's too, then
/// `format_procedure`'s signature -- the name, qualified when its schema is
/// off the search path, and each input argument's `format_type_be`, joined
/// by a bare comma (`function f(integer,s.t)`).
pub(crate) fn routine_description(d: &Document) -> String {
    let q = secantus_pgplan::scalar::quote_identifier;
    let name = d.get_str("name").unwrap_or_default();
    let schema = d.get_str("schema").unwrap_or("public");
    let shown = if schema.starts_with("pg_temp")
        || secantus_pgplan::schemas::search_path()
            .iter()
            .any(|s| s == schema)
    {
        q(name)
    } else {
        format!("{}.{}", q(schema), q(name))
    };
    let args: Vec<String> = d
        .get_array("param_types")
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|t| {
                    secantus_pgplan::user_casts::type_oid(t)
                        .map(secantus_pgplan::regtype_text)
                        .unwrap_or_else(|| t.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    format!("function {shown}({})", args.join(","))
}

/// Every type a stored routine's signature names: each parameter's (OUT,
/// INOUT and TABLE ones included, as `proallargtypes` holds them), each
/// RETURNS TABLE column's, and the result's.
fn signature_types(d: &Document) -> Vec<String> {
    let strings = |key: &str| -> Vec<String> {
        d.get_array(key)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut types = strings("all_param_types");
    types.extend(strings("param_types"));
    if let Ok(cols) = d.get_array("table_columns") {
        types.extend(
            cols.iter()
                .filter_map(|c| c.as_document())
                .filter_map(|c| c.get_str("type_tag").ok())
                .map(str::to_string),
        );
    }
    if let Ok(ret) = d.get_str("return_tag") {
        types.push(ret.to_string());
    }
    types
}

impl PgHandler {
    /// What names relation `table`'s row type or its array type, in
    /// PostgreSQL's order (see the module comment): a table's, a view's or
    /// a materialized view's.
    pub(crate) fn row_type_dependents(&self, table: &str) -> PgWireResult<RowTypeDependents> {
        if KEEP_ROW_TYPE.with(|k| k.get()) {
            return Ok(RowTypeDependents::default());
        }
        let array = format!("{table}[]");
        let row_oid = secantus_pgplan::user_casts::type_oid(table);
        let array_oid = secantus_pgplan::user_casts::type_oid(&array);
        // `format_type_be`: qualified when its schema is off the path.
        let type_text = |key: &str, oid: Option<i64>| -> String {
            format!(
                "type {}",
                oid.map(secantus_pgplan::regtype_text)
                    .unwrap_or_else(|| key.to_string())
            )
        };
        let (row_text, array_text) = (type_text(table, row_oid), type_text(&array, array_oid));

        // `(oid, key, description, signature types)` of every routine, in
        // creation order.
        let mut routines: Vec<(i64, String, String, Vec<String>)> = self
            .type_catalog_docs(Self::FUNCTION_COLLECTION)?
            .iter()
            .filter_map(|d| {
                let key = d.get_str("_id").ok()?.to_string();
                let oid = d
                    .get("oid")
                    .and_then(bson_i64)
                    .unwrap_or_else(|| self.function_oid(&key));
                Some((oid, key, routine_description(d), signature_types(d)))
            })
            .collect();
        routines.sort();
        // A signature type names `on` by its catalog key, or by its oid when
        // it was written another way (`public.t`).
        let names = |t: &str, on: &str, oid: Option<i64>| {
            t == on || oid.is_some_and(|o| secantus_pgplan::user_casts::type_oid(t) == Some(o))
        };

        let mut out = RowTypeDependents::default();
        let functions = self.functions()?;
        let mut implementing: Vec<(String, String, String)> = Vec::new();
        let groups = [
            (array.as_str(), array_oid, &array_text),
            (table, row_oid, &row_text),
        ];
        for (on, on_oid, on_text) in groups {
            for (_, key, desc, types) in &routines {
                if out.functions.contains(key) || !types.iter().any(|t| names(t, on, on_oid)) {
                    continue;
                }
                out.lines.push((desc.clone(), on_text.clone()));
                out.functions.push(key.clone());
                if let Some(f) = functions.iter().find(|f| f.key == *key) {
                    for (cast, cast_desc) in self.casts_on_function(&f.name, &f.param_types) {
                        implementing.push((cast, cast_desc, desc.clone()));
                    }
                }
            }
        }
        for (oid, on_text) in [(array_oid, &array_text), (row_oid, &row_text)] {
            let Some(oid) = oid else { continue };
            for (key, desc) in self.casts_on_type(oid) {
                if !out.casts.contains(&key) {
                    out.lines.push((desc, on_text.clone()));
                    out.casts.push(key);
                }
            }
        }
        for (key, desc, function) in implementing {
            if !out.casts.contains(&key) {
                out.lines.push((desc, function));
                out.casts.push(key);
            }
        }
        for (oid, on_text) in [(array_oid, &array_text), (row_oid, &row_text)] {
            let Some(oid) = oid else { continue };
            for (key, desc) in self.transforms_on_type(oid) {
                if !out.transforms.contains(&key) {
                    out.lines.push((desc, on_text.clone()));
                    out.transforms.push(key);
                }
            }
        }
        Ok(out)
    }

    /// DROP TABLE ... CASCADE: drop what [`Self::row_type_dependents`]
    /// found, each cast and transform before the routine it may call.
    pub(crate) fn drop_row_type_dependents(&self, deps: &RowTypeDependents) -> PgWireResult<()> {
        for key in &deps.transforms {
            self.drop_transform_key(key)?;
        }
        for key in &deps.casts {
            self.drop_cast_key(key)?;
        }
        for key in &deps.functions {
            self.delete_type_doc(Self::FUNCTION_COLLECTION, key)?;
            // As DROP FUNCTION: one recreated under the name starts with
            // the default ACL.
            self.drop_object_acl("function", key)?;
        }
        Ok(())
    }
}
