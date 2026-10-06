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

/// The built-in function calls `sql` makes, each as its name and the
/// argument types of the overload chosen (`None` when undetermined), for
/// the EXECUTE check PostgreSQL makes at every call.
pub fn builtin_calls(
    sql: &str,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Vec<(String, Option<Vec<String>>)> {
    let Ok(parsed) = parse_tree(sql) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for raw in &parsed.stmts {
        if let Some(node) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) {
            out.extend(crate::optype::builtin_calls(node, lookup));
        }
    }
    out
}

/// `(relation, privilege)` for every relation `sql` names, before view
/// expansion. The target of an INSERT / UPDATE / DELETE takes that
/// privilege (and SELECT too when the statement reads its rows to choose
/// them); every other relation it reads takes SELECT. A CTE's name is not a
/// relation, and a view's DEFINITION is not checked here: `CREATE VIEW`
/// over a table the creator cannot read succeeds, and is refused on use.
pub fn sql_relations(sql: &str) -> Vec<(String, &'static str)> {
    // Asked for every statement (the table locks, the privilege check): the
    // parse-tree walk is remembered per thread by text.
    thread_local! {
        static MEMO: std::cell::RefCell<std::collections::HashMap<String, Vec<(String, &'static str)>>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    // The names resolved through the search path, remembered too while
    // nothing resolution reads has been installed anew on this thread
    // (`schemas::resolve_epoch`): resolving was most of this call.
    type Resolved = std::collections::HashMap<String, (u64, Vec<(String, &'static str)>)>;
    thread_local! {
        static RESOLVED: std::cell::RefCell<Resolved> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let epoch = crate::schemas::resolve_epoch();
    if let Some(hit) = RESOLVED.with(|m| {
        m.borrow()
            .get(sql)
            .filter(|(e, _)| *e == epoch)
            .map(|(_, v)| v.clone())
    }) {
        return hit;
    }
    // The names are resolved through the search path afterwards: only the
    // parse-tree walk is a function of the text alone.
    let raw = memoised(&MEMO, sql, sql_relations_uncached);
    let mut out = Vec::with_capacity(raw.len());
    for (r, p) in raw {
        let (schema, name) = r.split_once('\u{1f}').unwrap_or(("", r.as_str()));
        push(&mut out, &key_parts(schema, name), p);
    }
    // Resolving installs nothing, so the epoch read above still holds.
    RESOLVED.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() >= 512 {
            m.clear();
        }
        m.insert(sql.to_string(), (epoch, out.clone()));
    });
    out
}

/// A relation as written -- `schema<U+001F>name` -- for `sql_relations`'
/// memo, resolved by `key_parts` on every call.
fn raw_key(r: &pg_query::protobuf::RangeVar) -> String {
    format!("{}\u{1f}{}", r.schemaname, r.relname)
}

/// `f(sql)`, remembered in `memo` (at most `MEMO_MAX` texts; emptied when
/// full).
fn memoised<V: Clone>(
    memo: &'static std::thread::LocalKey<std::cell::RefCell<std::collections::HashMap<String, V>>>,
    sql: &str,
    f: impl FnOnce(&str) -> V,
) -> V {
    const MEMO_MAX: usize = 512;
    if let Some(v) = memo.with(|m| m.borrow().get(sql).cloned()) {
        return v;
    }
    let v = f(sql);
    memo.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() >= MEMO_MAX {
            m.clear();
        }
        m.insert(sql.to_string(), v.clone());
    });
    v
}

fn sql_relations_uncached(sql: &str) -> Vec<(String, &'static str)> {
    let Ok(parsed) = parse_tree(sql) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for raw in &parsed.stmts {
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
                    targets.push((raw_key(r), "INSERT"));
                    let updates = i.on_conflict_clause.as_ref().is_some_and(|c| {
                        c.action == pg_query::protobuf::OnConflictAction::OnconflictUpdate as i32
                    });
                    if updates {
                        targets.push((raw_key(r), "UPDATE"));
                    }
                    if !i.returning_list.is_empty() {
                        targets.push((raw_key(r), "SELECT"));
                    }
                }
            }
            Some(N::UpdateStmt(u)) => {
                if let Some(r) = &u.relation {
                    targets.push((raw_key(r), "UPDATE"));
                    if u.where_clause.is_some() || !u.returning_list.is_empty() {
                        targets.push((raw_key(r), "SELECT"));
                    }
                }
            }
            Some(N::DeleteStmt(d)) => {
                if let Some(r) = &d.relation {
                    targets.push((raw_key(r), "DELETE"));
                    if d.where_clause.is_some() || !d.returning_list.is_empty() {
                        targets.push((raw_key(r), "SELECT"));
                    }
                }
            }
            Some(N::TruncateStmt(t)) => {
                for r in &t.relations {
                    if let Some(N::RangeVar(r)) = r.node.as_ref() {
                        targets.push((raw_key(r), "TRUNCATE"));
                    }
                }
            }
            Some(N::CopyStmt(c)) => {
                if let Some(r) = &c.relation {
                    targets.push((raw_key(r), if c.is_from { "INSERT" } else { "SELECT" }));
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
                    push(&mut out, &raw_key(r), "SELECT");
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
    let Ok(parsed) = parse_tree(sql) else {
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
    for raw in &parsed.stmts {
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
    key_parts(&r.schemaname, &r.relname)
}

fn key_parts(s: &str, relname: &str) -> String {
    if s.is_empty() {
        let found = crate::schemas::resolve_unqualified(relname);
        return if found.starts_with("pg_temp_") {
            relname.to_string()
        } else {
            found
        };
    }
    if s == "pg_temp" || s.starts_with("pg_temp_") {
        return relname.to_string();
    }
    crate::schemas::relation_key(s, relname)
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
    thread_local! {
        static MEMO: std::cell::RefCell<std::collections::HashMap<String, Vec<(String, i32)>>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    memoised(&MEMO, sql, ddl_locks_uncached)
}

fn ddl_locks_uncached(sql: &str) -> Vec<(String, i32)> {
    use pg_query::protobuf::ObjectType;
    let Ok(parsed) = parse_tree(sql) else {
        return Vec::new();
    };
    let name = |r: &pg_query::protobuf::RangeVar| r.relname.clone();
    let mut out = Vec::new();
    for raw in &parsed.stmts {
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

/// The schema each object a `CREATE` statement in `sql` makes lands in --
/// the one it names, else the first usable schema on the search path -- for
/// the schema `CREATE` privilege PostgreSQL checks there. A temporary
/// object needs none.
pub fn sql_creation_schemas(sql: &str) -> Vec<String> {
    let Ok(parsed) = parse_tree(sql) else {
        return Vec::new();
    };
    let names_of = |list: &[pg_query::protobuf::Node]| -> Vec<String> {
        list.iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .collect()
    };
    let mut out = Vec::new();
    for raw in &parsed.stmts {
        let Some(stmt) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
            continue;
        };
        // `Some(schema written)` ("" when unqualified); `None`: no check.
        let target: Option<String> = match stmt {
            N::CreateStmt(c) => c.relation.as_ref().and_then(range_var_schema),
            N::ViewStmt(v) => v.view.as_ref().and_then(range_var_schema),
            N::CreateSeqStmt(c) => c.sequence.as_ref().and_then(range_var_schema),
            N::CreateTableAsStmt(c) => c
                .into
                .as_ref()
                .and_then(|i| i.rel.as_ref())
                .and_then(range_var_schema),
            N::CompositeTypeStmt(c) => c.typevar.as_ref().and_then(range_var_schema),
            N::CreateFunctionStmt(f) => {
                let names = names_of(&f.funcname);
                Some(if names.len() > 1 {
                    names[names.len() - 2].clone()
                } else {
                    String::new()
                })
            }
            N::CreateEnumStmt(e) => {
                let names = names_of(&e.type_name);
                Some(if names.len() > 1 {
                    names[names.len() - 2].clone()
                } else {
                    String::new()
                })
            }
            N::CreateDomainStmt(d) => {
                let names = names_of(&d.domainname);
                Some(if names.len() > 1 {
                    names[names.len() - 2].clone()
                } else {
                    String::new()
                })
            }
            _ => None,
        };
        let Some(written) = target else {
            continue;
        };
        if written == "pg_temp" || written.starts_with("pg_temp_") {
            continue;
        }
        let schema = if written.is_empty() {
            match crate::schemas::creation_schema() {
                Ok(s) => s,
                Err(_) => continue,
            }
        } else {
            written
        };
        if !out.contains(&schema) {
            out.push(schema);
        }
    }
    out
}

/// A CREATE target's written schema ("" when unqualified); `None` for a
/// temporary relation, which needs no schema privilege.
fn range_var_schema(r: &pg_query::protobuf::RangeVar) -> Option<String> {
    (r.relpersistence != "t").then(|| r.schemaname.clone())
}
