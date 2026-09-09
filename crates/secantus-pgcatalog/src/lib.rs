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
/// Where sequence state lives: one document per sequence, keyed by name, in
/// the shape the Python server writes (`last_value` / `is_called` are the two
/// `nextval` reads and moves).
pub const SEQUENCE_COLLECTION: &str = "__sql_sequences__";

/// The sequence document for a fresh `serial` column, owned by `owned_by`
/// (`table.column`, so dropping the table drops it). `max_value` is the
/// column type's ceiling, as PostgreSQL sizes a serial's sequence.
pub fn sequence_document(name: &str, owned_by: &str, max_value: i64) -> Document {
    doc! {
        "_id": name,
        "sequence": name,
        "last_value": 1i64,
        "start": 1i64,
        "increment": 1i64,
        "min_value": 1i64,
        "max_value": max_value,
        "cycle": false,
        "is_called": false,
        "owned_by": owned_by,
    }
}

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
    /// The sequence a `serial` column draws its default from, by name.
    pub sequence: Option<String>,
    /// A LITERAL column DEFAULT, already cast to the column's type, applied
    /// when an INSERT omits the column. `Some(Bson::Null)` is an explicit
    /// `DEFAULT NULL`; `None` is no default at all.
    pub default: Option<Bson>,
}

impl Column {
    pub fn new(name: &str, pg_type: &str, pk: bool) -> Self {
        Self {
            name: name.to_string(),
            pg_type: pg_type.to_string(),
            pk,
            // A PRIMARY KEY column is NOT NULL by definition.
            nullable: !pk,
            sequence: None,
            default: None,
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
            "has_default": self.default.is_some(),
            "default": self.default.clone().unwrap_or(Bson::Null),
            "default_expr": Bson::Null,
            "comment": Bson::Null,
            "sequence": self.sequence.as_deref().map_or(Bson::Null, Bson::from),
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
            sequence: d.get_str("sequence").ok().map(str::to_string),
            default: d
                .get_bool("has_default")
                .unwrap_or(false)
                .then(|| d.get("default").cloned().unwrap_or(Bson::Null)),
        })
    }
}

/// A declared CHECK constraint. `expression` is the SQL text of the
/// predicate (PostgreSQL's deparse of what the user wrote, e.g. `(a > 0)`);
/// the server re-plans it against the table's columns on every write.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckConstraint {
    pub name: String,
    pub expression: String,
}

/// A declared FOREIGN KEY constraint, in the Python server's on-disk shape.
/// `on_delete` / `on_update` are the referential action keywords as PostgreSQL
/// spells them (`NO ACTION`, `RESTRICT`, `CASCADE`, `SET NULL`, `SET DEFAULT`);
/// `None` is the default (`NO ACTION`).
#[derive(Debug, Clone, PartialEq)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: Option<String>,
    pub on_update: Option<String>,
    pub deferrable: bool,
    pub initially_deferred: bool,
}

impl CheckConstraint {
    pub fn to_document(&self) -> Document {
        doc! {
            "name": &self.name,
            "expression": &self.expression,
            "comment": Bson::Null,
        }
    }

    pub fn from_document(d: &Document) -> Option<Self> {
        Some(Self {
            name: d.get_str("name").ok()?.to_string(),
            expression: d.get_str("expression").ok()?.to_string(),
        })
    }
}

impl ForeignKey {
    pub fn to_document(&self) -> Document {
        doc! {
            "name": &self.name,
            "columns": self.columns.clone(),
            "ref_table": &self.ref_table,
            "ref_columns": self.ref_columns.clone(),
            "on_delete": self.on_delete.as_deref().map_or(Bson::Null, Bson::from),
            "on_update": self.on_update.as_deref().map_or(Bson::Null, Bson::from),
            "deferrable": self.deferrable,
            "initially_deferred": self.initially_deferred,
            "comment": Bson::Null,
        }
    }

    pub fn from_document(d: &Document) -> Option<Self> {
        let strings = |key: &str| -> Option<Vec<String>> {
            Some(
                d.get_array(key)
                    .ok()?
                    .iter()
                    .filter_map(|b| b.as_str().map(str::to_string))
                    .collect(),
            )
        };
        Some(Self {
            name: d.get_str("name").ok()?.to_string(),
            columns: strings("columns")?,
            ref_table: d.get_str("ref_table").ok()?.to_string(),
            ref_columns: strings("ref_columns")?,
            on_delete: d.get_str("on_delete").ok().map(str::to_string),
            on_update: d.get_str("on_update").ok().map(str::to_string),
            deferrable: d.get_bool("deferrable").unwrap_or(false),
            initially_deferred: d.get_bool("initially_deferred").unwrap_or(false),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableDef {
    pub name: String,
    pub columns: Vec<Column>,
    /// `CREATE TEMP TABLE`: reflected as living in a `pg_temp_N` schema.
    pub temp: bool,
    pub check_constraints: Vec<CheckConstraint>,
    pub foreign_keys: Vec<ForeignKey>,
}

impl TableDef {
    pub fn new(name: &str, columns: Vec<Column>) -> Self {
        Self {
            name: name.to_string(),
            columns,
            temp: false,
            check_constraints: Vec::new(),
            foreign_keys: Vec::new(),
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
            "temp": self.temp,
            "foreign_keys": self.foreign_keys.iter().map(|f| Bson::Document(f.to_document()))
                .collect::<Vec<_>>(),
            "check_constraints": self.check_constraints.iter()
                .map(|c| Bson::Document(c.to_document()))
                .collect::<Vec<_>>(),
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
            temp: d.get_bool("temp").unwrap_or(false),
            check_constraints: d
                .get_array("check_constraints")
                .map(|a| {
                    a.iter()
                        .filter_map(|b| b.as_document())
                        .filter_map(CheckConstraint::from_document)
                        .collect()
                })
                .unwrap_or_default(),
            foreign_keys: d
                .get_array("foreign_keys")
                .map(|a| {
                    a.iter()
                        .filter_map(|b| b.as_document())
                        .filter_map(ForeignKey::from_document)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod golden;
