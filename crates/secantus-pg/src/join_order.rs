//! The join order PostgreSQL picks for four tables (RP-028).
//!
//! `join_search` plans three tables; four are planned here by the same
//! dynamic programming PostgreSQL runs (`standard_join_search`), for the
//! subset whose search has one shape: a chain of four stored tables, each
//! joined INNER or LEFT to one table before it on one column equality, no
//! column in two clauses. A LEFT join constrains the orders as PostgreSQL's
//! `join_is_legal` does, over its `SpecialJoinInfo` (`Search::outer`). The join graph is then a tree, every join relation the
//! search builds holds connected tables, any two of them that can join do
//! so on exactly one clause, and no relation's sorted output serves a join
//! above it -- so each relation's cheapest path is the only one the next
//! level reads.
//!
//! `join_search_one_level` builds each level in PostgreSQL's order: every
//! relation of the level below joined to each base table a clause
//! connects (`make_rels_by_clause_joins`; at level 2 only the tables after
//! it), then the bushy pairs (`make_rels_by_clauseless_joins` aside, which
//! a tree never needs). A relation is sized by the first pair that builds
//! it (`build_join_rel`), and every pair's paths go into its one
//! `add_path` list (`joincost::JoinRel`), both sides outer in turn
//! (`populate_joinrel_with_paths`). The rows are then built in the plan the
//! four-table relation picks, each join by `join_search`'s `run_step`.

use super::*;
use crate::join_search::{alias_of, column_of, filter_fixes, joinrel_side, Input, Step};
use secantus_pgplan::joincost::{JoinChoice, JoinRel, JoinSide};
use secantus_pgplan::joins::{JoinKind, JoinNode};

/// One clause of the chain: the base tables it joins (by index), their
/// keys, and the ON (by its join's index) it comes from.
struct Edge {
    u: usize,
    u_key: String,
    v: usize,
    v_key: String,
    on: usize,
}

/// One way a join relation was built: its two input relations, `a` first,
/// and the clause between them.
struct Built {
    a: usize,
    b: usize,
    edge: usize,
    /// A LEFT join, `a` its preserved side.
    left: bool,
}

/// A relation of the search: its tables (a bit each), its rows, its paths
/// and the ways it was built, and, once its level is done, its cheapest
/// path.
struct Rel {
    set: u8,
    rows: f64,
    paths: JoinRel,
    built: Vec<Built>,
    best: Option<JoinChoice>,
}

/// The search's tables, clauses and relations.
struct Search {
    tables: Vec<(String, Document)>,
    edges: Vec<Edge>,
    rels: Vec<Rel>,
    /// The LEFT joins' `SpecialJoinInfo`s: `(min_lefthand, min_righthand)`.
    /// A chain's ON is a strict equality, so a LEFT join above commutes
    /// with one below whose nullable side it reads (`make_outerjoininfo`
    /// adds nothing to its min_lefthand), and each is its two tables.
    outer: Vec<(u8, u8)>,
}

/// A chain of four tables: its leaves, in FROM order, and its joins' ONs.
struct Chain<'a> {
    leaves: Vec<&'a JoinNode>,
    ons: Vec<Option<&'a secantus_pgplan::ColumnExpr>>,
}

impl Search {
    /// `join_is_legal` for joining relations `a` and `b`: `None` when the
    /// join would break an outer join; else whether it performs one, and
    /// if so whether `b` holds its preserved side (`reversed`).
    fn join_is_legal(&self, a: u8, b: u8) -> Option<Option<bool>> {
        let joined = a | b;
        let mut matched: Option<bool> = None;
        let mut must_be_left = false;
        for &(lhs, rhs) in &self.outer {
            let sub = |x: u8, y: u8| x & !y == 0;
            if rhs & joined == 0 || sub(joined, rhs) {
                continue;
            }
            if (sub(lhs, a) && sub(rhs, a)) || (sub(lhs, b) && sub(rhs, b)) {
                continue;
            }
            if sub(lhs, a) && sub(rhs, b) {
                if matched.is_some() {
                    return None;
                }
                matched = Some(false);
            } else if sub(lhs, b) && sub(rhs, a) {
                if matched.is_some() {
                    return None;
                }
                matched = Some(true);
            } else {
                if a & rhs != 0 && b & rhs != 0 {
                    continue;
                }
                if joined & lhs != 0 {
                    return None;
                }
                must_be_left = true;
            }
        }
        if must_be_left && matched.is_none() {
            return None;
        }
        Some(matched)
    }

    fn edge_between(&self, a: u8, b: u8) -> Option<usize> {
        let mut found = None;
        for (i, e) in self.edges.iter().enumerate() {
            let (u, v) = (1u8 << e.u, 1u8 << e.v);
            if (a & u != 0 && b & v != 0) || (a & v != 0 && b & u != 0) {
                if found.is_some() {
                    return None;
                }
                found = Some(i);
            }
        }
        found
    }
}

impl PgHandler {
    /// `node` as a chain of four stored tables joined INNER or LEFT, each on
    /// one column equality to a table before it, no column in two clauses.
    fn four_table_chain<'a>(&self, node: &'a JoinNode) -> Option<(Chain<'a>, Search)> {
        let mut joins = Vec::new();
        let mut cur = node;
        while let JoinNode::Join { left, .. } = cur {
            joins.push(cur);
            cur = left;
        }
        joins.reverse();
        if joins.len() != 3 {
            return None;
        }
        let mut leaves = vec![cur];
        let mut ons = Vec::new();
        let mut tables = Vec::new();
        let mut edges = Vec::new();
        let mut kinds = Vec::new();
        let leaf_alias = |n: &JoinNode| match n {
            JoinNode::Leaf { columns, .. } => columns.first().map(|(k, _)| alias_of(k).to_string()),
            _ => None,
        };
        for (i, j) in joins.iter().enumerate() {
            let JoinNode::Join {
                kind,
                right,
                on,
                equi,
                merged,
                expr_keys,
                ec,
                on_aliases,
                ..
            } = j
            else {
                return None;
            };
            if !matches!(kind, JoinKind::Inner | JoinKind::Left)
                || !merged.is_empty()
                || !expr_keys.is_empty()
                || ec.is_some()
            {
                return None;
            }
            let [(lk, rk)] = equi.as_slice() else {
                return None;
            };
            leaves.push(right.as_ref());
            ons.push(on.as_ref());
            let aliases: Vec<String> = leaves
                .iter()
                .map(|n| leaf_alias(n))
                .collect::<Option<_>>()?;
            let new = i + 1;
            let u = aliases[..new].iter().position(|a| a == alias_of(lk))?;
            if alias_of(rk) != aliases[new]
                || on_aliases
                    .iter()
                    .any(|a| *a != aliases[u] && *a != aliases[new])
            {
                return None;
            }
            edges.push(Edge {
                u,
                u_key: lk.clone(),
                v: new,
                v_key: rk.clone(),
                on: i,
            });
            kinds.push(*kind);
        }
        // `reduce_outer_joins`, top down: an inner join's strict clause
        // makes the tables it reads non-null below it, and a LEFT join
        // whose nullable table is so is an inner join.
        let mut nonnull = 0u8;
        for i in (0..edges.len()).rev() {
            let e = &edges[i];
            if kinds[i] == JoinKind::Left && nonnull & (1 << e.v) != 0 {
                kinds[i] = JoinKind::Inner;
            }
            if kinds[i] == JoinKind::Inner {
                nonnull |= (1 << e.u) | (1 << e.v);
            }
        }
        let outer = edges
            .iter()
            .zip(&kinds)
            .filter(|(_, k)| **k == JoinKind::Left)
            .map(|(e, _)| (1u8 << e.u, 1u8 << e.v))
            .collect();
        // No column in two clauses: an equivalence class of three or more,
        // whose derived clauses this search does not weigh.
        let mut keys: Vec<&String> = edges.iter().flat_map(|e| [&e.u_key, &e.v_key]).collect();
        keys.sort();
        if keys.windows(2).any(|w| w[0] == w[1]) {
            return None;
        }
        for leaf in &leaves {
            let (table, filter) = plain_leaf_table(leaf)?;
            if !self.join_small_enough(&table) {
                return None;
            }
            let def = self.lookup(&table)?;
            let alias = leaf_alias(leaf)?;
            for e in &edges {
                for key in [&e.u_key, &e.v_key] {
                    if alias_of(key) == alias {
                        let field = def.field_of(column_of(key)?)?;
                        if filter_fixes(&filter, &field) {
                            return None;
                        }
                    }
                }
            }
            tables.push((table, filter));
        }
        Some((
            Chain { leaves, ons },
            Search {
                tables,
                edges,
                rels: Vec::new(),
                outer,
            },
        ))
    }

    /// The side of a join a relation is, joined on `key` of base table
    /// `key_table`.
    fn search_side(
        &self,
        search: &Search,
        rel: usize,
        key: &str,
        key_table: usize,
    ) -> Option<JoinSide> {
        let (table, filter) = &search.tables[key_table];
        let base = self.join_side(table, JoinKey::Column(column_of(key)?), filter, 0)?;
        let r = &search.rels[rel];
        if r.set.count_ones() == 1 {
            return Some(base);
        }
        Some(joinrel_side(r.rows, r.best.as_ref()?, None, &base))
    }

    /// `make_join_rel`: the relation of `a` and `b`'s tables, built (or
    /// found) and given their join's paths.
    /// A join the outer joins do not allow is not built.
    fn make_join_rel(&self, search: &mut Search, a: usize, b: usize) -> Option<()> {
        use secantus_pgplan::joincost::join_rows;
        let Some(outer) = search.join_is_legal(search.rels[a].set, search.rels[b].set) else {
            return Some(());
        };
        // A LEFT join's preserved side first (`make_join_rel` swaps a
        // reversed pair).
        let (a, b) = if outer == Some(true) { (b, a) } else { (a, b) };
        let left = outer.is_some();
        let (sa, sb) = (search.rels[a].set, search.rels[b].set);
        let edge = search.edge_between(sa, sb)?;
        let e = &search.edges[edge];
        let ((a_key, a_table), (b_key, b_table)) = if sa & (1 << e.u) != 0 {
            ((e.u_key.clone(), e.u), (e.v_key.clone(), e.v))
        } else {
            ((e.v_key.clone(), e.v), (e.u_key.clone(), e.u))
        };
        let side_a = self.search_side(search, a, &a_key, a_table)?;
        let side_b = self.search_side(search, b, &b_key, b_table)?;
        let set = sa | sb;
        let idx = match search.rels.iter().position(|r| r.set == set) {
            Some(i) => i,
            None => {
                search.rels.push(Rel {
                    set,
                    rows: join_rows(&side_a, &side_b, left),
                    paths: JoinRel::new(false),
                    built: Vec::new(),
                    best: None,
                });
                search.rels.len() - 1
            }
        };
        let rel = &mut search.rels[idx];
        let combo = rel.built.len();
        rel.built.push(Built { a, b, edge, left });
        rel.paths.add_paths(combo, &side_a, &side_b, left, true);
        Some(())
    }

    /// `standard_join_search` over the chain's four tables; the index of the
    /// four-table relation.
    fn search_four(&self, search: &mut Search) -> Option<usize> {
        let n = search.tables.len();
        for i in 0..n {
            search.rels.push(Rel {
                set: 1 << i,
                rows: 0.0,
                paths: JoinRel::new(false),
                built: Vec::new(),
                best: None,
            });
        }
        let mut levels: Vec<Vec<usize>> = vec![Vec::new(), (0..n).collect()];
        let connected = |s: &Search, a: usize, b: usize| {
            let (sa, sb) = (s.rels[a].set, s.rels[b].set);
            sa & sb == 0 && s.edge_between(sa, sb).is_some()
        };
        for level in 2..=n {
            let before = search.rels.len();
            // Each relation a level down with each base table it joins.
            for (pos, &old) in levels[level - 1].clone().iter().enumerate() {
                let others: Vec<usize> = if level == 2 {
                    levels[1][pos + 1..].to_vec()
                } else {
                    levels[1].clone()
                };
                for other in others {
                    if connected(search, old, other) {
                        self.make_join_rel(search, old, other)?;
                    }
                }
            }
            // The bushy pairs: k tables with level - k.
            for k in 2..=level / 2 {
                for (pos, &old) in levels[k].clone().iter().enumerate() {
                    let others: Vec<usize> = if k == level - k {
                        levels[k][pos + 1..].to_vec()
                    } else {
                        levels[level - k].clone()
                    };
                    for other in others {
                        if connected(search, old, other) {
                            self.make_join_rel(search, old, other)?;
                        }
                    }
                }
            }
            let built: Vec<usize> = (before..search.rels.len()).collect();
            for &i in &built {
                let rel = &mut search.rels[i];
                rel.best = Some(rel.paths.cheapest(None, rel.rows)?);
            }
            levels.push(built);
        }
        levels[n].first().copied()
    }

    /// The rows of relation `rel` in the plan its cheapest path is.
    fn run_searched(
        &self,
        search: &Search,
        chain: &Chain<'_>,
        rel: usize,
    ) -> PgWireResult<Option<Input>> {
        let r = &search.rels[rel];
        if r.set.count_ones() == 1 {
            let leaf = chain.leaves[r.set.trailing_zeros() as usize];
            return Ok(Some(Input {
                rows: self.join_rows(leaf)?,
                keys: secantus_pgplan::joins::node_keys(leaf),
            }));
        }
        let Some(best) = r.best.as_ref() else {
            return Ok(None);
        };
        let built = &r.built[best.combo];
        let e = &search.edges[built.edge];
        let sa = search.rels[built.a].set;
        let (a_key, a_table, b_key) = if sa & (1 << e.u) != 0 {
            (e.u_key.clone(), e.u, e.v_key.clone())
        } else {
            (e.v_key.clone(), e.v, e.u_key.clone())
        };
        let (Some(a), Some(b)) = (
            self.run_searched(search, chain, built.a)?,
            self.run_searched(search, chain, built.b)?,
        ) else {
            return Ok(None);
        };
        let a_rows = if sa.count_ones() == 1 {
            let (table, filter) = &search.tables[a_table];
            column_of(&a_key)
                .and_then(|c| self.join_side(table, JoinKey::Column(c), filter, 0))
                .map_or(0.0, |s| s.rows)
        } else {
            search.rels[built.a].rows
        };
        let step = Step {
            method: best.method,
            left: built.left,
            a_key,
            b_key,
            a_rows,
            on: None,
            filters: Vec::new(),
        };
        let a_type = chain
            .leaves
            .iter()
            .find_map(|n| leaf_key_type(n, &step.a_key));
        self.run_step(&step, a, b, a_type, chain.ons[e.on])
    }

    /// Whether `node` is a chain of four tables whose join order is searched.
    pub(crate) fn four_table_plan_applies(&self, node: &JoinNode) -> bool {
        if crate::join_search::where_spans() || LOCK_ROW_IDS.with(|c| c.get()) {
            return false;
        }
        let Some((_, mut search)) = self.four_table_chain(node) else {
            return false;
        };
        self.search_four(&mut search).is_some()
    }

    /// `node`'s rows in the plan PostgreSQL's join order search picks for
    /// four tables, or `None` for any other join.
    pub(crate) fn four_table_join(
        &self,
        node: &JoinNode,
        limit: Option<usize>,
    ) -> PgWireResult<Option<Vec<Document>>> {
        if limit.is_some() || crate::join_search::where_spans() || LOCK_ROW_IDS.with(|c| c.get()) {
            return Ok(None);
        }
        let Some((chain, mut search)) = self.four_table_chain(node) else {
            return Ok(None);
        };
        let Some(top) = self.search_four(&mut search) else {
            return Ok(None);
        };
        Ok(self.run_searched(&search, &chain, top)?.map(|i| i.rows))
    }
}
