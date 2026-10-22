//! PostgreSQL's own views over its catalogs (RP-070): `information_schema`'s
//! and the `pg_catalog` views the server does not compute itself, each read
//! through PostgreSQL 15.19's definition (`pg15_system_views.tsv`, written by
//! `tools/probes/pg15_system_catalog_dump.py`) as the rewriter reads any view.
//! What they show is what the server's catalogs hold.

use std::collections::HashMap;
use std::sync::OnceLock;

const VIEWS: &str = include_str!("pg15_system_views.tsv");

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// `schema.view` -> (oid, definition).
fn views() -> &'static HashMap<String, (i64, String)> {
    static MAP: OnceLock<HashMap<String, (i64, String)>> = OnceLock::new();
    MAP.get_or_init(|| {
        VIEWS
            .lines()
            .filter_map(|l| {
                let mut c = l.splitn(3, '\t');
                let name = c.next()?;
                let oid = c.next()?.parse().ok()?;
                Some((name.to_string(), (oid, unescape(c.next()?))))
            })
            .collect()
    })
}

/// The key a relation reference names: `pg_catalog` is searched before the
/// path, so an unqualified name reaches its views first.
fn key(schema: &str, name: &str) -> Option<String> {
    match schema {
        "" | "pg_catalog" => Some(format!("pg_catalog.{name}")),
        "information_schema" => Some(format!("information_schema.{name}")),
        _ => None,
    }
}

/// The definition of the system view `schema.name`, if it is one.
pub(crate) fn definition(schema: &str, name: &str) -> Option<&'static str> {
    views().get(&key(schema, name)?).map(|(_, d)| d.as_str())
}

/// A system view's relation oid, by the name its expansion is recorded under.
pub(crate) fn oid(view: &str) -> Option<i64> {
    let k = if view.contains('.') {
        view.to_string()
    } else {
        format!("pg_catalog.{view}")
    };
    views().get(&k).map(|(o, _)| *o)
}

/// The system relations refused by name (0A000), each with what the server
/// does not keep that PostgreSQL's answer is made of.
const REFUSED: &[(&str, &str)] = &[
    // The server's internals (Joe, 2026-10-22): no PostgreSQL memory
    // contexts, shared memory, background writer, WAL, SLRU or archiver.
    (
        "pg_backend_memory_contexts",
        "this server has no PostgreSQL memory contexts",
    ),
    (
        "pg_shmem_allocations",
        "this server has no PostgreSQL shared memory",
    ),
    ("pg_stat_bgwriter", "this server has no background writer"),
    ("pg_stat_wal", "this server writes no PostgreSQL WAL"),
    ("pg_stat_slru", "this server has no SLRU caches"),
    ("pg_stat_archiver", "this server has no WAL archiver"),
    // ANALYZE computes no column statistics here.
    (
        "pg_statistic",
        "ANALYZE does not compute column statistics on this server",
    ),
    (
        "pg_stats",
        "ANALYZE does not compute column statistics on this server",
    ),
    (
        "pg_statistic_ext_data",
        "ANALYZE does not compute extended statistics on this server",
    ),
    (
        "pg_stats_ext",
        "ANALYZE does not compute extended statistics on this server",
    ),
    (
        "pg_stats_ext_exprs",
        "ANALYZE does not compute extended statistics on this server",
    ),
    // No command reports its progress.
    (
        "pg_stat_progress_analyze",
        "commands do not report their progress on this server",
    ),
    (
        "pg_stat_progress_basebackup",
        "commands do not report their progress on this server",
    ),
    (
        "pg_stat_progress_cluster",
        "commands do not report their progress on this server",
    ),
    (
        "pg_stat_progress_copy",
        "commands do not report their progress on this server",
    ),
    (
        "pg_stat_progress_create_index",
        "commands do not report their progress on this server",
    ),
    (
        "pg_stat_progress_vacuum",
        "commands do not report their progress on this server",
    ),
];

/// 0A000 for a system relation refused by name.
pub(crate) fn refusal(schema: &str, name: &str) -> Option<crate::Error> {
    if !matches!(schema, "" | "pg_catalog") {
        return None;
    }
    REFUSED
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(n, why)| crate::Error::FeatureNotSupported(format!("{n} is not supported: {why}")))
}

/// Does the FROM list reference a system view, inside JOINs too?
pub(crate) fn in_from(from: &[pg_query::protobuf::Node]) -> bool {
    use pg_query::protobuf::node::Node as N;
    fn walk(n: &pg_query::protobuf::Node) -> bool {
        match n.node.as_ref() {
            Some(N::RangeVar(r)) => {
                r.catalogname.is_empty()
                    && (definition(&r.schemaname, &r.relname).is_some()
                        || refusal(&r.schemaname, &r.relname).is_some())
            }
            Some(N::JoinExpr(j)) => {
                j.larg.as_deref().is_some_and(walk) || j.rarg.as_deref().is_some_and(walk)
            }
            // A FROM subquery's own FROM, and its set operation's arms.
            Some(N::RangeSubselect(rs)) => {
                match rs.subquery.as_deref().and_then(|q| q.node.as_ref()) {
                    Some(N::SelectStmt(sel)) => select(sel),
                    _ => false,
                }
            }
            _ => false,
        }
    }
    fn select(s: &pg_query::protobuf::SelectStmt) -> bool {
        s.from_clause.iter().any(walk)
            || s.larg.as_deref().is_some_and(select)
            || s.rarg.as_deref().is_some_and(select)
    }
    from.iter().any(walk)
}
