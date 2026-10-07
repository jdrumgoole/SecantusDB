//! `CREATE COLLATION` / `DROP COLLATION`: stored in `__sql_collations__`,
//! installed into the planner (`secantus_pgplan::collation`), which applies
//! them, and listed in `pg_collation`. A column that names a collation
//! depends on it.

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgcatalog::TableDef;
use secantus_pgplan::collation::UserCollation;

use crate::PgHandler;

pub(crate) const COLLATION_COLLECTION: &str = "__sql_collations__";

impl PgHandler {
    /// The database's collations, for the planner.
    pub(crate) fn user_collations(&self) -> Vec<UserCollation> {
        self.type_catalog_docs(COLLATION_COLLECTION)
            .map(|docs| {
                docs.iter()
                    .map(|d| UserCollation {
                        name: d.get_str("_id").unwrap_or_default().to_string(),
                        provider: d
                            .get_str("provider")
                            .ok()
                            .and_then(|p| p.chars().next())
                            .unwrap_or('c'),
                        locale: d.get_str("locale").unwrap_or_default().to_string(),
                        deterministic: d.get_bool("deterministic").unwrap_or(true),
                        oid: d
                            .get_i64("oid")
                            .or_else(|_| d.get_i32("oid").map(i64::from))
                            .unwrap_or(0),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn create_collation(
        &self,
        collation: UserCollation,
        if_not_exists: bool,
    ) -> PgWireResult<Vec<Response>> {
        let tag = || Ok(vec![Response::Execution(Tag::new("CREATE COLLATION"))]);
        let builtin = matches!(
            collation.name.as_str(),
            "C" | "POSIX" | "ucs_basic" | "default"
        ) || collation.name.ends_with("-x-icu");
        if builtin
            || self
                .user_collations()
                .iter()
                .any(|c| c.name == collation.name)
        {
            let message = format!("collation \"{}\" already exists", collation.name);
            if if_not_exists {
                self.notice("42710", format!("{message}, skipping"), None);
                return tag();
            }
            return Err(Self::user_error("42710", message));
        }
        let doc = bson::doc! {
            "_id": &collation.name,
            "provider": collation.provider.to_string(),
            "locale": &collation.locale,
            "deterministic": collation.deterministic,
            "oid": Self::index_oid(&format!("coll:{}", collation.name)),
            "owner": self.current_role_name(),
        };
        self.put(COLLATION_COLLECTION, &collation.name, doc)?;
        tag()
    }

    /// The columns that name collation `name`: `(table, column)`.
    fn collation_columns(&self, name: &str) -> PgWireResult<Vec<(TableDef, String)>> {
        Ok(self
            .all_table_defs()?
            .into_iter()
            .flat_map(|t| {
                t.columns
                    .iter()
                    .filter(|c| c.extra.get_str("collation") == Ok(name))
                    .map(|c| c.name.clone())
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(move |c| (t.clone(), c))
            })
            .collect())
    }

    pub(crate) fn drop_collations(
        &self,
        names: &[String],
        if_exists: bool,
        cascade: bool,
    ) -> PgWireResult<Vec<Response>> {
        let existing = self.user_collations();
        for name in names {
            if !existing.iter().any(|c| c.name == *name) {
                if if_exists {
                    self.notice(
                        "00000",
                        format!("collation \"{name}\" does not exist, skipping"),
                        None,
                    );
                    continue;
                }
                return Err(Self::user_error(
                    "42704",
                    format!("collation \"{name}\" for encoding \"UTF8\" does not exist"),
                ));
            }
            let dependents = self.collation_columns(name)?;
            if !dependents.is_empty() {
                if !cascade {
                    let mut info = ErrorInfo::new(
                        "ERROR".into(),
                        "2BP01".into(),
                        format!("cannot drop collation {name} because other objects depend on it"),
                    );
                    info.detail = Some(
                        dependents
                            .iter()
                            .map(|(t, c)| {
                                format!(
                                    "column {c} of table {} depends on collation {name}",
                                    t.name
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                    info.hint =
                        Some("Use DROP ... CASCADE to drop the dependent objects too.".into());
                    return Err(PgWireError::UserError(Box::new(info)));
                }
                for (t, c) in dependents {
                    self.execute_inner(
                        secantus_pgplan::Statement::AlterTable {
                            table: t.name.clone(),
                            missing_ok: false,
                            actions: vec![secantus_pgplan::AlterTableAction::DropColumn {
                                name: c,
                                if_exists: true,
                            }],
                        },
                        0,
                    )?;
                }
            }
            self.delete_type_doc(COLLATION_COLLECTION, name)?;
        }
        Ok(vec![Response::Execution(Tag::new("DROP COLLATION"))])
    }

    /// `pg_collation`'s rows for the user collations and the ICU built-ins
    /// clients look up by name.
    pub(crate) fn user_collation_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        let row =
            |oid: i64, name: &str, provider: char, deterministic: bool, locale: &str, ns: i64| {
                let mut d = Document::new();
                d.insert(f("oid"), Bson::Int64(oid));
                d.insert(f("collname"), name);
                d.insert(f("collnamespace"), Bson::Int64(ns));
                d.insert(f("collowner"), Bson::Int64(10));
                d.insert(f("collprovider"), provider.to_string());
                d.insert(f("collisdeterministic"), deterministic);
                d.insert(
                    f("collencoding"),
                    Bson::Int32(if provider == 'i' { -1 } else { 6 }),
                );
                if provider == 'i' {
                    d.insert(f("collcollate"), Bson::Null);
                    d.insert(f("collctype"), Bson::Null);
                    d.insert(f("colliculocale"), locale);
                } else {
                    d.insert(f("collcollate"), locale);
                    d.insert(f("collctype"), locale);
                    d.insert(f("colliculocale"), Bson::Null);
                }
                d
            };
        let mut rows = vec![
            row(12713, "und-x-icu", 'i', true, "und", 11),
            row(12860, "en-x-icu", 'i', true, "en", 11),
        ];
        for c in self.user_collations() {
            rows.push(row(
                c.oid,
                &c.name,
                c.provider,
                c.deterministic,
                &c.locale,
                Self::PUBLIC_NAMESPACE_OID,
            ));
        }
        rows
    }
}
