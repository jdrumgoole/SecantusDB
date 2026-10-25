//! The join order PostgreSQL picks for three tables (RP-028).
//!
//! A join of two tables already runs in the plan PostgreSQL picks for it
//! (`order_general_join_inputs`); a chain of three, `x JOIN y ON .. JOIN z
//! ON ..`, is planned by `standard_join_search` over every order the
//! joins allow, and the order it picks decides the order its rows come
//! out in. This is that search for three stored tables joined by two
//! column equalities, inner or LEFT: the joins of two tables a clause
//! connects (`join_search_one_level`'s level 2), each priced as two
//! tables are and kept by its cheapest path, then the three-table
//! relation built from each of them with the third table
//! (`make_rels_by_clause_joins`), every path in one `add_path` list
//! (`joincost::JoinRel`). An outer join's order follows the identities
//! PostgreSQL's `join_is_legal` allows: a LEFT join commutes with an
//! inner join whose clause reads only its preserved side, and with a LEFT
//! join below it whose nullable side its strict clause reads.
//!
//! The rows are then built in that plan: each join's inputs put in its
//! method's order (`reorder_join_inputs`, `hash_table_order`) and joined
//! by `join_rows_core`, the side the plan reads first outer. Anything else
//! -- a WHERE over more than one table, a constant on a join key, a key
//! two clauses share, a LIMIT, a lock -- is declined, and the join runs as
//! written.

use super::*;
use secantus_pgplan::joincost::{JoinChoice, JoinMethod, JoinRel, JoinSide};
use secantus_pgplan::joins::{JoinKind, JoinNode, SEP};

thread_local! {
    /// Set while a join source whose query's WHERE spans tables
    /// (`JoinRows::where_spans`) is read: its join order is not searched.
    static WHERE_SPANS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// One of those conjuncts is an equality between tables
    /// (`JoinRows::where_spans_equality`).
    static WHERE_SPANS_EQUALITY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The tables each spanning conjunct reads (`JoinRows::where_span_aliases`).
    static WHERE_SPAN_ALIASES: std::cell::RefCell<Vec<Vec<String>>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// The joined-row keys the query reads above the join source being
    /// read (`JoinRows::needed`); `None` outside one.
    static NEEDED: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

/// The keys the query reads above the join source being read, when known.
pub(crate) fn needed_keys() -> Option<Vec<String>> {
    NEEDED.with(|n| n.borrow().clone())
}

/// Whether the join source being read has a WHERE over more than one
/// table (`with_join_where`).
pub(crate) fn where_spans() -> bool {
    WHERE_SPANS.with(|c| c.get())
}

/// Run `f` with the join source `join`'s WHERE recorded for the search.
pub(crate) fn with_join_where<T>(
    join: &secantus_pgplan::joins::JoinRows,
    f: impl FnOnce() -> T,
) -> T {
    let outer = WHERE_SPANS.with(|c| c.replace(join.where_spans));
    let outer_eq = WHERE_SPANS_EQUALITY.with(|c| c.replace(join.where_spans_equality));
    let outer_aliases = WHERE_SPAN_ALIASES.with(|c| c.replace(join.where_span_aliases.clone()));
    let outer_needed = NEEDED.with(|n| n.replace(Some(join.needed.clone())));
    let out = f();
    WHERE_SPANS.with(|c| c.set(outer));
    WHERE_SPANS_EQUALITY.with(|c| c.set(outer_eq));
    WHERE_SPAN_ALIASES.with(|c| *c.borrow_mut() = outer_aliases);
    NEEDED.with(|n| *n.borrow_mut() = outer_needed);
    out
}

/// One table of the three.
pub(crate) struct Base {
    alias: String,
    table: String,
    filter: Document,
}

/// One join of the plan: how, which side is preserved, and what it joins
/// on.
pub(crate) struct Step {
    pub(crate) method: JoinMethod,
    pub(crate) left: bool,
    pub(crate) a_key: String,
    pub(crate) b_key: String,
    /// The estimated rows of `a`, which a hash right join's table is sized
    /// by, and of `b`, which a hash full join with `a` outer sizes its by.
    pub(crate) a_rows: f64,
    pub(crate) b_rows: f64,
    /// The ON (1 or 2) the join evaluates, if one reads only its inputs,
    /// and the ONs it checks its rows against after (an equivalence
    /// class's derived clause stood in for them below).
    pub(crate) on: Option<usize>,
    pub(crate) filters: Vec<usize>,
    /// A hash join whose table is split into batches: its buckets, its
    /// batches and its OUTER key's type, whose hash places an outer row
    /// in its batch. `None` for one batch.
    pub(crate) batches: Option<(u64, u64, String)>,
}

/// The search's answer: which two tables join first (by index into the
/// three, `a` side first), that join, and the join of its output with the
/// third table (`joinrel_first`: the two-table output is `a`).
pub(crate) struct ThreePlan {
    first: (usize, usize),
    first_step: Step,
    joinrel_first: bool,
    second_step: Step,
}

/// A planned join's input: its rows and the joined-row keys they carry.
pub(crate) struct Input {
    pub(crate) rows: Vec<Document>,
    pub(crate) keys: Vec<String>,
}

pub(crate) fn alias_of(key: &str) -> &str {
    key.split(SEP).next().unwrap_or("")
}

pub(crate) fn column_of(key: &str) -> Option<&str> {
    key.split(SEP).nth(1)
}

/// Whether a filter document holds an equality of `field` to a constant
/// anywhere (its bare value, or `$eq`): an equivalence class with a
/// constant, which this search does not model.
pub(crate) fn filter_fixes(filter: &Document, field: &str) -> bool {
    filter.iter().any(|(k, v)| match v {
        Bson::Document(d) if k == field => {
            d.contains_key("$eq") || !d.keys().any(|k| k.starts_with('$'))
        }
        Bson::Document(d) => filter_fixes(d, field),
        Bson::Array(items) => items.iter().any(|i| match i {
            Bson::Document(d) => filter_fixes(d, field),
            _ => false,
        }),
        _ => k == field,
    })
}

/// A join relation's output as the next join's input
/// (`joincost::JoinSide`): its estimated rows and the cost of its cheapest
/// path, the key's statistics its base table's, never unique. `keyed`,
/// when the key is one its merge join's output is ordered by: its cheapest
/// path in that order, and whether that is its cheapest path overall.
pub(crate) fn joinrel_side(
    rows: f64,
    choice: &JoinChoice,
    keyed: Option<&JoinChoice>,
    base: &JoinSide,
) -> JoinSide {
    JoinSide {
        rows,
        scan: (choice.startup, choice.total),
        startup_scan: (choice.startup, choice.total),
        ordered: keyed.map(|k| (k.startup, k.total)),
        scan_ordered: keyed.is_some_and(|k| k == choice),
        unique: false,
        key_ops: 0.0,
        ..*base
    }
}

/// One join of two tables the search weighs (`join_search_one_level`'s
/// level 2): the tables by index, `a` first, their keys, whether it is a
/// LEFT join (`a` preserved), and the ON it evaluates.
struct Pair {
    a: usize,
    b: usize,
    a_key: String,
    b_key: String,
    left: bool,
    /// A FULL join (`left` false).
    full: bool,
    on: Option<usize>,
}

/// A pair's planning: its rows, its cheapest path, and its cheapest path
/// in key order.
struct PairRel {
    rows: f64,
    total: JoinChoice,
    keyed: Option<JoinChoice>,
}

/// One way to build the three-table relation (level 3): a pair with the
/// third table, on the pair's key `pair_key` and the table's `base_key`;
/// `pair_first`: the pair is `a`.
struct Combo {
    pair: usize,
    base: usize,
    pair_key: String,
    base_key: String,
    left: bool,
    /// A FULL join (`left` false).
    full: bool,
    pair_first: bool,
}

impl PgHandler {
    /// The plan PostgreSQL picks for `node`, a chain of three stored tables
    /// on two column equalities; `None` for any other join.
    pub(crate) fn three_table_plan(&self, node: &JoinNode) -> Option<ThreePlan> {
        use secantus_pgplan::joincost::join_rows;
        // A WHERE over two of the three tables that is no equality is a
        // join filter of the top join, where it is first evaluable, and its
        // rows' estimate and cost are the same for every order the search
        // compares; the WHERE runs over the joined rows above, in their
        // order. An equality would join a class and change the clauses.
        if WHERE_SPANS_EQUALITY.with(|c| c.get()) || LOCK_ROW_IDS.with(|c| c.get()) {
            return None;
        }
        let JoinNode::Join {
            kind: k2,
            left: lower,
            right: z_node,
            equi: e2,
            merged: m2,
            expr_keys: x2,
            ec: ec2,
            on_aliases: on2,
            on_other: other2,
            ..
        } = node
        else {
            return None;
        };
        let JoinNode::Join {
            kind: k1,
            left: x_node,
            right: y_node,
            equi: e1,
            merged: m1,
            expr_keys: x1,
            ec: ec1,
            on_aliases: on1,
            on_other: other1,
            ..
        } = lower.as_ref()
        else {
            return None;
        };
        // A FULL join with a join filter is a hash full join only, which
        // the search does not weigh alone.
        if (*k1 == JoinKind::Full && *other1) || (*k2 == JoinKind::Full && *other2) {
            return None;
        }
        let modelled =
            |k: &JoinKind| matches!(k, JoinKind::Inner | JoinKind::Left | JoinKind::Full);
        if !modelled(k1)
            || !modelled(k2)
            || !m1.is_empty()
            || !m2.is_empty()
            || !x1.is_empty()
            || !x2.is_empty()
            || ec1.is_some()
            || ec2.is_some()
        {
            return None;
        }
        let ([(xk, yk)], [(wk, zk)]) = (e1.as_slice(), e2.as_slice()) else {
            return None;
        };
        let base = |n: &'_ JoinNode| -> Option<(String, String, Document)> {
            let (table, filter) = plain_leaf_table(n)?;
            let JoinNode::Leaf { columns, .. } = n else {
                return None;
            };
            let alias = alias_of(&columns.first()?.0).to_string();
            Some((alias, table, filter))
        };
        let mut bases = Vec::new();
        for n in [x_node.as_ref(), y_node.as_ref(), z_node.as_ref()] {
            let (alias, table, filter) = base(n)?;
            bases.push(Base {
                alias,
                table,
                filter,
            });
        }
        let [x, y, z] = [&bases[0], &bases[1], &bases[2]];
        // The two clauses: x-y, and w-z with w one of x and y; each ON
        // reads only its two tables.
        if alias_of(xk) != x.alias || alias_of(yk) != y.alias || alias_of(zk) != z.alias {
            return None;
        }
        let w = if alias_of(wk) == x.alias {
            0
        } else if alias_of(wk) == y.alias {
            1
        } else {
            return None;
        };
        let w_alias = &bases[w].alias;
        // A spanning conjunct over a pair the search may join first is that
        // join's filter, below the top: not modelled. One over x and z (or
        // y and z, past the pair) is the top join's.
        let below_top = |set: &[String]| {
            let within = |a: &str, b: &str| set.iter().all(|t| t == a || t == b);
            !set.iter().all(|t| bases.iter().any(|b| b.alias == *t))
                || within(&x.alias, &y.alias)
                || within(w_alias, &z.alias)
        };
        if WHERE_SPAN_ALIASES.with(|c| c.borrow().iter().any(|s| below_top(s))) {
            return None;
        }
        if on1.iter().any(|a| *a != x.alias && *a != y.alias)
            || on2.iter().any(|a| a != w_alias && *a != z.alias)
        {
            return None;
        }
        for b in &bases {
            if !self.join_small_enough(&b.table) {
                return None;
            }
            // A constant on a join key joins an equivalence class.
            let def = self.lookup(&b.table)?;
            for key in [xk, yk, wk, zk] {
                if alias_of(key) == b.alias {
                    let field = def.field_of(column_of(key)?)?;
                    if filter_fixes(&b.filter, &field) {
                        return None;
                    }
                }
            }
        }
        // `reduce_outer_joins`: an inner join whose clause reads the
        // nullable side of the LEFT join below it makes that join inner.
        let k2 = *k2;
        let k1 = if k2 == JoinKind::Inner && *k1 == JoinKind::Left && w == 1 {
            JoinKind::Inner
        } else {
            *k1
        };
        let left1 = k1 == JoinKind::Left;
        let left2 = k2 == JoinKind::Left;
        let (full1, full2) = (k1 == JoinKind::Full, k2 == JoinKind::Full);
        let shared = wk == xk || wk == yk;
        let side = |i: usize, key: &str| {
            let b = &bases[i];
            self.join_side(&b.table, JoinKey::Column(column_of(key)?), &b.filter, 0)
        };
        let leaves = [x_node.as_ref(), y_node.as_ref(), z_node.as_ref()];
        // An inner chain whose clauses share a key: one equivalence class
        // over three tables, which joins any two of them on its derived
        // clause (`generate_join_implied_equalities`: its first member on
        // each side, in the order the clauses gave them). Its derived
        // clauses are matched by their keys' hash alone, so only for
        // integer keys, where that is equality; the ONs still decide every
        // row at the top.
        let ec = shared && !left1 && !left2 && !full1 && !full2;
        let (pairs, combos) = if ec {
            let types: Vec<Option<String>> = [(0, xk), (1, yk), (2, zk)]
                .iter()
                .map(|(i, k)| leaf_key_type(leaves[*i], k))
                .collect();
            let int = |t: &Option<String>| matches!(t.as_deref(), Some("int2" | "int4" | "int8"));
            if !types.iter().all(int) || types.windows(2).any(|t| t[0] != t[1]) {
                return None;
            }
            let on_of = |a: usize, b: usize| {
                let pair = [&bases[a].alias, &bases[b].alias];
                if on1.iter().all(|x| pair.contains(&x)) {
                    Some(1)
                } else if on2.iter().all(|x| pair.contains(&x)) {
                    Some(2)
                } else {
                    None
                }
            };
            let pair = |a: usize, b: usize, ak: &String, bk: &String| Pair {
                a,
                b,
                a_key: ak.clone(),
                b_key: bk.clone(),
                left: false,
                full: false,
                on: on_of(a, b),
            };
            let pairs = vec![pair(0, 1, xk, yk), pair(0, 2, xk, zk), pair(1, 2, yk, zk)];
            let combo = |pair: usize, base: usize, pk: &String, bk: &String| Combo {
                pair,
                base,
                pair_key: pk.clone(),
                base_key: bk.clone(),
                left: false,
                full: false,
                pair_first: true,
            };
            let combos = vec![
                combo(0, 2, xk, zk),
                combo(1, 1, xk, yk),
                combo(2, 0, yk, xk),
            ];
            (pairs, combos)
        } else {
            // The chain: x-y, then w-z; each ON its own join's.
            let pairs = vec![
                Pair {
                    a: 0,
                    b: 1,
                    a_key: xk.clone(),
                    b_key: yk.clone(),
                    left: left1,
                    full: full1,
                    on: Some(1),
                },
                Pair {
                    a: w,
                    b: 2,
                    a_key: wk.clone(),
                    b_key: zk.clone(),
                    left: left2,
                    full: full2,
                    on: Some(2),
                },
            ];
            // Level 3: x-y with z, then w-z with the other table v. A LEFT
            // join's preserved side is `a` (`make_join_rel` swaps it first):
            // with x-y LEFT, x is, so v = x is `a`, else the w-z rel (holding
            // x) is.
            let v = 1 - w;
            let (v_key, wz_key) = if v == 0 { (xk, yk) } else { (yk, xk) };
            let mut combos = vec![
                Combo {
                    pair: 0,
                    base: 2,
                    pair_key: wk.clone(),
                    base_key: zk.clone(),
                    left: left2,
                    full: full2,
                    pair_first: true,
                },
                Combo {
                    pair: 1,
                    base: v,
                    pair_key: wz_key.clone(),
                    base_key: v_key.clone(),
                    left: left1,
                    full: full1,
                    pair_first: !(left1 && v == 0),
                },
            ];
            // A FULL join's minimum sides are its whole syntactic ones
            // (`make_outerjoininfo`), so with one in the chain x-y is
            // joined first, then z (`join_is_legal`).
            if full1 || full2 {
                combos.truncate(1);
            }
            (pairs, combos)
        };
        // Level 2, in `join_search_one_level`'s order.
        let mut rels = Vec::new();
        for p in &pairs {
            let (a, b) = (side(p.a, &p.a_key)?, side(p.b, &p.b_key)?);
            let mut rel = JoinRel::new(false);
            let rows = if p.full {
                rel.add_full_paths(0, &a, &b, true);
                secantus_pgplan::joincost::full_join_rows(&a, &b)
            } else {
                rel.add_paths(0, &a, &b, p.left, true);
                join_rows(&a, &b, p.left)
            };
            rels.push(PairRel {
                rows,
                total: rel.cheapest(None, rows)?,
                keyed: rel.cheapest_keyed(),
            });
        }
        // Level 3: every combo's paths in one relation.
        let mut top = JoinRel::new(false);
        let mut rows3 = None;
        let mut sides = Vec::new();
        for (i, c) in combos.iter().enumerate() {
            let p = &pairs[c.pair];
            let rel = &rels[c.pair];
            // The pair's merge output is in its key's order: either key for
            // an inner join, the preserved side's for a LEFT one.
            let in_order = c.pair_key == p.a_key || (!p.left && c.pair_key == p.b_key);
            let key_rel = if alias_of(&c.pair_key) == bases[p.a].alias {
                p.a
            } else {
                p.b
            };
            let pair_side = joinrel_side(
                rel.rows,
                &rel.total,
                rel.keyed.as_ref().filter(|_| in_order),
                &side(key_rel, &c.pair_key)?,
            );
            let base_side = side(c.base, &c.base_key)?;
            let (a, b) = if c.pair_first {
                (pair_side, base_side)
            } else {
                (base_side, pair_side)
            };
            if c.full {
                top.add_full_paths(i, &a, &b, true);
                rows3.get_or_insert(secantus_pgplan::joincost::full_join_rows(&a, &b));
            } else {
                top.add_paths(i, &a, &b, c.left, true);
                rows3.get_or_insert(join_rows(&a, &b, c.left));
            }
            sides.push((a, b));
        }
        let choice = top.cheapest(None, rows3?)?;
        let c = &combos[choice.combo];
        let p = &pairs[c.pair];
        let rel = &rels[c.pair];
        // The pair runs in its key order when the top merge join reads it
        // unsorted.
        let pair_unsorted = match choice.method {
            JoinMethod::MergeOuterA {
                a_by_index,
                b_by_index,
            }
            | JoinMethod::MergeOuterB {
                a_by_index,
                b_by_index,
            } => {
                if c.pair_first {
                    a_by_index
                } else {
                    b_by_index
                }
            }
            _ => false,
        };
        let first = if pair_unsorted {
            rel.keyed.as_ref()?
        } else {
            &rel.total
        };
        let (a, _) = &sides[choice.combo];
        let applied: Vec<usize> = p.on.into_iter().collect();
        let mut rest: Vec<usize> = [1, 2]
            .into_iter()
            .filter(|o| !applied.contains(o))
            .collect();
        let top_on = (!rest.is_empty()).then(|| rest.remove(0));
        let (a_key, b_key) = if c.pair_first {
            (c.pair_key.clone(), c.base_key.clone())
        } else {
            (c.base_key.clone(), c.pair_key.clone())
        };
        let plan = ThreePlan {
            first: (p.a, p.b),
            first_step: Step {
                method: first.method,
                left: p.left,
                a_key: p.a_key.clone(),
                b_key: p.b_key.clone(),
                a_rows: side(p.a, &p.a_key)?.rows,
                b_rows: side(p.b, &p.b_key)?.rows,
                on: p.on,
                filters: Vec::new(),
                batches: None,
            },
            joinrel_first: c.pair_first,
            second_step: Step {
                method: choice.method,
                left: c.left,
                a_key,
                b_key,
                a_rows: a.rows,
                b_rows: sides[choice.combo].1.rows,
                on: top_on,
                filters: rest,
                batches: None,
            },
        };
        Some(plan)
    }

    /// `node`'s rows in the plan `three_table_plan` picks, or `None` when it
    /// picks none (or the plan's order cannot be reproduced).
    pub(crate) fn three_table_join(
        &self,
        node: &JoinNode,
        limit: Option<usize>,
    ) -> PgWireResult<Option<Vec<Document>>> {
        if limit.is_some() {
            return Ok(None);
        }
        let Some(plan) = self.three_table_plan(node) else {
            return Ok(None);
        };
        let JoinNode::Join {
            left: lower,
            right: z_node,
            on: on2,
            ..
        } = node
        else {
            return Ok(None);
        };
        let JoinNode::Join {
            left: x_node,
            right: y_node,
            on: on1,
            ..
        } = lower.as_ref()
        else {
            return Ok(None);
        };
        let nodes = [x_node.as_ref(), y_node.as_ref(), z_node.as_ref()];
        let mut inputs: Vec<Option<Input>> = Vec::new();
        for n in nodes {
            inputs.push(Some(Input {
                rows: self.join_rows(n)?,
                keys: secantus_pgplan::joins::node_keys(n),
            }));
        }
        let on = |i: Option<usize>| match i {
            Some(1) => on1.as_ref(),
            Some(_) => on2.as_ref(),
            None => None,
        };
        let (fa, fb) = plan.first;
        let a = inputs[fa].take().expect("each table read once");
        let b = inputs[fb].take().expect("each table read once");
        let a_type = leaf_key_type(nodes[fa], &plan.first_step.a_key);
        let b_type = leaf_key_type(nodes[fb], &plan.first_step.b_key);
        let Some(joined) = self.run_step(
            &plan.first_step,
            a,
            b,
            a_type,
            b_type,
            on(plan.first_step.on),
        )?
        else {
            return Ok(None);
        };
        let third = inputs.into_iter().flatten().next().expect("one table left");
        // A key's type is its base table's.
        let type_of = |key: &str| nodes.iter().find_map(|n| leaf_key_type(n, key));
        let (a_type, b_type) = (
            type_of(&plan.second_step.a_key),
            type_of(&plan.second_step.b_key),
        );
        let (a, b) = if plan.joinrel_first {
            (joined, third)
        } else {
            (third, joined)
        };
        let Some(joined) = self.run_step(
            &plan.second_step,
            a,
            b,
            a_type,
            b_type,
            on(plan.second_step.on),
        )?
        else {
            return Ok(None);
        };
        // The ONs a derived clause stood in for decide every row.
        let mut rows = joined.rows;
        for f in &plan.second_step.filters {
            if let Some(expr) = on(Some(*f)) {
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

    /// One join of a planned order: `a` and `b` put in the order `step`'s
    /// method reads them, then joined with its outer side first. `None`
    /// when a hash right join's table order is not reproducible.
    pub(crate) fn run_step(
        &self,
        step: &Step,
        mut a: Input,
        mut b: Input,
        a_type: Option<String>,
        b_type: Option<String>,
        on: Option<&secantus_pgplan::ColumnExpr>,
    ) -> PgWireResult<Option<Input>> {
        let a_first = (step.a_key.clone(), step.b_key.clone());
        let b_first = (step.b_key.clone(), step.a_key.clone());
        let (outer, inner, kind, (outer_key, inner_key)) = match step.method {
            // A FULL join: a hash full join's hashed side in its table's
            // order, which its probes' matches and the rows nothing matched
            // both keep; a merge full join's sides in key order.
            JoinMethod::HashFullA => {
                let ordered = b_type
                    .is_some_and(|ty| hash_table_order(&mut b.rows, &step.b_key, &ty, step.b_rows));
                if !ordered {
                    return Ok(None);
                }
                (a, b, JoinKind::Full, a_first)
            }
            JoinMethod::HashFullB => {
                let ordered = a_type
                    .is_some_and(|ty| hash_table_order(&mut a.rows, &step.a_key, &ty, step.a_rows));
                if !ordered {
                    return Ok(None);
                }
                (b, a, JoinKind::Full, b_first)
            }
            JoinMethod::MergeFullA { .. } => {
                reorder_join_inputs(
                    step.method,
                    &mut a.rows,
                    &step.a_key,
                    &mut b.rows,
                    &step.b_key,
                );
                (a, b, JoinKind::Full, a_first)
            }
            JoinMethod::MergeFullB { .. } => {
                reorder_join_inputs(
                    step.method,
                    &mut a.rows,
                    &step.a_key,
                    &mut b.rows,
                    &step.b_key,
                );
                (b, a, JoinKind::Full, b_first)
            }
            JoinMethod::MergeRightB {
                a_by_index,
                b_by_index,
            } => {
                reorder_join_inputs(
                    JoinMethod::MergeOuterB {
                        a_by_index,
                        b_by_index,
                    },
                    &mut a.rows,
                    &step.a_key,
                    &mut b.rows,
                    &step.b_key,
                );
                (b, a, JoinKind::Right, b_first)
            }
            JoinMethod::HashRightB => {
                let ordered = a_type
                    .is_some_and(|ty| hash_table_order(&mut a.rows, &step.a_key, &ty, step.a_rows));
                if !ordered {
                    return Ok(None);
                }
                (b, a, JoinKind::Right, b_first)
            }
            method => {
                reorder_join_inputs(method, &mut a.rows, &step.a_key, &mut b.rows, &step.b_key);
                let kind = if step.left {
                    JoinKind::Left
                } else {
                    JoinKind::Inner
                };
                if !joincost_outer_is_b(method) {
                    (a, b, kind, a_first)
                } else if step.left {
                    return Ok(None);
                } else {
                    (b, a, kind, b_first)
                }
            }
        };
        // A hash table split into batches joins each outer row in its key's
        // batch: batch 0's as the outer side is read, every later batch's
        // after it, in the order they were read (`ExecHashJoinNewBatch`). A
        // NULL key matches nothing and is not saved (a LEFT join's row comes
        // out where it is read). A right or full join's unmatched rows would
        // come out batch by batch too, which is not modelled.
        let mut outer = outer;
        if let Some((nbuckets, nbatch, ty)) = &step.batches {
            match step.method {
                JoinMethod::HashOuterA | JoinMethod::HashOuterB => {
                    let mut tagged = Vec::with_capacity(outer.rows.len());
                    for d in std::mem::take(&mut outer.rows) {
                        let batch = match d.get(&outer_key) {
                            None | Some(Bson::Null) => 0,
                            Some(v) => {
                                let Some(h) = secantus_pgplan::joincost::hash_value(v, ty) else {
                                    return Ok(None);
                                };
                                secantus_pgplan::joincost::hash_batch(h, *nbuckets, *nbatch)
                            }
                        };
                        tagged.push((batch, d));
                    }
                    tagged.sort_by_key(|(b, _)| *b);
                    outer.rows = tagged.into_iter().map(|(_, d)| d).collect();
                }
                JoinMethod::HashRightB | JoinMethod::HashFullA | JoinMethod::HashFullB => {
                    return Ok(None)
                }
                _ => {}
            }
        }
        let merge_full = matches!(
            step.method,
            JoinMethod::MergeFullA { .. } | JoinMethod::MergeFullB { .. }
        );
        let inner_max = if merge_full {
            inner
                .rows
                .iter()
                .filter_map(|r| r.get(&inner_key).filter(|v| **v != Bson::Null))
                .max_by(|p, q| secantus_pgplan::compare_values(p, q).unwrap_or(Ordering::Equal))
                .cloned()
        } else {
            None
        };
        let (fill_outer, fill_inner) = (outer_key.clone(), inner_key.clone());
        let mut outer_rows = Some(outer.rows);
        let mut out = Vec::new();
        // Where the last batch begins: a FULL join's last is the inner rows
        // nothing matched.
        let mut last = 0;
        self.join_rows_core(
            kind,
            on,
            &[(outer_key, inner_key)],
            &[],
            &outer.keys,
            &inner.keys,
            &mut |sink| {
                if let Some(rows) = outer_rows.take() {
                    sink(rows)?;
                }
                Ok(())
            },
            inner.rows,
            &mut |docs| {
                last = out.len();
                out.extend(docs);
                Ok(true)
            },
        )?;
        if merge_full {
            let tail = out.split_off(last);
            out = crate::merge_full_place(out, tail, &fill_outer, &fill_inner, inner_max.as_ref());
        }
        if matches!(step.method, JoinMethod::MergeRightB { .. }) {
            out = merge_right_fill(out, &step.b_key, &step.a_key);
        }
        let keys = outer.keys.into_iter().chain(inner.keys).collect();
        Ok(Some(Input { rows: out, keys }))
    }
}

/// A merge right join's rows: `rows` as the RIGHT join built them (the
/// matched rows in key order, then the preserved rows nothing matched, in
/// key order too), with each unmatched row moved to its key's place, after
/// the matched rows of an equal key -- where the merge passes it
/// (`ExecMergeJoin` fills an inner tuple once the outer has moved past
/// it). A row is unmatched when its outer key is NULL, which a matched
/// row's never is.
fn merge_right_fill(rows: Vec<Document>, outer_key: &str, inner_key: &str) -> Vec<Document> {
    let is_null = |d: &Document, k: &str| matches!(d.get(k), None | Some(Bson::Null));
    let (matched, unmatched): (Vec<Document>, Vec<Document>) =
        rows.into_iter().partition(|d| !is_null(d, outer_key));
    let key_cmp = |x: &Document, y: &Document| match (
        x.get(inner_key).filter(|v| **v != Bson::Null),
        y.get(inner_key).filter(|v| **v != Bson::Null),
    ) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(p), Some(q)) => secantus_pgplan::compare_values(p, q).unwrap_or(Ordering::Equal),
    };
    let mut out = Vec::with_capacity(matched.len() + unmatched.len());
    let mut m = matched.into_iter().peekable();
    for u in unmatched {
        while m
            .peek()
            .is_some_and(|d| key_cmp(d, &u) != Ordering::Greater)
        {
            out.push(m.next().expect("peeked"));
        }
        out.push(u);
    }
    out.extend(m);
    out
}
