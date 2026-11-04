//! What `EXPLAIN` prints of a sequential scan of one relation (RP-314):
//! PostgreSQL 15's `cost_seqscan` over the relation's size
//! (`table_block_relation_estimate_size`), its rows (`clamp_row_est` of the
//! tuples through `clauselist_selectivity`) and its width (`set_rel_width`:
//! each output column's `stawidth`, or its type's `get_typavgwidth`).
//!
//! A user table is sized as the planner's cost model already sizes it
//! ([`PgHandler::planner_table_lazy`]); a built-in catalog from what
//! `initdb` left: its `relpages` / `reltuples`, its heap's blocks
//! (`catalog_heap`) and its `pg_statistic` rows (RP-311). Anything the model
//! does not price -- a clause it does not estimate, an output expression --
//! has no estimate, and the scan prints PostgreSQL's layout with zeros.

use super::*;
use secantus_pgplan::{colstats, costsize, hashagg};

/// A plan node's `(startup cost, total cost, rows, width)`.
pub(crate) type Estimate = (f64, f64, f64, i64);

/// A scan's estimate, and the index it reads when the cheapest path is an
/// index scan (`None` for a sequential scan).
pub(crate) type ScanEstimate = (Estimate, Option<ScanIndex>);

/// The index an index scan reads, and whether the scan is index-only (the
/// index holds every column the statement reads, `check_index_only`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScanIndex {
    pub name: String,
    pub only: bool,
    /// A bitmap heap scan over the index: its Bitmap Index Scan's
    /// estimate (RP-338).
    pub bitmap: Option<Estimate>,
}

impl ScanIndex {
    /// The plan node's name, as `ExplainNode` prints it.
    pub(crate) fn node_name(&self, table: &str) -> String {
        if self.bitmap.is_some() {
            return format!("Bitmap Heap Scan on {table}");
        }
        let kind = if self.only {
            "Index Only Scan"
        } else {
            "Index Scan"
        };
        format!("{kind} using {} on {table}", self.name)
    }
}

/// One of PostgreSQL's own B-tree indexes on a catalog: its oid, name,
/// key columns (stored fields, in order) and uniqueness.
pub(crate) struct CatalogIndex {
    pub oid: i64,
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

/// The built-in indexes of catalog `oid` (`pg_index_system.tsv`), their
/// key columns named as `def` stores them; `None` where a key is not a
/// column of `def`.
pub(crate) fn catalog_indexes(
    oid: i64,
    def: &secantus_pgcatalog::TableDef,
) -> Option<Vec<CatalogIndex>> {
    let mut out = Vec::new();
    for line in catalog_meta::system_index_lines() {
        let c: Vec<&str> = line.split('\t').collect();
        if c.get(1)?.parse::<i64>().ok()? != oid {
            continue;
        }
        let index: i64 = c.first()?.parse().ok()?;
        let nkey: usize = c.get(3)?.parse().ok()?;
        let unique = *c.get(4)? == "1";
        let columns = c
            .get(15)?
            .split(' ')
            .take(nkey)
            .map(|a| {
                let attnum: i64 = a.parse().ok()?;
                let i = (0..def.columns.len()).find(|&i| i64::from(def.attnum(i)) == attnum)?;
                Some(def.columns[i].field())
            })
            .collect::<Option<Vec<_>>>()?;
        let name = catalog_meta::system_class_lines().find_map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c.first()?.parse::<i64>().ok()? == index).then(|| c.get(1).map(|s| s.to_string()))?
        })?;
        out.push(CatalogIndex {
            oid: index,
            name,
            columns,
            unique,
        });
    }
    Some(out)
}

/// The key columns of the built-in catalog index `name` (by column name),
/// for `EXPLAIN`'s `Index Cond`; empty for any other name.
pub(crate) fn catalog_index_columns(name: &str) -> Vec<String> {
    let found = catalog_meta::system_class_lines().find_map(|l| {
        let c: Vec<&str> = l.split('\t').collect();
        (c.get(1) == Some(&name) && c.get(3) == Some(&"i"))
            .then(|| c.first()?.parse::<i64>().ok())
            .flatten()
    });
    let Some(index) = found else {
        return Vec::new();
    };
    let table = catalog_meta::system_index_lines().find_map(|l| {
        let c: Vec<&str> = l.split('\t').collect();
        (c.first()?.parse::<i64>().ok()? == index)
            .then(|| c.get(1)?.parse::<i64>().ok())
            .flatten()
    });
    let def = table.and_then(|t| {
        catalog_meta::system_class_lines().find_map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c.first()?.parse::<i64>().ok()? == t)
                .then(|| PgHandler::virtual_table(c.get(1)?))
                .flatten()
        })
    });
    match (table, def) {
        (Some(t), Some(def)) => catalog_indexes(t, &def)
            .and_then(|ix| ix.into_iter().find(|i| i.oid == index))
            .map(|i| {
                i.columns
                    .iter()
                    .filter_map(|f| def.columns.iter().find(|c| &c.field() == f))
                    .map(|c| c.name.clone())
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `get_typavgwidth` of a column with no `stawidth`.
fn type_width(pg_type: &str, typmod: i32) -> i64 {
    // A pseudo-type (`void`, a call's return type, RP-340) has its own
    // typlen too.
    let typlen = secantus_pgplan::pgtypes::oid_of_name(pg_type)
        .or_else(|| secantus_pgplan::pgtypes::pseudo_type_oid(pg_type))
        .and_then(|oid| pg_type_facts::builtin(oid, "typlen"))
        .and_then(|v| match v {
            Bson::Int32(n) => Some(i64::from(n)),
            Bson::Int64(n) => Some(n),
            _ => None,
        })
        .unwrap_or(-1);
    hashagg::type_avg_width(pg_type, typmod, typlen)
}

/// What `EXPLAIN` prints of a select with no FROM (RP-325): PostgreSQL 15
/// plans it as a `Result` over an `RTE_RESULT` relation of one row
/// (`cost_resultscan`: `cpu_tuple_cost` for its row, no startup, plus the
/// target list's per-tuple cost), its width the select list's
/// (`set_pathtarget_cost_width`: each expression's type's
/// `get_typavgwidth`). A constant WHERE is a one-time filter that leaves
/// the row estimate at one. A call the planner keeps (RP-340) adds its
/// `cost_qual_eval`: see [`call_cost`]. A FROM function is a Function Scan.
pub(crate) fn result_estimate(sc: &secantus_pgplan::SelectConstant) -> Option<Estimate> {
    if sc.source.is_some() {
        return None;
    }
    let mut width = 0;
    let mut per_tuple = 0.0;
    for (_, col, pg_type, typmod) in &sc.columns {
        per_tuple += call_cost(col)?;
        width += type_width(pg_type, *typmod);
    }
    // The one-time filter's evaluation is paid before the first row and
    // counted in the total too (`create_result_path`, RP-342).
    let gate = sc.one_time_cost;
    Some((
        gate,
        gate + costsize::CPU_TUPLE_COST + per_tuple,
        1.0,
        width,
    ))
}

/// `cost_qual_eval_walker`'s per-tuple cost of one select-list column
/// (RP-340): a constant is free, and each call the planner keeps unfolded
/// -- a stable or volatile function (`current_setting`, `nextval`, ...)
/// or an `SQLValueFunction` (`session_user`, `current_user`) -- costs its
/// `procost` (1 for every one of these built-ins) times
/// `cpu_operator_cost`, plus its arguments' own cost. `None` for a
/// set-returning call (a ProjectSet, not a Result) and a FROM column.
fn call_cost(col: &secantus_pgplan::ConstCol) -> Option<f64> {
    use secantus_pgplan::ConstCol as C;
    let op = costsize::CPU_OPERATOR_COST;
    Some(match col {
        C::Value(_) => 0.0,
        C::CurrentSetting { .. }
        | C::SetConfig { .. }
        | C::BackendPid
        | C::SessionUser
        | C::CurrentUser
        | C::CurrentDatabase
        | C::Sleep(_)
        | C::PgNotify { .. } => op,
        C::BlockingPids(a)
        | C::NextVal(a)
        | C::CurrVal(a)
        | C::TerminateBackend(a)
        | C::CancelBackend(a) => op + call_cost(a)?,
        C::SetVal {
            sequence,
            value,
            is_called,
        } => op + call_cost(sequence)? + call_cost(value)? + call_cost(is_called)?,
        C::SerialSequence { table, column } => op + call_cost(table)? + call_cost(column)?,
        C::ListeningChannels | C::FromColumn => return None,
    })
}

/// What `EXPLAIN` prints of a `VALUES` list of several rows (RP-326):
/// PostgreSQL 15's `cost_valuesscan` charges each row one
/// `cpu_operator_cost` for evaluating its list and one `cpu_tuple_cost`,
/// with no startup; the rows are the lists' count and the width each
/// column's type's `get_typavgwidth` under its common typmod
/// (`set_rel_width` over the RTE_VALUES' Vars). A one-row list is a
/// `Result` in PostgreSQL, and rows a set-returning function produced are
/// not a Values Scan: neither is priced here.
pub(crate) fn values_estimate(vc: &secantus_pgplan::ValuesConstant) -> Option<Estimate> {
    let typmods = vc.typmods.as_ref()?;
    if vc.rows.len() < 2 {
        return None;
    }
    let rows = vc.rows.len() as f64;
    let width = vc
        .types
        .iter()
        .zip(typmods)
        .map(|(t, m)| type_width(t, *m))
        .sum();
    const CPU_OPERATOR_COST: f64 = 0.0025;
    let total = (CPU_OPERATOR_COST + costsize::CPU_TUPLE_COST) * rows;
    Some((0.0, total, rows, width))
}

/// What `EXPLAIN` prints of the `Append` a top-level `UNION ALL` becomes
/// (RP-324). `flatten_simple_union_all` makes it an appendrel, whose rows
/// are its children's and whose width is their row-weighted average
/// (`set_append_rel_size`); PostgreSQL 15's `cost_append` for an
/// unordered, non-parallel append starts when its first child does and
/// totals its children's totals plus half a `cpu_tuple_cost` per row
/// (`APPEND_CPU_COST_MULTIPLIER`). Each child is `(estimate, costed)`:
/// the scan EXPLAIN prints, and the total the Append was priced with.
pub(crate) fn append_estimate(children: &[(Estimate, f64)]) -> Option<Estimate> {
    const APPEND_CPU_COST_MULTIPLIER: f64 = 0.5;
    let startup = children.first()?.0 .0;
    let rows: f64 = children.iter().map(|(c, _)| c.2).sum();
    let total = children.iter().map(|(_, t)| t).sum::<f64>()
        + costsize::CPU_TUPLE_COST * APPEND_CPU_COST_MULTIPLIER * rows;
    let size: f64 = children.iter().map(|(c, _)| c.2 * c.3 as f64).sum();
    let width = if rows > 0.0 {
        (size / rows).round_ties_even() as i64
    } else {
        0
    };
    Some((startup, total, rows, width))
}

/// The width of a select list of plain columns (`fields`, stored fields),
/// which the planner applies to the scan as its target
/// (`set_pathtarget_cost_width`): each entry, a repeated one again, its
/// column's `stawidth` where ANALYZE stored one above zero, else its type's
/// average. `None` for an entry that is not a column.
fn output_width(
    def: &secantus_pgcatalog::TableDef,
    analyzed: Option<&HashMap<String, Option<AnalyzedColumn>>>,
    fields: &[String],
) -> Option<i64> {
    let mut width = 0;
    for f in fields {
        let c = def.columns.iter().find(|c| &c.field() == f)?;
        let stawidth = analyzed
            .and_then(|stats| stats.get(f)?.as_ref())
            .map_or(0, |a| a.stats.width);
        width += if stawidth > 0 {
            i64::from(stawidth)
        } else {
            type_width(&c.pg_type, c.typmod)
        };
    }
    Some(width)
}

impl PgHandler {
    /// The children's and the node's estimates of a `UNION ALL` of two
    /// plain selects, each one user table's sequential scan of stored
    /// columns of the same types (RP-324), or `None` where it is not
    /// priced: another set operation, an ORDER BY or LIMIT over it, a
    /// side PostgreSQL would read through an index or project (a computed
    /// column, a coercion), or a nested union, which PostgreSQL flattens
    /// into one Append.
    ///
    /// A side with a WHERE is not a safe append member
    /// (`is_safe_append_member`: its quals have nowhere to go), so it
    /// stays a subquery: the Append is priced over its Subquery Scan
    /// (`cost_subqueryscan`, one `cpu_tuple_cost` per row), which
    /// `set_plan_references` then removes as trivial, leaving the scan's
    /// own estimate on the printed node.
    pub(crate) fn union_all_estimate(
        &self,
        set: &secantus_pgplan::SetOpSelect,
    ) -> Option<(Vec<Estimate>, Estimate)> {
        use secantus_pgplan::Statement;
        if !matches!(set.kind, secantus_pgplan::SetOpKind::Union)
            || !set.all
            || !set.order.is_empty()
            || set.limit.is_some()
            || set.offset != 0
            || set.with_ties
        {
            return None;
        }
        let defs = self.all_table_defs().ok()?;
        let mut children = Vec::new();
        let mut types: Vec<Vec<String>> = Vec::new();
        for side in [&set.left, &set.right] {
            let Statement::Select(sel) = side.as_ref() else {
                return None;
            };
            if sel.residual.is_some()
                || sel.sub.is_some()
                || sel.series.is_some()
                || sel.join.is_some()
                || !sel.windows.is_empty()
                || !sel.order.is_empty()
                || sel.limit.is_some()
                || sel.offset != 0
                || !matches!(sel.distinct, secantus_pgplan::Distinct::None)
                || sel.with_ties
                || sel.casts.iter().any(Option::is_some)
            {
                return None;
            }
            let def = defs.iter().find(|d| d.name == sel.table)?;
            let fields: Vec<String> = sel.columns.iter().map(|(_, f)| f.clone()).collect();
            let typed = fields
                .iter()
                .map(|f| {
                    let c = def.columns.iter().find(|c| &c.field() == f)?;
                    Some(c.pg_type.clone())
                })
                .collect::<Option<Vec<_>>>()?;
            let (estimate, None) = self.scan_estimate(&sel.table, &sel.filter, &fields)? else {
                return None;
            };
            let costed = if sel.filter.is_empty() {
                estimate.1
            } else {
                estimate.1 + costsize::CPU_TUPLE_COST * estimate.2
            };
            children.push((estimate, costed));
            types.push(typed);
        }
        // Sides of different types are coerced, which a projection does.
        if types[0] != types[1] {
            return None;
        }
        let estimate = append_estimate(&children)?;
        Some((children.into_iter().map(|(e, _)| e).collect(), estimate))
    }

    /// A Nested Loop joining two tables' sequential scans with nothing to
    /// hash or merge on (RP-329): `FROM a, b`, its WHERE's conjuncts over
    /// both tables only `<`, `<=`, `>`, `>=` (`DEFAULT_INEQ_SEL` each, as
    /// `clauselist_selectivity` multiplies them). Each side is
    /// `cost_seqscan` of the columns the join reads from it
    /// (`build_base_rel_tlists`), the join's width the columns read above
    /// it (`build_joinrel_tlist`), and the plan
    /// [`joincost::cheapest_nest_loop`]'s. `None` -- zeros -- where a table
    /// has an index (a parameterized inner index scan may win, which is
    /// not modelled here) or anything else sits in the plan.
    pub(crate) fn nest_loop_estimate(
        &self,
        sel: &secantus_pgplan::Select,
    ) -> Option<(
        secantus_pgplan::joincost::NestLoopPlan,
        Estimate,
        Estimate,
        Estimate,
    )> {
        use secantus_pgplan::joins::{FilterKind, JoinKind, JoinNode, JoinRows};
        use secantus_pgplan::Statement;
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
            || ((!sel.filter.is_empty() || sel.residual.is_some()) && !j.where_modelled)
            || j.where_spans_equality
            || j.where_span_aliases.len() != j.where_filters.len()
            || j.where_filters
                .iter()
                .any(|f| f.kind != FilterKind::Inequality)
        {
            return None;
        }
        let JoinNode::Join {
            kind: JoinKind::Inner,
            left,
            right,
            on: None,
            equi,
            merged,
            expr_keys,
            ..
        } = &j.tree
        else {
            return None;
        };
        if !equi.is_empty() || !merged.is_empty() || !expr_keys.is_empty() {
            return None;
        }
        let indexes = self.index_relations();
        let project = j.project.as_deref();
        let side = |node: &JoinNode| -> Option<(Estimate, i64)> {
            let JoinNode::Leaf { plan, columns, .. } = node else {
                return None;
            };
            let Statement::Select(leaf) = plan.as_ref() else {
                return None;
            };
            if leaf.residual.is_some()
                || leaf.sub.is_some()
                || leaf.series.is_some()
                || leaf.join.is_some()
                || !leaf.windows.is_empty()
                || !leaf.order.is_empty()
                || leaf.limit.is_some()
                || leaf.offset != 0
                || !matches!(leaf.distinct, secantus_pgplan::Distinct::None)
                || leaf.casts.iter().any(Option::is_some)
                || indexes.iter().any(|ix| ix.table.name == leaf.table)
            {
                return None;
            }
            let read: Vec<String> = columns
                .iter()
                .filter(|(key, _)| JoinRows::carries(project, key))
                .map(|(_, f)| f.clone())
                .collect();
            let above: Vec<String> = columns
                .iter()
                .filter(|(key, _)| j.needed.contains(key) || project.is_none())
                .map(|(_, f)| f.clone())
                .collect();
            let (scan, None) = self.scan_estimate(&leaf.table, &leaf.filter, &read)? else {
                return None;
            };
            let (width, None) = self.scan_estimate(&leaf.table, &Document::new(), &above)? else {
                return None;
            };
            Some((scan, width.3))
        };
        let (a, a_above) = side(left)?;
        let (b, b_above) = side(right)?;
        let qual: f64 = j
            .where_filters
            .iter()
            .map(|f| f.ops as f64 * costsize::CPU_OPERATOR_COST)
            .sum();
        let sel_factor = (1.0_f64 / 3.0).powi(j.where_filters.len() as i32);
        let plan = secantus_pgplan::joincost::cheapest_nest_loop(a, b, qual);
        let rows = costsize::clamp_row_est(a.2 * b.2 * sel_factor);
        let join = (plan.startup, plan.total, rows, a_above + b_above);
        Some((plan, a, b, join))
    }

    /// A Hash Join of two tables' sequential scans on one column equality
    /// of an inner JOIN's ON, both columns analysed (RP-330): each side is
    /// `cost_seqscan` of the columns the join reads from it, the clause's
    /// selectivity `eqjoinsel_inner` over the two columns' MCV lists, the
    /// hashed side's buckets `estimate_hash_bucket_stats`, and the join
    /// `cost_hashjoin` over the side PostgreSQL's `add_path` keeps hashed
    /// (RP-359). Returns the printed outer scan's, the hashed scan's and
    /// the join's estimates, and whether the left table is the hashed one;
    /// the Hash node is its input's total. `None` -- zeros -- for any
    /// other shape.
    pub(crate) fn hash_join_estimate(
        &self,
        sel: &secantus_pgplan::Select,
    ) -> Option<(Estimate, Estimate, Estimate, bool)> {
        use secantus_pgplan::joins::{JoinKind, JoinNode, JoinRows};
        use secantus_pgplan::Statement;
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
        {
            return None;
        }
        let JoinNode::Join {
            kind: JoinKind::Inner,
            left,
            right,
            on: Some(_),
            on_other: false,
            equi,
            merged,
            expr_keys,
            ..
        } = &j.tree
        else {
            return None;
        };
        let [(left_key, right_key)] = equi.as_slice() else {
            return None;
        };
        if !merged.is_empty() || !expr_keys.is_empty() {
            return None;
        }
        let project = j.project.as_deref();
        // A side: its scan's estimate, the width it carries above the
        // join, its key column's statistics and type, and its planner
        // table.
        let side = |node: &JoinNode, key: &str| -> Option<HashSide> {
            let JoinNode::Leaf { plan, columns, .. } = node else {
                return None;
            };
            let Statement::Select(leaf) = plan.as_ref() else {
                return None;
            };
            if !leaf.filter.is_empty()
                || leaf.residual.is_some()
                || leaf.sub.is_some()
                || leaf.series.is_some()
                || leaf.join.is_some()
                || !leaf.windows.is_empty()
                || !leaf.order.is_empty()
                || leaf.limit.is_some()
                || leaf.offset != 0
                || !matches!(leaf.distinct, secantus_pgplan::Distinct::None)
                || leaf.casts.iter().any(Option::is_some)
            {
                return None;
            }
            // The leaf's output names, as the stored fields they are
            // (a primary key is stored as `_id`).
            let stored = |name: &str| -> Option<String> {
                leaf.columns
                    .iter()
                    .find(|(out, _)| out == name)
                    .map(|(_, f)| f.clone())
            };
            let field = stored(&columns.iter().find(|(k, _)| k == key)?.1)?;
            let read: Vec<String> = columns
                .iter()
                .filter(|(key, _)| JoinRows::carries(project, key))
                .map(|(_, f)| stored(f))
                .collect::<Option<_>>()?;
            let above: Vec<String> = columns
                .iter()
                .filter(|(key, _)| j.needed.contains(key) || project.is_none())
                .map(|(_, f)| stored(f))
                .collect::<Option<_>>()?;
            self.hash_side(&leaf.table, &leaf.filter, &field, &read, &above)
        };
        let a = side(left, left_key)?;
        let b = side(right, right_key)?;
        hash_join_of(a, b, false)
    }

    /// [`Self::hash_join_estimate`] for a two-table inner join planned on
    /// the narrow path (`Select::join`, RP-361): each side's scan reads
    /// the columns the select list takes from it and its join key, and
    /// carries the select list's above the join.
    pub(crate) fn join_select_hash_estimate(
        &self,
        sel: &secantus_pgplan::Select,
    ) -> Option<(Estimate, Estimate, Estimate, bool)> {
        let j = sel.join.as_ref()?;
        if sel.sub.is_some()
            || sel.series.is_some()
            || !sel.windows.is_empty()
            || !sel.order.is_empty()
            || sel.limit.is_some()
            || sel.offset != 0
            || !matches!(sel.distinct, secantus_pgplan::Distinct::None)
            || sel.with_ties
            || !sel.filter.is_empty()
            || sel.residual.is_some()
            || j.left_sub.is_some()
            || j.right_sub.is_some()
            || j.order.is_some()
            || j.exprs.iter().any(Option::is_some)
        {
            return None;
        }
        let (a, b) = j.on.as_ref()?;
        // Each side's WHERE as its scan's filter (RP-362). A predicate on a
        // join key puts a constant in the key's equivalence class, which
        // leaves no join clause; that plan is not this one.
        for pred in &j.filter {
            if (pred.alias == a.0 && pred.col == a.1) || (pred.alias == b.0 && pred.col == b.1) {
                return None;
            }
        }
        // A WHERE on a LEFT join's nullable side is strict, so PostgreSQL
        // reduces the join to an inner one (`reduce_outer_joins`).
        if j.left_join && j.filter.iter().any(|p| p.alias == j.right.1) {
            return None;
        }
        let indexes = self.index_relations();
        let side_filter = |table: &str, alias: &str| -> Option<Document> {
            use secantus_pgplan::JoinOp;
            let def = self.lookup(table)?;
            let mut clauses = Vec::new();
            // A WHERE over a table with an index may be an index scan's,
            // which this estimate does not weigh.
            if j.filter.iter().any(|p| p.alias == alias)
                && indexes.iter().any(|ix| ix.table.name == table)
            {
                return None;
            }
            for pred in j.filter.iter().filter(|p| p.alias == alias) {
                let op = match pred.op {
                    JoinOp::Eq => "$eq",
                    JoinOp::Gt => "$gt",
                    JoinOp::Ge => "$gte",
                    JoinOp::Lt => "$lt",
                    JoinOp::Le => "$lte",
                    JoinOp::NotTrue => return None,
                };
                let field = def.field_of(&pred.col)?;
                clauses.push(bson::doc! { field: { op: pred.value.clone() } });
            }
            Some(match clauses.len() {
                0 => Document::new(),
                1 => clauses.pop().expect("one clause"),
                _ => bson::doc! { "$and": clauses },
            })
        };
        let side = |(table, alias): &(String, String)| -> Option<HashSide> {
            let key = if a.0 == *alias {
                &a.1
            } else if b.0 == *alias {
                &b.1
            } else {
                return None;
            };
            let count = self
                .storage
                .count_matching(self.db(), table, &Document::new(), None)
                .ok()?;
            let def = self.planner_table(table, count)?.def;
            let above: Vec<String> = j
                .columns
                .iter()
                .filter(|(_, side, _)| side == alias)
                .map(|(_, _, c)| def.field_of(c))
                .collect::<Option<_>>()?;
            let field = def.field_of(key)?;
            let mut read = above.clone();
            if !read.contains(&field) {
                read.push(field.clone());
            }
            let filter = side_filter(table, alias)?;
            self.hash_side(table, &filter, &field, &read, &above)
        };
        // A self-join's two sides share a table; their aliases differ.
        if j.left.1 == j.right.1 {
            return None;
        }
        hash_join_of(side(&j.left)?, side(&j.right)?, j.left_join)
    }

    /// [`Self::nest_loop_estimate`] for a cross join planned on the narrow
    /// two-table path (`Select::join` with no ON, RP-362): each side's
    /// sequential scan of the columns the select list takes from it, over
    /// tables with no index, and the plan `cheapest_nest_loop` keeps.
    pub(crate) fn join_select_nest_loop_estimate(
        &self,
        sel: &secantus_pgplan::Select,
    ) -> Option<(
        secantus_pgplan::joincost::NestLoopPlan,
        Estimate,
        Estimate,
        Estimate,
    )> {
        let j = sel.join.as_ref()?;
        if j.on.is_some()
            || sel.sub.is_some()
            || sel.series.is_some()
            || !sel.windows.is_empty()
            || !sel.order.is_empty()
            || sel.limit.is_some()
            || sel.offset != 0
            || !matches!(sel.distinct, secantus_pgplan::Distinct::None)
            || sel.with_ties
            || !sel.filter.is_empty()
            || sel.residual.is_some()
            || j.left_join
            || j.left_sub.is_some()
            || j.right_sub.is_some()
            || !j.filter.is_empty()
            || j.order.is_some()
            || j.exprs.iter().any(Option::is_some)
            || j.left.1 == j.right.1
        {
            return None;
        }
        let indexes = self.index_relations();
        let side = |(table, alias): &(String, String)| -> Option<Estimate> {
            if indexes.iter().any(|ix| &ix.table.name == table) {
                return None;
            }
            let def = self.lookup(table)?;
            let read: Vec<String> = j
                .columns
                .iter()
                .filter(|(_, side, _)| side == alias)
                .map(|(_, _, c)| def.field_of(c))
                .collect::<Option<_>>()?;
            let (scan, None) = self.scan_estimate(table, &Document::new(), &read)? else {
                return None;
            };
            Some(scan)
        };
        let (a, b) = (side(&j.left)?, side(&j.right)?);
        let plan = secantus_pgplan::joincost::cheapest_nest_loop(a, b, 0.0);
        let rows = costsize::clamp_row_est(a.2 * b.2);
        Some((plan, a, b, (plan.startup, plan.total, rows, a.3 + b.3)))
    }

    /// One table of a hash join priced by `cost_hashjoin` (RP-330): its
    /// scan's estimate reading `read`, the width of `above` (the columns
    /// it carries above the join), and its key `field`'s statistics.
    /// `None` where the key column is not analysed.
    fn hash_side(
        &self,
        table: &str,
        filter: &Document,
        field: &str,
        read: &[String],
        above: &[String],
    ) -> Option<HashSide> {
        let (scan, None) = self.scan_estimate(table, filter, read)? else {
            return None;
        };
        let (width, None) = self.scan_estimate(table, &Document::new(), above)? else {
            return None;
        };
        let count = self
            .storage
            .count_matching(self.db(), table, &Document::new(), None)
            .ok()?;
        let pt = self.planner_table(table, count)?;
        let col = pt.def.columns.iter().find(|c| c.field() == field)?;
        let analysed = pt.analyzed.as_ref()?.get(field)?.as_ref()?;
        let unique = pt.unique.iter().any(|u| u == field);
        // `get_variable_numdistinct`: a unique column's distinct count
        // is its non-null rows; a negative `stadistinct` is a fraction
        // of the rows; none measured is the 200 default, which the
        // estimate below does not model.
        let null_frac = f64::from(analysed.stats.null_frac);
        let stadistinct = if unique {
            -(1.0 - null_frac)
        } else {
            f64::from(analysed.stats.distinct)
        };
        let nd = if stadistinct > 0.0 {
            stadistinct
        } else if stadistinct < 0.0 {
            -stadistinct * pt.tuples
        } else {
            return None;
        };
        let nd = costsize::clamp_row_est(nd.min(pt.tuples.max(1.0)));
        let mcv = analysed
            .slots
            .as_ref()
            .map(|s| s.mcv.clone())
            .unwrap_or_default();
        Some(HashSide {
            scan,
            above: width.3,
            pg_type: col.pg_type.clone(),
            stats: (nd, null_frac, mcv),
            unique,
            tuples: pt.tuples,
        })
    }

    /// The Seq Scan's and the Limit's estimates of a LIMIT straight over
    /// one relation's sequential scan (RP-319): the scan's estimate, then
    /// `adjust_limit_rows_costs`. Only where the Seq Scan is the path
    /// PostgreSQL keeps under a LIMIT too: no clause an index could serve
    /// (`get_cheapest_fractional_path` may prefer a cheaper-to-start index
    /// scan) and no ORDER BY, DISTINCT or other node between them.
    pub(crate) fn limit_scan_estimate(
        &self,
        sel: &secantus_pgplan::Select,
    ) -> Option<(Estimate, Estimate)> {
        let indexed = self
            .index_relations()
            .iter()
            .any(|ix| ix.table.name == sel.table);
        if sel.residual.is_some()
            || sel.sub.is_some()
            || sel.series.is_some()
            || sel.join.is_some()
            || !sel.windows.is_empty()
            || !sel.order.is_empty()
            || !matches!(sel.distinct, secantus_pgplan::Distinct::None)
            || sel.with_ties
            || sel.casts.iter().any(Option::is_some)
            || (!sel.filter.is_empty() && indexed)
        {
            return None;
        }
        let fields: Vec<String> = sel.columns.iter().map(|(_, f)| f.clone()).collect();
        let (estimate, None) = self.scan_estimate(&sel.table, &sel.filter, &fields)? else {
            return None;
        };
        // `preprocess_limit`: a constant LIMIT of 0 or less is estimated
        // as one row.
        let count = sel.limit.map_or(0, |n| n.max(1));
        Some((estimate, limit_estimate(estimate, sel.offset, count)))
    }

    /// The Seq Scan's, the Limit's and the Subquery Scan's estimates of a
    /// WHERE over a FROM-subquery that is such a LIMIT of one user table's
    /// stored columns (RP-328). The qual keeps the Subquery Scan
    /// (`trivial_subqueryscan` needs none), priced by `cost_subqueryscan`:
    /// the Limit's costs, plus `cpu_tuple_cost` and the qual's per-tuple
    /// cost for each row the Limit returns.
    ///
    /// The subquery's rows are its relation's `tuples`
    /// (`set_subquery_size_estimates`), and a clause on one of its columns
    /// reads the base column's statistics through it
    /// (`examine_simple_variable`), so a distinct count stored as a
    /// fraction of the table scales to the subquery's rows. A subquery's
    /// column is never `isunique` and has no index to read a range from.
    pub(crate) fn subquery_scan_estimate(
        &self,
        sel: &secantus_pgplan::Select,
    ) -> Option<(Estimate, Estimate, Estimate)> {
        use secantus_pgplan::Statement;
        let sub = sel.sub.as_ref()?;
        let Statement::Select(isel) = sub.plan.as_ref() else {
            return None;
        };
        if sel.filter.is_empty()
            || sel.residual.is_some()
            || sel.series.is_some()
            || sel.join.is_some()
            || !sel.windows.is_empty()
            || !sel.order.is_empty()
            || sel.limit.is_some()
            || sel.offset != 0
            || !matches!(sel.distinct, secantus_pgplan::Distinct::None)
            || sel.with_ties
            || sel.casts.iter().any(Option::is_some)
            || isel.limit.is_none()
        {
            return None;
        }
        let (scan, limit) = self.limit_scan_estimate(isel)?;
        if !self
            .all_table_defs()
            .ok()?
            .iter()
            .any(|d| d.name == isel.table)
        {
            return None;
        }
        // The subquery's output columns are stored fields of its relation.
        let field_of = |f: &str| {
            isel.columns
                .iter()
                .find(|(out, _)| out == f)
                .map(|(_, field)| field.clone())
        };
        let fields = sel
            .columns
            .iter()
            .map(|(_, f)| field_of(f))
            .collect::<Option<Vec<_>>>()?;
        let filter = rename_fields(&sel.filter, &field_of)?;
        let counted: std::cell::OnceCell<Option<usize>> = std::cell::OnceCell::new();
        let count = || {
            *counted.get_or_init(|| {
                self.storage
                    .count_matching(self.db(), &isel.table, &Document::new(), None)
                    .ok()
            })
        };
        let pt = self.planner_table_lazy(&isel.table, &count)?;
        let tuples = limit.2;
        let (sel_frac, quals) = if let Some(stats) = &pt.analyzed {
            let w = self.analyzed_where(&isel.table, &pt.def, &[], stats, tuples, &filter, &[])?;
            (w.selectivity, w.quals)
        } else {
            let nd = |f: &str| {
                if pt
                    .def
                    .columns
                    .iter()
                    .any(|c| c.field() == f && c.pg_type == "bool")
                {
                    2.0
                } else {
                    tuples.min(200.0)
                }
            };
            (
                secantus_pgplan::hashagg::filter_selectivity(&filter, &nd)?,
                costsize::conjuncts(&filter)?,
            )
        };
        let width = output_width(&pt.def, pt.analyzed.as_ref(), &fields)?;
        let (startup, total) = costsize::subquery_scan_path((limit.0, limit.1), tuples, &quals);
        Some((
            scan,
            limit,
            (
                startup,
                total,
                costsize::clamp_row_est(tuples * sel_frac),
                width,
            ),
        ))
    }

    /// The child's and the node's estimates of an `INSERT`, `UPDATE` or
    /// `DELETE` with no RETURNING (RP-331). `create_modifytable_path` takes
    /// its subpath's startup and total costs and, with no RETURNING list,
    /// zero rows and zero width. The subpath is:
    ///
    /// - for a one-row `VALUES` insert, a `Result` (`cost_resultscan`:
    ///   `cpu_tuple_cost`) whose target list is every column of the table
    ///   (`expand_insert_targetlist` fills an unnamed one with its default
    ///   or NULL), each its type's `get_typavgwidth`;
    /// - for an `UPDATE`, the relation's scan, emitting the new values of
    ///   the assigned columns (each a non-Var, so its type's width) and the
    ///   `ctid` (6 bytes);
    /// - for a `DELETE`, the relation's scan emitting the `ctid` alone.
    ///
    /// Not priced (`None`, zeros): RETURNING, ON CONFLICT, `INSERT ...
    /// SELECT` and multi-row VALUES, a write through a view, a SET that
    /// reads the row (its expression's `cost_qual_eval` is not modelled),
    /// a WHERE that does not lower to a filter, and a WHERE on a table
    /// with an index, where PostgreSQL may choose an index path.
    pub(crate) fn modify_table_estimate(&self, stmt: &Statement) -> Option<(Estimate, Estimate)> {
        const CTID_WIDTH: i64 = 6;
        let defs = self.all_table_defs().ok()?;
        let def_of = |table: &str| defs.iter().find(|d| d.name == table);
        let scan = |table: &str, filter: &Document, width: i64| -> Option<Estimate> {
            if !filter.is_empty()
                && self
                    .index_relations()
                    .iter()
                    .any(|ix| ix.table.name == table)
            {
                return None;
            }
            let ((startup, total, rows, _), index) = self.scan_estimate(table, filter, &[])?;
            index.is_none().then_some((startup, total, rows, width))
        };
        let child = match stmt {
            Statement::Insert(ins) => {
                if ins.rows.len() != 1
                    || ins.source.is_some()
                    || ins.returning.is_some()
                    || ins.on_conflict.is_some()
                    || !ins.view_checks.is_empty()
                {
                    return None;
                }
                let def = def_of(&ins.table)?;
                let width = def
                    .columns
                    .iter()
                    .map(|c| type_width(&c.pg_type, c.typmod))
                    .sum();
                (0.0, costsize::CPU_TUPLE_COST, 1.0, width)
            }
            Statement::Update(u) => {
                if u.returning.is_some()
                    || u.residual.is_some()
                    || !u.view_checks.is_empty()
                    || !u.set_exprs.is_empty()
                    || !u.set_subscripts.is_empty()
                    || u.set.is_empty()
                {
                    return None;
                }
                let def = def_of(&u.table)?;
                let mut width = CTID_WIDTH;
                for (field, _) in &u.set {
                    let c = def.columns.iter().find(|c| &c.field() == field)?;
                    width += type_width(&c.pg_type, c.typmod);
                }
                scan(&u.table, &u.filter, width)?
            }
            Statement::Delete(d) => {
                if d.returning.is_some() || d.residual.is_some() {
                    return None;
                }
                def_of(&d.table)?;
                scan(&d.table, &d.filter, CTID_WIDTH)?
            }
            _ => return None,
        };
        Some((child, (child.0, child.1, 0.0, 0)))
    }

    /// The estimate of the cheapest scan of `table` through `filter` that
    /// outputs `fields` (stored fields), or `None` where it is not priced.
    /// A user table's scan is the sequential one the storage planner
    /// chose; a catalog's is the cheapest of its paths (RP-334).
    pub(crate) fn scan_estimate(
        &self,
        table: &str,
        filter: &Document,
        fields: &[String],
    ) -> Option<ScanEstimate> {
        if self.all_table_defs().ok()?.iter().any(|d| d.name == table) {
            Some((self.user_seq_scan(table, filter, fields)?, None))
        } else {
            self.catalog_scan(table, filter, fields)
        }
    }

    /// Under ANALYZE, the `Heap Fetches` of an index-only scan of catalog
    /// `table` through its index `index` for `filter`, which returned
    /// `rows` rows (RP-360): the index walked for the equalities on its
    /// leading columns, each tuple on a block the visibility map leaves
    /// uncovered fetched ([`catalog_btree::index_only_heap_fetches`]).
    /// `None` where the scan's key is not those equalities.
    pub(crate) fn catalog_heap_fetches(
        &self,
        table: &str,
        index: &str,
        filter: &Document,
        rows: usize,
    ) -> Option<usize> {
        let name = table.strip_prefix("pg_catalog.").unwrap_or(table);
        let oid = catalog_meta::system_class_lines().find_map(|line| {
            let c: Vec<&str> = line.split('\t').collect();
            (c.get(1) == Some(&name) && c.get(2) == Some(&"pg_catalog") && c.get(3) == Some(&"r"))
                .then(|| c.first()?.parse::<i64>().ok())
                .flatten()
        })?;
        let def = Self::virtual_table(name)?;
        let ix = catalog_indexes(oid, &def)?
            .into_iter()
            .find(|ix| ix.name == index)?;
        let mut key = Vec::new();
        for column in &ix.columns {
            let Some(value) = filter.get(column) else {
                break;
            };
            let value = match value {
                Bson::Document(d) if d.len() == 1 => d.get("$eq")?,
                Bson::Document(_) => return None,
                v => v,
            };
            key.push(match value {
                Bson::Int32(v) => catcache::K::O(i64::from(*v)),
                Bson::Int64(v) => catcache::K::O(*v),
                Bson::String(v) => catcache::K::N(v.clone()),
                _ => return None,
            });
        }
        if key.is_empty() {
            return None;
        }
        catalog_btree::index_only_heap_fetches(ix.oid, &key, rows)
    }

    /// The estimate of a plain index scan of user table `table` through the
    /// index whose key columns are `key` (stored fields), when that is the
    /// path `set_cheapest` keeps over an analysed table (RP-317):
    /// `cost_index` over `btcostestimate`, the index sized as the planner's
    /// cost model sizes it and read in its leading column's correlation.
    /// `None` -- the node prints zeros -- where another path is cheaper or
    /// the scan is one the model does not price: a table never analysed, a
    /// clause on a later key column, a range on the leading one (its ends
    /// are read from the index, `get_actual_variable_range`), an index
    /// holding every column read (an index-only scan), an `OR`, or more
    /// than one index a clause serves (`choose_bitmap_and`).
    pub(crate) fn index_scan_estimate(
        &self,
        table: &str,
        filter: &Document,
        fields: &[String],
        key: &[String],
    ) -> Option<Estimate> {
        if filter.is_empty() || !self.all_table_defs().ok()?.iter().any(|d| d.name == table) {
            return None;
        }
        let counted: std::cell::OnceCell<Option<usize>> = std::cell::OnceCell::new();
        let count = || {
            *counted.get_or_init(|| {
                self.storage
                    .count_matching(self.db(), table, &Document::new(), None)
                    .ok()
            })
        };
        let pt = self.planner_table_lazy(table, &count)?;
        let stats = pt.analyzed.as_ref()?;
        let mut indexes = self.cost_indexes(table, &pt.def, count()? as u64);
        let w = self.analyzed_where(
            table, &pt.def, &pt.unique, stats, pt.tuples, filter, &indexes,
        )?;
        if !w.bitmap_ors.is_empty() {
            return None;
        }
        let mut named = Vec::new();
        filter_fields(filter, &mut named)?;
        let correlations = Self::stats_correlations(&pt.def, stats);
        let mut serving = Vec::new();
        for ix in &mut indexes {
            if !named.iter().any(|f| ix.columns.contains(f)) {
                continue;
            }
            let lead = ix.columns.first()?.clone();
            if !named.contains(&lead) || named.iter().any(|f| ix.columns[1..].contains(f)) {
                return None;
            }
            let ranged = w
                .quals
                .iter()
                .any(|(f, q)| *f == lead && matches!(q, costsize::Qual::Range(_)));
            let covering = fields
                .iter()
                .chain(named.iter())
                .all(|f| ix.columns.contains(f));
            if ranged || covering {
                return None;
            }
            let c = correlations.get(&lead).copied().flatten()?;
            ix.correlation = if ix.columns.len() > 1 { c * 0.75 } else { c };
            if !ix.unique {
                if let Some(Some((pages, height))) = self.dedup_index_size(table, ix) {
                    ix.pages = pages;
                    ix.height = height;
                }
            }
            // The primary key is listed both as the SQL key and as the
            // storage's own `_id` index: one relation in PostgreSQL.
            if !serving
                .iter()
                .any(|s: &costsize::Index| s.columns == ix.columns)
            {
                serving.push(ix.clone());
            }
        }
        if serving.len() != 1 || serving[0].columns != key {
            return None;
        }
        let rows = costsize::clamp_row_est(pt.tuples * w.selectivity);
        let rel = costsize::Rel {
            pages: pt.pages,
            tuples: pt.tuples,
        };
        let colsel =
            |f: &str| costsize::ColSel::Given(w.index_selectivity.get(f).copied().unwrap_or(1.0));
        // `set_cheapest` over every path, `disable_cost` added to a kind an
        // `enable_*` setting turns off.
        const DISABLE_COST: f64 = 1.0e10;
        let off = |name: &str| self.setting_is(name, "off");
        let mut best: Option<(costsize::ScanKind, (f64, f64))> = None;
        for (kind, (mut startup, mut total)) in
            costsize::scan_paths(&rel, &serving, &w.quals, &colsel, rows)
        {
            let disabled = match kind {
                costsize::ScanKind::Seq => off("enable_seqscan"),
                costsize::ScanKind::Bitmap(_) => off("enable_bitmapscan"),
                costsize::ScanKind::Index(_) => off("enable_indexscan"),
            };
            if disabled {
                startup += DISABLE_COST;
                total += DISABLE_COST;
            }
            if best.is_none_or(|(_, (bs, bt))| total < bt || (total == bt && startup < bs)) {
                best = Some((kind, (startup, total)));
            }
        }
        let (costsize::ScanKind::Index(_), (startup, total)) = best? else {
            return None;
        };
        let width = output_width(&pt.def, Some(stats), fields)?;
        Some((startup, total, rows, width))
    }

    fn user_seq_scan(&self, table: &str, filter: &Document, fields: &[String]) -> Option<Estimate> {
        let counted: std::cell::OnceCell<Option<usize>> = std::cell::OnceCell::new();
        let count = || {
            *counted.get_or_init(|| {
                self.storage
                    .count_matching(self.db(), table, &Document::new(), None)
                    .ok()
            })
        };
        let pt = self.planner_table_lazy(table, &count)?;
        let (sel, quals) = if filter.is_empty() {
            (1.0, Vec::new())
        } else if let Some(stats) = &pt.analyzed {
            let indexes = self.cost_indexes(table, &pt.def, count()? as u64);
            let w = self.analyzed_where(
                table, &pt.def, &pt.unique, stats, pt.tuples, filter, &indexes,
            )?;
            (w.selectivity, w.quals)
        } else {
            let nd = |f: &str| {
                if pt.unique.iter().any(|u| u == f) {
                    pt.tuples
                } else if pt
                    .def
                    .columns
                    .iter()
                    .any(|c| c.field() == f && c.pg_type == "bool")
                {
                    2.0
                } else {
                    pt.tuples.min(200.0)
                }
            };
            (
                hashagg::filter_selectivity(filter, &nd)?,
                costsize::conjuncts(filter)?,
            )
        };
        let quals = with_in_lengths(quals);
        let width = output_width(&pt.def, pt.analyzed.as_ref(), fields)?;
        let rel = costsize::Rel {
            pages: pt.pages,
            tuples: pt.tuples,
        };
        let (startup, total) = costsize::seq_scan_path(&rel, &quals);
        Some((
            startup,
            total,
            costsize::clamp_row_est(pt.tuples * sel),
            width,
        ))
    }

    /// A built-in catalog of `pg_catalog`, sized and measured as `initdb`
    /// left it. Its rows written since are not counted: its heap's later
    /// pages are RP-291's, and PostgreSQL's estimate does not move with
    /// them until a VACUUM or ANALYZE of the catalog (RP-313).
    ///
    /// A clause on a column one of its B-tree indexes leads with opens that
    /// index's paths (`create_index_paths`): a plain index scan
    /// (`cost_index` over `btcostestimate`, the index's `initdb` pages and
    /// height) and a bitmap heap scan, kept over the sequential scan when
    /// cheaper (`set_cheapest`). An index holding every column read is
    /// scanned index-only, its heap fetches cut by the heap's all-visible
    /// fraction (RP-337). Not priced: a range on an indexed column
    /// (`get_actual_variable_range` reads the index), a clause on an
    /// index's later column, an `OR` over indexed columns (a BitmapOr),
    /// two indexes on different leading columns (a BitmapAnd).
    fn catalog_scan(
        &self,
        table: &str,
        filter: &Document,
        fields: &[String],
    ) -> Option<ScanEstimate> {
        let name = table.strip_prefix("pg_catalog.").unwrap_or(table);
        let oid = catalog_meta::system_class_lines().find_map(|line| {
            let c: Vec<&str> = line.split('\t').collect();
            (c.get(1) == Some(&name) && c.get(2) == Some(&"pg_catalog") && c.get(3) == Some(&"r"))
                .then(|| c.first()?.parse::<i64>().ok())
                .flatten()
        })?;
        let blocks = catalog_heap::initdb_heaps().get(&oid)?.blocks;
        let (relpages, reltuples, allvisible) = catalog_meta::initdb_class_size(oid)?;
        let def = Self::virtual_table(name)?;
        // Each column's `pg_statistic` row, its slots keyed as `colstats`
        // compares them; a column of a type whose order is not modelled
        // has its scalars only, so a clause on it is not estimated.
        // A database-wide ANALYZE's measure replaces `initdb`'s row of each
        // column it re-measured (RP-313).
        let measured = self
            .relstats(statistic::CATALOG_STATISTICS)
            .and_then(|d| d.get_document("catalogs").ok().cloned())
            .unwrap_or_default();
        let mut stats: HashMap<String, Option<AnalyzedColumn>> = HashMap::new();
        for row in statistic::builtin_stats(oid) {
            let get = |k: &str| row.iter().find(|(n, _)| n == k).map(|(_, v)| v);
            let Some(Bson::Int32(attnum)) = get("staattnum") else {
                continue;
            };
            let Some(i) =
                (0..def.columns.len()).find(|&i| i64::from(def.attnum(i)) == i64::from(*attnum))
            else {
                continue;
            };
            let c = &def.columns[i];
            if let Ok(entry) = measured.get_document(format!("{oid}:{attnum}")) {
                stats.insert(
                    c.field(),
                    Some(AnalyzedColumn {
                        stats: hashagg::ColumnStats {
                            null_frac: entry.get_f64("null_frac").unwrap_or(0.0) as f32,
                            distinct: entry.get_f64("distinct").unwrap_or(0.0) as f32,
                            width: entry.get_i32("width").unwrap_or(0),
                        },
                        slots: Self::stored_slots(entry),
                    }),
                );
                continue;
            }
            let base = c.pg_type.trim_start_matches("pg_catalog.");
            let float = |k: &str| get(k).and_then(Bson::as_f64).unwrap_or(0.0) as f32;
            let scalars = hashagg::ColumnStats {
                null_frac: float("stanullfrac"),
                distinct: float("stadistinct"),
                width: match get("stawidth") {
                    Some(Bson::Int32(w)) => *w,
                    _ => 0,
                },
            };
            let slot = |kind: i32| {
                (1..=5).find(|n| get(&format!("stakind{n}")) == Some(&Bson::Int32(kind)))
            };
            let keys = |n: usize| -> Option<Vec<Vec<u8>>> {
                match get(&format!("stavalues{n}"))? {
                    Bson::Array(values) => {
                        values.iter().map(|v| Self::stats_key(base, v)).collect()
                    }
                    _ => None,
                }
            };
            let slots = (|| {
                let mcv = match slot(1) {
                    Some(n) => {
                        let freqs = match get(&format!("stanumbers{n}"))? {
                            Bson::Array(f) => f
                                .iter()
                                .map(|x| x.as_f64().map(|x| x as f32))
                                .collect::<Option<Vec<_>>>()?,
                            _ => return None,
                        };
                        keys(n)?.into_iter().zip(freqs).collect()
                    }
                    None => Vec::new(),
                };
                let histogram = match slot(2) {
                    Some(n) => keys(n)?,
                    None => Vec::new(),
                };
                let correlation = slot(3).and_then(|n| match get(&format!("stanumbers{n}"))? {
                    Bson::Array(f) => f.first()?.as_f64().map(|x| x as f32),
                    _ => None,
                });
                Some(colstats::Slots {
                    mcv,
                    histogram,
                    correlation,
                    // The list `initdb`'s ANALYZE stored, not one this
                    // server sampled: its ties are already decided.
                    mcv_complete: true,
                })
            })();
            stats.insert(
                c.field(),
                Some(AnalyzedColumn {
                    stats: scalars,
                    slots,
                }),
            );
        }
        let data_width: i64 = def
            .columns
            .iter()
            .map(|c| type_width(&c.pg_type, c.typmod))
            .sum();
        let measured = (reltuples >= 0.0).then_some((f64::from(relpages), reltuples));
        let (pages, tuples) = hashagg::table_size(data_width, 0, Some(u64::from(blocks)), measured);
        let rel = costsize::Rel { pages, tuples };
        let width = output_width(&def, Some(&stats), fields)?;
        if filter.is_empty() {
            let (startup, total) = costsize::seq_scan_path(&rel, &[]);
            return Some((
                (startup, total, costsize::clamp_row_est(tuples), width),
                None,
            ));
        }
        let w = self.analyzed_where(table, &def, &[], &stats, tuples, filter, &[])?;
        let rows = costsize::clamp_row_est(tuples * w.selectivity);
        let mut named = Vec::new();
        filter_fields(filter, &mut named)?;
        let catalog = catalog_indexes(oid, &def)?;
        let mut indexes = Vec::new();
        for ix in &catalog {
            if !named.iter().any(|f| ix.columns.contains(f)) {
                continue;
            }
            let lead = ix.columns.first()?;
            // A clause only on a later key column: the B-tree path that
            // scans the whole index for it is not costed, and the index is
            // left out, as before RP-334.
            if !named.contains(lead) {
                continue;
            }
            let later = named.iter().any(|f| ix.columns[1..].contains(f));
            let ranged = w
                .quals
                .iter()
                .any(|(f, q)| f == lead && matches!(q, costsize::Qual::Range(_)));
            // `check_index_only`, which `enable_indexonlyscan = off` skips.
            let covering = !self.setting_is("enable_indexonlyscan", "off")
                && fields
                    .iter()
                    .chain(named.iter())
                    .all(|f| ix.columns.contains(f));
            if later || ranged || !w.bitmap_ors.is_empty() {
                return None;
            }
            // An index holding every column read is scanned index-only,
            // its heap fetches cut to the pages not all-visible
            // (`cost_index` over `estimate_rel_size`'s `allvisfrac`:
            // `relallvisible` over the heap's actual blocks).
            let visible = if covering {
                let allvisible = f64::from(allvisible);
                let blocks = f64::from(blocks);
                if allvisible == 0.0 || blocks <= 0.0 {
                    0.0
                } else if allvisible >= blocks {
                    1.0
                } else {
                    allvisible / blocks
                }
            } else {
                0.0
            };
            let (ipages, height) = catalog_btree::initdb_index_size(ix.oid)?;
            let c = stats
                .get(lead)
                .and_then(|a| a.as_ref()?.slots.as_ref()?.correlation)
                .map_or(0.0, f64::from);
            indexes.push((
                ix.name.clone(),
                costsize::Index {
                    field: lead.clone(),
                    unique: ix.unique && ix.columns.len() == 1,
                    pages: f64::from(ipages),
                    height,
                    columns: ix.columns.clone(),
                    correlation: if ix.columns.len() > 1 { c * 0.75 } else { c },
                    visible,
                    tuples: None,
                    predicate: None,
                    hash: false,
                },
                covering,
            ));
        }
        // Two indexes' bitmap paths may be ANDed (`choose_bitmap_and`),
        // which is not costed here -- unless every index serves the same
        // clauses (each is used only on its leading column, above): an
        // index whose clauses an earlier one already covers is skipped as
        // redundant, so the bitmap is the cheapest single index's.
        if indexes
            .iter()
            .any(|(_, ix, _)| ix.field != indexes[0].1.field)
        {
            return None;
        }
        let paths: Vec<costsize::Index> = indexes.iter().map(|(_, ix, _)| ix.clone()).collect();
        let colsel =
            |f: &str| costsize::ColSel::Given(w.index_selectivity.get(f).copied().unwrap_or(1.0));
        // `set_cheapest` over every path, `disable_cost` added to the
        // startup and total of a kind an `enable_*` setting turns off.
        const DISABLE_COST: f64 = 1.0e10;
        let off = |name: &str| self.setting_is(name, "off");
        let mut best: Option<(costsize::ScanKind, (f64, f64))> = None;
        for (kind, (mut startup, mut total)) in
            costsize::scan_paths(&rel, &paths, &w.quals, &colsel, rows)
        {
            let disabled = match kind {
                costsize::ScanKind::Seq => off("enable_seqscan"),
                costsize::ScanKind::Bitmap(_) => off("enable_bitmapscan"),
                // `cost_index` disables an index-only scan with the plain
                // one; `enable_indexonlyscan` only stops it being built.
                costsize::ScanKind::Index(_) => off("enable_indexscan"),
            };
            if disabled {
                startup += DISABLE_COST;
                total += DISABLE_COST;
            }
            if best.is_none_or(|(_, (bs, bt))| total < bt || (total == bt && startup < bs)) {
                best = Some((kind, (startup, total)));
            }
        }
        let (kind, (startup, total)) = best?;
        let index = match kind {
            costsize::ScanKind::Seq => None,
            costsize::ScanKind::Index(i) => {
                let (name, _, only) = indexes.get(i)?;
                Some(ScanIndex {
                    name: name.clone(),
                    only: *only,
                    bitmap: None,
                })
            }
            // A bitmap heap scan over one index (`create_bitmap_subplan`):
            // the Bitmap Index Scan prints the index's own cost and the
            // rows its selectivity yields, width 0 (RP-338).
            costsize::ScanKind::Bitmap(i) => {
                let (name, ix, _) = indexes.get(i)?;
                let (itotal, isel) =
                    costsize::bitmap_index_scan(&rel, ix, &w.quals, colsel(&ix.field))?;
                Some(ScanIndex {
                    name: name.clone(),
                    only: false,
                    bitmap: Some((0.0, itotal, costsize::clamp_row_est(isel * tuples), 0)),
                })
            }
        };
        Some(((startup, total, rows, width), index))
    }
}

/// `clamp_row_est`: a row count rounded, and never below one.
/// `filter` with every field it names replaced by `rename`'s answer; `None`
/// where one has none.
fn rename_fields(filter: &Document, rename: &dyn Fn(&str) -> Option<String>) -> Option<Document> {
    let mut out = Document::new();
    for (k, v) in filter {
        let key = if k.starts_with('$') {
            k.clone()
        } else {
            rename(k)?
        };
        let value = match (k.starts_with('$'), v) {
            (true, bson::Bson::Array(items)) => bson::Bson::Array(
                items
                    .iter()
                    .map(|i| match i {
                        bson::Bson::Document(d) => {
                            rename_fields(d, rename).map(bson::Bson::Document)
                        }
                        other => Some(other.clone()),
                    })
                    .collect::<Option<Vec<_>>>()?,
            ),
            (true, bson::Bson::Document(d)) => bson::Bson::Document(rename_fields(d, rename)?),
            _ => v.clone(),
        };
        out.insert(key, value);
    }
    Some(out)
}

fn clamp_rows(rows: f64) -> f64 {
    if rows <= 1.0 {
        1.0
    } else {
        rows.round_ties_even()
    }
}

/// A Limit's estimate over its input's (RP-319): PostgreSQL 15's
/// `adjust_limit_rows_costs` (`pathnode.c`). `offset` and `count` are
/// `preprocess_limit`'s estimates: 0 is none, a negative one is unknown
/// (10% of the input), a constant LIMIT of 0 or less is counted as 1.
pub(crate) fn limit_estimate(input: Estimate, offset: i64, count: i64) -> Estimate {
    let (in_startup, in_total, in_rows, width) = input;
    let (mut startup, mut total, mut rows) = (in_startup, in_total, in_rows);
    if offset != 0 {
        let mut skip = if offset > 0 {
            offset as f64
        } else {
            clamp_rows(in_rows * 0.10)
        };
        if skip > rows {
            skip = rows;
        }
        if in_rows > 0.0 {
            startup += (in_total - in_startup) * skip / in_rows;
        }
        rows -= skip;
        if rows < 1.0 {
            rows = 1.0;
        }
    }
    if count != 0 {
        let mut take = if count > 0 {
            count as f64
        } else {
            clamp_rows(in_rows * 0.10)
        };
        if take > rows {
            take = rows;
        }
        if in_rows > 0.0 {
            total = startup + (in_total - in_startup) * take / in_rows;
        }
        rows = take;
        if rows < 1.0 {
            rows = 1.0;
        }
    }
    (startup, total, rows, width)
}

/// A Sort's estimate over its input's (RP-318): PostgreSQL 15's
/// `cost_sort` and `cost_tuplesort` (`costsize.c`), with no LIMIT bound,
/// `work_mem` in kB (`sort_mem`) and a comparison of
/// `2 * cpu_operator_cost`. The input is read in full before the first row
/// is returned, so its total cost is the Sort's startup.
pub(crate) fn sort_estimate(input: Estimate, work_mem_kb: usize) -> Estimate {
    const CPU_OPERATOR_COST: f64 = 0.0025;
    const SEQ_PAGE_COST: f64 = 1.0;
    const RANDOM_PAGE_COST: f64 = 4.0;
    const BLCKSZ: f64 = 8192.0;
    let (_, in_total, rows, width) = input;
    let tuples = if rows < 2.0 { 2.0 } else { rows };
    let comparison_cost = 2.0 * CPU_OPERATOR_COST;
    // `relation_byte_size`: MAXALIGN(width) + MAXALIGN(SizeofHeapTupleHeader).
    let maxalign = |n: i64| ((n + 7) / 8 * 8) as f64;
    let input_bytes = tuples * (maxalign(width) + maxalign(23));
    let sort_mem_bytes = work_mem_kb as f64 * 1024.0;
    let mut startup = if input_bytes > sort_mem_bytes {
        // An external merge sort.
        let npages = (input_bytes / BLCKSZ).ceil();
        let nruns = input_bytes / sort_mem_bytes;
        // `tuplesort_merge_order`.
        let merge_order = (sort_mem_bytes / (2.0 * BLCKSZ + 32.0 * BLCKSZ))
            .floor()
            .clamp(6.0, 500.0);
        let log_runs = if nruns > merge_order {
            (nruns.ln() / merge_order.ln()).ceil()
        } else {
            1.0
        };
        let page_accesses = 2.0 * npages * log_runs;
        comparison_cost * tuples * tuples.log2()
            + page_accesses * (SEQ_PAGE_COST * 0.75 + RANDOM_PAGE_COST * 0.25)
    } else {
        comparison_cost * tuples * tuples.log2()
    };
    let run = CPU_OPERATOR_COST * tuples;
    startup += in_total;
    (startup, startup + run, rows, width)
}

/// A Unique's estimate over the Sort beneath it (RP-320): PostgreSQL 15's
/// `create_upper_unique_path` (`pathnode.c`). It starts when the Sort
/// does, compares `ncols` columns of every input row, and returns `groups`
/// rows (`estimate_num_groups`).
pub(crate) fn unique_estimate(input: Estimate, ncols: usize, groups: f64) -> Estimate {
    const CPU_OPERATOR_COST: f64 = 0.0025;
    let (startup, total, rows, width) = input;
    (
        startup,
        total + CPU_OPERATOR_COST * rows * ncols as f64,
        groups,
        width,
    )
}

/// The HashAggregate `create_distinct_paths` builds for a SELECT DISTINCT
/// (RP-320): `cost_agg` with `AGG_HASHED` and no aggregates, over its
/// input's estimate. Its spilling past `hash_mem` is not priced, which
/// only ever makes it cheaper than PostgreSQL's.
pub(crate) fn hashed_distinct_estimate(input: Estimate, ncols: usize, groups: f64) -> Estimate {
    const CPU_OPERATOR_COST: f64 = 0.0025;
    const CPU_TUPLE_COST: f64 = 0.01;
    let (_, in_total, rows, width) = input;
    let startup = in_total + CPU_OPERATOR_COST * ncols as f64 * rows;
    (startup, startup + CPU_TUPLE_COST * groups, groups, width)
}

impl PgHandler {
    /// `estimate_num_groups` for a SELECT DISTINCT of user table `table`'s
    /// stored `fields` through `filter` (RP-320): each column's
    /// `get_variable_numdistinct`, over the rows the WHERE keeps. `None`
    /// for a catalog, or a WHERE or column the model does not estimate.
    pub(crate) fn distinct_groups(
        &self,
        table: &str,
        filter: &Document,
        fields: &[String],
    ) -> Option<f64> {
        if !self.all_table_defs().ok()?.iter().any(|d| d.name == table) {
            return None;
        }
        let counted: std::cell::OnceCell<Option<usize>> = std::cell::OnceCell::new();
        let count = || {
            *counted.get_or_init(|| {
                self.storage
                    .count_matching(self.db(), table, &Document::new(), None)
                    .ok()
            })
        };
        let pt = self.planner_table_lazy(table, &count)?;
        let keys: Vec<hashagg::KeyStat> = fields
            .iter()
            .map(|f| {
                let c = pt.def.columns.iter().find(|c| &c.field() == f)?;
                let unique = pt.unique.contains(f);
                let measured = pt.analyzed.as_ref().and_then(|a| a.get(f));
                Some(if c.pg_type == "bool" {
                    hashagg::KeyStat::Bool
                } else if let Some(stats) = measured {
                    hashagg::KeyStat::Analyzed {
                        stats: stats.as_ref()?.stats,
                        unique,
                    }
                } else if unique {
                    hashagg::KeyStat::Unique
                } else {
                    hashagg::KeyStat::Unknown
                })
            })
            .collect::<Option<_>>()?;
        let (_, _, rows, _) = self.user_seq_scan(table, filter, &[])?;
        Some(hashagg::estimate_groups(&keys, pt.tuples, rows) as f64)
    }
}

#[cfg(test)]
mod unique_tests {
    use super::{hashed_distinct_estimate, sort_estimate, unique_estimate};

    #[test]
    fn a_unique_compares_each_sorted_row() {
        // PostgreSQL 15.19: `select distinct a from e order by a` over a Sort
        // of (64.83..67.33 rows=1000 width=4) is (64.83..69.83 rows=1000).
        let (s, t, r, w) = unique_estimate((64.83, 67.33, 1000.0, 4), 1, 1000.0);
        assert_eq!(
            (format!("{s:.2}"), format!("{t:.2}"), r, w),
            ("64.83".into(), "69.83".into(), 1000.0, 4)
        );
    }

    #[test]
    fn few_groups_are_cheaper_hashed() {
        // Ten groups of 1000 rows: the HashAggregate and a Sort of its ten
        // rows undercut the Sort of all 1000, as PostgreSQL chooses.
        let scan = (0.0, 15.0, 1000.0, 3);
        let unique = unique_estimate(sort_estimate(scan, 4096), 1, 10.0);
        let hashed = sort_estimate(hashed_distinct_estimate(scan, 1, 10.0), 4096);
        assert!(hashed.1 < unique.1);
    }
}

/// A plain Aggregate's estimate over its input's (RP-321), for `n` counts:
/// PostgreSQL 15's `cost_agg` with AGG_PLAIN (`costsize.c`). Each count's
/// transition function costs one `cpu_operator_cost` per input row and
/// none has a final function; the one output row costs `cpu_tuple_cost`.
/// Each result is an int8, eight bytes wide.
pub(crate) fn count_agg_estimate(input: Estimate, n: usize) -> Estimate {
    const CPU_OPERATOR_COST: f64 = 0.0025;
    const CPU_TUPLE_COST: f64 = 0.01;
    let (_, in_total, rows, _) = input;
    let startup = in_total + CPU_OPERATOR_COST * n as f64 * rows;
    (startup, startup + CPU_TUPLE_COST, 1.0, 8 * n as i64)
}

/// A HashAggregate's estimate for a GROUP BY of `ncols` keys with `n`
/// counts (RP-322): PostgreSQL 15's `cost_agg` with AGG_HASHED
/// (`costsize.c`), over its input's estimate. Every input row hashes each
/// key (one `cpu_operator_cost` a column) and runs each count's transition
/// function (one more); each of the `groups` output rows costs
/// `cpu_tuple_cost`, and a count has no final function. `width` is the
/// projected keys' width; each count adds an int8. The hash table's
/// spilling past `hash_mem` is not priced: the caller prices no plan with
/// that many groups.
pub(crate) fn hashed_count_agg_estimate(
    input: Estimate,
    ncols: usize,
    n: usize,
    groups: f64,
    width: i64,
) -> Estimate {
    let (startup, total) = secantus_pgplan::costsize::hashed_grouping_cost(
        input.1,
        input.2,
        groups,
        ncols,
        Some(secantus_pgplan::costsize::AggCosts::simple(n, 0)),
    );
    (startup, total, groups, width + 8 * n as i64)
}

#[cfg(test)]
mod hashed_agg_tests {
    use super::hashed_count_agg_estimate;

    #[test]
    fn a_grouped_count_hashes_each_input_row() {
        // PostgreSQL 15.19: `select b, count(*) from e group by b` over a
        // Seq Scan of (0.00..15.00 rows=1000 width=3), ten groups, is
        // (20.00..20.10 rows=10 width=11).
        let (s, t, r, w) = hashed_count_agg_estimate((0.0, 15.0, 1000.0, 3), 1, 1, 10.0, 3);
        assert_eq!(
            (format!("{s:.2}"), format!("{t:.2}"), r, w),
            ("20.00".into(), "20.10".into(), 10.0, 11)
        );
    }
}

/// A `generate_series` Function Scan's estimate (RP-327): PostgreSQL 15's
/// `cost_functionscan` (`costsize.c`). The row count is the support
/// function's (`generate_series_int4_support` / `_int8_support`, which
/// answer for constant arguments): `floor((stop - start) / step) + 1`, none
/// when that is negative, clamped to at least one row
/// (`expression_returns_set_rows`; PostgreSQL 15.19 prints `rows=1` for
/// `generate_series(5, 1)`). The function call's own
/// cost (`procost` 1, one `cpu_operator_cost`) is paid at startup; each row
/// then costs `cpu_tuple_cost`. Each of `columns` output entries is the
/// series' type wide: four bytes for int4, eight for int8.
pub(crate) fn series_scan_estimate(series: &secantus_pgplan::Series, columns: usize) -> Estimate {
    const CPU_OPERATOR_COST: f64 = 0.0025;
    let nsteps = ((series.stop as f64 - series.start as f64) / series.step as f64).floor() + 1.0;
    let rows = nsteps.max(0.0);
    // clamp_row_est
    let rows = if rows <= 1.0 { 1.0 } else { rows.round() };
    let startup = CPU_OPERATOR_COST;
    let width = if series.int8 { 8 } else { 4 } * columns as i64;
    (
        startup,
        startup + costsize::CPU_TUPLE_COST * rows,
        rows,
        width,
    )
}

#[cfg(test)]
mod series_tests {
    use super::series_scan_estimate;

    fn series(start: i64, stop: i64, step: i64, int8: bool) -> secantus_pgplan::Series {
        secantus_pgplan::Series {
            start,
            stop,
            step,
            column: "generate_series".into(),
            int8,
        }
    }

    fn shown(e: super::Estimate) -> (String, String, f64, i64) {
        (format!("{:.2}", e.0), format!("{:.2}", e.1), e.2, e.3)
    }

    #[test]
    fn ten_steps_are_ten_rows() {
        // PostgreSQL 15.19: `select * from generate_series(1, 10)` is
        // (cost=0.00..0.10 rows=10 width=4).
        assert_eq!(
            shown(series_scan_estimate(&series(1, 10, 1, false), 1)),
            ("0.00".into(), "0.10".into(), 10.0, 4)
        );
    }

    #[test]
    fn an_empty_series_is_one_row_and_a_bigint_is_eight_bytes() {
        // PostgreSQL 15.19: `generate_series(5, 1)` is (0.00..0.01 rows=1
        // width=4) and `generate_series(1::int8, 300)` (0.00..3.00 rows=300
        // width=8).
        assert_eq!(
            shown(series_scan_estimate(&series(5, 1, 1, false), 1)),
            ("0.00".into(), "0.01".into(), 1.0, 4)
        );
        assert_eq!(
            shown(series_scan_estimate(&series(1, 300, 1, true), 1)),
            ("0.00".into(), "3.00".into(), 300.0, 8)
        );
    }
}

#[cfg(test)]
mod agg_tests {
    use super::count_agg_estimate;

    #[test]
    fn a_count_prices_one_transition_per_input_row() {
        // PostgreSQL 15.19: `select count(*) from e` over a Seq Scan of
        // (0.00..15.00 rows=1000 width=0) is (17.50..17.51 rows=1 width=8).
        let (s, t, r, w) = count_agg_estimate((0.0, 15.0, 1000.0, 0), 1);
        assert_eq!(
            (format!("{s:.2}"), format!("{t:.2}"), r, w),
            ("17.50".into(), "17.51".into(), 1.0, 8)
        );
    }
}

/// A WindowAgg's estimate over its input's (RP-323), for `n`
/// argument-less ranking functions (`row_number`, `rank`, `dense_rank`)
/// over an empty window: PostgreSQL 15's `cost_windowagg` (`costsize.c`).
/// Each function costs its `procost` (1) times `cpu_operator_cost` per
/// input row, there are no partition or order columns to compare, and
/// each row passed up costs `cpu_tuple_cost`; the startup is the input's.
/// The rows are the input's, each wider by the functions' int8 results.
///
/// `key_cols` is the window's partition plus order columns (RP-347), each
/// compared once per input row at `cpu_operator_cost`.
///
/// `results` is each window function's result type (RP-351): every
/// built-in window function and aggregate has `procost` 1, and an argument
/// that is a plain column is a `Var`, which costs nothing to evaluate; each
/// result widens the row by its type's `get_typavgwidth`.
///
/// `arg_ops` is the operators the functions' arguments and FILTER clauses
/// evaluate per row, together (RP-352): `cost_windowagg` adds each one's
/// `cost_qual_eval_node` to the function's per-row cost.
pub(crate) fn window_agg_estimate(
    input: Estimate,
    results: &[&str],
    arg_ops: u32,
    key_cols: usize,
) -> Estimate {
    const CPU_OPERATOR_COST: f64 = 0.0025;
    const CPU_TUPLE_COST: f64 = 0.01;
    let (startup, in_total, rows, width) = input;
    let n = results.len();
    let total = in_total
        + CPU_OPERATOR_COST * (n as f64 + f64::from(arg_ops)) * rows
        + CPU_OPERATOR_COST * key_cols as f64 * rows
        + CPU_TUPLE_COST * rows;
    let widened: i64 = results.iter().map(|t| type_width(t, -1)).sum();
    (startup, total, rows, width + widened)
}

#[cfg(test)]
mod window_tests {
    use super::window_agg_estimate;

    #[test]
    fn a_filter_clause_costs_its_operators_per_row() {
        // PostgreSQL 15.19: `select a, sum(a) filter (where a > 5) over ()
        // from e` over a Seq Scan of (0.00..15.00 rows=1000 width=4) is
        // (0.00..30.00 rows=1000 width=12): the `>` is one more
        // `cpu_operator_cost` a row (RP-352).
        let (s, t, r, w) = window_agg_estimate((0.0, 15.0, 1000.0, 4), &["int8"], 1, 0);
        assert_eq!(
            (format!("{s:.2}"), format!("{t:.2}"), r, w),
            ("0.00".into(), "30.00".into(), 1000.0, 12)
        );
    }

    #[test]
    fn a_lag_widens_the_row_by_its_argument_type() {
        // PostgreSQL 15.19: `select a, lag(a) over (order by a) from e`
        // over a Sort of (64.83..67.33 rows=1000 width=4) is
        // (64.83..82.33 rows=1000 width=8): lag's result is int4 (RP-351).
        let (s, t, r, w) = window_agg_estimate((64.83, 67.33, 1000.0, 4), &["int4"], 0, 1);
        assert_eq!(
            (format!("{s:.2}"), format!("{t:.2}"), r, w),
            ("64.83".into(), "82.33".into(), 1000.0, 8)
        );
    }

    #[test]
    fn an_ordered_window_compares_its_key_once_per_row() {
        // PostgreSQL 15.19: `select a, row_number() over (order by a) from e`
        // over a Sort of (64.83..67.33 rows=1000 width=4) is
        // (64.83..82.33 rows=1000 width=12) (RP-347).
        let (s, t, r, w) = window_agg_estimate((64.83, 67.33, 1000.0, 4), &["int8"], 0, 1);
        assert_eq!(
            (format!("{s:.2}"), format!("{t:.2}"), r, w),
            ("64.83".into(), "82.33".into(), 1000.0, 12)
        );
    }

    #[test]
    fn a_row_number_prices_one_call_and_one_tuple_per_row() {
        // PostgreSQL 15.19: `select a, row_number() over () from e` over a
        // Seq Scan of (0.00..15.00 rows=1000 width=4) is
        // (0.00..27.50 rows=1000 width=12).
        let (s, t, r, w) = window_agg_estimate((0.0, 15.0, 1000.0, 4), &["int8"], 0, 0);
        assert_eq!(
            (format!("{s:.2}"), format!("{t:.2}"), r, w),
            ("0.00".into(), "27.50".into(), 1000.0, 12)
        );
    }
}

#[cfg(test)]
mod sort_tests {
    use super::sort_estimate;

    #[test]
    fn a_sort_prices_its_comparisons_over_the_whole_input() {
        // PostgreSQL 15.19: `select * from e order by b` over a Seq Scan of
        // (0.00..15.00 rows=1000 width=7) is (64.83..67.33).
        let (s, t, r, w) = sort_estimate((0.0, 15.0, 1000.0, 7), 4096);
        assert_eq!(
            (format!("{s:.2}"), format!("{t:.2}"), r, w),
            ("64.83".into(), "67.33".into(), 1000.0, 7)
        );
    }

    #[test]
    fn a_sort_past_work_mem_prices_its_merge_pages() {
        // PostgreSQL 15.19, `work_mem = 4MB`: 200000 rows of width 7 over a
        // Seq Scan of (0.00..2885.00) is (23231.64..23731.64); with 8MB it
        // sorts in memory, (20494.64..20994.64).
        let (s, t, _, _) = sort_estimate((0.0, 2885.0, 200000.0, 7), 4096);
        assert_eq!(
            (format!("{s:.2}"), format!("{t:.2}")),
            ("23231.64".into(), "23731.64".into())
        );
        let (s, t, _, _) = sort_estimate((0.0, 2885.0, 200000.0, 7), 8192);
        assert_eq!(
            (format!("{s:.2}"), format!("{t:.2}")),
            ("20494.64".into(), "20994.64".into())
        );
    }

    #[test]
    fn a_sort_of_one_row_counts_two() {
        // `cost_tuplesort` sorts at least two tuples; the rows stay one.
        let (s, t, r, _) = sort_estimate((0.0, 1.0, 1.0, 4), 4096);
        assert!((s - 1.01).abs() < 1e-9 && (t - 1.015).abs() < 1e-9 && r == 1.0);
    }
}

#[cfg(test)]
mod limit_tests {
    use super::limit_estimate;

    #[test]
    fn a_limit_prices_its_share_of_the_input() {
        // PostgreSQL 15.19: `select * from e limit 5` over a Seq Scan of
        // (0.00..15.00 rows=1000 width=7) is (0.00..0.07 rows=5 width=7).
        let (s, t, r, w) = limit_estimate((0.0, 15.0, 1000.0, 7), 0, 5);
        assert_eq!(
            (format!("{s:.2}"), format!("{t:.2}"), r, w),
            ("0.00".into(), "0.07".into(), 5.0, 7)
        );
        // `offset 100 limit 5`: (1.50..1.57 rows=5).
        let (s, t, r, _) = limit_estimate((0.0, 15.0, 1000.0, 7), 100, 5);
        assert_eq!(
            (format!("{s:.2}"), format!("{t:.2}"), r),
            ("1.50".into(), "1.57".into(), 5.0)
        );
        // A limit past the input keeps the input's rows.
        let (_, t, r, _) = limit_estimate((0.0, 15.0, 1000.0, 7), 0, 5000);
        assert_eq!((t, r), (15.0, 1000.0));
    }
}

/// The built-in operators whose every PostgreSQL 15 implementation over
/// the built-in types is a function of `procost` 1 (`pg_proc.dat`): one
/// `cpu_operator_cost` per evaluation.
const UNIT_COST_OPERATORS: &[&str] = &[
    "+", "-", "*", "/", "%", "||", "=", "<>", "<", ">", "<=", ">=",
];

/// `cost_qual_eval_node`'s per-row cost, in operators, of a select-list
/// expression over one row (RP-333): each operator one `cpu_operator_cost`
/// (its function's `procost`), a column, a constant or a parameter free. A
/// subtree reading no column is folded to a constant by
/// `eval_const_expressions` and costs nothing. `None` for any node not
/// modelled here: a cast (a relabelling, a cast function or an I/O
/// conversion, priced differently), a function call, an operator not
/// known to cost one.
fn row_expr_ops(n: &pg_query::protobuf::Node, nparams: usize) -> Option<(u32, bool)> {
    use pg_query::protobuf::node::Node as N;
    match n.node.as_ref()? {
        N::AConst(_) => Some((0, false)),
        N::ParamRef(p) => Some((0, usize::try_from(p.number).ok()? > nparams)),
        N::AExpr(e) if e.kind == pg_query::protobuf::AExprKind::AexprOp as i32 => {
            let op = match e.name.last()?.node.as_ref()? {
                N::String(s) => s.sval.as_str(),
                _ => return None,
            };
            if !UNIT_COST_OPERATORS.contains(&op) {
                return None;
            }
            let (mut ops, mut reads) = (0, false);
            for side in [e.lexpr.as_deref(), e.rexpr.as_deref()]
                .into_iter()
                .flatten()
            {
                let (o, r) = row_expr_ops(side, nparams)?;
                ops += o;
                reads |= r;
            }
            Some((if reads { ops + 1 } else { 0 }, reads))
        }
        _ => None,
    }
}

/// The stored fields a row expression reads: the `ParamRef`s past the
/// statement's parameters, which `row_column_expr` binds to `fields`. Walks
/// only the nodes `row_expr_ops` prices.
fn row_expr_reads(
    n: &pg_query::protobuf::Node,
    nparams: usize,
    fields: &[secantus_pgplan::RowField],
    out: &mut Vec<String>,
) -> Option<()> {
    use pg_query::protobuf::node::Node as N;
    match n.node.as_ref()? {
        N::AConst(_) => Some(()),
        N::ParamRef(p) => {
            let i = usize::try_from(p.number).ok()?;
            if i > nparams {
                let (_, field, _) = fields.get(i - nparams - 1)?;
                if !out.contains(field) {
                    out.push(field.clone());
                }
            }
            Some(())
        }
        N::AExpr(e) => {
            for side in [e.lexpr.as_deref(), e.rexpr.as_deref()]
                .into_iter()
                .flatten()
            {
                row_expr_reads(side, nparams, fields, out)?;
            }
            Some(())
        }
        _ => None,
    }
}

/// `cost_qual_eval_node`'s per-row cost, in operators, of an expression a
/// window function evaluates per input row -- its argument or its FILTER
/// (RP-352) -- and the stored fields it reads, which the scan below the
/// WindowAgg returns. `None` for an expression not priced here (see
/// `row_expr_ops`), or one over a type not built in.
pub(crate) fn column_expr_cost(e: &secantus_pgplan::ColumnExpr) -> Option<(u32, Vec<String>)> {
    let secantus_pgplan::ColumnExpr::Row {
        expr,
        fields,
        params,
        result_type,
        ..
    } = e
    else {
        return None;
    };
    let mut read = Vec::new();
    row_expr_reads(expr, params.len(), fields, &mut read)?;
    let builtin = |t: &str| secantus_pgplan::pgtypes::oid_of_name(t).is_some_and(|oid| oid < 10000);
    if !builtin(result_type)
        || read
            .iter()
            .any(|r| !fields.iter().any(|(_, f, t)| f == r && builtin(t)))
    {
        return None;
    }
    let (ops, _) = row_expr_ops(expr, params.len())?;
    Some((ops, read))
}

/// A select list's cost and width beside its plain columns (RP-333):
/// `(per-row cost, width)` of its computed entries, as
/// `set_pathtarget_cost_width` sets them -- an expression's width is its
/// type's `get_typavgwidth` -- and the plain columns' stored fields, whose
/// widths the scan measures. `None` where an entry is not priced.
pub(crate) fn target_cost(sel: &secantus_pgplan::Select) -> Option<(f64, i64, Vec<String>)> {
    use secantus_pgplan::ColumnExpr;
    const CPU_OPERATOR_COST: f64 = 0.0025;
    let (mut cost, mut width, mut fields) = (0.0, 0, Vec::new());
    for (i, (_, field)) in sel.columns.iter().enumerate() {
        match sel.casts.get(i).and_then(Option::as_ref) {
            None => fields.push(field.clone()),
            Some(ColumnExpr::Const { result_type, .. }) => {
                width += type_width(result_type, -1);
            }
            Some(ColumnExpr::Row {
                expr,
                fields: read,
                params,
                result_type,
                ..
            }) => {
                if read.iter().any(|(_, _, t)| {
                    secantus_pgplan::pgtypes::oid_of_name(t).is_none_or(|oid| oid >= 10000)
                }) || secantus_pgplan::pgtypes::oid_of_name(result_type)
                    .is_none_or(|oid| oid >= 10000)
                {
                    return None;
                }
                let (ops, _) = row_expr_ops(expr, params.len())?;
                cost += f64::from(ops) * CPU_OPERATOR_COST;
                width += type_width(result_type, -1);
            }
            Some(_) => return None,
        }
    }
    Some((cost, width, fields))
}

#[cfg(test)]
mod target_tests {
    use super::row_expr_ops;

    fn target(sql: &str) -> pg_query::protobuf::Node {
        use pg_query::protobuf::node::Node as N;
        let tree = pg_query::parse(sql).unwrap().protobuf;
        let Some(N::SelectStmt(s)) = tree.stmts[0].stmt.as_ref().unwrap().node.as_ref() else {
            panic!("not a select");
        };
        let Some(N::ResTarget(t)) = s.target_list[0].node.as_ref() else {
            panic!("not a target");
        };
        *t.val.clone().unwrap()
    }

    #[test]
    fn each_operator_over_a_column_costs_one() {
        // `$1` is the statement's parameter, `$2` a column.
        assert_eq!(
            row_expr_ops(&target("select $2 * 2 + 1"), 1),
            Some((2, true))
        );
        // Folded by `eval_const_expressions`: free.
        assert_eq!(row_expr_ops(&target("select $1 + 2"), 1), Some((0, false)));
        // Not modelled: a cast, a function call.
        assert_eq!(row_expr_ops(&target("select $2::text"), 1), None);
        assert_eq!(row_expr_ops(&target("select abs($2)"), 1), None);
    }
}

#[cfg(test)]
mod append_tests {
    use super::append_estimate;

    fn shown(e: (f64, f64, f64, i64)) -> (String, String, f64, i64) {
        (format!("{:.2}", e.0), format!("{:.2}", e.1), e.2, e.3)
    }

    #[test]
    fn an_append_adds_half_a_tuple_cost_per_row() {
        // PostgreSQL 15.19: `select a from e union all select a from k`,
        // two analysed 1000-row tables, is (0.00..40.00 rows=2000 width=4).
        let scan = (0.0, 15.0, 1000.0, 4);
        assert_eq!(
            shown(append_estimate(&[(scan, 15.0), (scan, 15.0)]).unwrap()),
            ("0.00".into(), "40.00".into(), 2000.0, 4)
        );
    }

    #[test]
    fn a_filtered_side_is_priced_through_its_subquery_scan() {
        // PostgreSQL 15.19: `select a from e where a < 100 union all select
        // a from k` is (0.00..38.98 rows=1099); the filtered side's 17.50
        // carries a cpu_tuple_cost per row of the Subquery Scan setrefs
        // removes. Widths 3 and 7 weigh by rows: (99*3 + 1000*7) / 1099.
        let filtered = (0.0, 17.5, 99.0, 3);
        let plain = (0.0, 15.0, 1000.0, 7);
        assert_eq!(
            shown(append_estimate(&[(filtered, 17.5 + 0.99), (plain, 15.0)]).unwrap()),
            ("0.00".into(), "38.98".into(), 1099.0, 7)
        );
    }
}

thread_local! {
    /// The EXPLAIN being estimated: its single relation's top-level
    /// `col IN (...)` lists, each column's stored field and the list's
    /// length with its NULL elements (RP-349).
    static EXPLAIN_IN_LENGTHS: std::cell::RefCell<Vec<(String, usize)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Holds an EXPLAIN's `IN` list lengths until it is dropped.
pub(crate) struct InLengthsGuard(());

impl Drop for InLengthsGuard {
    fn drop(&mut self) {
        EXPLAIN_IN_LENGTHS.with(|c| c.borrow_mut().clear());
    }
}

/// Make `lengths` (`ExplainOptions::scan_in_lengths`) what this EXPLAIN's
/// scans are priced with, until the guard is dropped.
pub(crate) fn explain_in_lengths(lengths: &[(String, usize)]) -> InLengthsGuard {
    EXPLAIN_IN_LENGTHS.with(|c| *c.borrow_mut() = lengths.to_vec());
    InLengthsGuard(())
}

/// An `IN` list's cost counts every element, NULLs included
/// (`estimate_array_length`), though the lowered filter -- where a NULL
/// would match a NULL column -- drops them; its selectivity keeps counting
/// only the non-NULL ones (RP-349).
fn with_in_lengths(mut quals: Vec<(String, costsize::Qual)>) -> Vec<(String, costsize::Qual)> {
    EXPLAIN_IN_LENGTHS.with(|c| {
        let lengths = c.borrow();
        for (field, q) in &mut quals {
            if let costsize::Qual::In(len, nonnull) = q {
                if let Some((_, full)) = lengths.iter().find(|(f, n)| f == field && *n > *len) {
                    *q = costsize::Qual::In(*full, *nonnull);
                }
            }
        }
    });
    quals
}

/// A side of a priced hash join: its scan's estimate, the width it
/// carries above the join, its key column's type and statistics
/// (distinct count, null fraction, MCVs), whether the key is unique, and
/// the table's tuples.
struct HashSide {
    scan: Estimate,
    above: i64,
    pg_type: String,
    stats: (f64, f64, Vec<(Vec<u8>, f32)>),
    unique: bool,
    tuples: f64,
}

/// `cost_hashjoin` over two priced sides, the hashed one PostgreSQL's
/// `add_path` keeps (RP-330, RP-359): the printed outer scan's, the hashed
/// scan's and the join's estimates, and whether the left table is hashed.
/// With `left_join`, `a` is the preserved side: the join keeps at least
/// its rows (`calc_joinrel_size_estimate`), and the swapped path is the
/// `Hash Right Join` PostgreSQL builds the other way round.
fn hash_join_of(
    a: HashSide,
    b: HashSide,
    left_join: bool,
) -> Option<(Estimate, Estimate, Estimate, bool)> {
    use secantus_pgplan::joincost::{self, JoinSide, KeyStats};
    if a.pg_type != b.pg_type {
        return None;
    }
    fn key(s: &HashSide) -> KeyStats<'_> {
        KeyStats {
            ndistinct: s.stats.0,
            default_ndistinct: false,
            null_frac: s.stats.1,
            mcv: &s.stats.2,
        }
    }
    let (ka, kb) = (key(&a), key(&b));
    let selectivity = joincost::eqjoinsel_inner(&ka, &kb);
    let join_side = |s: &HashSide| JoinSide {
        rows: s.scan.2,
        tuples: s.tuples,
        scan: (s.scan.0, s.scan.1),
        startup_scan: (s.scan.0, s.scan.1),
        ordered: None,
        scan_ordered: false,
        unique: s.unique,
        ndistinct: s.stats.0,
        default_ndistinct: false,
        key_ops: 0.0,
        rel_rows: s.scan.2,
        width: Some(s.scan.3),
        extra: joincost::ExtraKeys::default(),
    };
    let (outer, inner) = (join_side(&a), join_side(&b));
    // A side unique on the key is `inner_unique` when hashed, and
    // PostgreSQL may then prefer another plan; not modelled here.
    if outer.unique || inner.unique {
        return None;
    }
    let (startup, total) = joincost::hash_join_stats(&outer, &inner, selectivity, &kb);
    // PostgreSQL builds the path hashing the right relation first
    // (`add_paths_to_joinrel` with the left one outer) and then the
    // other way round; `add_path` keeps the second only when it is
    // cheaper beyond the fuzz factor (`compare_path_costs_fuzzily`,
    // 1.01), and `set_cheapest` then takes the cheaper total. The
    // plan printed follows it: with `swap`, the left table hashed
    // (RP-359).
    let (l_startup, l_total) = joincost::hash_join_stats(&inner, &outer, selectivity, &ka);
    let fuzzy_eq = |x: f64, y: f64| x <= y * 1.01 && y <= x * 1.01;
    let right_kept = total <= l_total || (fuzzy_eq(startup, l_startup) && fuzzy_eq(total, l_total));
    let mut rows = outer.rows * inner.rows * selectivity;
    if left_join {
        rows = rows.max(outer.rows);
    }
    let rows = costsize::clamp_row_est(rows);
    let width = a.above + b.above;
    Some(if right_kept {
        (a.scan, b.scan, (startup, total, rows, width), false)
    } else {
        (b.scan, a.scan, (l_startup, l_total, rows, width), true)
    })
}
