//! The rules `initdb` creates: every view of `system_views.sql` and
//! `information_schema.sql` (PostgreSQL 15.19's own files, vendored beside
//! this module), analysed as `initdb` runs them -- each query as the
//! single-user backend reads it, under `search_path=pg_catalog` until
//! `information_schema.sql` sets its own.

use super::catalog::{Catalog, PG_CATALOG_NAMESPACE};
use super::nodes::Oid;
use super::parse::{analyze_create_rule, analyze_create_view, split_initdb_file, AnalysisContext};
use super::{AnalysisError, RuleTrees};
use pg_query::protobuf::node::Node as N;

const SYSTEM_VIEWS: &str = include_str!("pg15_system_views.sql");
const INFORMATION_SCHEMA: &str = include_str!("pg15_information_schema.sql");

/// One system rule's trees: (relation oid, rule name, trees or why none).
pub type SystemRule = (Oid, String, Result<RuleTrees, AnalysisError>);

/// Analyse every rule the two files create.
pub fn system_rule_trees(cat: &Catalog) -> Vec<SystemRule> {
    let mut out = Vec::new();
    let mut path: Vec<Oid> = vec![PG_CATALOG_NAMESPACE];
    for file in [SYSTEM_VIEWS, INFORMATION_SCHEMA] {
        for chunk in split_initdb_file(file) {
            let Ok(parsed) = pg_query::parse(&chunk) else {
                continue;
            };
            for (i, raw) in parsed.protobuf.stmts.iter().enumerate() {
                let Some(node) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
                    continue;
                };
                match node {
                    N::VariableSetStmt(v) if v.name == "search_path" => {
                        path = v
                            .args
                            .iter()
                            .filter_map(|a| match a.node.as_ref() {
                                Some(N::AConst(c)) => match c.val.as_ref() {
                                    Some(pg_query::protobuf::a_const::Val::Sval(s)) => {
                                        cat.namespace_oid(&s.sval)
                                    }
                                    _ => None,
                                },
                                _ => None,
                            })
                            .collect();
                    }
                    N::ViewStmt(v) => {
                        let Some(rv) = v.view.as_ref() else { continue };
                        let Some(relid) = lookup(cat, &path, &rv.schemaname, &rv.relname) else {
                            continue;
                        };
                        let ctx = AnalysisContext {
                            catalog: cat,
                            search_path: path.clone(),
                            allow_system_table_mods: true,
                        };
                        out.push((
                            relid,
                            "_RETURN".to_string(),
                            analyze_create_view(&ctx, &chunk, i, relid),
                        ));
                    }
                    N::RuleStmt(r) => {
                        let Some(rv) = r.relation.as_ref() else {
                            continue;
                        };
                        let Some(relid) = lookup(cat, &path, &rv.schemaname, &rv.relname) else {
                            continue;
                        };
                        let ctx = AnalysisContext {
                            catalog: cat,
                            search_path: path.clone(),
                            allow_system_table_mods: true,
                        };
                        out.push((
                            relid,
                            r.rulename.clone(),
                            analyze_create_rule(&ctx, &chunk, i, relid),
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

/// The relation a `CREATE` names: in its schema, or the creation
/// namespace (the path's first).
fn lookup(cat: &Catalog, path: &[Oid], schema: &str, name: &str) -> Option<Oid> {
    let nsp = if schema.is_empty() {
        *path.first()?
    } else {
        cat.namespace_oid(schema)?
    };
    cat.rels_named(name).find(|r| r.nsp == nsp).map(|r| r.oid)
}
