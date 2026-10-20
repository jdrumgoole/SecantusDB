//! The join order PostgreSQL picks for four to eight tables (RP-028).
//!
//! `join_search` plans three tables; four to eight are planned here by the
//! same dynamic programming PostgreSQL runs (`standard_join_search`), for
//! the subset whose search has one shape: a chain of stored tables, each
//! joined INNER or LEFT to one table before it on one column equality. A
//! LEFT join constrains the orders as PostgreSQL's `join_is_legal` does,
//! over its `SpecialJoinInfo` (`Search::outer`). With no column in two
//! clauses the join graph is a tree, every join relation the search builds
//! holds connected tables, any two of them that can join do so on exactly
//! one clause, and no relation's sorted output serves a join above it -- so
//! each relation's cheapest path is the only one the next level reads.
//!
//! Eight is `join_collapse_limit`'s default: a longer chain of explicit
//! JOINs is not flattened into one search, and is declined here.
//!
//! An INNER chain whose clauses share a column is one equivalence class
//! over three or more tables (`Search::class`): its members in the order
//! `process_equivalence` adds them (each ON's left column, then its right,
//! as written), any two relations holding members joined on its derived
//! clause, the first member each side
//! (`generate_join_implied_equalities_normal`). A merge join on the class
//! leaves its rows in the class's order, which a merge join above on it
//! reads unsorted (`Rel::keyed`), and paths of one relation in two classes'
//! orders are both kept (`JoinRel::add_paths_on`). Integer keys only, as in
//! `join_search`: a derived clause is matched by its keys' values, and the
//! ONs it stands in for are checked on every row at the end.
//!
//! `join_search_one_level` builds each level in PostgreSQL's order: every
//! relation of the level below joined to each base table a clause
//! connects (`make_rels_by_clause_joins`; at level 2 only the tables after
//! it), then the bushy pairs (`make_rels_by_clauseless_joins` aside, which
//! a tree never needs). A relation is sized by the first pair that builds
//! it (`build_join_rel`), and every pair's paths go into its one
//! `add_path` list (`joincost::JoinRel`), both sides outer in turn
//! (`populate_joinrel_with_paths`). The rows are then built in the plan the
//! relation of every table picks, each join by `join_search`'s `run_step`.

use super::*;
use crate::join_search::{alias_of, column_of, filter_fixes, joinrel_side, Input, Step};
use secantus_pgplan::joincost::{JoinChoice, JoinMethod, JoinRel, JoinSide};
use secantus_pgplan::joins::{JoinKind, JoinNode};

/// A column's width with no statistics (`get_typavgwidth`): its type's
/// length, else what its declared maximum or 32 makes it.
fn column_width(c: &secantus_pgcatalog::Column) -> i64 {
    let typlen = secantus_pgplan::pgtypes::oid_of_name(&c.pg_type)
        .and_then(|oid| crate::pg_type_facts::builtin(oid, "typlen"))
        .and_then(|v| match v {
            Bson::Int32(n) => Some(i64::from(n)),
            Bson::Int64(n) => Some(n),
            _ => None,
        })
        .unwrap_or(-1);
    secantus_pgplan::hashagg::type_avg_width(&c.pg_type, c.typmod, typlen)
}

/// One clause of the chain: the base tables it joins (by index), their
/// keys, and the ON (by its join's index) it comes from.
struct Edge {
    u: usize,
    u_key: String,
    v: usize,
    v_key: String,
    on: usize,
}

/// The one clause joining two relations: `a_key` of base table `a_table`
/// in the first, `b_key` of `b_table` in the second, and the ON it is
/// (`None` for the class's derived clause).
#[derive(Clone)]
struct Clause {
    a_key: String,
    a_table: usize,
    b_key: String,
    b_table: usize,
    on: Option<usize>,
    /// The class's derived clause rather than an edge.
    class: bool,
    /// The equivalence class it is in, for `JoinRel::add_paths_on`: 0 for
    /// every clause when the chain has no class (as before there was
    /// one), else 1 for the class and 2 + its index for an edge.
    key_class: usize,
}

/// One way a join relation was built: its two input relations, `a` first,
/// and the clause between them.
struct Built {
    a: usize,
    b: usize,
    clause: Clause,
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
    /// Its cheapest path in the class's order (a merge join on it).
    keyed: Option<JoinChoice>,
}

/// The search's tables, clauses and relations.
struct Search {
    tables: Vec<(String, Document)>,
    edges: Vec<Edge>,
    rels: Vec<Rel>,
    /// The LEFT joins' `SpecialJoinInfo`s: `(min_lefthand, min_righthand)`.
    /// A chain's ON is a strict equality, so a LEFT join above commutes
    /// with one below whose nullable side it reads (`make_outerjoininfo`
    /// adds nothing to its min_lefthand): its left is its clause's table,
    /// and its right is its nullable side -- one table, or the tables an
    /// inner join under it joins, which `min_righthand` takes whole.
    outer: Vec<(u8, u8)>,
    /// The equivalence class the clauses sharing a column make: its members
    /// `(table, key)` in `ec_members` order. Its clauses are not `edges`.
    class: Vec<(usize, String)>,
    /// The ONs the class stands in for, checked on every row at the end.
    class_ons: Vec<usize>,
    /// Each table's columns, by joined-row key, with their widths
    /// (`get_typavgwidth`), and the keys the query reads above the join;
    /// `None` when that is not known, and then no width is.
    widths: Vec<Vec<(String, i64)>>,
    needed: Option<Vec<String>>,
}

/// A chain of four to eight tables: its leaves, in FROM order, and its
/// joins' ONs.
struct Chain<'a> {
    leaves: Vec<&'a JoinNode>,
    ons: Vec<Option<&'a secantus_pgplan::ColumnExpr>>,
}

/// One join of the tree as `four_table_chain` walks it: the tables (a bit
/// each) on its two sides, whether its right side is joined by inner joins
/// only, and the joins (by index) that are its two sides.
struct Walked<'a> {
    node: &'a JoinNode,
    left: u8,
    right: u8,
    right_inner: bool,
    left_join: Option<usize>,
    right_join: Option<usize>,
}

/// Walk `n`, pushing its tables left to right and its joins bottom-up;
/// returns its tables, whether every join in it is inner, and its own
/// join's index when it is one. `None` for anything but stored tables
/// joined two at a time, or more than eight tables.
fn walk_tree<'a>(
    n: &'a JoinNode,
    leaves: &mut Vec<&'a JoinNode>,
    joins: &mut Vec<Walked<'a>>,
) -> Option<(u8, bool, Option<usize>)> {
    match n {
        JoinNode::Leaf { .. } => {
            if leaves.len() >= 8 {
                return None;
            }
            leaves.push(n);
            Some((1 << (leaves.len() - 1), true, None))
        }
        JoinNode::Join {
            kind, left, right, ..
        } => {
            let (l, l_inner, l_join) = walk_tree(left, leaves, joins)?;
            let (r, r_inner, r_join) = walk_tree(right, leaves, joins)?;
            joins.push(Walked {
                node: n,
                left: l,
                right: r,
                right_inner: r_inner,
                left_join: l_join,
                right_join: r_join,
            });
            let inner = l_inner && r_inner && *kind == JoinKind::Inner;
            Some((l | r, inner, Some(joins.len() - 1)))
        }
        _ => None,
    }
}

impl Search {
    /// The width of relation `set`'s rows (`build_joinrel_tlist`): each of
    /// its tables' columns still needed above it -- read by the query, a
    /// key of a clause to a table outside it, or a member of the class
    /// while a member is outside it (`generate_base_implied_equalities`
    /// marks every member needed by every table of the class).
    fn width(&self, set: u8) -> Option<i64> {
        let needed = self.needed.as_ref()?;
        let outside = |t: usize| set & (1 << t) == 0;
        let class_open = self.class.iter().any(|(t, _)| outside(*t));
        let mut width = 0;
        for (t, cols) in self.widths.iter().enumerate() {
            if outside(t) {
                continue;
            }
            for (key, w) in cols {
                let used = needed.contains(key)
                    || self.edges.iter().any(|e| {
                        (e.u == t && e.u_key == *key && outside(e.v))
                            || (e.v == t && e.v_key == *key && outside(e.u))
                    })
                    || (class_open && self.class.iter().any(|(ct, k)| *ct == t && k == key));
                if used {
                    width += w;
                }
            }
        }
        Some(width)
    }

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

    /// The one clause between relations `a` and `b`; `None` when there is
    /// none, or more than one (which the search does not model).
    fn clause_between(&self, a: u8, b: u8) -> Option<Clause> {
        let classed = !self.class.is_empty();
        let mut found = None;
        for (i, e) in self.edges.iter().enumerate() {
            let (u, v) = (1u8 << e.u, 1u8 << e.v);
            let clause = if a & u != 0 && b & v != 0 {
                (e.u_key.clone(), e.u, e.v_key.clone(), e.v)
            } else if a & v != 0 && b & u != 0 {
                (e.v_key.clone(), e.v, e.u_key.clone(), e.u)
            } else {
                continue;
            };
            if found.is_some() {
                return None;
            }
            let (a_key, a_table, b_key, b_table) = clause;
            found = Some(Clause {
                a_key,
                a_table,
                b_key,
                b_table,
                on: Some(e.on),
                class: false,
                key_class: if classed { 2 + i } else { 0 },
            });
        }
        // The class: its first member each side.
        let first = |set: u8| self.class.iter().find(|(t, _)| set & (1 << t) != 0);
        if let (Some((at, ak)), Some((bt, bk))) = (first(a), first(b)) {
            if found.is_some() {
                return None;
            }
            found = Some(Clause {
                a_key: ak.clone(),
                a_table: *at,
                b_key: bk.clone(),
                b_table: *bt,
                on: None,
                class: true,
                key_class: 1,
            });
        }
        found
    }
}

impl PgHandler {
    /// `node` as a chain of four to eight stored tables joined INNER or
    /// LEFT, each on one column equality to a table before it; the
    /// clauses sharing a column make at most one equivalence class.
    fn four_table_chain<'a>(&self, node: &'a JoinNode) -> Option<(Chain<'a>, Search)> {
        // The tree, as `deconstruct_jointree` reads it: its tables left to
        // right, and its joins bottom-up, each left side before its right.
        let mut leaves: Vec<&'a JoinNode> = Vec::new();
        let mut joins: Vec<Walked<'a>> = Vec::new();
        walk_tree(node, &mut leaves, &mut joins)?;
        // Four tables to `join_collapse_limit`'s eight; three only when a
        // join's right side is a join (a left-deep three is
        // `three_table_plan`'s).
        let bushy = joins.iter().any(|j| j.right.count_ones() > 1);
        if !(4..=8).contains(&leaves.len()) && !(leaves.len() == 3 && bushy) {
            return None;
        }
        let mut ons = Vec::new();
        let mut tables = Vec::new();
        let mut edges = Vec::new();
        let mut reversed = Vec::new();
        let mut kinds = Vec::new();
        let leaf_alias = |n: &JoinNode| match n {
            JoinNode::Leaf { columns, .. } => columns.first().map(|(k, _)| alias_of(k).to_string()),
            _ => None,
        };
        let aliases: Vec<String> = leaves
            .iter()
            .map(|n| leaf_alias(n))
            .collect::<Option<_>>()?;
        for (i, w) in joins.iter().enumerate() {
            let JoinNode::Join {
                kind,
                on,
                equi,
                equi_reversed,
                merged,
                expr_keys,
                ec,
                on_aliases,
                ..
            } = w.node
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
            // A LEFT join's nullable side is a table, or a join of tables
            // by inner joins only (its `min_righthand` is then all of it).
            if *kind == JoinKind::Left && w.right.count_ones() > 1 && !w.right_inner {
                return None;
            }
            let [(lk, rk)] = equi.as_slice() else {
                return None;
            };
            ons.push(on.as_ref());
            let side = |key: &str, set: u8| {
                (0..leaves.len()).find(|&t| set & (1 << t) != 0 && aliases[t] == alias_of(key))
            };
            let u = side(lk, w.left)?;
            let v = side(rk, w.right)?;
            if on_aliases
                .iter()
                .any(|a| *a != aliases[u] && *a != aliases[v])
            {
                return None;
            }
            edges.push(Edge {
                u,
                u_key: lk.clone(),
                v,
                v_key: rk.clone(),
                on: i,
            });
            reversed.push(equi_reversed.first().copied().unwrap_or(false));
            kinds.push(*kind);
        }
        // `reduce_outer_joins`, top down: an inner join's strict clause
        // makes the tables it reads non-null on both sides below it, a LEFT
        // join passes what is non-null above it to its left side only, and
        // a LEFT join whose nullable side holds such a table is inner.
        fn reduce(
            i: usize,
            upper: u8,
            joins: &[Walked<'_>],
            edges: &[Edge],
            kinds: &mut [JoinKind],
        ) {
            let w = &joins[i];
            if kinds[i] == JoinKind::Left && upper & w.right != 0 {
                kinds[i] = JoinKind::Inner;
            }
            let local = (1u8 << edges[i].u) | (1u8 << edges[i].v);
            let (to_left, to_right) = if kinds[i] == JoinKind::Inner {
                (upper | local, upper | local)
            } else {
                (upper, 0)
            };
            if let Some(l) = w.left_join {
                reduce(l, to_left, joins, edges, kinds);
            }
            if let Some(r) = w.right_join {
                reduce(r, to_right, joins, edges, kinds);
            }
        }
        reduce(joins.len() - 1, 0, &joins, &edges, &mut kinds);
        // Each LEFT join's `SpecialJoinInfo`: its clause's table on the
        // left, and its whole nullable side (`make_outerjoininfo` adds the
        // inner-joined tables under it to `min_righthand`).
        let outer = edges
            .iter()
            .zip(&kinds)
            .zip(&joins)
            .filter(|((_, k), _)| **k == JoinKind::Left)
            .map(|((e, _), w)| (1u8 << e.u, w.right))
            .collect();
        // A column in two clauses: the equivalence class they make.
        let (class, class_ons) = Self::chain_class(&mut edges, &reversed)?;
        if !class.is_empty() && kinds.iter().any(|k| *k != JoinKind::Inner) {
            return None;
        }
        for leaf in &leaves {
            let (table, filter) = plain_leaf_table(leaf)?;
            if !self.join_small_enough(&table) {
                return None;
            }
            let def = self.lookup(&table)?;
            let alias = leaf_alias(leaf)?;
            let keys = edges
                .iter()
                .flat_map(|e| [&e.u_key, &e.v_key])
                .chain(class.iter().map(|(_, k)| k));
            for key in keys {
                if alias_of(key) == alias {
                    let field = def.field_of(column_of(key)?)?;
                    if filter_fixes(&filter, &field) {
                        return None;
                    }
                }
            }
            tables.push((table, filter));
        }
        // The class's derived clauses are matched by value: integers of
        // one type only.
        if !class.is_empty() {
            let types: Vec<Option<String>> = class
                .iter()
                .map(|(t, k)| leaf_key_type(leaves[*t], k))
                .collect();
            let int = |t: &Option<String>| matches!(t.as_deref(), Some("int2" | "int4" | "int8"));
            if !types.iter().all(int) || types.windows(2).any(|t| t[0] != t[1]) {
                return None;
            }
        }
        let widths = leaves
            .iter()
            .map(|leaf| match leaf {
                JoinNode::Leaf { def, columns, .. } => columns
                    .iter()
                    .map(|(key, field)| {
                        let w = def
                            .columns
                            .iter()
                            .find(|c| c.field() == *field)
                            .map_or(32, column_width);
                        (key.clone(), w)
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        Some((
            Chain { leaves, ons },
            Search {
                tables,
                edges,
                rels: Vec::new(),
                outer,
                class,
                class_ons,
                widths,
                needed: crate::join_search::needed_keys(),
            },
        ))
    }

    /// `process_equivalence` over the chain's clauses, in ON order: each
    /// clause's written left column, then its right, joins the class of
    /// either, two classes merging the second's members after the first's.
    /// A class of three or more members is returned with the ONs it
    /// absorbed, and those edges are taken out of `edges`; empty when no
    /// column is in two clauses. `None` for two such classes, or a class
    /// with two members in one table.
    #[allow(clippy::type_complexity)]
    fn chain_class(
        edges: &mut Vec<Edge>,
        reversed: &[bool],
    ) -> Option<(Vec<(usize, String)>, Vec<usize>)> {
        let mut classes: Vec<Vec<(usize, String)>> = Vec::new();
        let mut class_of_edge = Vec::new();
        for (e, rev) in edges.iter().zip(reversed) {
            let (l, r) = ((e.u, e.u_key.clone()), (e.v, e.v_key.clone()));
            let (item1, item2) = if *rev { (r, l) } else { (l, r) };
            let find = |c: &Vec<Vec<(usize, String)>>, m: &(usize, String)| {
                c.iter().position(|members| members.contains(m))
            };
            let at = match (find(&classes, &item1), find(&classes, &item2)) {
                (Some(i), Some(j)) if i == j => i,
                (Some(i), Some(j)) => {
                    let moved = classes.remove(j);
                    let i = if j < i { i - 1 } else { i };
                    for c in class_of_edge.iter_mut() {
                        if *c == j {
                            *c = i;
                        } else if *c > j {
                            *c -= 1;
                        }
                    }
                    classes[i].extend(moved);
                    i
                }
                (Some(i), None) => {
                    classes[i].push(item2);
                    i
                }
                (None, Some(j)) => {
                    classes[j].push(item1);
                    j
                }
                (None, None) => {
                    classes.push(vec![item1, item2]);
                    classes.len() - 1
                }
            };
            class_of_edge.push(at);
        }
        let big: Vec<usize> = (0..classes.len())
            .filter(|&i| classes[i].len() > 2)
            .collect();
        let [class_at] = big.as_slice() else {
            return big.is_empty().then(|| (Vec::new(), Vec::new()));
        };
        let class = classes[*class_at].clone();
        let mut tables: Vec<usize> = class.iter().map(|(t, _)| *t).collect();
        tables.sort_unstable();
        if tables.windows(2).any(|w| w[0] == w[1]) {
            return None;
        }
        let mut class_ons = Vec::new();
        let mut kept = Vec::new();
        for (e, c) in std::mem::take(edges).into_iter().zip(class_of_edge) {
            if c == *class_at {
                class_ons.push(e.on);
            } else {
                kept.push(e);
            }
        }
        *edges = kept;
        Some((class, class_ons))
    }

    /// The side of a join a relation is, joined on `key` of base table
    /// `key_table`.
    fn search_side(
        &self,
        search: &Search,
        rel: usize,
        key: &str,
        key_table: usize,
        class: bool,
    ) -> Option<JoinSide> {
        let (table, filter) = &search.tables[key_table];
        let base = self.join_side(table, JoinKey::Column(column_of(key)?), filter, 0)?;
        let r = &search.rels[rel];
        let width = search.width(r.set);
        if r.set.count_ones() == 1 {
            return Some(JoinSide { width, ..base });
        }
        // Joined on the class, a relation merged on it is in its order.
        let keyed = r.keyed.as_ref().filter(|_| class);
        Some(JoinSide {
            width,
            ..joinrel_side(r.rows, r.best.as_ref()?, keyed, &base)
        })
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
        let clause = search.clause_between(sa, sb)?;
        let side_a = self.search_side(search, a, &clause.a_key, clause.a_table, clause.class)?;
        let side_b = self.search_side(search, b, &clause.b_key, clause.b_table, clause.class)?;
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
                    keyed: None,
                });
                search.rels.len() - 1
            }
        };
        // `truncate_useless_pathkeys`: the join's order is of use above it
        // only while a clause still to come is on its key, which for the
        // class is a member outside the relation; an edge's own clause is
        // inside it now. (The search runs with no LIMIT, and an ORDER BY is
        // applied after it, so no query order is there to serve.)
        let useful = clause.class && search.class.iter().any(|(t, _)| set & (1 << *t) == 0);
        let key_class = if useful {
            clause.key_class
        } else {
            secantus_pgplan::joincost::USELESS_KEYS
        };
        let rel = &mut search.rels[idx];
        let combo = rel.built.len();
        rel.built.push(Built { a, b, clause, left });
        rel.paths
            .add_paths_on(combo, key_class, &side_a, &side_b, left, true);
        Some(())
    }

    /// `standard_join_search` over the chain's tables; the index of the
    /// relation of all of them.
    fn search_four(&self, search: &mut Search) -> Option<usize> {
        let n = search.tables.len();
        for i in 0..n {
            search.rels.push(Rel {
                set: 1 << i,
                rows: 0.0,
                paths: JoinRel::new(false),
                built: Vec::new(),
                best: None,
                keyed: None,
            });
        }
        let mut levels: Vec<Vec<usize>> = vec![Vec::new(), (0..n).collect()];
        let connected = |s: &Search, a: usize, b: usize| {
            let (sa, sb) = (s.rels[a].set, s.rels[b].set);
            sa & sb == 0 && s.clause_between(sa, sb).is_some()
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
            let classed = !search.class.is_empty();
            for &i in &built {
                let rel = &mut search.rels[i];
                rel.best = Some(rel.paths.cheapest(None, rel.rows)?);
                if classed {
                    rel.keyed = rel.paths.cheapest_keyed_on(1);
                }
            }
            levels.push(built);
        }
        levels[n].first().copied()
    }

    /// The rows of relation `rel` in the plan its cheapest path is, or its
    /// cheapest path in the class's order (`keyed`: a merge join above
    /// reads it unsorted).
    fn run_searched(
        &self,
        search: &Search,
        chain: &Chain<'_>,
        rel: usize,
        keyed: bool,
    ) -> PgWireResult<Option<Input>> {
        let r = &search.rels[rel];
        if r.set.count_ones() == 1 {
            let leaf = chain.leaves[r.set.trailing_zeros() as usize];
            return Ok(Some(Input {
                rows: self.join_rows(leaf)?,
                keys: secantus_pgplan::joins::node_keys(leaf),
            }));
        }
        let choice = if keyed {
            r.keyed.as_ref()
        } else {
            r.best.as_ref()
        };
        let Some(best) = choice else {
            return Ok(None);
        };
        let built = &r.built[best.combo];
        let clause = &built.clause;
        let (a_key, a_table, b_key) = (clause.a_key.clone(), clause.a_table, clause.b_key.clone());
        // A merge join reads a side "by index" when it is in the key's
        // order already: for a join relation, its path in the class's.
        let (a_sorted, b_sorted) = match best.method {
            JoinMethod::MergeOuterA {
                a_by_index,
                b_by_index,
            }
            | JoinMethod::MergeOuterB {
                a_by_index,
                b_by_index,
            }
            | JoinMethod::MergeRightB {
                a_by_index,
                b_by_index,
            } => (a_by_index, b_by_index),
            _ => (false, false),
        };
        let (Some(a), Some(b)) = (
            self.run_searched(search, chain, built.a, clause.class && a_sorted)?,
            self.run_searched(search, chain, built.b, clause.class && b_sorted)?,
        ) else {
            return Ok(None);
        };
        let rows_of = |rel: usize, table: usize, key: &str| {
            if search.rels[rel].set.count_ones() == 1 {
                let (name, filter) = &search.tables[table];
                column_of(key)
                    .and_then(|c| self.join_side(name, JoinKey::Column(c), filter, 0))
                    .map_or(0.0, |s| s.rows)
            } else {
                search.rels[rel].rows
            }
        };
        let a_rows = rows_of(built.a, a_table, &a_key);
        // The hashed side's table size, as `ExecHashTableCreate` sizes it
        // from the plan's estimate, and the outer key whose hash places a
        // row in its batch.
        let hashed = match best.method {
            JoinMethod::HashOuterA => {
                Some((built.b, rows_of(built.b, clause.b_table, &b_key), &a_key))
            }
            JoinMethod::HashOuterB | JoinMethod::HashRightB => Some((built.a, a_rows, &b_key)),
            _ => None,
        };
        let batches = hashed.and_then(|(rel, rows, outer_key)| {
            let width = search.width(search.rels[rel].set)?;
            let (nbuckets, nbatch) = secantus_pgplan::joincost::hash_table_size(rows, width);
            let ty = chain
                .leaves
                .iter()
                .find_map(|n| leaf_key_type(n, outer_key))?;
            (nbatch > 1).then_some((nbuckets, nbatch, ty))
        });
        let step = Step {
            method: best.method,
            left: built.left,
            a_key: a_key.clone(),
            b_key: b_key.clone(),
            a_rows,
            on: None,
            filters: Vec::new(),
            batches,
        };
        let a_type = chain
            .leaves
            .iter()
            .find_map(|n| leaf_key_type(n, &step.a_key));
        let on = clause.on.and_then(|i| chain.ons[i]);
        self.run_step(&step, a, b, a_type, on)
    }

    /// Whether `node` is a chain of four to eight tables whose join order is
    /// searched.
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
    /// four to eight tables, or `None` for any other join.
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
        let Some(joined) = self.run_searched(&search, &chain, top, false)? else {
            return Ok(None);
        };
        // The ONs the class's derived clauses stood in for decide every row.
        let mut rows = joined.rows;
        for &i in &search.class_ons {
            if let Some(expr) = chain.ons[i] {
                let mut kept = Vec::with_capacity(rows.len());
                for d in rows {
                    let keep = matches!(
                        secantus_pgplan::apply_row_expr(expr, &d).map_err(|e| Self::err(&e))?,
                        Bson::Boolean(true)
                    );
                    if keep {
                        kept.push(d);
                    }
                }
                rows = kept;
            }
        }
        Ok(Some(rows))
    }
}
