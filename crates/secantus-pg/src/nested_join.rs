//! EXPLAIN of an inner join nested on the nullable side of a LEFT join
//! (RP-978): `s LEFT JOIN (e JOIN k ON ..) ON ..`.
//!
//! The LEFT join's nullable side is the whole inner join
//! (`make_outerjoininfo`), so PostgreSQL's join search builds that pair
//! first, as the two-table join it is, and then the three-table relation
//! from it and the preserved table -- each method `add_paths_to_joinrel`
//! weighs over the pair's cheapest path: a nested loop with the pair inner
//! (materialised or not), a hash join hashing either side (the pair
//! outer is a `Hash Right Join`), a merge join of either order. Each is
//! priced as two tables are, the pair standing as one side
//! ([`crate::scan_estimate::JoinedRel`]): its path's cost, rows and width,
//! its key's statistics its base table's.
//!
//! Modelled: no WHERE, the LEFT join's ON one column equality between the
//! preserved table and a table of the pair, or nothing (`ON true`); the
//! pair a join of two plain tables priced as two tables are. Anything else
//! keeps the tree as written, unpriced.

use super::*;
use crate::explain::PlanNode;
use crate::join_search::alias_of;
use crate::scan_estimate::JoinedRel;
use secantus_pgplan::joins::{JoinKind, JoinNode};

/// The alias a plain table leaf's joined keys carry.
fn leaf_alias(n: &JoinNode) -> Option<&str> {
    match n {
        JoinNode::Leaf { columns, .. } => Some(alias_of(&columns.first()?.0)),
        _ => None,
    }
}

/// The statement of the inner join `sel`'s LEFT join null-extends, where
/// `sel` is one (see the module): what its scans' filters and counts are
/// read from under ANALYZE.
pub(crate) fn nested_pair_sel(sel: &secantus_pgplan::Select) -> Option<secantus_pgplan::Select> {
    let Statement::JoinRows(j) = sel.sub.as_ref()?.plan.as_ref() else {
        return None;
    };
    let JoinNode::Join {
        kind: JoinKind::Left,
        right,
        ..
    } = &j.tree
    else {
        return None;
    };
    if !matches!(
        right.as_ref(),
        JoinNode::Join {
            kind: JoinKind::Inner,
            ..
        }
    ) {
        return None;
    }
    with_tree(sel, right.as_ref().clone(), &[])
}

/// `sel` reading `tree` as its join source in place of its own.
fn with_tree(
    sel: &secantus_pgplan::Select,
    tree: JoinNode,
    needed: &[String],
) -> Option<secantus_pgplan::Select> {
    let sub = sel.sub.as_ref()?;
    let Statement::JoinRows(j) = sub.plan.as_ref() else {
        return None;
    };
    let mut j = j.clone();
    j.tree = tree;
    for k in needed {
        if !j.needed.contains(k) {
            j.needed.push(k.clone());
        }
    }
    let mut sub = sub.as_ref().clone();
    sub.plan = Box::new(Statement::JoinRows(j));
    let mut out = sel.clone();
    out.sub = Some(Box::new(sub));
    Some(out)
}

impl PgHandler {
    /// Price `tree`, the plan drawn for `sel`, where `sel` is a LEFT join
    /// of a table to an inner join of two (see the module): the inner
    /// join's subtree as two tables are priced, then the join above it as
    /// the cheapest path over it, drawn in that path's order and method.
    /// `false`, the tree untouched, for any other shape.
    pub(crate) fn price_nested_outer_join(
        &self,
        sel: &secantus_pgplan::Select,
        tree: &mut PlanNode,
        join_conds: &[String],
    ) -> bool {
        self.try_price_nested_outer_join(sel, tree, join_conds)
            .is_some()
    }

    fn try_price_nested_outer_join(
        &self,
        sel: &secantus_pgplan::Select,
        tree: &mut PlanNode,
        join_conds: &[String],
    ) -> Option<()> {
        let sub = sel.sub.as_ref()?;
        let Statement::JoinRows(j) = sub.plan.as_ref() else {
            return None;
        };
        if sel.series.is_some()
            || sel.join.is_some()
            || !sel.windows.is_empty()
            || !sel.order.is_empty()
            || sel.limit.is_some()
            || sel.offset != 0
            || !matches!(sel.distinct, secantus_pgplan::Distinct::None)
            || sel.with_ties
            || sel.casts.iter().any(Option::is_some)
            || !sel.filter.is_empty()
            || sel.residual.is_some()
            || !j.where_filters.is_empty()
            || j.where_spans_equality
            || !j.where_span_aliases.is_empty()
            || !j.where_modelled
        {
            return None;
        }
        let JoinNode::Join {
            kind: JoinKind::Left,
            left: preserved,
            right: pair,
            on,
            on_other: false,
            on_filters,
            equi,
            merged,
            expr_keys,
            ec: None,
            ..
        } = &j.tree
        else {
            return None;
        };
        if !on_filters.is_empty() || !merged.is_empty() || !expr_keys.is_empty() || equi.len() > 1 {
            return None;
        }
        // With no equality the ON is nothing, or `true`.
        if equi.is_empty()
            && !matches!(
                on,
                None | Some(secantus_pgplan::ColumnExpr::Const {
                    value: Bson::Boolean(true),
                    ..
                })
            )
        {
            return None;
        }
        let preserved_alias = leaf_alias(preserved)?;
        let JoinNode::Join {
            kind: JoinKind::Inner,
            left: x,
            right: y,
            merged: pair_merged,
            expr_keys: pair_exprs,
            ec: None,
            ..
        } = pair.as_ref()
        else {
            return None;
        };
        if !pair_merged.is_empty() || !pair_exprs.is_empty() {
            return None;
        }
        let pair_aliases = [leaf_alias(x)?, leaf_alias(y)?];
        // The pair's table the LEFT join's equality reads, and its key.
        let (key_alias, pair_keys): (&str, Vec<String>) = match equi.as_slice() {
            [(l, r)] => {
                if alias_of(l) != preserved_alias || !pair_aliases.contains(&alias_of(r)) {
                    return None;
                }
                (alias_of(r), vec![r.clone()])
            }
            _ => (pair_aliases[0], Vec::new()),
        };
        let key_leaf = if key_alias == pair_aliases[0] { x } else { y };

        // The pair, priced as the two-table join it is; its rows carry the
        // LEFT join's key too (`build_joinrel_tlist`).
        let pair_sel = with_tree(sel, pair.as_ref().clone(), &pair_keys)?;
        let hashed = equi.len() == 1;
        // Where the drawn tree holds the pair: under the Hash of a hash
        // join, or the inner side of a nested loop.
        let top_ok = if hashed {
            tree.name == "Hash Left Join"
                && tree.children.len() == 2
                && tree.children[1].name == "Hash"
                && tree.children[1].children.len() == 1
        } else {
            tree.name == "Nested Loop Left Join" && tree.children.len() == 2
        };
        if !top_ok
            || !tree.children[0].children.is_empty()
            || !tree.children[0].name.starts_with("Seq Scan on ")
        {
            return None;
        }
        // Priced on a copy, which replaces the tree only when every join
        // in it is priced: a pair priced under an unpriced join would print
        // half a plan.
        let mut priced = tree.clone();
        let slot = if hashed {
            &mut priced.children[1].children[0]
        } else {
            &mut priced.children[1]
        };
        self.price_pair(&pair_sel, slot, join_conds)?;
        let est = slot.estimate?;
        // What the pair passes above the LEFT join: the query's columns
        // of it, without the key only the LEFT join's clause reads -- the
        // width of the pair priced with nothing more needed of it.
        let above = if pair_keys.is_empty() {
            est.3
        } else {
            let bare_sel = with_tree(sel, pair.as_ref().clone(), &[])?;
            let mut bare = if hashed {
                tree.children[1].children[0].clone()
            } else {
                tree.children[1].clone()
            };
            self.price_pair(&bare_sel, &mut bare, join_conds)?;
            bare.estimate?.3
        };
        let rel = JoinedRel {
            alias: key_alias.to_string(),
            est,
            above,
        };

        // The LEFT join, the pair standing as its key's table.
        let JoinNode::Join {
            kind,
            left,
            on,
            on_aliases,
            on_other,
            on_filters,
            equi,
            equi_reversed,
            merged,
            expr_keys,
            ec,
            left_keys,
            right_keys,
            ..
        } = &j.tree
        else {
            return None;
        };
        let top = JoinNode::Join {
            kind: *kind,
            left: left.clone(),
            right: key_leaf.clone(),
            on: on.clone(),
            on_aliases: on_aliases.clone(),
            on_other: *on_other,
            on_filters: on_filters.clone(),
            equi: equi.clone(),
            equi_reversed: equi_reversed.clone(),
            merged: merged.clone(),
            expr_keys: expr_keys.clone(),
            ec: ec.clone(),
            left_keys: left_keys.clone(),
            right_keys: right_keys.clone(),
        };
        let top_sel = with_tree(sel, top, &[])?;
        if hashed {
            let (a, b, join, pick, indexes) = self.hash_join_estimate_over(&top_sel, Some(&rel))?;
            match pick {
                // The pair never takes a parameterized path here.
                scan_estimate::JoinPick::NestParam { .. }
                | scan_estimate::JoinPick::Nest { .. } => return None,
                scan_estimate::JoinPick::Merge { .. } => {
                    explain::join_key_texts(&top_sel)?;
                }
                scan_estimate::JoinPick::Hash { .. } => {}
            }
            let pair_first = matches!(
                pick,
                scan_estimate::JoinPick::Hash { swap: true }
                    | scan_estimate::JoinPick::Merge { b_outer: true, .. }
            );
            let [first, second] = &indexes;
            let scan_index = if pair_first { second } else { first };
            explain::apply_join_pick(
                &mut priced,
                &top_sel,
                (a, b, join, pick, [None, None]),
                join_conds,
            );
            // The preserved table's scan read through its index (RP-369).
            if let Some(index) = scan_index {
                if let Some(scan) = find_scan(&mut priced, preserved_alias) {
                    explain::name_scan_index(scan, &Some((index.clone(), false)));
                }
            }
        } else {
            let est = self.nest_loop_estimate_over(&top_sel, Some(&rel))?;
            // The preserved side is always the loop's outer.
            if est.0.b_outer || est.4.is_some() {
                return None;
            }
            explain::apply_nest_loop(&mut priced, est);
        }
        priced.estimate?;
        *tree = priced;
        Some(())
    }

    /// Price `tree`, the plan drawn for a join of two plain tables, as the
    /// two-table join it is: the hash, merge or parameterized nested loop
    /// path [`Self::hash_join_estimate`] picks, or the nested loop
    /// [`Self::nest_loop_estimate`] prices. `None` where neither prices it.
    fn price_pair(
        &self,
        sel: &secantus_pgplan::Select,
        tree: &mut PlanNode,
        join_conds: &[String],
    ) -> Option<()> {
        let plain_scan = |n: &PlanNode| n.children.is_empty() && n.name.starts_with("Seq Scan on ");
        let hash = matches!(tree.name.as_str(), "Hash Join")
            && tree.children.len() == 2
            && plain_scan(&tree.children[0])
            && tree.children[1].name == "Hash"
            && tree.children[1].children.len() == 1
            && plain_scan(&tree.children[1].children[0]);
        let nest = tree.name == "Nested Loop"
            && tree.details.is_empty()
            && tree.children.len() == 2
            && tree.children.iter().all(plain_scan);
        if hash {
            let est = self.hash_join_estimate(sel)?;
            explain::apply_join_pick(tree, sel, est, join_conds);
        } else if nest {
            let est = self.nest_loop_estimate(sel)?;
            explain::apply_nest_loop(tree, est);
        } else {
            return None;
        }
        tree.estimate.map(|_| ())
    }
}

/// The scan of `alias` among `n`'s leaves.
fn find_scan<'a>(n: &'a mut PlanNode, alias: &str) -> Option<&'a mut PlanNode> {
    if n.children.is_empty() {
        let prop = |k: &str| n.props.iter().find(|(p, _)| p == k).map(|(_, v)| v.clone());
        let name = prop("Alias").or_else(|| prop("Relation Name"))?;
        return (name == alias).then_some(n);
    }
    n.children.iter_mut().find_map(|c| find_scan(c, alias))
}
