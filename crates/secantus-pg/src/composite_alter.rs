//! `ALTER TYPE t ADD | DROP | ALTER ATTRIBUTE ...` on a composite type, as
//! PostgreSQL 15.19 runs it (an ALTER TABLE of the type's relation):
//!
//! - ADD appends an attribute, and every value of the type already stored
//!   reads it as NULL; DROP removes one, its `attnum` left as a dropped slot
//!   (`........pg.dropped.N........` in `pg_attribute`), so the attributes
//!   after it keep theirs. Both are allowed while a column uses the type.
//! - ALTER ATTRIBUTE TYPE is refused (0A000) while a column uses the type.
//! - A type with typed tables (`CREATE TABLE ... OF type`) is 2BP01 unless
//!   the action says CASCADE, which carries it to those tables' columns.
//!
//! PostgreSQL leaves stored values alone and reads them against the
//! attribute list with its dropped slots. Here a value is its attributes in
//! order, so ADD and DROP rewrite each stored value of the type -- in a
//! column of it, of an array of it, or of a composite or domain over it --
//! which every reader then sees as PostgreSQL's would.

use super::*;
use secantus_pgplan::typed_tables::{AttributeAction, OF_TYPE_KEY};

/// What a stored value of the target type undergoes.
enum Edit {
    /// A NULL appended (ADD) for the attribute of this name and type.
    Append { name: String, ty: String },
    /// The attribute at this position removed (DROP).
    Remove(usize),
}

impl PgHandler {
    pub(crate) fn alter_type_attributes(
        &self,
        name: &str,
        actions: Vec<AttributeAction>,
    ) -> PgWireResult<Vec<Response>> {
        let tag = || Ok(vec![Response::Execution(Tag::new("ALTER TYPE"))]);
        let Some(mut doc) = self
            .type_catalog_docs(Self::COMPOSITE_COLLECTION)?
            .iter()
            .find(|d| d.get_str("_id") == Ok(name) && !d.get_bool("relation").unwrap_or(false))
            .cloned()
        else {
            // A table's row type is not altered here (PostgreSQL's
            // ATSimplePermissions); anything else is not a relation.
            if self.lookup(name).is_some() {
                return Err(Self::user_error(
                    "42809",
                    format!("\"{name}\" is not a composite type"),
                ));
            }
            return Err(Self::user_error(
                "42P01",
                format!("relation \"{name}\" does not exist"),
            ));
        };
        let shown = doc.get_str("composite").unwrap_or(name).to_string();
        let typed: Vec<String> = self
            .all_table_defs()?
            .into_iter()
            .filter(|t| t.extra.get_str(OF_TYPE_KEY) == Ok(name))
            .map(|t| t.name)
            .collect();
        for action in actions {
            let cascade = match &action {
                AttributeAction::Add { cascade, .. }
                | AttributeAction::Drop { cascade, .. }
                | AttributeAction::AlterType { cascade, .. } => *cascade,
            };
            if !typed.is_empty() && !cascade {
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "2BP01".into(),
                    format!(
                        "cannot alter type \"{shown}\" because it is the type of a typed table"
                    ),
                );
                info.hint = Some("Use ALTER ... CASCADE to alter the typed tables too.".into());
                return Err(PgWireError::UserError(Box::new(info)));
            }
            let mut fields: Vec<Bson> = doc.get_array("fields").cloned().unwrap_or_default();
            let names: Vec<String> = fields
                .iter()
                .filter_map(|f| match f {
                    Bson::Array(p) => p.first().and_then(Bson::as_str).map(str::to_string),
                    _ => None,
                })
                .collect();
            let mut widths: Vec<Bson> = doc
                .get_array("widths")
                .cloned()
                .unwrap_or_else(|_| vec![Bson::Null; fields.len()]);
            widths.resize(fields.len(), Bson::Null);
            // Each attribute's attnum, and the dropped slots between them.
            let mut attnums: Vec<Bson> = doc
                .get_array("attnums")
                .cloned()
                .unwrap_or_else(|_| (1..=fields.len() as i32).map(Bson::Int32).collect());
            let mut dropped: Vec<Bson> = doc.get_array("dropped").cloned().unwrap_or_default();
            let missing = |attr: &str| {
                Self::user_error(
                    "42703",
                    format!("column \"{attr}\" of relation \"{shown}\" does not exist"),
                )
            };
            let cascade_sql = |sql: String| -> PgWireResult<()> {
                let previous = self
                    .typed_cascade
                    .swap(true, std::sync::atomic::Ordering::Relaxed);
                let out = self.internal_sql(&sql);
                self.typed_cascade
                    .store(previous, std::sync::atomic::Ordering::Relaxed);
                out
            };
            let q = secantus_pgplan::scalar::quote_identifier;
            match action {
                AttributeAction::Add {
                    name: attr,
                    ty,
                    typmod,
                    ..
                } => {
                    if names.contains(&attr) {
                        return Err(Self::user_error(
                            "42701",
                            format!("column \"{attr}\" of relation \"{shown}\" already exists"),
                        ));
                    }
                    let key = self.function_type_key(&ty)?;
                    if self.type_oid_by_name(&key).is_none() {
                        return Err(Self::user_error(
                            "42704",
                            format!(
                                "type \"{}\" does not exist",
                                secantus_pgplan::pgtypes::user_type_catalog_name(&ty)
                            ),
                        ));
                    }
                    let next = attnums
                        .iter()
                        .chain(dropped.iter())
                        .filter_map(bson_i64)
                        .max()
                        .unwrap_or(0)
                        + 1;
                    fields.push(Bson::Array(vec![
                        Bson::String(attr.clone()),
                        Bson::String(key.clone()),
                        Bson::Null,
                    ]));
                    widths.push(if key == "bpchar" && typmod > 4 {
                        Bson::Int32(typmod - 4)
                    } else {
                        Bson::Null
                    });
                    attnums.push(Bson::Int32(next as i32));
                    self.rewrite_type_values(
                        name,
                        &Edit::Append {
                            name: attr.clone(),
                            ty: key.clone(),
                        },
                    )?;
                    for t in &typed {
                        cascade_sql(format!("ALTER TABLE {} ADD COLUMN {} {ty}", q(t), q(&attr)))?;
                    }
                }
                AttributeAction::Drop {
                    name: attr,
                    if_exists,
                    ..
                } => {
                    let Some(at) = names.iter().position(|n| *n == attr) else {
                        if if_exists {
                            self.notice(
                                "00000",
                                format!(
                                    "column \"{attr}\" of relation \"{shown}\" does not exist, skipping"
                                ),
                                None,
                            );
                            continue;
                        }
                        return Err(missing(&attr));
                    };
                    fields.remove(at);
                    widths.remove(at);
                    dropped.push(attnums.remove(at));
                    self.rewrite_type_values(name, &Edit::Remove(at))?;
                    for t in &typed {
                        cascade_sql(format!(
                            "ALTER TABLE {} DROP COLUMN IF EXISTS {}",
                            q(t),
                            q(&attr)
                        ))?;
                    }
                }
                AttributeAction::AlterType {
                    name: attr,
                    ty,
                    typmod,
                    ..
                } => {
                    let Some(at) = names.iter().position(|n| *n == attr) else {
                        return Err(missing(&attr));
                    };
                    if let Some((table, column)) = self.column_using_type(name, &typed)? {
                        return Err(Self::user_error(
                            "0A000",
                            format!(
                                "cannot alter type \"{shown}\" because column \"{table}.{column}\" uses it"
                            ),
                        ));
                    }
                    let key = self.function_type_key(&ty)?;
                    if self.type_oid_by_name(&key).is_none() {
                        return Err(Self::user_error(
                            "42704",
                            format!(
                                "type \"{}\" does not exist",
                                secantus_pgplan::pgtypes::user_type_catalog_name(&ty)
                            ),
                        ));
                    }
                    fields[at] = Bson::Array(vec![
                        Bson::String(attr.clone()),
                        Bson::String(key.clone()),
                        Bson::Null,
                    ]);
                    widths[at] = if key == "bpchar" && typmod > 4 {
                        Bson::Int32(typmod - 4)
                    } else {
                        Bson::Null
                    };
                    for t in &typed {
                        cascade_sql(format!(
                            "ALTER TABLE {} ALTER COLUMN {} TYPE {ty}",
                            q(t),
                            q(&attr)
                        ))?;
                    }
                }
            }
            doc.insert("fields", fields);
            doc.insert("widths", widths);
            doc.insert("attnums", attnums);
            doc.insert("dropped", dropped);
            self.delete_type_doc(Self::COMPOSITE_COLLECTION, name)?;
            self.insert_type_doc(Self::COMPOSITE_COLLECTION, name, doc.clone())?;
            // The next action, and the next statement, see the new shape.
            self.install_user_types();
        }
        tag()
    }

    /// `ALTER TYPE t RENAME ATTRIBUTE a TO b [CASCADE]`: a type with typed
    /// tables is 2BP01 without CASCADE, which renames their columns too.
    pub(crate) fn rename_attribute_cascade(
        &self,
        target: &str,
        attr: &str,
        to: &str,
        cascade: bool,
    ) -> PgWireResult<Vec<Response>> {
        let typed: Vec<String> = self
            .all_table_defs()?
            .into_iter()
            .filter(|t| t.extra.get_str(OF_TYPE_KEY) == Ok(target))
            .map(|t| t.name)
            .collect();
        if !typed.is_empty() && !cascade {
            let shown = secantus_pgplan::pgtypes::user_type_catalog_name(target);
            let mut info = ErrorInfo::new(
                "ERROR".into(),
                "2BP01".into(),
                format!("cannot alter type \"{shown}\" because it is the type of a typed table"),
            );
            info.hint = Some("Use ALTER ... CASCADE to alter the typed tables too.".into());
            return Err(PgWireError::UserError(Box::new(info)));
        }
        let out = self.rename_attribute(target, attr, to)?;
        let q = secantus_pgplan::scalar::quote_identifier;
        for t in &typed {
            let previous = self
                .typed_cascade
                .swap(true, std::sync::atomic::Ordering::Relaxed);
            let renamed = self.internal_sql(&format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {}",
                q(t),
                q(attr),
                q(to)
            ));
            self.typed_cascade
                .store(previous, std::sync::atomic::Ordering::Relaxed);
            renamed?;
        }
        Ok(out)
    }

    /// The first column (`table`, `column`) holding a value of composite
    /// `target` -- of it, an array of it, or a composite over it -- other
    /// than a typed table's own columns. A column of the array type is
    /// found first, as PostgreSQL 15.19 reports it.
    fn column_using_type(
        &self,
        target: &str,
        typed: &[String],
    ) -> PgWireResult<Option<(String, String)>> {
        let composites = self.composite_field_map()?;
        let tables: Vec<TableDef> = self
            .all_table_defs()?
            .into_iter()
            .filter(|t| !typed.contains(&t.name))
            .collect();
        for arrays in [true, false] {
            for t in &tables {
                for c in &t.columns {
                    if c.pg_type.ends_with("[]") == arrays
                        && reaches(&c.pg_type, target, &composites, 0)
                    {
                        return Ok(Some((t.name.clone(), c.name.clone())));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Every composite's `(resolution name -> field types)`.
    fn composite_field_map(&self) -> PgWireResult<HashMap<String, Vec<String>>> {
        Ok(self
            .type_catalog_docs(Self::COMPOSITE_COLLECTION)?
            .iter()
            .filter_map(|d| {
                let id = d.get_str("_id").ok()?.to_string();
                let types = Self::composite_fields(d)
                    .into_iter()
                    .map(|(_, t)| t)
                    .collect();
                Some((id, types))
            })
            .collect())
    }

    /// Apply `edit` to every stored value of composite `target`.
    fn rewrite_type_values(&self, target: &str, edit: &Edit) -> PgWireResult<()> {
        let composites = self.composite_field_map()?;
        for t in self.all_table_defs()? {
            let columns: Vec<(String, String)> = t
                .columns
                .iter()
                .filter(|c| reaches(&c.pg_type, target, &composites, 0))
                .map(|c| (c.field(), c.pg_type.clone()))
                .collect();
            if columns.is_empty() {
                continue;
            }
            self.rewrite_rows(&t.name, |d| {
                for (field, ty) in &columns {
                    if let Some(v) = d.get_mut(field) {
                        edit_value(v, ty, target, &composites, edit, 0);
                    }
                }
                Ok(())
            })?;
        }
        Ok(())
    }
}

/// Whether a value of type `ty` holds values of composite `target`.
fn reaches(ty: &str, target: &str, composites: &HashMap<String, Vec<String>>, depth: u8) -> bool {
    if depth > 16 {
        return false;
    }
    if ty == target {
        return true;
    }
    if let Some(element) = ty.strip_suffix("[]") {
        return reaches(element, target, composites, depth + 1);
    }
    composites.get(ty).is_some_and(|fields| {
        fields
            .iter()
            .any(|f| reaches(f, target, composites, depth + 1))
    })
}

/// Apply `edit` to the values of composite `target` inside `v`, a value of
/// type `ty`.
fn edit_value(
    v: &mut Bson,
    ty: &str,
    target: &str,
    composites: &HashMap<String, Vec<String>>,
    edit: &Edit,
    depth: u8,
) {
    if depth > 16 {
        return;
    }
    if let Some(element) = ty.strip_suffix("[]") {
        if let Bson::Array(items) = v {
            for item in items {
                // A multidimensional array nests arrays of the same type.
                let ty = if matches!(item, Bson::Array(_)) {
                    ty
                } else {
                    element
                };
                edit_value(item, ty, target, composites, edit, depth + 1);
            }
        }
        return;
    }
    let Bson::Document(d) = v else {
        return;
    };
    if ty == target {
        for key in [
            secantus_pgplan::RECORD_KEY,
            secantus_pgplan::RECORD_TYPES_KEY,
            secantus_pgplan::RECORD_NAMES_KEY,
        ] {
            if let Some(Bson::Array(items)) = d.get_mut(key) {
                match edit {
                    Edit::Append { name, ty } => items.push(match key {
                        secantus_pgplan::RECORD_TYPES_KEY => Bson::String(ty.clone()),
                        secantus_pgplan::RECORD_NAMES_KEY => Bson::String(name.clone()),
                        _ => Bson::Null,
                    }),
                    Edit::Remove(at) if *at < items.len() => {
                        items.remove(*at);
                    }
                    Edit::Remove(_) => {}
                }
            }
        }
        return;
    }
    let Some(field_types) = composites.get(ty) else {
        return;
    };
    if let Some(Bson::Array(items)) = d.get_mut(secantus_pgplan::RECORD_KEY) {
        for (item, fty) in items.iter_mut().zip(field_types) {
            edit_value(item, fty, target, composites, edit, depth + 1);
        }
    }
}
