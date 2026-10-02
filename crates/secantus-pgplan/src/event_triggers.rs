//! `CREATE` / `ALTER` / `DROP EVENT TRIGGER`: planned and validated here as
//! PostgreSQL's `evtcache` / `event_trigger.c` validate them; the server
//! stores and fires them.

use bson::Bson;
use pg_query::protobuf::node::Node as N;

use crate::{Error, Result, Statement};

/// The events a trigger can name.
pub const EVENTS: [&str; 4] = [
    "ddl_command_start",
    "ddl_command_end",
    "sql_drop",
    "table_rewrite",
];

/// The command tags an event trigger fires for (`event_trigger_ok` in
/// PostgreSQL 15's `cmdtaglist.h`).
pub const OK_TAGS: &[&str] = &[
    "ALTER ACCESS METHOD",
    "ALTER AGGREGATE",
    "ALTER CAST",
    "ALTER COLLATION",
    "ALTER CONSTRAINT",
    "ALTER CONVERSION",
    "ALTER DEFAULT PRIVILEGES",
    "ALTER DOMAIN",
    "ALTER EXTENSION",
    "ALTER FOREIGN DATA WRAPPER",
    "ALTER FOREIGN TABLE",
    "ALTER FUNCTION",
    "ALTER INDEX",
    "ALTER LANGUAGE",
    "ALTER LARGE OBJECT",
    "ALTER MATERIALIZED VIEW",
    "ALTER OPERATOR",
    "ALTER OPERATOR CLASS",
    "ALTER OPERATOR FAMILY",
    "ALTER POLICY",
    "ALTER PROCEDURE",
    "ALTER PUBLICATION",
    "ALTER ROUTINE",
    "ALTER RULE",
    "ALTER SCHEMA",
    "ALTER SEQUENCE",
    "ALTER SERVER",
    "ALTER STATISTICS",
    "ALTER SUBSCRIPTION",
    "ALTER TABLE",
    "ALTER TEXT SEARCH CONFIGURATION",
    "ALTER TEXT SEARCH DICTIONARY",
    "ALTER TEXT SEARCH PARSER",
    "ALTER TEXT SEARCH TEMPLATE",
    "ALTER TRANSFORM",
    "ALTER TRIGGER",
    "ALTER TYPE",
    "ALTER USER MAPPING",
    "ALTER VIEW",
    "COMMENT",
    "CREATE ACCESS METHOD",
    "CREATE AGGREGATE",
    "CREATE CAST",
    "CREATE COLLATION",
    "CREATE CONSTRAINT",
    "CREATE CONVERSION",
    "CREATE DOMAIN",
    "CREATE EXTENSION",
    "CREATE FOREIGN DATA WRAPPER",
    "CREATE FOREIGN TABLE",
    "CREATE FUNCTION",
    "CREATE INDEX",
    "CREATE LANGUAGE",
    "CREATE MATERIALIZED VIEW",
    "CREATE OPERATOR",
    "CREATE OPERATOR CLASS",
    "CREATE OPERATOR FAMILY",
    "CREATE POLICY",
    "CREATE PROCEDURE",
    "CREATE PUBLICATION",
    "CREATE ROUTINE",
    "CREATE RULE",
    "CREATE SCHEMA",
    "CREATE SEQUENCE",
    "CREATE SERVER",
    "CREATE STATISTICS",
    "CREATE SUBSCRIPTION",
    "CREATE TABLE",
    "CREATE TABLE AS",
    "CREATE TEXT SEARCH CONFIGURATION",
    "CREATE TEXT SEARCH DICTIONARY",
    "CREATE TEXT SEARCH PARSER",
    "CREATE TEXT SEARCH TEMPLATE",
    "CREATE TRANSFORM",
    "CREATE TRIGGER",
    "CREATE TYPE",
    "CREATE USER MAPPING",
    "CREATE VIEW",
    "DROP ACCESS METHOD",
    "DROP AGGREGATE",
    "DROP CAST",
    "DROP COLLATION",
    "DROP CONSTRAINT",
    "DROP CONVERSION",
    "DROP DOMAIN",
    "DROP EXTENSION",
    "DROP FOREIGN DATA WRAPPER",
    "DROP FOREIGN TABLE",
    "DROP FUNCTION",
    "DROP INDEX",
    "DROP LANGUAGE",
    "DROP MATERIALIZED VIEW",
    "DROP OPERATOR",
    "DROP OPERATOR CLASS",
    "DROP OPERATOR FAMILY",
    "DROP OWNED",
    "DROP POLICY",
    "DROP PROCEDURE",
    "DROP PUBLICATION",
    "DROP ROUTINE",
    "DROP RULE",
    "DROP SCHEMA",
    "DROP SEQUENCE",
    "DROP SERVER",
    "DROP STATISTICS",
    "DROP SUBSCRIPTION",
    "DROP TABLE",
    "DROP TEXT SEARCH CONFIGURATION",
    "DROP TEXT SEARCH DICTIONARY",
    "DROP TEXT SEARCH PARSER",
    "DROP TEXT SEARCH TEMPLATE",
    "DROP TRANSFORM",
    "DROP TRIGGER",
    "DROP TYPE",
    "DROP USER MAPPING",
    "DROP VIEW",
    "GRANT",
    "IMPORT FOREIGN SCHEMA",
    "REFRESH MATERIALIZED VIEW",
    "REVOKE",
    "SECURITY LABEL",
    "SELECT INTO",
];

/// Tags PostgreSQL knows but will not fire an event trigger for.
const NOT_OK_TAGS: &[&str] = &[
    "ALTER DATABASE",
    "ALTER EVENT TRIGGER",
    "ALTER ROLE",
    "ALTER SYSTEM",
    "ALTER TABLESPACE",
    "ANALYZE",
    "BEGIN",
    "CALL",
    "CHECKPOINT",
    "CLUSTER",
    "COMMIT",
    "COPY",
    "CREATE DATABASE",
    "CREATE EVENT TRIGGER",
    "CREATE ROLE",
    "CREATE TABLESPACE",
    "DELETE",
    "DISCARD",
    "DO",
    "DROP DATABASE",
    "DROP EVENT TRIGGER",
    "DROP ROLE",
    "DROP TABLESPACE",
    "EXPLAIN",
    "GRANT ROLE",
    "INSERT",
    "LISTEN",
    "LOCK TABLE",
    "NOTIFY",
    "REINDEX",
    "REVOKE ROLE",
    "ROLLBACK",
    "SELECT",
    "SET",
    "SHOW",
    "TRUNCATE TABLE",
    "UPDATE",
    "VACUUM",
];

fn syntax(message: String) -> Error {
    Error::Sqlstate("42601", message)
}

pub(crate) fn plan_create(c: &pg_query::protobuf::CreateEventTrigStmt) -> Result<Statement> {
    if !EVENTS.contains(&c.eventname.as_str()) {
        return Err(syntax(format!(
            "unrecognized event name \"{}\"",
            c.eventname
        )));
    }
    let mut tags: Option<Vec<String>> = None;
    for w in &c.whenclause {
        let Some(N::DefElem(d)) = w.node.as_ref() else {
            continue;
        };
        if d.defname != "tag" {
            return Err(syntax(format!(
                "unrecognized filter variable \"{}\"",
                d.defname
            )));
        }
        if tags.is_some() {
            return Err(syntax(format!(
                "filter variable \"{}\" specified more than once",
                d.defname
            )));
        }
        let values = match d.arg.as_deref().and_then(|a| a.node.as_ref()) {
            Some(N::List(l)) => crate::string_list(&l.items),
            _ => Vec::new(),
        };
        let mut list = Vec::with_capacity(values.len());
        for v in values {
            let tag = v.to_ascii_uppercase();
            if NOT_OK_TAGS.contains(&tag.as_str()) {
                return Err(Error::FeatureNotSupported(format!(
                    "event triggers are not supported for {tag}"
                )));
            }
            if !OK_TAGS.contains(&tag.as_str()) {
                return Err(syntax(format!(
                    "filter value \"{v}\" not recognized for filter variable \"tag\""
                )));
            }
            list.push(tag);
        }
        tags = Some(list);
    }
    if c.eventname == "table_rewrite" && tags.is_some() {
        // table_rewrite filters on ALTER TABLE / ALTER TYPE only.
        for t in tags.iter().flatten() {
            if !matches!(
                t.as_str(),
                "ALTER TABLE" | "ALTER TYPE" | "ALTER MATERIALIZED VIEW"
            ) {
                return Err(Error::FeatureNotSupported(format!(
                    "event \"table_rewrite\" is not supported for {t}"
                )));
            }
        }
    }
    let function = crate::string_list(&c.funcname)
        .pop()
        .ok_or_else(|| Error::Parse("an event trigger needs a function".into()))?;
    Ok(Statement::CreateEventTrigger {
        name: c.trigname.clone(),
        event: c.eventname.clone(),
        tags,
        function,
    })
}

pub(crate) fn plan_alter(a: &pg_query::protobuf::AlterEventTrigStmt) -> Result<Statement> {
    Ok(Statement::AlterEventTrigger {
        name: a.trigname.clone(),
        enabled: a.tgenabled.clone(),
    })
}

pub(crate) fn plan_drop(d: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    Ok(Statement::DropEventTrigger {
        names: crate::string_list(&d.objects),
        if_exists: d.missing_ok,
    })
}

thread_local! {
    /// The event whose trigger functions are running, set by the executor.
    static CONTEXT: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    /// The relation a `table_rewrite` event is rewriting, and why.
    static REWRITE: std::cell::Cell<Option<(i64, i32)>> = const { std::cell::Cell::new(None) };
}

/// Install the event whose trigger functions run next (`None` after).
pub fn set_context(event: Option<&'static str>) {
    CONTEXT.with(|c| c.set(event));
}

/// Install the relation a `table_rewrite` trigger sees, and the reason
/// (`AT_REWRITE_*`: 1 persistence, 2 default value, 4 column rewrite, 8
/// access method).
pub fn set_rewrite(rewrite: Option<(i64, i32)>) {
    REWRITE.with(|r| r.set(rewrite));
}

fn outside(function: &str, event: &str) -> Error {
    Error::Sqlstate(
        "39P03",
        if event == "ddl_command_end" {
            format!("{function}() can only be called in an event trigger function")
        } else {
            format!("{function}() can only be called in a {event} event trigger function")
        },
    )
}

/// `pg_event_trigger_table_rewrite_oid()` / `_reason()`.
pub(crate) fn rewrite_function(name: &str) -> Option<Result<Bson>> {
    let which = match name {
        "pg_event_trigger_table_rewrite_oid" => 0,
        "pg_event_trigger_table_rewrite_reason" => 1,
        _ => return None,
    };
    Some(match REWRITE.with(std::cell::Cell::get) {
        Some((oid, reason)) if CONTEXT.with(std::cell::Cell::get) == Some("table_rewrite") => {
            Ok(if which == 0 {
                crate::regclass_value(oid)
            } else {
                Bson::Int32(reason)
            })
        }
        _ => Err(outside(name, "table_rewrite")),
    })
}

/// The functions an event trigger's body reads, served by the server as
/// relations of the same name.
pub const SOURCES: [&str; 2] = [
    "pg_event_trigger_dropped_objects",
    "pg_event_trigger_ddl_commands",
];

/// The source relation `n` names, when it is a call to one of `SOURCES`.
fn source_of(n: &pg_query::protobuf::Node) -> Option<Result<pg_query::protobuf::Node>> {
    let Some(N::RangeFunction(rf)) = n.node.as_ref() else {
        return None;
    };
    let [item] = rf.functions.as_slice() else {
        return None;
    };
    let Some(N::List(l)) = item.node.as_ref() else {
        return None;
    };
    let Some(N::FuncCall(f)) = l.items.first().and_then(|n| n.node.as_ref()) else {
        return None;
    };
    let name = crate::string_list(&f.funcname).pop()?;
    if !(SOURCES.contains(&name.as_str()) || name == "pg_get_keywords") || !f.args.is_empty() {
        return None;
    }
    // Only inside the event that fills it (`sql_drop`, `ddl_command_end`).
    // `pg_get_keywords()` is the grammar's keyword list: always there.
    if name != "pg_get_keywords" {
        let event = if name == "pg_event_trigger_dropped_objects" {
            "sql_drop"
        } else {
            "ddl_command_end"
        };
        if CONTEXT.with(std::cell::Cell::get) != Some(event) {
            return Some(Err(outside(&name, event)));
        }
    }
    Some(Ok(pg_query::protobuf::Node {
        node: Some(N::RangeVar(pg_query::protobuf::RangeVar {
            relname: name,
            inh: true,
            relpersistence: "p".into(),
            alias: rf.alias.clone(),
            ..Default::default()
        })),
    }))
}

/// Turn `FROM pg_event_trigger_dropped_objects()` (and its sibling) into a
/// read of the relation the server fills while an event trigger runs.
pub(crate) fn rewrite_sources(sql: &str, node: &mut pg_query::protobuf::Node) -> Result<()> {
    let lower = sql.to_ascii_lowercase();
    if !lower.contains("pg_event_trigger_") && !lower.contains("pg_get_keywords") {
        return Ok(());
    }
    let Some(root) = node.node.as_mut() else {
        return Ok(());
    };
    for _ in 0..1_000 {
        let mut replaced = false;
        // SAFETY: each pointer is used only until the first replacement,
        // after which the loop restarts with a fresh list.
        unsafe {
            for (n, _, _) in root.nodes_mut() {
                use pg_query::NodeMut as M;
                let slots: Vec<*mut pg_query::protobuf::Node> = match n {
                    M::SelectStmt(s) => (*s).from_clause.iter_mut().map(|n| n as *mut _).collect(),
                    M::JoinExpr(j) => [(*j).larg.as_deref_mut(), (*j).rarg.as_deref_mut()]
                        .into_iter()
                        .flatten()
                        .map(|n| n as *mut _)
                        .collect(),
                    _ => continue,
                };
                for slot in slots {
                    if let Some(new) = source_of(&*slot) {
                        *slot = new?;
                        replaced = true;
                    }
                }
                if replaced {
                    break;
                }
            }
        }
        if !replaced {
            return Ok(());
        }
    }
    Ok(())
}
