//! Which tables a statement touches, and the privilege each needs: the
//! executor checks them for a role that is neither a superuser nor the
//! table's owner, as PostgreSQL's `ExecCheckRTPerms` does.
//!
//! `sql_relations` reads them off the SQL as WRITTEN, before any view is
//! expanded and any subquery run: a view is checked as the view (its base
//! tables are the view owner's business, not the caller's), and a subquery
//! anywhere -- correlated ones included -- is checked with the rest of the
//! statement.

use super::*;

/// `(relation, privilege)` for every relation `sql` names, before view
/// expansion. The target of an INSERT / UPDATE / DELETE takes that
/// privilege (and SELECT too when the statement reads its rows to choose
/// them); every other relation it reads takes SELECT. A CTE's name is not a
/// relation, and a view's DEFINITION is not checked here: `CREATE VIEW`
/// over a table the creator cannot read succeeds, and is refused on use.
pub fn sql_relations(sql: &str) -> Vec<(String, &'static str)> {
    let Ok(parsed) = pg_query::parse(sql) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for raw in &parsed.protobuf.stmts {
        let Some(stmt) = raw.stmt.as_ref() else {
            continue;
        };
        let mut ctes: Vec<String> = Vec::new();
        let mut targets: Vec<(String, &'static str)> = Vec::new();
        match stmt.node.as_ref() {
            Some(N::ViewStmt(_)) | Some(N::CreateFunctionStmt(_)) | Some(N::RuleStmt(_)) => {
                continue
            }
            Some(N::InsertStmt(i)) => {
                if let Some(r) = &i.relation {
                    targets.push((r.relname.clone(), "INSERT"));
                    let updates = i.on_conflict_clause.as_ref().is_some_and(|c| {
                        c.action == pg_query::protobuf::OnConflictAction::OnconflictUpdate as i32
                    });
                    if updates {
                        targets.push((r.relname.clone(), "UPDATE"));
                    }
                    if !i.returning_list.is_empty() {
                        targets.push((r.relname.clone(), "SELECT"));
                    }
                }
            }
            Some(N::UpdateStmt(u)) => {
                if let Some(r) = &u.relation {
                    targets.push((r.relname.clone(), "UPDATE"));
                    if u.where_clause.is_some() || !u.returning_list.is_empty() {
                        targets.push((r.relname.clone(), "SELECT"));
                    }
                }
            }
            Some(N::DeleteStmt(d)) => {
                if let Some(r) = &d.relation {
                    targets.push((r.relname.clone(), "DELETE"));
                    if d.where_clause.is_some() || !d.returning_list.is_empty() {
                        targets.push((r.relname.clone(), "SELECT"));
                    }
                }
            }
            Some(N::TruncateStmt(t)) => {
                for r in &t.relations {
                    if let Some(N::RangeVar(r)) = r.node.as_ref() {
                        targets.push((r.relname.clone(), "TRUNCATE"));
                    }
                }
            }
            Some(N::CopyStmt(c)) => {
                if let Some(r) = &c.relation {
                    targets.push((
                        r.relname.clone(),
                        if c.is_from { "INSERT" } else { "SELECT" },
                    ));
                }
            }
            _ => {}
        }
        for (t, p) in targets {
            push(&mut out, &t, p);
        }
        let Some(inner) = stmt.node.as_ref() else {
            continue;
        };
        let nodes = inner.nodes();
        for (node, _, _, _) in &nodes {
            if let pg_query::NodeRef::CommonTableExpr(c) = node {
                ctes.push(c.ctename.clone());
            }
        }
        for (node, _, context, _) in &nodes {
            if let pg_query::NodeRef::RangeVar(r) = node {
                let catalog = matches!(r.schemaname.as_str(), "pg_catalog" | "information_schema");
                if *context == pg_query::Context::Select && !catalog && !ctes.contains(&r.relname) {
                    push(&mut out, &r.relname, "SELECT");
                }
            }
        }
    }
    out
}

fn push(out: &mut Vec<(String, &'static str)>, table: &str, privilege: &'static str) {
    if !table.is_empty() && !out.iter().any(|(t, p)| t == table && *p == privilege) {
        out.push((table.to_string(), privilege));
    }
}

/// The relations a DDL statement locks, with PostgreSQL's lock mode for each
/// (1 ACCESS SHARE .. 8 ACCESS EXCLUSIVE): `ALTER TABLE`, `DROP TABLE`, a
/// rename and `CLUSTER` take ACCESS EXCLUSIVE, so they wait for a session
/// still reading the table; `CREATE INDEX` takes SHARE (which a reader does
/// not block). Empty for any other statement.
pub fn ddl_locks(sql: &str) -> Vec<(String, i32)> {
    use pg_query::protobuf::ObjectType;
    let Ok(parsed) = pg_query::parse(sql) else {
        return Vec::new();
    };
    let name = |r: &pg_query::protobuf::RangeVar| r.relname.clone();
    let mut out = Vec::new();
    for raw in &parsed.protobuf.stmts {
        let Some(node) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
            continue;
        };
        match node {
            N::AlterTableStmt(a) if a.objtype == ObjectType::ObjectTable as i32 => {
                if let Some(r) = &a.relation {
                    out.push((name(r), 8));
                }
            }
            N::RenameStmt(r)
                if matches!(
                    ObjectType::try_from(r.rename_type),
                    Ok(ObjectType::ObjectTable | ObjectType::ObjectColumn)
                ) =>
            {
                if let Some(rel) = &r.relation {
                    out.push((name(rel), 8));
                }
            }
            N::ClusterStmt(c) => {
                if let Some(r) = &c.relation {
                    out.push((name(r), 8));
                }
            }
            N::IndexStmt(i) if !i.concurrent => {
                if let Some(r) = &i.relation {
                    out.push((name(r), 5));
                }
            }
            N::DropStmt(d) if d.remove_type == ObjectType::ObjectTable as i32 => {
                for o in &d.objects {
                    if let Some(N::List(l)) = o.node.as_ref() {
                        if let Some(N::String(s)) = l.items.last().and_then(|n| n.node.as_ref()) {
                            out.push((s.sval.clone(), 8));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}
