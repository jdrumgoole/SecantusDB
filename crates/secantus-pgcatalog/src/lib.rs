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
    /// A stored field other than the one the name implies: a COMPOSITE
    /// primary key's columns live at `_id.<name>` inside a subdocument `_id`,
    /// which is how the Python server lays one out too. `None` for everything
    /// else.
    pub field_override: Option<String>,
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
    /// Where this column's values are READ from, for the RowDescription's
    /// `ftable` / `ftablecol`: `(relation oid, 1-based attnum)` for a column
    /// that passes a base table's column straight through, `None` for a
    /// computed one. Never stored: the wire layer stamps it when it reads
    /// the catalog, and a projection's output columns inherit it.
    pub source: Option<(i64, i16)>,
    /// `GENERATED ... AS IDENTITY`: `"always"` (a user-supplied value is
    /// rejected) or `"by_default"` (like a serial). The Python server's own
    /// spelling, because the two share this catalog.
    pub identity: Option<String>,
    /// Every catalog key this model does NOT own, kept verbatim.
    ///
    /// The Python server records more per column than this one models --
    /// `enum_type`, `domain_type`, `generated`, `comment`, `default_expr`,
    /// `composite_type` -- and they were written back as unconditional NULLs,
    /// so any Rust rewrite of a catalog row silently erased them. That was
    /// unreachable while nothing here rewrote an existing row; `ALTER TABLE`
    /// made it reachable, and a column that quietly stopped being an enum or
    /// a generated column is exactly the silent divergence this catalog is
    /// shared to avoid.
    pub extra: Document,
    /// `atttypmod`: a declared width or precision, plus the varlena header
    /// for the string types -- `char(4)` is 8 -- or -1 for none.
    ///
    /// Stored, and shared with the Python server, which writes a `char(n)`
    /// column as `type: "text"` with `decl_oid: 1042` and this modifier. The
    /// Rust side modelled neither field, so it read such a column as plain
    /// `text` (oid 25) with no width -- and wrote `type: "bpchar"`, which the
    /// Python side reads as text in turn. The VALUES always survived; the
    /// declared type did not, in either direction.
    pub typmod: i32,
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
            source: None,
            identity: None,
            extra: Document::new(),
            field_override: None,
            typmod: -1,
        }
    }

    /// The catalog keys this model owns, and therefore writes itself. Anything
    /// else a document carries is kept in `extra` and written back unchanged.
    /// What `to_document` writes for the keys this model does not own.
    ///
    /// `extra` keeps only what DIFFERS from these, so a column built here
    /// round-trips to ITSELF (the nulls carry no information and would
    /// otherwise make `def -> doc -> def` inequal), while a real value the
    /// other server wrote is still preserved.
    fn unmodelled_defaults() -> Document {
        doc! {
            "default_expr": Bson::Null,
            "comment": Bson::Null,
            "enum_type": Bson::Null,
            "domain_type": Bson::Null,
            "generated": Bson::Null,
            "composite_type": Bson::Null,
            "composite_fields": Bson::Null,
            "json_plain": false,
        }
    }

    const OWNED_KEYS: &'static [&'static str] = &[
        "name",
        "type",
        "field",
        "pk",
        "nullable",
        "has_default",
        "default",
        "sequence",
        "identity",
        "decl_oid",
        "typmod",
    ];

    /// The `(type, decl_oid)` pair the catalog stores for this column.
    ///
    /// The Python server records a declared string type as `text` plus the
    /// oid it was declared with, so that is what both servers write.
    fn stored_type(&self) -> (&str, Bson) {
        match self.pg_type.as_str() {
            "bpchar" => ("text", Bson::Int32(1042)),
            "varchar" => ("text", Bson::Int32(1043)),
            other => (other, Bson::Null),
        }
    }

    /// The internal type name for a stored `(type, decl_oid)` pair.
    fn type_from_stored(pg_type: &str, decl_oid: Option<i32>) -> String {
        match decl_oid {
            Some(1042) => "bpchar".to_string(),
            Some(1043) => "varchar".to_string(),
            _ => pg_type.to_string(),
        }
    }

    pub fn field(&self) -> String {
        match &self.field_override {
            Some(f) => f.clone(),
            None => field_for(&self.name, self.pk),
        }
    }

    /// The column sub-document, field-for-field as the Python server writes it.
    ///
    /// The many always-null members are deliberate: they are part of the shared
    /// on-disk shape, and omitting them would make a Python-side read see a
    /// column with missing keys rather than explicit nulls.
    pub fn to_document(&self) -> Document {
        let (stored_type, decl_oid) = self.stored_type();
        let mut out = doc! {
            "name": &self.name,
            "type": stored_type,
            "field": self.field(),
            "pk": self.pk,
            "nullable": self.nullable,
            "has_default": self.default.is_some(),
            "default": self.default.clone().unwrap_or(Bson::Null),
            "default_expr": Bson::Null,
            "comment": Bson::Null,
            "sequence": self.sequence.as_deref().map_or(Bson::Null, Bson::from),
            "identity": self.identity.as_deref().map_or(Bson::Null, Bson::from),
            "enum_type": Bson::Null,
            "domain_type": Bson::Null,
            "generated": Bson::Null,
            "composite_type": Bson::Null,
            "composite_fields": Bson::Null,
            "json_plain": false,
            "decl_oid": decl_oid,
            "typmod": self.typmod,
        };
        // Anything the other server wrote that this one does not model goes
        // back exactly as it came, OVER the nulls above.
        for (k, v) in &self.extra {
            out.insert(k.clone(), v.clone());
        }
        out
    }

    /// An EXPRESSION default (`now()`, `gen_random_uuid()`), as SQL text,
    /// evaluated for each row an INSERT leaves the column out of. Stored under
    /// the Python server's `default_expr` key, beside `has_default: false`.
    pub fn default_expr(&self) -> Option<&str> {
        self.extra.get_str("default_expr").ok()
    }

    /// Set or clear the expression default. A literal default and an
    /// expression one are exclusive.
    pub fn set_default_expr(&mut self, expr: Option<String>) {
        match expr {
            Some(e) => {
                self.default = None;
                self.extra.insert("default_expr", e);
            }
            None => {
                self.extra.remove("default_expr");
            }
        }
    }

    /// How PostgreSQL's catalog prints a FOLDED constant default that was
    /// written as an expression (`DEFAULT 1 + 2` stores 3, prints `(1 + 2)`).
    /// Display only; the value is `default`.
    pub fn default_sql(&self) -> Option<&str> {
        self.extra.get_str("default_sql").ok()
    }

    pub fn set_default_sql(&mut self, sql: Option<String>) {
        match sql {
            Some(s) => {
                self.extra.insert("default_sql", s);
            }
            None => {
                self.extra.remove("default_sql");
            }
        }
    }

    pub fn from_document(d: &Document) -> Option<Self> {
        let decl_oid = d.get_i32("decl_oid").ok();
        Some(Self {
            name: d.get_str("name").ok()?.to_string(),
            field_override: d
                .get_str("field")
                .ok()
                .filter(|f| f.starts_with("_id."))
                .map(str::to_string),
            pg_type: Self::type_from_stored(d.get_str("type").ok()?, decl_oid),
            pk: d.get_bool("pk").unwrap_or(false),
            nullable: d.get_bool("nullable").unwrap_or(true),
            sequence: d.get_str("sequence").ok().map(str::to_string),
            identity: d.get_str("identity").ok().map(str::to_string),
            // Everything this model does not own, kept so a rewrite puts it
            // back rather than erasing it.
            extra: {
                let defaults = Self::unmodelled_defaults();
                d.iter()
                    .filter(|(k, v)| {
                        !Self::OWNED_KEYS.contains(&k.as_str())
                            && defaults.get(k.as_str()) != Some(v)
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            },
            default: d
                .get_bool("has_default")
                .unwrap_or(false)
                .then(|| d.get("default").cloned().unwrap_or(Bson::Null)),
            source: None,
            typmod: d.get_i32("typmod").unwrap_or(-1),
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
    /// `COMMENT ON CONSTRAINT`, shared with the Python server.
    pub comment: Option<String>,
    /// Added `NOT VALID`: enforced on new writes, not yet checked against the
    /// rows already there (`VALIDATE CONSTRAINT` does that). Recorded only
    /// when set, so the shared shape is unchanged otherwise.
    pub not_valid: bool,
}

/// A declared UNIQUE constraint, in the Python server's on-disk shape
/// (`src/secantus/sql/catalog.py`'s `UniqueConstraint`). Every key that side
/// writes is round-tripped here, including the ones this server does not act
/// on yet, so a table created by one server reads back intact in the other —
/// dropping an unknown key would silently rewrite the other server's catalog.
///
/// `deferrable` constraints are judged at COMMIT rather than per write, so they
/// are deliberately NOT backed by a storage index; `exclusion` marks an
/// `EXCLUDE (col WITH =)`, which is unique enforcement reported as `23P01`.
#[derive(Debug, Clone, PartialEq)]
pub struct UniqueConstraint {
    pub name: String,
    pub columns: Vec<String>,
    pub deferrable: bool,
    pub initially_deferred: bool,
    pub comment: Option<String>,
    pub exclusion: bool,
    /// An `EXCLUDE` constraint's per-column operators when any is not `=`
    /// (`&&` over a range): enforced by the executor row by row rather than
    /// by a unique index. Empty for UNIQUE and for an all-`=` EXCLUDE, which
    /// is the Python server's shape.
    pub exclusion_ops: Vec<String>,
    /// The EXCLUDE's access method (`gist`, `btree`), for its definition.
    pub exclusion_method: Option<String>,
    /// `UNIQUE NULLS NOT DISTINCT` (PostgreSQL 15): NULLs collide like any
    /// other value. Recorded only when set, so the shared shape is unchanged
    /// for every constraint without it.
    pub nulls_not_distinct: bool,
}

impl UniqueConstraint {
    pub fn new(name: &str, columns: Vec<String>) -> Self {
        Self {
            name: name.to_string(),
            columns,
            deferrable: false,
            initially_deferred: false,
            comment: None,
            exclusion: false,
            exclusion_ops: Vec::new(),
            exclusion_method: None,
            nulls_not_distinct: false,
        }
    }

    pub fn to_document(&self) -> Document {
        let mut d = doc! {
            "name": &self.name,
            "columns": self.columns.iter().map(|c| Bson::String(c.clone()))
                .collect::<Vec<_>>(),
            "deferrable": self.deferrable,
            "initially_deferred": self.initially_deferred,
            "comment": self.comment.clone().map(Bson::String).unwrap_or(Bson::Null),
            "exclusion": self.exclusion,
        };
        if !self.exclusion_ops.is_empty() {
            d.insert("exclusion_ops", self.exclusion_ops.clone());
        }
        if let Some(m) = &self.exclusion_method {
            d.insert("exclusion_method", m.as_str());
        }
        if self.nulls_not_distinct {
            d.insert("nulls_not_distinct", true);
        }
        d
    }

    pub fn from_document(d: &Document) -> Option<Self> {
        Some(Self {
            name: d.get_str("name").ok()?.to_string(),
            columns: d
                .get_array("columns")
                .ok()?
                .iter()
                .filter_map(|b| b.as_str().map(str::to_string))
                .collect(),
            deferrable: d.get_bool("deferrable").unwrap_or(false),
            initially_deferred: d.get_bool("initially_deferred").unwrap_or(false),
            comment: d.get_str("comment").ok().map(str::to_string),
            exclusion: d.get_bool("exclusion").unwrap_or(false),
            exclusion_ops: d
                .get_array("exclusion_ops")
                .map(|a| {
                    a.iter()
                        .filter_map(|b| b.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            exclusion_method: d.get_str("exclusion_method").ok().map(str::to_string),
            nulls_not_distinct: d.get_bool("nulls_not_distinct").unwrap_or(false),
        })
    }
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
    /// `MATCH FULL`: a key with SOME null columns is a violation, not a
    /// pass. Written only when set, so the shared document shape is the
    /// Python server's for the default MATCH SIMPLE.
    pub match_full: bool,
    /// `COMMENT ON CONSTRAINT`, shared with the Python server.
    pub comment: Option<String>,
}

impl CheckConstraint {
    pub fn to_document(&self) -> Document {
        let mut d = doc! {
            "name": &self.name,
            "expression": &self.expression,
            "comment": self.comment.clone().map_or(Bson::Null, Bson::String),
        };
        if self.not_valid {
            d.insert("not_valid", true);
        }
        d
    }

    pub fn from_document(d: &Document) -> Option<Self> {
        Some(Self {
            name: d.get_str("name").ok()?.to_string(),
            expression: d.get_str("expression").ok()?.to_string(),
            comment: d.get_str("comment").ok().map(str::to_string),
            not_valid: d.get_bool("not_valid").unwrap_or(false),
        })
    }
}

impl ForeignKey {
    pub fn to_document(&self) -> Document {
        let mut d = doc! {
            "name": &self.name,
            "columns": self.columns.clone(),
            "ref_table": &self.ref_table,
            "ref_columns": self.ref_columns.clone(),
            "on_delete": self.on_delete.as_deref().map_or(Bson::Null, Bson::from),
            "on_update": self.on_update.as_deref().map_or(Bson::Null, Bson::from),
            "deferrable": self.deferrable,
            "initially_deferred": self.initially_deferred,
            "comment": self.comment.clone().map_or(Bson::Null, Bson::String),
        };
        if self.match_full {
            d.insert("match_full", true);
        }
        d
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
            match_full: d.get_bool("match_full").unwrap_or(false),
            comment: d.get_str("comment").ok().map(str::to_string),
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
    pub unique_constraints: Vec<UniqueConstraint>,
    /// Every table-level catalog key this model does not own -- `comment`,
    /// `pk_name`, `pk_comment`, `pk_column_order`, `expr_indexes` -- kept
    /// verbatim, as `Column::extra` does for columns. They used to be written
    /// back as NULLs, so a Rust rewrite of a table erased what the Python
    /// server recorded (a `COMMENT ON TABLE`, a named primary key).
    pub extra: Document,
}

impl TableDef {
    pub fn new(name: &str, columns: Vec<Column>) -> Self {
        Self {
            name: name.to_string(),
            columns,
            temp: false,
            check_constraints: Vec::new(),
            foreign_keys: Vec::new(),
            unique_constraints: Vec::new(),
            extra: Document::new(),
        }
    }

    const OWNED_KEYS: &'static [&'static str] = &[
        "_id",
        "table",
        "collection",
        "columns",
        "temp",
        "foreign_keys",
        "check_constraints",
        "unique_constraints",
    ];

    /// What `to_document` writes for the keys this model does not own; see
    /// `Column::unmodelled_defaults`.
    fn unmodelled_defaults() -> Document {
        doc! {
            "comment": Bson::Null,
            "pk_name": Bson::Null,
            "pk_comment": Bson::Null,
            "pk_column_order": Bson::Null,
            "expr_indexes": Vec::<Bson>::new(),
        }
    }

    /// `COMMENT ON TABLE`'s text, if any.
    pub fn comment(&self) -> Option<&str> {
        self.extra.get_str("comment").ok()
    }

    /// Set or clear (`None`) the table's comment.
    pub fn set_comment(&mut self, comment: Option<String>) {
        match comment {
            Some(c) => {
                self.extra.insert("comment", c);
            }
            None => {
                self.extra.remove("comment");
            }
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
        // In the Python server's key order (the golden test pins it), each
        // unmodelled key its stored value or its default.
        let defaults = Self::unmodelled_defaults();
        let get = |k: &str| -> Bson {
            self.extra
                .get(k)
                .cloned()
                .or_else(|| defaults.get(k).cloned())
                .unwrap_or(Bson::Null)
        };
        let mut out = doc! {
            "_id": &self.name,
            "table": &self.name,
            "collection": &self.name,
            "columns": self.columns.iter().map(|c| Bson::Document(c.to_document()))
                .collect::<Vec<_>>(),
            "comment": get("comment"),
            "pk_name": get("pk_name"),
            "pk_comment": get("pk_comment"),
            "pk_column_order": get("pk_column_order"),
            "temp": self.temp,
            "foreign_keys": self.foreign_keys.iter().map(|f| Bson::Document(f.to_document()))
                .collect::<Vec<_>>(),
            "check_constraints": self.check_constraints.iter()
                .map(|c| Bson::Document(c.to_document()))
                .collect::<Vec<_>>(),
            "unique_constraints": self.unique_constraints.iter()
                .map(|u| Bson::Document(u.to_document()))
                .collect::<Vec<_>>(),
            "expr_indexes": get("expr_indexes"),
        };
        // Keys neither model knows, after the known ones.
        for (k, v) in &self.extra {
            if !out.contains_key(k) {
                out.insert(k.clone(), v.clone());
            }
        }
        out
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
            unique_constraints: d
                .get_array("unique_constraints")
                .map(|a| {
                    a.iter()
                        .filter_map(|b| b.as_document())
                        .filter_map(UniqueConstraint::from_document)
                        .collect()
                })
                .unwrap_or_default(),
            extra: {
                let defaults = Self::unmodelled_defaults();
                d.iter()
                    .filter(|(k, v)| {
                        !Self::OWNED_KEYS.contains(&k.as_str())
                            && defaults.get(k.as_str()) != Some(v)
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            },
        })
    }
}

#[cfg(test)]
mod golden;
