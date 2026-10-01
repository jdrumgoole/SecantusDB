//! Expression indexes kept in storage.
//!
//! An expression index (`CREATE UNIQUE INDEX ON t (lower(email))`) is a
//! storage index on a hidden field, `__sqlexpr_<name>`, partial on that field
//! existing. Every row the server writes carries the field: the expression's
//! value (a document `{"0": .., "1": ..}` for several expressions), or no
//! field at all when a value is NULL or the index's WHERE does not hold -- so
//! NULLs stay distinct, as PostgreSQL's are, and the storage index enforces
//! UNIQUE.
//!
//! Before this the field was never written: the index was empty and UNIQUE
//! was checked by re-evaluating the expression over EVERY stored row on every
//! write -- a single-row INSERT took 1.6 s at 20,000 rows.
//!
//! Rows another writer left (the Python server knows no expression indexes,
//! and a MongoDB client can write the collection) carry no field, or a stale
//! one. The store is held by one process at a time, so the fields are
//! recomputed once when the server opens it (`refresh_all_expression_indexes`),
//! before any client connects.

use bson::{Bson, Document};
use pgwire::error::PgWireResult;
use secantus_pgcatalog::TableDef;

use crate::{decode_doc, encode_doc, PgHandler};

thread_local! {
    /// Each hidden field's `(index name, key SQL)`, recorded as the indexes
    /// are planned, for the 23505 a collision on it is reported as.
    static FIELDS: std::cell::RefCell<std::collections::HashMap<String, (String, Vec<String>)>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The expression index a hidden field belongs to: `(name, key SQL)`.
pub(crate) fn field_index(field: &str) -> Option<(String, Vec<String>)> {
    FIELDS.with(|f| f.borrow().get(field).cloned())
}

/// One expression index, its expressions planned once.
pub(crate) struct ExprIndex {
    pub field: String,
    exprs: Vec<secantus_pgplan::ColumnExpr>,
    predicate: Option<secantus_pgplan::ColumnExpr>,
}

impl ExprIndex {
    /// The value this row's index field takes; `None` for no field.
    fn value(&self, row: &Document) -> PgWireResult<Option<Bson>> {
        if let Some(p) = &self.predicate {
            if secantus_pgplan::apply_row_expr(p, row).map_err(|e| PgHandler::err(&e))?
                != Bson::Boolean(true)
            {
                return Ok(None);
            }
        }
        let mut values = Vec::with_capacity(self.exprs.len());
        for e in &self.exprs {
            let v = secantus_pgplan::apply_row_expr(e, row).map_err(|e| PgHandler::err(&e))?;
            if v == Bson::Null {
                return Ok(None);
            }
            values.push(v);
        }
        Ok(Some(match values.len() {
            1 => values.remove(0),
            _ => Bson::Document(
                values
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| (i.to_string(), v))
                    .collect(),
            ),
        }))
    }
}

impl PgHandler {
    /// `table`'s expression indexes, planned.
    pub(crate) fn expr_indexes(&self, def: &TableDef) -> PgWireResult<Vec<ExprIndex>> {
        let indexes = self
            .storage
            .list_indexes(self.db(), &def.name)
            .map_err(|e| Self::storage_err("could not list the indexes", e))?;
        let mut out = Vec::new();
        for ix in indexes {
            let Some((exprs, keys)) = Self::index_expressions(&ix) else {
                continue;
            };
            let name = ix.get_str("name").unwrap_or_default().to_string();
            FIELDS.with(|f| {
                f.borrow_mut().insert(
                    Self::expression_index_field(&name),
                    (name.clone(), keys.clone()),
                )
            });
            let planned = exprs
                .iter()
                .map(|e| secantus_pgplan::plan_check_expression(e, def).map_err(|e| Self::err(&e)))
                .collect::<PgWireResult<Vec<_>>>()?;
            let predicate = ix
                .get_str("sqlPredicate")
                .ok()
                .or_else(|| {
                    ix.get_document("options")
                        .ok()
                        .and_then(|o| o.get_str("sqlPredicate").ok())
                })
                .map(|p| secantus_pgplan::plan_check_expression(p, def).map_err(|e| Self::err(&e)))
                .transpose()?;
            out.push(ExprIndex {
                field: Self::expression_index_field(&name),
                exprs: planned,
                predicate,
            });
        }
        Ok(out)
    }

    /// Set (or clear) every expression index's field on `row`.
    pub(crate) fn fill_expr_fields(indexes: &[ExprIndex], row: &mut Document) -> PgWireResult<()> {
        for ix in indexes {
            match ix.value(row)? {
                Some(v) => {
                    row.insert(ix.field.clone(), v);
                }
                None => {
                    row.remove(&ix.field);
                }
            }
        }
        Ok(())
    }

    /// Encoded rows about to be written to `table`, with their expression
    /// index fields set. A table with none passes them through untouched.
    pub(crate) fn with_expr_fields(
        &self,
        table: &str,
        rows: Vec<Vec<u8>>,
    ) -> PgWireResult<Vec<Vec<u8>>> {
        let Some(def) = self.lookup(table) else {
            return Ok(rows);
        };
        let indexes = self.expr_indexes(&def)?;
        if indexes.is_empty() {
            return Ok(rows);
        }
        rows.into_iter()
            .map(|raw| {
                let mut d =
                    decode_doc(&raw).map_err(|e| Self::storage_err("could not read a row", e))?;
                Self::fill_expr_fields(&indexes, &mut d)?;
                encode_doc(&d).map_err(|e| Self::storage_err("could not encode a row", e))
            })
            .collect()
    }

    /// Recompute the expression index fields of `table`'s rows matching
    /// `filter` (all of them for an empty one), writing only the rows whose
    /// fields change. A duplicate is the storage index's 23505.
    pub(crate) fn refresh_expr_fields(&self, table: &str, filter: &Document) -> PgWireResult<()> {
        let Some(def) = self.lookup(table) else {
            return Ok(());
        };
        let indexes = self.expr_indexes(&def)?;
        if indexes.is_empty() {
            return Ok(());
        }
        let rows = self
            .storage
            .find_matching(self.db(), table, filter)
            .map_err(|e| Self::storage_err("could not read", e))?;
        for raw in rows {
            let old = decode_doc(&raw).map_err(|e| Self::storage_err("could not read a row", e))?;
            let mut new = old.clone();
            Self::fill_expr_fields(&indexes, &mut new)?;
            let mut set = Document::new();
            let mut unset = Vec::new();
            for ix in &indexes {
                match (old.get(&ix.field), new.get(&ix.field)) {
                    (a, Some(b)) if a != Some(b) => {
                        set.insert(ix.field.clone(), b.clone());
                    }
                    (Some(_), None) => unset.push(ix.field.clone()),
                    _ => {}
                }
            }
            if set.is_empty() && unset.is_empty() {
                continue;
            }
            let Some(id) = old.get("_id") else {
                continue;
            };
            self.update_rows_raw(table, &bson::doc! { "_id": id.clone() }, &set, &unset)?;
        }
        Ok(())
    }

    /// Recompute every expression index's fields in this database: rows
    /// another writer left carry none, or a stale one. A row whose value
    /// collides (written by a server that does not enforce the index) keeps
    /// no field and is reported, since the data already violates it.
    pub(crate) fn refresh_all_expression_indexes(&self) -> PgWireResult<()> {
        for def in self.all_table_defs()? {
            let indexes = self.expr_indexes(&def)?;
            if indexes.is_empty() {
                continue;
            }
            if let Err(e) = self.refresh_expr_fields(&def.name, &Document::new()) {
                eprintln!(
                    "secantusd-pg: could not rebuild the expression indexes of \"{}\": {e}",
                    def.name
                );
            }
        }
        Ok(())
    }

    /// Remove a dropped expression index's field from every row.
    pub(crate) fn clear_expr_field(&self, table: &str, field: &str) -> PgWireResult<()> {
        self.update_rows_raw(
            table,
            &bson::doc! { field: { "$exists": true } },
            &Document::new(),
            &[field.to_string()],
        )
        .map(|_| ())
    }
}
