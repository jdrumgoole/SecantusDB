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
                    targets.push((key(r), "INSERT"));
                    let updates = i.on_conflict_clause.as_ref().is_some_and(|c| {
                        c.action == pg_query::protobuf::OnConflictAction::OnconflictUpdate as i32
                    });
                    if updates {
                        targets.push((key(r), "UPDATE"));
                    }
                    if !i.returning_list.is_empty() {
                        targets.push((key(r), "SELECT"));
                    }
                }
            }
            Some(N::UpdateStmt(u)) => {
                if let Some(r) = &u.relation {
                    targets.push((key(r), "UPDATE"));
                    if u.where_clause.is_some() || !u.returning_list.is_empty() {
                        targets.push((key(r), "SELECT"));
                    }
                }
            }
            Some(N::DeleteStmt(d)) => {
                if let Some(r) = &d.relation {
                    targets.push((key(r), "DELETE"));
                    if d.where_clause.is_some() || !d.returning_list.is_empty() {
                        targets.push((key(r), "SELECT"));
                    }
                }
            }
            Some(N::TruncateStmt(t)) => {
                for r in &t.relations {
                    if let Some(N::RangeVar(r)) = r.node.as_ref() {
                        targets.push((key(r), "TRUNCATE"));
                    }
                }
            }
            Some(N::CopyStmt(c)) => {
                if let Some(r) = &c.relation {
                    targets.push((key(r), if c.is_from { "INSERT" } else { "SELECT" }));
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
                    push(&mut out, &key(r), "SELECT");
                }
            }
        }
    }
    out
}

/// The columns of `table` a statement needs `privilege` on, for checking a
/// role that holds only COLUMN privileges (`GRANT SELECT (a) ON t`), as
/// PostgreSQL's `ExecCheckRTPerms` checks `selectedCols` / `insertedCols` /
/// `updatedCols`. SELECT: every column read anywhere in the statement, `*` or
/// a whole-row reference meaning all of them. INSERT: the target list (all
/// columns without one). UPDATE: the SET targets. `columns` is the table's
/// column list, which attributes an unqualified name.
pub fn sql_columns(sql: &str, table: &str, columns: &[String], privilege: &str) -> Vec<String> {
    let Ok(parsed) = pg_query::parse(sql) else {
        return columns.to_vec();
    };
    let mut out: Vec<String> = Vec::new();
    let add = |c: &str, out: &mut Vec<String>| {
        if columns.iter().any(|k| k == c) && !out.iter().any(|k| k == c) {
            out.push(c.to_string());
        }
    };
    let target_names = |list: &[pg_query::protobuf::Node]| -> Vec<String> {
        list.iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(N::ResTarget(r)) => Some(r.name.clone()),
                _ => None,
            })
            .collect()
    };
    for raw in &parsed.protobuf.stmts {
        let Some(stmt) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
            continue;
        };
        match (privilege, stmt) {
            ("INSERT", N::InsertStmt(i))
                if i.relation.as_ref().is_some_and(|r| key(r) == table) =>
            {
                if i.cols.is_empty() {
                    return columns.to_vec();
                }
                for c in target_names(&i.cols) {
                    add(&c, &mut out);
                }
            }
            ("UPDATE", N::UpdateStmt(u))
                if u.relation.as_ref().is_some_and(|r| key(r) == table) =>
            {
                for c in target_names(&u.target_list) {
                    add(&c, &mut out);
                }
            }
            ("UPDATE", N::InsertStmt(i))
                if i.relation.as_ref().is_some_and(|r| key(r) == table) =>
            {
                if let Some(oc) = &i.on_conflict_clause {
                    for c in target_names(&oc.target_list) {
                        add(&c, &mut out);
                    }
                }
            }
            ("SELECT", _) => {
                // The names this table goes by in the statement.
                let mut names = vec![table.to_string()];
                if let Some((_, bare)) = table.split_once('.') {
                    names.push(bare.to_string());
                }
                for (node, _, _, _) in stmt.nodes() {
                    if let pg_query::NodeRef::RangeVar(r) = node {
                        if key(r) == table {
                            if let Some(a) = &r.alias {
                                names.push(a.aliasname.clone());
                            }
                        }
                    }
                }
                for (node, _, _, _) in stmt.nodes() {
                    let pg_query::NodeRef::ColumnRef(c) = node else {
                        continue;
                    };
                    let parts: Vec<Option<&str>> = c
                        .fields
                        .iter()
                        .map(|f| match f.node.as_ref() {
                            Some(N::String(s)) => Some(s.sval.as_str()),
                            _ => None,
                        })
                        .collect();
                    match parts.as_slice() {
                        // `*`, or `t.*`
                        [None] => return columns.to_vec(),
                        [Some(q), None] if names.iter().any(|n| n == q) => {
                            return columns.to_vec();
                        }
                        // A bare name: a column, or the whole row.
                        [Some(n)] => {
                            if columns.iter().any(|k| k == n) {
                                add(n, &mut out);
                            } else if names.iter().any(|t| t == n) {
                                return columns.to_vec();
                            }
                        }
                        [.., Some(q), Some(n)] if names.iter().any(|t| t == q) => add(n, &mut out),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The catalog key of the relation `r` names: `schema.name` for a table of
/// a schema other than `public` (written qualified, or found first on the
/// search path), else the bare name -- so a privilege is checked on the
/// table the statement reads, not on whichever shares its bare name.
fn key(r: &pg_query::protobuf::RangeVar) -> String {
    let s = r.schemaname.as_str();
    if s.is_empty() {
        let found = crate::schemas::resolve_unqualified(&r.relname);
        return if found.starts_with("pg_temp_") {
            r.relname.clone()
        } else {
            found
        };
    }
    if s == "pg_temp" || s.starts_with("pg_temp_") {
        return r.relname.clone();
    }
    crate::schemas::relation_key(s, &r.relname)
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
