//! Foreign data wrappers, servers, user mappings and foreign tables (see
//! `secantus_pgplan::fdw`), stored in `__sql_fdw__`. A foreign table is a
//! table whose catalog row carries `foreign_server` / `foreign_wrapper` /
//! `foreign_options`; the planner refuses to read or write one.

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgcatalog::{Column, TableDef};
use secantus_pgplan::fdw::{apply_option_edits, FdwKind, FdwOp, OptionEdit};
use secantus_pgplan::Statement;

use crate::PgHandler;

pub(crate) const FDW_COLLECTION: &str = "__sql_fdw__";

fn strings(d: &Document, key: &str) -> Vec<String> {
    d.get_array(key)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn options_bson(options: &[String]) -> Bson {
    Bson::Array(options.iter().cloned().map(Bson::String).collect())
}

/// Stored options as a catalog column: NULL when there are none.
fn options_column(d: &Document) -> Bson {
    let o = strings(d, "options");
    if o.is_empty() {
        Bson::Null
    } else {
        options_bson(&o)
    }
}

thread_local! {
    /// A `DROP FOREIGN TABLE` is running: its table drop may take one.
    static DROPPING_FOREIGN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Refuse a plain `DROP TABLE` of a foreign table.
pub(crate) fn check_drop_table(def: &TableDef) -> PgWireResult<()> {
    if secantus_pgplan::fdw::is_foreign(def) && !DROPPING_FOREIGN.with(|d| d.get()) {
        let mut info = ErrorInfo::new(
            "ERROR".into(),
            "42809".into(),
            format!("\"{}\" is not a table", def.name),
        );
        info.hint = Some("Use DROP FOREIGN TABLE to remove a foreign table.".into());
        return Err(PgWireError::UserError(Box::new(info)));
    }
    Ok(())
}

impl PgHandler {
    fn fdw_docs(&self, kind: &str) -> Vec<Document> {
        self.type_catalog_docs(FDW_COLLECTION)
            .map(|d| {
                d.iter()
                    .filter(|d| d.get_str("kind") == Ok(kind))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn fdw_doc(&self, kind: &str, name: &str) -> Option<Document> {
        self.fdw_docs(kind)
            .into_iter()
            .find(|d| d.get_str("name") == Ok(name))
    }

    fn wrapper_missing(name: &str) -> PgWireError {
        Self::user_error(
            "42704",
            format!("foreign-data wrapper \"{name}\" does not exist"),
        )
    }

    fn server_missing(name: &str) -> PgWireError {
        Self::user_error("42704", format!("server \"{name}\" does not exist"))
    }

    fn fdw_oid(kind: &str, name: &str) -> i64 {
        Self::index_oid(&format!("fdw:{kind}:{name}"))
    }

    /// A user mapping's user as stored: a role name, or `public`.
    fn mapping_user(&self, user: &str) -> PgWireResult<String> {
        Ok(match user {
            "CURRENT_USER" | "CURRENT_ROLE" => self.current_role_name(),
            "SESSION_USER" => self.session_user_name(),
            "PUBLIC" => "public".to_string(),
            other => {
                if self.role(other)?.is_none() && other != self.session_user_name() {
                    return Err(Self::user_error(
                        "42704",
                        format!("role \"{other}\" does not exist"),
                    ));
                }
                other.to_string()
            }
        })
    }

    fn put_fdw(&self, doc: Document) -> PgWireResult<()> {
        let id = doc.get_str("_id").unwrap_or_default().to_string();
        self.put(FDW_COLLECTION, &id, doc)
    }

    fn edit_options(&self, doc: &mut Document, edits: &[OptionEdit]) -> PgWireResult<()> {
        let options =
            apply_option_edits(&strings(doc, "options"), edits).map_err(|e| Self::err(&e))?;
        doc.insert("options", options_bson(&options));
        Ok(())
    }

    /// The objects that depend on server `name`, as `DETAIL` lines.
    fn server_dependents(&self, name: &str) -> PgWireResult<(Vec<String>, Vec<String>)> {
        let mut lines = Vec::new();
        for m in self.fdw_docs("mapping") {
            if m.get_str("server") == Ok(name) {
                lines.push(format!(
                    "user mapping for {} on server {name} depends on server {name}",
                    m.get_str("user").unwrap_or_default()
                ));
            }
        }
        let mut tables = Vec::new();
        for t in self.all_table_defs()? {
            if t.extra.get_str("foreign_server") == Ok(name) {
                lines.push(format!("foreign table {} depends on server {name}", t.name));
                tables.push(t.name.clone());
            }
        }
        Ok((lines, tables))
    }

    fn dependents_error(what: &str, name: &str, lines: &[String]) -> PgWireError {
        let mut info = ErrorInfo::new(
            "ERROR".into(),
            "2BP01".into(),
            format!("cannot drop {what} {name} because other objects depend on it"),
        );
        info.detail = Some(lines.join("\n"));
        info.hint = Some("Use DROP ... CASCADE to drop the dependent objects too.".into());
        PgWireError::UserError(Box::new(info))
    }

    fn drop_server_cascade(&self, name: &str, tables: &[String]) -> PgWireResult<()> {
        for t in tables {
            DROPPING_FOREIGN.with(|d| d.set(true));
            let out = self.execute(
                Statement::DropTable(secantus_pgplan::DropTable {
                    tables: vec![t.clone()],
                    if_exists: true,
                    cascade: true,
                }),
                0,
            );
            DROPPING_FOREIGN.with(|d| d.set(false));
            out?;
        }
        for m in self.fdw_docs("mapping") {
            if m.get_str("server") == Ok(name) {
                self.delete_type_doc(FDW_COLLECTION, m.get_str("_id").unwrap_or_default())?;
            }
        }
        self.delete_type_doc(FDW_COLLECTION, &format!("s:{name}"))
    }

    pub(crate) fn execute_fdw(&self, op: FdwOp) -> PgWireResult<Vec<Response>> {
        let tag = op.tag();
        let done = || Ok(vec![Response::Execution(Tag::new(tag))]);
        match op {
            FdwOp::CreateWrapper {
                name,
                handler,
                validator,
                options,
            } => {
                let role = self.current_role_name();
                if !self.is_superuser(&role) {
                    let mut info = ErrorInfo::new(
                        "ERROR".into(),
                        "42501".into(),
                        format!("permission denied to create foreign-data wrapper \"{name}\""),
                    );
                    info.hint = Some("Must be superuser to create a foreign-data wrapper.".into());
                    return Err(PgWireError::UserError(Box::new(info)));
                }
                if self.fdw_doc("wrapper", &name).is_some() {
                    return Err(Self::user_error(
                        "42710",
                        format!("foreign-data wrapper \"{name}\" already exists"),
                    ));
                }
                let functions = self.user_function_docs()?;
                if let Some(h) = &handler {
                    let Some(f) = functions
                        .iter()
                        .find(|d| d.get_str("name") == Ok(h) && d.get_i32("nargs") == Ok(0))
                    else {
                        return Err(Self::user_error(
                            "42883",
                            format!("function {h}() does not exist"),
                        ));
                    };
                    if f.get_str("return_tag") != Ok("fdw_handler") {
                        return Err(Self::user_error(
                            "42809",
                            format!("function {h} must return type fdw_handler"),
                        ));
                    }
                }
                if let Some(v) = &validator {
                    if !functions
                        .iter()
                        .any(|d| d.get_str("name") == Ok(v) && d.get_i32("nargs") == Ok(2))
                    {
                        return Err(Self::user_error(
                            "42883",
                            format!("function {v}(text[], oid) does not exist"),
                        ));
                    }
                }
                let mut doc = bson::doc! {
                    "_id": format!("w:{name}"),
                    "kind": "wrapper",
                    "name": &name,
                    "owner": role,
                    "options": options_bson(&options),
                };
                if let Some(h) = handler {
                    doc.insert("handler", h);
                }
                if let Some(v) = validator {
                    doc.insert("validator", v);
                }
                self.put_fdw(doc)?;
                done()
            }
            FdwOp::AlterWrapper { name, options } => {
                let mut doc = self
                    .fdw_doc("wrapper", &name)
                    .ok_or_else(|| Self::wrapper_missing(&name))?;
                self.edit_options(&mut doc, &options)?;
                self.put_fdw(doc)?;
                done()
            }
            FdwOp::CreateServer {
                name,
                wrapper,
                server_type,
                version,
                options,
                if_not_exists,
            } => {
                if self.fdw_doc("server", &name).is_some() {
                    let message = format!("server \"{name}\" already exists");
                    if if_not_exists {
                        self.notice("42710", format!("{message}, skipping"), None);
                        return done();
                    }
                    return Err(Self::user_error("42710", message));
                }
                if self.fdw_doc("wrapper", &wrapper).is_none() {
                    return Err(Self::wrapper_missing(&wrapper));
                }
                let mut doc = bson::doc! {
                    "_id": format!("s:{name}"),
                    "kind": "server",
                    "name": &name,
                    "wrapper": wrapper,
                    "owner": self.current_role_name(),
                    "options": options_bson(&options),
                };
                if let Some(t) = server_type {
                    doc.insert("type", t);
                }
                if let Some(v) = version {
                    doc.insert("version", v);
                }
                self.put_fdw(doc)?;
                done()
            }
            FdwOp::AlterServer {
                name,
                version,
                options,
            } => {
                let mut doc = self
                    .fdw_doc("server", &name)
                    .ok_or_else(|| Self::server_missing(&name))?;
                match version {
                    Some(Some(v)) => {
                        doc.insert("version", v);
                    }
                    Some(None) => {
                        doc.remove("version");
                    }
                    None => {}
                }
                self.edit_options(&mut doc, &options)?;
                self.put_fdw(doc)?;
                done()
            }
            FdwOp::CreateUserMapping {
                user,
                server,
                options,
                if_not_exists,
            } => {
                let user = self.mapping_user(&user)?;
                if self.fdw_doc("server", &server).is_none() {
                    return Err(Self::server_missing(&server));
                }
                let id = format!("u:{server}/{user}");
                if self
                    .fdw_docs("mapping")
                    .iter()
                    .any(|d| d.get_str("_id") == Ok(id.as_str()))
                {
                    let message = format!(
                        "user mapping for \"{user}\" already exists for server \"{server}\""
                    );
                    if if_not_exists {
                        self.notice("42710", format!("{message}, skipping"), None);
                        return done();
                    }
                    return Err(Self::user_error("42710", message));
                }
                self.put_fdw(bson::doc! {
                    "_id": &id,
                    "kind": "mapping",
                    "name": &id,
                    "user": user,
                    "server": server,
                    "options": options_bson(&options),
                })?;
                done()
            }
            FdwOp::AlterUserMapping {
                user,
                server,
                options,
            } => {
                let user = self.mapping_user(&user)?;
                if self.fdw_doc("server", &server).is_none() {
                    return Err(Self::server_missing(&server));
                }
                let id = format!("u:{server}/{user}");
                let mut doc = self
                    .fdw_docs("mapping")
                    .into_iter()
                    .find(|d| d.get_str("_id") == Ok(id.as_str()))
                    .ok_or_else(|| {
                        Self::user_error(
                            "42704",
                            format!("user mapping for \"{user}\" does not exist for server \"{server}\""),
                        )
                    })?;
                self.edit_options(&mut doc, &options)?;
                self.put_fdw(doc)?;
                done()
            }
            FdwOp::DropUserMapping {
                user,
                server,
                if_exists,
            } => {
                let user = self.mapping_user(&user)?;
                if self.fdw_doc("server", &server).is_none() {
                    if if_exists {
                        self.notice(
                            "00000",
                            format!("server \"{server}\" does not exist, skipping"),
                            None,
                        );
                        return done();
                    }
                    return Err(Self::server_missing(&server));
                }
                let id = format!("u:{server}/{user}");
                if !self
                    .fdw_docs("mapping")
                    .iter()
                    .any(|d| d.get_str("_id") == Ok(id.as_str()))
                {
                    let message = format!(
                        "user mapping for \"{user}\" does not exist for server \"{server}\""
                    );
                    if if_exists {
                        self.notice("00000", format!("{message}, skipping"), None);
                        return done();
                    }
                    return Err(Self::user_error("42704", message));
                }
                self.delete_type_doc(FDW_COLLECTION, &id)?;
                done()
            }
            FdwOp::Drop {
                kind,
                names,
                if_exists,
                cascade,
            } => {
                let (word, doc_kind) = match kind {
                    FdwKind::Wrapper => ("foreign-data wrapper", "wrapper"),
                    FdwKind::Server => ("server", "server"),
                };
                for name in &names {
                    if self.fdw_doc(doc_kind, name).is_none() {
                        if if_exists {
                            self.notice(
                                "00000",
                                format!("{word} \"{name}\" does not exist, skipping"),
                                None,
                            );
                            continue;
                        }
                        return Err(match kind {
                            FdwKind::Wrapper => Self::wrapper_missing(name),
                            FdwKind::Server => Self::server_missing(name),
                        });
                    }
                    let servers: Vec<String> = match kind {
                        FdwKind::Server => vec![name.clone()],
                        FdwKind::Wrapper => Vec::new(),
                    };
                    let mut lines = Vec::new();
                    let mut plan = Vec::new();
                    if kind == FdwKind::Wrapper {
                        for s in self.fdw_docs("server") {
                            if s.get_str("wrapper") == Ok(name.as_str()) {
                                let sname = s.get_str("name").unwrap_or_default().to_string();
                                lines.push(format!(
                                    "server {sname} depends on foreign-data wrapper {name}"
                                ));
                                let (more, tables) = self.server_dependents(&sname)?;
                                lines.extend(more);
                                plan.push((sname, tables));
                            }
                        }
                    }
                    for s in servers {
                        let (more, tables) = self.server_dependents(&s)?;
                        lines.extend(more);
                        plan.push((s, tables));
                    }
                    let has_dependents = match kind {
                        FdwKind::Wrapper => !plan.is_empty(),
                        FdwKind::Server => {
                            plan.iter().any(|(_, t)| !t.is_empty()) || !lines.is_empty()
                        }
                    };
                    if has_dependents && !cascade {
                        return Err(Self::dependents_error(word, name, &lines));
                    }
                    if has_dependents {
                        let descs: Vec<String> = lines
                            .iter()
                            .map(|l| {
                                l.split(" depends on ")
                                    .next()
                                    .unwrap_or_default()
                                    .to_string()
                            })
                            .collect();
                        self.cascade_notice(&descs);
                    }
                    for (s, tables) in plan {
                        self.drop_server_cascade(&s, &tables)?;
                    }
                    if kind == FdwKind::Wrapper {
                        self.delete_type_doc(FDW_COLLECTION, &format!("w:{name}"))?;
                    }
                }
                done()
            }
            FdwOp::CreateForeignTable {
                create,
                table,
                server,
                options,
            } => {
                let Some(s) = self.fdw_doc("server", &server) else {
                    return Err(Self::server_missing(&server));
                };
                self.execute(*create, 0)?;
                if let Some(mut def) = self.lookup(&table) {
                    def.extra.insert("foreign_server", server);
                    def.extra
                        .insert("foreign_wrapper", s.get_str("wrapper").unwrap_or_default());
                    def.extra.insert("foreign_options", options_bson(&options));
                    self.rewrite_catalog(&table, &def)?;
                }
                done()
            }
            FdwOp::AlterForeignTable { alter, .. } => {
                self.execute(*alter, 0)?;
                done()
            }
            FdwOp::DropForeignTables { drop, .. } => {
                DROPPING_FOREIGN.with(|d| d.set(true));
                let out = self.execute(*drop, 0);
                DROPPING_FOREIGN.with(|d| d.set(false));
                out?;
                done()
            }
            FdwOp::ImportForeignSchema { server } => {
                let Some(s) = self.fdw_doc("server", &server) else {
                    return Err(Self::server_missing(&server));
                };
                Err(Self::user_error(
                    "55000",
                    format!(
                        "foreign-data wrapper \"{}\" has no handler",
                        s.get_str("wrapper").unwrap_or_default()
                    ),
                ))
            }
        }
    }

    pub(crate) fn fdw_catalog_rows(&self, name: &str, def: &TableDef) -> Option<Vec<Document>> {
        let f = |c: &str| def.field_of(c).expect("column");
        let db = self.db().to_string();
        let opt =
            |d: &Document, k: &str| d.get_str(k).map_or(Bson::Null, |s| Bson::String(s.into()));
        let rows = match name {
            "pg_foreign_data_wrapper" => self
                .fdw_docs("wrapper")
                .iter()
                .map(|w| {
                    let n = w.get_str("name").unwrap_or_default();
                    let mut r = Document::new();
                    r.insert(f("oid"), Bson::Int64(Self::fdw_oid("w", n)));
                    r.insert(f("fdwname"), n);
                    r.insert(
                        f("fdwowner"),
                        Bson::Int64(self.role_oid_of(w.get_str("owner").unwrap_or_default())),
                    );
                    r.insert(f("fdwhandler"), Bson::Int64(0));
                    r.insert(f("fdwvalidator"), Bson::Int64(0));
                    r.insert(f("fdwacl"), Bson::Null);
                    r.insert(f("fdwoptions"), options_column(w));
                    r
                })
                .collect(),
            "pg_foreign_server" => self
                .fdw_docs("server")
                .iter()
                .map(|s| {
                    let n = s.get_str("name").unwrap_or_default();
                    let mut r = Document::new();
                    r.insert(f("oid"), Bson::Int64(Self::fdw_oid("s", n)));
                    r.insert(f("srvname"), n);
                    r.insert(
                        f("srvowner"),
                        Bson::Int64(self.role_oid_of(s.get_str("owner").unwrap_or_default())),
                    );
                    r.insert(
                        f("srvfdw"),
                        Bson::Int64(Self::fdw_oid("w", s.get_str("wrapper").unwrap_or_default())),
                    );
                    r.insert(f("srvtype"), opt(s, "type"));
                    r.insert(f("srvversion"), opt(s, "version"));
                    r.insert(f("srvacl"), Bson::Null);
                    r.insert(f("srvoptions"), options_column(s));
                    r
                })
                .collect(),
            "pg_user_mapping" | "pg_user_mappings" => self
                .fdw_docs("mapping")
                .iter()
                .map(|m| {
                    let id = m.get_str("_id").unwrap_or_default();
                    let server = m.get_str("server").unwrap_or_default();
                    let user = m.get_str("user").unwrap_or_default();
                    let user_oid = if user == "public" {
                        0
                    } else {
                        self.role_oid_of(user)
                    };
                    let mut r = Document::new();
                    if name == "pg_user_mapping" {
                        r.insert(f("oid"), Bson::Int64(Self::fdw_oid("u", id)));
                        r.insert(f("umuser"), Bson::Int64(user_oid));
                        r.insert(f("umserver"), Bson::Int64(Self::fdw_oid("s", server)));
                    } else {
                        r.insert(f("umid"), Bson::Int64(Self::fdw_oid("u", id)));
                        r.insert(f("srvid"), Bson::Int64(Self::fdw_oid("s", server)));
                        r.insert(f("srvname"), server);
                        r.insert(f("umuser"), Bson::Int64(user_oid));
                        r.insert(f("usename"), user);
                    }
                    r.insert(f("umoptions"), options_column(m));
                    r
                })
                .collect(),
            "pg_foreign_table" => self
                .all_table_defs()
                .ok()?
                .iter()
                .filter(|t| secantus_pgplan::fdw::is_foreign(t))
                .map(|t| {
                    let options = t
                        .extra
                        .get_array("foreign_options")
                        .ok()
                        .filter(|a| !a.is_empty())
                        .cloned()
                        .map_or(Bson::Null, Bson::Array);
                    let mut r = Document::new();
                    r.insert(
                        f("ftrelid"),
                        Bson::Int64(self.relation_oid(&t.name).unwrap_or(0)),
                    );
                    r.insert(
                        f("ftserver"),
                        Bson::Int64(Self::fdw_oid(
                            "s",
                            t.extra.get_str("foreign_server").unwrap_or_default(),
                        )),
                    );
                    r.insert(f("ftoptions"), options);
                    r
                })
                .collect(),
            "information_schema.foreign_data_wrappers" => self
                .fdw_docs("wrapper")
                .iter()
                .map(|w| {
                    let mut r = Document::new();
                    r.insert(f("foreign_data_wrapper_catalog"), db.as_str());
                    r.insert(
                        f("foreign_data_wrapper_name"),
                        w.get_str("name").unwrap_or_default(),
                    );
                    r.insert(
                        f("authorization_identifier"),
                        w.get_str("owner").unwrap_or_default(),
                    );
                    r.insert(f("library_name"), Bson::Null);
                    r.insert(f("foreign_data_wrapper_language"), "c");
                    r
                })
                .collect(),
            "information_schema.foreign_servers" => self
                .fdw_docs("server")
                .iter()
                .map(|s| {
                    let mut r = Document::new();
                    r.insert(f("foreign_server_catalog"), db.as_str());
                    r.insert(
                        f("foreign_server_name"),
                        s.get_str("name").unwrap_or_default(),
                    );
                    r.insert(f("foreign_data_wrapper_catalog"), db.as_str());
                    r.insert(
                        f("foreign_data_wrapper_name"),
                        s.get_str("wrapper").unwrap_or_default(),
                    );
                    r.insert(f("foreign_server_type"), opt(s, "type"));
                    r.insert(f("foreign_server_version"), opt(s, "version"));
                    r.insert(
                        f("authorization_identifier"),
                        s.get_str("owner").unwrap_or_default(),
                    );
                    r
                })
                .collect(),
            "information_schema.foreign_tables" => self
                .all_table_defs()
                .ok()?
                .iter()
                .filter(|t| secantus_pgplan::fdw::is_foreign(t))
                .map(|t| {
                    let mut r = Document::new();
                    r.insert(f("foreign_table_catalog"), db.as_str());
                    r.insert(f("foreign_table_schema"), "public");
                    r.insert(f("foreign_table_name"), t.name.as_str());
                    r.insert(f("foreign_server_catalog"), db.as_str());
                    r.insert(
                        f("foreign_server_name"),
                        t.extra.get_str("foreign_server").unwrap_or_default(),
                    );
                    r
                })
                .collect(),
            _ => return None,
        };
        Some(rows)
    }
}

/// The FDW catalogs' definitions.
pub(crate) fn fdw_catalog_def(name: &str) -> Option<TableDef> {
    let col = |n: &str, t: &str| Column::new(n, t, false);
    let columns = match name {
        "pg_foreign_data_wrapper" => vec![
            col("oid", "oid"),
            col("fdwname", "name"),
            col("fdwowner", "oid"),
            col("fdwhandler", "oid"),
            col("fdwvalidator", "oid"),
            col("fdwacl", "aclitem[]"),
            col("fdwoptions", "text[]"),
        ],
        "pg_foreign_server" => vec![
            col("oid", "oid"),
            col("srvname", "name"),
            col("srvowner", "oid"),
            col("srvfdw", "oid"),
            col("srvtype", "text"),
            col("srvversion", "text"),
            col("srvacl", "aclitem[]"),
            col("srvoptions", "text[]"),
        ],
        "pg_user_mapping" => vec![
            col("oid", "oid"),
            col("umuser", "oid"),
            col("umserver", "oid"),
            col("umoptions", "text[]"),
        ],
        "pg_user_mappings" => vec![
            col("umid", "oid"),
            col("srvid", "oid"),
            col("srvname", "name"),
            col("umuser", "oid"),
            col("usename", "name"),
            col("umoptions", "text[]"),
        ],
        "pg_foreign_table" => vec![
            col("ftrelid", "oid"),
            col("ftserver", "oid"),
            col("ftoptions", "text[]"),
        ],
        "information_schema.foreign_data_wrappers" => vec![
            col("foreign_data_wrapper_catalog", "name"),
            col("foreign_data_wrapper_name", "name"),
            col("authorization_identifier", "name"),
            col("library_name", "varchar"),
            col("foreign_data_wrapper_language", "varchar"),
        ],
        "information_schema.foreign_servers" => vec![
            col("foreign_server_catalog", "name"),
            col("foreign_server_name", "name"),
            col("foreign_data_wrapper_catalog", "name"),
            col("foreign_data_wrapper_name", "name"),
            col("foreign_server_type", "varchar"),
            col("foreign_server_version", "varchar"),
            col("authorization_identifier", "name"),
        ],
        "information_schema.foreign_tables" => vec![
            col("foreign_table_catalog", "name"),
            col("foreign_table_schema", "name"),
            col("foreign_table_name", "name"),
            col("foreign_server_catalog", "name"),
            col("foreign_server_name", "name"),
        ],
        _ => return None,
    };
    Some(TableDef::new(name, columns))
}
