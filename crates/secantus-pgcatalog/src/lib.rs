//! The SQL catalog, as the Python server persists it.
//!
//! A declared table records its columns, types and primary key in a per-db
//! `__sql_catalog__` collection -- one document per table, keyed by table name.
//! A table maps 1:1 to a collection of the same name; a column maps to a
//! document *field*, and the single PRIMARY KEY column maps to `_id` so SQL PK
//! uniqueness rides the storage layer's `_id` index for free.
//!
//! **This format is a compatibility contract, not an implementation detail.**
//! The Python server, the Rust Mongo server and this one share one on-disk
//! store; a catalog document written subtly wrong here is read as truth by the
//! others. Every field the Python server emits is emitted here, in the same
//! order, including the ones that are always null today -- `golden.rs` pins the
//! exact document against a capture from the Python server.

use bson::{doc, Bson, Document};

pub const CATALOG_COLLECTION: &str = "__sql_catalog__";

/// Where a column's value lives inside the stored document.
///
/// The PK column is stored as `_id`; every other column as a field of its own
/// name. Kept as a method rather than a stored string so the two can never
/// disagree.
pub fn field_for(column: &str, pk: bool) -> String {
    if pk {
        "_id".to_string()
    } else {
        column.to_string()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    /// The PostgreSQL type name, e.g. `int4` / `text`.
    pub pg_type: String,
    pub pk: bool,
    pub nullable: bool,
}

impl Column {
    pub fn new(name: &str, pg_type: &str, pk: bool) -> Self {
        Self {
            name: name.to_string(),
            pg_type: pg_type.to_string(),
            pk,
            // A PRIMARY KEY column is NOT NULL by definition.
            nullable: !pk,
        }
    }

    pub fn field(&self) -> String {
        field_for(&self.name, self.pk)
    }

    /// The column sub-document, field-for-field as the Python server writes it.
    ///
    /// The many always-null members are deliberate: they are part of the shared
    /// on-disk shape, and omitting them would make a Python-side read see a
    /// column with missing keys rather than explicit nulls.
    pub fn to_document(&self) -> Document {
        doc! {
            "name": &self.name,
            "type": &self.pg_type,
            "field": self.field(),
            "pk": self.pk,
            "nullable": self.nullable,
            "has_default": false,
            "default": Bson::Null,
            "default_expr": Bson::Null,
            "comment": Bson::Null,
            "sequence": Bson::Null,
            "identity": Bson::Null,
            "enum_type": Bson::Null,
            "domain_type": Bson::Null,
            "generated": Bson::Null,
            "composite_type": Bson::Null,
            "composite_fields": Bson::Null,
            "json_plain": false,
            "decl_oid": Bson::Null,
            "typmod": -1i32,
        }
    }

    pub fn from_document(d: &Document) -> Option<Self> {
        Some(Self {
            name: d.get_str("name").ok()?.to_string(),
            pg_type: d.get_str("type").ok()?.to_string(),
            pk: d.get_bool("pk").unwrap_or(false),
            nullable: d.get_bool("nullable").unwrap_or(true),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableDef {
    pub name: String,
    pub columns: Vec<Column>,
}

impl TableDef {
    pub fn new(name: &str, columns: Vec<Column>) -> Self {
        Self {
            name: name.to_string(),
            columns,
        }
    }

    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// The stored field for a column name, for lowering a predicate: the PK
    /// becomes `_id`. Returns `None` for a column the table does not have, so
    /// the caller can raise PostgreSQL's 42703 rather than invent a field.
    pub fn field_of(&self, column: &str) -> Option<String> {
        self.column(column).map(|c| c.field())
    }

    pub fn to_document(&self) -> Document {
        doc! {
            "_id": &self.name,
            "table": &self.name,
            "collection": &self.name,
            "columns": self.columns.iter().map(|c| Bson::Document(c.to_document()))
                .collect::<Vec<_>>(),
            "comment": Bson::Null,
            "pk_name": Bson::Null,
            "pk_comment": Bson::Null,
            "pk_column_order": Bson::Null,
            "temp": false,
            "foreign_keys": Vec::<Bson>::new(),
            "check_constraints": Vec::<Bson>::new(),
            "unique_constraints": Vec::<Bson>::new(),
            "expr_indexes": Vec::<Bson>::new(),
        }
    }

    pub fn from_document(d: &Document) -> Option<Self> {
        let cols = d.get_array("columns").ok()?;
        Some(Self {
            name: d.get_str("table").ok()?.to_string(),
            columns: cols
                .iter()
                .filter_map(|b| b.as_document())
                .filter_map(Column::from_document)
                .collect(),
        })
    }
}

#[cfg(test)]
mod golden;
