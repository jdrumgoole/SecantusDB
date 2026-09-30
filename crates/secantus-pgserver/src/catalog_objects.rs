//! Executing the record-only statements `secantus_pgplan::catalog_stmts`
//! plans -- extended statistics, tablespaces, publications -- and the
//! catalogs that report them. Rust-server-only stores; nothing here changes
//! what a query answers.

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgcatalog::{Column, TableDef};
use secantus_pgplan::catalog_stmts::{CatalogOp, PubAction, PubTable};

use crate::PgHandler;

pub(crate) const STATISTICS_COLLECTION: &str = "__sql_statistics__";
pub(crate) const TABLESPACE_COLLECTION: &str = "__sql_tablespaces__";
pub(crate) const PUBLICATION_COLLECTION: &str = "__sql_publications__";

/// PostgreSQL's fixed oids for its own two tablespaces.
const PG_DEFAULT_OID: i64 = 1663;
const PG_GLOBAL_OID: i64 = 1664;

fn strings(d: &Document, key: &str) -> Vec<String> {
    d.get_array(key)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn pub_tables(d: &Document) -> Vec<PubTable> {
    d.get_array("tables")
        .map(|a| {
            a.iter()
                .filter_map(|t| t.as_document())
                .map(|t| PubTable {
                    table: t.get_str("table").unwrap_or_default().to_string(),
                    columns: strings(t, "columns"),
                    row_filter: t.get_str("row_filter").ok().map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn pub_tables_bson(tables: &[PubTable]) -> Vec<Bson> {
    tables
        .iter()
        .map(|t| {
            let mut d = bson::doc! {
                "table": &t.table,
                "columns": t.columns.iter().map(|c| Bson::String(c.clone())).collect::<Vec<_>>(),
            };
            if let Some(f) = &t.row_filter {
                d.insert("row_filter", f);
            }
            Bson::Document(d)
        })
        .collect()
}

impl PgHandler {
    fn catalog_docs(&self, collection: &'static str) -> Vec<Document> {
        self.type_catalog_docs(collection)
            .map(|d| d.to_vec())
            .unwrap_or_default()
    }

    fn catalog_doc(&self, collection: &'static str, id: &str) -> Option<Document> {
        self.catalog_docs(collection)
            .into_iter()
            .find(|d| d.get_str("_id") == Ok(id))
    }

    /// The user tablespaces' names, for the planner's `TABLESPACE` check.
    pub(crate) fn tablespace_names(&self) -> Vec<String> {
        self.catalog_docs(TABLESPACE_COLLECTION)
            .iter()
            .filter_map(|d| d.get_str("_id").ok().map(str::to_string))
            .collect()
    }

    /// What goes with a dropped table: its statistics objects, and its
    /// membership of any publication.
    pub(crate) fn drop_table_catalog_objects(&self, table: &str) -> PgWireResult<()> {
        for d in self.catalog_docs(STATISTICS_COLLECTION) {
            if d.get_str("table") == Ok(table) {
                self.delete_type_doc(STATISTICS_COLLECTION, d.get_str("_id").unwrap_or_default())?;
            }
        }
        for mut d in self.catalog_docs(PUBLICATION_COLLECTION) {
            let mut tables = pub_tables(&d);
            let before = tables.len();
            tables.retain(|t| t.table != table);
            if tables.len() != before {
                let id = d.get_str("_id").unwrap_or_default().to_string();
                d.insert("tables", pub_tables_bson(&tables));
                self.delete_type_doc(PUBLICATION_COLLECTION, &id)?;
                self.insert_type_doc(PUBLICATION_COLLECTION, &id, d)?;
            }
        }
        Ok(())
    }

    fn put(&self, collection: &'static str, id: &str, doc: Document) -> PgWireResult<()> {
        self.ensure_collection(collection)?;
        if self.catalog_doc(collection, id).is_some() {
            self.delete_type_doc(collection, id)?;
        }
        self.insert_type_doc(collection, id, doc)
    }

    fn missing_notice(&self, what: String) {
        self.notice("00000", format!("{what} does not exist, skipping"), None);
    }

    pub(crate) fn execute_catalog(&self, op: CatalogOp) -> PgWireResult<Vec<Response>> {
        let tag = op.tag();
        let done = || Ok(vec![Response::Execution(Tag::new(tag))]);
        match op {
            CatalogOp::CreateStatistics {
                name,
                table,
                columns,
                exprs,
                kinds,
                if_not_exists,
            } => {
                if self.catalog_doc(STATISTICS_COLLECTION, &name).is_some() {
                    let msg = format!("statistics object \"{name}\" already exists");
                    if if_not_exists {
                        self.notice("42710", format!("{msg}, skipping"), None);
                        return done();
                    }
                    return Err(Self::user_error("42710", msg));
                }
                let doc = bson::doc! {
                    "_id": &name,
                    "table": &table,
                    "columns": columns,
                    "exprs": exprs,
                    "kinds": kinds,
                    "owner": self.current_role_name(),
                };
                self.put(STATISTICS_COLLECTION, &name, doc)?;
                done()
            }
            CatalogOp::AlterStatistics {
                name,
                rename,
                missing_ok,
            } => {
                let Some(mut doc) = self.catalog_doc(STATISTICS_COLLECTION, &name) else {
                    let what = format!("statistics object \"{name}\"");
                    if missing_ok {
                        self.missing_notice(what);
                        return done();
                    }
                    return Err(Self::user_error("42704", format!("{what} does not exist")));
                };
                if let Some(to) = rename {
                    if self.catalog_doc(STATISTICS_COLLECTION, &to).is_some() {
                        return Err(Self::user_error(
                            "42710",
                            format!(
                                "statistics object \"{to}\" already exists in schema \"public\""
                            ),
                        ));
                    }
                    self.delete_type_doc(STATISTICS_COLLECTION, &name)?;
                    doc.insert("_id", &to);
                    self.put(STATISTICS_COLLECTION, &to, doc)?;
                }
                done()
            }
            CatalogOp::DropStatistics { names, if_exists } => {
                if !if_exists {
                    if let Some(missing) = names
                        .iter()
                        .find(|n| self.catalog_doc(STATISTICS_COLLECTION, n).is_none())
                    {
                        return Err(Self::user_error(
                            "42704",
                            format!("statistics object \"{missing}\" does not exist"),
                        ));
                    }
                }
                for name in &names {
                    if self.catalog_doc(STATISTICS_COLLECTION, name).is_none() {
                        let what = format!("statistics object \"{name}\"");
                        if if_exists {
                            self.missing_notice(what);
                            continue;
                        }
                        return Err(Self::user_error("42704", format!("{what} does not exist")));
                    }
                    self.delete_type_doc(STATISTICS_COLLECTION, name)?;
                }
                done()
            }
            CatalogOp::CreateTablespace {
                name,
                location,
                owner,
            } => {
                if self.transaction_handle_open() {
                    return Err(Self::user_error(
                        "25001",
                        "CREATE TABLESPACE cannot run inside a transaction block".into(),
                    ));
                }
                if name.starts_with("pg_") {
                    let mut info = ErrorInfo::new(
                        "ERROR".into(),
                        "42939".into(),
                        format!("unacceptable tablespace name \"{name}\""),
                    );
                    info.detail =
                        Some("The prefix \"pg_\" is reserved for system tablespaces.".into());
                    return Err(PgWireError::UserError(Box::new(info)));
                }
                if !location.starts_with('/') {
                    return Err(Self::user_error(
                        "42P17",
                        "tablespace location must be an absolute path".into(),
                    ));
                }
                if !std::path::Path::new(&location).is_dir() {
                    return Err(Self::user_error(
                        "58P01",
                        format!("directory \"{location}\" does not exist"),
                    ));
                }
                if self.catalog_doc(TABLESPACE_COLLECTION, &name).is_some() {
                    return Err(Self::user_error(
                        "42710",
                        format!("tablespace \"{name}\" already exists"),
                    ));
                }
                let owner = match owner {
                    Some(o) => self.grantee_name(&o)?,
                    None => self.current_role_name(),
                };
                let doc = bson::doc! { "_id": &name, "location": location, "owner": owner };
                self.put(TABLESPACE_COLLECTION, &name, doc)?;
                done()
            }
            CatalogOp::DropTablespace { name, if_exists } => {
                if self.transaction_handle_open() {
                    return Err(Self::user_error(
                        "25001",
                        "DROP TABLESPACE cannot run inside a transaction block".into(),
                    ));
                }
                if self.catalog_doc(TABLESPACE_COLLECTION, &name).is_none() {
                    let what = format!("tablespace \"{name}\"");
                    if if_exists {
                        self.missing_notice(what);
                        return done();
                    }
                    return Err(Self::user_error("42704", format!("{what} does not exist")));
                }
                self.delete_type_doc(TABLESPACE_COLLECTION, &name)?;
                done()
            }
            CatalogOp::CreatePublication {
                name,
                all_tables,
                tables,
                publish,
                via_root,
            } => {
                if self.catalog_doc(PUBLICATION_COLLECTION, &name).is_some() {
                    return Err(Self::user_error(
                        "42710",
                        format!("publication \"{name}\" already exists"),
                    ));
                }
                let role = self.current_role_name();
                if all_tables && !self.is_superuser(&role) {
                    return Err(Self::user_error(
                        "42501",
                        "must be superuser to create FOR ALL TABLES publication".into(),
                    ));
                }
                let doc = bson::doc! {
                    "_id": &name,
                    "all_tables": all_tables,
                    "tables": pub_tables_bson(&tables),
                    "publish": publish.unwrap_or_else(|| "insert, update, delete, truncate".into()),
                    "via_root": via_root.unwrap_or(false),
                    "owner": role,
                };
                self.put(PUBLICATION_COLLECTION, &name, doc)?;
                done()
            }
            CatalogOp::AlterPublication {
                name,
                action,
                tables,
                publish,
                via_root,
            } => {
                let Some(mut doc) = self.catalog_doc(PUBLICATION_COLLECTION, &name) else {
                    return Err(Self::user_error(
                        "42704",
                        format!("publication \"{name}\" does not exist"),
                    ));
                };
                if let Some(p) = publish {
                    doc.insert("publish", p);
                }
                if let Some(v) = via_root {
                    doc.insert("via_root", v);
                }
                if !tables.is_empty() || action == PubAction::Set {
                    if doc.get_bool("all_tables").unwrap_or(false) && !tables.is_empty() {
                        let mut info = ErrorInfo::new(
                            "ERROR".into(),
                            "55000".into(),
                            format!("publication \"{name}\" is defined as FOR ALL TABLES"),
                        );
                        info.detail = Some(
                            "Tables cannot be added to or dropped from FOR ALL TABLES publications."
                                .into(),
                        );
                        return Err(PgWireError::UserError(Box::new(info)));
                    }
                    let mut current = pub_tables(&doc);
                    match action {
                        PubAction::Add => {
                            for t in tables {
                                if current.iter().any(|c| c.table == t.table) {
                                    return Err(Self::user_error(
                                        "42710",
                                        format!(
                                            "relation \"{}\" is already member of publication \"{name}\"",
                                            t.table
                                        ),
                                    ));
                                }
                                current.push(t);
                            }
                        }
                        PubAction::Drop => {
                            for t in tables {
                                if !current.iter().any(|c| c.table == t.table) {
                                    return Err(Self::user_error(
                                        "42704",
                                        format!(
                                            "relation \"{}\" is not part of the publication",
                                            t.table
                                        ),
                                    ));
                                }
                                current.retain(|c| c.table != t.table);
                            }
                        }
                        PubAction::Set => current = tables,
                    }
                    doc.insert("tables", pub_tables_bson(&current));
                }
                self.put(PUBLICATION_COLLECTION, &name, doc)?;
                done()
            }
            CatalogOp::DropPublication { names, if_exists } => {
                // Every name is checked before any is dropped.
                if !if_exists {
                    if let Some(missing) = names
                        .iter()
                        .find(|n| self.catalog_doc(PUBLICATION_COLLECTION, n).is_none())
                    {
                        return Err(Self::user_error(
                            "42704",
                            format!("publication \"{missing}\" does not exist"),
                        ));
                    }
                }
                for name in &names {
                    if self.catalog_doc(PUBLICATION_COLLECTION, name).is_none() {
                        let what = format!("publication \"{name}\"");
                        if if_exists {
                            self.missing_notice(what);
                            continue;
                        }
                        return Err(Self::user_error("42704", format!("{what} does not exist")));
                    }
                    self.delete_type_doc(PUBLICATION_COLLECTION, name)?;
                }
                done()
            }
        }
    }

    /// The catalogs these statements populate.
    pub(crate) fn catalog_object_table(name: &str) -> Option<TableDef> {
        let c = |n: &str, t: &str| Column::new(n, t, false);
        Some(match name {
            "pg_statistic_ext" => TableDef::new(
                "pg_statistic_ext",
                vec![
                    c("oid", "oid"),
                    c("stxrelid", "oid"),
                    c("stxname", "name"),
                    c("stxnamespace", "oid"),
                    c("stxowner", "oid"),
                    c("stxstattarget", "int4"),
                    c("stxkeys", "int2vector"),
                    c("stxkind", "text[]"),
                    c("stxexprs", "text"),
                ],
            ),
            "pg_tablespace" => TableDef::new(
                "pg_tablespace",
                vec![
                    c("oid", "oid"),
                    c("spcname", "name"),
                    c("spcowner", "oid"),
                    c("spcacl", "text[]"),
                    c("spcoptions", "text[]"),
                ],
            ),
            "pg_publication" => TableDef::new(
                "pg_publication",
                vec![
                    c("oid", "oid"),
                    c("pubname", "name"),
                    c("pubowner", "oid"),
                    c("puballtables", "bool"),
                    c("pubinsert", "bool"),
                    c("pubupdate", "bool"),
                    c("pubdelete", "bool"),
                    c("pubtruncate", "bool"),
                    c("pubviaroot", "bool"),
                ],
            ),
            "pg_publication_rel" => TableDef::new(
                "pg_publication_rel",
                vec![
                    c("oid", "oid"),
                    c("prpubid", "oid"),
                    c("prrelid", "oid"),
                    c("prqual", "text"),
                    c("prattrs", "int2vector"),
                ],
            ),
            "pg_publication_tables" => TableDef::new(
                "pg_publication_tables",
                vec![
                    c("pubname", "name"),
                    c("schemaname", "name"),
                    c("tablename", "name"),
                    c("attnames", "name[]"),
                    c("rowfilter", "text"),
                ],
            ),
            _ => return None,
        })
    }

    fn role_oid_of(&self, name: &str) -> i64 {
        self.role(name).ok().flatten().map_or(10, |r| r.oid)
    }

    pub(crate) fn catalog_object_rows(&self, name: &str, def: &TableDef) -> Option<Vec<Document>> {
        let f = |n: &str| def.field_of(n).expect("column");
        let oid = |kind: &str, id: &str| Bson::Int64(Self::index_oid(&format!("{kind}:{id}")));
        let rows = match name {
            "pg_statistic_ext" => self
                .catalog_docs(STATISTICS_COLLECTION)
                .iter()
                .map(|d| {
                    let id = d.get_str("_id").unwrap_or_default();
                    let table = d.get_str("table").unwrap_or_default();
                    let tdef = self.lookup(table);
                    let keys: Vec<Bson> = strings(d, "columns")
                        .iter()
                        .filter_map(|c| {
                            tdef.as_ref()
                                .and_then(|t| t.columns.iter().position(|x| &x.name == c))
                                .map(|p| Bson::Int32(p as i32 + 1))
                        })
                        .collect();
                    let exprs = strings(d, "exprs");
                    let mut r = Document::new();
                    r.insert(f("oid"), oid("stx", id));
                    r.insert(
                        f("stxrelid"),
                        Bson::Int64(self.relation_oid(table).unwrap_or(0)),
                    );
                    r.insert(f("stxname"), id);
                    r.insert(f("stxnamespace"), Bson::Int64(Self::PUBLIC_NAMESPACE_OID));
                    r.insert(
                        f("stxowner"),
                        Bson::Int64(self.role_oid_of(d.get_str("owner").unwrap_or_default())),
                    );
                    r.insert(f("stxstattarget"), Bson::Int32(-1));
                    r.insert(f("stxkeys"), Bson::Array(keys));
                    r.insert(
                        f("stxkind"),
                        Bson::Array(strings(d, "kinds").into_iter().map(Bson::String).collect()),
                    );
                    r.insert(
                        f("stxexprs"),
                        if exprs.is_empty() {
                            Bson::Null
                        } else {
                            Bson::String(exprs.join(", "))
                        },
                    );
                    r
                })
                .collect(),
            "pg_tablespace" => {
                let mut rows = Vec::new();
                for (o, n) in [(PG_DEFAULT_OID, "pg_default"), (PG_GLOBAL_OID, "pg_global")] {
                    let mut r = Document::new();
                    r.insert(f("oid"), Bson::Int64(o));
                    r.insert(f("spcname"), n);
                    r.insert(f("spcowner"), Bson::Int64(10));
                    r.insert(f("spcacl"), Bson::Null);
                    r.insert(f("spcoptions"), Bson::Null);
                    rows.push(r);
                }
                for d in self.catalog_docs(TABLESPACE_COLLECTION) {
                    let id = d.get_str("_id").unwrap_or_default();
                    let mut r = Document::new();
                    r.insert(f("oid"), oid("spc", id));
                    r.insert(f("spcname"), id);
                    r.insert(
                        f("spcowner"),
                        Bson::Int64(self.role_oid_of(d.get_str("owner").unwrap_or_default())),
                    );
                    r.insert(f("spcacl"), Bson::Null);
                    r.insert(f("spcoptions"), Bson::Null);
                    rows.push(r);
                }
                rows
            }
            "pg_publication" => self
                .catalog_docs(PUBLICATION_COLLECTION)
                .iter()
                .map(|d| {
                    let id = d.get_str("_id").unwrap_or_default();
                    let publish = d.get_str("publish").unwrap_or_default();
                    let has = |w: &str| publish.split(',').any(|p| p.trim() == w);
                    let mut r = Document::new();
                    r.insert(f("oid"), oid("pub", id));
                    r.insert(f("pubname"), id);
                    r.insert(
                        f("pubowner"),
                        Bson::Int64(self.role_oid_of(d.get_str("owner").unwrap_or_default())),
                    );
                    r.insert(f("puballtables"), d.get_bool("all_tables").unwrap_or(false));
                    r.insert(f("pubinsert"), has("insert"));
                    r.insert(f("pubupdate"), has("update"));
                    r.insert(f("pubdelete"), has("delete"));
                    r.insert(f("pubtruncate"), has("truncate"));
                    r.insert(f("pubviaroot"), d.get_bool("via_root").unwrap_or(false));
                    r
                })
                .collect(),
            "pg_publication_rel" => self
                .catalog_docs(PUBLICATION_COLLECTION)
                .iter()
                .flat_map(|d| {
                    let id = d.get_str("_id").unwrap_or_default().to_string();
                    pub_tables(d).into_iter().map(move |t| (id.clone(), t))
                })
                .map(|(id, t)| {
                    let tdef = self.lookup(&t.table);
                    let attrs: Vec<Bson> = t
                        .columns
                        .iter()
                        .filter_map(|c| {
                            tdef.as_ref()
                                .and_then(|d| d.columns.iter().position(|x| &x.name == c))
                                .map(|p| Bson::Int32(p as i32 + 1))
                        })
                        .collect();
                    let mut r = Document::new();
                    r.insert(f("oid"), oid("pubrel", &format!("{id}.{}", t.table)));
                    r.insert(f("prpubid"), oid("pub", &id));
                    r.insert(
                        f("prrelid"),
                        Bson::Int64(self.relation_oid(&t.table).unwrap_or(0)),
                    );
                    r.insert(f("prqual"), t.row_filter.map_or(Bson::Null, Bson::String));
                    r.insert(
                        f("prattrs"),
                        if attrs.is_empty() {
                            Bson::Null
                        } else {
                            Bson::Array(attrs)
                        },
                    );
                    r
                })
                .collect(),
            "pg_publication_tables" => {
                let all: Vec<String> = self
                    .all_table_defs()
                    .ok()?
                    .into_iter()
                    .map(|t| t.name)
                    .collect();
                let mut rows = Vec::new();
                for d in self.catalog_docs(PUBLICATION_COLLECTION) {
                    let id = d.get_str("_id").unwrap_or_default();
                    let tables: Vec<PubTable> = if d.get_bool("all_tables").unwrap_or(false) {
                        all.iter()
                            .map(|t| PubTable {
                                table: t.clone(),
                                columns: Vec::new(),
                                row_filter: None,
                            })
                            .collect()
                    } else {
                        pub_tables(&d)
                    };
                    for t in tables {
                        let cols: Vec<String> = if t.columns.is_empty() {
                            self.lookup(&t.table)
                                .map(|d| d.columns.iter().map(|c| c.name.clone()).collect())
                                .unwrap_or_default()
                        } else {
                            t.columns.clone()
                        };
                        let mut r = Document::new();
                        r.insert(f("pubname"), id);
                        r.insert(f("schemaname"), "public");
                        r.insert(f("tablename"), t.table.as_str());
                        r.insert(
                            f("attnames"),
                            Bson::Array(cols.into_iter().map(Bson::String).collect()),
                        );
                        r.insert(
                            f("rowfilter"),
                            t.row_filter.map_or(Bson::Null, Bson::String),
                        );
                        rows.push(r);
                    }
                }
                rows
            }
            _ => return None,
        };
        Some(rows)
    }
}
