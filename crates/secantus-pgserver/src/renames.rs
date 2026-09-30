//! `ALTER ... RENAME` for the objects that are not a table, view or column:
//! indexes, constraints, sequences, triggers, rules, types (with their
//! attributes), domains and schemas. Each rewrites the catalog rows that
//! name the object, and the rows and indexes that store data under it.

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgcatalog::SEQUENCE_COLLECTION;

use crate::{encode_doc, triggers::TRIGGER_COLLECTION, PgHandler};

fn error(code: &str, message: String) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        code.into(),
        message,
    )))
}

fn done(tag: &str) -> PgWireResult<Vec<Response>> {
    Ok(vec![Response::Execution(Tag::new(tag))])
}

impl PgHandler {
    /// `ALTER <kind> ... RENAME ...`.
    pub(crate) fn rename_object(
        &self,
        kind: &str,
        target: &str,
        sub: &str,
        to: &str,
        missing_ok: bool,
    ) -> PgWireResult<Vec<Response>> {
        match kind {
            "index" => self.rename_index(target, to, missing_ok),
            "constraint" => self.rename_constraint(target, sub, to),
            "sequence" => self.rename_sequence(target, to, missing_ok),
            "trigger" => self.rename_trigger(target, sub, to),
            "rule" => self.rename_rule(target, sub, to),
            _ => self.rename_type_object(kind, target, sub, to, missing_ok),
        }
    }

    /// The relation-name check every relation rename shares: 42P07 when the
    /// new name is taken by any relation.
    fn relation_name_free(&self, to: &str) -> PgWireResult<()> {
        let taken = self.lookup(to).is_some()
            || self.views()?.iter().any(|(n, _)| n == to)
            || self.index_relations().iter().any(|ix| ix.name == to)
            || self.sequence_doc(to)?.is_some();
        if taken {
            return Err(error("42P07", format!("relation \"{to}\" already exists")));
        }
        Ok(())
    }

    /// `ALTER INDEX i RENAME TO j`. A storage index is renamed by building
    /// it again under the new name and dropping the old one; an index that
    /// backs a UNIQUE / PRIMARY KEY constraint renames the constraint too,
    /// as PostgreSQL's does.
    fn rename_index(&self, index: &str, to: &str, missing_ok: bool) -> PgWireResult<Vec<Response>> {
        let Some(ix) = self
            .index_relations()
            .into_iter()
            .find(|ix| ix.name == index)
        else {
            if missing_ok {
                self.notice(
                    "00000",
                    format!("relation \"{index}\" does not exist, skipping"),
                    None,
                );
                return done("ALTER INDEX");
            }
            return Err(error(
                "42P01",
                format!("relation \"{index}\" does not exist"),
            ));
        };
        self.relation_name_free(to)?;
        let mut def = ix.table.clone();
        let table = def.name.clone();
        if ix.primary {
            // The primary key's storage index is the `_id` one; only its
            // name (the constraint's) is recorded.
            def.extra.insert("pk_name", to);
            self.rewrite_catalog(&table, &def)?;
            return done("ALTER INDEX");
        }
        let stored = self
            .storage
            .list_indexes(self.db(), &table)
            .map_err(|e| Self::storage_err("could not read the indexes", e))?
            .into_iter()
            .find(|d| d.get_str("name") == Ok(index));
        if let Some(stored) = stored {
            let key = stored.get_document("key").cloned().unwrap_or_default();
            let mut options = stored.clone();
            for k in ["name", "key", "v", "ns"] {
                options.remove(k);
            }
            let mut key_spec = key.clone();
            // An expression index keeps its computed key in a hidden field
            // named after the index: that field moves with the name.
            if stored.contains_key("sqlExpressions") {
                let (old_field, new_field) = (
                    Self::expression_index_field(index),
                    Self::expression_index_field(to),
                );
                self.rewrite_rows(&table, |d| {
                    if let Some(v) = d.remove(&old_field) {
                        d.insert(new_field.clone(), v);
                    }
                    Ok(())
                })?;
                key_spec = Document::new();
                for (k, v) in key.iter() {
                    let k = if *k == old_field {
                        new_field.clone()
                    } else {
                        k.clone()
                    };
                    key_spec.insert(k, v.clone());
                }
                options.insert(
                    "partialFilterExpression",
                    bson::doc! { new_field.clone(): { "$exists": true } },
                );
            }
            self.storage
                .create_index(self.db(), &table, to, &key_spec, &options)
                .map_err(|e| Self::storage_err("could not rename the index", e))?;
            self.storage
                .drop_index(self.db(), &table, index)
                .map_err(|e| Self::storage_err("could not rename the index", e))?;
        }
        if let Some(u) = def.unique_constraints.iter_mut().find(|u| u.name == index) {
            u.name = to.to_string();
            self.rewrite_catalog(&table, &def)?;
        }
        done("ALTER INDEX")
    }

    /// `ALTER TABLE t RENAME CONSTRAINT c TO d`: a CHECK, FOREIGN KEY, UNIQUE
    /// or PRIMARY KEY (whose index takes the new name as well).
    fn rename_constraint(&self, table: &str, name: &str, to: &str) -> PgWireResult<Vec<Response>> {
        let Some(mut def) = self.lookup(table) else {
            return Err(error(
                "42P01",
                format!("relation \"{table}\" does not exist"),
            ));
        };
        let taken = def.check_constraints.iter().any(|c| c.name == to)
            || def.unique_constraints.iter().any(|u| u.name == to)
            || def.foreign_keys.iter().any(|f| f.name == to);
        if taken {
            return Err(error(
                "42710",
                format!("constraint \"{to}\" for relation \"{table}\" already exists"),
            ));
        }
        let pk_name = def
            .extra
            .get_str("pk_name")
            .map(str::to_string)
            .unwrap_or_else(|_| format!("{table}_pkey"));
        if let Some(c) = def.check_constraints.iter_mut().find(|c| c.name == name) {
            c.name = to.to_string();
        } else if let Some(f) = def.foreign_keys.iter_mut().find(|f| f.name == name) {
            f.name = to.to_string();
        } else if def.unique_constraints.iter().any(|u| u.name == name) {
            // The constraint's index carries its name: rename them together.
            return self
                .rename_index(name, to, false)
                .map(|_| vec![Response::Execution(Tag::new("ALTER TABLE"))]);
        } else if name == pk_name && def.columns.iter().any(|c| c.pk) {
            def.extra.insert("pk_name", to);
        } else {
            return Err(error(
                "42704",
                format!("constraint \"{name}\" for table \"{table}\" does not exist"),
            ));
        }
        self.rewrite_catalog(table, &def)?;
        done("ALTER TABLE")
    }

    /// `ALTER SEQUENCE s RENAME TO t`, and every column drawing on it.
    fn rename_sequence(
        &self,
        name: &str,
        to: &str,
        missing_ok: bool,
    ) -> PgWireResult<Vec<Response>> {
        let Some(mut doc) = self.sequence_doc(name)? else {
            if missing_ok {
                self.notice(
                    "00000",
                    format!("relation \"{name}\" does not exist, skipping"),
                    None,
                );
                return done("ALTER SEQUENCE");
            }
            return Err(error(
                "42P01",
                format!("relation \"{name}\" does not exist"),
            ));
        };
        self.relation_name_free(to)?;
        self.storage
            .delete_matching(
                self.db(),
                SEQUENCE_COLLECTION,
                &bson::doc! {"_id": name},
                0,
                &Document::new(),
                None,
            )
            .map_err(|e| Self::storage_err("could not rename the sequence", e))?;
        doc.insert("_id", to);
        let bytes = encode_doc(&doc).map_err(|e| Self::storage_err("could not encode", e))?;
        self.insert_checked(
            SEQUENCE_COLLECTION,
            vec![bytes],
            "could not rename the sequence",
        )?;
        for mut def in self.all_table_defs()? {
            let mut changed = false;
            for c in &mut def.columns {
                if c.sequence.as_deref() == Some(name) {
                    c.sequence = Some(to.to_string());
                    changed = true;
                }
            }
            if changed {
                self.rewrite_catalog(&def.name.clone(), &def)?;
            }
        }
        done("ALTER SEQUENCE")
    }

    /// `ALTER TRIGGER t ON tab RENAME TO u`.
    fn rename_trigger(&self, table: &str, name: &str, to: &str) -> PgWireResult<Vec<Response>> {
        let (old, new) = (format!("{table}::{name}"), format!("{table}::{to}"));
        let docs = self.type_catalog_docs(TRIGGER_COLLECTION)?;
        if docs.iter().any(|d| d.get_str("_id") == Ok(new.as_str())) {
            return Err(error(
                "42710",
                format!("trigger \"{to}\" for relation \"{table}\" already exists"),
            ));
        }
        let Some(mut doc) = docs
            .iter()
            .find(|d| d.get_str("_id") == Ok(old.as_str()))
            .cloned()
        else {
            return Err(error(
                "42704",
                format!("trigger \"{name}\" for table \"{table}\" does not exist"),
            ));
        };
        self.delete_type_doc(TRIGGER_COLLECTION, &old)?;
        doc.insert("_id", new.as_str());
        doc.insert("name", to);
        self.insert_type_doc(TRIGGER_COLLECTION, &new, doc)?;
        done("ALTER TRIGGER")
    }

    /// `ALTER RULE r ON tab RENAME TO s`.
    fn rename_rule(&self, table: &str, name: &str, to: &str) -> PgWireResult<Vec<Response>> {
        let (old, new) = (format!("{table}/{name}"), format!("{table}/{to}"));
        let docs = self.type_catalog_docs(crate::rules::RULE_COLLECTION)?;
        if docs.iter().any(|d| d.get_str("_id") == Ok(new.as_str())) {
            return Err(error(
                "42710",
                format!("rule \"{to}\" for relation \"{table}\" already exists"),
            ));
        }
        let Some(mut doc) = docs
            .iter()
            .find(|d| d.get_str("_id") == Ok(old.as_str()))
            .cloned()
        else {
            return Err(error(
                "42704",
                format!("rule \"{name}\" for relation \"{table}\" does not exist"),
            ));
        };
        self.delete_type_doc(crate::rules::RULE_COLLECTION, &old)?;
        doc.insert("_id", new.as_str());
        doc.insert("name", to);
        self.insert_type_doc(crate::rules::RULE_COLLECTION, &new, doc)?;
        done("ALTER RULE")
    }

    /// The type catalogs a user type lives in.
    fn type_collections() -> [&'static str; 5] {
        [
            Self::ENUM_COLLECTION,
            Self::COMPOSITE_COLLECTION,
            Self::RANGE_COLLECTION,
            Self::BASE_TYPE_COLLECTION,
            Self::DOMAIN_COLLECTION,
        ]
    }

    /// Types, attributes, domains and schemas.
    fn rename_type_object(
        &self,
        kind: &str,
        target: &str,
        sub: &str,
        to: &str,
        missing_ok: bool,
    ) -> PgWireResult<Vec<Response>> {
        match kind {
            "type" | "domain" => self.rename_type(kind, target, to, missing_ok),
            "attribute" => self.rename_attribute(target, sub, to),
            "domain constraint" => self.rename_domain_constraint(target, sub, to),
            "schema" => self.rename_schema(target, to),
            other => Err(error(
                "0A000",
                format!("ALTER {} ... RENAME", other.to_uppercase()),
            )),
        }
    }

    /// Every stored reference to type `old` (its catalog key) moved to
    /// `new`: table columns (and their arrays), composite fields, domain
    /// bases, range subtypes and routine signatures.
    fn retarget_type_references(&self, old: &str, new: &str) -> PgWireResult<()> {
        let (old_array, new_array) = (format!("{old}[]"), format!("{new}[]"));
        let moved = |t: &str| -> Option<String> {
            if t == old {
                Some(new.to_string())
            } else if t == old_array {
                Some(new_array.clone())
            } else {
                None
            }
        };
        for mut def in self.all_table_defs()? {
            let mut changed = false;
            for c in &mut def.columns {
                if let Some(t) = moved(&c.pg_type) {
                    c.pg_type = t;
                    changed = true;
                }
                let keys: Vec<String> = c.extra.keys().cloned().collect();
                for k in keys {
                    if let Some(t) = c.extra.get_str(&k).ok().and_then(moved) {
                        c.extra.insert(k, t);
                        changed = true;
                    }
                }
            }
            if changed {
                self.rewrite_catalog(&def.name.clone(), &def)?;
            }
        }
        let retag = |v: &mut Bson| -> bool {
            match v {
                Bson::String(t) => match moved(t) {
                    Some(n) => {
                        *t = n;
                        true
                    }
                    None => false,
                },
                _ => false,
            }
        };
        for coll in [
            Self::COMPOSITE_COLLECTION,
            Self::DOMAIN_COLLECTION,
            Self::RANGE_COLLECTION,
            Self::FUNCTION_COLLECTION,
        ] {
            let docs = if coll == Self::FUNCTION_COLLECTION {
                self.type_catalog_docs_raw(coll)?
            } else {
                self.type_catalog_docs(coll)?
            };
            for d in docs.iter() {
                let mut doc = d.clone();
                let mut changed = false;
                for key in ["base_tag", "subtype", "return_tag"] {
                    if let Some(v) = doc.get_mut(key) {
                        changed |= retag(v);
                    }
                }
                for key in ["fields", "param_types"] {
                    if let Some(Bson::Array(items)) = doc.get_mut(key) {
                        for item in items.iter_mut() {
                            match item {
                                // A composite field: `[name, type, null]`.
                                Bson::Array(f) if f.len() > 1 => changed |= retag(&mut f[1]),
                                other => changed |= retag(other),
                            }
                        }
                    }
                }
                if let Some(Bson::Array(cols)) = doc.get_mut("table_columns") {
                    for c in cols.iter_mut() {
                        if let Bson::Document(c) = c {
                            if let Some(v) = c.get_mut("type_tag") {
                                changed |= retag(v);
                            }
                        }
                    }
                }
                if changed {
                    let id = doc.get_str("_id").unwrap_or_default().to_string();
                    self.delete_type_doc(coll, &id)?;
                    self.insert_type_doc(coll, &id, doc)?;
                }
            }
        }
        Ok(())
    }

    /// `ALTER TYPE t RENAME TO u` / `ALTER DOMAIN d RENAME TO e`.
    fn rename_type(
        &self,
        kind: &str,
        target: &str,
        to: &str,
        missing_ok: bool,
    ) -> PgWireResult<Vec<Response>> {
        let tag = if kind == "domain" {
            "ALTER DOMAIN"
        } else {
            "ALTER TYPE"
        };
        let collections: Vec<&'static str> = if kind == "domain" {
            vec![Self::DOMAIN_COLLECTION]
        } else {
            Self::type_collections().to_vec()
        };
        let mut found = None;
        for coll in &collections {
            if let Some(d) = self
                .type_catalog_docs(coll)?
                .iter()
                .find(|d| d.get_str("_id") == Ok(target))
            {
                found = Some((*coll, d.clone()));
                break;
            }
        }
        let Some((coll, mut doc)) = found else {
            if missing_ok {
                self.notice(
                    "00000",
                    format!("type \"{target}\" does not exist, skipping"),
                    None,
                );
                return done(tag);
            }
            return Err(error("42704", format!("type \"{target}\" does not exist")));
        };
        let old_bare = target.rsplit('.').next().unwrap_or(target).to_string();
        let new_key = match target.rsplit_once('.') {
            Some((schema, _)) => format!("{schema}.{to}"),
            None => to.to_string(),
        };
        let taken = secantus_pgplan::pgtypes::oid_of_name(to).is_some()
            || Self::type_collections().iter().any(|c| {
                self.type_catalog_docs(c).is_ok_and(|docs| {
                    docs.iter()
                        .any(|d| d.get_str("_id") == Ok(new_key.as_str()))
                })
            })
            || self.lookup(to).is_some();
        if taken {
            return Err(error("42710", format!("type \"{to}\" already exists")));
        }
        doc.insert("_id", new_key.as_str());
        for key in ["enum", "composite", "range", "domain", "name", "type"] {
            if doc.get_str(key) == Ok(old_bare.as_str()) {
                doc.insert(key, to);
            }
        }
        self.delete_type_doc(coll, target)?;
        self.insert_type_doc(coll, &new_key, doc)?;
        self.retarget_type_references(target, &new_key)?;
        done(tag)
    }

    /// `ALTER TYPE t RENAME ATTRIBUTE a TO b` on a composite type.
    fn rename_attribute(&self, target: &str, attr: &str, to: &str) -> PgWireResult<Vec<Response>> {
        if self.lookup(target).is_some() {
            let mut info = ErrorInfo::new(
                "ERROR".into(),
                "42809".into(),
                format!("\"{target}\" is a table"),
            );
            info.hint = Some("Use ALTER TABLE instead.".into());
            return Err(PgWireError::UserError(Box::new(info)));
        }
        let Some(mut doc) = self
            .type_catalog_docs(Self::COMPOSITE_COLLECTION)?
            .iter()
            .find(|d| d.get_str("_id") == Ok(target))
            .cloned()
        else {
            return Err(error("42704", format!("type \"{target}\" does not exist")));
        };
        let Some(Bson::Array(fields)) = doc.get_mut("fields") else {
            return Err(error("42704", format!("type \"{target}\" does not exist")));
        };
        let name_of = |f: &Bson| match f {
            Bson::Array(f) => f.first().and_then(Bson::as_str).map(str::to_string),
            _ => None,
        };
        if fields.iter().any(|f| name_of(f).as_deref() == Some(to)) {
            return Err(error(
                "42701",
                format!("column \"{to}\" of relation \"{target}\" already exists"),
            ));
        }
        let Some(field) = fields
            .iter_mut()
            .find(|f| name_of(f).as_deref() == Some(attr))
        else {
            return Err(error("42703", format!("column \"{attr}\" does not exist")));
        };
        if let Bson::Array(f) = field {
            f[0] = Bson::String(to.to_string());
        }
        self.delete_type_doc(Self::COMPOSITE_COLLECTION, target)?;
        self.insert_type_doc(Self::COMPOSITE_COLLECTION, target, doc)?;
        done("ALTER TYPE")
    }

    /// `ALTER DOMAIN d RENAME CONSTRAINT c TO k`.
    fn rename_domain_constraint(
        &self,
        domain: &str,
        name: &str,
        to: &str,
    ) -> PgWireResult<Vec<Response>> {
        let Some(mut doc) = self
            .type_catalog_docs(Self::DOMAIN_COLLECTION)?
            .iter()
            .find(|d| d.get_str("_id") == Ok(domain))
            .cloned()
        else {
            return Err(error("42704", format!("type \"{domain}\" does not exist")));
        };
        let Some(Bson::Array(checks)) = doc.get_mut("checks") else {
            return Err(error(
                "42704",
                format!("constraint \"{name}\" for domain {domain} does not exist"),
            ));
        };
        let named =
            |c: &Bson, n: &str| matches!(c, Bson::Document(d) if d.get_str("name") == Ok(n));
        if checks.iter().any(|c| named(c, to)) {
            return Err(error(
                "42710",
                format!("constraint \"{to}\" for domain {domain} already exists"),
            ));
        }
        let Some(Bson::Document(check)) = checks.iter_mut().find(|c| named(c, name)) else {
            return Err(error(
                "42704",
                format!("constraint \"{name}\" for domain {domain} does not exist"),
            ));
        };
        check.insert("name", to);
        self.delete_type_doc(Self::DOMAIN_COLLECTION, domain)?;
        self.insert_type_doc(Self::DOMAIN_COLLECTION, domain, doc)?;
        done("ALTER DOMAIN")
    }

    /// `ALTER SCHEMA s RENAME TO t`: the schema row, and every type and
    /// routine recorded in it.
    fn rename_schema(&self, name: &str, to: &str) -> PgWireResult<Vec<Response>> {
        if !self.namespaces().iter().any(|(n, _)| n == name) {
            return Err(error("3F000", format!("schema \"{name}\" does not exist")));
        }
        if self.namespaces().iter().any(|(n, _)| n == to) {
            return Err(error("42P06", format!("schema \"{to}\" already exists")));
        }
        if matches!(
            name,
            "public" | "pg_catalog" | "information_schema" | "pg_toast"
        ) {
            return Err(error("42939", format!("unacceptable schema name \"{to}\"")));
        }
        if let Some(mut doc) = self
            .type_catalog_docs(Self::SCHEMA_COLLECTION)?
            .iter()
            .find(|d| d.get_str("_id") == Ok(name))
            .cloned()
        {
            doc.insert("_id", to);
            self.delete_type_doc(Self::SCHEMA_COLLECTION, name)?;
            self.insert_type_doc(Self::SCHEMA_COLLECTION, to, doc)?;
        }
        let prefix = format!("{name}.");
        for coll in Self::type_collections() {
            for d in self.type_catalog_docs(coll)?.iter() {
                if d.get_str("schema") != Ok(name) {
                    continue;
                }
                let id = d.get_str("_id").unwrap_or_default().to_string();
                let new_id = match id.strip_prefix(&prefix) {
                    Some(bare) => format!("{to}.{bare}"),
                    None => id.clone(),
                };
                let mut doc = d.clone();
                doc.insert("schema", to);
                doc.insert("_id", new_id.as_str());
                self.delete_type_doc(coll, &id)?;
                self.insert_type_doc(coll, &new_id, doc)?;
                if new_id != id {
                    self.retarget_type_references(&id, &new_id)?;
                }
            }
        }
        for d in self
            .type_catalog_docs_raw(Self::FUNCTION_COLLECTION)?
            .iter()
        {
            if d.get_str("schema") == Ok(name) {
                let mut doc = d.clone();
                doc.insert("schema", to);
                let id = doc.get_str("_id").unwrap_or_default().to_string();
                self.delete_type_doc(Self::FUNCTION_COLLECTION, &id)?;
                self.insert_type_doc(Self::FUNCTION_COLLECTION, &id, doc)?;
            }
        }
        done("ALTER SCHEMA")
    }
}
