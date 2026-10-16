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
}

/// Run `f` with the join source `join`'s WHERE recorded for the search.
pub(crate) fn with_join_where<T>(
    join: &secantus_pgplan::joins::JoinRows,
    f: impl FnOnce() -> T,
) -> T {
    let outer = WHERE_SPANS.with(|c| c.replace(join.where_spans));
    let out = f();
    WHERE_SPANS.with(|c| c.set(outer));
    out
}

/// One table of the three.
struct Base {
    alias: String,
    table: String,
    filter: Document,
}

/// One join of the plan: how, which side is preserved, and what it joins
/// on.
struct Step {
    method: JoinMethod,
    left: bool,
    a_key: String,
    b_key: String,
    /// The estimated rows of `a`, which a hash right join's table is sized
    /// by.
    a_rows: f64,
    on: usize,
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
struct Input {
    rows: Vec<Document>,
    keys: Vec<String>,
}

fn alias_of(key: &str) -> &str {
    key.split(SEP).next().unwrap_or("")
}

fn column_of(key: &str) -> Option<&str> {
    key.split(SEP).nth(1)
}

/// Whether a filter document holds an equality of `field` to a constant
/// anywhere (its bare value, or `$eq`): an equivalence class with a
/// constant, which this search does not model.
fn filter_fixes(filter: &Document, field: &str) -> bool {
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
/// path, the key's statistics its base table's, never unique, in no
/// useful order.
fn joinrel_side(rows: f64, choice: &JoinChoice, base: &JoinSide) -> JoinSide {
    JoinSide {
        rows,
        scan: (choice.startup, choice.total),
        startup_scan: (choice.startup, choice.total),
        ordered: None,
        unique: false,
        key_ops: 0.0,
        ..*base
    }
}

impl PgHandler {
    /// The plan PostgreSQL picks for `node`, a chain of three stored tables
    /// on two column equalities; `None` for any other join.
    pub(crate) fn three_table_plan(&self, node: &JoinNode) -> Option<ThreePlan> {
        use secantus_pgplan::joincost::join_rows;
        if WHERE_SPANS.with(|c| c.get()) || LOCK_ROW_IDS.with(|c| c.get()) {
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
            ..
        } = lower.as_ref()
        else {
            return None;
        };
        let modelled = |k: &JoinKind| matches!(k, JoinKind::Inner | JoinKind::Left);
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
        // reads only its two tables; no column in both (an equivalence
        // class of three, which PostgreSQL would join every way round).
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
        if on1.iter().any(|a| *a != x.alias && *a != y.alias)
            || on2.iter().any(|a| a != w_alias && *a != z.alias)
            || [xk, yk].iter().any(|k| *k == wk || *k == zk)
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
        let side = |b: &Base, key: &str| {
            self.join_side(&b.table, JoinKey::Column(column_of(key)?), &b.filter, 0)
        };
        let (sx, sy) = (side(x, xk)?, side(y, yk)?);
        let (sw, sz) = (side(&bases[w], wk)?, side(z, zk)?);
        // Level 2, in `join_search_one_level`'s order: x-y, then w-z.
        let level2 = |a: &JoinSide, b: &JoinSide, left: bool| -> Option<(f64, JoinChoice)> {
            let mut rel = JoinRel::new(false);
            rel.add_paths(0, a, b, left, true);
            let rows = join_rows(a, b, left);
            Some((rows, rel.cheapest(None, rows)?))
        };
        let (xy_rows, xy) = level2(&sx, &sy, left1)?;
        let (wz_rows, wz) = level2(&sw, &sz, left2)?;
        // Level 3: x-y with z, then w-z with the other table v. A LEFT
        // join's preserved side is `a` (`make_join_rel` swaps it first).
        let v = 1 - w;
        let mut top = JoinRel::new(false);
        let a0 = joinrel_side(xy_rows, &xy, &sw);
        top.add_paths(0, &a0, &sz, left2, true);
        let rows = join_rows(&a0, &sz, left2);
        // The v join is the x-y join: v's key and the w-z rel's key into it.
        let (v_key, wz_key) = if v == 0 { (xk, yk) } else { (yk, xk) };
        let (v_side, wz_base) = if v == 0 { (&sx, &sy) } else { (&sy, &sx) };
        let wz_side = joinrel_side(wz_rows, &wz, wz_base);
        // With x-y LEFT, x is preserved: v = x is `a`, else the w-z rel
        // (holding x) is.
        let v_first = left1 && v == 0;
        if v_first {
            top.add_paths(1, v_side, &wz_side, true, true);
        } else {
            top.add_paths(1, &wz_side, v_side, left1, true);
        }
        let choice = top.cheapest(None, rows)?;
        let plan = if choice.combo == 0 {
            ThreePlan {
                first: (0, 1),
                first_step: Step {
                    method: xy.method,
                    left: left1,
                    a_key: xk.clone(),
                    b_key: yk.clone(),
                    a_rows: sx.rows,
                    on: 1,
                },
                joinrel_first: true,
                second_step: Step {
                    method: choice.method,
                    left: left2,
                    a_key: wk.clone(),
                    b_key: zk.clone(),
                    a_rows: xy_rows,
                    on: 2,
                },
            }
        } else {
            let (a_key, b_key, a_rows) = if v_first {
                (v_key.clone(), wz_key.clone(), v_side.rows)
            } else {
                (wz_key.clone(), v_key.clone(), wz_rows)
            };
            ThreePlan {
                first: (w, 2),
                first_step: Step {
                    method: wz.method,
                    left: left2,
                    a_key: wk.clone(),
                    b_key: zk.clone(),
                    a_rows: sw.rows,
                    on: 2,
                },
                joinrel_first: !v_first,
                second_step: Step {
                    method: choice.method,
                    left: left1,
                    a_key,
                    b_key,
                    a_rows,
                    on: 1,
                },
            }
        };
        if [plan.first_step.method, plan.second_step.method].contains(&JoinMethod::Unmodelled) {
            return None;
        }
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
        let on = |i: usize| if i == 1 { on1.as_ref() } else { on2.as_ref() };
        let (fa, fb) = plan.first;
        let a = inputs[fa].take().expect("each table read once");
        let b = inputs[fb].take().expect("each table read once");
        let a_type = leaf_key_type(nodes[fa], &plan.first_step.a_key);
        let Some(joined) = self.run_step(&plan.first_step, a, b, a_type, on(plan.first_step.on))?
        else {
            return Ok(None);
        };
        let third = inputs.into_iter().flatten().next().expect("one table left");
        let third_node = nodes
            .iter()
            .enumerate()
            .find(|(i, _)| *i != fa && *i != fb)
            .map(|(_, n)| *n)
            .expect("one table left");
        let (a, b, a_type) = if plan.joinrel_first {
            // The key's type is its base table's.
            let ty = nodes
                .iter()
                .find_map(|n| leaf_key_type(n, &plan.second_step.a_key));
            (joined, third, ty)
        } else {
            (
                third,
                joined,
                leaf_key_type(third_node, &plan.second_step.a_key),
            )
        };
        Ok(self
            .run_step(&plan.second_step, a, b, a_type, on(plan.second_step.on))?
            .map(|i| i.rows))
    }

    /// One join of a planned order: `a` and `b` put in the order `step`'s
    /// method reads them, then joined with its outer side first. `None`
    /// when a hash right join's table order is not reproducible.
    fn run_step(
        &self,
        step: &Step,
        mut a: Input,
        mut b: Input,
        a_type: Option<String>,
        on: Option<&secantus_pgplan::ColumnExpr>,
    ) -> PgWireResult<Option<Input>> {
        let a_first = (step.a_key.clone(), step.b_key.clone());
        let b_first = (step.b_key.clone(), step.a_key.clone());
        let (outer, inner, kind, (outer_key, inner_key)) = match step.method {
            JoinMethod::Unmodelled => return Ok(None),
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
        let mut outer_rows = Some(outer.rows);
        let mut out = Vec::new();
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
                out.extend(docs);
                Ok(true)
            },
        )?;
        let keys = outer.keys.into_iter().chain(inner.keys).collect();
        Ok(Some(Input { rows: out, keys }))
    }
}
