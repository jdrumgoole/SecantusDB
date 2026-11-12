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
    /// A nested loop's inner index scan parameterized by the outer row
    /// (RP-354): `(alias, column)` whose comparison with the outer side
    /// is its `Index Cond`, taken from the join's filter.
    pub param_cond: Option<(String, String)>,
    /// Under ANALYZE, a Bitmap Heap Scan's `Heap Blocks: exact=` (its
    /// `exact_pages`): the heap pages its bitmap named (RP-383).
    pub heap_blocks: Option<usize>,
    /// Under ANALYZE, a child node's own `actual rows` (a Bitmap Index
    /// Scan's index tuples, RP-383); a child without one prints none.
    pub actual_rows: Option<usize>,
    /// Under ANALYZE, an index scan's `Rows Removed by Filter` (its
    /// `nfiltered1`): the rows its Index Cond fetched that its Filter
    /// rejected (RP-384).
    pub removed_by_filter: Option<usize>,
    /// Under ANALYZE, how many times a child ran (`nloops`), where it is
    /// not once: a nested loop's inner side, rescanned per outer row
    /// (RP-386). Its `actual_rows` is then the per-loop average.
    pub actual_loops: Option<usize>,
    /// Under ANALYZE, a join's `Rows Removed by Join Filter` (its
    /// `nfiltered1`): the pairs its Join Filter rejected (RP-386).
    pub join_filter_removed: Option<usize>,
    /// Under ANALYZE, a Hash node's table as `show_hash_info` prints it
    /// (RP-386).
    pub hash_info: Option<HashInfo>,
}

/// A hash join's hash table after it was built (`HashInstrumentation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashInfo {
    pub buckets: usize,
    pub original_buckets: usize,
    pub batches: usize,
    pub original_batches: usize,
    /// `spacePeak`, in bytes.
    pub peak_bytes: usize,
}

impl HashInfo {
    /// `spacePeak` in kB, rounded up (`BYTES_TO_KILOBYTES`).
    pub(crate) fn peak_kb(&self) -> usize {
        self.peak_bytes.div_ceil(1024)
    }

    /// `show_hash_info`'s text line.
    pub(crate) fn text(&self) -> String {
        let kb = self.peak_kb();
        if self.batches != self.original_batches {
            format!(
                "Buckets: {} (originally {})  Batches: {} (originally {})  Memory Usage: {kb}kB",
                self.buckets, self.original_buckets, self.batches, self.original_batches
            )
        } else if self.buckets != self.original_buckets {
            format!(
                "Buckets: {} (originally {})  Batches: {}  Memory Usage: {kb}kB",
                self.buckets, self.original_buckets, self.batches
            )
        } else {
            format!(
                "Buckets: {}  Batches: {}  Memory Usage: {kb}kB",
                self.buckets, self.batches
            )
        }
    }
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
            heap_blocks: None,
            actual_rows: None,
            removed_by_filter: None,
            actual_loops: None,
            join_filter_removed: None,
            hash_info: None,
            skip_cond: false,
            implied_filter: None,
            param_cond: None,
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
                node.details
                    .push(("Filter".into(), FILTER_PLACEHOLDER.into()));
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
pub(crate) fn constant_equalities(filter: &Document, out: &mut Vec<String>) {
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
    if !matches!(root.name.as_str(), "Nested Loop" | "Nested Loop Left Join")
        || root.children.len() != 2
    {
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

/// Make a priced `Nested Loop`'s inner scan the index scan parameterized
/// by the outer row the cost model chose (RP-354): its name and index,
/// and the column whose join clause [`move_param_conds`] makes its
/// `Index Cond`.
pub(crate) fn param_index_inner(root: &mut PlanNode, p: &crate::scan_estimate::ParamScan) {
    let Some(inner) = root.children.get_mut(1) else {
        return;
    };
    let Some(table) = inner.name.strip_prefix("Seq Scan on ") else {
        return;
    };
    let kind = if p.index_only {
        "Index Only Scan"
    } else {
        "Index Scan"
    };
    inner.name = format!("{kind} using {} on {table}", p.index);
    inner.props.retain(|(k, _)| k != "Index Name");
    inner
        .props
        .insert(0, ("Index Name".into(), p.index.clone()));
    inner.param_cond = Some((p.alias.clone(), p.column.clone()));
}

/// Move each nested loop's join clause that its inner index scan takes as
/// a parameterized index qual from the join's `Join Filter` to the scan's
/// `Index Cond`, written inner column first and unqualified, its operator
/// commuted where the clause named the outer side first
/// (`fix_indexqual_references`, RP-354). The other conjuncts stay the
/// join's filter.
pub(crate) fn move_param_conds(root: &mut PlanNode) {
    for c in &mut root.children {
        move_param_conds(c);
    }
    if !root.name.starts_with("Nested Loop") || root.children.len() != 2 {
        return;
    }
    let Some((alias, column)) = root.children[1].param_cond.clone() else {
        return;
    };
    let Some(at) = root.details.iter().position(|(k, _)| k == "Join Filter") else {
        return;
    };
    let text = root.details[at].1.clone();
    let conjuncts = split_conjuncts(&text);
    // Every clause bounding the column is an index qual, in the join's
    // order: two bounds print as one `Index Cond` of both.
    let own = format!("{alias}.{column}");
    let mut conds = Vec::new();
    let mut rest = Vec::new();
    for c in conjuncts {
        match index_cond_text(&c, &own, &column) {
            Some(moved) => conds.push(moved),
            None => rest.push(c),
        }
    }
    let cond = match conds.as_slice() {
        [] => return,
        [one] => one.clone(),
        many => format!("({})", many.join(" AND ")),
    };
    // The other join clauses move into the scan too
    // (`join_clause_is_movable_into`): its `Filter`, its own columns
    // unqualified. One that reads no inner column -- a LEFT join's ON
    // restriction of its outer side -- cannot move, and stays the join's
    // `Join Filter`.
    let (rest, kept): (Vec<String>, Vec<String>) =
        rest.into_iter().partition(|c| mentions_alias(c, &alias));
    match kept.as_slice() {
        [] => {
            root.details.remove(at);
        }
        [one] => root.details[at].1 = one.clone(),
        many => root.details[at].1 = format!("({})", many.join(" AND ")),
    }
    let inner = &mut root.children[1];
    inner.details.insert(0, ("Index Cond".into(), cond));
    // A clause an index qual implies is not checked again
    // (`create_indexscan_plan`'s `predicate_implied_by`), though it was
    // costed: a strict `<` or `>` implies `<>` of the same two operands.
    let implied = |c: &str| -> bool {
        let operands = |t: &str, ops: &[&str]| -> Option<(String, String)> {
            let body = t.strip_prefix('(')?.strip_suffix(')')?;
            ops.iter().find_map(|op| {
                let (l, r) = body.split_once(&format!(" {op} "))?;
                Some((l.to_string(), r.to_string()))
            })
        };
        let Some((l, r)) = operands(c, &["<>"]) else {
            return false;
        };
        conds.iter().any(|q| {
            matches!(operands(q, &["<", ">"]), Some((cl, cr)) if (l == cl && r == cr) || (l == cr && r == cl))
        })
    };
    let rest: Vec<String> = rest
        .iter()
        .map(|c| unqualify(c, &alias))
        .filter(|c| !implied(c))
        .collect();
    if !rest.is_empty() {
        drop_filter_placeholder(inner);
    }
    match rest.as_slice() {
        [] => {}
        [one] => inner.details.insert(1, ("Filter".into(), one.clone())),
        many => inner
            .details
            .insert(1, ("Filter".into(), format!("({})", many.join(" AND ")))),
    }
}

/// Whether `text` names a column qualified by `alias` outside its string
/// literals.
fn mentions_alias(text: &str, alias: &str) -> bool {
    let prefix = format!("{alias}.");
    let mut quoted = false;
    let mut prev: Option<char> = None;
    for (i, c) in text.char_indices() {
        if c == '\'' {
            quoted = !quoted;
        } else if !quoted
            && text[i..].starts_with(&prefix)
            && prev.is_none_or(|p| !(p.is_alphanumeric() || p == '_' || p == '.' || p == '"'))
        {
            return true;
        }
        prev = Some(c);
    }
    false
}

/// `text` with each `alias.` qualifier dropped, as a scan prints its own
/// relation's columns.
fn unqualify(text: &str, alias: &str) -> String {
    let prefix = format!("{alias}.");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(&prefix) {
        let boundary = rest[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.' || c == '"'));
        out.push_str(&rest[..at]);
        if !boundary {
            out.push_str(&prefix);
        }
        rest = &rest[at + prefix.len()..];
    }
    out.push_str(rest);
    out
}

/// A `Join Filter`'s conjuncts: `((x) AND (y))` is `(x)` and `(y)`; one
/// conjunct is itself.
pub(crate) fn split_conjuncts(text: &str) -> Vec<String> {
    let Some(body) = text.strip_prefix('(').and_then(|t| t.strip_suffix(')')) else {
        return vec![text.to_string()];
    };
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b' ' if depth == 0 && body[i..].starts_with(" AND ") => {
                parts.push(body[start..i].to_string());
                i += 5;
                start = i;
                continue;
            }
            _ => {}
        }
        if depth < 0 {
            return vec![text.to_string()];
        }
        i += 1;
    }
    parts.push(body[start..].to_string());
    if parts.len() == 1 {
        return vec![text.to_string()];
    }
    if parts.iter().all(|p| p.starts_with('(') && p.ends_with(')')) {
        parts
    } else {
        vec![text.to_string()]
    }
}

/// `(own op other)` or `(other op own)` as an index scan's `Index Cond`:
/// `(column op other)`, the operator commuted for the second.
fn index_cond_text(c: &str, own: &str, column: &str) -> Option<String> {
    let body = c.strip_prefix('(')?.strip_suffix(')')?;
    for op in ["<=", ">=", "<", ">"] {
        let sep = format!(" {op} ");
        let Some((l, r)) = body.split_once(&sep) else {
            continue;
        };
        if r.contains(&sep) {
            return None;
        }
        if l == own && r != own {
            return Some(format!("({column} {op} {r})"));
        }
        if r == own && l != own {
            let flipped = match op {
                "<" => ">",
                "<=" => ">=",
                ">" => "<",
                _ => "<=",
            };
            return Some(format!("({column} {flipped} {l})"));
        }
        return None;
    }
    None
}

/// A two-table join's keys as a Sort Key prints them, `(left, right)`:
/// `alias.column` each (RP-366). From the narrow path's ON or the joined
/// rows' one equality; `None` for anything else.
pub(crate) fn join_key_texts(sel: &secantus_pgplan::Select) -> Option<(String, String)> {
    if let Some(j) = &sel.join {
        let (x, y) = j.on.as_ref()?;
        let (l, r) = if x.0 == j.left.1 { (x, y) } else { (y, x) };
        return Some((format!("{}.{}", l.0, l.1), format!("{}.{}", r.0, r.1)));
    }
    let sub = sel.sub.as_ref()?;
    let Statement::JoinRows(rows) = sub.plan.as_ref() else {
        return None;
    };
    let JoinNode::Join { equi, .. } = &rows.tree else {
        return None;
    };
    let [(l, r)] = equi.as_slice() else {
        return None;
    };
    let dotted = |k: &str| k.replace(secantus_pgplan::joins::SEP, ".");
    Some((dotted(l), dotted(r)))
}

/// Make the `Hash Join` this server drew the merge join PostgreSQL keeps
/// (RP-366): each input under a `Sort` on its key, the inner read through
/// a `Materialize` when the plan says so, and the join's, Sorts' and scans'
/// estimates. `b_outer` puts the right table outer (a LEFT join then the
/// `Merge Right Join` of the two the other way round); `outer` and `inner`
/// are the scans' estimates in that order; `keys` are the left and right
/// tables' key texts.
pub(crate) fn price_merge_join(
    root: &mut PlanNode,
    outer: crate::scan_estimate::Estimate,
    inner: crate::scan_estimate::Estimate,
    join: crate::scan_estimate::Estimate,
    plan: &secantus_pgplan::joincost::MergePlan,
    b_outer: bool,
    keys: &(String, String),
) {
    let kind = match root.name.as_str() {
        "Hash Join" => "Merge Join",
        "Hash Left Join" if b_outer => "Merge Right Join",
        "Hash Left Join" => "Merge Left Join",
        _ => return,
    };
    if root.children.len() != 2
        || root.children[1].name != "Hash"
        || root.children[1].children.len() != 1
    {
        return;
    }
    let mut hash = root.children.pop().expect("two children");
    let right = hash.children.pop().expect("one child");
    let left = root.children.pop().expect("two children");
    let (mut o, mut i, okey, ikey) = if b_outer {
        (right, left, &keys.1, &keys.0)
    } else {
        (left, right, &keys.0, &keys.1)
    };
    o.estimate = Some(outer);
    i.estimate = Some(inner);
    let mut osort = PlanNode::over("Sort", o);
    osort.details.push(("Sort Key".into(), okey.clone()));
    osort.estimate = Some((plan.outer_sort.0, plan.outer_sort.1, outer.2, outer.3));
    let mut isort = PlanNode::over("Sort", i);
    isort.details.push(("Sort Key".into(), ikey.clone()));
    isort.estimate = Some((plan.inner_sort.0, plan.inner_sort.1, inner.2, inner.3));
    let inner_node = match plan.material {
        Some((s, t)) => {
            let mut m = PlanNode::over("Materialize", isort);
            m.estimate = Some((s, t, inner.2, inner.3));
            m
        }
        None => isort,
    };
    root.children.push(osort);
    root.children.push(inner_node);
    root.name = kind.into();
    root.commute_cond = b_outer;
    if b_outer {
        root.inner_unique = false;
    }
    root.estimate = Some(join);
}

/// Whether a hash join's one-equality `Hash Cond` must be commuted so
/// its outer side's operand comes first (RP-373): `create_hashjoin_plan`
/// runs `get_switched_clauses`, which commutes every clause whose left
/// operand is not from the outer relation. `None` when the clause is not
/// one equality of two qualified columns, or neither operand names a
/// relation scanned under the outer child.
fn outer_first(n: &PlanNode, c: &str) -> Option<bool> {
    if !(n.name.starts_with("Hash") && n.name.ends_with("Join")) || n.children.is_empty() {
        return None;
    }
    let inner = c.strip_prefix('(')?.strip_suffix(')')?;
    let (a, b) = inner.split_once(" = ")?;
    if b.contains(" = ") {
        return None;
    }
    let qual = |o: &str| o.split_once('.').map(|(q, _)| q.to_string());
    let (qa, qb) = (qual(a)?, qual(b)?);
    let mut outer = Vec::new();
    relation_names(&n.children[0], &mut outer);
    match (outer.contains(&qa), outer.contains(&qb)) {
        (false, true) => Some(true),
        (true, false) => Some(false),
        _ => None,
    }
}

/// The names a subtree's scans go by in a qualified column: each scan's
/// alias, or its relation's name when it has none.
fn relation_names(n: &PlanNode, out: &mut Vec<String>) {
    let prop = |k: &str| n.props.iter().find(|(p, _)| p == k).map(|(_, v)| v.clone());
    if let Some(name) = prop("Alias").or_else(|| prop("Relation Name")) {
        out.push(name);
    }
    for c in &n.children {
        relation_names(c, out);
    }
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

/// Name a priced join's two scans, outer then inner, after the index each
/// reads (RP-369): `Index Scan using <index> on <table>`, its `Index Name`
/// first among its properties, as [`table_scan`] builds one; a `None`
/// is the Seq Scan the cost model priced.
pub(crate) fn name_index_scans(root: &mut PlanNode, indexes: &[Option<String>; 2]) {
    fn leaves<'a>(n: &'a mut PlanNode, out: &mut Vec<&'a mut PlanNode>) {
        if n.children.is_empty() {
            out.push(n);
        } else {
            for c in &mut n.children {
                leaves(c, out);
            }
        }
    }
    let mut scans = Vec::new();
    leaves(root, &mut scans);
    if scans.len() != 2 {
        return;
    }
    for (scan, index) in scans.into_iter().zip(indexes) {
        let table = match scan.name.strip_prefix("Seq Scan on ") {
            Some(t) => t.to_string(),
            None => match scan
                .name
                .strip_prefix("Index Scan using ")
                .and_then(|r| r.split_once(" on "))
            {
                Some((_, t)) => t.to_string(),
                None => continue,
            },
        };
        scan.props.retain(|(k, _)| k != "Index Name");
        match index {
            Some(index) => {
                scan.name = format!("Index Scan using {index} on {table}");
                scan.props.insert(0, ("Index Name".into(), index.clone()));
            }
            None => scan.name = format!("Seq Scan on {table}"),
        }
    }
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
            let loops = n.actual_loops.unwrap_or(1);
            if options.timing {
                header.push_str(&format!(
                    " (actual time=0.000..0.000 rows={rows} loops={loops})"
                ));
            } else {
                header.push_str(&format!(" (actual rows={rows} loops={loops})"));
            }
        }
        out.push(header);
        for (label, value) in &n.details {
            out.push(format!("{}{label}: {value}", " ".repeat(2 + 6 * depth)));
            // `show_instrumentation_count`: the text format prints a
            // non-zero count only.
            if let (Some(r), Some(_), "Filter") = (n.removed_by_filter, actual, label.as_str()) {
                if r > 0 {
                    out.push(format!(
                        "{}Rows Removed by Filter: {r}",
                        " ".repeat(2 + 6 * depth)
                    ));
                }
            }
            if let (Some(r), Some(_), "Join Filter") =
                (n.join_filter_removed, actual, label.as_str())
            {
                if r > 0 {
                    out.push(format!(
                        "{}Rows Removed by Join Filter: {r}",
                        " ".repeat(2 + 6 * depth)
                    ));
                }
            }
        }
        if let (Some(n), Some(_)) = (n.heap_fetches, actual) {
            out.push(format!("{}Heap Fetches: {n}", " ".repeat(2 + 6 * depth)));
        }
        if let (Some(n), Some(_)) = (n.heap_blocks, actual) {
            out.push(format!(
                "{}Heap Blocks: exact={n}",
                " ".repeat(2 + 6 * depth)
            ));
        }
        if let (Some(h), Some(_)) = (n.hash_info, actual) {
            out.push(format!("{}{}", " ".repeat(2 + 6 * depth), h.text()));
        }
        for c in &n.children {
            let a = actual.and(c.actual_rows);
            walk(c, depth + 1, options, a, out);
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
            let loops = n.actual_loops.unwrap_or(1);
            fields.push(("Actual Loops".into(), Value::Num(loops.to_string())));
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
            // An index scan's counts after the qual each belongs to
            // (`ExplainNode`'s T_IndexScan); the structured formats print
            // a zero too (RP-384).
            if let (Some(r), Some(_)) = (n.removed_by_filter, actual) {
                match k.as_str() {
                    "Index Cond" => fields.push((
                        "Rows Removed by Index Recheck".into(),
                        Value::Num("0".into()),
                    )),
                    "Filter" => {
                        fields.push(("Rows Removed by Filter".into(), Value::Num(r.to_string())))
                    }
                    _ => {}
                }
            }
            if let (Some(r), Some(_), "Join Filter") = (n.join_filter_removed, actual, k.as_str()) {
                fields.push((
                    "Rows Removed by Join Filter".into(),
                    Value::Num(r.to_string()),
                ));
            }
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
        // A bitmap heap scan's (`show_tidbitmap_info`), after its recheck
        // count.
        if let (Some(_), Some(b)) = (actual, n.heap_blocks) {
            fields.push((
                "Rows Removed by Index Recheck".into(),
                Value::Num("0".into()),
            ));
            fields.push(("Exact Heap Blocks".into(), Value::Num(b.to_string())));
            fields.push(("Lossy Heap Blocks".into(), Value::Num("0".into())));
        }
        // A Hash node's table (`show_hash_info`), structured.
        if let (Some(_), Some(h)) = (actual, n.hash_info) {
            for (k, v) in [
                ("Hash Buckets", h.buckets),
                ("Original Hash Buckets", h.original_buckets),
                ("Hash Batches", h.batches),
                ("Original Hash Batches", h.original_batches),
                ("Peak Memory Usage", h.peak_kb()),
            ] {
                fields.push((k.into(), Value::Num(v.to_string())));
            }
        }
        if !n.children.is_empty() {
            let kids = n
                .children
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    // ExplainNode's `Member` plans: an Append's, and a
                    // BitmapOr's / BitmapAnd's (RP-378).
                    let rel = if matches!(node_type.as_str(), "Append" | "BitmapOr" | "BitmapAnd") {
                        "Member"
                    } else if node_type == "Subquery Scan" {
                        "Subquery"
                    } else if i == 0 {
                        "Outer"
                    } else {
                        "Inner"
                    };
                    node(c, options, actual.and(c.actual_rows), Some(rel))
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

/// The `Filter` text `plan_tree` gives a scan whose statement keeps a
/// residual it cannot print from the lowered filter.
const FILTER_PLACEHOLDER: &str = "(...)";

/// Drop the placeholder `Filter` from a scan about to print its real one:
/// the printed WHERE replaces what a residual left, alone (RP-367) or as a
/// join's scan (RP-405), where it was printed above the real line.
fn drop_filter_placeholder(scan: &mut PlanNode) {
    scan.details
        .retain(|(k, v)| !(k == "Filter" && v == FILTER_PLACEHOLDER));
}

/// Put a single-relation statement's WHERE on its scan, as PostgreSQL shows
/// it: the conjuncts an index scan's key constrains as `Index Cond`, the
/// rest as `Filter`. A plan with more than one scan is left alone.
pub(crate) fn attach_scan_quals(
    root: &mut PlanNode,
    quals: &[(String, Option<String>)],
    or_arms: &secantus_pgplan::ruleutils::OrArms,
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
    if !or_quals_on_scan(scan, quals, or_arms, index_columns) {
        quals_on_scan(scan, quals, index_columns);
    }
}

/// A Bitmap Heap Scan over a BitmapOr (RP-378), as
/// `create_bitmap_scan_plan` prints it: each arm's Bitmap Index Scan takes
/// that arm's conjuncts on its index's columns as its `Index Cond`; the
/// heap rechecks the `OR` of those (`Recheck Cond`), and filters on every
/// other conjunct and, where an arm held more than its index conditions,
/// the whole `OR` (`Filter`). `false` for any other scan.
fn or_quals_on_scan(
    scan: &mut PlanNode,
    quals: &[(String, Option<String>)],
    or_arms: &secantus_pgplan::ruleutils::OrArms,
    index_columns: &dyn Fn(&str) -> Vec<String>,
) -> bool {
    if !scan.name.starts_with("Bitmap Heap Scan on ")
        || !scan.children.first().is_some_and(|c| c.name == "BitmapOr")
    {
        return false;
    }
    let children = scan.children[0].children.len();
    let Some((at, arms)) = quals.iter().enumerate().find_map(|(i, (text, _))| {
        or_arms
            .iter()
            .find(|(t, a)| t == text && a.len() == children)
            .map(|(_, a)| (i, a))
    }) else {
        return false;
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
    let mut whole = true;
    let mut conds = Vec::with_capacity(children);
    for (child, arm) in scan.children[0].children.iter_mut().zip(arms) {
        let keys = child
            .props
            .iter()
            .find(|(k, _)| k == "Index Name")
            .map(|(_, v)| index_columns(v))
            .unwrap_or_default();
        let mut keyed: Vec<(usize, &String)> = arm
            .iter()
            .filter_map(|(text, col)| {
                Some((keys.iter().position(|k| Some(k) == col.as_ref())?, text))
            })
            .collect();
        if keyed.is_empty() {
            return false;
        }
        whole &= keyed.len() == arm.len();
        keyed.sort_by_key(|(at, _)| *at);
        let cond = join(&keyed.iter().map(|(_, t)| *t).collect::<Vec<_>>());
        child.details.push(("Index Cond".into(), cond.clone()));
        conds.push(cond);
    }
    let (text, _) = &quals[at];
    let recheck = if whole {
        text.clone()
    } else {
        format!("({})", conds.join(" OR "))
    };
    drop_filter_placeholder(scan);
    scan.details.push(("Recheck Cond".into(), recheck));
    let filter: Vec<&String> = quals
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != at || !whole)
        .map(|(_, (t, _))| t)
        .collect();
    if !filter.is_empty() {
        scan.details.push(("Filter".into(), join(&filter)));
    }
    true
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

/// A relation whose WHERE folded to constant FALSE or NULL is dummy
/// (`relation_excluded_by_constraints`, RP-409): where its scan, or the join
/// of its tables, would be, PostgreSQL prints a `Result` costing nothing
/// whose `One-Time Filter` is `false`, with nothing under it. The topmost
/// scan or join is replaced; the nodes above it (an Aggregate, a Sort, a
/// ModifyTable) stay.
pub(crate) fn dummy_relation(root: &mut PlanNode) {
    fn relation(n: &PlanNode) -> bool {
        is_scan_name(&n.name) || n.name.ends_with(" Join") || n.name == "Nested Loop"
    }
    fn find(n: &mut PlanNode) -> Option<&mut PlanNode> {
        if relation(n) {
            return Some(n);
        }
        n.children.iter_mut().find_map(find)
    }
    if let Some(node) = find(root) {
        let mut result = PlanNode::new("Result");
        // Under ANALYZE it ran once and returned nothing.
        result.actual_rows = Some(0);
        result
            .details
            .push(("One-Time Filter".into(), "false".into()));
        *node = result;
    }
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
    drop_filter_placeholder(scan);
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
        let mut keyed: Vec<(usize, &String)> = Vec::new();
        for (text, col) in quals {
            match col.as_ref().and_then(|c| keys.iter().position(|k| k == c)) {
                Some(at) => keyed.push((at, text)),
                None => filter.push(text),
            }
        }
        // In the index's column order (`fix_indexqual_references` keeps
        // the path's index clauses, which `match_clauses_to_index` lists
        // column by column).
        keyed.sort_by_key(|(at, _)| *at);
        cond.extend(keyed.into_iter().map(|(_, t)| t));
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

/// Make `tree`, one relation's scan, the index path `index` names: an
/// Index Scan or Index Only Scan naming its index, or a Bitmap Heap Scan
/// over one Bitmap Index Scan (RP-338) or over a BitmapOr of one per arm
/// (RP-378).
pub(crate) fn index_path_node(
    tree: &mut PlanNode,
    index: crate::scan_estimate::ScanIndex,
    table: &str,
) {
    tree.name = index.node_name(table);
    tree.props.retain(|(k, _)| k != "Index Name");
    tree.children.clear();
    match index.bitmap {
        Some(child) if !index.or_arms.is_empty() => {
            let mut or = PlanNode::new("BitmapOr");
            or.estimate = Some(child);
            for (name, estimate) in index.or_arms {
                or.children.push(bitmap_index_scan(&name, estimate));
            }
            tree.children.push(or);
        }
        Some(child) => tree.children.push(bitmap_index_scan(&index.name, child)),
        None => tree.props.insert(0, ("Index Name".into(), index.name)),
    }
}

/// Make `tree`, one relation's scan under a Sort, a Limit or an
/// Aggregate, the path chosen for it (RP-382): `index`'s, or with none
/// the relation's Seq Scan.
pub(crate) fn scan_path_node(
    tree: &mut PlanNode,
    index: Option<crate::scan_estimate::ScanIndex>,
    table: &str,
) {
    match index {
        Some(index) => index_path_node(tree, index, table),
        None => {
            tree.name = format!("Seq Scan on {table}");
            tree.props.retain(|(k, _)| k != "Index Name");
            tree.children.clear();
        }
    }
}

/// Whether `name` is one relation's scan node the storage planner prints
/// under a Sort, a Limit or an Aggregate: a Seq Scan or an index scan,
/// which the cost model may replace (RP-382).
pub(crate) fn is_relation_scan(name: &str) -> bool {
    name.starts_with("Seq Scan on ")
        || name.starts_with("Index Scan using ")
        || name.starts_with("Index Only Scan using ")
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
/// on a nested loop. A hash or merge join whose ON also holds conjuncts
/// that are not join-key equalities (`splits`) takes only the equalities
/// as its `Hash Cond` / `Merge Cond` and the rest as its `Join Filter`
/// (RP-375).
pub(crate) fn attach_join_conds(
    root: &mut PlanNode,
    conds: &[String],
    splits: &[Option<(String, String)>],
) {
    fn walk(
        n: &mut PlanNode,
        conds: &[String],
        splits: &[Option<(String, String)>],
        next: &mut usize,
    ) {
        for c in &mut n.children {
            walk(c, conds, splits, next);
        }
        let label = if n.name.starts_with("Hash") && n.name.ends_with("Join") {
            "Hash Cond"
        } else if n.name.starts_with("Merge") && n.name.ends_with("Join") {
            "Merge Cond"
        } else if n.name.starts_with("Nested Loop") {
            "Join Filter"
        } else {
            return;
        };
        let split = splits.get(*next).and_then(Option::as_ref);
        if let (Some((hash, rest)), "Hash Cond" | "Merge Cond") = (split, label) {
            if !n.skip_cond {
                let c = match outer_first(n, hash) {
                    Some(true) => commute_equality(hash).unwrap_or_else(|| hash.clone()),
                    None if n.commute_cond => {
                        commute_equality(hash).unwrap_or_else(|| hash.clone())
                    }
                    _ => hash.clone(),
                };
                n.details.insert(0, ("Join Filter".into(), rest.clone()));
                n.details.insert(0, (label.into(), c));
            }
            *next += 1;
            return;
        }
        if let Some(c) = conds.get(*next) {
            if !c.is_empty() && !n.skip_cond {
                let c = match outer_first(n, c) {
                    Some(true) => commute_equality(c).unwrap_or_else(|| c.clone()),
                    Some(false) => c.clone(),
                    None if n.commute_cond => commute_equality(c).unwrap_or_else(|| c.clone()),
                    None => c.clone(),
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
    walk(root, conds, splits, &mut next);
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
                .find(|(k, v)| k == "Filter" && v != FILTER_PLACEHOLDER)
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
                    drop_filter_placeholder(n);
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
pub(crate) fn attach_join_where(
    root: &mut PlanNode,
    quals: &[secantus_pgplan::ruleutils::JoinQual],
    index_columns: &dyn Fn(&str) -> Vec<String>,
) {
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
    // Per home: its conjuncts' texts, and each with its column.
    type Home<'q> = (
        Option<String>,
        Vec<&'q String>,
        Vec<(String, Option<String>)>,
    );
    let mut homes: Vec<Home<'_>> = Vec::new();
    for (text, home, column) in quals {
        let i = match homes.iter().position(|(h, _, _)| h == home) {
            Some(i) => i,
            None => {
                homes.push((home.clone(), Vec::new(), Vec::new()));
                homes.len() - 1
            }
        };
        homes[i].1.push(text);
        homes[i].2.push((text.clone(), column.clone()));
    }
    for (home, parts, with_columns) in homes {
        let target = match &home {
            Some(rel) => scan_for(root, rel),
            None => top_join(root),
        };
        if let Some(n) = target {
            // An index scan takes the conjuncts on its key columns as its
            // `Index Cond`, the rest as its `Filter`, as a lone scan does
            // (RP-369).
            if home.is_some() && n.props.iter().any(|(k, _)| k == "Index Name") {
                quals_on_scan(n, &with_columns, index_columns);
                continue;
            }
            // Over an inner join a WHERE conjunct is a join qual.
            let inner = home.is_none()
                && (n.name == "Hash Join" || n.name == "Nested Loop" || n.name == "Merge Join");
            let label = if inner { "Join Filter" } else { "Filter" };
            if home.is_some() {
                drop_filter_placeholder(n);
            }
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
    /// A nested loop's parameterized inner takes the join clause on its
    /// index column as its `Index Cond`, inner column first, and the
    /// other clauses as its `Filter`, unqualified (RP-354).
    #[test]
    fn a_parameterized_inner_takes_its_join_clauses() {
        assert_eq!(
            index_cond_text("(e.a < k.a)", "k.a", "a").as_deref(),
            Some("(a > e.a)")
        );
        assert_eq!(
            index_cond_text("(k.a <= e.a)", "k.a", "a").as_deref(),
            Some("(a <= e.a)")
        );
        assert_eq!(index_cond_text("(e.b <> k.a)", "k.a", "a"), None);
        assert_eq!(
            split_conjuncts("((e.a < k.a) AND (e.b <> k.b))"),
            vec!["(e.a < k.a)".to_string(), "(e.b <> k.b)".to_string()]
        );
        assert_eq!(unqualify("(e.b <> k.b)", "k"), "(e.b <> b)");
        assert_eq!(unqualify("(kk.b <> k.b)", "k"), "(kk.b <> b)");
        let rel = |name: &str| {
            let mut n = PlanNode::new(format!("Seq Scan on {name}"));
            n.props.push(("Relation Name".into(), name.to_string()));
            n
        };
        let mut root = PlanNode::new("Nested Loop");
        root.children.push(rel("e"));
        root.children.push(rel("k"));
        param_index_inner(
            &mut root,
            &crate::scan_estimate::ParamScan {
                index: "k_pkey".into(),
                alias: "k".into(),
                column: "a".into(),
                index_only: false,
            },
        );
        root.details.push((
            "Join Filter".into(),
            "((e.a > k.a) AND (e.a <> k.a) AND (e.b <> k.b))".into(),
        ));
        move_param_conds(&mut root);
        assert!(root.details.is_empty());
        assert_eq!(root.children[1].name, "Index Scan using k_pkey on k");
        assert_eq!(
            root.children[1].details,
            vec![
                ("Index Cond".to_string(), "(a < e.a)".to_string()),
                ("Filter".to_string(), "(e.b <> b)".to_string()),
            ]
        );
        // Two bounds on the column are one Index Cond of both, in the
        // join's order.
        let mut root = PlanNode::new("Nested Loop Left Join");
        root.children.push(rel("e"));
        root.children.push(rel("k"));
        param_index_inner(
            &mut root,
            &crate::scan_estimate::ParamScan {
                index: "k_pkey".into(),
                alias: "k".into(),
                column: "a".into(),
                index_only: false,
            },
        );
        root.details.push((
            "Join Filter".into(),
            "((e.a < k.a) AND (e.a > k.a) AND (e.a <> k.a))".into(),
        ));
        move_param_conds(&mut root);
        assert!(root.details.is_empty());
        assert_eq!(
            root.children[1].details,
            vec![(
                "Index Cond".to_string(),
                "((a > e.a) AND (a < e.a))".to_string()
            )]
        );
        // An inner whose index holds every column read is index-only.
        let mut root = PlanNode::new("Nested Loop");
        root.children.push(rel("e"));
        root.children.push(rel("k"));
        param_index_inner(
            &mut root,
            &crate::scan_estimate::ParamScan {
                index: "k_pkey".into(),
                alias: "k".into(),
                column: "a".into(),
                index_only: true,
            },
        );
        assert_eq!(root.children[1].name, "Index Only Scan using k_pkey on k");
    }

    /// A LEFT join's ON restriction of its outer side reads no inner
    /// column, so no inner scan can take it (`join_clause_is_movable_into`):
    /// it stays the join's `Join Filter`, a literal naming the inner's
    /// column included (RP-354).
    #[test]
    fn an_outer_restriction_stays_the_join_filter() {
        assert!(mentions_alias("(e.a <> k.b)", "k"));
        assert!(!mentions_alias("(e.a > 990)", "k"));
        assert!(!mentions_alias("(e.b <> 'k.a'::text)", "k"));
        assert!(!mentions_alias("(kk.a > 990)", "k"));
        let rel = |name: &str| {
            let mut n = PlanNode::new(format!("Seq Scan on {name}"));
            n.props.push(("Relation Name".into(), name.to_string()));
            n
        };
        let joined = |filter: &str| {
            let mut root = PlanNode::new("Nested Loop Left Join");
            root.children.push(rel("e"));
            root.children.push(rel("k"));
            param_index_inner(
                &mut root,
                &crate::scan_estimate::ParamScan {
                    index: "k_pkey".into(),
                    alias: "k".into(),
                    column: "a".into(),
                    index_only: false,
                },
            );
            root.details.push(("Join Filter".into(), filter.into()));
            move_param_conds(&mut root);
            root
        };
        let root = joined("((e.a < k.a) AND (e.a > 990))");
        assert_eq!(
            root.details,
            vec![("Join Filter".to_string(), "(e.a > 990)".to_string())]
        );
        assert_eq!(
            root.children[1].details,
            vec![("Index Cond".to_string(), "(a > e.a)".to_string())]
        );
        let root = joined("((e.a < k.a) AND (e.a > 990) AND (e.b <> k.b) AND (e.b = 'v1'::text))");
        assert_eq!(
            root.details,
            vec![(
                "Join Filter".to_string(),
                "((e.a > 990) AND (e.b = 'v1'::text))".to_string()
            )]
        );
        assert_eq!(
            root.children[1].details,
            vec![
                ("Index Cond".to_string(), "(a > e.a)".to_string()),
                ("Filter".to_string(), "(e.b <> b)".to_string()),
            ]
        );
    }

    #[test]
    fn commute_equality_swaps_one_equality() {
        assert_eq!(
            commute_equality("(e.b = k.b)").as_deref(),
            Some("(k.b = e.b)")
        );
        assert_eq!(commute_equality("((e.a = k.a) AND (e.b = k.b))"), None);
        assert_eq!(commute_equality("e.b = k.b"), None);
    }

    /// A `Hash Cond` written inner side first is printed outer side
    /// first, as `get_switched_clauses` commutes it (RP-373).
    #[test]
    fn a_hash_cond_is_written_outer_side_first() {
        let rel = |name: &str| {
            let mut n = PlanNode::new(format!("Seq Scan on {name}"));
            n.props.push(("Relation Name".into(), name.to_string()));
            n
        };
        let mut root = PlanNode::new("Hash Join");
        root.children.push(rel("e"));
        root.children.push(PlanNode::over("Hash", rel("k")));
        attach_join_conds(&mut root, &["(k.b = e.b)".to_string()], &[]);
        assert_eq!(root.details[0].1, "(e.b = k.b)");
        let mut root = PlanNode::new("Hash Join");
        root.children.push(rel("e"));
        root.children.push(PlanNode::over("Hash", rel("k")));
        attach_join_conds(&mut root, &["(e.b = k.b)".to_string()], &[]);
        assert_eq!(root.details[0].1, "(e.b = k.b)");
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

    /// A BitmapOr's arms each take their own conjuncts on their index as
    /// `Index Cond`; an arm holding a clause no index serves leaves the
    /// whole `OR` a `Filter` and the heap rechecks only the index
    /// conditions, as PostgreSQL 15.19 prints `where (a = 5 and b = 'v1')
    /// or a = 6` (RP-378).
    #[test]
    fn a_bitmap_or_puts_each_arms_conditions_on_its_index_scan() {
        let mut heap = PlanNode::new("Bitmap Heap Scan on k");
        let mut or = PlanNode::new("BitmapOr");
        or.children
            .push(bitmap_index_scan("k_pkey", (0.0, 4.28, 1.0, 0)));
        or.children
            .push(bitmap_index_scan("k_pkey", (0.0, 4.28, 1.0, 0)));
        heap.children.push(or);
        let text = "(((a = 5) AND (b = 'v1'::text)) OR (a = 6))".to_string();
        let quals = vec![(text.clone(), None)];
        let arms = vec![(
            text.clone(),
            vec![
                vec![
                    ("(a = 5)".to_string(), Some("a".to_string())),
                    ("(b = 'v1'::text)".to_string(), Some("b".to_string())),
                ],
                vec![("(a = 6)".to_string(), Some("a".to_string()))],
            ],
        )];
        attach_scan_quals(&mut heap, &quals, &arms, &|_| vec!["a".into()]);
        assert_eq!(
            heap.details,
            vec![
                (
                    "Recheck Cond".to_string(),
                    "((a = 5) OR (a = 6))".to_string()
                ),
                ("Filter".to_string(), text),
            ]
        );
        let conds: Vec<&str> = heap.children[0]
            .children
            .iter()
            .map(|c| c.details[0].1.as_str())
            .collect();
        assert_eq!(conds, ["(a = 5)", "(a = 6)"]);
    }

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
        attach_scan_quals(&mut heap, &quals, &Vec::new(), &|_| {
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
            scan_or_arms: Vec::new(),
            scan_in_lengths: Vec::new(),
            scan_array_quals: None,
            scan_patterns: Vec::new(),
            scan_expr_quals: None,
            scan_array_cmp_filter: None,
            setop_quals: Vec::new(),
            join_conds: Vec::new(),
            join_cond_splits: Vec::new(),
            join_where: Vec::new(),
            subquery_quals: Vec::new(),
            one_time_false: 0,
            dummy_relation: false,
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

    #[test]
    fn an_index_scans_filter_count_follows_its_filter() {
        // PostgreSQL 15.19 (RP-384): text prints a non-zero count only;
        // the structured formats print both counts, each after its qual.
        let mut n = PlanNode::new("Index Scan using k_pkey on k");
        n.details.push(("Index Cond".into(), "(a < 30)".into()));
        n.details.push(("Filter".into(), "(b = 'v5'::text)".into()));
        n.removed_by_filter = Some(26);
        assert_eq!(
            render_text(&n, &options("text"), Some(3)),
            vec![
                "Index Scan using k_pkey on k (actual rows=3 loops=1)",
                "  Index Cond: (a < 30)",
                "  Filter: (b = 'v5'::text)",
                "  Rows Removed by Filter: 26",
            ]
        );
        assert_eq!(render_text(&n, &options("text"), None).len(), 3);
        n.removed_by_filter = Some(0);
        assert_eq!(render_text(&n, &options("text"), Some(3)).len(), 3);
        let json = render_json(&n, &options("json"), Some(3));
        let at = |k: &str| json.find(k).unwrap_or_else(|| panic!("{k} in {json}"));
        assert!(at("\"Index Cond\"") < at("\"Rows Removed by Index Recheck\": 0"));
        assert!(at("\"Rows Removed by Index Recheck\"") < at("\"Filter\""));
        assert!(at("\"Filter\"") < at("\"Rows Removed by Filter\": 0"));
    }
}

#[cfg(test)]
mod join_where_tests {
    use super::*;

    /// `Hash Join` over e's scan, which keeps a residual's placeholder, and
    /// a hashed scan of s.
    fn joined() -> PlanNode {
        let mut e = PlanNode::new("Seq Scan on e");
        e.props.push(("Relation Name".into(), "e".into()));
        e.details.push(("Filter".into(), FILTER_PLACEHOLDER.into()));
        let mut s = PlanNode::new("Seq Scan on s");
        s.props.push(("Relation Name".into(), "s".into()));
        let mut root = PlanNode::new("Hash Join");
        root.details
            .push(("Hash Cond".into(), "(e.a = s.a)".into()));
        root.children.push(e);
        root.children.push(PlanNode::over("Hash", s));
        root
    }

    #[test]
    fn a_join_scans_where_replaces_the_placeholder() {
        // PostgreSQL 15.19 (RP-405): `... where e.a + 1 > 996` is the one
        // `Filter: ((a + 1) > 996)` on e's scan.
        let mut root = joined();
        let quals = vec![("((a + 1) > 996)".to_string(), Some("e".to_string()), None)];
        attach_join_where(&mut root, &quals, &|_| Vec::new());
        assert_eq!(
            root.children[0].details,
            vec![("Filter".to_string(), "((a + 1) > 996)".to_string())]
        );
    }

    #[test]
    fn a_conjunct_left_on_the_join_keeps_the_placeholder() {
        // A conjunct over both sides is the join's; the scan's placeholder
        // stands until something prints the scan's own.
        let mut root = joined();
        let quals = vec![("((e.a + s.a) > 3)".to_string(), None, None)];
        attach_join_where(&mut root, &quals, &|_| Vec::new());
        assert_eq!(
            root.details.last(),
            Some(&("Join Filter".to_string(), "((e.a + s.a) > 3)".to_string()))
        );
        assert_eq!(root.children[0].details.len(), 1);
    }
}
