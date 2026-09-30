//! Foreign data: `CREATE` / `ALTER` / `DROP FOREIGN DATA WRAPPER`, `SERVER`,
//! `USER MAPPING` and `FOREIGN TABLE`, and `IMPORT FOREIGN SCHEMA`.
//!
//! This server loads no FDW handler (none can be written in SQL or
//! PL/pgSQL), so these are catalog objects: stored, listed in
//! `pg_foreign_*` and `information_schema`, and validated as PostgreSQL
//! validates them. Reading or writing a foreign table, and importing a
//! schema, answer PostgreSQL's own refusal for a wrapper with no handler.

use pg_query::protobuf::node::Node as N;
use pg_query::protobuf::{DefElemAction, ObjectType};

use crate::{Error, Result, Statement, TableDef};

/// One `OPTIONS (...)` entry: `ADD` (the default), `SET` or `DROP`.
#[derive(Debug, Clone, PartialEq)]
pub enum OptionEdit {
    Add(String, String),
    Set(String, String),
    Drop(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdwKind {
    Wrapper,
    Server,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FdwOp {
    CreateWrapper {
        name: String,
        handler: Option<String>,
        validator: Option<String>,
        options: Vec<String>,
    },
    AlterWrapper {
        name: String,
        options: Vec<OptionEdit>,
    },
    CreateServer {
        name: String,
        wrapper: String,
        server_type: Option<String>,
        version: Option<String>,
        options: Vec<String>,
        if_not_exists: bool,
    },
    AlterServer {
        name: String,
        /// `VERSION 'v'` / `NO VERSION` (`Some(None)`), when given.
        version: Option<Option<String>>,
        options: Vec<OptionEdit>,
    },
    CreateUserMapping {
        user: String,
        server: String,
        options: Vec<String>,
        if_not_exists: bool,
    },
    AlterUserMapping {
        user: String,
        server: String,
        options: Vec<OptionEdit>,
    },
    DropUserMapping {
        user: String,
        server: String,
        if_exists: bool,
    },
    Drop {
        kind: FdwKind,
        names: Vec<String>,
        if_exists: bool,
        cascade: bool,
    },
    /// The table is created by `create`, then marked foreign.
    CreateForeignTable {
        create: Box<Statement>,
        table: String,
        server: String,
        options: Vec<String>,
    },
    /// `ALTER FOREIGN TABLE`: the table alteration it is.
    AlterForeignTable {
        alter: Box<Statement>,
        table: String,
    },
    /// `DROP FOREIGN TABLE`: the table drop it is.
    DropForeignTables {
        drop: Box<Statement>,
        tables: Vec<String>,
        if_exists: bool,
    },
    ImportForeignSchema {
        server: String,
    },
}

impl FdwOp {
    pub fn tag(&self) -> &'static str {
        match self {
            FdwOp::CreateWrapper { .. } => "CREATE FOREIGN DATA WRAPPER",
            FdwOp::AlterWrapper { .. } => "ALTER FOREIGN DATA WRAPPER",
            FdwOp::CreateServer { .. } => "CREATE SERVER",
            FdwOp::AlterServer { .. } => "ALTER SERVER",
            FdwOp::CreateUserMapping { .. } => "CREATE USER MAPPING",
            FdwOp::AlterUserMapping { .. } => "ALTER USER MAPPING",
            FdwOp::DropUserMapping { .. } => "DROP USER MAPPING",
            FdwOp::Drop {
                kind: FdwKind::Wrapper,
                ..
            } => "DROP FOREIGN DATA WRAPPER",
            FdwOp::Drop {
                kind: FdwKind::Server,
                ..
            } => "DROP SERVER",
            FdwOp::CreateForeignTable { .. } => "CREATE FOREIGN TABLE",
            FdwOp::AlterForeignTable { .. } => "ALTER FOREIGN TABLE",
            FdwOp::DropForeignTables { .. } => "DROP FOREIGN TABLE",
            FdwOp::ImportForeignSchema { .. } => "IMPORT FOREIGN SCHEMA",
        }
    }
}

fn duplicate(name: &str) -> Error {
    Error::Sqlstate(
        "42710",
        format!("option \"{name}\" provided more than once"),
    )
}

fn option_value(d: &pg_query::protobuf::DefElem) -> String {
    match d.arg.as_deref().and_then(|a| a.node.as_ref()) {
        Some(N::String(s)) => s.sval.clone(),
        Some(N::Integer(i)) => i.ival.to_string(),
        Some(N::Float(f)) => f.fval.clone(),
        Some(N::Boolean(b)) => b.boolval.to_string(),
        _ => String::new(),
    }
}

fn def_elems(nodes: &[pg_query::protobuf::Node]) -> Vec<&pg_query::protobuf::DefElem> {
    nodes
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::DefElem(d)) => Some(&**d),
            _ => None,
        })
        .collect()
}

/// A CREATE's options as PostgreSQL stores them: `name=value`.
fn create_options(nodes: &[pg_query::protobuf::Node]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for d in def_elems(nodes) {
        if out
            .iter()
            .any(|o| o.split('=').next() == Some(d.defname.as_str()))
        {
            return Err(duplicate(&d.defname));
        }
        out.push(format!("{}={}", d.defname, option_value(d)));
    }
    Ok(out)
}

pub(crate) fn option_edits_public(nodes: &[pg_query::protobuf::Node]) -> Vec<OptionEdit> {
    option_edits(nodes)
}

fn option_edits(nodes: &[pg_query::protobuf::Node]) -> Vec<OptionEdit> {
    def_elems(nodes)
        .into_iter()
        .map(|d| match DefElemAction::try_from(d.defaction) {
            Ok(DefElemAction::DefelemSet) => OptionEdit::Set(d.defname.clone(), option_value(d)),
            Ok(DefElemAction::DefelemDrop) => OptionEdit::Drop(d.defname.clone()),
            _ => OptionEdit::Add(d.defname.clone(), option_value(d)),
        })
        .collect()
}

/// Apply ALTER's option edits to stored options, with PostgreSQL's errors.
pub fn apply_option_edits(stored: &[String], edits: &[OptionEdit]) -> Result<Vec<String>> {
    let mut out = stored.to_vec();
    let pos =
        |out: &[String], name: &str| out.iter().position(|o| o.split('=').next() == Some(name));
    let missing = |name: &str| Error::Sqlstate("42704", format!("option \"{name}\" not found"));
    for e in edits {
        match e {
            OptionEdit::Add(k, v) => {
                if pos(&out, k).is_some() {
                    return Err(duplicate(k));
                }
                out.push(format!("{k}={v}"));
            }
            OptionEdit::Set(k, v) => {
                let i = pos(&out, k).ok_or_else(|| missing(k))?;
                out[i] = format!("{k}={v}");
            }
            OptionEdit::Drop(k) => {
                let i = pos(&out, k).ok_or_else(|| missing(k))?;
                out.remove(i);
            }
        }
    }
    Ok(out)
}

fn role(r: &Option<pg_query::protobuf::RoleSpec>) -> String {
    r.as_ref().map(crate::role_spec_name).unwrap_or_default()
}

/// The `name` of a function a `HANDLER` / `VALIDATOR` clause names.
fn func_option(opts: &[pg_query::protobuf::Node], which: &str) -> Option<String> {
    def_elems(opts)
        .into_iter()
        .find(|d| d.defname == which)
        .and_then(|d| match d.arg.as_deref().and_then(|a| a.node.as_ref()) {
            Some(N::List(l)) => crate::string_list(&l.items).pop(),
            _ => None,
        })
}

pub(crate) fn plan(
    node: &N,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[bson::Bson],
) -> Result<Option<Statement>> {
    let op = match node {
        N::CreateFdwStmt(c) => FdwOp::CreateWrapper {
            name: c.fdwname.clone(),
            handler: func_option(&c.func_options, "handler"),
            validator: func_option(&c.func_options, "validator"),
            options: create_options(&c.options)?,
        },
        N::AlterFdwStmt(a) => {
            if !a.func_options.is_empty() {
                return Err(Error::FeatureNotSupported(
                    "changing a foreign-data wrapper's HANDLER or VALIDATOR".into(),
                ));
            }
            FdwOp::AlterWrapper {
                name: a.fdwname.clone(),
                options: option_edits(&a.options),
            }
        }
        N::CreateForeignServerStmt(c) => FdwOp::CreateServer {
            name: c.servername.clone(),
            wrapper: c.fdwname.clone(),
            server_type: Some(c.servertype.clone()).filter(|s| !s.is_empty()),
            version: Some(c.version.clone()).filter(|s| !s.is_empty()),
            options: create_options(&c.options)?,
            if_not_exists: c.if_not_exists,
        },
        N::AlterForeignServerStmt(a) => FdwOp::AlterServer {
            name: a.servername.clone(),
            version: a
                .has_version
                .then(|| Some(a.version.clone()).filter(|s| !s.is_empty())),
            options: option_edits(&a.options),
        },
        N::CreateUserMappingStmt(c) => FdwOp::CreateUserMapping {
            user: role(&c.user),
            server: c.servername.clone(),
            options: create_options(&c.options)?,
            if_not_exists: c.if_not_exists,
        },
        N::AlterUserMappingStmt(a) => FdwOp::AlterUserMapping {
            user: role(&a.user),
            server: a.servername.clone(),
            options: option_edits(&a.options),
        },
        N::DropUserMappingStmt(d) => FdwOp::DropUserMapping {
            user: role(&d.user),
            server: d.servername.clone(),
            if_exists: d.missing_ok,
        },
        N::ImportForeignSchemaStmt(i) => FdwOp::ImportForeignSchema {
            server: i.server_name.clone(),
        },
        N::CreateForeignTableStmt(c) => {
            let base = c
                .base_stmt
                .as_ref()
                .ok_or_else(|| Error::Parse("CREATE FOREIGN TABLE without columns".into()))?;
            let table = base
                .relation
                .as_ref()
                .map(|r| r.relname.clone())
                .unwrap_or_default();
            let create = crate::plan_node_public(N::CreateStmt(base.clone()), lookup, params)?;
            FdwOp::CreateForeignTable {
                create: Box::new(create),
                table,
                server: c.servername.clone(),
                options: create_options(&c.options)?,
            }
        }
        N::AlterTableStmt(a) if a.objtype == ObjectType::ObjectForeignTable as i32 => {
            let table = a
                .relation
                .as_ref()
                .map(|r| r.relname.clone())
                .unwrap_or_default();
            if let Some(def) = lookup(&table) {
                if !is_foreign(&def) {
                    return Err(Error::Sqlstate(
                        "42809",
                        format!("\"{table}\" is not a foreign table"),
                    ));
                }
            }
            let mut plain = a.clone();
            plain.objtype = ObjectType::ObjectTable as i32;
            let alter = crate::plan_node_public(N::AlterTableStmt(plain), lookup, params)?;
            FdwOp::AlterForeignTable {
                alter: Box::new(alter),
                table,
            }
        }
        N::DropStmt(d) => match ObjectType::try_from(d.remove_type) {
            Ok(ObjectType::ObjectFdw) | Ok(ObjectType::ObjectForeignServer) => FdwOp::Drop {
                kind: if d.remove_type == ObjectType::ObjectFdw as i32 {
                    FdwKind::Wrapper
                } else {
                    FdwKind::Server
                },
                names: crate::string_list(&d.objects),
                if_exists: d.missing_ok,
                cascade: d.behavior == pg_query::protobuf::DropBehavior::DropCascade as i32,
            },
            Ok(ObjectType::ObjectForeignTable) => {
                let mut plain = d.clone();
                plain.remove_type = ObjectType::ObjectTable as i32;
                let drop = crate::plan_node_public(N::DropStmt(plain), lookup, params)?;
                let tables = match &drop {
                    Statement::DropTable(t) => t.tables.clone(),
                    _ => Vec::new(),
                };
                for t in &tables {
                    if let Some(def) = lookup(t) {
                        if !is_foreign(&def) {
                            return Err(Error::Sqlstate(
                                "42809",
                                format!("\"{t}\" is not a foreign table"),
                            ));
                        }
                    }
                }
                FdwOp::DropForeignTables {
                    drop: Box::new(drop),
                    tables,
                    if_exists: d.missing_ok,
                }
            }
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    Ok(Some(Statement::Fdw(op)))
}

/// Is `def` a foreign table?
pub fn is_foreign(def: &TableDef) -> bool {
    def.extra.get_str("foreign_server").is_ok()
}

/// Refuse a SELECT or a write that reads or writes a foreign table: this
/// server has no FDW handler to reach the data through.
pub(crate) fn refuse_foreign_access(
    node: &N,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<()> {
    if !matches!(
        node,
        N::SelectStmt(_)
            | N::InsertStmt(_)
            | N::UpdateStmt(_)
            | N::DeleteStmt(_)
            | N::MergeStmt(_)
            | N::CopyStmt(_)
    ) {
        return Ok(());
    }
    for (n, _, _, _) in node.nodes() {
        if let pg_query::NodeRef::RangeVar(rv) = n {
            if let Some(def) = lookup(&rv.relname) {
                if let Ok(wrapper) = def.extra.get_str("foreign_wrapper") {
                    return Err(Error::Sqlstate(
                        "55000",
                        format!("foreign-data wrapper \"{wrapper}\" has no handler"),
                    ));
                }
            }
        }
    }
    Ok(())
}
