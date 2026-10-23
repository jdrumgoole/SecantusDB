//! `CREATE / DROP ACCESS METHOD`: a database's own access methods, kept in
//! `__sql_access_methods__` and listed in `pg_am` after the built-in ones.
//! A handler can only be one of PostgreSQL's built-in handler functions (a
//! C function cannot be created here), so a table method stores rows as
//! `heap` does, and an index method has no operator classes: every index
//! on one is refused as PostgreSQL refuses it. A table or materialized view
//! made `USING` one depends on it. Messages measured on PostgreSQL 15.19.

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgcatalog::TableDef;
use secantus_pgplan::misc_alter::{AccessMethodOp, BUILTIN_INDEX_AMS};

use crate::PgHandler;

pub(crate) const ACCESS_METHOD_COLLECTION: &str = "__sql_access_methods__";

/// The built-in handlers of index access methods.
const INDEX_HANDLERS: [&str; 6] = [
    "bthandler",
    "hashhandler",
    "gisthandler",
    "ginhandler",
    "spghandler",
    "brinhandler",
];

/// One of the database's access methods.
pub(crate) struct UserAccessMethod {
    pub name: String,
    pub kind: char,
    pub handler: String,
    pub oid: i64,
}

impl PgHandler {
    pub(crate) fn user_access_methods(&self) -> Vec<UserAccessMethod> {
        self.type_catalog_docs(ACCESS_METHOD_COLLECTION)
            .map(|docs| {
                docs.iter()
                    .map(|d| UserAccessMethod {
                        name: d.get_str("_id").unwrap_or_default().to_string(),
                        kind: d
                            .get_str("kind")
                            .ok()
                            .and_then(|k| k.chars().next())
                            .unwrap_or('t'),
                        handler: d.get_str("handler").unwrap_or_default().to_string(),
                        oid: d.get_i64("oid").unwrap_or(0),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The tables and materialized views made `USING` access method `name`,
    /// as `(description, relation)`.
    fn access_method_dependents(&self, name: &str) -> PgWireResult<Vec<(String, String)>> {
        let matviews: Vec<String> = self.matviews()?.into_iter().map(|(n, _, _)| n).collect();
        let mut out: Vec<(String, String)> = self
            .all_table_defs()?
            .into_iter()
            .filter(|t| t.extra.get_str("access_method") == Ok(name))
            .map(|t| {
                let kind = if matviews.contains(&t.name) {
                    "materialized view"
                } else {
                    "table"
                };
                (format!("{kind} {}", t.name), t.name)
            })
            .collect();
        // In creation order, as PostgreSQL lists dependents.
        out.sort_by_key(|(_, rel)| self.relation_oid(rel).unwrap_or(0));
        Ok(out)
    }

    pub(crate) fn execute_access_method(&self, op: AccessMethodOp) -> PgWireResult<Vec<Response>> {
        let tag = op.tag();
        match op {
            AccessMethodOp::Create {
                name,
                kind,
                handler,
            } => {
                if name == "heap"
                    || BUILTIN_INDEX_AMS.contains(&name.as_str())
                    || self.user_access_methods().iter().any(|a| a.name == name)
                {
                    return Err(Self::user_error(
                        "42710",
                        format!("access method \"{name}\" already exists"),
                    ));
                }
                let func = handler.last().cloned().unwrap_or_default();
                if secantus_pgplan::regobj::builtin_proc_oids(&func).is_empty() {
                    return Err(Self::user_error(
                        "42883",
                        format!("function {}(internal) does not exist", handler.join(".")),
                    ));
                }
                let fits = if kind == 'i' {
                    INDEX_HANDLERS.contains(&func.as_str())
                } else {
                    func == "heap_tableam_handler"
                };
                if !fits {
                    return Err(Self::user_error(
                        "42809",
                        format!(
                            "function {func} must return type {}",
                            if kind == 'i' {
                                "index_am_handler"
                            } else {
                                "table_am_handler"
                            }
                        ),
                    ));
                }
                self.ensure_collection(ACCESS_METHOD_COLLECTION)?;
                self.put(
                    ACCESS_METHOD_COLLECTION,
                    &name,
                    bson::doc! {
                        "_id": &name,
                        "kind": kind.to_string(),
                        "handler": &func,
                        "oid": Self::index_oid(&format!("am:{name}")),
                    },
                )?;
            }
            AccessMethodOp::Drop {
                names,
                if_exists,
                cascade,
            } => {
                let mine = self.user_access_methods();
                let mut doomed: Vec<String> = Vec::new();
                for name in &names {
                    if name == "heap" || BUILTIN_INDEX_AMS.contains(&name.as_str()) {
                        return Err(Self::user_error(
                            "2BP01",
                            format!(
                                "cannot drop access method {name} because it is required by the \
                                 database system"
                            ),
                        ));
                    }
                    if mine.iter().any(|a| a.name == *name) {
                        doomed.push(name.clone());
                    } else if if_exists {
                        self.notice(
                            "00000",
                            format!("access method \"{name}\" does not exist, skipping"),
                            None,
                        );
                    } else {
                        return Err(Self::user_error(
                            "42704",
                            format!("access method \"{name}\" does not exist"),
                        ));
                    }
                }
                let mut dependents: Vec<(String, String, String)> = Vec::new();
                for name in &doomed {
                    for (desc, rel) in self.access_method_dependents(name)? {
                        dependents.push((desc, rel, name.clone()));
                    }
                }
                if !dependents.is_empty() {
                    if !cascade {
                        let message = match doomed.as_slice() {
                            [one] => format!(
                                "cannot drop access method {one} because other objects depend on it"
                            ),
                            _ => "cannot drop desired object(s) because other objects depend \
                                  on them"
                                .to_string(),
                        };
                        let mut info = ErrorInfo::new("ERROR".into(), "2BP01".into(), message);
                        info.detail = Some(
                            dependents
                                .iter()
                                .map(|(d, _, am)| format!("{d} depends on access method {am}"))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                        info.hint =
                            Some("Use DROP ... CASCADE to drop the dependent objects too.".into());
                        return Err(PgWireError::UserError(Box::new(info)));
                    }
                    let descs: Vec<String> = dependents.iter().map(|(d, _, _)| d.clone()).collect();
                    self.cascade_notice(&descs);
                    for (desc, rel, _) in &dependents {
                        let stmt = if desc.starts_with("materialized view") {
                            secantus_pgplan::Statement::DropMatView {
                                names: vec![rel.clone()],
                                if_exists: true,
                            }
                        } else {
                            secantus_pgplan::Statement::DropTable(secantus_pgplan::DropTable {
                                tables: vec![rel.clone()],
                                if_exists: true,
                                cascade: true,
                            })
                        };
                        self.execute_inner(stmt, 0)?;
                    }
                }
                for name in &doomed {
                    self.delete_type_doc(ACCESS_METHOD_COLLECTION, name)?;
                }
            }
        }
        Ok(vec![Response::Execution(Tag::new(tag))])
    }

    /// The database's access methods as `pg_am` rows.
    pub(crate) fn user_access_method_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        self.user_access_methods()
            .into_iter()
            .map(|a| {
                let mut d = Document::new();
                d.insert(f("oid"), Bson::Int64(a.oid));
                d.insert(f("amname"), a.name);
                d.insert(f("amhandler"), self.regproc_bson(&a.handler));
                d.insert(f("amtype"), a.kind.to_string());
                d
            })
            .collect()
    }

    /// `pg_class.relam` of a table or materialized view: its access
    /// method's oid (`heap`'s 2 unless it was made `USING` another).
    pub(crate) fn table_relam(&self, def: Option<&TableDef>) -> i64 {
        def.and_then(|t| t.extra.get_str("access_method").ok())
            .and_then(|am| {
                self.user_access_methods()
                    .into_iter()
                    .find(|a| a.name == am)
                    .map(|a| a.oid)
            })
            .unwrap_or(2)
    }
}
