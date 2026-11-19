//! Expression indexes kept in storage.
//!
//! An expression index (`CREATE UNIQUE INDEX ON t (lower(email))`) is a
//! storage index on a hidden field, `__sqlexpr_<name>`, partial on that field
//! existing. Every row the server writes carries the field: the expression's
//! value (a document `{"0": .., "1": ..}` for several expressions), or no
//! field at all when a value is NULL or the index's WHERE does not hold -- so
//! NULLs stay distinct, as PostgreSQL's are, and the storage index enforces
//! UNIQUE. A `NULLS NOT DISTINCT` index keeps a NULL value as its key.
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

/// Whether an index key `sql` (planned as `expr`) is a `bpchar`: a bare
/// `bpchar` column, or an expression of that type. Such a key compares by
/// `bpcharcmp`, its trailing blanks ignored (RP-693).
pub(crate) fn is_bpchar_key(def: &TableDef, sql: &str, expr: &secantus_pgplan::ColumnExpr) -> bool {
    let bare = sql
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .map_or_else(|| sql.to_string(), |r| r.replace("\"\"", "\""));
    def.column(&bare).map_or_else(
        || secantus_pgplan::column_expr_type(expr) == "bpchar",
        |c| c.pg_type == "bpchar",
    )
}

/// One expression index, its expressions planned once.
pub(crate) struct ExprIndex {
    pub field: String,
    exprs: Vec<secantus_pgplan::ColumnExpr>,
    /// Per expression, whether its key is a `bpchar`: keyed with its
    /// trailing blanks trimmed (`bcTruelen`), as `bpcharcmp` compares
    /// (RP-693).
    bpchar: Vec<bool>,
    predicate: Option<secantus_pgplan::ColumnExpr>,
    /// `NULLS NOT DISTINCT`: a NULL value is a key like any other.
    nulls_not_distinct: bool,
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
        for (e, bpchar) in self.exprs.iter().zip(&self.bpchar) {
            let v = secantus_pgplan::apply_row_expr(e, row).map_err(|e| PgHandler::err(&e))?;
            let v = match v {
                Bson::String(s) if *bpchar => Bson::String(s.trim_end_matches(' ').to_string()),
                v => v,
            };
            if v == Bson::Null && !self.nulls_not_distinct {
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
            .list_indexes_shared(self.db(), &def.name)
            .map_err(|e| Self::storage_err("could not list the indexes", e))?;
        let mut out = Vec::new();
        for ix in indexes.iter() {
            let Some((exprs, keys)) = Self::index_expressions(ix) else {
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
            let bpchar = exprs
                .iter()
                .zip(&planned)
                .map(|(sql, e)| is_bpchar_key(def, sql, e))
                .collect();
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
                bpchar,
                predicate,
                nulls_not_distinct: crate::nulls_not_distinct(ix),
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
        let Some(def) = self.lookup_ref(table) else {
            return Ok(rows);
        };
        let indexes = self.expr_indexes(&def)?;
        let keyed = def.has_bpchar_key();
        if indexes.is_empty() && !keyed {
            return Ok(rows);
        }
        rows.into_iter()
            .map(|raw| {
                let mut d =
                    decode_doc(&raw).map_err(|e| Self::storage_err("could not read a row", e))?;
                // A `bpchar` key is trimmed however the row was built
                // (COPY included; RP-693).
                def.derive_bpchar_keys(&mut d);
                Self::fill_expr_fields(&indexes, &mut d)?;
                encode_doc(&d).map_err(|e| Self::storage_err("could not encode a row", e))
            })
            .collect()
    }

    /// [`Self::with_expr_fields`] over rows not yet encoded, giving each
    /// stored document with its encoding: what `with_expr_fields` writes,
    /// as storage would decode it, so the insert need not decode it again
    /// (`Storage::insert_documents`, RP-014).
    pub(crate) fn stored_rows(
        &self,
        table: &str,
        rows: &[Document],
    ) -> PgWireResult<Vec<(Vec<u8>, Document)>> {
        self.stored_rows_of(table, rows.iter().map(std::borrow::Cow::Borrowed))
    }

    /// [`Self::stored_rows`] of rows the caller no longer needs: each is
    /// moved into its stored form rather than copied (RP-014).
    pub(crate) fn stored_rows_owned(
        &self,
        table: &str,
        rows: Vec<Document>,
    ) -> PgWireResult<Vec<(Vec<u8>, Document)>> {
        self.stored_rows_of(table, rows.into_iter().map(std::borrow::Cow::Owned))
    }

    fn stored_rows_of<'a>(
        &self,
        table: &str,
        rows: impl Iterator<Item = std::borrow::Cow<'a, Document>>,
    ) -> PgWireResult<Vec<(Vec<u8>, Document)>> {
        let def = self.lookup_ref(table);
        let indexes = match &def {
            Some(def) => self.expr_indexes(def)?,
            None => Vec::new(),
        };
        let keyed = def.as_ref().filter(|d| d.has_bpchar_key());
        rows.map(|row| {
            // With expression indexes, the row as `with_expr_fields`
            // reads it back from its encoding, and fills.
            let filled = if indexes.is_empty() {
                keyed.map(|def| {
                    let mut d = row.clone().into_owned();
                    def.derive_bpchar_keys(&mut d);
                    d
                })
            } else {
                let raw = crate::encode_doc(&row)
                    .map_err(|e| Self::storage_err("could not encode a row", e))?;
                let mut d =
                    decode_doc(&raw).map_err(|e| Self::storage_err("could not read a row", e))?;
                if let Some(def) = keyed {
                    def.derive_bpchar_keys(&mut d);
                }
                Self::fill_expr_fields(&indexes, &mut d)?;
                Some(d)
            };
            let stored = match filled {
                Some(d) => crate::stored_form(&d),
                None => crate::stored_form_owned(row),
            };
            let bytes = bson::to_vec(&stored)
                .map_err(|e| Self::storage_err("could not encode a row", e))?;
            Ok((bytes, stored))
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
            self.update_rows_raw(
                table,
                &bson::doc! { "_id": { "$eq": id.clone() } },
                &set,
                &unset,
            )?;
        }
        Ok(())
    }

    /// Recompute every expression index's fields in this database: rows
    /// another writer left carry none, or a stale one. A row whose value
    /// collides (written by a server that does not enforce the index) keeps
    /// no field and is reported, since the data already violates it.
    pub(crate) fn refresh_all_expression_indexes(&self) -> PgWireResult<()> {
        for def in self.shared_table_defs()?.iter() {
            let indexes = self.expr_indexes(def)?;
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
