//! Subqueries in a GROUPED query's select list, HAVING or ORDER BY.
//!
//! `SELECT d.name, (SELECT count(*) FROM e WHERE e.dept = d.name) FROM d
//! GROUP BY d.name HAVING count(*) > (SELECT ...)` needs the grouped rows to
//! exist before the subqueries run, and the aggregate planner has no per-row
//! stage of its own. So the query is split, before its subqueries are
//! resolved:
//!
//! ```sql
//! SELECT __g1 AS name, (SELECT count(*) FROM e WHERE e.dept = __g1)
//! FROM (SELECT d.name AS __g1, count(*) AS __a1 FROM d GROUP BY d.name) __grp
//! WHERE __a1 > (SELECT ...)
//! ```
//!
//! The inner query is an ordinary aggregate; the outer one is an ordinary
//! SELECT over a FROM subquery, where correlated subqueries already work.
//! Every aggregate call and every grouped column outside an aggregate
//! becomes a slot. Inside a subquery, an aggregate whose arguments read only
//! OUTER columns (`min(d.id)` in `WHERE x.dept_id = min(d.id)`) is the outer
//! query's aggregate, as PostgreSQL has it; `count(*)` there stays the
//! subquery's own.

use super::*;

struct Slots<'a> {
    /// The level being split.
    ours: Level,
    /// The FROM subquery's alias, unique against every name the statement
    /// already uses: a split nested inside another's subquery must not
    /// shadow the outer one's slots.
    alias: String,
    lookup: &'a dyn Fn(&str) -> Option<TableDef>,
    /// `(print of the grouped expression, its bare column name if any, slot)`.
    groups: Vec<(String, Option<String>, String)>,
    /// `(print of the aggregate call, slot)`.
    aggs: Vec<(String, String)>,
    inner_targets: Vec<pg_query::protobuf::Node>,
    /// An outer column a subquery reads that is neither grouped nor inside
    /// an aggregate: PostgreSQL's 42803.
    ungrouped: Option<String>,
}

/// A slot as the outer query (or any subquery, at any depth) reads it:
/// qualified by the FROM subquery's alias, so a nested subquery finds it as
/// an outer reference the same way it finds `d.id`.
fn slot_ref(alias: &str, slot: &str) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
            fields: vec![string_node(alias), string_node(slot)],
            location: -1,
        })),
    }
}

fn target(name: &str, val: pg_query::protobuf::Node) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
            name: name.to_string(),
            val: Some(Box::new(val)),
            location: -1,
            ..Default::default()
        }))),
    }
}

/// `__grp`, or `__grp2`, `__grp3`, ... when the statement already has it.
fn fresh_alias(s: &pg_query::protobuf::SelectStmt) -> String {
    let printed = format!("{s:?}");
    let mut k = 1;
    loop {
        let name = if k == 1 {
            "__grp".to_string()
        } else {
            format!("__grp{k}")
        };
        if !printed.contains(&format!("\"{name}\"")) {
            return name;
        }
        k += 1;
    }
}

/// Is this ORDER BY item a bare name one of the output columns carries?
fn bare_output_name(item: &pg_query::protobuf::Node, out_names: &[String]) -> bool {
    let Some(N::SortBy(sb)) = item.node.as_ref() else {
        return false;
    };
    match sb.node.as_deref().and_then(|n| n.node.as_ref()) {
        Some(N::ColumnRef(c)) if c.fields.len() == 1 => {
            column_ref_name(c).is_some_and(|n| out_names.contains(&n))
        }
        _ => false,
    }
}

pub(crate) fn contains_sublink(n: &pg_query::protobuf::Node) -> bool {
    let mut found = false;
    let mut n = n.clone();
    let _ = walk_expr(&mut n, &mut |x| {
        found |= matches!(x.node.as_ref(), Some(N::SubLink(_)));
        Ok(())
    });
    found
}

/// Names a subquery's own FROM exposes (tables and aliases).
fn from_names(s: &pg_query::protobuf::SelectStmt) -> Vec<String> {
    let mut out = Vec::new();
    for item in &s.from_clause {
        collect_from_names(item, &mut out);
    }
    out
}

/// What one query level's FROM exposes: its relation names and aliases,
/// and every column name when every FROM item's columns are known.
#[derive(Clone)]
pub(crate) struct Level {
    names: Vec<String>,
    cols: Option<Vec<String>>,
}

impl Level {
    pub(crate) fn of(
        s: &pg_query::protobuf::SelectStmt,
        lookup: &dyn Fn(&str) -> Option<TableDef>,
    ) -> Level {
        let mut cols = Some(Vec::new());
        for item in &s.from_clause {
            from_item_cols(item, lookup, &mut cols);
        }
        Level {
            names: from_names(s),
            cols,
        }
    }

    /// `None` when the level's columns are not all known.
    fn has_col(&self, c: &str) -> Option<bool> {
        self.cols.as_ref().map(|v| v.iter().any(|x| x == c))
    }
}

fn from_item_cols(
    item: &pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    cols: &mut Option<Vec<String>>,
) {
    if cols.is_none() {
        return;
    }
    let mut unknown = false;
    let mut found: Vec<String> = Vec::new();
    match item.node.as_ref() {
        Some(N::RangeVar(r)) => match lookup(&relation_name(r)) {
            Some(def) => found.extend(def.columns.iter().map(|c| c.name.clone())),
            None => unknown = true,
        },
        Some(N::RangeSubselect(rs)) => {
            let renamed: Vec<String> = rs
                .alias
                .as_ref()
                .map(|a| {
                    a.colnames
                        .iter()
                        .filter_map(|c| match c.node.as_ref() {
                            Some(N::String(s)) => Some(s.sval.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            match rs.subquery.as_deref().and_then(|q| q.node.as_ref()) {
                Some(N::SelectStmt(b))
                    if b.op == pg_query::protobuf::SetOperation::SetopNone as i32
                        && !b.target_list.is_empty() =>
                {
                    for (i, t) in b.target_list.iter().enumerate() {
                        if let Some(n) = renamed.get(i) {
                            found.push(n.clone());
                            continue;
                        }
                        match t.node.as_ref() {
                            Some(N::ResTarget(rt)) if !rt.name.is_empty() => {
                                found.push(rt.name.clone())
                            }
                            Some(N::ResTarget(rt)) => match rt.val.as_deref() {
                                Some(v) => {
                                    let star = matches!(v.node.as_ref(),
                                        Some(N::ColumnRef(c)) if c.fields.iter().any(|f|
                                            matches!(f.node.as_ref(), Some(N::AStar(_)))));
                                    if star {
                                        unknown = true;
                                    } else {
                                        found.push(expression_column_name(v));
                                    }
                                }
                                None => unknown = true,
                            },
                            _ => unknown = true,
                        }
                    }
                }
                _ => unknown = true,
            }
        }
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                from_item_cols(side, lookup, cols);
            }
            return;
        }
        _ => unknown = true,
    }
    if unknown {
        *cols = None;
    } else if let Some(c) = cols.as_mut() {
        c.extend(found);
    }
}

/// Where a column reference made inside a subquery resolves, relative to
/// the query level `ours` whose aggregates are being decided.
#[derive(PartialEq)]
enum At {
    /// A subquery between (or at) the reference and `ours`.
    Inner,
    Ours,
    /// A level above `ours`: a constant to it.
    Higher,
}

fn ref_parts(c: &pg_query::protobuf::ColumnRef) -> Vec<String> {
    c.fields
        .iter()
        .filter_map(|p| match p.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

fn ref_level(c: &pg_query::protobuf::ColumnRef, stack: &[Level], ours: &Level) -> At {
    if stack.is_empty() {
        return At::Ours;
    }
    let parts = ref_parts(c);
    if parts.len() >= 2 {
        let q = &parts[parts.len() - 2];
        if stack.iter().any(|l| l.names.contains(q)) {
            At::Inner
        } else if ours.names.contains(q) {
            At::Ours
        } else {
            At::Higher
        }
    } else {
        let Some(col) = parts.last() else {
            return At::Inner;
        };
        if c.fields.len() != 1 || stack.iter().any(|l| l.has_col(col) != Some(false)) {
            At::Inner
        } else if ours.has_col(col) != Some(false) {
            At::Ours
        } else {
            At::Higher
        }
    }
}

/// PostgreSQL's rule: an aggregate belongs to the LOWEST query level whose
/// variables it reads (`agglevelsup`). So inside a subquery, `max(s.x)`
/// over only the outer `s` is the outer query's aggregate; `count(*)`, and
/// anything reading the subquery's own columns, stays the subquery's.
fn agg_is_ours(n: &pg_query::protobuf::Node, stack: &[Level], ours: &Level) -> bool {
    if stack.is_empty() {
        return true;
    }
    let mut inner = false;
    let mut mine = false;
    let mut n = n.clone();
    let _ = walk_expr(&mut n, &mut |x| {
        match x.node.as_ref() {
            Some(N::SubLink(_)) => inner = true,
            Some(N::ColumnRef(c)) => match ref_level(c, stack, ours) {
                At::Inner => inner = true,
                At::Ours => mine = true,
                At::Higher => {}
            },
            _ => {}
        }
        Ok(())
    });
    mine && !inner
}

impl Slots<'_> {
    fn agg_slot(&mut self, call: &pg_query::protobuf::Node) -> String {
        let print = node_print(call);
        if let Some((_, s)) = self.aggs.iter().find(|(p, _)| *p == print) {
            return s.clone();
        }
        let slot = format!("__a{}", self.aggs.len() + 1);
        self.inner_targets.push(target(&slot, call.clone()));
        self.aggs.push((print, slot.clone()));
        slot
    }

    fn group_slot(&self, n: &pg_query::protobuf::Node) -> Option<String> {
        let print = node_print(n);
        if let Some((_, _, s)) = self.groups.iter().find(|(p, _, _)| *p == print) {
            return Some(s.clone());
        }
        if let Some(N::ColumnRef(c)) = n.node.as_ref() {
            let name = column_ref_name(c)?;
            return self
                .groups
                .iter()
                .find(|(_, col, _)| col.as_deref() == Some(name.as_str()))
                .map(|(_, _, s)| s.clone());
        }
        None
    }

    /// Rewrite an outer expression: aggregates and grouped columns to slots.
    /// `stack` holds the subquery levels between `n` and the level being
    /// split, innermost last (empty at that level itself), whose own
    /// aggregates and columns stay put.
    fn rewrite(&mut self, n: &mut pg_query::protobuf::Node, stack: &[Level]) {
        // A grouped EXPRESSION (`GROUP BY n % 2`) written again in the
        // outer query is its slot, wherever it sits (`(n % 2)::text`).
        if stack.is_empty()
            && !matches!(
                n.node.as_ref(),
                Some(N::ColumnRef(_)) | Some(N::AConst(_)) | None
            )
        {
            if let Some(slot) = self.group_slot(n) {
                *n = slot_ref(&self.alias, &slot);
                return;
            }
        }
        match n.node.as_mut() {
            // `GROUPING(k)` is the grouped query's to answer, like an
            // aggregate: the inner query computes it.
            Some(N::GroupingFunc(_)) if stack.is_empty() => {
                let slot = self.agg_slot(n);
                *n = slot_ref(&self.alias, &slot);
                return;
            }
            Some(N::FuncCall(f)) if is_aggregate_call(f) && f.over.is_none() => {
                if agg_is_ours(n, stack, &self.ours) {
                    let slot = self.agg_slot(n);
                    *n = slot_ref(&self.alias, &slot);
                    return;
                }
            }
            Some(N::ColumnRef(c)) => {
                let c = c.clone();
                let qualified_outer =
                    !stack.is_empty() && ref_level(&c, stack, &self.ours) == At::Ours;
                if stack.is_empty() || qualified_outer {
                    match self.group_slot(n) {
                        Some(slot) => *n = slot_ref(&self.alias, &slot),
                        None if qualified_outer && self.ungrouped.is_none() => {
                            let parts: Vec<String> = c
                                .fields
                                .iter()
                                .filter_map(|p| match p.node.as_ref() {
                                    Some(N::String(s)) => Some(s.sval.clone()),
                                    _ => None,
                                })
                                .collect();
                            self.ungrouped = Some(parts.join("."));
                        }
                        None => {}
                    }
                }
                return;
            }
            Some(N::SubLink(sl)) => {
                if let Some(N::SelectStmt(body)) =
                    sl.subselect.as_deref_mut().and_then(|q| q.node.as_mut())
                {
                    let mut deeper = stack.to_vec();
                    deeper.push(Level::of(body, self.lookup));
                    self.rewrite_select(body, &deeper);
                }
                if let Some(t) = sl.testexpr.as_deref_mut() {
                    self.rewrite(t, stack);
                }
                return;
            }
            _ => {}
        }
        // Descend one level into the node's children.
        let mut children: Vec<&mut pg_query::protobuf::Node> = Vec::new();
        match n.node.as_mut() {
            Some(N::AExpr(e)) => {
                children.extend(e.lexpr.as_deref_mut());
                children.extend(e.rexpr.as_deref_mut());
            }
            Some(N::BoolExpr(b)) => children.extend(b.args.iter_mut()),
            Some(N::FuncCall(f)) => children.extend(f.args.iter_mut()),
            Some(N::TypeCast(t)) => children.extend(t.arg.as_deref_mut()),
            Some(N::NullTest(t)) => children.extend(t.arg.as_deref_mut()),
            Some(N::BooleanTest(t)) => children.extend(t.arg.as_deref_mut()),
            Some(N::CoalesceExpr(c)) => children.extend(c.args.iter_mut()),
            Some(N::MinMaxExpr(m)) => children.extend(m.args.iter_mut()),
            Some(N::List(l)) => children.extend(l.items.iter_mut()),
            Some(N::RowExpr(r)) => children.extend(r.args.iter_mut()),
            Some(N::AArrayExpr(a)) => children.extend(a.elements.iter_mut()),
            // `(array_agg(x))[1]`: the subscripted value and its subscripts.
            Some(N::AIndirection(a)) => {
                children.extend(a.arg.as_deref_mut());
                for i in &mut a.indirection {
                    if let Some(N::AIndices(ix)) = i.node.as_mut() {
                        children.extend(ix.lidx.as_deref_mut());
                        children.extend(ix.uidx.as_deref_mut());
                    }
                }
            }
            Some(N::CaseExpr(c)) => {
                children.extend(c.arg.as_deref_mut());
                for w in &mut c.args {
                    if let Some(N::CaseWhen(cw)) = w.node.as_mut() {
                        children.extend(cw.expr.as_deref_mut());
                        children.extend(cw.result.as_deref_mut());
                    }
                }
                children.extend(c.defresult.as_deref_mut());
            }
            Some(N::ResTarget(rt)) => children.extend(rt.val.as_deref_mut()),
            Some(N::SortBy(sb)) => children.extend(sb.node.as_deref_mut()),
            _ => {}
        }
        for c in children {
            self.rewrite(c, stack);
        }
    }

    fn rewrite_select(&mut self, s: &mut pg_query::protobuf::SelectStmt, stack: &[Level]) {
        for t in &mut s.target_list {
            self.rewrite(t, stack);
        }
        if let Some(w) = s.where_clause.as_deref_mut() {
            self.rewrite(w, stack);
        }
        if let Some(h) = s.having_clause.as_deref_mut() {
            self.rewrite(h, stack);
        }
        for o in &mut s.sort_clause {
            self.rewrite(o, stack);
        }
    }
}

/// Split a grouped SELECT whose select list, HAVING or ORDER BY holds a
/// subquery; `None` when there is nothing to split (or a shape this does not
/// split: DISTINCT, windows, grouping sets, set operations).
pub(crate) fn split(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    check_misplaced_outer_aggregates(s, lookup)?;
    let Some(out) = split_inner_with(s, false, lookup) else {
        return Ok(None);
    };
    match out {
        (_, Some(col)) => Err(Error::Grouping(format!(
            "subquery uses ungrouped column \"{col}\" from outer query"
        ))),
        (stmt, None) => {
            if !s.group_clause.is_empty() || has_aggregate(s) {
                Ok(Some(stmt))
            } else {
                // Grouped only by an aggregate a subquery holds for it: an
                // outer column read outside one is 42803 as for any other
                // aggregate query.
                check_stray(s, &stmt, Some(&Level::of(s, lookup)))?;
                Ok(Some(stmt))
            }
        }
    }
}

/// `split` where a subquery holds an aggregate for `s` (or misplaces one):
/// for a query below the statement, which is otherwise planned as it is --
/// re-splitting a split's own inner query (an aggregate over a subquery,
/// `sum((select ...))`) would never end.
pub(crate) fn split_for_outer_aggregates(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    check_misplaced_outer_aggregates(s, lookup)?;
    let holds = s
        .target_list
        .iter()
        .chain(s.having_clause.as_deref())
        .chain(s.sort_clause.iter())
        .any(|n| outer_level_aggregates(s, lookup, n).is_some());
    if !holds {
        return Ok(None);
    }
    split(s, lookup)
}

/// The aggregates a subquery in `n` holds for the level `s` -- PostgreSQL's
/// outer-level aggregates (see `agg_is_ours`).
fn outer_level_aggregates(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    n: &pg_query::protobuf::Node,
) -> Option<i32> {
    if !contains_sublink(n) {
        return None;
    }
    let mut slots = Slots {
        ours: Level::of(s, lookup),
        alias: fresh_alias(s),
        lookup,
        groups: Vec::new(),
        aggs: Vec::new(),
        inner_targets: Vec::new(),
        ungrouped: None,
    };
    // Only the subqueries: an aggregate written directly at this level is
    // the level's own business (and checked where the level is planned).
    let mut n = n.clone();
    let mut bodies: Vec<pg_query::protobuf::Node> = Vec::new();
    let _ = walk_expr(&mut n, &mut |x| {
        if matches!(x.node.as_ref(), Some(N::SubLink(_))) {
            bodies.push(x.clone());
        }
        Ok(())
    });
    for mut b in bodies {
        slots.rewrite(&mut b, &[]);
    }
    slots.inner_targets.first().map(|t| match t.node.as_ref() {
        Some(N::ResTarget(rt)) => match rt.val.as_deref().and_then(|v| v.node.as_ref()) {
            Some(N::FuncCall(f)) => f.location,
            _ => -1,
        },
        _ => -1,
    })
}

/// An outer-level aggregate inside a subquery in WHERE, a JOIN condition or
/// GROUP BY is misplaced exactly as one written there directly (42803).
fn check_misplaced_outer_aggregates(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<()> {
    let fail = |at: i32, what: &str| -> Result<()> {
        if at >= 0 {
            set_error_location(at);
        }
        Err(Error::Grouping(format!(
            "aggregate functions are not allowed in {what}"
        )))
    };
    if let Some(w) = s.where_clause.as_deref() {
        if let Some(at) = outer_level_aggregates(s, lookup, w) {
            return fail(at, "WHERE");
        }
    }
    fn quals(n: &pg_query::protobuf::Node, out: &mut Vec<pg_query::protobuf::Node>) {
        if let Some(N::JoinExpr(j)) = n.node.as_ref() {
            out.extend(j.quals.as_deref().cloned());
            for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                quals(side, out);
            }
        }
    }
    let mut on = Vec::new();
    for f in &s.from_clause {
        quals(f, &mut on);
    }
    for q in &on {
        if let Some(at) = outer_level_aggregates(s, lookup, q) {
            return fail(at, "JOIN conditions");
        }
    }
    for g in &s.group_clause {
        if let Some(at) = outer_level_aggregates(s, lookup, g) {
            return fail(at, "GROUP BY");
        }
    }
    // A LATERAL subquery's aggregate over only its left siblings belongs
    // to this level, whose FROM it sits in.
    fn laterals(n: &pg_query::protobuf::Node, out: &mut Vec<pg_query::protobuf::SelectStmt>) {
        match n.node.as_ref() {
            Some(N::JoinExpr(j)) => {
                for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                    laterals(side, out);
                }
            }
            Some(N::RangeSubselect(rs)) if rs.lateral => {
                if let Some(N::SelectStmt(b)) = rs.subquery.as_deref().and_then(|q| q.node.as_ref())
                {
                    out.push((**b).clone());
                }
            }
            _ => {}
        }
    }
    let mut bodies = Vec::new();
    for f in &s.from_clause {
        laterals(f, &mut bodies);
    }
    for mut b in bodies {
        let mut slots = Slots {
            ours: Level::of(s, lookup),
            alias: String::new(),
            lookup,
            groups: Vec::new(),
            aggs: Vec::new(),
            inner_targets: Vec::new(),
            ungrouped: None,
        };
        let level = Level::of(&b, lookup);
        slots.rewrite_select(&mut b, &[level]);
        if let Some(t) = slots.inner_targets.first() {
            let at = match t.node.as_ref() {
                Some(N::ResTarget(rt)) => match rt.val.as_deref().and_then(|v| v.node.as_ref()) {
                    Some(N::FuncCall(f)) => f.location,
                    _ => -1,
                },
                _ => -1,
            };
            return fail(at, "FROM clause of their own query level");
        }
    }
    Ok(())
}

/// Split a grouped SELECT whose select list, HAVING or ORDER BY computes
/// over its groups -- `n::text ... GROUP BY n`, `-n, count(*)` -- which the
/// aggregate planner refuses: the inner query groups, the outer one computes.
/// The planner's fallback when the grouped plan is unsupported. An outer
/// column that is neither grouped nor aggregated is PostgreSQL's 42803.
pub(crate) fn split_expressions(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    let Some((stmt, ungrouped)) = split_inner_with(s, true, lookup) else {
        return Ok(None);
    };
    if let Some(col) = ungrouped {
        return Err(Error::Grouping(format!(
            "subquery uses ungrouped column \"{col}\" from outer query"
        )));
    }
    check_stray(s, &stmt, None)?;
    Ok(Some(stmt))
}

/// A column the split's outer query still reads that is not a slot: 42803.
/// With `only`, just the columns that level provably owns (a reference to
/// a level above it is a constant there, not a stray).
fn check_stray(
    s: &pg_query::protobuf::SelectStmt,
    stmt: &pg_query::protobuf::SelectStmt,
    only: Option<&Level>,
) -> Result<()> {
    let alias = match stmt.from_clause.first().and_then(|f| f.node.as_ref()) {
        Some(N::RangeSubselect(rs)) => rs
            .alias
            .as_ref()
            .map(|a| a.aliasname.clone())
            .unwrap_or_default(),
        _ => String::new(),
    };
    let mut stray: Option<Vec<String>> = None;
    let mut check = |n: &pg_query::protobuf::Node| {
        let mut n = n.clone();
        let _ = walk_expr(&mut n, &mut |x| {
            match x.node.as_ref() {
                // A subquery's own columns are its business; blank it so
                // the walk does not descend.
                Some(N::SubLink(_)) => *x = pg_query::protobuf::Node { node: None },
                // `GROUPING()`'s arguments are checked against the GROUP BY
                // by the aggregate planner, with PostgreSQL's own message.
                Some(N::GroupingFunc(_)) => *x = pg_query::protobuf::Node { node: None },
                Some(N::ColumnRef(c)) => {
                    let parts: Vec<String> = c
                        .fields
                        .iter()
                        .filter_map(|p| match p.node.as_ref() {
                            Some(N::String(s)) => Some(s.sval.clone()),
                            _ => None,
                        })
                        .collect();
                    let owned = match only {
                        None => true,
                        Some(l) => match parts.as_slice() {
                            [.., q, _] => l.names.contains(q),
                            [c] => l.has_col(c) == Some(true),
                            _ => false,
                        },
                    };
                    if owned && parts.first() != Some(&alias) && stray.is_none() {
                        stray = Some(parts);
                    }
                }
                _ => {}
            }
            Ok(())
        });
    };
    for t in &stmt.target_list {
        check(t);
    }
    if let Some(w) = stmt.where_clause.as_deref() {
        check(w);
    }
    let out_names: Vec<String> = stmt
        .target_list
        .iter()
        .filter_map(|t| match t.node.as_ref() {
            Some(N::ResTarget(rt)) => Some(rt.name.clone()),
            _ => None,
        })
        .collect();
    for o in &stmt.sort_clause {
        if !bare_output_name(o, &out_names) {
            check(o);
        }
    }
    if let Some(mut parts) = stray {
        if parts.len() == 1 {
            if let [only] = s.from_clause.as_slice() {
                if let Some(N::RangeVar(rv)) = only.node.as_ref() {
                    let q = rv
                        .alias
                        .as_ref()
                        .map(|a| a.aliasname.clone())
                        .unwrap_or_else(|| rv.relname.clone());
                    parts.insert(0, q);
                }
            }
        }
        return Err(Error::Grouping(format!(
            "column \"{}\" must appear in the GROUP BY clause or be used in an aggregate function",
            parts.join(".")
        )));
    }
    Ok(())
}

/// The key expressions of one GROUP BY item: itself, or what a
/// `ROLLUP` / `CUBE` / `GROUPING SETS` (nested or not) is made of.
fn grouping_keys<'a>(g: &'a pg_query::protobuf::Node, out: &mut Vec<&'a pg_query::protobuf::Node>) {
    match g.node.as_ref() {
        Some(N::GroupingSet(set)) => {
            for c in &set.content {
                grouping_keys(c, out);
            }
        }
        // `rollup ((a, b), c)`: a parenthesised pair is one step of two keys.
        Some(N::RowExpr(r)) => {
            for c in &r.args {
                grouping_keys(c, out);
            }
        }
        Some(_) => out.push(g),
        None => {}
    }
}

fn split_inner_with(
    s: &pg_query::protobuf::SelectStmt,
    force: bool,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<(pg_query::protobuf::SelectStmt, Option<String>)> {
    // An aggregate a subquery in the select list, HAVING or ORDER BY holds
    // for this level makes this an aggregate query, as one written here
    // directly does: `select (select max(s.x) from t) from s` is ONE row.
    let grouped = !s.group_clause.is_empty()
        || has_aggregate(s)
        || s.target_list
            .iter()
            .chain(s.having_clause.as_deref())
            .chain(s.sort_clause.iter())
            .any(|n| outer_level_aggregates(s, lookup, n).is_some());
    // A plain DISTINCT applies to the grouped rows: the outer query's.
    let plain_distinct = matches!(s.distinct_clause.as_slice(), [d] if d.node.is_none());
    if !grouped
        || (!s.distinct_clause.is_empty() && !plain_distinct)
        || !s.window_clause.is_empty()
        || has_window(s)
        || s.op != pg_query::protobuf::SetOperation::SetopNone as i32
    {
        return None;
    }
    let has = s.target_list.iter().any(contains_sublink)
        || s.having_clause.as_deref().is_some_and(contains_sublink)
        || s.sort_clause.iter().any(contains_sublink);
    if !has && !force {
        return None;
    }
    let mut slots = Slots {
        ours: Level::of(s, lookup),
        alias: fresh_alias(s),
        lookup,
        groups: Vec::new(),
        aggs: Vec::new(),
        inner_targets: Vec::new(),
        ungrouped: None,
    };
    // Under `ROLLUP` / `CUBE` / `GROUPING SETS` the keys are the
    // expressions the sets are made of; the inner query keeps the clause
    // and yields each key once, NULL in a row whose set leaves it out.
    let mut keys: Vec<&pg_query::protobuf::Node> = Vec::new();
    for g in &s.group_clause {
        grouping_keys(g, &mut keys);
    }
    for g in keys {
        let print = node_print(g);
        if slots.groups.iter().any(|(p, _, _)| *p == print) {
            continue;
        }
        let slot = format!("__g{}", slots.groups.len() + 1);
        let col = match g.node.as_ref() {
            Some(N::ColumnRef(c)) => column_ref_name(c),
            _ => None,
        };
        slots.groups.push((print, col, slot.clone()));
        slots.inner_targets.push(target(&slot, g.clone()));
    }
    let mut outer_targets = Vec::new();
    for t in &s.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return None;
        };
        let mut val = rt.val.as_deref()?.clone();
        let name = if rt.name.is_empty() {
            match val.node.as_ref() {
                Some(N::SubLink(sl))
                    if SubLinkType::try_from(sl.sub_link_type)
                        == Ok(SubLinkType::ExistsSublink) =>
                {
                    "exists".to_string()
                }
                Some(N::SubLink(_)) => {
                    // A scalar subquery is named after its own single column.
                    match &val.node {
                        Some(N::SubLink(sl)) => {
                            match sl.subselect.as_deref().and_then(|q| q.node.as_ref()) {
                                Some(N::SelectStmt(b)) => {
                                    match b.target_list.first().and_then(|t| t.node.as_ref()) {
                                        Some(N::ResTarget(r)) if !r.name.is_empty() => {
                                            r.name.clone()
                                        }
                                        Some(N::ResTarget(r)) => r
                                            .val
                                            .as_deref()
                                            .map(expression_column_name)
                                            .unwrap_or_else(|| "?column?".into()),
                                        _ => "?column?".into(),
                                    }
                                }
                                _ => "?column?".into(),
                            }
                        }
                        _ => "?column?".into(),
                    }
                }
                _ => expression_column_name(&val),
            }
        } else {
            rt.name.clone()
        };
        slots.rewrite(&mut val, &[]);
        outer_targets.push(target(&name, val));
    }
    let mut having = s.having_clause.as_deref().cloned();
    if let Some(h) = having.as_mut() {
        slots.rewrite(h, &[]);
    }
    // A bare name in ORDER BY that is an OUTPUT column's name is that output
    // column, before it is any input column (`SELECT n::text ... ORDER BY n`
    // sorts the text): the outer query resolves it, so it stays as written.
    let out_names: Vec<String> = outer_targets
        .iter()
        .filter_map(|t| match t.node.as_ref() {
            Some(N::ResTarget(rt)) => Some(rt.name.clone()),
            _ => None,
        })
        .collect();
    let mut sort = s.sort_clause.clone();
    for item in &mut sort {
        if bare_output_name(item, &out_names) {
            continue;
        }
        slots.rewrite(item, &[]);
    }
    // An output alias in ORDER BY names the output column, which the outer
    // query still has under the same name.
    let mut inner = s.clone();
    inner.target_list = slots.inner_targets.clone();
    inner.having_clause = None;
    inner.distinct_clause = Vec::new();
    inner.sort_clause = Vec::new();
    inner.limit_count = None;
    inner.limit_offset = None;
    inner.limit_option = pg_query::protobuf::LimitOption::Default as i32;
    if inner.target_list.is_empty() {
        inner.target_list.push(target(
            "__n",
            pg_query::protobuf::Node {
                node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
                    funcname: vec![string_node("count")],
                    agg_star: true,
                    funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall as i32,
                    location: -1,
                    ..Default::default()
                }))),
            },
        ));
    }
    let ungrouped = slots.ungrouped.take();
    Some((
        pg_query::protobuf::SelectStmt {
            target_list: outer_targets,
            from_clause: vec![pg_query::protobuf::Node {
                node: Some(N::RangeSubselect(Box::new(
                    pg_query::protobuf::RangeSubselect {
                        lateral: false,
                        subquery: Some(Box::new(pg_query::protobuf::Node {
                            node: Some(N::SelectStmt(Box::new(inner))),
                        })),
                        alias: Some(pg_query::protobuf::Alias {
                            aliasname: slots.alias.clone(),
                            colnames: Vec::new(),
                        }),
                    },
                ))),
            }],
            where_clause: having.map(Box::new),
            distinct_clause: s.distinct_clause.clone(),
            sort_clause: sort,
            limit_count: s.limit_count.clone(),
            limit_offset: s.limit_offset.clone(),
            limit_option: s.limit_option,
            op: pg_query::protobuf::SetOperation::SetopNone as i32,
            ..Default::default()
        },
        ungrouped,
    ))
}
