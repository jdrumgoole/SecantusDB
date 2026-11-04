//! `EXPLAIN`: the SHAPE of a statement's plan, in PostgreSQL's layout.
//!
//! The node TYPES and their nesting are real -- a scan is an `Index Scan`
//! exactly when the storage planner would use the index. A node's costs,
//! rows and width are PostgreSQL's estimate where the cost model prices
//! that node (a single relation's Seq Scan, `scan_estimate.rs`, RP-314; a
//! select with no FROM, a `Result`, RP-325 and RP-340; an INSERT,
//! UPDATE or DELETE, RP-331; a Nested Loop of two Seq Scans with its
//! Materialize, RP-329; an inner Hash Join of two Seq Scans and its Hash,
//! RP-330);
//! every other node still prints zeros, each kind its own backlog item.

use bson::Document;
use secantus_pgplan::joins::{JoinKind, JoinNode};
use secantus_pgplan::{ExplainOptions, Statement};

/// One plan node: its header, its detail lines, its children.
pub(crate) struct PlanNode {
    pub name: String,
    /// `(label, value)`: `Sort Key: id`, `Group Key: g`, `Index Name: ...`.
    pub details: Vec<(String, String)>,
    pub children: Vec<PlanNode>,
    /// JSON-only facts (`Relation Name`, `Index Name`).
    pub props: Vec<(String, String)>,
    /// The planner's `(startup, total, rows, width)`; `None` prints zeros.
    pub estimate: Option<crate::scan_estimate::Estimate>,
    /// A join whose inner side can match each outer row at most once
    /// (`Join.inner_unique`, `innerrel_is_unique`), which the structured
    /// formats print as `Inner Unique` (RP-353).
    pub inner_unique: bool,
    /// For a join whose inner side is one table's scan: that table and
    /// the columns its join clauses and constant equalities fix, which
    /// [`resolve_inner_unique`] checks against the table's unique indexes.
    pub unique_probe: Option<(String, Vec<String>)>,
    /// A hash join whose sides the cost model swapped (RP-359): its
    /// `Hash Cond` is written outer side first, as `create_hashjoin_plan`
    /// commutes the clause to `outer = inner`.
    pub commute_cond: bool,
    /// A join whose clause an equivalence class with a constant replaced
    /// (RP-363): the ON's condition is not printed on it.
    pub skip_cond: bool,
    /// A scan the class filters by the constant too (RP-363): `(own key
    /// column, (alias, key column) whose WHERE fixed the other key)`. Its
    /// `Filter` takes the other scan's constant, over this column.
    pub implied_filter: Option<ImpliedFilter>,
    /// Under ANALYZE, an index-only scan's `Heap Fetches`
    /// (`ioss_HeapFetches`), printed after its quals (RP-360).
    pub heap_fetches: Option<usize>,
}

impl PlanNode {
    pub(crate) fn new(name: impl Into<String>) -> Self {
        PlanNode {
            name: name.into(),
            details: Vec::new(),
            children: Vec::new(),
            props: Vec::new(),
            estimate: None,
            inner_unique: false,
            unique_probe: None,
            commute_cond: false,
            heap_fetches: None,
            skip_cond: false,
            implied_filter: None,
        }
    }

    fn over(name: impl Into<String>, child: PlanNode) -> Self {
        let mut n = PlanNode::new(name);
        n.children.push(child);
        n
    }
}

/// How a table scan will run: `Some((index, key))` for an index scan.
pub(crate) type ScanChooser<'a> = dyn Fn(&str, &Document) -> Option<String> + 'a;

/// The plan tree of a planned statement.
pub(crate) fn plan_tree(stmt: &Statement, scan: &ScanChooser<'_>) -> PlanNode {
    match stmt {
        Statement::Select(sel) => {
            let mut node = if let Some(sub) = &sel.sub {
                match sub.plan.as_ref() {
                    Statement::JoinRows(j) => join_tree(&j.tree, scan),
                    inner => {
                        let mut n = PlanNode::over(
                            format!("Subquery Scan on {}", display_alias(&sub.alias)),
                            plan_tree(inner, scan),
                        );
                        n.props.push(("Alias".into(), display_alias(&sub.alias)));
                        n
                    }
                }
            } else if sel.series.is_some() {
                PlanNode::new("Function Scan on generate_series")
            } else if let Some(j) = &sel.join {
                join_select_tree(j, scan)
            } else if sel.table.is_empty() {
                PlanNode::new("Result")
            } else {
                table_scan(&sel.table, &sel.filter, scan)
            };
            if sel.residual.is_some() && node.details.is_empty() && !sel.table.is_empty() {
                node.details.push(("Filter".into(), "(...)".into()));
            }
            if !sel.windows.is_empty() {
                // Each distinct window is a WindowAgg, stacked in the order
                // PostgreSQL's `select_active_windows` gives them, each over
                // a Sort by its partition then order columns when the rows
                // below are not already in that order (RP-347, RP-355). A
                // stack this server cannot order as PostgreSQL does prints
                // one WindowAgg.
                match window_stack(sel) {
                    Some(steps) => {
                        for step in steps {
                            if step.sort {
                                let mut sort = PlanNode::over("Sort", node);
                                let shown: Vec<String> =
                                    step.keys.iter().map(|k| sort_key_text(k)).collect();
                                sort.details.push(("Sort Key".into(), shown.join(", ")));
                                node = sort;
                            }
                            node = PlanNode::over("WindowAgg", node);
                        }
                    }
                    None => node = PlanNode::over("WindowAgg", node),
                }
            }
            if !sel.order.is_empty() {
                let mut sort = PlanNode::over("Sort", node);
                let keys: Vec<String> = sel
                    .order
                    .iter()
                    .map(|k| {
                        let name = sel
                            .columns
                            .iter()
                            .find(|(_, f)| *f == k.field)
                            .map_or_else(|| display_alias(&k.field), |(o, _)| o.clone());
                        if k.ascending {
                            name
                        } else {
                            format!("{name} DESC")
                        }
                    })
                    .collect();
                sort.details.push(("Sort Key".into(), keys.join(", ")));
                node = sort;
            }
            if !matches!(sel.distinct, secantus_pgplan::Distinct::None) {
                node = PlanNode::over("Unique", node);
            }
            if sel.limit.is_some() || sel.offset > 0 {
                node = PlanNode::over("Limit", node);
            }
            node
        }
        Statement::Aggregate(agg) => {
            let source = if let Some(sub) = &agg.sub {
                match sub.plan.as_ref() {
                    Statement::JoinRows(j) => join_tree(&j.tree, scan),
                    inner => PlanNode::over(
                        format!("Subquery Scan on {}", display_alias(&sub.alias)),
                        plan_tree(inner, scan),
                    ),
                }
            } else if agg.table.is_empty() {
                PlanNode::new("Result")
            } else {
                table_scan(&agg.table, &agg.filter, scan)
            };
            let mut node = if agg.group_by.is_empty() {
                PlanNode::over("Aggregate", source)
            } else {
                let mut n = PlanNode::over("HashAggregate", source);
                let keys: Vec<String> = agg.group_by.iter().map(|k| k.name.clone()).collect();
                n.details.push(("Group Key".into(), keys.join(", ")));
                n
            };
            if !agg.order.is_empty() {
                node = PlanNode::over("Sort", node);
            }
            if agg.limit.is_some() || agg.offset > 0 {
                node = PlanNode::over("Limit", node);
            }
            node
        }
        Statement::SetOp(set) => {
            let mut n = PlanNode::new("Append");
            n.children.push(plan_tree(&set.left, scan));
            n.children.push(plan_tree(&set.right, scan));
            n
        }
        Statement::SelectConstant(sc) => {
            let mut n = PlanNode::new("Result");
            // A WHERE folded to false or NULL gates the row (RP-341).
            if let Some(f) = &sc.one_time_filter {
                n.details.push(("One-Time Filter".into(), f.clone()));
            }
            n
        }
        Statement::ValuesConstant(_) => PlanNode::new("Values Scan on \"*VALUES*\""),
        Statement::Insert(ins) => {
            let child = match &ins.source {
                Some(src) => plan_tree(src, scan),
                None if ins.rows.len() > 1 => PlanNode::new("Values Scan on \"*VALUES*\""),
                None => PlanNode::new("Result"),
            };
            PlanNode::over(format!("Insert on {}", ins.table), child)
        }
        Statement::Update(u) => PlanNode::over(
            format!("Update on {}", u.table),
            table_scan(&u.table, &u.filter, scan),
        ),
        Statement::Delete(d) => PlanNode::over(
            format!("Delete on {}", d.table),
            table_scan(&d.table, &d.filter, scan),
        ),
        _ => PlanNode::new("Result"),
    }
}

/// A join node's `(Node Type, Join Type)` from the name the text format
/// prints (`Hash Left Join` is a `Hash Join` of type `Left`), as
/// `ExplainNode` builds the one from the other; `None` for any other node.
fn join_type_of(name: &str) -> Option<(&'static str, &'static str)> {
    let (pname, rest) = if let Some(r) = name.strip_prefix("Nested Loop") {
        ("Nested Loop", r)
    } else if let Some(r) = name.strip_prefix("Hash") {
        ("Hash Join", r)
    } else {
        ("Merge Join", name.strip_prefix("Merge")?)
    };
    let jt = match rest {
        "" if pname == "Nested Loop" => "Inner",
        " Join" if pname != "Nested Loop" => "Inner",
        " Left Join" => "Left",
        " Full Join" => "Full",
        " Right Join" => "Right",
        " Semi Join" => "Semi",
        " Anti Join" => "Anti",
        _ => return None,
    };
    Some((pname, jt))
}

/// Decide each join's `inner_unique` (RP-353) as `innerrel_is_unique`
/// does for a base relation: its inner side is unique when one of the
/// table's unique indexes -- immediate, without a predicate, on plain
/// columns -- has every key column fixed by a join clause or a constant
/// equality (`relation_has_unique_index_for`). `unique` answers that for
/// `(table, fixed fields)`.
pub(crate) fn resolve_inner_unique(n: &mut PlanNode, unique: &dyn Fn(&str, &[String]) -> bool) {
    if let Some((table, fields)) = n.unique_probe.take() {
        n.inner_unique = unique(&table, &fields);
    }
    for c in &mut n.children {
        resolve_inner_unique(c, unique);
    }
}

/// Whether a subquery's output is distinct on the output columns `fixed`
/// marks (by position), as `query_is_distinct_for` decides it: a DISTINCT
/// (or DISTINCT ON) whose every key is fixed, a GROUP BY whose every key
/// is a fixed output, `GROUP BY ()` or an aggregate without GROUP BY (one
/// row), or a UNION / INTERSECT / EXCEPT without ALL whose every output is
/// fixed. `None` when the statement is none of those, which leaves a plain
/// table scan to the unique-index check; `Some(false)` when it is one but
/// a key is not fixed.
fn query_is_distinct_for(plan: &Statement, fixed: &[bool]) -> Option<bool> {
    use secantus_pgplan::{Distinct, OutputCol};
    let is_fixed = |i: usize| fixed.get(i).copied().unwrap_or(false);
    let all_fixed = || !fixed.is_empty() && fixed.iter().all(|&f| f);
    match plan {
        Statement::Select(s) => match &s.distinct {
            Distinct::None => None,
            Distinct::All => Some(all_fixed()),
            // Each key a bare output column the join fixes.
            Distinct::On(keys) => Some(keys.iter().all(|k| {
                s.columns.iter().enumerate().any(|(i, (_, f))| {
                    f == k && s.casts.get(i).is_none_or(Option::is_none) && is_fixed(i)
                })
            })),
        },
        Statement::Aggregate(a) => {
            if a.distinct && all_fixed() {
                return Some(true);
            }
            Some(match &a.grouping_sets {
                // `GROUP BY ()` alone is one row; any other set is not
                // proven.
                Some(sets) => sets.len() == 1 && sets[0].is_empty(),
                None if a.group_by.is_empty() => true,
                None => (0..a.group_by.len()).all(|g| {
                    a.select.iter().enumerate().any(|(i, (_, o))| {
                        matches!(o, OutputCol::Group(x) if *x == g) && is_fixed(i)
                    })
                }),
            })
        }
        Statement::SetOp(op) => Some(!op.all && all_fixed()),
        _ => None,
    }
}

/// Whether any join in the tree waits on [`resolve_inner_unique`].
pub(crate) fn has_unique_probe(n: &PlanNode) -> bool {
    n.unique_probe.is_some() || n.children.iter().any(has_unique_probe)
}

/// The fields of a leaf table's rows that a scan filter fixes to a
/// constant: `{f: v}` or `{f: {$eq: v}}` at the top level, or under `$and`.
fn constant_equalities(filter: &Document, out: &mut Vec<String>) {
    for (k, v) in filter {
        if k == "$and" {
            if let bson::Bson::Array(a) = v {
                for d in a.iter().filter_map(bson::Bson::as_document) {
                    constant_equalities(d, out);
                }
            }
        } else if k.starts_with('$') {
            continue;
        } else if let bson::Bson::Document(d) = v {
            if d.len() == 1
                && d.contains_key("$eq")
                && !matches!(d.get("$eq"), Some(bson::Bson::Null))
            {
                out.push(k.clone());
            }
        } else if !matches!(v, bson::Bson::Null | bson::Bson::RegularExpression(_)) {
            out.push(k.clone());
        }
    }
}
/// A window's sort key as EXPLAIN's `Sort Key` prints it.
fn sort_key_text(k: &secantus_pgplan::OrderKey) -> String {
    let name = display_alias(&k.field);
    match (k.ascending, k.nulls) {
        (true, secantus_pgplan::Nulls::Last) => name,
        (true, secantus_pgplan::Nulls::First) => format!("{name} NULLS FIRST"),
        (false, secantus_pgplan::Nulls::First) => format!("{name} DESC"),
        (false, secantus_pgplan::Nulls::Last) => format!("{name} DESC NULLS LAST"),
    }
}

/// One WindowAgg of a SELECT's stack: the window functions it computes
/// (indices into `Select::windows`), its window's partition then order
/// columns, and whether a Sort by them sits below it.
pub(crate) struct WindowStep<'a> {
    pub(crate) funcs: Vec<usize>,
    pub(crate) keys: Vec<&'a secantus_pgplan::OrderKey>,
    pub(crate) sort: bool,
}

/// A SELECT's WindowAggs, bottom first, as PostgreSQL 15 stacks them
/// (RP-355). Functions over identical windows share one WindowAgg
/// (`transformWindowFuncCall` reuses an identical WindowClause).
/// `select_active_windows` orders the windows by `common_prefix_cmp`:
/// position by position, the higher `tleSortGroupRef` first (refs number
/// the key columns in order of first appearance, each window's partition
/// columns then its order columns), and a longer list before its prefix.
/// `create_one_window_path` then sorts below a window only when the rows
/// are not already in its order.
///
/// `None` where this server does not reproduce PostgreSQL's order: a
/// computed key, a column repeated within one window, the same column at
/// one position under two directions (the comparator then falls to the
/// sort operators' oids), two windows with the same keys and different
/// frames (an unstable tie), and a window sharing only a prefix of the
/// order below it (an Incremental Sort).
pub(crate) fn window_stack(sel: &secantus_pgplan::Select) -> Option<Vec<WindowStep<'_>>> {
    let mut steps: Vec<WindowStep<'_>> = Vec::new();
    for (i, w) in sel.windows.iter().enumerate() {
        let same = steps.iter_mut().find(|s| {
            let f = &sel.windows[s.funcs[0]];
            f.partition_by == w.partition_by && f.order_by == w.order_by && f.frame == w.frame
        });
        match same {
            Some(s) => s.funcs.push(i),
            None => steps.push(WindowStep {
                funcs: vec![i],
                keys: w.partition_by.iter().chain(&w.order_by).collect(),
                sort: false,
            }),
        }
    }
    if steps.len() == 1 {
        // One window: a Sort below it whenever it has keys (RP-347).
        let s = &mut steps[0];
        s.sort = !s.keys.is_empty() && s.keys.iter().all(|k| k.expr.is_none());
        return Some(steps);
    }
    let mut refs: Vec<&str> = Vec::new();
    for s in &steps {
        for (j, k) in s.keys.iter().enumerate() {
            if k.expr.is_some() || s.keys[..j].iter().any(|p| p.field == k.field) {
                return None;
            }
            if !refs.contains(&k.field.as_str()) {
                refs.push(&k.field);
            }
        }
    }
    let rank = |k: &secantus_pgplan::OrderKey| refs.iter().position(|f| *f == k.field);
    let same_order = |a: &secantus_pgplan::OrderKey, b: &secantus_pgplan::OrderKey| {
        a.field == b.field && a.ascending == b.ascending && a.nulls == b.nulls
    };
    // The comparator's ties this server cannot break as PostgreSQL does.
    for (x, a) in steps.iter().enumerate() {
        for b in &steps[x + 1..] {
            let common = a
                .keys
                .iter()
                .zip(&b.keys)
                .take_while(|(p, q)| p.field == q.field);
            if common.clone().any(|(p, q)| !same_order(p, q)) {
                return None;
            }
            if common.count() == a.keys.len() && a.keys.len() == b.keys.len() {
                return None;
            }
        }
    }
    steps.sort_by(|a, b| {
        for (p, q) in a.keys.iter().zip(&b.keys) {
            match rank(q).cmp(&rank(p)) {
                std::cmp::Ordering::Equal => {}
                o => return o,
            }
        }
        b.keys.len().cmp(&a.keys.len())
    });
    let mut order: Vec<&secantus_pgplan::OrderKey> = Vec::new();
    for s in &mut steps {
        let held =
            s.keys.len() <= order.len() && s.keys.iter().zip(&order).all(|(p, q)| same_order(p, q));
        if held {
            continue;
        }
        if !s.keys.is_empty() && !order.is_empty() && same_order(s.keys[0], order[0]) {
            return None;
        }
        s.sort = true;
        order = s.keys.clone();
    }
    Some(steps)
}

/// A join's key names are `alias<US>column`; a plan names only the alias.
fn display_alias(name: &str) -> String {
    name.split(secantus_pgplan::joins::SEP)
        .next()
        .unwrap_or(name)
        .to_string()
}

fn table_scan(table: &str, filter: &Document, scan: &ScanChooser<'_>) -> PlanNode {
    let mut n = match scan(table, filter) {
        Some(index) => {
            let mut n = PlanNode::new(format!("Index Scan using {index} on {table}"));
            n.props.push(("Index Name".into(), index));
            n
        }
        None => PlanNode::new(format!("Seq Scan on {table}")),
    };
    n.props.push(("Relation Name".into(), table.to_string()));
    n
}

fn join_tree(node: &JoinNode, scan: &ScanChooser<'_>) -> PlanNode {
    match node {
        JoinNode::Lateral { .. } => PlanNode::new("Function Scan"),
        JoinNode::Leaf { plan, def, columns } => {
            let mut n = plan_tree(plan, scan);
            if n.name == "Result" && !def.name.is_empty() {
                n.name = format!("Subquery Scan on {}", def.name);
            }
            // A scan of an aliased relation names the alias too.
            let alias = columns
                .first()
                .and_then(|(key, _)| key.split(secantus_pgplan::joins::SEP).next())
                .map(str::to_string);
            let relation = n
                .props
                .iter()
                .find(|(k, _)| k == "Relation Name")
                .map(|(_, v)| v.clone());
            if let (Some(alias), Some(rel)) = (alias, relation) {
                if alias != rel && !alias.is_empty() {
                    n.name = format!("{} {alias}", n.name);
                    n.props.push(("Alias".into(), alias));
                }
            }
            n
        }
        JoinNode::Join {
            kind,
            left,
            right,
            equi,
            merged,
            on,
            ..
        } => {
            let side = match kind {
                JoinKind::Inner => "",
                JoinKind::Left => " Left",
                JoinKind::Right => " Right",
                JoinKind::Full => " Full",
            };
            let name = if equi.is_empty() {
                format!("Nested Loop{side} Join").replace("Nested Loop Join", "Nested Loop")
            } else {
                format!("Hash{side} Join")
            };
            let mut n = PlanNode::new(name);
            // The inner side one table's plain scan: the fields its join
            // clauses fix, for `resolve_inner_unique`.
            if let JoinNode::Leaf { plan, columns, .. } = right.as_ref() {
                // A subquery's DISTINCT, GROUP BY or set operation proves
                // its output unique on the columns the join fixes
                // (`rel_is_distinct_for`, RP-356).
                let fixed_outputs: Vec<bool> = columns
                    .iter()
                    .map(|(c, _)| {
                        equi.iter().any(|(_, r)| r == c) || merged.iter().any(|(_, _, r)| r == c)
                    })
                    .collect();
                if let Some(unique) = query_is_distinct_for(plan, &fixed_outputs) {
                    // No join clause at all is never unique
                    // (`innerrel_is_unique` returns false first).
                    n.inner_unique = unique && (on.is_some() || fixed_outputs.iter().any(|&f| f));
                } else if let Statement::Select(leaf) = plan.as_ref() {
                    if leaf.sub.is_none()
                        && leaf.join.is_none()
                        && leaf.series.is_none()
                        && !leaf.table.is_empty()
                    {
                        // Column names: a join key is `alias<US>column`.
                        let name = |key: &str| {
                            key.split(secantus_pgplan::joins::SEP)
                                .nth(1)
                                .map(str::to_string)
                        };
                        let mut fixed: Vec<String> = equi
                            .iter()
                            .map(|(_, r)| r.as_str())
                            .chain(merged.iter().map(|(_, _, r)| r.as_str()))
                            .filter(|k| columns.iter().any(|(c, _)| c == k))
                            .filter_map(name)
                            .collect();
                        if !fixed.is_empty() {
                            let mut consts = Vec::new();
                            constant_equalities(&leaf.filter, &mut consts);
                            fixed.extend(consts.iter().filter_map(|f| {
                                columns
                                    .iter()
                                    .find(|(_, cf)| cf == f)
                                    .and_then(|(k, _)| name(k))
                            }));
                            n.unique_probe = Some((leaf.table.clone(), fixed));
                        }
                    }
                }
            }
            n.children.push(join_tree(left, scan));
            let right = join_tree(right, scan);
            n.children.push(if equi.is_empty() {
                right
            } else {
                PlanNode::over("Hash", right)
            });
            n
        }
    }
}

/// The plan tree of a two-table join planned on the narrow path
/// (`Select::join`, RP-361), as `join_tree` draws a `JoinRows` join: each
/// side its table's scan named by its alias, or its subquery's; an ON
/// equality a `Hash Join` with the right side under a `Hash` (a LEFT join
/// `Hash Left Join`), and a cross join a `Nested Loop`.
fn join_select_tree(j: &secantus_pgplan::JoinSelect, scan: &ScanChooser<'_>) -> PlanNode {
    let side = |(table, alias): &(String, String), sub: &Option<Box<Statement>>| {
        let mut n = match sub {
            Some(plan) => {
                let mut n = PlanNode::over(
                    format!("Subquery Scan on {}", display_alias(alias)),
                    plan_tree(plan, scan),
                );
                n.props.push(("Alias".into(), display_alias(alias)));
                return n;
            }
            None => table_scan(table, &Document::new(), scan),
        };
        if !alias.is_empty() && alias != table {
            n.name = format!("{} {alias}", n.name);
            n.props.push(("Alias".into(), alias.clone()));
        }
        n
    };
    let kind = if j.left_join { " Left" } else { "" };
    let left = side(&j.left, &j.left_sub);
    let right = side(&j.right, &j.right_sub);
    match &j.on {
        Some((a, b)) => {
            let mut n = PlanNode::new(format!("Hash{kind} Join"));
            // The inner side a plain table: the column its join clause
            // fixes, for `resolve_inner_unique`.
            if j.right_sub.is_none() && !j.right.0.is_empty() {
                let col = if a.0 == j.right.1 { &a.1 } else { &b.1 };
                n.unique_probe = Some((j.right.0.clone(), vec![col.clone()]));
            }
            n.children.push(left);
            n.children.push(PlanNode::over("Hash", right));
            n
        }
        None => {
            let mut n = PlanNode::new(if j.left_join {
                "Nested Loop Left Join"
            } else {
                "Nested Loop"
            });
            n.children.push(left);
            n.children.push(right);
            n
        }
    }
}

/// Give a `Nested Loop` over two scans the plan the cost model chose
/// (RP-329): its estimate and its inputs', the outer side first, and the
/// inner read through a `Materialize` when that is the cheaper path.
/// `a` and `b` are the scans in the order the join tree lists them.
pub(crate) fn price_nest_loop(
    root: &mut PlanNode,
    plan: &secantus_pgplan::joincost::NestLoopPlan,
    a: crate::scan_estimate::Estimate,
    b: crate::scan_estimate::Estimate,
    join: crate::scan_estimate::Estimate,
) {
    if root.name != "Nested Loop" || root.children.len() != 2 {
        return;
    }
    let mut right = root.children.pop().expect("two children");
    let mut left = root.children.pop().expect("two children");
    left.estimate = Some(a);
    right.estimate = Some(b);
    let (outer, inner) = if plan.b_outer {
        (right, left)
    } else {
        (left, right)
    };
    let inner = match plan.material {
        Some((startup, total)) => {
            let (_, _, rows, width) = inner.estimate.expect("priced");
            let mut m = PlanNode::over("Materialize", inner);
            m.estimate = Some((startup, total, rows, width));
            m
        }
        None => inner,
    };
    root.children.push(outer);
    root.children.push(inner);
    root.estimate = Some(join);
}

/// `(a = b)` as `(b = a)`: one equality of two operands, neither holding
/// an ` = ` of its own; `None` for anything else.
fn commute_equality(c: &str) -> Option<String> {
    let inner = c.strip_prefix('(')?.strip_suffix(')')?;
    let (a, b) = inner.split_once(" = ")?;
    if b.contains(" = ") {
        return None;
    }
    Some(format!("({b} = {a})"))
}

/// Give an inner `Hash Join` of two scans, the right one under a `Hash`,
/// the cost model's estimate (RP-330): the join's, each scan's, and the
/// Hash node's, which is its input's total for both startup and total
/// (`create_hashjoin_plan` copies the inner path's costs). With `swap`,
/// the path PostgreSQL keeps hashes the LEFT table (RP-359): the right
/// scan becomes the outer side and the left one goes under the Hash.
/// `outer` and `inner` are the estimates of the scans in the order
/// printed.
pub(crate) fn price_hash_join(
    root: &mut PlanNode,
    outer: crate::scan_estimate::Estimate,
    inner: crate::scan_estimate::Estimate,
    join: crate::scan_estimate::Estimate,
    swap: bool,
) {
    if !matches!(root.name.as_str(), "Hash Join" | "Hash Left Join") || root.children.len() != 2 {
        return;
    }
    if root.children[1].name != "Hash" || root.children[1].children.len() != 1 {
        return;
    }
    if swap {
        let mut hash = root.children.pop().expect("two children");
        let left = root.children.pop().expect("two children");
        let right = hash.children.pop().expect("one child");
        hash.children.push(left);
        root.children.push(right);
        root.children.push(hash);
        root.commute_cond = true;
        // A LEFT join hashing its preserved side is the right join of the
        // two the other way round (RP-362).
        if root.name == "Hash Left Join" {
            root.name = "Hash Right Join".into();
        }
        // The hashed side is now the left table, not proven unique on
        // the key (a unique key is not priced).
        root.inner_unique = false;
    }
    let [scan, hash] = root.children.as_mut_slice() else {
        return;
    };
    scan.estimate = Some(outer);
    hash.children[0].estimate = Some(inner);
    hash.estimate = Some((inner.1, inner.1, inner.2, inner.3));
    root.estimate = Some(join);
}

/// A node's estimate, zeros where it has none; `+ 0.0` turns the `-0.0` an
/// empty sum is into the `0.00` PostgreSQL prints.
fn estimate_of(n: &PlanNode) -> (f64, f64, f64, i64) {
    let (startup, total, rows, width) = n.estimate.unwrap_or((0.0, 0.0, 0.0, 0));
    (startup + 0.0, total + 0.0, rows + 0.0, width)
}

/// The header's costs, as `ExplainNode` prints them: `%.2f`, rows `%.0f`.
fn costs_text(n: &PlanNode) -> String {
    let (startup, total, rows, width) = estimate_of(n);
    format!("  (cost={startup:.2}..{total:.2} rows={rows:.0} width={width})")
}

/// The `QUERY PLAN` text rows, PostgreSQL's layout: a child at depth d is
/// `2 + 6(d-1)` spaces and `->  `, and a node's details sit at `2 + 6d`.
pub(crate) fn render_text(
    root: &PlanNode,
    options: &ExplainOptions,
    actual_rows: Option<usize>,
) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(
        n: &PlanNode,
        depth: usize,
        options: &ExplainOptions,
        actual: Option<usize>,
        out: &mut Vec<String>,
    ) {
        let mut header = if depth == 0 {
            n.name.clone()
        } else {
            format!("{}->  {}", " ".repeat(2 + 6 * (depth - 1)), n.name)
        };
        if options.costs {
            header.push_str(&costs_text(n));
        }
        if let Some(rows) = actual {
            if options.timing {
                header.push_str(&format!(" (actual time=0.000..0.000 rows={rows} loops=1)"));
            } else {
                header.push_str(&format!(" (actual rows={rows} loops=1)"));
            }
        }
        out.push(header);
        for (label, value) in &n.details {
            out.push(format!("{}{label}: {value}", " ".repeat(2 + 6 * depth)));
        }
        if let (Some(n), Some(_)) = (n.heap_fetches, actual) {
            out.push(format!("{}Heap Fetches: {n}", " ".repeat(2 + 6 * depth)));
        }
        for c in &n.children {
            walk(c, depth + 1, options, None, out);
        }
    }
    walk(root, 0, options, actual_rows, &mut out);
    if options.summary {
        out.push("Planning Time: 0.000 ms".into());
        if actual_rows.is_some() {
            out.push("Execution Time: 0.000 ms".into());
        }
    }
    out
}

/// The `QUERY PLAN` as PostgreSQL's `FORMAT JSON`: one array holding one
/// object whose `Plan` is the tree.
/// The plan as the structured formats (JSON / YAML / XML) share it: the
/// `[{"Plan": {...}}]` value tree, keys in PostgreSQL's order.
fn structured(
    root: &PlanNode,
    options: &ExplainOptions,
    actual_rows: Option<usize>,
) -> serde_json_lite::Value {
    use serde_json_lite::Value;
    fn node(
        n: &PlanNode,
        options: &ExplainOptions,
        actual: Option<usize>,
        relationship: Option<&str>,
    ) -> Value {
        let mut fields: Vec<(String, Value)> = Vec::new();
        let node_type = n
            .name
            .split(" on ")
            .next()
            .unwrap_or(&n.name)
            .split(" using ")
            .next()
            .unwrap_or(&n.name)
            .to_string();
        let join_type = join_type_of(&node_type);
        let node_type = match join_type {
            Some((pname, _)) => pname.to_string(),
            None => node_type,
        };
        fields.push(("Node Type".into(), Value::Str(node_type.clone())));
        if let Some(r) = relationship {
            fields.push(("Parent Relationship".into(), Value::Str(r.into())));
        }
        fields.push(("Parallel Aware".into(), Value::Num("false".into())));
        fields.push(("Async Capable".into(), Value::Num("false".into())));
        // A join names its type after `Async Capable` (`ExplainNode`).
        if let Some((_, jt)) = join_type {
            fields.push(("Join Type".into(), Value::Str(jt.into())));
        }
        // An index scan reads its index forward unless an ORDER BY runs
        // it backward (`ExplainIndexScanDetails`).
        if node_type == "Index Scan" || node_type == "Index Only Scan" {
            fields.push(("Scan Direction".into(), Value::Str("Forward".into())));
        }
        for (k, v) in &n.props {
            fields.push((k.clone(), Value::Str(v.clone())));
        }
        // A scanned relation's alias is its name unless the plan named one.
        if n.props.iter().any(|(k, _)| k == "Relation Name")
            && !n.props.iter().any(|(k, _)| k == "Alias")
        {
            let rel = n
                .props
                .iter()
                .find(|(k, _)| k == "Relation Name")
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            fields.push(("Alias".into(), Value::Str(rel)));
        }
        // The costs come before the node's quals, as `ExplainNode` prints.
        if options.costs {
            let (startup, total, rows, width) = estimate_of(n);
            fields.push(("Startup Cost".into(), Value::Num(format!("{startup:.2}"))));
            fields.push(("Total Cost".into(), Value::Num(format!("{total:.2}"))));
            fields.push(("Plan Rows".into(), Value::Num(format!("{rows:.0}"))));
            fields.push(("Plan Width".into(), Value::Num(width.to_string())));
        }
        // The actuals follow the costs, before any node's quals
        // (`ExplainNode`).
        if let Some(rows) = actual {
            if options.timing {
                fields.push(("Actual Startup Time".into(), Value::Num("0.000".into())));
                fields.push(("Actual Total Time".into(), Value::Num("0.000".into())));
            }
            fields.push(("Actual Rows".into(), Value::Num(rows.to_string())));
            fields.push(("Actual Loops".into(), Value::Num("1".into())));
        }
        // Every join says whether its inner side is unique, before its
        // quals; the text format only under VERBOSE (`ExplainNode`).
        if join_type.is_some() {
            let b = if n.inner_unique { "true" } else { "false" };
            fields.push(("Inner Unique".into(), Value::Num(b.into())));
        }
        for (k, v) in &n.details {
            let value = if k.ends_with("Key") {
                Value::Arr(v.split(", ").map(|s| Value::Str(s.to_string())).collect())
            } else {
                Value::Str(v.clone())
            };
            fields.push((k.clone(), value));
        }
        // An index-only scan's instrumentation after its quals; the
        // structured formats print a zero count too (`show_instrumentation_count`).
        if let (Some(_), Some(f)) = (actual, n.heap_fetches) {
            fields.push((
                "Rows Removed by Index Recheck".into(),
                Value::Num("0".into()),
            ));
            fields.push(("Heap Fetches".into(), Value::Num(f.to_string())));
        }
        if !n.children.is_empty() {
            let kids = n
                .children
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let rel = if node_type == "Append" {
                        "Member"
                    } else if node_type == "Subquery Scan" {
                        "Subquery"
                    } else if i == 0 {
                        "Outer"
                    } else {
                        "Inner"
                    };
                    node(c, options, None, Some(rel))
                })
                .collect();
            fields.push(("Plans".into(), Value::Arr(kids)));
        }
        Value::Obj(fields)
    }
    let mut top = vec![("Plan".to_string(), node(root, options, actual_rows, None))];
    if options.summary {
        top.push(("Planning Time".into(), Value::Num("0.000".into())));
    }
    if actual_rows.is_some() {
        top.push(("Triggers".into(), Value::Arr(Vec::new())));
        if options.summary {
            top.push(("Execution Time".into(), Value::Num("0.000".into())));
        }
    }
    Value::Arr(vec![Value::Obj(top)])
}

/// `FORMAT JSON`.
pub(crate) fn render_json(
    root: &PlanNode,
    options: &ExplainOptions,
    actual_rows: Option<usize>,
) -> String {
    structured(root, options, actual_rows).pretty(0)
}

/// `FORMAT YAML`: PostgreSQL's layout -- a list of one mapping, a nested
/// mapping or list opening on a line of its own after `key: ` (with that
/// trailing blank), strings double-quoted, numbers and booleans bare.
pub(crate) fn render_yaml(
    root: &PlanNode,
    options: &ExplainOptions,
    actual_rows: Option<usize>,
) -> String {
    use serde_json_lite::Value;
    fn scalar(v: &Value) -> Option<String> {
        match v {
            Value::Str(s) => Some(serde_json_lite::quote(s)),
            Value::Num(n) => Some(n.clone()),
            _ => None,
        }
    }
    fn field(key: &str, v: &Value, col: usize, lead: &str, out: &mut Vec<String>) {
        match v {
            Value::Obj(fs) => {
                out.push(format!("{lead}{key}: "));
                for (k, v) in fs {
                    field(k, v, col + 2, &" ".repeat(col + 2), out);
                }
            }
            Value::Arr(items) => {
                out.push(format!("{lead}{key}: "));
                for item in items {
                    list_item(item, col + 2, out);
                }
            }
            other => out.push(format!(
                "{lead}{key}: {}",
                scalar(other).unwrap_or_default()
            )),
        }
    }
    fn list_item(item: &Value, col: usize, out: &mut Vec<String>) {
        let dash = format!("{}- ", " ".repeat(col));
        match item {
            Value::Obj(fs) => {
                for (i, (k, v)) in fs.iter().enumerate() {
                    let lead = if i == 0 {
                        dash.clone()
                    } else {
                        " ".repeat(col + 2)
                    };
                    field(k, v, col + 2, &lead, out);
                }
            }
            other => out.push(format!("{dash}{}", scalar(other).unwrap_or_default())),
        }
    }
    let mut out = Vec::new();
    if let Value::Arr(items) = structured(root, options, actual_rows) {
        for item in &items {
            list_item(item, 0, &mut out);
        }
    }
    out.join("\n")
}

/// `FORMAT XML`: the `explain` document PostgreSQL writes, keys with their
/// blanks as hyphens, a list's items as `<Item>` (plans as `<Plan>`).
pub(crate) fn render_xml(
    root: &PlanNode,
    options: &ExplainOptions,
    actual_rows: Option<usize>,
) -> String {
    use serde_json_lite::Value;
    fn escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
    fn element(tag: &str, v: &Value, depth: usize, out: &mut Vec<String>) {
        let pad = " ".repeat(depth * 2);
        let tag = tag.replace(' ', "-");
        match v {
            Value::Str(s) => out.push(format!("{pad}<{tag}>{}</{tag}>", escape(s))),
            Value::Num(n) => out.push(format!("{pad}<{tag}>{n}</{tag}>")),
            Value::Obj(fs) => {
                out.push(format!("{pad}<{tag}>"));
                for (k, v) in fs {
                    element(k, v, depth + 1, out);
                }
                out.push(format!("{pad}</{tag}>"));
            }
            Value::Arr(items) => {
                out.push(format!("{pad}<{tag}>"));
                let item_tag = if tag == "Plans" { "Plan" } else { "Item" };
                for item in items {
                    element(item_tag, item, depth + 1, out);
                }
                out.push(format!("{pad}</{tag}>"));
            }
        }
    }
    let mut out = vec!["<explain xmlns=\"http://www.postgresql.org/2009/explain\">".to_string()];
    if let Value::Arr(items) = structured(root, options, actual_rows) {
        for item in &items {
            element("Query", item, 1, &mut out);
        }
    }
    out.push("</explain>".into());
    out.join("\n")
}

/// The little JSON writer `FORMAT JSON` needs, laid out as PostgreSQL lays it
/// out (two-space indent, `"key": value`).
mod serde_json_lite {
    pub enum Value {
        Str(String),
        Num(String),
        Arr(Vec<Value>),
        Obj(Vec<(String, Value)>),
    }

    pub fn quote(s: &str) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    impl Value {
        pub fn pretty(&self, indent: usize) -> String {
            let pad = " ".repeat(indent);
            let inner = " ".repeat(indent + 2);
            match self {
                Value::Str(s) => quote(s),
                Value::Num(n) => n.clone(),
                // An empty group (`Triggers`), opened and closed on lines
                // of their own as `ExplainOpenGroup` / `ExplainCloseGroup`
                // write it; no property list here is ever empty.
                Value::Arr(items) if items.is_empty() => format!("[\n{pad}]"),
                Value::Arr(items)
                    if items
                        .iter()
                        .all(|v| matches!(v, Value::Str(_) | Value::Num(_))) =>
                {
                    let parts: Vec<String> = items.iter().map(|v| v.pretty(indent)).collect();
                    format!("[{}]", parts.join(", "))
                }
                Value::Arr(items) => {
                    let parts: Vec<String> = items
                        .iter()
                        .map(|v| format!("{inner}{}", v.pretty(indent + 2)))
                        .collect();
                    format!("[\n{}\n{pad}]", parts.join(",\n"))
                }
                Value::Obj(fields) => {
                    let parts: Vec<String> = fields
                        .iter()
                        .map(|(k, v)| format!("{inner}{}: {}", quote(k), v.pretty(indent + 2)))
                        .collect();
                    format!("{{\n{}\n{pad}}}", parts.join(",\n"))
                }
            }
        }
    }
}

/// Put a single-relation statement's WHERE on its scan, as PostgreSQL shows
/// it: the conjuncts an index scan's key constrains as `Index Cond`, the
/// rest as `Filter`. A plan with more than one scan is left alone.
pub(crate) fn attach_scan_quals(
    root: &mut PlanNode,
    quals: &[(String, Option<String>)],
    index_columns: &dyn Fn(&str) -> Vec<String>,
) {
    if quals.is_empty() {
        return;
    }
    fn scans<'a>(n: &'a mut PlanNode, out: &mut Vec<&'a mut PlanNode>) {
        if is_scan_name(&n.name) {
            out.push(n);
            return;
        }
        for c in &mut n.children {
            scans(c, out);
        }
    }
    let mut found = Vec::new();
    scans(root, &mut found);
    let [scan] = found.as_mut_slice() else {
        return;
    };
    quals_on_scan(scan, quals, index_columns);
}

/// Gate a single-relation plan's scan with the `false` pseudoconstants its
/// equivalence classes generated (RP-350): a `Result` directly over the
/// scan, as `create_gating_plan` builds it, printing `One-Time Filter:
/// false` (one) or `(false AND false ...)`. A constant qual costs nothing,
/// so the Result carries the scan's estimate. A plan with more than one
/// scan is left alone.
pub(crate) fn gate_scan(root: &mut PlanNode, falses: usize) {
    if falses == 0 {
        return;
    }
    fn scans<'a>(n: &'a mut PlanNode, out: &mut Vec<&'a mut PlanNode>) {
        if is_scan_name(&n.name) {
            out.push(n);
            return;
        }
        for c in &mut n.children {
            scans(c, out);
        }
    }
    let mut found = Vec::new();
    scans(root, &mut found);
    let [scan] = found.as_mut_slice() else {
        return;
    };
    let filter = if falses == 1 {
        "false".to_string()
    } else {
        format!("({})", vec!["false"; falses].join(" AND "))
    };
    let child = std::mem::replace(&mut **scan, PlanNode::new("Result"));
    scan.estimate = child.estimate;
    scan.details.push(("One-Time Filter".into(), filter));
    scan.children.push(child);
}

/// Put a set operation's per-arm WHERE conjuncts on its scans, left to
/// right, as PostgreSQL prints each arm's own `Filter` / `Index Cond`
/// (RP-346). A plan whose scans do not pair one-to-one with the arms is
/// left alone.
pub(crate) fn attach_setop_quals(
    root: &mut PlanNode,
    arms: &[Vec<(String, Option<String>)>],
    index_columns: &dyn Fn(&str) -> Vec<String>,
) {
    if arms.iter().all(Vec::is_empty) {
        return;
    }
    fn scans<'a>(n: &'a mut PlanNode, out: &mut Vec<&'a mut PlanNode>) {
        if is_scan_name(&n.name) {
            out.push(n);
            return;
        }
        for c in &mut n.children {
            scans(c, out);
        }
    }
    let mut found = Vec::new();
    scans(root, &mut found);
    if found.len() != arms.len() {
        return;
    }
    for (scan, quals) in found.into_iter().zip(arms) {
        if !quals.is_empty() {
            quals_on_scan(scan, quals, index_columns);
        }
    }
}

fn quals_on_scan(
    scan: &mut PlanNode,
    quals: &[(String, Option<String>)],
    index_columns: &dyn Fn(&str) -> Vec<String>,
) {
    // The printed WHERE replaces the placeholder a residual left (RP-367).
    scan.details
        .retain(|(k, v)| !(k == "Filter" && v == "(...)"));
    let bitmap = scan.name.starts_with("Bitmap Heap Scan on ");
    let index_node = if bitmap {
        scan.children.first()
    } else {
        Some(&*scan)
    };
    let keys = index_node
        .and_then(|n| n.props.iter().find(|(k, _)| k == "Index Name"))
        .map(|(_, v)| index_columns(v))
        .unwrap_or_default();
    let (cond, filter): (Vec<&String>, Vec<&String>) = {
        let mut cond = Vec::new();
        let mut filter = Vec::new();
        for (text, col) in quals {
            if col.as_ref().is_some_and(|c| keys.contains(c)) {
                cond.push(text);
            } else {
                filter.push(text);
            }
        }
        (cond, filter)
    };
    let join = |parts: &[&String]| -> String {
        match parts {
            [one] => (*one).clone(),
            many => format!(
                "({})",
                many.iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" AND ")
            ),
        }
    };
    // A Bitmap Heap Scan rechecks its index quals on each heap row
    // (`Recheck Cond`, its `bitmapqualorig`); its Bitmap Index Scan child
    // holds them as `Index Cond` (`create_bitmap_scan_plan`).
    if !cond.is_empty() {
        if bitmap {
            scan.details.push(("Recheck Cond".into(), join(&cond)));
            if let Some(child) = scan.children.first_mut() {
                child.details.push(("Index Cond".into(), join(&cond)));
            }
        } else {
            scan.details.push(("Index Cond".into(), join(&cond)));
        }
    }
    if !filter.is_empty() {
        scan.details.push(("Filter".into(), join(&filter)));
    }
}

/// The Bitmap Index Scan under a Bitmap Heap Scan (RP-338): it names its
/// index and no relation, and prints `estimate`, width 0.
pub(crate) fn bitmap_index_scan(index: &str, estimate: crate::scan_estimate::Estimate) -> PlanNode {
    let mut n = PlanNode::new(format!("Bitmap Index Scan on {index}"));
    n.props.push(("Index Name".into(), index.to_string()));
    n.estimate = Some(estimate);
    n
}

/// Put each join's ON condition on its join node (post-order, as
/// `join_conditions` lists them): `Hash Cond` on a hash join, `Join Filter`
/// on a nested loop.
pub(crate) fn attach_join_conds(root: &mut PlanNode, conds: &[String]) {
    fn walk(n: &mut PlanNode, conds: &[String], next: &mut usize) {
        for c in &mut n.children {
            walk(c, conds, next);
        }
        let label = if n.name.starts_with("Hash") && n.name.ends_with("Join") {
            "Hash Cond"
        } else if n.name.starts_with("Nested Loop") {
            "Join Filter"
        } else {
            return;
        };
        if let Some(c) = conds.get(*next) {
            if !c.is_empty() && !n.skip_cond {
                let c = if n.commute_cond {
                    commute_equality(c).unwrap_or_else(|| c.clone())
                } else {
                    c.clone()
                };
                n.details.insert(0, (label.into(), c));
            }
        }
        *next += 1;
    }
    if conds.is_empty() {
        return;
    }
    let mut next = 0;
    walk(root, conds, &mut next);
}

/// Give each scan an equivalence class filters (RP-363, RP-364) the
/// constant the other side's WHERE compares the other key with. The class
/// re-derives both equalities (`generate_base_implied_equalities_const`),
/// so on each side the key's equality comes after the scan's own quals,
/// all in one `Filter`. A filter that is not a plain AND of
/// parenthesised conjuncts is left as it is.
/// `(own key column, (alias, key column) whose WHERE fixed the other
/// key)`: a scan an equivalence class filters by a constant (RP-363).
pub(crate) type ImpliedFilter = (String, (String, String));

/// A leaf's alias, its implied filter, and its printed `Filter`.
type LeafFilters = (Option<String>, Option<ImpliedFilter>, Option<String>);

pub(crate) fn apply_implied_filters(root: &mut PlanNode) {
    fn alias_of(n: &PlanNode) -> Option<String> {
        n.props
            .iter()
            .find(|(k, _)| k == "Alias")
            .or_else(|| n.props.iter().find(|(k, _)| k == "Relation Name"))
            .map(|(_, v)| v.clone())
    }
    // The conjuncts of a printed Filter, each parenthesised.
    fn conjuncts(text: &str) -> Option<Vec<String>> {
        if let Some(inner) = text.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
            if inner.starts_with('(') && inner.ends_with(')') && inner.contains(") AND (") {
                return Some(inner.split(" AND ").map(str::to_string).collect());
            }
        }
        (text.starts_with('(') && text.ends_with(')') && !text.contains(" AND "))
            .then(|| vec![text.to_string()])
    }
    fn render(parts: &[String]) -> String {
        if parts.len() == 1 {
            parts[0].clone()
        } else {
            format!("({})", parts.join(" AND "))
        }
    }
    let mut found: Vec<LeafFilters> = Vec::new();
    fn collect(n: &PlanNode, out: &mut Vec<LeafFilters>) {
        if n.children.is_empty() {
            let filter = n
                .details
                .iter()
                .find(|(k, _)| k == "Filter")
                .map(|(_, v)| v.clone());
            out.push((alias_of(n), n.implied_filter.clone(), filter));
        }
        for c in &n.children {
            collect(c, out);
        }
    }
    collect(root, &mut found);
    // The new Filter text for each alias.
    let mut texts: Vec<(String, String)> = Vec::new();
    for (alias, implied, filter) in &found {
        let (Some(alias), Some((own_col, (from_alias, from_col)))) = (alias, implied) else {
            continue;
        };
        let Some((_, _, Some(src))) = found
            .iter()
            .find(|(a, _, _)| a.as_deref() == Some(from_alias))
        else {
            continue;
        };
        let Some(mut src_parts) = conjuncts(src) else {
            continue;
        };
        let head = format!("({from_col} = ");
        let Some(at) = src_parts.iter().position(|p| p.starts_with(&head)) else {
            continue;
        };
        let key = src_parts.remove(at);
        let rhs = key[head.len()..].to_string();
        src_parts.push(key);
        texts.push((from_alias.clone(), render(&src_parts)));
        let mut own = match filter {
            Some(f) => match conjuncts(f) {
                Some(p) => p,
                None => continue,
            },
            None => Vec::new(),
        };
        own.push(format!("({own_col} = {rhs}"));
        texts.push((alias.clone(), render(&own)));
    }
    fn apply(n: &mut PlanNode, texts: &[(String, String)]) {
        if n.children.is_empty() {
            if let Some(alias) = alias_of(n) {
                if let Some((_, text)) = texts.iter().find(|(a, _)| *a == alias) {
                    match n.details.iter_mut().find(|(k, _)| k == "Filter") {
                        Some((_, f)) => *f = text.clone(),
                        None => n.details.push(("Filter".into(), text.clone())),
                    }
                }
            }
            n.implied_filter = None;
        }
        for c in &mut n.children {
            apply(c, texts);
        }
    }
    apply(root, &texts);
}

/// Make a narrow-path `Hash Join` whose clause an equivalence class
/// replaced (RP-363) the Nested Loop over the two scans it is: the Hash
/// removed, the ON's condition not printed, and each scan named in
/// `implied` (`(alias, key column)`) filtered by the constant too.
pub(crate) fn implied_nest_loop(
    root: &mut PlanNode,
    implied: &[(String, String)],
    from: &(String, String),
) {
    if root.children.len() != 2 || root.children[1].name != "Hash" {
        return;
    }
    let mut hash = root.children.pop().expect("two children");
    let right = hash.children.pop().expect("one child");
    root.children.push(right);
    root.name = "Nested Loop".into();
    root.skip_cond = true;
    root.inner_unique = false;
    root.unique_probe = None;
    mark_implied_filters(root, implied, from);
}

/// Mark the scans (anywhere under `root`) whose alias `implied` names
/// (`(alias, key column)`) to take `from`'s constant as a Filter too
/// ([`apply_implied_filters`], RP-363, RP-364).
pub(crate) fn mark_implied_filters(
    root: &mut PlanNode,
    implied: &[(String, String)],
    from: &(String, String),
) {
    if root.children.is_empty() {
        let alias = root
            .props
            .iter()
            .find(|(k, _)| k == "Alias")
            .or_else(|| root.props.iter().find(|(k, _)| k == "Relation Name"))
            .map(|(_, v)| v.clone());
        if let Some((_, col)) = implied.iter().find(|(a, _)| Some(a) == alias.as_ref()) {
            root.implied_filter = Some((col.clone(), from.clone()));
        }
        return;
    }
    for c in &mut root.children {
        mark_implied_filters(c, implied, from);
    }
}

/// Place a joined SELECT's WHERE conjuncts: one naming a single relation
/// that no outer join null-extends filters that relation's scan; any other
/// filters the top join.
pub(crate) fn attach_join_where(root: &mut PlanNode, quals: &[(String, Option<String>)]) {
    if quals.is_empty() {
        return;
    }
    fn is_scan(n: &PlanNode) -> bool {
        is_scan_name(&n.name)
    }
    fn scan_for<'a>(n: &'a mut PlanNode, rel: &str) -> Option<&'a mut PlanNode> {
        if is_scan(n) {
            let name = n
                .props
                .iter()
                .find(|(k, _)| k == "Alias")
                .or_else(|| n.props.iter().find(|(k, _)| k == "Relation Name"))
                .map(|(_, v)| v.clone());
            return (name.as_deref() == Some(rel)).then_some(n);
        }
        n.children.iter_mut().find_map(|c| scan_for(c, rel))
    }
    fn top_join(n: &mut PlanNode) -> Option<&mut PlanNode> {
        if n.name.contains("Join") || n.name.starts_with("Nested Loop") {
            return Some(n);
        }
        n.children.iter_mut().find_map(top_join)
    }
    let combine = |parts: &[&String]| -> String {
        match parts {
            [one] => (*one).clone(),
            many => format!(
                "({})",
                many.iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" AND ")
            ),
        }
    };
    let mut homes: Vec<(Option<String>, Vec<&String>)> = Vec::new();
    for (text, home) in quals {
        match homes.iter_mut().find(|(h, _)| h == home) {
            Some((_, v)) => v.push(text),
            None => homes.push((home.clone(), vec![text])),
        }
    }
    for (home, parts) in homes {
        let target = match &home {
            Some(rel) => scan_for(root, rel),
            None => top_join(root),
        };
        if let Some(n) = target {
            // Over an inner join a WHERE conjunct is a join qual.
            let inner = home.is_none()
                && (n.name == "Hash Join" || n.name == "Nested Loop" || n.name == "Merge Join");
            let label = if inner { "Join Filter" } else { "Filter" };
            n.details.push((label.into(), combine(&parts)));
        }
    }
}

#[cfg(test)]
mod cost_tests {
    use super::*;

    #[test]
    fn a_node_prints_its_estimate_or_zeros() {
        let mut n = PlanNode::new("Seq Scan on t");
        assert_eq!(costs_text(&n), "  (cost=0.00..0.00 rows=0 width=0)");
        // PostgreSQL 15.19's `Seq Scan on pg_class` through a filter; an
        // empty sum's -0.0 prints as 0.00.
        n.estimate = Some((-0.0, 19.125, 140.0, 265));
        assert_eq!(costs_text(&n), "  (cost=0.00..19.12 rows=140 width=265)");
    }
}

#[cfg(test)]
mod setop_quals_tests {
    use super::*;

    fn append() -> PlanNode {
        let mut root = PlanNode::new("Append");
        root.children.push(PlanNode::new("Seq Scan on e"));
        root.children.push(PlanNode::new("Seq Scan on k"));
        root
    }

    #[test]
    fn each_arm_filters_its_own_scan() {
        let mut root = append();
        let arms = vec![
            vec![("(a < 100)".to_string(), Some("a".to_string()))],
            vec![],
        ];
        attach_setop_quals(&mut root, &arms, &|_| Vec::new());
        assert_eq!(
            root.children[0].details,
            vec![("Filter".to_string(), "(a < 100)".to_string())]
        );
        assert!(root.children[1].details.is_empty());
    }

    #[test]
    fn unpaired_scans_are_left_alone() {
        let mut root = append();
        let arms = vec![vec![("(a < 100)".to_string(), None)]];
        attach_setop_quals(&mut root, &arms, &|_| Vec::new());
        assert!(root.children.iter().all(|c| c.details.is_empty()));
    }
}

#[cfg(test)]
mod join_type_tests {
    use super::*;

    /// An equivalence class's filter (RP-363) copies the other scan's
    /// constant onto this scan's key, ANDed after its own quals.
    #[test]
    fn implied_filters_and_after_own_quals() {
        let scan = |alias: &str, filter: Option<&str>, implied: Option<(&str, &str)>| {
            let mut n = PlanNode::new(format!("Seq Scan on {alias}"));
            n.props.push(("Relation Name".into(), alias.into()));
            if let Some(f) = filter {
                n.details.push(("Filter".into(), f.into()));
            }
            n.implied_filter = implied.map(|(c, f)| (c.into(), (f.into(), "b".into())));
            n
        };
        let mut root = PlanNode::new("Nested Loop");
        root.children
            .push(scan("e", Some("(b = 'v2'::text)"), None));
        root.children
            .push(scan("k", Some("(a < 500)"), Some(("c", "e"))));
        apply_implied_filters(&mut root);
        assert_eq!(
            root.children[1].details,
            vec![(
                "Filter".to_string(),
                "((a < 500) AND (c = 'v2'::text))".to_string()
            )]
        );
        let mut root = PlanNode::new("Nested Loop");
        root.children
            .push(scan("e", Some("(b = 'v2'::text)"), None));
        root.children.push(scan("k", None, Some(("b", "e"))));
        apply_implied_filters(&mut root);
        assert_eq!(root.children[1].details[0].1, "(b = 'v2'::text)");
    }

    /// A swapped hash join's `Hash Cond` is written outer side first
    /// (RP-359); anything but one plain equality is left as it was.
    #[test]
    fn commute_equality_swaps_one_equality() {
        assert_eq!(
            commute_equality("(e.b = k.b)").as_deref(),
            Some("(k.b = e.b)")
        );
        assert_eq!(commute_equality("((e.a = k.a) AND (e.b = k.b))"), None);
        assert_eq!(commute_equality("e.b = k.b"), None);
    }

    #[test]
    fn a_join_name_splits_into_node_and_join_type() {
        // `ExplainNode` builds the text name from the two (RP-353).
        assert_eq!(join_type_of("Nested Loop"), Some(("Nested Loop", "Inner")));
        assert_eq!(
            join_type_of("Nested Loop Left Join"),
            Some(("Nested Loop", "Left"))
        );
        assert_eq!(join_type_of("Hash Join"), Some(("Hash Join", "Inner")));
        assert_eq!(join_type_of("Hash Full Join"), Some(("Hash Join", "Full")));
        assert_eq!(
            join_type_of("Merge Anti Join"),
            Some(("Merge Join", "Anti"))
        );
        assert_eq!(join_type_of("Hash"), None);
        assert_eq!(join_type_of("HashAggregate"), None);
        assert_eq!(join_type_of("Seq Scan"), None);
    }

    #[test]
    fn a_constant_equality_fixes_its_field() {
        let mut out = Vec::new();
        let f = bson::doc! {
            "a": 3, "b": {"$eq": 4}, "c": {"$gt": 1}, "d": null,
            "$and": [{"e": "x"}],
        };
        constant_equalities(&f, &mut out);
        assert_eq!(out, ["a", "b", "e"]);
    }
}

/// A relation scan's node name: a Seq Scan, or an Index Scan or Index Only
/// Scan over one of its indexes.
fn is_scan_name(name: &str) -> bool {
    name.starts_with("Seq Scan on ")
        || name.starts_with("Bitmap Heap Scan on ")
        || name.starts_with("Index Scan using ")
        || name.starts_with("Index Only Scan using ")
}

#[cfg(test)]
mod bitmap_scan_tests {
    use super::*;

    /// A Bitmap Heap Scan rechecks its index quals and its Bitmap Index
    /// Scan holds them as its Index Cond; a qual on another column stays
    /// the heap's Filter (RP-338).
    #[test]
    fn a_bitmap_heap_scan_splits_its_quals_as_postgresql_does() {
        let mut heap = PlanNode::new("Bitmap Heap Scan on pg_class");
        heap.children.push(bitmap_index_scan(
            "pg_class_relname_nsp_index",
            (0.0, 4.28, 1.0, 0),
        ));
        let quals = vec![
            (
                "(relname = 'pg_class'::name)".to_string(),
                Some("relname".to_string()),
            ),
            (
                "(relkind = 'r'::\"char\")".to_string(),
                Some("relkind".to_string()),
            ),
        ];
        attach_scan_quals(&mut heap, &quals, &|_| {
            vec!["relname".into(), "relnamespace".into()]
        });
        assert_eq!(
            heap.details,
            vec![
                (
                    "Recheck Cond".to_string(),
                    "(relname = 'pg_class'::name)".to_string()
                ),
                (
                    "Filter".to_string(),
                    "(relkind = 'r'::\"char\")".to_string()
                ),
            ]
        );
        assert_eq!(
            heap.children[0].details,
            vec![(
                "Index Cond".to_string(),
                "(relname = 'pg_class'::name)".to_string()
            )]
        );
        assert!(heap.children[0]
            .props
            .iter()
            .all(|(k, _)| k != "Relation Name"));
    }
}

#[cfg(test)]
mod heap_fetches_tests {
    use super::*;

    fn options(format: &str) -> ExplainOptions {
        ExplainOptions {
            analyze: true,
            verbose: false,
            costs: false,
            timing: false,
            summary: false,
            format: format.into(),
            scan_quals: Vec::new(),
            scan_in_lengths: Vec::new(),
            setop_quals: Vec::new(),
            join_conds: Vec::new(),
            join_where: Vec::new(),
            subquery_quals: Vec::new(),
            one_time_false: 0,
        }
    }

    fn index_only_scan() -> PlanNode {
        let mut n = PlanNode::new("Index Only Scan using i on pg_attribute");
        n.details
            .push(("Index Cond".into(), "(attrelid = '1259'::oid)".into()));
        n.heap_fetches = Some(0);
        n
    }

    #[test]
    fn heap_fetches_follow_the_quals_under_analyze() {
        // PostgreSQL 15.19 (RP-360).
        let n = index_only_scan();
        assert_eq!(
            render_text(&n, &options("text"), Some(39)),
            vec![
                "Index Only Scan using i on pg_attribute (actual rows=39 loops=1)",
                "  Index Cond: (attrelid = '1259'::oid)",
                "  Heap Fetches: 0",
            ]
        );
        assert_eq!(render_text(&n, &options("text"), None).len(), 2);
        // The structured formats: the actuals after the costs, then the
        // quals, the recheck count and the fetches.
        let json = render_json(&n, &options("json"), Some(39));
        let at = |k: &str| json.find(k).unwrap_or_else(|| panic!("{k} in {json}"));
        assert!(at("\"Actual Loops\"") < at("\"Index Cond\""));
        assert!(at("\"Index Cond\"") < at("\"Rows Removed by Index Recheck\": 0"));
        assert!(at("\"Rows Removed by Index Recheck\"") < at("\"Heap Fetches\": 0"));
    }
}
