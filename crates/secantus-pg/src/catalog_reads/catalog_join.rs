//! The join method PostgreSQL 15 picks for a join of two catalogs, and the
//! catalog reads that follow from it (RP-290).
//!
//! The planner's `add_paths_to_joinrel` weighs merge joins, nested loops
//! and hash joins over each side's cheapest scan; the walk prices both
//! sides as [`PgHandler::scan_estimate`] prices a catalog scan and lets
//! [`joincost::JoinRel`] keep the cheapest path, as `add_path` and
//! `set_cheapest` do. What the chosen method changes in the reads:
//!
//! - `create_scan_plan` gives a scan beneath a nested loop, or a hash
//!   join's outer side, or a merge join's side read in order without a
//!   Sort, its relation's whole row (`use_physical_tlist`: no
//!   `CP_SMALL_TLIST` from the parent), and `ExecInitResultTypeTL` builds
//!   that row's tuple descriptor, looking up every column's type
//!   (`TupleDescInitEntry`). A Sort, a Hash and a Material ask their input
//!   for a small target list: only the columns needed above.
//! - A nested loop whose inner side is unique for the join clause
//!   (`innerrel_is_unique`: a one-column unique index on its key) stops
//!   reading the inner for an outer row at its first match
//!   (`single_match`), and starts no inner scan for an outer side with no
//!   rows. Walked for a sequential scan of `pg_namespace`, whose heap order
//!   `initdb` fixed, below a scan of `pg_class` whose rows the walk knows.
//!
//! Not walked: a join of more than two relations, a join with a LIMIT,
//! grouping or an aggregate, an ORDER BY a join key, a restriction the
//! catalog estimator does not price, a side whose cheapest path is a
//! sequential scan with a filter the estimate leaves out, and a full index
//! scan in the join key's order (a merge join's presorted input, which
//! the estimator does not price). There the reads stay as the walk counted
//! them before: no whole rows, the inner scanned once in full.

use secantus_pgplan::costsize::{clamp_row_est, CPU_OPERATOR_COST};
use secantus_pgplan::joincost::{self, JoinMethod, JoinRel, JoinSide};

use super::*;

/// `pg_namespace`.
const NAMESPACE_RELATION: i64 = 2615;

/// `function_selectivity`'s answer for a function with no support
/// function: `pg_table_is_visible(oid)` as a restriction.
const DEFAULT_FUNCTION_SELECTIVITY: f64 = 0.333_333_3;

/// How a side of the join is read: its whole row (`use_physical_tlist`)
/// or the columns needed above it (`CP_SMALL_TLIST`).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Read {
    Whole,
    Small,
}

/// A side's costing and whether its cheapest scan is index-only, whose
/// whole row is its index's (`indextlist`), as the walk already reads it.
struct Side {
    join: JoinSide,
    index_only: bool,
}

/// The chosen plan's sides: `(outer, inner)` with the side's index in
/// `scope.rels` and how each is read, and whether the join is a plain
/// nested loop (no Material over its inner).
struct Plan {
    outer: (usize, Read),
    inner: (usize, Read),
    plain_nest_loop: bool,
}

impl Model<'_> {
    /// The catalog reads a two-catalog join's method decides, walked after
    /// the planner's base relations (`clauses`: the join's merge-joinable
    /// clauses).
    pub(super) fn catalog_join(
        &mut self,
        s: &pb::SelectStmt,
        scope: &Scope,
        clauses: &[MergeClause],
        aggs_before: usize,
    ) {
        let Some((left, key)) = self.join_shape(s, scope, clauses, aggs_before) else {
            return;
        };
        let (a, b) = (&scope.rels[0].1, &scope.rels[1].1);
        let (a_key, b_key) = if key.1 .0 == a.oid {
            (key.1 .1, key.2 .1)
        } else {
            (key.2 .1, key.1 .1)
        };
        let (Some(sa), Some(sb)) = (self.join_side(a, a_key), self.join_side(b, b_key)) else {
            return;
        };
        let Some(plan) = choose(&sa.join, &sb.join, left) else {
            return;
        };
        let sides = [&sa, &sb];
        // `ExecInitNode`: the join's outer side, then its inner.
        for (i, read) in [plan.outer, plan.inner] {
            let rel = scope.rels[i].1.clone();
            match read {
                // An index-only scan's row is its index's columns, which
                // `plan_rel` looked up as the scan's own.
                Read::Whole if sides[i].index_only => {}
                Read::Whole => {
                    for (_, _, typ) in &rel.columns {
                        self.type_oid(*typ);
                    }
                }
                Read::Small => {
                    let needs = self.needs.get(&rel.oid).cloned().unwrap_or_default();
                    for att in needs.target.iter().chain(&needs.joins) {
                        if let Some((_, _, typ)) = rel.columns.iter().find(|c| c.0 == *att) {
                            self.type_oid(*typ);
                        }
                    }
                }
            }
        }
        let inner_unique = sides[plan.inner.0].join.unique;
        if plan.plain_nest_loop && inner_unique {
            let outer = scope.rels[plan.outer.0].1.clone();
            let inner = scope.rels[plan.inner.0].1.clone();
            self.single_match_scans(&outer, &inner);
        }
    }

    /// A FROM of one inner or LEFT join of two catalogs on one
    /// merge-joinable equality, with a WHERE of restrictions on one side
    /// each, under nothing that changes the paths kept: `(left, clause)`.
    fn join_shape(
        &self,
        s: &pb::SelectStmt,
        scope: &Scope,
        clauses: &[MergeClause],
        aggs_before: usize,
    ) -> Option<(bool, MergeClause)> {
        let [f] = s.from_clause.as_slice() else {
            return None;
        };
        let Some(N::JoinExpr(j)) = f.node.as_ref() else {
            return None;
        };
        let left = if j.jointype == pb::JoinType::JoinLeft as i32 {
            true
        } else if j.jointype == pb::JoinType::JoinInner as i32 {
            false
        } else {
            return None;
        };
        let range_var = |n: &Option<Box<pb::Node>>| matches!(node(n), Some(N::RangeVar(_)));
        if j.is_natural || !j.using_clause.is_empty() || !range_var(&j.larg) || !range_var(&j.rarg)
        {
            return None;
        }
        // The ON: the one equality of two columns.
        let Some(N::AExpr(on)) = node(&j.quals) else {
            return None;
        };
        let column = |n: &Option<Box<pb::Node>>| matches!(node(n), Some(N::ColumnRef(_)));
        if on.kind != pb::AExprKind::AexprOp as i32
            || names_of(&on.name).last().map(String::as_str) != Some("=")
            || !column(&on.lexpr)
            || !column(&on.rexpr)
        {
            return None;
        }
        if scope.rels.len() != 2
            || scope
                .rels
                .iter()
                .any(|(_, r)| !(r.catalog && r.kind == 'r'))
            || scope.rels[0].1.oid == scope.rels[1].1.oid
        {
            return None;
        }
        let [clause] = clauses else {
            return None;
        };
        if self.aggs > aggs_before
            || !s.group_clause.is_empty()
            || s.having_clause.is_some()
            || !s.distinct_clause.is_empty()
            || s.limit_count.is_some()
            || s.limit_offset.is_some()
            || !s.window_clause.is_empty()
            || !s.locking_clause.is_empty()
        {
            return None;
        }
        // Each WHERE conjunct a restriction the walk noted on one side.
        if let Some(w) = node(&s.where_clause) {
            if !conjuncts(w).iter().all(|c| restriction(c)) {
                return None;
            }
        }
        // An ORDER BY a join key could be a merge join's order.
        let keys = [clause.1, clause.2];
        for sb in &s.sort_clause {
            let Some(N::SortBy(b)) = sb.node.as_ref() else {
                return None;
            };
            if let Some(N::ColumnRef(c)) = node(&b.node) {
                let names = names_of(&c.fields);
                let name = names.last()?;
                let hit = scope.rels.iter().any(|(alias, r)| {
                    (names.len() < 2 || names[names.len() - 2] == *alias)
                        && r.columns
                            .iter()
                            .any(|c| &c.1 == name && keys.contains(&(r.oid, c.0)))
                });
                if hit {
                    return None;
                }
            }
        }
        Some((left, *clause))
    }

    /// The side `rel` as the join's costing reads it: its cheapest scan
    /// through its restrictions, priced as [`PgHandler::scan_estimate`]
    /// prices a catalog's, its key `key`'s statistics.
    fn join_side(&mut self, rel: &Rel, key: i64) -> Option<Side> {
        let needs = self.needs.get(&rel.oid).cloned().unwrap_or_default();
        if !needs.saops.is_empty() || !needs.range_consts.is_empty() {
            return None;
        }
        let def = self.h.catalog_def(&rel.name)?;
        let field = |att: i64| -> Option<String> {
            let col = rel.columns.iter().find(|c| c.0 == att)?;
            def.field_of(&col.1)
        };
        // Every restriction an equality with a constant, or a pattern whose
        // fixed prefix is all of it (`patternsel` estimates it as the
        // equality, and the index condition is that equality); the
        // pattern's own operator stays a filter on the rows the scan
        // fetches (`cost_index`'s `qpqual_cost`).
        let mut filter = bson::Document::new();
        let mut per_tuple = 0.0;
        for (att, opr, konst) in &needs.quals {
            if !konst {
                return None;
            }
            if let Some(op) = pattern_operator(*opr) {
                let (_, _, text) = needs.patterns.iter().find(|p| p.0 == *att && p.1 == *opr)?;
                let Prefix::Exact(p) = fixed_prefix(op, text) else {
                    return None;
                };
                filter.insert(field(*att)?, Bson::String(p));
                let o = self.h.cr_operator(*opr)?;
                let f = builtin().procs.get(&o.code)?;
                let cost = secantus_pgplan::joins::column_call_procost(&f.name, 2)?;
                per_tuple += cost * CPU_OPERATOR_COST;
                continue;
            }
            if self.h.cr_operator(*opr).is_none_or(|o| o.name != "=") {
                return None;
            }
            let (_, v) = needs.eq_consts.iter().find(|(a, _)| a == att)?;
            filter.insert(field(*att)?, v.clone());
        }
        let mut selectivity = 1.0;
        if needs.visible {
            let cost = secantus_pgplan::joins::column_call_procost("pg_table_is_visible", 1)?;
            per_tuple += cost * CPU_OPERATOR_COST;
            selectivity *= DEFAULT_FUNCTION_SELECTIVITY;
        }
        // `set_rel_width`: the columns needed above the scan.
        let mut fields = Vec::new();
        for att in needs.target.iter().chain(&needs.joins) {
            let f = field(*att)?;
            if !fields.contains(&f) {
                fields.push(f);
            }
        }
        let ((startup, mut total, rows, width), index) =
            self.h.scan_estimate(&rel.name, &filter, &fields)?;
        // A filter the estimate left out is paid on each fetched tuple of
        // every path; with the sequential scan cheapest it could tip the
        // choice to an index path, which is not repriced here.
        if per_tuple > 0.0 {
            index.as_ref()?;
            total += per_tuple * rows;
        }
        let rows = clamp_row_est(rows * selectivity);
        let (_, reltuples, _) = crate::catalog_meta::initdb_class_size(rel.oid)?;
        let unique = rel
            .indexes
            .iter()
            .any(|ix| ix.unique && ix.keys.as_slice() == [key]);
        let (ndistinct, default_ndistinct) = self.numdistinct(rel, key, unique, reltuples);
        let index_only = index.as_ref().is_some_and(|ix| ix.only);
        let join = JoinSide {
            rows,
            tuples: reltuples,
            scan: (startup, total),
            startup_scan: (startup, total),
            ordered: None,
            scan_ordered: false,
            unique,
            ndistinct,
            default_ndistinct,
            key_ops: 0.0,
            rel_rows: rows,
            width: Some(width),
            extra: Default::default(),
        };
        Some(Side { join, index_only })
    }

    /// `get_variable_numdistinct` of a catalog column: its `stadistinct`,
    /// a unique column's every row (`isunique`), else the table's rows
    /// when fewer than 200 (`DEFAULT_NUM_DISTINCT`), else 200, the
    /// default.
    fn numdistinct(&self, rel: &Rel, att: i64, unique: bool, tuples: f64) -> (f64, bool) {
        let stat = self.h.column_statistic(rel.oid, att);
        let get = |k: &str| stat.as_ref().and_then(|d| d.get(k)).and_then(num_f64);
        let nullfrac = get("stanullfrac").unwrap_or(0.0);
        let mut stadistinct = get("stadistinct").unwrap_or(0.0);
        if unique {
            stadistinct = -(1.0 - nullfrac);
        }
        if stadistinct > 0.0 {
            return (clamp_row_est(stadistinct), false);
        }
        if stadistinct < 0.0 {
            return (clamp_row_est(-stadistinct * tuples), false);
        }
        if tuples < 200.0 {
            return (clamp_row_est(tuples), false);
        }
        (200.0, true)
    }

    /// A plain nested loop's inner sequential scan of `pg_namespace`,
    /// unique on its oid, below `pg_class`: one scan an outer row, each
    /// stopping at the row its `relnamespace` names, in the heap's order.
    fn single_match_scans(&mut self, outer: &Rel, inner: &Rel) {
        if outer.oid != RELATION_RELATION || inner.oid != NAMESPACE_RELATION {
            return;
        }
        let Some(rows) = self.scan_rows.get(&outer.oid).cloned() else {
            return;
        };
        // The inner has no restriction of its own: every row it reads is
        // the join filter's to pass.
        let needs = self.needs.get(&inner.oid).cloned().unwrap_or_default();
        if !needs.quals.is_empty() || needs.visible {
            return;
        }
        let Some(order) = self.h.cr_namespace_heap_order() else {
            return;
        };
        let Some(at) = self
            .exec
            .iter()
            .rposition(|s| matches!(s, Scan::Seq(r, _) if *r == inner.oid))
        else {
            return;
        };
        let all = order.len() as i64;
        let scans: Vec<Scan> = rows
            .iter()
            .map(|(_, _, nsp)| {
                let n = order
                    .iter()
                    .position(|o| o == nsp)
                    .map_or(all, |i| i as i64 + 1);
                Scan::Seq(inner.oid, n)
            })
            .collect();
        self.exec.splice(at..=at, scans);
    }
}

/// `add_paths_to_joinrel` and `set_cheapest` over the two sides (`a` the
/// FROM's first, the preserved side of a LEFT join): the plan kept.
fn choose(a: &JoinSide, b: &JoinSide, left: bool) -> Option<Plan> {
    let mut rel = JoinRel::new(false);
    rel.add_paths(0, a, b, left, true);
    let choice = rel.cheapest(None, joincost::join_rows(a, b, left))?;
    let at = (choice.startup, choice.total);
    // A nested loop over the inner's scan, or over a Material of it.
    let nest = |outer: &JoinSide, inner: &JoinSide| joincost::nest_loop(outer, inner, false) == at;
    use Read::{Small, Whole};
    Some(match choice.method {
        JoinMethod::Other if choice.hash => Plan {
            outer: (0, Whole),
            inner: (1, Small),
            plain_nest_loop: false,
        },
        JoinMethod::Other => {
            let plain = nest(a, b);
            Plan {
                outer: (0, Whole),
                inner: (1, if plain { Whole } else { Small }),
                plain_nest_loop: plain,
            }
        }
        JoinMethod::NestOuterB => {
            let plain = nest(b, a);
            Plan {
                outer: (1, Whole),
                inner: (0, if plain { Whole } else { Small }),
                plain_nest_loop: plain,
            }
        }
        JoinMethod::HashOuterA => Plan {
            outer: (0, Whole),
            inner: (1, Small),
            plain_nest_loop: false,
        },
        JoinMethod::HashOuterB | JoinMethod::HashRightB => Plan {
            outer: (1, Whole),
            inner: (0, Small),
            plain_nest_loop: false,
        },
        // Each side read in order through a Sort (no presorted index path
        // is priced).
        JoinMethod::MergeOuterA { .. } => Plan {
            outer: (0, Small),
            inner: (1, Small),
            plain_nest_loop: false,
        },
        JoinMethod::MergeOuterB { .. } | JoinMethod::MergeRightB { .. } => Plan {
            outer: (1, Small),
            inner: (0, Small),
            plain_nest_loop: false,
        },
        _ => return None,
    })
}

/// A WHERE's AND-ed conjuncts.
fn conjuncts(n: &N) -> Vec<&N> {
    match n {
        N::BoolExpr(b) if b.boolop == pb::BoolExprType::AndExpr as i32 => b
            .args
            .iter()
            .filter_map(|a| a.node.as_ref())
            .flat_map(conjuncts)
            .collect(),
        _ => vec![n],
    }
}

/// A conjunct the walk notes as one relation's restriction: a column
/// compared with a constant, or `pg_table_is_visible` of a column.
fn restriction(n: &N) -> bool {
    let column = |e: &Option<Box<pb::Node>>| matches!(node(uncollate(e)), Some(N::ColumnRef(_)));
    let constant = |e: &Option<Box<pb::Node>>| node(uncollate(e)).is_some_and(is_constant);
    match n {
        N::AExpr(a) => {
            a.kind == pb::AExprKind::AexprOp as i32
                && ((column(&a.lexpr) && constant(&a.rexpr))
                    || (constant(&a.lexpr) && column(&a.rexpr)))
        }
        N::FuncCall(f) => {
            names_of(&f.funcname).last().map(String::as_str) == Some("pg_table_is_visible")
                && matches!(
                    f.args.as_slice(),
                    [a] if matches!(a.node.as_ref(), Some(N::ColumnRef(_)))
                )
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A side with PostgreSQL 15.20's `EXPLAIN` estimate of its scan.
    fn side(scan: (f64, f64), rows: f64, width: i64, unique: bool, ndistinct: f64) -> JoinSide {
        JoinSide {
            rows,
            tuples: if unique { 4.0 } else { 415.0 },
            scan,
            startup_scan: scan,
            ordered: None,
            scan_ordered: false,
            unique,
            ndistinct,
            default_ndistinct: false,
            key_ops: 0.0,
            rel_rows: rows,
            width: Some(width),
            extra: Default::default(),
        }
    }

    #[test]
    fn psqls_describe_query_is_a_nested_loop_over_pg_namespace() {
        // `pg_class` by its relname index through the pattern's equality
        // and the two filters (0.27..8.32 rows=1 width=72), `pg_namespace`
        // scanned (0.00..1.04 rows=4 width=68): `Nested Loop Left Join
        // (cost=0.27..9.41)`, its inner the plain scan.
        let class = side((0.27, 8.32), 1.0, 72, false, 4.0);
        let namespace = side((0.0, 1.04), 4.0, 68, true, 4.0);
        let plan = choose(&class, &namespace, true).expect("a plan");
        assert_eq!(plan.outer, (0, Read::Whole));
        assert_eq!(plan.inner, (1, Read::Whole));
        assert!(plan.plain_nest_loop);
    }

    #[test]
    fn a_sequential_scan_of_pg_class_is_a_hash_joins_outer() {
        // `relkind = 'v'`: `pg_class` scanned (0.00..19.12 rows=138
        // width=72): `Hash Left Join (cost=1.09..20.96)` over a Hash of
        // `pg_namespace`.
        let class = side((0.0, 19.12), 138.0, 72, false, 4.0);
        let namespace = side((0.0, 1.04), 4.0, 68, true, 4.0);
        let plan = choose(&class, &namespace, true).expect("a plan");
        assert_eq!(plan.outer, (0, Read::Whole));
        assert_eq!(plan.inner, (1, Read::Small));
        assert!(!plan.plain_nest_loop);
    }

    #[test]
    fn the_second_relation_outer_of_an_inner_join() {
        // `pg_namespace` first in the FROM, `pg_class` an index-only scan
        // (0.27..4.29 rows=1): `Nested Loop (cost=0.27..5.38)` with
        // `pg_class` outer.
        let namespace = side((0.0, 1.04), 4.0, 68, true, 4.0);
        let class = side((0.27, 4.29), 1.0, 68, false, 4.0);
        let plan = choose(&namespace, &class, false).expect("a plan");
        assert_eq!(plan.outer, (1, Read::Whole));
        assert_eq!(plan.inner, (0, Read::Whole));
        assert!(plan.plain_nest_loop);
    }
}
