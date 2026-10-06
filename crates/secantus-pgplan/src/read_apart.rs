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
/// `INTO`, no subquery (one runs at planning, in the block), no
/// set-returning FROM function, no CTE, no SQL value function (`now()`'s
/// spelled forms) and no function call outside [`PURE`]. Catalog relations
/// refuse it. `None` otherwise.
pub fn relations(sql: &str) -> Option<Vec<String>> {
    let parsed = parse_tree(sql).ok()?;
    let [raw] = parsed.stmts.as_slice() else {
        return None;
    };
    let node = raw.stmt.as_ref()?.node.as_ref()?;
    let N::SelectStmt(sel) = node else {
        return None;
    };
    if sel.into_clause.is_some() || sel.with_clause.is_some() || !sel.locking_clause.is_empty() {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for (n, _, _, _) in node.nodes() {
        match n {
            pg_query::NodeRef::LockingClause(_)
            | pg_query::NodeRef::IntoClause(_)
            | pg_query::NodeRef::SubLink(_)
            | pg_query::NodeRef::RangeSubselect(_)
            | pg_query::NodeRef::RangeFunction(_)
            | pg_query::NodeRef::RangeTableSample(_)
            | pg_query::NodeRef::CommonTableExpr(_)
            | pg_query::NodeRef::SqlvalueFunction(_)
            | pg_query::NodeRef::WithClause(_) => return None,
            pg_query::NodeRef::FuncCall(f) => {
                let parts: Vec<&str> = f
                    .funcname
                    .iter()
                    .filter_map(|p| match p.node.as_ref() {
                        Some(N::String(s)) => Some(s.sval.as_str()),
                        _ => None,
                    })
                    .collect();
                let ok = match parts.as_slice() {
                    [name] | ["pg_catalog", name] => PURE.contains(name),
                    _ => false,
                };
                if !ok || f.over.is_some() {
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
    (!out.is_empty()).then_some(out)
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
    fn anything_else_is_not() {
        for sql in [
            "select 1",
            "select * from a for update",
            "select * from a where x in (select y from b)",
            "select nextval('s') from a",
            "select f(x) from a",
            "select now(), x from a",
            "select current_timestamp from a",
            "with w as (select 1) select * from a, w",
            "select * from (select * from a) s",
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
