//! `COLLATE` clauses the planner can drop before it looks at a statement.
//!
//! psql's `\d` writes `c.relname OPERATOR(pg_catalog.~) '^(t)$' COLLATE
//! pg_catalog.default` into every describe query, and a `COLLATE "C"` on an
//! ORDER BY key is a common way to pin byte order. Both name the collation
//! this server already applies to every string: text sorts and compares by
//! code point, which IS `C` / `POSIX`, and is what an uncollated column gets
//! here (`tasks/backlog.md`: text ORDER BY is by raw bytes). So the clause
//! changes nothing and is unwrapped to its argument once, at parse time;
//! any other collation name is left in place for the planner to refuse, since
//! honouring it would need ICU.

use pg_query::protobuf::{self as pb, node::Node as N, Node};

/// The collation names this server's byte order already satisfies.
fn is_byte_order(collname: &[Node]) -> bool {
    let parts: Vec<&str> = collname
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect();
    matches!(
        parts.as_slice(),
        ["default"]
            | ["C"]
            | ["POSIX"]
            | ["pg_catalog", "default"]
            | ["pg_catalog", "C"]
            | ["pg_catalog", "POSIX"]
    )
}

/// Unwrap every byte-order `COLLATE` under the statements of `tree`.
pub(crate) fn strip_byte_order_collations(tree: &mut pb::ParseResult) {
    for raw in &mut tree.stmts {
        if let Some(stmt) = raw.stmt.as_deref_mut() {
            crate::walk::walk(stmt, &mut unwrap_collate);
        }
    }
}

/// Unwrap `node` while it is a byte-order `COLLATE`; `true` when it was.
fn unwrap_collate(node: &mut Node) -> bool {
    let unwrap =
        matches!(node.node.as_ref(), Some(N::CollateClause(c)) if is_byte_order(&c.collname));
    if !unwrap {
        return false;
    }
    let Some(N::CollateClause(c)) = node.node.take() else {
        return false;
    };
    match c.arg {
        Some(arg) => {
            *node = *arg;
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stripped(sql: &str) -> String {
        let mut tree = pg_query::parse(sql).unwrap().protobuf;
        strip_byte_order_collations(&mut tree);
        pg_query::ParseResult::new(tree, String::new())
            .deparse()
            .unwrap()
    }

    #[test]
    fn byte_order_collations_are_dropped() {
        assert_eq!(
            stripped("select 1 from t where n ~ '^x$' collate pg_catalog.default"),
            "SELECT 1 FROM t WHERE n ~ '^x$'"
        );
        assert_eq!(
            stripped("select (a collate \"C\") collate \"POSIX\" from t order by b collate \"C\""),
            "SELECT a FROM t ORDER BY b"
        );
        assert_eq!(
            stripped("select 1 from t join (select x collate \"C\" as x from u) u on t.x = u.x"),
            "SELECT 1 FROM t JOIN (SELECT x AS x FROM u) u ON t.x = u.x"
        );
    }

    #[test]
    fn other_collations_stay() {
        assert_eq!(
            stripped("select a collate \"en_US\" from t"),
            "SELECT a COLLATE \"en_US\" FROM t"
        );
    }
}
