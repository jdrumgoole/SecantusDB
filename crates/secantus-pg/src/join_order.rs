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
//! INNER clauses that share a column make equivalence classes over three or
//! more tables (`Search::classes`, as many as the clauses make): each
//! one's members in the order `process_equivalence` adds them (each ON's
//! left column, then its right, as written), any two relations holding
//! members of one joined on its derived clause, the first member each side
//! (`generate_join_implied_equalities_normal`). A merge join on a class
//! leaves its rows in that class's order, which a merge join above on it
//! reads unsorted (`Rel::keyed`), and paths of one relation in two classes'
//! orders are both kept (`JoinRel::add_paths_on`). An outer join's key in
//! a class -- or in the two-member class an inner join's equality makes --
//! reads a side merged on that class unsorted (`Search::key_ec`). Integer
//! keys only, as in `join_search`: a derived clause is matched by its keys'
//! values, and the ONs it stands in for are checked where their tables
//! meet.
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
//!
//! Under a LIMIT (no ORDER BY, or one of a plain column) the search also
//! takes three tables, and two whose WHERE holds a join filter, and plans
//! as PostgreSQL does for a positive `tuple_fraction`: every relation
//! weighs startup costs (`consider_startup`) and keeps its whole pathlist,
//! each base table's paths are its scans (`PgHandler::limit_base_paths`:
//! sequential, index and bitmap, whole-index in a key's order, and index
//! and bitmap scans parameterized by a clause's other table), and each join
//! relation's are `add_paths_to_joinrel`'s over its inputs' pathlists
//! (`JoinRel::add_paths_general`: nested loops over every outer path,
//! parameterized inners and Memoizes of them included). The plan is then
//! the cheapest under a Limit (`limit_choice`), or, with an ORDER BY, the
//! cheapest of the paths already in its order and a bounded Sort of the
//! cheapest (`ordered_limit_choice`).

use super::*;
use crate::join_search::{alias_of, column_of, filter_fixes, joinrel_side, Input, Step};
use secantus_pgplan::joincost::{
    InPath, JoinChoice, JoinMethod, JoinRel, JoinSide, MemoKey, RelPaths,
};
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

/// `cpu_operator_cost`.
const CPU_OPERATOR_COST: f64 = 0.0025;

/// An edge's `on` for a WHERE equality: no ON of the tree, an inner clause.
const ON_WHERE: usize = usize::MAX;

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
    /// A class's derived clause rather than an edge: the class's index in
    /// `Search::classes`.
    class: Option<usize>,
    /// The class (by index) the `a` or `b` key is a member of: for an edge
    /// (an outer join's clause), that side, merged on the class, is in its
    /// order, and a LEFT join's merge on its preserved `a` side comes out in
    /// it (`initialize_mergeclause_eclasses` finds the member's class).
    a_in_class: Option<usize>,
    b_in_class: Option<usize>,
    /// The equivalence class it is in: `class_id` of a class, 2 + its
    /// index for an edge.
    key_class: usize,
    /// The other edges between the two relations, in edge order (the ONs',
    /// then the WHERE's): `(a_key, a_table, b_key, b_table)` each, joined
    /// on with the first as further merge or hash keys.
    extra: Vec<(String, usize, String, usize)>,
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
    /// Its cheapest path in each class's order (a merge join on it), by
    /// the class's index.
    keyed: Vec<Option<JoinChoice>>,
    /// Under a LIMIT, once its level is done: its whole pathlist, in
    /// `add_path`'s order, and `set_cheapest`'s cheapest-total and
    /// cheapest-startup paths in it.
    list: Vec<JoinChoice>,
    cheapest: (usize, usize),
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
    /// A FULL join's are its whole sides, and it never commutes: the third
    /// field.
    outer: Vec<(u8, u8, bool)>,
    /// The equivalence classes of three or more members the clauses
    /// sharing a column make (`root->eq_classes` order): each one's members
    /// `(table, key)` in `ec_members` order. Their clauses are not `edges`.
    classes: Vec<Vec<(usize, String)>>,
    /// The ONs the classes stand in for, each checked where its two tables
    /// first meet.
    class_ons: Vec<(usize, u8)>,
    /// Each table's columns, by joined-row key, with their widths
    /// (`get_typavgwidth`), and the keys the query reads above the join;
    /// `None` when that is not known, and then no width is.
    widths: Vec<Vec<(String, i64)>>,
    needed: Option<Vec<String>>,
    /// A LIMIT's `(count, offset)` over the join: every relation then
    /// weighs startup costs and keeps its whole pathlist, and the plan is
    /// the one a Limit over it makes cheapest (`limit_choice`).
    limit: Option<(f64, f64)>,
    /// The WHERE's join filters, under a LIMIT: each's tables (a bit
    /// each), selectivity, operators a row, and index in the WHERE's
    /// filters. Each is evaluated by the first join holding its tables.
    filters: Vec<(u8, f64, f64, usize)>,
    /// Each table's alias, in FROM order.
    aliases: Vec<String>,
    /// Under a LIMIT, each base table's pathlist (`set_base_rel_pathlists`):
    /// its paths, each with the key an index scan returns its rows in the
    /// order of, and its cheapest-total and cheapest-startup paths.
    #[allow(clippy::type_complexity)]
    base_lists: Vec<(Vec<(InPath, Option<String>)>, usize, usize)>,
    /// Under a LIMIT with an ORDER BY of one column, its joined-row key and
    /// the equivalence class its order is (`query_pathkeys`): the class's,
    /// an inner join's clause's, or its own (`ORDER_CLASS`). Paths in that
    /// order are of use at every level.
    order: Option<(String, usize)>,
    /// Each edge's outer join (`Search::edges` order): the preserved table
    /// of a LEFT join, `usize::MAX` for a FULL join, `None` for an inner
    /// join's clause. An outer join's clause moves only into a scan of its
    /// nullable side (`join_clause_is_movable_to`).
    edge_outer: Vec<Option<usize>>,
    /// The search keeps whole pathlists, base tables' paths parameterized
    /// scans among them (`add_paths_general`): always under a LIMIT, and
    /// first without one.
    general: bool,
    /// Each base table's parameterized scans' clause: by its index in the
    /// table's pathlist, the table's key and the outer table's.
    #[allow(clippy::type_complexity)]
    param_keys: Vec<Vec<Option<(String, String)>>>,
}

/// Each base table's pathlist under a LIMIT (`Search::base_lists`) and its
/// parameterized scans' clause keys (`Search::param_keys`).
type BaseLists = (
    Vec<(Vec<(InPath, Option<String>)>, usize, usize)>,
    Vec<Vec<Option<(String, String)>>>,
);

/// The class of an ORDER BY column no clause reads: its own.
const ORDER_CLASS: usize = 1 << 20;

/// The ids past which `JoinRel` keys the second and later classes' orders.
const CLASS_IDS: usize = 1 << 19;

/// The id `JoinRel` keys class `k`'s order by (`Search::classes` index):
/// 1 for the first, distinct from every edge's `2 + i` and `ORDER_CLASS`.
fn class_id(k: usize) -> usize {
    if k == 0 {
        1
    } else {
        CLASS_IDS + k
    }
}

/// The class index a `class_id` names.
fn class_index(id: usize) -> Option<usize> {
    match id {
        1 => Some(0),
        _ if id > CLASS_IDS && id < ORDER_CLASS => Some(id - CLASS_IDS),
        _ => None,
    }
}

/// A chain of four to eight tables: its leaves, in FROM order, and its
/// joins' ONs.
struct Chain<'a> {
    leaves: Vec<&'a JoinNode>,
    ons: Vec<Option<&'a secantus_pgplan::ColumnExpr>>,
}

/// One join of the tree as `four_table_chain` walks it: the tables (a bit
/// each) on its two sides, and the joins (by index) that are its two sides.
struct Walked<'a> {
    node: &'a JoinNode,
    left: u8,
    right: u8,
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
                left_join: l_join,
                right_join: r_join,
            });
            let inner = l_inner && r_inner && *kind == JoinKind::Inner;
            Some((l | r, inner, Some(joins.len() - 1)))
        }
        _ => None,
    }
}

/// Which of a relation's paths a join above reads.
#[derive(Clone, Copy)]
enum Pick {
    /// Its cheapest path (`Rel::best`).
    Best,
    /// Its cheapest path in the order of the class at this index
    /// (`Rel::keyed`).
    Keyed(usize),
    /// Its path at this index in `Rel::list` (a LIMIT's search).
    Path(usize),
}

impl Search {
    /// The class (by index) `key` of base table `t` is a member of.
    fn member_class(&self, t: usize, key: &str) -> Option<usize> {
        self.classes
            .iter()
            .position(|c| c.iter().any(|(ct, k)| *ct == t && k == key))
    }

    /// The equivalence class `key` of base table `t` is in, as a merge
    /// clause finds it (`initialize_mergeclause_eclasses`, through
    /// `get_eclass_for_sort_expr`): a class of three or more members, or
    /// the two-member class an inner join's equality on it makes (an
    /// edge's `2 + i`), else the one-member class the first outer join's
    /// clause on it made. An outer join's clause puts its two keys in no
    /// class together, but its key's merge order is the class that key is
    /// in.
    fn key_ec(&self, t: usize, key: &str) -> Option<usize> {
        if let Some(k) = self.member_class(t, key) {
            return Some(class_id(k));
        }
        let holds = |e: &Edge| (e.u == t && e.u_key == key) || (e.v == t && e.v_key == key);
        let inner = self
            .edges
            .iter()
            .enumerate()
            .find(|(i, e)| self.edge_outer.get(*i).copied().flatten().is_none() && holds(e));
        // A key only outer joins' clauses read: the one-member class the
        // first of them made for it, which every later one finds again.
        inner
            .or_else(|| self.edges.iter().enumerate().find(|(_, e)| holds(e)))
            .map(|(i, _)| 2 + i)
    }

    /// Whether class `k` has a member outside the tables `set`: a join
    /// clause on it is still to come above a relation of them.
    fn class_open(&self, k: usize, set: u8) -> bool {
        self.classes[k].iter().any(|(t, _)| set & (1 << t) == 0)
    }

    /// Whether `key` of base table `t` is a member of a class with a member
    /// outside `set` (`generate_base_implied_equalities` marks every member
    /// needed by every table of its class).
    fn class_member_needed(&self, set: u8, t: usize, key: &str) -> bool {
        self.member_class(t, key)
            .is_some_and(|k| self.class_open(k, set))
    }

    /// The tables (a bit each) path `pick` of relation `rel` needs from an
    /// outer side.
    fn path_req_outer(&self, rel: usize, pick: Pick) -> u32 {
        let Pick::Path(i) = pick else { return 0 };
        let r = &self.rels[rel];
        if r.set.count_ones() == 1 {
            let t = r.set.trailing_zeros() as usize;
            return self
                .base_lists
                .get(t)
                .and_then(|(list, _, _)| list.get(i))
                .map_or(0, |(p, _)| p.req_outer);
        }
        r.list.get(i).map_or(0, |c| c.req_outer)
    }

    /// The rows path `pick` of relation `rel` is estimated to return, for a
    /// LIMIT's search's path.
    fn path_rows(&self, rel: usize, pick: Pick) -> Option<f64> {
        let Pick::Path(i) = pick else { return None };
        let r = &self.rels[rel];
        if r.set.count_ones() == 1 {
            let t = r.set.trailing_zeros() as usize;
            return self.base_lists.get(t)?.0.get(i).map(|(p, _)| p.rows);
        }
        r.list.get(i).map(|c| c.rows)
    }

    /// The joined-row keys path `pick` of relation `rel` returns, in the
    /// order `run_step` builds them: its outer side's, then its inner's.
    fn rel_keys(&self, chain: &Chain<'_>, rel: usize, pick: Pick) -> Vec<String> {
        let r = &self.rels[rel];
        if r.set.count_ones() == 1 {
            return secantus_pgplan::joins::node_keys(
                chain.leaves[r.set.trailing_zeros() as usize],
            );
        }
        let choice = match pick {
            Pick::Best => r.best.as_ref(),
            Pick::Keyed(k) => r.keyed.get(k).and_then(Option::as_ref),
            Pick::Path(i) => r.list.get(i),
        };
        let Some(c) = choice else {
            return Vec::new();
        };
        let built = &r.built[c.combo];
        let (a, b) = (
            self.rel_keys(chain, built.a, Pick::Path(c.a_path)),
            self.rel_keys(chain, built.b, Pick::Path(c.b_path)),
        );
        let b_outer = matches!(
            c.method,
            JoinMethod::HashOuterB
                | JoinMethod::MergeOuterB { .. }
                | JoinMethod::NestOuterB
                | JoinMethod::HashFullB
                | JoinMethod::MergeFullB { .. }
                | JoinMethod::HashRightB
                | JoinMethod::MergeRightB { .. }
        );
        if b_outer {
            b.into_iter().chain(a).collect()
        } else {
            a.into_iter().chain(b).collect()
        }
    }

    /// Relation `rel`'s pathlist as a join above reads it, with its
    /// cheapest-total and cheapest-startup paths' indexes: a base table's
    /// one scan, `side`'s. `None` for a table whose cheapest scan is not
    /// the one that starts soonest, or that an index reads in its key's
    /// order: their paths are not modelled.
    fn in_paths(&self, rel: usize) -> Option<(Vec<InPath>, usize, usize)> {
        let r = &self.rels[rel];
        if r.set.count_ones() == 1 {
            let (list, ct, cs) = self.base_lists.get(r.set.trailing_zeros() as usize)?;
            return Some((list.iter().map(|(p, _)| *p).collect(), *ct, *cs));
        }
        let paths = r
            .list
            .iter()
            .map(|c| InPath {
                startup: c.startup,
                total: c.total,
                class: c.class,
                rows: c.rows,
                req_outer: c.req_outer,
                rescan: c.rescan,
                indexed: false,
                memo_key: None,
            })
            .collect();
        Some((paths, r.cheapest.0, r.cheapest.1))
    }

    /// The width of relation `set`'s rows (`build_joinrel_tlist`): each of
    /// its tables' columns still needed above it -- read by the query, a
    /// key of a clause to a table outside it, or a member of the class
    /// while a member is outside it (`generate_base_implied_equalities`
    /// marks every member needed by every table of the class).
    /// The columns relation `rel`'s rows carry up to a join above it (its
    /// hashed tuples' columns, `build_joinrel_tlist`) with their one fixed
    /// length; `None` when they are not all of one fixed-length type.
    fn hash_tuple(&self, chain: &Chain<'_>, rel: usize) -> Option<(Vec<String>, i64)> {
        let set = self.rels[rel].set;
        let needed = self.needed.as_ref()?;
        let outside = |t: usize| set & (1 << t) == 0;
        let mut cols = Vec::new();
        let mut typlen = None;
        for (t, widths) in self.widths.iter().enumerate() {
            if outside(t) {
                continue;
            }
            for (key, _) in widths {
                let used = needed.contains(key)
                    || self.edges.iter().any(|e| {
                        (e.u == t && e.u_key == *key && outside(e.v))
                            || (e.v == t && e.v_key == *key && outside(e.u))
                    })
                    || self.class_member_needed(set, t, key);
                if !used {
                    continue;
                }
                let len = match leaf_key_type(chain.leaves[t], key)?.as_str() {
                    "int2" => 2,
                    "int4" | "float4" | "date" => 4,
                    "int8" | "float8" => 8,
                    _ => return None,
                };
                if *typlen.get_or_insert(len) != len {
                    return None;
                }
                cols.push(key.clone());
            }
        }
        Some((cols, typlen?))
    }

    fn width(&self, set: u8) -> Option<i64> {
        let needed = self.needed.as_ref()?;
        let outside = |t: usize| set & (1 << t) == 0;
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
                    || self.class_member_needed(set, t, key);
                if used {
                    width += w;
                }
            }
        }
        Some(width)
    }

    /// `join_is_legal` for joining relations `a` and `b`: `None` when the
    /// join would break an outer join; else whether it performs one, and
    /// if so whether `b` holds its left side (`reversed`) and whether it is
    /// a FULL join.
    fn join_is_legal(&self, a: u8, b: u8) -> Option<Option<(bool, bool)>> {
        let joined = a | b;
        let mut matched: Option<(bool, bool)> = None;
        let mut must_be_left = false;
        for &(lhs, rhs, full) in &self.outer {
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
                matched = Some((false, full));
            } else if sub(lhs, b) && sub(rhs, a) {
                if matched.is_some() {
                    return None;
                }
                matched = Some((true, full));
            } else {
                if a & rhs != 0 && b & rhs != 0 {
                    continue;
                }
                // Only a LEFT join's right side takes another join in.
                if full || joined & lhs != 0 {
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

    /// Whether a join clause connects relations `a` and `b`
    /// (`have_relevant_joinclause`: an edge between them, or a class with
    /// members on both sides, `have_relevant_eclass_joinclause`).
    fn has_clause(&self, a: u8, b: u8) -> bool {
        let edge = self.edges.iter().any(|e| {
            let (u, v) = (1u8 << e.u, 1u8 << e.v);
            (a & u != 0 && b & v != 0) || (a & v != 0 && b & u != 0)
        });
        let in_set = |c: &Vec<(usize, String)>, set: u8| c.iter().any(|(t, _)| set & (1 << t) != 0);
        edge || self.classes.iter().any(|c| in_set(c, a) && in_set(c, b))
    }

    /// The clause between relations `a` and `b`, with any further edges
    /// between them as its extra keys; `None` when there is none, or when
    /// two classes both join them (whose merge order the search does not
    /// model).
    fn clause_between(&self, a: u8, b: u8) -> Option<Clause> {
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
            if let Some(f) = found.as_mut() {
                let f: &mut Clause = f;
                f.extra.push(clause);
                continue;
            }
            let (a_key, a_table, b_key, b_table) = clause;
            found = Some(Clause {
                extra: Vec::new(),
                a_in_class: self.member_class(a_table, &a_key),
                b_in_class: self.member_class(b_table, &b_key),
                a_key,
                a_table,
                b_key,
                b_table,
                on: Some(e.on),
                class: None,
                key_class: 2 + i,
            });
        }
        // A class with members on both sides: its first member each side
        // (`generate_join_implied_equalities_normal`), the leading clause,
        // the edges beside it further keys (`Merge Cond: (class) AND
        // (edge)`).
        let mut joining = (0..self.classes.len()).filter_map(|k| {
            let first = |set: u8| self.classes[k].iter().find(|(t, _)| set & (1 << t) != 0);
            Some((k, first(a)?, first(b)?))
        });
        let class = joining.next();
        if joining.next().is_some() {
            return None;
        }
        if let Some((k, (at, ak), (bt, bk))) = class {
            let extra: Vec<(String, usize, String, usize)> = match found.take() {
                Some(f) => std::iter::once((f.a_key, f.a_table, f.b_key, f.b_table))
                    .chain(f.extra)
                    .collect(),
                None => Vec::new(),
            };
            found = Some(Clause {
                a_key: ak.clone(),
                a_table: *at,
                b_key: bk.clone(),
                b_table: *bt,
                on: None,
                class: Some(k),
                a_in_class: Some(k),
                b_in_class: Some(k),
                key_class: class_id(k),
                extra,
            });
        }
        found
    }
}

impl PgHandler {
    /// `node` as a chain of four to eight stored tables joined INNER or
    /// LEFT, each on one column equality to a table before it; the
    /// clauses sharing a column make at most one equivalence class.
    fn four_table_chain<'a>(
        &self,
        node: &'a JoinNode,
        limited: bool,
        ordered: bool,
    ) -> Option<(Chain<'a>, Search)> {
        // The tree, as `deconstruct_jointree` reads it: its tables left to
        // right, and its joins bottom-up, each left side before its right.
        let mut leaves: Vec<&'a JoinNode> = Vec::new();
        let mut joins: Vec<Walked<'a>> = Vec::new();
        walk_tree(node, &mut leaves, &mut joins)?;
        // Four tables to `join_collapse_limit`'s eight; three only when a
        // join's right side is a join (a left-deep three is
        // `three_table_plan`'s), or under a LIMIT, which only this search
        // plans for; two under a LIMIT whose WHERE holds a join filter,
        // which only this search places, or with an ORDER BY, whose plan
        // only this search weighs against a bounded Sort.
        let bushy = joins.iter().any(|j| j.right.count_ones() > 1);
        // The WHERE's equalities between tables are join clauses too; with
        // one, three tables' clauses can make a cycle, which only this
        // search takes.
        let where_clauses = if limited {
            crate::join_search::where_clauses_and_filters().map(|(e, _)| e)
        } else {
            crate::join_search::where_join_clauses()
        }
        .unwrap_or_default();
        let limit_filtered = limited
            && crate::join_search::where_clauses_and_filters().is_some_and(|(_, f)| !f.is_empty());
        if !(4..=8).contains(&leaves.len())
            && !(leaves.len() == 3 && (bushy || limited || !where_clauses.is_empty()))
            && !(leaves.len() == 2 && (limit_filtered || (limited && ordered)))
        {
            return None;
        }
        let mut ons = Vec::new();
        let mut tables = Vec::new();
        let mut edges = Vec::new();
        let mut reversed = Vec::new();
        let mut kinds = Vec::new();
        let mut further: Vec<(String, String)> = Vec::new();
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
                on_other,
                ..
            } = w.node
            else {
                return None;
            };
            // A FULL join with a join filter is a hash full join only.
            if *kind == JoinKind::Full && *on_other {
                return None;
            }
            if !matches!(kind, JoinKind::Inner | JoinKind::Left | JoinKind::Full)
                || !merged.is_empty()
                || !expr_keys.is_empty()
                || ec.as_ref().is_some_and(|ec| {
                    // A two-table join's classes that only restate its own
                    // clauses (no constant, no equality within a side)
                    // change nothing here.
                    !ec.left_restrict.is_empty()
                        || !ec.right_restrict.is_empty()
                        || ec.join_keys.len() != equi.len()
                        || ec.join_keys.iter().any(|k| !equi.contains(k))
                })
            {
                return None;
            }
            // An inner join's further equalities (a comma join's WHERE, or
            // an ON of several) are clauses too, added as edges below.
            let [(lk, rk), rest @ ..] = equi.as_slice() else {
                return None;
            };
            if !rest.is_empty() {
                if *kind != JoinKind::Inner || *on_other {
                    return None;
                }
                further.extend(rest.iter().cloned());
            }
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
        // join passes what is non-null above it to its left side and its
        // own clause's tables to its right side (a NULL-extended row of
        // the right side never passes the ON), and a LEFT join whose
        // nullable side holds such a table is inner.
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
            let (to_left, to_right) = match kinds[i] {
                JoinKind::Inner => (upper | local, upper | local),
                // A FULL join passes nothing down.
                JoinKind::Full => (0, 0),
                _ => (upper, local),
            };
            if let Some(l) = w.left_join {
                reduce(l, to_left, joins, edges, kinds);
            }
            if let Some(r) = w.right_join {
                reduce(r, to_right, joins, edges, kinds);
            }
        }
        reduce(joins.len() - 1, 0, &joins, &edges, &mut kinds);
        // Each LEFT join's `SpecialJoinInfo` (`make_outerjoininfo`), bottom
        // up. Its minimum sides start as the tables its clause reads, with
        // the tables inner-joined in its nullable side
        // (`deconstruct_recurse`'s `inner_join_rels`). A lower LEFT join
        // then adds its whole syntactic sides to them when the two cannot
        // commute: one in our left side whose nullable side our clause
        // reads, not strictly; one in our right side whose nullable side
        // our clause reads, or whose left side it does not. A clause is an
        // equality, strict for the tables it reads.
        let mut inner_rels: Vec<u8> = Vec::with_capacity(joins.len());
        for (i, w) in joins.iter().enumerate() {
            let below = w.left_join.map_or(0, |j| inner_rels[j])
                | w.right_join.map_or(0, |j| inner_rels[j]);
            inner_rels.push(if kinds[i] == JoinKind::Inner {
                w.left | w.right
            } else {
                below
            });
        }
        // `(min_lefthand, min_righthand, syn_lefthand, syn_righthand,
        // FULL)`. A FULL join's minimum sides are its syntactic ones, and a
        // join over one that overlaps either of its sides takes both whole.
        let mut specials: Vec<(u8, u8, u8, u8, bool)> = Vec::new();
        for (i, w) in joins.iter().enumerate() {
            if kinds[i] == JoinKind::Full {
                specials.push((w.left, w.right, w.left, w.right, true));
                continue;
            }
            if kinds[i] != JoinKind::Left {
                continue;
            }
            let clause = (1u8 << edges[i].u) | (1u8 << edges[i].v);
            let right_inner = w.right_join.map_or(0, |j| inner_rels[j]);
            let mut min_l = clause & w.left;
            let mut min_r = (clause | right_inner) & w.right;
            for &(o_min_l, o_min_r, o_syn_l, o_syn_r, o_full) in &specials {
                if o_full {
                    let both = o_syn_l | o_syn_r;
                    if w.left & both != 0 {
                        min_l |= both;
                    }
                    if w.right & both != 0 {
                        min_r |= both;
                    }
                    continue;
                }
                if w.left & o_syn_r != 0 && clause & o_syn_r != 0 && clause & o_min_r == 0 {
                    min_l |= o_syn_l | o_syn_r;
                }
                if w.right & o_syn_r != 0 && (clause & o_syn_r != 0 || clause & o_min_l == 0) {
                    min_r |= o_syn_l | o_syn_r;
                }
            }
            if min_l == 0 {
                min_l = w.left;
            }
            if min_r == 0 {
                min_r = w.right;
            }
            specials.push((min_l, min_r, w.left, w.right, false));
        }
        let outer = specials.iter().map(|&(l, r, _, _, f)| (l, r, f)).collect();
        // The WHERE's equalities between two tables, edges of no ON (`ON_WHERE`):
        // inner joins' clauses, the WHERE above still deciding every row.
        for (lk, rk) in further.iter().chain(&where_clauses) {
            let table = |key: &str| (0..leaves.len()).find(|&t| aliases[t] == alias_of(key));
            let (u, v) = (table(lk)?, table(rk)?);
            if u == v {
                return None;
            }
            // A comma join's WHERE equality is already its join's edge.
            let same =
                |e: &Edge| (e.u_key == *lk && e.v_key == *rk) || (e.u_key == *rk && e.v_key == *lk);
            if edges.iter().any(same) {
                continue;
            }
            edges.push(Edge {
                u,
                u_key: lk.clone(),
                v,
                v_key: rk.clone(),
                on: ON_WHERE,
            });
            reversed.push(false);
        }
        // A column in two inner joins' clauses: the equivalence class they
        // make. An outer join's clause is not one of its members'
        // equalities (`distribute_qual_to_rels` delays it), and stays an
        // edge.
        let (classes, class_ons) = Self::chain_class(&mut edges, &reversed, &kinds)?;
        // Each edge's outer join: a LEFT join's preserved table, or
        // `usize::MAX` for a FULL join's.
        let edge_outer: Vec<Option<usize>> = edges
            .iter()
            .map(|e| match kinds.get(e.on) {
                Some(JoinKind::Left) => Some(e.u),
                Some(JoinKind::Full) => Some(usize::MAX),
                _ => None,
            })
            .collect();
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
                .chain(classes.iter().flatten().map(|(_, k)| k));
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
        // A class's derived clauses are matched by value: integers of one
        // type only.
        for class in &classes {
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
                classes,
                class_ons,
                widths,
                needed: crate::join_search::needed_keys(),
                limit: None,
                filters: Vec::new(),
                aliases,
                base_lists: Vec::new(),
                order: None,
                edge_outer,
                general: false,
                param_keys: Vec::new(),
            },
        ))
    }

    /// `process_equivalence` over the chain's clauses, in ON order: each
    /// clause's written left column, then its right, joins the class of
    /// either, two classes merging the second's members after the first's
    /// (the second leaving `root->eq_classes`). The classes of three or
    /// more members are returned in that list's order with the ONs they
    /// absorbed, and those edges are taken out of `edges`; none when no
    /// column is in two clauses. `None` for a class with two members in one
    /// table (a restriction of that table, not modelled).
    #[allow(clippy::type_complexity)]
    fn chain_class(
        edges: &mut Vec<Edge>,
        reversed: &[bool],
        kinds: &[JoinKind],
    ) -> Option<(Vec<Vec<(usize, String)>>, Vec<(usize, u8)>)> {
        let mut classes: Vec<Vec<(usize, String)>> = Vec::new();
        let mut class_of_edge = Vec::new();
        for (e, rev) in edges.iter().zip(reversed) {
            if e.on != ON_WHERE && kinds[e.on] != JoinKind::Inner {
                class_of_edge.push(usize::MAX);
                continue;
            }
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
                    for c in class_of_edge.iter_mut().filter(|c| **c != usize::MAX) {
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
        for &i in &big {
            let mut tables: Vec<usize> = classes[i].iter().map(|(t, _)| *t).collect();
            tables.sort_unstable();
            if tables.windows(2).any(|w| w[0] == w[1]) {
                return None;
            }
        }
        let mut class_ons = Vec::new();
        let mut kept = Vec::new();
        for (e, c) in std::mem::take(edges).into_iter().zip(class_of_edge) {
            if big.contains(&c) {
                class_ons.push((e.on, (1u8 << e.u) | (1u8 << e.v)));
            } else {
                kept.push(e);
            }
        }
        *edges = kept;
        Some((big.iter().map(|&i| classes[i].clone()).collect(), class_ons))
    }

    /// The side of a join a relation is, joined on `key` of base table
    /// `key_table`.
    fn search_side(
        &self,
        search: &Search,
        rel: usize,
        key: &str,
        key_table: usize,
        class: Option<usize>,
    ) -> Option<JoinSide> {
        let (table, filter) = &search.tables[key_table];
        let base = self.join_side(table, JoinKey::Column(column_of(key)?), filter, 0)?;
        let r = &search.rels[rel];
        let width = search.width(r.set);
        if r.set.count_ones() == 1 {
            return Some(JoinSide { width, ..base });
        }
        // Joined on a class, a relation merged on it is in its order.
        let keyed = class.and_then(|k| r.keyed.get(k)).and_then(Option::as_ref);
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
        // reversed pair), a FULL join's left.
        let (a, b) = if matches!(outer, Some((true, _))) {
            (b, a)
        } else {
            (a, b)
        };
        let full = matches!(outer, Some((_, true)));
        let left = outer.is_some() && !full;
        let (sa, sb) = (search.rels[a].set, search.rels[b].set);
        let clause = search.clause_between(sa, sb)?;
        let mut side_a =
            self.search_side(search, a, &clause.a_key, clause.a_table, clause.a_in_class)?;
        let mut side_b =
            self.search_side(search, b, &clause.b_key, clause.b_table, clause.b_in_class)?;
        // Further clauses between the two: each side's distinct counts of
        // their keys (`ExtraKeys`). A side in one key's order is in no
        // order of all of them, and a merge on several keys is in an order
        // no clause above reads.
        if !clause.extra.is_empty() {
            let mut keys = (Vec::new(), Vec::new());
            for (ak, at, bk, bt) in &clause.extra {
                let sa = self.search_side(search, a, ak, *at, None)?;
                let sb = self.search_side(search, b, bk, *bt, None)?;
                keys.0.push((sa.ndistinct, sa.default_ndistinct, sa.unique));
                keys.1.push((sb.ndistinct, sb.default_ndistinct, sb.unique));
            }
            side_a.extra = secantus_pgplan::joincost::ExtraKeys::of(&keys.0)?;
            side_b.extra = secantus_pgplan::joincost::ExtraKeys::of(&keys.1)?;
            side_a.unique |= keys.0.iter().any(|k| k.2);
            side_b.unique |= keys.1.iter().any(|k| k.2);
            for s in [&mut side_a, &mut side_b] {
                s.ordered = None;
                s.scan_ordered = false;
            }
        }
        let set = sa | sb;
        // The join filters this join evaluates first: their selectivity
        // in its rows (`clauselist_selectivity`, each independent) and
        // their operators' cost a tuple.
        let firsts: Vec<&(u8, f64, f64, usize)> = search
            .filters
            .iter()
            .filter(|(t, ..)| t & !set == 0 && t & !sa != 0 && t & !sb != 0)
            .collect();
        let filter_sel: f64 = firsts.iter().map(|f| f.1).product();
        let filter_ops: f64 = firsts.iter().map(|f| f.2).sum();
        let idx = match search.rels.iter().position(|r| r.set == set) {
            Some(i) => i,
            None => {
                search.rels.push(Rel {
                    set,
                    rows: secantus_pgplan::costsize::clamp_row_est(
                        if full {
                            secantus_pgplan::joincost::full_join_rows(&side_a, &side_b)
                        } else {
                            join_rows(&side_a, &side_b, left)
                        } * filter_sel,
                    ),
                    paths: JoinRel::new(search.limit.is_some()),
                    built: Vec::new(),
                    best: None,
                    keyed: Vec::new(),
                    list: Vec::new(),
                    cheapest: (0, 0),
                });
                search.rels.len() - 1
            }
        };
        // `truncate_useless_pathkeys`: the join's order is of use above it
        // only while a clause still to come is on its key, which for the
        // class is a member outside the relation; an edge's own clause is
        // inside it now. (The search runs with no LIMIT, and an ORDER BY is
        // applied after it, so no query order is there to serve.)
        // A LEFT join merged on its preserved side's class member comes out
        // in the class's order.
        let in_class = clause.class.or(if left { clause.a_in_class } else { None });
        let key_class = in_class
            .filter(|&k| {
                (clause.extra.is_empty() || clause.class.is_some()) && search.class_open(k, set)
            })
            .map_or(secantus_pgplan::joincost::USELESS_KEYS, class_id);
        if search.general {
            // Each side's whole pathlist, and the classes its key's order
            // is in: the class for its derived clause, or for an outer
            // join's key that is one of its members.
            let (pa, pb) = (search.in_paths(a)?, search.in_paths(b)?);
            let (sa_set, sb_set) = (u32::from(sa), u32::from(sb));
            let ra = RelPaths {
                side: side_a,
                paths: &pa.0,
                cheapest_total: pa.1,
                cheapest_startup: pa.2,
                set: sa_set,
                width: side_a.width?,
                unique: side_a.unique,
                key_table: 1 << clause.a_table,
            };
            let rb = RelPaths {
                side: side_b,
                paths: &pb.0,
                cheapest_total: pb.1,
                cheapest_startup: pb.2,
                set: sb_set,
                width: side_b.width?,
                unique: side_b.unique,
                key_table: 1 << clause.b_table,
            };
            // The equivalence class each side's key is in: the class for
            // its derived clause, or for an outer join's key that is one of
            // its members; else the clause's own (an inner join's equality
            // makes one of its two keys, `process_equivalence`).
            let own = Some(clause.key_class);
            let a_class = match clause.class {
                Some(k) => Some(class_id(k)),
                None => search.key_ec(clause.a_table, &clause.a_key).or(own),
            };
            let b_class = match clause.class {
                Some(k) => Some(class_id(k)),
                None => search.key_ec(clause.b_table, &clause.b_key).or(own),
            };
            // A merge on several clauses that a side already in the
            // class's order would serve on its first is not modelled.
            if !clause.extra.is_empty() && pa.0.iter().chain(&pb.0).any(|p| p.class.is_some()) {
                return None;
            }
            // `truncate_useless_pathkeys`: an order is of use above a
            // relation while a clause still to come reads it -- the class's
            // while a member is outside it, an edge's while one of its
            // tables is.
            let open: Vec<bool> = (0..search.classes.len())
                .map(|k| search.class_open(k, set))
                .collect();
            let order_class = search.order.as_ref().map(|(_, c)| *c);
            let edge_open: Vec<bool> = search
                .edges
                .iter()
                .map(|e| set & (1 << e.u) == 0 || set & (1 << e.v) == 0)
                .collect();
            // An outer join's clause still to come reads the classes its
            // keys are in (`pathkeys_useful_for_merging` looks through the
            // relation's `joininfo` too).
            let mut outer_reads: Vec<usize> = Vec::new();
            for (i, e) in search.edges.iter().enumerate() {
                if search.edge_outer.get(i).copied().flatten().is_none() || !edge_open[i] {
                    continue;
                }
                for (t, k) in [(e.u, &e.u_key), (e.v, &e.v_key)] {
                    if set & (1 << t) != 0 {
                        outer_reads.extend(search.key_ec(t, k));
                    }
                }
            }
            let useful = move |c: usize| {
                if Some(c) == order_class || outer_reads.contains(&c) {
                    true
                } else if let Some(k) = class_index(c) {
                    open.get(k).copied().unwrap_or(false)
                } else {
                    c.checked_sub(2)
                        .and_then(|i| edge_open.get(i))
                        .copied()
                        .unwrap_or(false)
                }
            };
            // `param_source_rels`: for each outer join whose nullable side
            // this relation partly holds, without its preserved side, every
            // table outside that nullable side (a FULL join's both ways).
            let all = (1u8 << search.tables.len()) - 1;
            let mut param_source = 0u8;
            for &(min_l, min_r, full) in &search.outer {
                if set & min_r != 0 && set & min_l == 0 {
                    param_source |= all & !min_r;
                }
                if full && set & min_l != 0 && set & min_r == 0 {
                    param_source |= all & !min_l;
                }
            }
            let param_source = u32::from(param_source);
            let rel = &mut search.rels[idx];
            let rows = rel.rows;
            let combo = rel.built.len();
            rel.built.push(Built { a, b, clause, left });
            rel.paths.set_filter_cost(CPU_OPERATOR_COST * filter_ops);
            if full {
                rel.paths
                    .add_full_paths_general(combo, a_class, b_class, &ra, &rb, rows);
            } else {
                rel.paths.add_paths_general(
                    combo,
                    a_class,
                    b_class,
                    &useful,
                    &ra,
                    &rb,
                    left,
                    rows,
                    param_source,
                );
            }
            return Some(());
        }
        let rel = &mut search.rels[idx];
        let combo = rel.built.len();
        rel.built.push(Built { a, b, clause, left });
        if full {
            rel.paths.add_full_paths(combo, &side_a, &side_b, true);
        } else {
            rel.paths
                .add_paths_on(combo, key_class, &side_a, &side_b, left, true);
        }
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
                paths: JoinRel::new(search.limit.is_some()),
                built: Vec::new(),
                best: None,
                keyed: Vec::new(),
                list: Vec::new(),
                cheapest: (0, 0),
            });
        }
        let mut levels: Vec<Vec<usize>> = vec![Vec::new(), (0..n).collect()];
        let connected = |s: &Search, a: usize, b: usize| {
            let (sa, sb) = (s.rels[a].set, s.rels[b].set);
            sa & sb == 0 && s.has_clause(sa, sb)
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
            let classes = search.classes.len();
            let limited = search.general;
            for &i in &built {
                let rel = &mut search.rels[i];
                if limited {
                    rel.list = rel.paths.pathlist();
                    rel.cheapest = rel.paths.set_cheapest()?;
                    rel.best = Some(rel.list[rel.cheapest.0]);
                    continue;
                }
                rel.best = Some(rel.paths.cheapest(None, rel.rows)?);
                rel.keyed = (0..classes)
                    .map(|k| rel.paths.cheapest_keyed_on(class_id(k)))
                    .collect();
            }
            levels.push(built);
        }
        levels[n].first().copied()
    }

    /// The rows of relation `rel` in the plan its cheapest path is, or its
    /// cheapest path in the class's order (`keyed`: a merge join above
    /// reads it unsorted).
    ///
    /// `bind`: the outer row a parameterized path is read for (a nested
    /// loop's, `ExecNestLoop` passing its outer tuple's values down as
    /// parameters): a parameterized scan returns the rows whose key equals
    /// the outer row's, and a nested loop over a parameterized inner reads
    /// the inner again for each of its outer rows. `leaves` caches each
    /// table's rows across those reads.
    #[allow(clippy::too_many_arguments)]
    fn run_searched(
        &self,
        search: &Search,
        chain: &Chain<'_>,
        rel: usize,
        pick: Pick,
        bind: &Document,
        leaves: &mut HashMap<usize, Vec<Document>>,
    ) -> PgWireResult<Option<Input>> {
        let r = &search.rels[rel];
        if r.set.count_ones() == 1 {
            let t = r.set.trailing_zeros() as usize;
            let leaf = chain.leaves[t];
            if let std::collections::hash_map::Entry::Vacant(e) = leaves.entry(t) {
                e.insert(self.join_rows(leaf)?);
            }
            let all = &leaves[&t];
            // A scan parameterized by the outer row it is read for: its
            // index's rows whose key equals the outer row's, in the heap's
            // order. Read straight under the nested loop that binds it, the
            // loop's clause matches the same rows.
            let param = match pick {
                Pick::Path(i) => search
                    .param_keys
                    .get(t)
                    .and_then(|keys| keys.get(i))
                    .cloned()
                    .flatten(),
                _ => None,
            };
            let mut rows = match param
                .and_then(|(key, outer_key)| bind.get(&outer_key).map(|value| (key, value.clone())))
            {
                Some((key, value)) => all
                    .iter()
                    .filter(|d| {
                        value != Bson::Null
                            && d.get(&key).is_some_and(|v| {
                                secantus_pgplan::compare_values(v, &value) == Some(Ordering::Equal)
                            })
                    })
                    .cloned()
                    .collect(),
                None => all.clone(),
            };
            // A scan of a whole index returns its rows in its key's order,
            // equal keys in the heap's.
            let order_key = match pick {
                Pick::Path(i) => search
                    .base_lists
                    .get(t)
                    .and_then(|(list, _, _)| list.get(i))
                    .and_then(|(_, k)| k.clone()),
                _ => None,
            };
            if let Some(key) = order_key {
                rows.sort_by(|x, y| {
                    match (
                        x.get(&key).filter(|v| **v != Bson::Null),
                        y.get(&key).filter(|v| **v != Bson::Null),
                    ) {
                        (None, None) => Ordering::Equal,
                        (None, Some(_)) => Ordering::Greater,
                        (Some(_), None) => Ordering::Less,
                        (Some(p), Some(q)) => {
                            secantus_pgplan::compare_values(p, q).unwrap_or(Ordering::Equal)
                        }
                    }
                });
            }
            return Ok(Some(Input {
                rows,
                keys: secantus_pgplan::joins::node_keys(leaf),
            }));
        }
        let choice = match pick {
            Pick::Best => r.best.as_ref(),
            Pick::Keyed(k) => r.keyed.get(k).and_then(Option::as_ref),
            Pick::Path(i) => r.list.get(i),
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
        // Each input: the path the plan reads of it (a LIMIT's search
        // records it), else its cheapest, or its cheapest in the class's
        // order where the merge join reads it presorted.
        let legacy = |in_class: Option<usize>, sorted: bool| match in_class {
            Some(k) if sorted => Pick::Keyed(k),
            _ => Pick::Best,
        };
        let (a_pick, b_pick) = if matches!(pick, Pick::Path(_)) {
            (Pick::Path(best.a_path), Pick::Path(best.b_path))
        } else {
            (
                legacy(clause.a_in_class, a_sorted),
                legacy(clause.b_in_class, b_sorted),
            )
        };
        // A nested loop over a parameterized join reads it again for each
        // outer row, that row bound (`ExecReScan` with the new parameter).
        let b_outer = matches!(best.method, JoinMethod::NestOuterB);
        let nest = matches!(best.method, JoinMethod::Other | JoinMethod::NestOuterB);
        let (outer_rel, outer_pick, inner_rel, inner_pick) = if b_outer {
            (built.b, b_pick, built.a, a_pick)
        } else {
            (built.a, a_pick, built.b, b_pick)
        };
        if nest
            && search.rels[inner_rel].set.count_ones() > 1
            && search.path_req_outer(inner_rel, inner_pick) != 0
        {
            let Some(outer) =
                self.run_searched(search, chain, outer_rel, outer_pick, bind, leaves)?
            else {
                return Ok(None);
            };
            let step = Step {
                method: best.method,
                left: built.left,
                a_key: a_key.clone(),
                b_key: b_key.clone(),
                a_rows: 0.0,
                b_rows: 0.0,
                on: None,
                filters: Vec::new(),
                batches: None,
                hash_tuple: None,
                extra: clause
                    .extra
                    .iter()
                    .map(|(ak, _, bk, _)| (ak.clone(), bk.clone()))
                    .collect(),
            };
            let on = clause.on.and_then(|i| chain.ons.get(i).copied().flatten());
            let mut rows = Vec::new();
            let mut keys = None;
            for o in outer.rows {
                let mut bound = bind.clone();
                for (k, v) in &o {
                    bound.insert(k.clone(), v.clone());
                }
                let Some(inner) =
                    self.run_searched(search, chain, inner_rel, inner_pick, &bound, leaves)?
                else {
                    return Ok(None);
                };
                let one = Input {
                    rows: vec![o],
                    keys: outer.keys.clone(),
                };
                let (a, b) = if b_outer { (inner, one) } else { (one, inner) };
                let Some(joined) = self.run_step(&step, a, b, None, None, on)? else {
                    return Ok(None);
                };
                rows.extend(joined.rows);
                keys = Some(joined.keys);
            }
            let keys = match keys {
                Some(k) => k,
                None => {
                    let mut k = outer.keys;
                    k.extend(search.rel_keys(chain, inner_rel, inner_pick));
                    k
                }
            };
            return self.finish_join(search, chain, built, Input { rows, keys });
        }
        let (Some(a), Some(b)) = (
            self.run_searched(search, chain, built.a, a_pick, bind, leaves)?,
            self.run_searched(search, chain, built.b, b_pick, bind, leaves)?,
        ) else {
            return Ok(None);
        };
        let rows_of = |rel: usize, table: usize, key: &str| {
            // The input path's own rows (a parameterized one's are fewer).
            if let Some(rows) = search.path_rows(rel, if rel == built.a { a_pick } else { b_pick })
            {
                return rows;
            }
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
            JoinMethod::HashOuterB | JoinMethod::HashRightB | JoinMethod::HashFullB => {
                Some((built.a, a_rows, &b_key))
            }
            JoinMethod::HashFullA => {
                Some((built.b, rows_of(built.b, clause.b_table, &b_key), &a_key))
            }
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
        // The hashed side's tuples: the columns needed above it, when all
        // are of one fixed length (`hash_grown_order`).
        let hash_tuple = hashed.and_then(|(rel, _, _)| search.hash_tuple(chain, rel));
        let step = Step {
            method: best.method,
            left: built.left,
            a_key: a_key.clone(),
            b_key: b_key.clone(),
            a_rows,
            b_rows: rows_of(built.b, clause.b_table, &b_key),
            on: None,
            filters: Vec::new(),
            batches,
            hash_tuple,
            extra: clause
                .extra
                .iter()
                .map(|(ak, _, bk, _)| (ak.clone(), bk.clone()))
                .collect(),
        };
        let type_of = |key: &str| chain.leaves.iter().find_map(|n| leaf_key_type(n, key));
        let (a_type, b_type) = (type_of(&step.a_key), type_of(&step.b_key));
        let on = clause.on.and_then(|i| chain.ons.get(i).copied().flatten());
        let Some(joined) = self.run_step(&step, a, b, a_type, b_type, on)? else {
            return Ok(None);
        };
        self.finish_join(search, chain, built, joined)
    }

    /// A join's rows once its inputs are joined: the WHERE's join filters
    /// and the ONs the class stood in for that its two inputs complete.
    fn finish_join(
        &self,
        search: &Search,
        chain: &Chain<'_>,
        built: &Built,
        mut joined: Input,
    ) -> PgWireResult<Option<Input>> {
        // The ONs the class's derived clauses stood in for, each where its
        // two tables first meet: an inner join's ON is its own join's
        // qual, below any outer join that could NULL-extend its tables.
        // A LIMIT's join filters likewise, at the first join holding their
        // tables (the WHERE above still decides every row).
        let (sa, sb) = (search.rels[built.a].set, search.rels[built.b].set);
        for &(tables, _, _, i) in &search.filters {
            let first = tables & !(sa | sb) == 0 && tables & !sa != 0 && tables & !sb != 0;
            if !first {
                continue;
            }
            let Some(f) = crate::join_search::where_filter(i) else {
                return Ok(None);
            };
            let mut kept = Vec::with_capacity(joined.rows.len());
            for d in joined.rows {
                let keep = matches!(
                    secantus_pgplan::apply_row_expr(&f.expr, &d).map_err(|e| Self::err(&e))?,
                    Bson::Boolean(true)
                );
                if keep {
                    kept.push(d);
                }
            }
            joined.rows = kept;
        }
        for &(i, tables) in &search.class_ons {
            let completes = tables & !(sa | sb) == 0 && tables & !sa != 0 && tables & !sb != 0;
            if !completes {
                continue;
            }
            if let Some(expr) = chain.ons.get(i).copied().flatten() {
                let mut kept = Vec::with_capacity(joined.rows.len());
                for d in joined.rows {
                    let keep = matches!(
                        secantus_pgplan::apply_row_expr(expr, &d).map_err(|e| Self::err(&e))?,
                        Bson::Boolean(true)
                    );
                    if keep {
                        kept.push(d);
                    }
                }
                joined.rows = kept;
            }
        }
        Ok(Some(joined))
    }

    /// Whether `node` is a chain of four to eight tables whose join order is
    /// searched.
    pub(crate) fn four_table_plan_applies(&self, node: &JoinNode) -> bool {
        if crate::join_search::where_join_clauses().is_none() || LOCK_ROW_IDS.with(|c| c.get()) {
            return false;
        }
        let Some((_, mut search)) = self.four_table_chain(node, false, false) else {
            return false;
        };
        self.search_four(&mut search).is_some()
    }

    /// `node`'s rows in the plan PostgreSQL's join order search picks for
    /// four to eight tables, or `None` for any other join.
    ///
    /// Under a LIMIT (`limit`: its rows with the OFFSET's, and the
    /// OFFSET's) three tables are searched too, every relation keeping
    /// the paths that start sooner beside its cheapest
    /// (`consider_startup`), and the plan is the one a Limit over it makes
    /// cheapest (`limit_choice`): typically a nested loop that returns its
    /// first rows at once, in its outer side's order.
    ///
    /// With an ORDER BY of one column (`order`, its joined-row key) the
    /// LIMIT's plan is `create_ordered_paths`' pick: a path already in its
    /// order, or the cheapest under a bounded Sort, which the select then
    /// runs (`GENERAL_JOIN_PRESORTED` says it need not).
    pub(crate) fn four_table_join(
        &self,
        node: &JoinNode,
        limit: Option<(usize, usize)>,
        order: Option<&str>,
    ) -> PgWireResult<Option<Vec<Document>>> {
        if order.is_some() && limit.is_none() {
            return Ok(None);
        }
        if limit.is_some() {
            return self.searched_rows(node, limit, order, true);
        }
        // With no LIMIT the search keeps whole pathlists too, so that a
        // nested loop over a parameterized scan is weighed; where that
        // search declines, the one over cheapest paths runs.
        if let Some(rows) = self.searched_rows(node, None, None, true)? {
            return Ok(Some(rows));
        }
        self.searched_rows(node, None, None, false)
    }

    /// [`PgHandler::four_table_join`]'s search, over whole pathlists when
    /// `general`.
    fn searched_rows(
        &self,
        node: &JoinNode,
        limit: Option<(usize, usize)>,
        order: Option<&str>,
        general: bool,
    ) -> PgWireResult<Option<Vec<Document>>> {
        let spanning_modelled = if limit.is_some() {
            crate::join_search::where_clauses_and_filters().is_some()
        } else {
            crate::join_search::where_join_clauses().is_some()
        };
        if !spanning_modelled || LOCK_ROW_IDS.with(|c| c.get()) {
            return Ok(None);
        }
        let Some((chain, mut search)) =
            self.four_table_chain(node, limit.is_some(), order.is_some())
        else {
            return Ok(None);
        };
        if general {
            search.general = true;
            if limit.is_some() {
                let Some(filters) = self.search_filters(&search) else {
                    return Ok(None);
                };
                search.filters = filters;
            }
            if let Some(key) = order {
                let Some(class) = Self::order_class(&search, &chain, key) else {
                    return Ok(None);
                };
                search.order = Some((key.to_string(), class));
            }
            let Some((base_lists, param_keys)) = self.build_base_lists(&search, &chain) else {
                return Ok(None);
            };
            search.base_lists = base_lists;
            search.param_keys = param_keys;
        }
        if let Some((rows, offset)) = limit {
            // `preprocess_limit`: a count of at least one.
            let count = rows.saturating_sub(offset).max(1);
            search.limit = Some((count as f64, offset as f64));
        }
        let Some(top) = self.search_four(&mut search) else {
            return Ok(None);
        };
        let pick = match search.limit {
            Some((count, offset)) => {
                let r = &search.rels[top];
                let chosen = match &search.order {
                    Some((_, class)) => {
                        let Some(width) = search.width(r.set) else {
                            return Ok(None);
                        };
                        secantus_pgplan::joincost::ordered_limit_choice(
                            &r.list,
                            r.cheapest.0,
                            r.rows,
                            width,
                            *class,
                            count,
                            offset,
                        )
                        .map(|(i, presorted)| {
                            crate::GENERAL_JOIN_PRESORTED.with(|c| c.set(presorted));
                            i
                        })
                    }
                    None => secantus_pgplan::joincost::limit_choice(&r.list, r.rows, count, offset),
                };
                let Some(i) = chosen else {
                    return Ok(None);
                };
                Pick::Path(i)
            }
            // The whole result: the cheapest path in total (`set_cheapest`).
            None if search.general => Pick::Path(search.rels[top].cheapest.0),
            None => Pick::Best,
        };
        let Some(joined) = self.run_searched(
            &search,
            &chain,
            top,
            pick,
            &Document::new(),
            &mut HashMap::new(),
        )?
        else {
            return Ok(None);
        };
        Ok(Some(joined.rows))
    }

    /// The class of the ORDER BY column `key` (`Search::order`): the class
    /// when it is a member, the clause's when one inner join's clause reads
    /// it (both its keys one class), the class an outer join's preserved
    /// key is in, else its own -- an outer join's nullable key's too: no
    /// merge join on it leaves its rows in its order
    /// (`build_join_pathkeys` gives a right or full join none). `None` for
    /// a column not an integer.
    fn order_class(search: &Search, chain: &Chain<'_>, key: &str) -> Option<usize> {
        let t = search.aliases.iter().position(|a| a == alias_of(key))?;
        let int = |ty: Option<String>| matches!(ty.as_deref(), Some("int2" | "int4" | "int8"));
        if !int(leaf_key_type(chain.leaves[t], key)) {
            return None;
        }
        if let Some(k) = search.member_class(t, key) {
            return Some(class_id(k));
        }
        // A key an outer join can NULL-extend: its own class, whose order
        // no join above keeps.
        let nullable = (0..search.edges.len()).any(|i| {
            let e = &search.edges[i];
            (e.u_key == key || e.v_key == key)
                && search
                    .edge_outer
                    .get(i)
                    .copied()
                    .flatten()
                    .is_some_and(|preserved| preserved != t)
        });
        if nullable {
            return Some(ORDER_CLASS);
        }
        Some(search.key_ec(t, key).unwrap_or(ORDER_CLASS))
    }

    /// The WHERE's join filters over the search's tables (`Search::filters`);
    /// `None` when one reads a table outside them, a filter's selectivity is
    /// not estimated, or an outer join is in the search (a WHERE over its
    /// nullable side is not placed).
    fn search_filters(&self, search: &Search) -> Option<Vec<(u8, f64, f64, usize)>> {
        let (_, filters) = crate::join_search::where_clauses_and_filters()?;
        if filters.is_empty() {
            return Some(Vec::new());
        }
        if !search.outer.is_empty() {
            return None;
        }
        let table_of = |alias: &str| search.aliases.iter().position(|a| a == alias);
        let nd_of = |key: &str| -> Option<f64> {
            let t = table_of(alias_of(key))?;
            let (table, filter) = &search.tables[t];
            self.join_side(table, JoinKey::Column(column_of(key)?), filter, 0)
                .map(|s| s.ndistinct)
        };
        let mut out = Vec::new();
        for (i, f) in filters {
            let mut tables = 0u8;
            for a in &f.aliases {
                tables |= 1 << table_of(a)?;
            }
            let sel = crate::join_search::filter_selectivity(&f.kind, &nd_of)?;
            out.push((tables, sel, f.ops as f64, i));
        }
        Some(out)
    }

    /// Each base table's pathlist for a LIMIT's search
    /// (`set_plain_rel_pathlist`, every relation weighing startup costs,
    /// `PgHandler::limit_base_paths`): its sequential scan, its index and
    /// bitmap heap scans through its WHERE, its whole index scans in a join
    /// key's order, and its index and bitmap heap scans parameterized by a
    /// clause's other table (by each other member's, for the class), a
    /// nested loop's inner. `None` when a table's paths are not all
    /// modelled (`limit_base_paths` declines) or, with a parameterized
    /// scan, the search holds an outer join, a join filter or two clauses
    /// between two tables.
    #[allow(clippy::type_complexity)]
    fn build_base_lists(&self, search: &Search, chain: &Chain<'_>) -> Option<BaseLists> {
        let side_of = |t: usize, key: &str| {
            let (table, filter) = &search.tables[t];
            self.join_side(table, JoinKey::Column(column_of(key)?), filter, 0)
        };
        let width_of = |t: usize, key: &str| {
            search.widths[t]
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, w)| *w)
        };
        // Two edges between one pair of tables are several clauses.
        let mut pairs: Vec<(usize, usize)> = search
            .edges
            .iter()
            .map(|e| (e.u.min(e.v), e.u.max(e.v)))
            .collect();
        pairs.sort_unstable();
        let multi = pairs.windows(2).any(|w| w[0] == w[1]);
        let mut out = Vec::new();
        let mut param_keys = Vec::new();
        let mut parameterized = false;
        for (t, (table, filter)) in search.tables.iter().enumerate() {
            // Each outer table's clause with this one: this table's key and
            // the outer table's, which a scan parameterized by it binds.
            let mut binds: Vec<(u32, String, String)> = Vec::new();
            let JoinNode::Leaf { columns, .. } = chain.leaves.get(t)? else {
                return None;
            };
            // The table's ends of the edges, each with the other end's rows
            // (the times a scan parameterized by it repeats) and key; and
            // its member of the class with each other member's, whose
            // derived clause any two join on.
            let mut keys = Vec::new();
            for (k, class) in search.classes.iter().enumerate() {
                let Some((_, key)) = class.iter().find(|(ct, _)| *ct == t) else {
                    continue;
                };
                for (u, u_key) in class.iter().filter(|(ct, _)| *ct != t) {
                    let outer = side_of(*u, u_key)?;
                    let memo_key = MemoKey {
                        ndistinct: outer.ndistinct,
                        default: outer.default_ndistinct,
                        tuples: outer.tuples,
                        rel_rows: outer.rows,
                        width: width_of(*u, u_key)?,
                    };
                    keys.push((
                        class_id(k),
                        column_of(key)?.to_string(),
                        1u32 << u,
                        outer.rows,
                        memo_key,
                    ));
                    binds.push((1u32 << u, key.clone(), u_key.clone()));
                }
            }
            for (i, e) in search.edges.iter().enumerate() {
                let (key, u, u_key) = if e.u == t {
                    (&e.u_key, e.v, &e.v_key)
                } else if e.v == t {
                    (&e.v_key, e.u, &e.u_key)
                } else {
                    continue;
                };
                let outer = side_of(u, u_key)?;
                let memo_key = MemoKey {
                    ndistinct: outer.ndistinct,
                    default: outer.default_ndistinct,
                    tuples: outer.tuples,
                    rel_rows: outer.rows,
                    width: width_of(u, u_key)?,
                };
                // An outer join's clause moves into a scan of its nullable
                // side only: the preserved table's index serves no
                // parameterized scan, only its order.
                let movable = match search.edge_outer.get(i).copied().flatten() {
                    None => true,
                    Some(preserved) => preserved != t && preserved != usize::MAX,
                };
                let bit = if movable { 1u32 << u } else { 0 };
                keys.push((
                    2 + i,
                    column_of(key)?.to_string(),
                    bit,
                    outer.rows,
                    memo_key,
                ));
                binds.push((bit, key.clone(), u_key.clone()));
            }
            // An index on the ORDER BY's column of its own: its scan in that
            // order is of use (`pathkeys_useful_for_ordering`).
            let order_col = match &search.order {
                Some((key, ORDER_CLASS)) if alias_of(key) == search.aliases[t] => {
                    Some((ORDER_CLASS, column_of(key)?.to_string()))
                }
                _ => None,
            };
            let list = self.limit_base_paths(table, filter, columns, &keys, order_col)?;
            parameterized |= list.iter().any(|(p, _)| p.req_outer != 0);
            let cands: Vec<InPath> = list.iter().map(|(p, _)| *p).collect();
            let (kept, ct, cs) = secantus_pgplan::joincost::base_pathlist(&cands, true)?;
            param_keys.push(
                kept.iter()
                    .map(|&i| {
                        let req = list[i].0.req_outer;
                        binds
                            .iter()
                            .find(|(b, ..)| req != 0 && *b == req)
                            .map(|(_, k, uk)| (k.clone(), uk.clone()))
                    })
                    .collect(),
            );
            out.push((kept.iter().map(|&i| list[i].clone()).collect(), ct, cs));
        }
        // With a scan parameterized by an outer row: a FULL join's
        // parameterized join paths, a join filter or a second clause
        // between two tables (a clause the scan's index would not enforce)
        // are not modelled.
        let full = search.edge_outer.contains(&Some(usize::MAX));
        if parameterized && (full || !search.filters.is_empty() || multi) {
            return None;
        }
        Some((out, param_keys))
    }
}
