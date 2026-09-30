//! Which tables a planned statement touches, and the privilege each needs:
//! the executor checks them for a role that is neither a superuser nor the
//! table's owner, as PostgreSQL's `ExecCheckRTPerms` does.
//!
//! Only the FROM tree is walked: a subquery in an expression is run while the
//! statement is PLANNED, through the executor, and checked there.

use super::*;

/// `(table, privilege)` pairs, privilege being `SELECT` / `INSERT` /
/// `UPDATE` / `DELETE` / `TRUNCATE`.
pub fn statement_relations(stmt: &Statement) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    collect(stmt, &mut out);
    out
}

fn push(out: &mut Vec<(String, &'static str)>, table: &str, privilege: &'static str) {
    if !table.is_empty() && !out.iter().any(|(t, p)| t == table && *p == privilege) {
        out.push((table.to_string(), privilege));
    }
}

fn collect_join(node: &joins::JoinNode, out: &mut Vec<(String, &'static str)>) {
    match node {
        joins::JoinNode::Leaf { plan, .. } => collect(plan, out),
        joins::JoinNode::Join { left, right, .. } => {
            collect_join(left, out);
            collect_join(right, out);
        }
        _ => {}
    }
}

fn collect(stmt: &Statement, out: &mut Vec<(String, &'static str)>) {
    match stmt {
        Statement::Select(s) => {
            push(out, &s.table, "SELECT");
            if let Some(sub) = &s.sub {
                collect(&sub.plan, out);
            }
            if let Some(j) = &s.join {
                push(out, &j.left.0, "SELECT");
                push(out, &j.right.0, "SELECT");
                if let Some(ls) = &j.left_sub {
                    collect(ls, out);
                }
            }
        }
        Statement::Aggregate(a) => {
            push(out, &a.table, "SELECT");
            if let Some(sub) = &a.sub {
                collect(&sub.plan, out);
            }
            if let Some(j) = &a.join {
                push(out, &j.left.0, "SELECT");
                push(out, &j.right.0, "SELECT");
                if let Some(ls) = &j.left_sub {
                    collect(ls, out);
                }
            }
        }
        Statement::JoinRows(j) => collect_join(&j.tree, out),
        Statement::SetOp(set) => {
            collect(&set.left, out);
            collect(&set.right, out);
        }
        Statement::Insert(i) => {
            push(out, &i.table, "INSERT");
            if let Some(src) = &i.source {
                collect(src, out);
            }
            if matches!(
                i.on_conflict.as_ref().map(|c| &c.action),
                Some(ConflictAction::Update { .. })
            ) {
                push(out, &i.table, "UPDATE");
            }
        }
        Statement::Update(u) => {
            push(out, &u.table, "UPDATE");
            // Reading the rows to choose them needs SELECT too.
            if !u.filter.is_empty() || u.residual.is_some() {
                push(out, &u.table, "SELECT");
            }
        }
        Statement::Delete(d) => {
            push(out, &d.table, "DELETE");
            if !d.filter.is_empty() || d.residual.is_some() {
                push(out, &d.table, "SELECT");
            }
        }
        Statement::Truncate { tables, .. } => {
            for t in tables {
                push(out, t, "TRUNCATE");
            }
        }
        Statement::CopyFrom(c) => push(out, &c.table, "INSERT"),
        Statement::CopyTo(c) => match &c.query {
            Some(q) => collect(q, out),
            None => push(out, &c.table, "SELECT"),
        },
        _ => {}
    }
}
