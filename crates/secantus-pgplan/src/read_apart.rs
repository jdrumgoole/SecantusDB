//! Which READ COMMITTED reads may run apart from their block.
//!
//! A READ COMMITTED block that has written keeps its WiredTiger snapshot; a
//! read of tables the block has NOT written can instead run in a fresh
//! read-only transaction of its own, which is exactly the per-statement
//! snapshot PostgreSQL takes, without replaying the block's write set.
//! That is sound only when the read cannot run user code, cannot have a
//! side effect, and cannot reach the block's own state: [`relations`] says
//! which stored relations a statement reads when it is such a read, and
//! `None` for anything else.

use super::*;

/// Built-in functions with no side effect and no dependence on the
/// transaction they run in. Anything not listed refuses the read-apart
/// path (a refusal only costs the replay it would have saved).
const PURE: &[&str] = &[
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "bool_and",
    "bool_or",
    "every",
    "string_agg",
    "array_agg",
    "lower",
    "upper",
    "length",
    "char_length",
    "octet_length",
    "abs",
    "round",
    "trunc",
    "floor",
    "ceil",
    "ceiling",
    "mod",
    "substr",
    "substring",
    "concat",
    "coalesce",
    "btrim",
    "ltrim",
    "rtrim",
    "replace",
    "left",
    "right",
    "md5",
];

/// The relations (as written, `schema.name` or `name`) a single plain
/// `SELECT` reads, when it is a read that may run apart: no row lock, no
/// `INTO`, no set-returning FROM function, no recursive or data-modifying
/// CTE, no SQL value function (`now()`'s spelled forms) and no function
/// call outside [`PURE`] -- at any depth. Subqueries and plain-SELECT CTEs
/// are allowed: their relations are listed too, and the server runs the
/// ones evaluated at planning in the same snapshot as the statement
/// (`isolate_for_planning`). Catalog relations
/// refuse it. `None` otherwise.
pub fn relations(sql: &str) -> Option<Vec<String>> {
    relations_with(sql, &|_: &str| None)
}

/// What a statement's function calls read: for a name as written
/// (`schema.name` or `name`), `None` when no user function has it, else
/// `Some(None)` when one may not run apart (a VOLATILE one, one that may
/// write, one whose body this server cannot see through) and
/// `Some(Some(relations))` for the relations it reads. A user function's
/// name refuses or answers even where [`PURE`] lists it.
pub trait UserReads: Fn(&str) -> Option<Option<Vec<String>>> {}
impl<F: Fn(&str) -> Option<Option<Vec<String>>>> UserReads for F {}

/// [`relations`], where a function call outside [`PURE`] is judged by
/// `user` ([`UserReads`]).
pub fn relations_with(sql: &str, user: &dyn UserReads) -> Option<Vec<String>> {
    let out = reads_with(sql, user)?;
    (!out.is_empty()).then_some(out)
}

/// [`relations_with`] for a function BODY's statement: a SELECT with no
/// FROM (`select $1 + 1`) reads nothing and is still a read.
pub fn body_relations_with(sql: &str, user: &dyn UserReads) -> Option<Vec<String>> {
    reads_with(sql, user)
}

fn reads_with(sql: &str, user: &dyn UserReads) -> Option<Vec<String>> {
    let parsed = parse_tree(sql).ok()?;
    let [raw] = parsed.stmts.as_slice() else {
        return None;
    };
    let node = raw.stmt.as_ref()?.node.as_ref()?;
    let N::SelectStmt(sel) = node else {
        return None;
    };
    if sel.into_clause.is_some() || !sel.locking_clause.is_empty() {
        return None;
    }
    // A recursive WITH, at any depth (the parse tree's walk does not show
    // its clause): refused on the keyword, which only costs a replay.
    if sql
        .as_bytes()
        .windows(9)
        .any(|w| w.eq_ignore_ascii_case(b"recursive"))
    {
        return None;
    }
    // A WITH item's name is not a stored relation: every reference to one
    // (anywhere -- a CTE is visible in its subqueries) is skipped below.
    let mut ctes: Vec<String> = Vec::new();
    for (n, _, _, _) in node.nodes() {
        match n {
            // The walk does not enter a SELECT's locking or INTO clause:
            // checked on each nested SELECT itself.
            pg_query::NodeRef::SelectStmt(s)
                if !s.locking_clause.is_empty() || s.into_clause.is_some() =>
            {
                return None;
            }
            pg_query::NodeRef::CommonTableExpr(c) => {
                // Only a plain SELECT body: a data-modifying WITH item writes.
                match c.ctequery.as_deref().and_then(|q| q.node.as_ref()) {
                    Some(N::SelectStmt(_)) => ctes.push(c.ctename.clone()),
                    _ => return None,
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<String> = Vec::new();
    for (n, _, _, _) in node.nodes() {
        match n {
            pg_query::NodeRef::LockingClause(_)
            | pg_query::NodeRef::IntoClause(_)
            | pg_query::NodeRef::RangeFunction(_)
            | pg_query::NodeRef::RangeTableSample(_)
            | pg_query::NodeRef::SqlvalueFunction(_) => return None,
            pg_query::NodeRef::FuncCall(f) => {
                let parts: Vec<&str> = f
                    .funcname
                    .iter()
                    .filter_map(|p| match p.node.as_ref() {
                        Some(N::String(s)) => Some(s.sval.as_str()),
                        _ => None,
                    })
                    .collect();
                if f.over.is_some() {
                    return None;
                }
                if let Some(reads) = user(&parts.join(".")) {
                    for name in reads? {
                        if !out.contains(&name) {
                            out.push(name);
                        }
                    }
                    continue;
                }
                let ok = match parts.as_slice() {
                    [name] | ["pg_catalog", name] => PURE.contains(name),
                    _ => false,
                };
                if !ok {
                    return None;
                }
            }
            pg_query::NodeRef::RangeVar(r) => {
                if matches!(r.schemaname.as_str(), "pg_catalog" | "information_schema")
                    || r.relname.starts_with("pg_")
                    || !r.catalogname.is_empty()
                {
                    return None;
                }
                if r.schemaname.is_empty() && ctes.contains(&r.relname) {
                    continue;
                }
                let name = if r.schemaname.is_empty() {
                    r.relname.clone()
                } else {
                    format!("{}.{}", r.schemaname, r.relname)
                };
                if !out.contains(&name) {
                    out.push(name);
                }
            }
            _ => {}
        }
    }
    Some(out)
}

/// Does `sql` hold a subquery or a WITH item anywhere -- something the
/// planner may RUN while planning? A cheap textual test first: no `select`
/// beyond a SELECT's own keyword, and no `with`, means no subquery.
pub fn has_subquery(sql: &str) -> bool {
    // Run on every statement, so no allocation on the way to "no".
    let bytes = sql.as_bytes();
    let count = |word: &[u8]| {
        bytes
            .windows(word.len())
            .filter(|w| w.eq_ignore_ascii_case(word))
            .take(2)
            .count()
    };
    // A SELECT's own keyword is one; any other statement has none of its own.
    let own = usize::from(
        sql.trim_start()
            .as_bytes()
            .get(..6)
            .is_some_and(|w| w.eq_ignore_ascii_case(b"select")),
    );
    if count(b"select") <= own && count(b"with") == 0 {
        return false;
    }
    let Ok(parsed) = parse_tree(sql) else {
        return false;
    };
    parsed.stmts.iter().any(|raw| {
        raw.stmt
            .as_ref()
            .and_then(|s| s.node.as_ref())
            .is_some_and(|node| {
                node.nodes().into_iter().any(|(n, _, _, _)| {
                    matches!(
                        n,
                        pg_query::NodeRef::SubLink(_)
                            | pg_query::NodeRef::RangeSubselect(_)
                            | pg_query::NodeRef::CommonTableExpr(_)
                    )
                })
            })
    })
}

#[cfg(test)]
mod read_apart_tests {
    use super::relations;

    #[test]
    fn joins_and_aggregates_are_reads_apart() {
        assert_eq!(
            relations("select a.x, count(*) from a join b on a.id = b.id group by a.x order by lower(a.x)"),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(
            relations("select * from s.t"),
            Some(vec!["s.t".to_string()])
        );
    }

    #[test]
    fn has_subquery_finds_them() {
        assert!(super::has_subquery("select (select 1) from a"));
        assert!(super::has_subquery("with w as (select 1) select * from w"));
        assert!(super::has_subquery(
            "update a set x = (select max(y) from b)"
        ));
        assert!(!super::has_subquery("select * from a where id = 1"));
        assert!(!super::has_subquery("select 'select' from a"));
    }

    #[test]
    fn subqueries_and_ctes_list_their_relations() {
        assert_eq!(
            relations("select * from a where x in (select y from b)"),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(
            relations("with w as (select max(y) m from b) select m, (select sum(x) from a) from w"),
            Some(vec!["b".to_string(), "a".to_string()])
        );
        assert_eq!(
            relations("select sum(x) from (select x from a) s"),
            Some(vec!["a".to_string()])
        );
    }

    #[test]
    fn anything_else_is_not() {
        for sql in [
            "select 1",
            "select * from a for update",
            "select nextval('s') from a",
            "select f(x) from a",
            "select now(), x from a",
            "select current_timestamp from a",
            "with recursive w as (select 1) select * from a, w",
            "with w as (insert into b values (1) returning 1) select * from a, w",
            "select * from a where x in (select nextval('s') from b)",
            "select * from a where x in (select y from b for update)",
            "with w as (select * from b for update) select * from a, w",
            "select * from a where x = (select max(y) from b where y = f(1))",
            "select * from pg_class",
            "select row_number() over () from a",
            "insert into a values (1)",
            "select * from a; select * from b",
            "select * into z from a",
        ] {
            assert_eq!(relations(sql), None, "{sql}");
        }
    }
}
