//! `INSTEAD OF` triggers: a write to a VIEW that has one is not rewritten
//! onto the view's base table (see `view_dml`) but handed, row by row, to the
//! trigger function -- which is what PostgreSQL's executor does.
//!
//! The planner's part is to name the rows. Each write becomes one query over
//! the VIEW whose result the executor walks:
//!
//! * `INSERT INTO v [(cols)] <source>` -- the source's rows, `columns` naming
//!   which view column each position fills (empty: the leading columns);
//! * `UPDATE v SET c = e ... WHERE w` -- `SELECT *, e ... FROM v WHERE w`, the
//!   view's columns (OLD) followed by the new value of each `set` column;
//! * `DELETE FROM v WHERE w` -- `SELECT * FROM v WHERE w` (OLD).
//!
//! The query travels as SQL so the executor plans it with everything a
//! top-level query gets, subqueries included.

use super::*;

thread_local! {
    /// `(view, event)` for each `INSTEAD OF` row trigger, published by the
    /// executor beside the views.
    static INSTEAD_OF: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the views' `INSTEAD OF` triggers for the statements that follow.
pub fn set_instead_of_triggers(v: Vec<(String, String)>) {
    INSTEAD_OF.with(|t| *t.borrow_mut() = v);
}

fn has_trigger(view: &str, event: &str) -> bool {
    INSTEAD_OF.with(|t| t.borrow().iter().any(|(v, e)| v == view && e == event))
}

/// A write to a view handed to its `INSTEAD OF` triggers.
#[derive(Debug, Clone, PartialEq)]
pub struct InsteadOf {
    pub view: String,
    /// `INSERT`, `UPDATE` or `DELETE`.
    pub event: String,
    /// The query naming the rows (see the module comment).
    pub query_sql: String,
    /// INSERT: the target columns, in source order (empty: positional).
    /// UPDATE: the SET columns, in the order their new values follow OLD.
    pub columns: Vec<String>,
}

fn relation_of(r: &Option<pg_query::protobuf::RangeVar>) -> Option<(String, Option<String>)> {
    let r = r.as_ref()?;
    if !(r.schemaname.is_empty() || r.schemaname == "public") {
        return None;
    }
    let alias = r
        .alias
        .as_ref()
        .map(|a| a.aliasname.clone())
        .filter(|a| !a.is_empty());
    Some((r.relname.clone(), alias))
}

fn deparse_node(n: &pg_query::protobuf::Node) -> Result<String> {
    n.deparse().map_err(|e| Error::Parse(e.to_string()))
}

/// The `INSTEAD OF` plan for `node`, when it writes a view that has such a
/// trigger for its event.
pub(crate) fn plan(node: &pg_query::protobuf::Node) -> Result<Option<Statement>> {
    let q = scalar::quote_identifier;
    match node.node.as_ref() {
        Some(N::InsertStmt(i)) => {
            let Some((view, _)) = relation_of(&i.relation) else {
                return Ok(None);
            };
            if !has_trigger(&view, "INSERT") {
                return Ok(None);
            }
            refuse_extras(!i.returning_list.is_empty(), i.on_conflict_clause.is_some())?;
            let columns = i
                .cols
                .iter()
                .filter_map(|c| match c.node.as_ref() {
                    Some(N::ResTarget(rt)) => Some(rt.name.clone()),
                    _ => None,
                })
                .collect();
            let source = match i.select_stmt.as_deref() {
                Some(s) => deparse_node(s)?,
                // DEFAULT VALUES: one row, every column its default (NULL on
                // a view without column defaults).
                None => "SELECT".to_string(),
            };
            Ok(Some(Statement::InsteadOf(InsteadOf {
                view,
                event: "INSERT".into(),
                query_sql: source,
                columns,
            })))
        }
        Some(N::UpdateStmt(u)) => {
            let Some((view, alias)) = relation_of(&u.relation) else {
                return Ok(None);
            };
            if !has_trigger(&view, "UPDATE") {
                return Ok(None);
            }
            refuse_extras(!u.returning_list.is_empty(), false)?;
            if !u.from_clause.is_empty() {
                return Err(Error::Unsupported(
                    "UPDATE ... FROM through an INSTEAD OF trigger".into(),
                ));
            }
            let mut columns = Vec::new();
            let mut values = Vec::new();
            for t in &u.target_list {
                let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                    continue;
                };
                if !rt.indirection.is_empty() {
                    return Err(Error::Unsupported(
                        "a subscripted SET through an INSTEAD OF trigger".into(),
                    ));
                }
                columns.push(rt.name.clone());
                values.push(format!(
                    "({})",
                    rt.val
                        .as_deref()
                        .map(deparse_expr)
                        .transpose()?
                        .unwrap_or_default()
                ));
            }
            let from = match &alias {
                Some(a) => format!("{} AS {}", q(&view), q(a)),
                None => q(&view),
            };
            let star = format!("{}.*", q(alias.as_deref().unwrap_or(&view)));
            let mut sql = format!("SELECT {star}, {} FROM {from}", values.join(", "));
            if let Some(w) = u.where_clause.as_deref() {
                sql.push_str(&format!(" WHERE {}", deparse_expr(w)?));
            }
            Ok(Some(Statement::InsteadOf(InsteadOf {
                view,
                event: "UPDATE".into(),
                query_sql: sql,
                columns,
            })))
        }
        Some(N::DeleteStmt(d)) => {
            let Some((view, alias)) = relation_of(&d.relation) else {
                return Ok(None);
            };
            if !has_trigger(&view, "DELETE") {
                return Ok(None);
            }
            refuse_extras(!d.returning_list.is_empty(), false)?;
            if !d.using_clause.is_empty() {
                return Err(Error::Unsupported(
                    "DELETE ... USING through an INSTEAD OF trigger".into(),
                ));
            }
            let from = match &alias {
                Some(a) => format!("{} AS {}", q(&view), q(a)),
                None => q(&view),
            };
            let mut sql = format!("SELECT * FROM {from}");
            if let Some(w) = d.where_clause.as_deref() {
                sql.push_str(&format!(" WHERE {}", deparse_expr(w)?));
            }
            Ok(Some(Statement::InsteadOf(InsteadOf {
                view,
                event: "DELETE".into(),
                query_sql: sql,
                columns: Vec::new(),
            })))
        }
        _ => Ok(None),
    }
}

fn refuse_extras(returning: bool, on_conflict: bool) -> Result<()> {
    if returning {
        return Err(Error::Unsupported(
            "RETURNING through an INSTEAD OF trigger".into(),
        ));
    }
    if on_conflict {
        return Err(Error::Unsupported(
            "ON CONFLICT through an INSTEAD OF trigger".into(),
        ));
    }
    Ok(())
}
