//! Event triggers: `CREATE` / `ALTER` / `DROP EVENT TRIGGER`, stored in
//! `__sql_event_triggers__` and listed in `pg_event_trigger`, and fired
//! around a DDL statement as PostgreSQL fires them:
//!
//! * `ddl_command_start` before the command runs;
//! * `sql_drop` after it, when it dropped something, with
//!   `pg_event_trigger_dropped_objects()` listing what;
//! * `ddl_command_end` last, with `pg_event_trigger_ddl_commands()` listing
//!   what the command created or altered.
//!
//! The functions run in the statement's transaction, so a trigger that raises
//! undoes the command. Only a top-level command fires them: the statements a
//! command runs internally (a `CREATE TABLE AS`'s INSERT) do not.

use std::cell::{Cell, RefCell};

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgcatalog::{Column, TableDef};
use secantus_pgplan::Statement;

use crate::plpgsql_fn::{self, TriggerData};
use crate::{PgHandler, PlHost};

pub(crate) const EVENT_TRIGGER_COLLECTION: &str = "__sql_event_triggers__";

/// One object a command dropped, as `pg_event_trigger_dropped_objects()`
/// reports it.
#[derive(Debug, Clone)]
pub(crate) struct Dropped {
    classid: i64,
    objid: i64,
    original: bool,
    normal: bool,
    object_type: &'static str,
    schema: Option<String>,
    name: Option<String>,
    identity: String,
    address_names: Vec<String>,
    address_args: Vec<String>,
}

/// One object a command created or altered, for
/// `pg_event_trigger_ddl_commands()`.
#[derive(Debug, Clone)]
pub(crate) struct DdlCommand {
    classid: i64,
    objid: i64,
    tag: String,
    object_type: &'static str,
    identity: String,
}

thread_local! {
    /// A command is running with its event triggers armed: the statements it
    /// runs internally do not fire them again.
    static IN_COMMAND: Cell<bool> = const { Cell::new(false) };
    static DROPPED: RefCell<Vec<Dropped>> = const { RefCell::new(Vec::new()) };
    static COMMANDS: RefCell<Vec<DdlCommand>> = const { RefCell::new(Vec::new()) };
}

const PG_CLASS: i64 = 1259;
const PG_TYPE: i64 = 1247;
const PG_PROC: i64 = 1255;
const PG_REWRITE: i64 = 2618;
const PG_CONSTRAINT: i64 = 2606;
const PG_ATTRDEF: i64 = 2604;
const PG_NAMESPACE: i64 = 2615;

/// A type as `format_type_be_qualified` prints it: the SQL-standard names
/// bare, every other built-in under `pg_catalog`, a user type under `public`.
fn qualified_type(name: &str) -> String {
    if let Some(elem) = name.strip_suffix("[]") {
        return format!("{}[]", qualified_type(elem));
    }
    let display = secantus_pgplan::display_type(name);
    let standard = matches!(
        display.as_str(),
        "bit"
            | "boolean"
            | "character"
            | "real"
            | "double precision"
            | "smallint"
            | "integer"
            | "bigint"
            | "numeric"
            | "interval"
            | "time without time zone"
            | "time with time zone"
            | "timestamp without time zone"
            | "timestamp with time zone"
            | "bit varying"
            | "character varying"
    );
    if standard {
        display
    } else if secantus_pgplan::pgtypes::oid_of_name(name).is_some() {
        format!("pg_catalog.{display}")
    } else {
        format!("public.{}", secantus_pgplan::scalar::quote_identifier(name))
    }
}

fn ident(name: &str) -> String {
    secantus_pgplan::scalar::quote_identifier(name)
}

impl Dropped {
    fn relation(object_type: &'static str, oid: i64, name: &str, original: bool) -> Self {
        Self {
            classid: PG_CLASS,
            objid: oid,
            original,
            normal: false,
            object_type,
            schema: Some("public".into()),
            name: Some(name.to_string()),
            identity: format!("public.{}", ident(name)),
            address_names: vec!["public".into(), name.to_string()],
            address_args: Vec::new(),
        }
    }

    /// A relation's row type and its array type.
    fn row_types(oid: i64, name: &str) -> [Self; 2] {
        let row = Self {
            classid: PG_TYPE,
            objid: oid,
            original: false,
            normal: false,
            object_type: "type",
            schema: Some("public".into()),
            name: Some(name.to_string()),
            identity: format!("public.{}", ident(name)),
            address_names: vec![format!("public.{}", ident(name))],
            address_args: Vec::new(),
        };
        let array = Self {
            objid: 0,
            name: Some(format!("_{name}")),
            identity: format!("public.{}[]", ident(name)),
            address_names: vec![format!("public.{}[]", ident(name))],
            ..row.clone()
        };
        [row, array]
    }
}

impl PgHandler {
    fn event_trigger_docs(&self) -> Vec<Document> {
        self.type_catalog_docs(EVENT_TRIGGER_COLLECTION)
            .map(|d| d.to_vec())
            .unwrap_or_default()
    }

    fn event_trigger_missing(name: &str) -> PgWireError {
        Self::user_error("42704", format!("event trigger \"{name}\" does not exist"))
    }

    pub(crate) fn create_event_trigger(
        &self,
        name: &str,
        event: &str,
        tags: Option<Vec<String>>,
        function: &str,
    ) -> PgWireResult<Vec<Response>> {
        let role = self.current_role_name();
        if !self.is_superuser(&role) {
            let mut info = ErrorInfo::new(
                "ERROR".into(),
                "42501".into(),
                format!("permission denied to create event trigger \"{name}\""),
            );
            info.hint = Some("Must be superuser to create an event trigger.".into());
            return Err(PgWireError::UserError(Box::new(info)));
        }
        if self
            .event_trigger_docs()
            .iter()
            .any(|d| d.get_str("_id") == Ok(name))
        {
            return Err(Self::user_error(
                "42710",
                format!("event trigger \"{name}\" already exists"),
            ));
        }
        let Some(f) = self
            .user_function_docs()?
            .into_iter()
            .find(|d| d.get_str("name") == Ok(function) && d.get_i32("nargs") == Ok(0))
        else {
            return Err(Self::user_error(
                "42883",
                format!("function {function}() does not exist"),
            ));
        };
        if f.get_str("return_tag") != Ok("event_trigger") {
            return Err(Self::user_error(
                "42P17",
                format!("function {function} must return type event_trigger"),
            ));
        }
        let mut doc = bson::doc! {
            "_id": name,
            "event": event,
            "function": function,
            "enabled": "O",
            "owner": role,
            "oid": Self::index_oid(&format!("evt:{name}")),
        };
        if let Some(tags) = tags {
            doc.insert(
                "tags",
                tags.into_iter().map(Bson::String).collect::<Vec<_>>(),
            );
        }
        self.put(EVENT_TRIGGER_COLLECTION, name, doc)?;
        Ok(vec![Response::Execution(Tag::new("CREATE EVENT TRIGGER"))])
    }

    pub(crate) fn alter_event_trigger(
        &self,
        name: &str,
        enabled: &str,
    ) -> PgWireResult<Vec<Response>> {
        let Some(mut doc) = self
            .event_trigger_docs()
            .into_iter()
            .find(|d| d.get_str("_id") == Ok(name))
        else {
            return Err(Self::event_trigger_missing(name));
        };
        doc.insert("enabled", enabled);
        self.put(EVENT_TRIGGER_COLLECTION, name, doc)?;
        Ok(vec![Response::Execution(Tag::new("ALTER EVENT TRIGGER"))])
    }

    pub(crate) fn drop_event_triggers(
        &self,
        names: &[String],
        if_exists: bool,
    ) -> PgWireResult<Vec<Response>> {
        let docs = self.event_trigger_docs();
        for name in names {
            if !docs.iter().any(|d| d.get_str("_id") == Ok(name)) {
                if if_exists {
                    self.notice(
                        "00000",
                        format!("event trigger \"{name}\" does not exist, skipping"),
                        None,
                    );
                    continue;
                }
                return Err(Self::event_trigger_missing(name));
            }
            self.delete_type_doc(EVENT_TRIGGER_COLLECTION, name)?;
        }
        Ok(vec![Response::Execution(Tag::new("DROP EVENT TRIGGER"))])
    }

    /// Does any event trigger exist? The DDL path checks this first.
    pub(crate) fn has_event_triggers(&self) -> bool {
        !self.event_trigger_docs().is_empty()
    }

    /// The command tag an event trigger sees for `stmt`, when it is one
    /// PostgreSQL fires them for.
    fn event_tag(stmt: &Statement) -> Option<String> {
        let tag = match stmt {
            Statement::CreateRule { .. } => "CREATE RULE",
            Statement::DropRule { .. } => "DROP RULE",
            Statement::CreateCollation { .. } => "CREATE COLLATION",
            _ => Self::write_verb(stmt)?,
        };
        secantus_pgplan::event_triggers::OK_TAGS
            .contains(&tag)
            .then(|| tag.to_string())
    }

    /// Run `stmt`, firing the event triggers PostgreSQL fires around it.
    pub(crate) fn with_event_triggers(
        &self,
        stmt: Statement,
        run: impl FnOnce(Statement) -> PgWireResult<Vec<Response>>,
    ) -> PgWireResult<Vec<Response>> {
        if IN_COMMAND.with(Cell::get) || !self.has_event_triggers() {
            return run(stmt);
        }
        let Some(tag) = Self::event_tag(&stmt) else {
            return run(stmt);
        };
        IN_COMMAND.with(|c| c.set(true));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                IN_COMMAND.with(|c| c.set(false));
                DROPPED.with(|d| d.borrow_mut().clear());
                COMMANDS.with(|c| c.borrow_mut().clear());
            }
        }
        let _reset = Reset;
        self.fire_event("ddl_command_start", &tag)?;
        let dropped = self.dropped_by(&stmt);
        let created = Self::created_by(&stmt);
        let out = run(stmt)?;
        if !dropped.is_empty() {
            DROPPED.with(|d| *d.borrow_mut() = dropped);
            self.fire_event("sql_drop", &tag)?;
            DROPPED.with(|d| d.borrow_mut().clear());
        }
        let commands = self.commands_for(&tag, created);
        COMMANDS.with(|c| *c.borrow_mut() = commands);
        self.fire_event("ddl_command_end", &tag)?;
        Ok(out)
    }

    fn fire_event(&self, event: &str, tag: &str) -> PgWireResult<()> {
        let mut triggers: Vec<Document> = self
            .event_trigger_docs()
            .into_iter()
            .filter(|d| {
                d.get_str("event") == Ok(event)
                    && d.get_str("enabled") != Ok("D")
                    // A trigger ENABLE REPLICA fires only in the replica
                    // role; this server's sessions are always origin.
                    && d.get_str("enabled") != Ok("R")
                    && d.get_array("tags")
                        .map(|t| t.iter().any(|v| v.as_str() == Some(tag)))
                        .unwrap_or(true)
            })
            .collect();
        triggers.sort_by(|a, b| a.get_str("_id").ok().cmp(&b.get_str("_id").ok()));
        for trg in triggers {
            let function = trg.get_str("function").unwrap_or_default();
            let Some(doc) = self
                .user_function_docs()?
                .into_iter()
                .find(|d| d.get_str("name") == Ok(function) && d.get_i32("nargs") == Ok(0))
            else {
                return Err(Self::user_error(
                    "42883",
                    format!("function {function}() does not exist"),
                ));
            };
            let data = TriggerData {
                new: None,
                old: None,
                op: tag.to_string(),
                name: trg.get_str("_id").unwrap_or_default().to_string(),
                table: String::new(),
                when: event.to_string(),
                level: plpgsql_fn::EVENT_LEVEL.to_string(),
                args: Vec::new(),
            };
            self.with_call_depth(|| {
                plpgsql_fn::run(
                    &crate::plpgsql_create_sql(&doc),
                    plpgsql_fn::Invocation {
                        args: &[],
                        arg_types: &[],
                        trigger: Some(data),
                        returns_set: false,
                        out_params: &[],
                    },
                    &PlHost { h: self },
                )
                .map_err(crate::wire_pl_error)
            })?;
        }
        Ok(())
    }

    /// What `stmt` will drop, read before it runs.
    fn dropped_by(&self, stmt: &Statement) -> Vec<Dropped> {
        let mut out = Vec::new();
        match stmt {
            Statement::DropTable(d) => {
                for t in &d.tables {
                    if let Some(def) = self.lookup(t) {
                        self.dropped_table(&def, &mut out);
                    }
                }
            }
            Statement::DropView { names, .. } => {
                let views = self.views().unwrap_or_default();
                for v in names {
                    if !views.iter().any(|(n, _)| n == v) {
                        continue;
                    }
                    out.push(Dropped::relation("view", Self::view_oid(v), v, true));
                    out.extend(Dropped::row_types(self.type_oid_by_name(v).unwrap_or(0), v));
                    out.push(Dropped {
                        classid: PG_REWRITE,
                        objid: 0,
                        original: false,
                        normal: true,
                        object_type: "rule",
                        schema: None,
                        name: None,
                        identity: format!("\"_RETURN\" on public.{}", ident(v)),
                        address_names: vec!["public".into(), v.clone(), "_RETURN".into()],
                        address_args: Vec::new(),
                    });
                }
            }
            Statement::DropIndex { names, .. } => {
                let indexes = self.index_relations();
                for i in names {
                    if let Some(ix) = indexes.iter().find(|ix| ix.name == *i) {
                        out.push(Dropped::relation("index", ix.oid, i, true));
                    }
                }
            }
            Statement::DropSequence { names, .. } => {
                for s in names {
                    if self.sequence_doc(s).ok().flatten().is_some() {
                        out.push(Dropped::relation(
                            "sequence",
                            Self::sequence_oid(s),
                            s,
                            true,
                        ));
                    }
                }
            }
            Statement::DropType { names, .. } => {
                let composites = self.composites().unwrap_or_default();
                for t in names {
                    let Some(oid) = self.type_oid_by_name(t) else {
                        continue;
                    };
                    let [mut row, array] = Dropped::row_types(oid, t);
                    row.original = true;
                    out.push(row);
                    if composites.iter().any(|(n, _, _)| n == t) {
                        out.push(Dropped::relation("composite type", 0, t, false));
                    }
                    out.push(array);
                }
            }
            Statement::DropSchema { names, .. } => {
                for s in names {
                    out.push(Dropped {
                        classid: PG_NAMESPACE,
                        objid: 0,
                        original: true,
                        normal: false,
                        object_type: "schema",
                        schema: None,
                        name: Some(s.clone()),
                        identity: ident(s),
                        address_names: vec![s.clone()],
                        address_args: Vec::new(),
                    });
                }
            }
            Statement::DropFunction {
                name, arg_types, ..
            } => {
                let docs = self.user_function_docs().unwrap_or_default();
                let matching: Vec<&Document> = docs
                    .iter()
                    .filter(|d| d.get_str("name") == Ok(name.as_str()))
                    .filter(|d| match arg_types {
                        Some(a) => d.get_i32("nargs") == Ok(a.len() as i32),
                        None => true,
                    })
                    .collect();
                if let Some(f) = matching.first() {
                    let args: Vec<String> = f
                        .get_array("param_types")
                        .map(|a| {
                            a.iter()
                                .filter_map(|t| t.as_str().map(qualified_type))
                                .collect()
                        })
                        .unwrap_or_default();
                    out.push(Dropped {
                        classid: PG_PROC,
                        objid: 0,
                        original: true,
                        normal: false,
                        object_type: "function",
                        schema: Some("public".into()),
                        name: None,
                        identity: format!("public.{}({})", ident(name), args.join(",")),
                        address_names: vec!["public".into(), name.clone()],
                        address_args: args,
                    });
                }
            }
            Statement::Sequence(_, statements) => {
                for st in statements {
                    out.extend(self.dropped_by(st));
                }
            }
            _ => {}
        }
        out
    }

    /// A table and what goes with it, in PostgreSQL's dependency order for
    /// the objects a client can see (no TOAST table).
    fn dropped_table(&self, def: &TableDef, out: &mut Vec<Dropped>) {
        let oid = self.relation_oid(&def.name).unwrap_or(0);
        out.push(Dropped::relation("table", oid, &def.name, true));
        for c in &def.columns {
            if let Some(seq) = &c.sequence {
                out.push(Dropped::relation(
                    "sequence",
                    Self::sequence_oid(seq),
                    seq,
                    false,
                ));
            }
        }
        out.extend(Dropped::row_types(
            self.type_oid_by_name(&def.name).unwrap_or(0),
            &def.name,
        ));
        for c in &def.columns {
            if c.default.is_some() || c.sequence.is_some() {
                out.push(Dropped {
                    classid: PG_ATTRDEF,
                    objid: 0,
                    original: false,
                    normal: true,
                    object_type: "default value",
                    schema: Some("public".into()),
                    name: None,
                    identity: format!("for public.{}.{}", ident(&def.name), ident(&c.name)),
                    address_names: vec!["public".into(), def.name.clone(), c.name.clone()],
                    address_args: Vec::new(),
                });
            }
        }
        for ix in self
            .index_relations()
            .into_iter()
            .filter(|ix| ix.table.name == def.name)
        {
            let constraint = ix.primary || def.unique_constraints.iter().any(|u| u.name == ix.name);
            if constraint {
                out.push(Dropped {
                    classid: PG_CONSTRAINT,
                    objid: 0,
                    original: false,
                    normal: false,
                    object_type: "table constraint",
                    schema: Some("public".into()),
                    name: None,
                    identity: format!("{} on public.{}", ident(&ix.name), ident(&def.name)),
                    address_names: vec!["public".into(), def.name.clone(), ix.name.clone()],
                    address_args: Vec::new(),
                });
            }
            out.push(Dropped::relation("index", ix.oid, &ix.name, false));
        }
    }

    /// What `stmt` creates or alters, by name, resolved after it runs.
    fn created_by(stmt: &Statement) -> Vec<(&'static str, String)> {
        match stmt {
            Statement::CreateTable(def, _) => {
                let mut v = Vec::new();
                for c in &def.columns {
                    if let Some(seq) = &c.sequence {
                        v.push(("sequence", seq.clone()));
                    }
                }
                v.push(("table", def.name.clone()));
                v
            }
            Statement::AlterTable { table, .. }
            | Statement::RenameTable { table, .. }
            | Statement::RenameColumn { table, .. } => vec![("table", table.clone())],
            Statement::CreateIndex(ci) => vec![("index", ci.name.clone().unwrap_or_default())],
            Statement::CreateView(v) => vec![("view", v.name.clone())],
            Statement::CreateSequence { name, .. } => vec![("sequence", name.clone())],
            Statement::CreateComposite { name, .. } | Statement::CreateEnum { name, .. } => {
                vec![("type", name.clone())]
            }
            Statement::CreateUserFunction(def) => {
                let args: Vec<String> = def.params.iter().map(|(_, t)| qualified_type(t)).collect();
                vec![(
                    "function",
                    format!("{}({})", ident(&def.name), args.join(",")),
                )]
            }
            _ => Vec::new(),
        }
    }

    /// `pg_event_trigger_ddl_commands()`'s rows for a command that ran.
    fn commands_for(&self, tag: &str, created: Vec<(&'static str, String)>) -> Vec<DdlCommand> {
        let mut out = Vec::new();
        for (kind, name) in created {
            let relation = |kind: &'static str, name: &str, tag: &str| DdlCommand {
                classid: PG_CLASS,
                objid: self.relation_oid(name).unwrap_or(0),
                tag: tag.to_string(),
                object_type: kind,
                identity: format!("public.{}", ident(name)),
            };
            match kind {
                "table" if tag == "CREATE TABLE" => {
                    out.push(relation("table", &name, tag));
                    for ix in self
                        .index_relations()
                        .into_iter()
                        .filter(|ix| ix.table.name == name)
                    {
                        out.push(relation("index", &ix.name, "CREATE INDEX"));
                    }
                    if let Some(def) = self.lookup(&name) {
                        for c in &def.columns {
                            if let Some(seq) = &c.sequence {
                                out.push(relation("sequence", seq, "ALTER SEQUENCE"));
                            }
                        }
                    }
                }
                "sequence" if tag == "CREATE TABLE" => {
                    out.push(relation("sequence", &name, "CREATE SEQUENCE"))
                }
                "index" if name.is_empty() => {}
                "function" => out.push(DdlCommand {
                    classid: PG_PROC,
                    objid: 0,
                    tag: tag.to_string(),
                    object_type: "function",
                    identity: format!("public.{name}"),
                }),
                "type" => out.push(DdlCommand {
                    classid: PG_TYPE,
                    objid: self.type_oid_by_name(&name).unwrap_or(0),
                    tag: tag.to_string(),
                    object_type: "type",
                    identity: format!("public.{}", ident(&name)),
                }),
                _ => out.push(relation(kind, &name, tag)),
            }
        }
        out
    }

    /// `pg_event_trigger`.
    pub(crate) fn pg_event_trigger_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        self.event_trigger_docs()
            .iter()
            .map(|d| {
                let function = d.get_str("function").unwrap_or_default();
                let mut r = Document::new();
                r.insert(f("oid"), Bson::Int64(d.get_i64("oid").unwrap_or(0)));
                r.insert(f("evtname"), d.get_str("_id").unwrap_or_default());
                r.insert(f("evtevent"), d.get_str("event").unwrap_or_default());
                r.insert(
                    f("evtowner"),
                    Bson::Int64(
                        self.role(d.get_str("owner").unwrap_or_default())
                            .ok()
                            .flatten()
                            .map_or(10, |r| r.oid),
                    ),
                );
                r.insert(
                    f("evtfoid"),
                    Bson::Int64(Self::index_oid(&format!("func:{function}"))),
                );
                r.insert(f("evtenabled"), d.get_str("enabled").unwrap_or("O"));
                r.insert(
                    f("evttags"),
                    d.get_array("tags").cloned().map_or(Bson::Null, Bson::Array),
                );
                r
            })
            .collect()
    }

    /// `pg_event_trigger_dropped_objects()`, while a `sql_drop` trigger runs.
    pub(crate) fn dropped_objects_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        let strs = |v: &[String]| Bson::Array(v.iter().cloned().map(Bson::String).collect());
        let opt = |v: &Option<String>| v.clone().map_or(Bson::Null, Bson::String);
        DROPPED.with(|d| {
            d.borrow()
                .iter()
                .map(|o| {
                    let mut r = Document::new();
                    r.insert(f("classid"), Bson::Int64(o.classid));
                    r.insert(f("objid"), Bson::Int64(o.objid));
                    r.insert(f("objsubid"), Bson::Int32(0));
                    r.insert(f("original"), o.original);
                    r.insert(f("normal"), o.normal);
                    r.insert(f("is_temporary"), false);
                    r.insert(f("object_type"), o.object_type);
                    r.insert(f("schema_name"), opt(&o.schema));
                    r.insert(f("object_name"), opt(&o.name));
                    r.insert(f("object_identity"), o.identity.as_str());
                    r.insert(f("address_names"), strs(&o.address_names));
                    r.insert(f("address_args"), strs(&o.address_args));
                    r
                })
                .collect()
        })
    }

    /// `pg_event_trigger_ddl_commands()`, while a `ddl_command_end` trigger
    /// runs.
    pub(crate) fn ddl_commands_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        COMMANDS.with(|c| {
            c.borrow()
                .iter()
                .map(|o| {
                    let mut r = Document::new();
                    r.insert(f("classid"), Bson::Int64(o.classid));
                    r.insert(f("objid"), Bson::Int64(o.objid));
                    r.insert(f("objsubid"), Bson::Int32(0));
                    r.insert(f("command_tag"), o.tag.as_str());
                    r.insert(f("object_type"), o.object_type);
                    r.insert(f("schema_name"), "public");
                    r.insert(f("object_identity"), o.identity.as_str());
                    r.insert(f("in_extension"), false);
                    r
                })
                .collect()
        })
    }
}

/// `pg_event_trigger`.
pub(crate) fn pg_event_trigger_def() -> TableDef {
    TableDef::new(
        "pg_event_trigger",
        vec![
            Column::new("oid", "oid", false),
            Column::new("evtname", "name", false),
            Column::new("evtevent", "name", false),
            Column::new("evtowner", "oid", false),
            Column::new("evtfoid", "oid", false),
            Column::new("evtenabled", secantus_pgplan::QUOTED_CHAR, false),
            Column::new("evttags", "text[]", false),
        ],
    )
}

/// `pg_event_trigger_dropped_objects()`'s columns.
pub(crate) fn dropped_objects_def() -> TableDef {
    TableDef::new(
        "pg_event_trigger_dropped_objects",
        vec![
            Column::new("classid", "oid", false),
            Column::new("objid", "oid", false),
            Column::new("objsubid", "int4", false),
            Column::new("original", "bool", false),
            Column::new("normal", "bool", false),
            Column::new("is_temporary", "bool", false),
            Column::new("object_type", "text", false),
            Column::new("schema_name", "text", false),
            Column::new("object_name", "text", false),
            Column::new("object_identity", "text", false),
            Column::new("address_names", "text[]", false),
            Column::new("address_args", "text[]", false),
        ],
    )
}

/// `pg_event_trigger_ddl_commands()`'s columns, less `command` (a
/// `pg_ddl_command`, which has no output form).
pub(crate) fn ddl_commands_def() -> TableDef {
    TableDef::new(
        "pg_event_trigger_ddl_commands",
        vec![
            Column::new("classid", "oid", false),
            Column::new("objid", "oid", false),
            Column::new("objsubid", "int4", false),
            Column::new("command_tag", "text", false),
            Column::new("object_type", "text", false),
            Column::new("schema_name", "text", false),
            Column::new("object_identity", "text", false),
            Column::new("in_extension", "bool", false),
        ],
    )
}
