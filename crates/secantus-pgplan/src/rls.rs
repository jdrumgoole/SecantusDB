//! Row-level security, the planner half: the executor publishes, for the
//! EFFECTIVE role and each table RLS applies to, the combined policy
//! conditions; this module writes them into the statement.
//!
//! * a read of the table (`FROM t`) reads `(SELECT * FROM ONLY t WHERE
//!   <select filter>) AS t` -- the `ONLY` marks the inner reference as
//!   already filtered;
//! * `UPDATE` / `DELETE` AND their filter into the WHERE;
//! * `INSERT` / `UPDATE` hold their new rows to the policies' `WITH CHECK`,
//!   through the same channel a view's CHECK OPTION uses, under a name the
//!   executor reports as PostgreSQL's 42501.

use super::*;

/// One table's combined policy conditions for the effective role; `None`
/// where RLS does not restrict that command.
#[derive(Debug, Clone, Default)]
pub struct RlsTable {
    pub table: String,
    pub select: Option<String>,
    pub update: Option<String>,
    pub delete: Option<String>,
    pub insert_check: Option<String>,
    pub update_check: Option<String>,
}

/// The check-option name marking a row-level-security check.
pub const CHECK_PREFIX: &str = "\u{1}rls:";

thread_local! {
    static RLS: std::cell::RefCell<Vec<RlsTable>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Per view, the tables RLS restricts for the view's OWNER: a table read
    /// through a view is subject to the owner's policies (and exempt when
    /// the owner is), not the caller's.
    static VIEW_RLS: std::cell::RefCell<Vec<(String, Vec<RlsTable>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// The set in force while a view's body is expanded.
    static INSIDE_VIEW: std::cell::RefCell<Vec<Vec<RlsTable>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the tables RLS restricts, for the statements that follow.
pub fn set_rls(tables: Vec<RlsTable>) {
    RLS.with(|t| *t.borrow_mut() = tables);
}

/// Install, per view, the tables RLS restricts for that view's owner.
pub fn set_view_rls(views: Vec<(String, Vec<RlsTable>)>) {
    VIEW_RLS.with(|v| *v.borrow_mut() = views);
}

fn entry(table: &str) -> Option<RlsTable> {
    let inner = INSIDE_VIEW.with(|v| {
        v.borrow()
            .last()
            .map(|set| set.iter().find(|e| e.table == table).cloned())
    });
    match inner {
        Some(found) => found,
        None => RLS.with(|t| t.borrow().iter().find(|e| e.table == table).cloned()),
    }
}

/// Is any table restricted for the current statement, directly or through
/// a view?
pub(crate) fn active() -> bool {
    RLS.with(|t| !t.borrow().is_empty())
        || VIEW_RLS.with(|v| v.borrow().iter().any(|(_, set)| !set.is_empty()))
}

/// Run `f` -- the expansion of view `view`'s body -- under the view owner's
/// row-level security.
pub(crate) fn within_view<R>(view: &str, f: impl FnOnce() -> R) -> R {
    let set = VIEW_RLS.with(|v| {
        v.borrow()
            .iter()
            .find(|(n, _)| n == view)
            .map(|(_, set)| set.clone())
    });
    let Some(set) = set else {
        return f();
    };
    struct Pop;
    impl Drop for Pop {
        fn drop(&mut self) {
            INSIDE_VIEW.with(|v| {
                v.borrow_mut().pop();
            });
        }
    }
    INSIDE_VIEW.with(|v| v.borrow_mut().push(set));
    let _pop = Pop;
    f()
}

fn condition(sql: &str) -> Result<pg_query::protobuf::Node> {
    domains::parse_default_sql(sql)
}

fn and_into(
    where_clause: &mut Option<Box<pg_query::protobuf::Node>>,
    extra: pg_query::protobuf::Node,
) {
    *where_clause = Some(Box::new(match where_clause.take() {
        None => extra,
        Some(w) => pg_query::protobuf::Node {
            node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
                boolop: BoolExprType::AndExpr as i32,
                args: vec![*w, extra],
                location: -1,
                ..Default::default()
            }))),
        },
    }));
}

/// `FROM t` over a restricted table, as the filtered subquery.
pub(crate) fn expand_from(item: &mut pg_query::protobuf::Node) -> Result<()> {
    let Some(N::RangeVar(r)) = item.node.as_mut() else {
        return Ok(());
    };
    if !r.inh || !(r.schemaname.is_empty() || r.schemaname == "public") {
        return Ok(());
    }
    let Some(filter) = entry(&r.relname).and_then(|e| e.select) else {
        // Read through a view whose owner RLS does not restrict: marked
        // `ONLY`, as a filtered read is, so a later planning pass over the
        // expanded statement does not apply the CALLER's policies to it.
        let caller_restricted = RLS.with(|t| t.borrow().iter().any(|e| e.table == r.relname));
        if INSIDE_VIEW.with(|v| !v.borrow().is_empty()) && caller_restricted {
            r.inh = false;
        }
        return Ok(());
    };
    let q = scalar::quote_identifier;
    let sql = format!("SELECT * FROM ONLY {} WHERE {filter}", q(&r.relname));
    let N::SelectStmt(body) = parse_one(&sql)? else {
        return Err(Error::Internal("row-level security filter".into()));
    };
    let alias = r
        .alias
        .clone()
        .filter(|a| !a.aliasname.is_empty())
        .unwrap_or(pg_query::protobuf::Alias {
            aliasname: r.relname.clone(),
            colnames: Vec::new(),
        });
    item.node = Some(N::RangeSubselect(Box::new(
        pg_query::protobuf::RangeSubselect {
            lateral: false,
            subquery: Some(Box::new(pg_query::protobuf::Node {
                node: Some(N::SelectStmt(body)),
            })),
            alias: Some(alias),
        },
    )));
    Ok(())
}

/// The DML half: filters into UPDATE / DELETE, checks for new rows.
pub(crate) fn rewrite_dml(node: &mut pg_query::protobuf::Node) -> Result<()> {
    match node.node.as_mut() {
        Some(N::UpdateStmt(u)) => {
            let Some(table) = u.relation.as_ref().map(|r| r.relname.clone()) else {
                return Ok(());
            };
            let Some(e) = entry(&table) else {
                return Ok(());
            };
            // A statement that READS the rows (a WHERE, a RETURNING, or a SET
            // over a column) must see them, so the SELECT policies apply too.
            let reads = u.where_clause.is_some()
                || !u.returning_list.is_empty()
                || u.target_list.iter().any(|t| match t.node.as_ref() {
                    Some(N::ResTarget(rt)) => rt.val.as_deref().is_some_and(references_columns),
                    _ => false,
                });
            if reads {
                if let Some(f) = &e.select {
                    and_into(&mut u.where_clause, condition(f)?);
                }
            }
            if let Some(f) = &e.update {
                and_into(&mut u.where_clause, condition(f)?);
            }
            if let Some(c) = &e.update_check {
                view_dml::add_check(format!("{CHECK_PREFIX}{table}"), c.clone());
            }
        }
        Some(N::DeleteStmt(d)) => {
            let Some(table) = d.relation.as_ref().map(|r| r.relname.clone()) else {
                return Ok(());
            };
            let Some(e) = entry(&table) else {
                return Ok(());
            };
            if d.where_clause.is_some() || !d.returning_list.is_empty() {
                if let Some(f) = &e.select {
                    and_into(&mut d.where_clause, condition(f)?);
                }
            }
            if let Some(f) = &e.delete {
                and_into(&mut d.where_clause, condition(f)?);
            }
        }
        Some(N::InsertStmt(i)) => {
            let Some(table) = i.relation.as_ref().map(|r| r.relname.clone()) else {
                return Ok(());
            };
            if let Some(c) = entry(&table).and_then(|e| e.insert_check) {
                view_dml::add_check(format!("{CHECK_PREFIX}{table}"), c);
            }
        }
        _ => {}
    }
    Ok(())
}
