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

/// One index's scan of a BitmapOr arm (RP-378): the index with the arm's
/// later-column selectivities, the arm's quals on it, the leading column's
/// selectivity, and the clauses those quals are (by position).
type ArmScan = (
    secantus_pgplan::costsize::Index,
    Vec<(String, secantus_pgplan::costsize::Qual)>,
    f64,
    Vec<usize>,
);

/// The index an index scan reads, and whether the scan is index-only (the
/// index holds every column the statement reads, `check_index_only`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScanIndex {
    pub name: String,
    pub only: bool,
    /// A bitmap heap scan over the index: its Bitmap Index Scan's
    /// estimate (RP-338), or with `or_arms` its BitmapOr's.
    pub bitmap: Option<Estimate>,
    /// A BitmapOr's arms (RP-378): each one Bitmap Index Scan, its index
    /// and estimate. Empty for any other scan.
    pub or_arms: Vec<(String, Estimate)>,
}

/// [`PgHandler::upper_scan`]'s answer: the scan's estimate, its index
/// path (none for the Seq Scan) and the presorted index scans'
/// `(startup, total)`.
pub(crate) type UpperScan = (Estimate, Option<ScanIndex>, Vec<(f64, f64)>);

/// What a node over a user table's scan asks of it (RP-382).
#[derive(Debug, Default)]
pub(crate) struct PathWant {
    /// A LIMIT's rows (its count plus its OFFSET): the path is
    /// `get_cheapest_fractional_path`'s.
    pub limit: Option<f64>,
    /// An ORDER BY's stored fields, in order and one direction: the index
    /// scans that return them in that order are priced too.
    pub order: Vec<String>,
}

/// A user table's chosen scan and its presorted alternatives (RP-382).
#[derive(Debug)]
pub(crate) struct UserPath {
    /// The path chosen; `None` from `user_paths` is the sequential scan.
    pub chosen: Option<ScanEstimate>,
    /// Each index scan in the ORDER BY's order: `(startup, total)`.
    pub ordered: Vec<(f64, f64)>,
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
pub(crate) fn type_width(pg_type: &str, typmod: i32) -> i64 {
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
        let defs = self.shared_table_defs().ok()?;
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
    /// [`joincost::cheapest_nest_loop_param`]'s: over an analysed table
    /// whose B-tree leads with a column an inequality compares to the other
    /// side's, that side's index scan parameterized by the outer row is
    /// weighed as the inner (RP-354), and the fifth element names it when
    /// it wins. A LEFT join's preserved side, filtered by its WHERE, is
    /// that relation's cheapest path (`set_cheapest`), an index path where
    /// that wins, which the last element names (RP-354). `None` -- zeros
    /// -- where an index the model does not price could serve, or anything
    /// else sits in the plan.
    pub(crate) fn nest_loop_estimate(
        &self,
        sel: &secantus_pgplan::Select,
    ) -> Option<NestLoopEstimate> {
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
        {
            return None;
        }
        let JoinNode::Join {
            kind,
            left,
            right,
            on,
            on_filters,
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
        // An outer join's ON is its join clause. A WHERE conjunct over its
        // nullable side that rejects NULLs has made the join inner already
        // (`reduce_outer_joins`); one over its preserved side alone is
        // pushed to that side's scan (`distribute_qual_to_rels`), its
        // Filter, and is modelled there (RP-354). Any other WHERE runs
        // above the join, which is not.
        let left_join = match kind {
            JoinKind::Inner => false,
            JoinKind::Left => {
                let JoinNode::Leaf { def, .. } = left.as_ref() else {
                    return None;
                };
                let prefix = format!("{}{}", def.name, secantus_pgplan::joins::SEP);
                let mut fields = Vec::new();
                filter_fields(&sel.filter, &mut fields)?;
                if !j.where_filters.is_empty()
                    || sel.residual.is_some()
                    || fields.iter().any(|f| !f.starts_with(&prefix))
                {
                    return None;
                }
                true
            }
            _ => return None,
        };
        // The join's clauses: an ON's conjuncts (RP-354), then the
        // WHERE's; `distribute_qual_to_rels` puts both on the join.
        let filters: Vec<&secantus_pgplan::joins::JoinFilter> = on_filters
            .iter()
            .filter(|_| on.is_some())
            .chain(j.where_filters.iter())
            .collect();
        // An ON conjunct reading a LEFT join's preserved side only stays
        // the join's filter, a restriction clause `clause_selectivity`
        // estimates from that table's statistics (RP-354). One over an
        // inner join's side is pushed to its scan, which is not modelled
        // here; a LEFT join's nullable side's was pushed already.
        let preserved = match left.as_ref() {
            JoinNode::Leaf { def, .. } if left_join => Some(def.name.as_str()),
            _ => None,
        };
        let restriction = |f: &secantus_pgplan::joins::JoinFilter| {
            f.restrict.is_some() && f.aliases.len() == 1 && Some(f.aliases[0].as_str()) == preserved
        };
        if filters.iter().any(|f| {
            !restriction(f)
                && (!matches!(
                    f.kind,
                    FilterKind::Inequality
                        | FilterKind::NotEqual(..)
                        | FilterKind::NotEqualCast(..)
                ) || f.aliases.len() < 2)
        }) {
            return None;
        }
        // A `<`, `<=`, `>` or `>=` between the sides can be an indexed
        // side's parameterized Index Cond (RP-354), priced below where it
        // compares two plain keys; a `<>` never is one (no B-tree
        // strategy). Any other inequality could be one only where an
        // operand is that side's key (or the index is over an
        // expression), which is not priced: an expression of the side's
        // keys (`e.a < k.a + 1`) matches no column index, so it stays the
        // join's filter over that side's plain scan. A LEFT join's
        // preserved side is never parameterized, so never one.
        let opaque_keys: Vec<&str> = filters
            .iter()
            .filter(|f| {
                !restriction(f) && matches!(f.kind, FilterKind::Inequality) && f.compare.is_none()
            })
            .flat_map(|f| f.inequality_keys.iter().map(String::as_str))
            .collect();
        let opaque_inequality = filters.iter().any(|f| {
            !restriction(f) && matches!(f.kind, FilterKind::Inequality) && f.compare.is_none()
        });
        let indexes = self.index_relations();
        let project = j.project.as_deref();
        // Each joined key's table and field, for `neqjoinsel`.
        let mut key_columns: Vec<(String, String, String)> = Vec::new();
        // Each side's table, its keys' stored fields and SQL names, and
        // the fields it reads.
        let mut leaves: Vec<SideLeaf> = Vec::new();
        // `preserved_side`: a LEFT join's outer relation, always the
        // loop's outer, its own cheapest path whatever it is.
        let mut side = |node: &JoinNode,
                        preserved_side: bool|
         -> Option<(Estimate, i64, Option<ScanIndex>)> {
            let JoinNode::Leaf { plan, def, columns } = node else {
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
                // An indexed table's own filter may be an index scan's,
                // priced below only on a LEFT join's preserved side.
                || (indexes.iter().any(|ix| ix.table.name == leaf.table)
                    && ((!preserved_side
                        && opaque_inequality
                        && (columns.iter().any(|(k, _)| opaque_keys.contains(&k.as_str()))
                            || indexes
                                .iter()
                                .any(|ix| ix.table.name == leaf.table && ix.keys.contains(&0))))
                        || (!leaf.filter.is_empty() && !preserved_side)))
            {
                return None;
            }
            // A leaf row's field as the table stores it: a primary key's
            // column is the stored `_id`.
            let table = self.lookup(&leaf.table)?;
            let stored = |f: &str| -> Option<String> {
                let c = def.columns.iter().find(|c| c.field() == f)?;
                table.field_of(&c.name)
            };
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
            let (scan, index) = self.scan_estimate(&leaf.table, &leaf.filter, &read)?;
            if index.is_some() && !preserved_side {
                return None;
            }
            let (width, None) = self.scan_estimate(&leaf.table, &Document::new(), &above)? else {
                return None;
            };
            let mut keys = Vec::new();
            for (key, f) in columns {
                key_columns.push((key.clone(), leaf.table.clone(), stored(f)?));
                let name = def.columns.iter().find(|c| c.field() == *f)?.name.clone();
                keys.push((key.clone(), stored(f)?, name));
            }
            leaves.push(SideLeaf {
                table: leaf.table.clone(),
                keys,
                read,
            });
            Some((scan, width.3, index))
        };
        let (a, a_above, a_index) = side(left, left_join)?;
        let (b, b_above, _) = side(right, false)?;
        let qual: f64 = filters
            .iter()
            .map(|f| f.ops as f64 * costsize::CPU_OPERATOR_COST)
            .sum();
        // `clauselist_selectivity`: the conjuncts' product, in order. A
        // `<>` (or IS DISTINCT FROM) is `neqjoinsel`, one minus
        // `eqjoinsel` over the two columns' statistics; one never
        // analysed is not modelled here. A restriction is its table's
        // `clause_selectivity`, except that one bound of a column against
        // a constant joins the range clauses (`addRangeClause`), which are
        // multiplied in last, a column's two bounds as one range.
        let mut sel_factor = 1.0_f64;
        let mut ranges: Vec<Document> = Vec::new();
        for f in &filters {
            if let (true, Some(r)) = (restriction(f), &f.restrict) {
                if single_bound(r) {
                    ranges.push(r.clone());
                } else {
                    sel_factor *= self.restriction_selectivity(&leaves.first()?.table, r)?;
                }
                continue;
            }
            let s = match &f.kind {
                FilterKind::Inequality => 1.0 / 3.0,
                FilterKind::NotEqual(l, r) => {
                    let stats = |k: &str| {
                        let (_, table, field) = key_columns.iter().find(|(c, _, _)| c == k)?;
                        self.analysed_key(table, field)
                    };
                    let (x, y) = (stats(l)?, stats(r)?);
                    if x.column.pg_type != y.column.pg_type {
                        return None;
                    }
                    fn key(s: &AnalysedKey) -> secantus_pgplan::joincost::KeyStats<'_> {
                        secantus_pgplan::joincost::KeyStats {
                            ndistinct: s.ndistinct,
                            default_ndistinct: false,
                            null_frac: s.null_frac,
                            mcv: &s.mcv,
                        }
                    }
                    let eq = secantus_pgplan::joincost::eqjoinsel_inner(&key(&x), &key(&y));
                    1.0 - eq.clamp(0.0, 1.0)
                }
                // A cast key is an expression with no statistics: its
                // distinct count `get_variable_numdistinct`'s default, no
                // NULLs and no MCV list.
                FilterKind::NotEqualCast(l, r) => {
                    let (x, y) = (
                        ne_operand(self, &key_columns, l)?,
                        ne_operand(self, &key_columns, r)?,
                    );
                    if x.type_oid != y.type_oid {
                        return None;
                    }
                    let eq = secantus_pgplan::joincost::eqjoinsel_inner(&x.stats(), &y.stats());
                    1.0 - eq.clamp(0.0, 1.0)
                }
                _ => return None,
            };
            sel_factor *= s;
        }
        if !ranges.is_empty() {
            let filter = match ranges.len() {
                1 => ranges.remove(0),
                _ => bson::doc! { "$and": ranges },
            };
            sel_factor *= self.restriction_selectivity(&leaves.first()?.table, &filter)?;
        }
        // Each side as the inner of a nested loop through its index,
        // parameterized by the other side's row: `param[0]` is `b` inner
        // (a LEFT join's nullable side), `param[1]` is `a`.
        let mut param: [Option<secantus_pgplan::joincost::ParamInner>; 2] = [None, None];
        let mut param_scans: [Option<ParamScan>; 2] = [None, None];
        let [la, lb] = leaves.as_slice() else {
            return None;
        };
        // A restriction on the outer side is not movable into the inner's
        // scan (`join_clause_is_movable_into`): it stays the join's
        // filter, checked on each pair the scan returns.
        let movable: Vec<&secantus_pgplan::joins::JoinFilter> = filters
            .iter()
            .copied()
            .filter(|f| !restriction(f))
            .collect();
        let kept: f64 = filters
            .iter()
            .filter(|f| restriction(f))
            .map(|f| f.ops as f64 * costsize::CPU_OPERATOR_COST)
            .sum();
        for (slot, (inner_leaf, outer_rows, inner_rows)) in
            [(lb, a.2, b.2), (la, b.2, a.2)].into_iter().enumerate()
        {
            if left_join && slot == 1 {
                break;
            }
            // `get_parameterized_baserel_size`: the join clauses moved
            // into the scan, estimated as restrictions of the inner.
            let param_sel = param_selectivity(self, inner_leaf, &movable, &key_columns)?;
            let found = self.param_index_inner(
                inner_leaf,
                &movable,
                outer_rows,
                inner_rows * param_sel,
                inner_rows,
            );
            match found {
                ParamFound::None => {}
                ParamFound::Unmodelled => return None,
                ParamFound::Scan(mut p, scan) => {
                    p.qual = kept;
                    param[slot] = Some(p);
                    param_scans[slot] = Some(scan);
                }
            }
        }
        let plan =
            secantus_pgplan::joincost::cheapest_nest_loop_param(a, b, qual, param, left_join);
        let rows = if left_join {
            // `calc_joinrel_size_estimate`'s JOIN_LEFT: never fewer rows
            // than the outer side.
            (a.2 * b.2 * sel_factor).max(a.2)
        } else {
            a.2 * b.2 * sel_factor
        };
        let rows = costsize::clamp_row_est(rows);
        let join = (plan.startup, plan.total, rows, a_above + b_above);
        let (a, b, scan) = if plan.param {
            let slot = usize::from(plan.b_outer);
            let p = param[slot]?;
            let est = |e: Estimate| (p.startup, p.total, p.rows, e.3);
            let scan = param_scans[slot].take();
            if plan.b_outer {
                (est(a), b, scan)
            } else {
                (a, est(b), scan)
            }
        } else {
            (a, b, None)
        };
        // The preserved side is the loop's outer, never moved.
        if a_index.is_some() && plan.b_outer {
            return None;
        }
        Some((plan, a, b, join, scan, a_index))
    }

    /// `leaf` as the inner side of a nested loop through one of its
    /// table's B-trees, its index qual one of `filters`' inequalities
    /// between its leading column and the outer side's key (RP-354):
    /// `create_index_paths`' join clauses, `get_loop_count` the outer
    /// side's `outer_rows`. Every other join clause moves into the scan
    /// too (`join_clause_is_movable_into`), checked on each row it fetches,
    /// so the join checks none and the scan returns `param_rows` -- the
    /// inner rows times every join clause's selectivity, as
    /// `get_parameterized_baserel_size` multiplies them; `inner_rows` is
    /// the plain scan's. [`ParamFound::Unmodelled`] where an index could serve
    /// in a way not priced here: a later key column, two bounds or a
    /// table never analysed.
    fn param_index_inner(
        &self,
        leaf: &SideLeaf,
        filters: &[&secantus_pgplan::joins::JoinFilter],
        outer_rows: f64,
        param_rows: f64,
        inner_rows: f64,
    ) -> ParamFound {
        let relations = self.index_relations();
        let table_indexes: Vec<_> = relations
            .iter()
            .filter(|ix| ix.table.name == leaf.table)
            .collect();
        if table_indexes.is_empty() {
            return ParamFound::None;
        }
        let key_fields = |ix: &crate::IndexRelation| -> Option<Vec<String>> {
            ix.keys
                .iter()
                .map(|k| {
                    let i = usize::try_from(*k).ok()?.checked_sub(1)?;
                    Some(ix.table.columns.get(i)?.field())
                })
                .collect()
        };
        // The comparisons naming one of this side's keys, as `(this side's
        // stored field, SQL column, its operator written this side first,
        // the filter's cost)`.
        let mut clauses = Vec::new();
        for f in filters {
            let Some((l, op, r)) = &f.compare else {
                continue;
            };
            let mine = |k: &str| leaf.keys.iter().find(|(key, _, _)| key == k);
            let (own, op) = match (mine(l), mine(r)) {
                (Some(_), Some(_)) => return ParamFound::Unmodelled,
                (Some(k), None) => (k, op.clone()),
                (None, Some(k)) => (k, commuted(op).to_string()),
                (None, None) => continue,
            };
            clauses.push((own.1.clone(), own.2.clone(), op, f.ops));
        }
        let mut serving = Vec::new();
        for ix in &table_indexes {
            let Some(fields) = key_fields(ix) else {
                return ParamFound::Unmodelled;
            };
            for (field, _, _, _) in &clauses {
                match fields.iter().position(|f| f == field) {
                    Some(0) => serving.push((*ix, field.clone())),
                    Some(_) => return ParamFound::Unmodelled,
                    None => {}
                }
            }
        }
        if serving.is_empty() {
            return ParamFound::None;
        }
        // One clause over one index (the primary key is listed as both
        // the SQL key and the storage's `_id` index: one relation).
        let field = serving[0].1.clone();
        if serving.iter().any(|(_, f)| *f != field)
            || clauses.iter().filter(|(f, _, _, _)| *f == field).count() != 1
        {
            return ParamFound::Unmodelled;
        }
        let Some(key) = self.analysed_key(&leaf.table, &field) else {
            return ParamFound::Unmodelled;
        };
        let Some(mut ki) = self.key_index(&leaf.table, &Document::new(), &field, &key, inner_rows)
        else {
            return ParamFound::Unmodelled;
        };
        // An index holding every column the side reads is scanned
        // index-only (`check_index_only`), its heap fetches cut by the
        // visibility map's all-visible fraction (`cost_index`); with
        // `enable_indexonlyscan` off it is a plain index scan.
        let index_only = !self.setting_is("enable_indexonlyscan", "off")
            && leaf.read.iter().all(|f| ki.index.columns.contains(f));
        if index_only {
            ki.index.visible = ki.allvisfrac;
        }
        let (_, column, op, ops) = clauses
            .iter()
            .find(|(f, _, _, _)| *f == field)
            .expect("counted above");
        let lower = op.starts_with('>');
        // The other join clauses, a filter on each row the scan fetches.
        let rest_ops = (filters.iter().map(|f| f.ops).sum::<usize>() - *ops) as f64;
        let rest: Vec<(String, costsize::Qual)> = if rest_ops > 0.0 {
            vec![(String::new(), costsize::Qual::Filter(rest_ops))]
        } else {
            Vec::new()
        };
        let Some((startup, total)) =
            costsize::param_range_index_scan(&ki.rel, &ki.index, &rest, lower, outer_rows)
        else {
            return ParamFound::Unmodelled;
        };
        let Some(name) = serving
            .iter()
            .map(|(ix, _)| ix.name.clone())
            .find(|n| !n.starts_with('_'))
            .or_else(|| serving.first().map(|(ix, _)| ix.name.clone()))
        else {
            return ParamFound::Unmodelled;
        };
        let alias = leaf
            .keys
            .first()
            .and_then(|(k, _, _)| k.split(secantus_pgplan::joins::SEP).next())
            .unwrap_or(&leaf.table)
            .to_string();
        ParamFound::Scan(
            secantus_pgplan::joincost::ParamInner {
                startup,
                total,
                rows: costsize::clamp_row_est(param_rows),
                qual: 0.0,
            },
            ParamScan {
                index: name,
                alias,
                column: column.clone(),
                index_only,
            },
        )
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
    /// For EXPLAIN ANALYZE below a join (RP-390): the filter the scan of
    /// `alias` runs, over its stored fields -- its leaf's own filter ANDed
    /// with the WHERE conjuncts above the join that read only it, as
    /// `distribute_qual_to_rels` pushes them down. `None` where the join
    /// is not two plain table leaves, or a conjunct reads several tables
    /// or is not a plain comparison.
    pub(crate) fn join_leaf_scan_filter(
        &self,
        sel: &secantus_pgplan::Select,
        alias: &str,
    ) -> Option<Document> {
        use secantus_pgplan::joins::{JoinKind, JoinNode, SEP};
        use secantus_pgplan::Statement;
        // A residual is a filter of no scan only where it is one operator
        // over two or more tables: a comma join's `e.a < k.a`, its join
        // clause. Any other may hold a conjunct over one table, which is
        // that table's scan's filter.
        if let Some(residual) = &sel.residual {
            if !joins_tables(residual) {
                return None;
            }
        }
        if sel.join.is_some() {
            return None;
        }
        let Statement::JoinRows(j) = sel.sub.as_ref()?.plan.as_ref() else {
            return None;
        };
        if !j.where_modelled {
            return None;
        }
        let JoinNode::Join {
            kind, left, right, ..
        } = &j.tree
        else {
            return None;
        };
        // A LEFT join's WHERE runs above it (`reduce_outer_joins`).
        if *kind != JoinKind::Inner && !sel.filter.is_empty() {
            return None;
        }
        let prefix = format!("{alias}{SEP}");
        let (leaf, columns) = [left.as_ref(), right.as_ref()].into_iter().find_map(|n| {
            let JoinNode::Leaf { plan, columns, .. } = n else {
                return None;
            };
            let Statement::Select(leaf) = plan.as_ref() else {
                return None;
            };
            columns
                .iter()
                .any(|(k, _)| k.starts_with(&prefix))
                .then_some((leaf, columns))
        })?;
        if leaf.residual.is_some() || leaf.sub.is_some() || leaf.series.is_some() {
            return None;
        }
        let stored = |name: &str| -> Option<String> {
            leaf.columns
                .iter()
                .find(|(out, _)| out == name)
                .map(|(_, f)| f.clone())
        };
        let mut clauses: Vec<bson::Bson> = Vec::new();
        if !leaf.filter.is_empty() {
            clauses.push(bson::Bson::Document(leaf.filter.clone()));
        }
        let pushed = PushedConjuncts::new(&leaf.filter);
        for (k, v) in joined_conjuncts(&sel.filter)? {
            if !k.starts_with(&prefix) {
                // Another table's: it must be some leaf's own column.
                if !k.contains(SEP) {
                    return None;
                }
                continue;
            }
            let (_, f) = columns.iter().find(|(ck, _)| *ck == k)?;
            let field = stored(f)?;
            if pushed.holds(&field, &v) {
                continue;
            }
            let mut d = Document::new();
            d.insert(field, v);
            clauses.push(bson::Bson::Document(d));
        }
        Some(match clauses.len() {
            0 => Document::new(),
            1 => match clauses.pop() {
                Some(bson::Bson::Document(d)) => d,
                _ => return None,
            },
            _ => bson::doc! { "$and": clauses },
        })
    }

    /// For EXPLAIN ANALYZE of a hash join on one key (RP-386): the outer
    /// side's key field, the hashed side's, and the fields the hashed
    /// side's scan projects (its target list, in the order the planner
    /// builds it: what the join's output reads, then the key), by the two
    /// sides' aliases. `None` for a join on several keys.
    pub(crate) fn hash_join_reads(
        &self,
        sel: &secantus_pgplan::Select,
        outer_alias: &str,
        inner_alias: &str,
    ) -> Option<(String, String, Vec<String>)> {
        if let Some(j) = &sel.join {
            if j.left_sub.is_some() || j.right_sub.is_some() {
                return None;
            }
            let (a, b) = j.on.as_ref()?;
            let col = |alias: &str| {
                if a.0 == alias {
                    Some(&a.1)
                } else if b.0 == alias {
                    Some(&b.1)
                } else {
                    None
                }
            };
            let table = |alias: &str| {
                [&j.left, &j.right]
                    .into_iter()
                    .find(|(_, al)| al == alias)
                    .map(|(t, _)| t.clone())
            };
            let defs = self.shared_table_defs().ok()?;
            let def_of = |alias: &str| {
                let t = table(alias)?;
                defs.iter().find(|d| d.name == t).cloned()
            };
            let (outer_def, inner_def) = (def_of(outer_alias)?, def_of(inner_alias)?);
            let outer_key = outer_def.field_of(col(outer_alias)?)?;
            let inner_key = inner_def.field_of(col(inner_alias)?)?;
            let mut read: Vec<String> = j
                .columns
                .iter()
                .filter(|(_, side, _)| side == inner_alias)
                .map(|(_, _, c)| inner_def.field_of(c))
                .collect::<Option<_>>()?;
            if !read.contains(&inner_key) {
                read.push(inner_key.clone());
            }
            return Some((outer_key, inner_key, read));
        }
        let (a, b, more, _, _) = self.joined_sides(sel)?;
        if !more.is_empty() {
            return None;
        }
        let (outer, inner) = if a.alias == outer_alias && b.alias == inner_alias {
            (a, b)
        } else if b.alias == outer_alias && a.alias == inner_alias {
            (b, a)
        } else {
            return None;
        };
        Some((outer.field, inner.field.clone(), inner.read))
    }

    pub(crate) fn hash_join_estimate(&self, sel: &secantus_pgplan::Select) -> Option<JoinEstimate> {
        let (a, b, more, filter, left_join) = self.joined_sides(sel)?;
        // A WHERE conjunct pushed into a side's scan (RP-365) is priced as
        // that scan's filter, through an index where that is the cheaper
        // path (RP-369, in `hash_side`). Not where it names the join key: a
        // constant there joins the key's equivalence class, which leaves no
        // join clause (RP-363; joined rows, [`Self::joined_implied_estimate`]).
        let priced = |p: &JoinedSide| {
            if !p.filter.is_empty()
                && filter_names(&p.filter, &p.field)
                && !bounds_only(&p.filter, &p.field)
            {
                return None;
            }
            self.hash_side(&p.table, &p.filter, &p.field, &p.read, &p.above)
        };
        // `hash_join_of` prices at most `MORE_HASH_CLAUSES` further
        // clauses: past them, pricing each side's scan for nothing would
        // cost a scan per clause the client wrote (RP-388).
        if more.len() > MORE_HASH_CLAUSES {
            return None;
        }
        let more: Vec<(HashSide, HashSide)> = more
            .iter()
            .map(|(l, r)| Some((priced(l)?, priced(r)?)))
            .collect::<Option<_>>()?;
        hash_join_of(priced(&a)?, priced(&b)?, &more, left_join, false, filter)
    }

    /// The two sides of an inner join of two stored tables planned as
    /// joined rows, on its first equality and then each further one: each
    /// side's table, its scan's filter (its own WHERE and the conjuncts
    /// pushed down to it), its key's stored field, and the fields its scan
    /// reads and carries above the join, the ON's join filter (see
    /// [`JoinFilterCost`]), and whether it is a LEFT join (RP-379), whose
    /// left table is preserved. `None` for any other shape
    /// ([`Self::hash_join_estimate`] lists them).
    #[allow(clippy::type_complexity)]
    fn joined_sides<'q>(
        &self,
        sel: &'q secantus_pgplan::Select,
    ) -> Option<(
        JoinedSide,
        JoinedSide,
        Vec<(JoinedSide, JoinedSide)>,
        JoinFilterCost,
        bool,
    )> {
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
            || !j.where_modelled
        {
            return None;
        }
        let JoinNode::Join {
            kind: kind @ (JoinKind::Inner | JoinKind::Left),
            left,
            right,
            on,
            on_other,
            on_filters,
            equi,
            merged,
            expr_keys,
            ..
        } = &j.tree
        else {
            return None;
        };
        let left_join = *kind == JoinKind::Left;
        // A LEFT join's WHERE is applied above it, where a conjunct on the
        // nullable side can reduce it to an inner join
        // (`reduce_outer_joins`): only one with no WHERE is priced
        // (RP-379).
        if left_join && (!sel.filter.is_empty() || sel.residual.is_some()) {
            return None;
        }
        // An ON's conjuncts beside its equalities are the join's filter,
        // checked on each pair the hash clauses match (RP-375). Priced
        // where each is `<`, `<=`, `>` or `>=` between the two tables
        // (`DEFAULT_INEQ_SEL`) or a `<>` between two of their columns
        // (`neqjoinsel`, RP-379); any other stays unpriced.
        let priced_kind = |f: &secantus_pgplan::joins::JoinFilter| {
            f.aliases.len() == 2
                && matches!(
                    f.kind,
                    secantus_pgplan::joins::FilterKind::Inequality
                        | secantus_pgplan::joins::FilterKind::NotEqual(..)
                )
        };
        if *on_other && (on_filters.is_empty() || !on_filters.iter().all(priced_kind)) {
            return None;
        }
        let on_filters: Vec<&secantus_pgplan::joins::JoinFilter> = on_filters.iter().collect();
        let ((left_key, right_key), rest) = equi.split_first()?;
        // The join clauses are an ON's, or -- a comma join, `FROM e, k
        // WHERE e.b = k.b` -- the WHERE's conjuncts over both tables, those
        // same equalities: PostgreSQL makes an inner join's WHERE and ON
        // quals one list (`deconstruct_jointree`), so both plan alike
        // (RP-372; several equalities, RP-374). Any other conjunct over
        // both tables stays unpriced.
        let pairs: std::collections::HashSet<(&str, &str)> = equi
            .iter()
            .flat_map(|(a, b)| [(a.as_str(), b.as_str()), (b.as_str(), a.as_str())])
            .collect();
        let same_pair = |(l, r): &(String, String)| pairs.contains(&(l.as_str(), r.as_str()));
        // Beside them, a comma join's WHERE may hold inequalities between
        // the two tables: its join filter, as an ON's (RP-375).
        let comma_filters: Vec<&secantus_pgplan::joins::JoinFilter> = j
            .where_filters
            .iter()
            .filter(|f| {
                matches!(
                    f.kind,
                    secantus_pgplan::joins::FilterKind::Inequality
                        | secantus_pgplan::joins::FilterKind::NotEqual(..)
                )
            })
            .collect();
        let comma_clause = on.is_none()
            && !left_join
            && j.where_spans_equality
            && j.where_filters.len() == equi.len() + comma_filters.len()
            && j.where_span_aliases.len() == j.where_filters.len()
            && comma_filters.iter().all(|f| f.aliases.len() == 2)
            && j.where_equalities.len() == equi.len()
            && j.where_equalities.iter().all(same_pair);
        let filters = if comma_clause && !comma_filters.is_empty() {
            comma_filters
        } else {
            on_filters
        };
        if !comma_clause && (on.is_none() || !j.where_filters.is_empty() || j.where_spans_equality)
        {
            return None;
        }
        // The executor re-checks a comma join's equalities above the join
        // as the select's residual; those are the join clauses, not a
        // filter.
        if let Some(residual) = &sel.residual {
            if !comma_clause || !is_key_equality(residual, equi, filters.len()) {
                return None;
            }
        }
        if !merged.is_empty() || !expr_keys.is_empty() {
            return None;
        }
        let project = j.project.as_deref();
        // The WHERE conjuncts still above the join, each over one table's
        // joined keys: PostgreSQL pushes each down to that table's scan
        // (`distribute_qual_to_rels`), as it does a reduced LEFT join's
        // (RP-365). `None` for any other shape of filter.
        let above_filter = joined_conjuncts(&sel.filter)?;
        // A side's scan, whichever of its columns is the key: the scan's
        // filter, ANDed from its own WHERE and this side's share of the
        // conjuncts above the join, and the fields it carries above the
        // join. Built once per table, however many equalities join the two
        // (RP-388: built per equality, it made EXPLAIN quadratic in the
        // conjuncts a client writes).
        struct SideScan<'s> {
            leaf: &'s secantus_pgplan::Select,
            columns: &'s [(String, String)],
            filter: std::rc::Rc<Document>,
            above: Vec<String>,
            /// The scan's target list where no WHERE conjunct is above
            /// the join (`build_base_rel_tlists`).
            carried: Option<Vec<String>>,
        }
        // The leaf's output names, as the stored fields they are (a
        // primary key is stored as `_id`).
        fn stored(leaf: &secantus_pgplan::Select, name: &str) -> Option<String> {
            leaf.columns
                .iter()
                .find(|(out, _)| out == name)
                .map(|(_, f)| f.clone())
        }
        let scan = |node: &'q JoinNode| -> Option<SideScan<'q>> {
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
            {
                return None;
            }
            let mut clauses: Vec<bson::Bson> = Vec::new();
            if !leaf.filter.is_empty() {
                clauses.push(bson::Bson::Document(leaf.filter.clone()));
            }
            let pushed = PushedConjuncts::new(&leaf.filter);
            for (k, v) in &above_filter {
                let Some((_, f)) = columns.iter().find(|(ck, _)| ck == k) else {
                    continue;
                };
                let field = stored(leaf, f)?;
                // The planner pushes such a conjunct into the scan and
                // keeps it above as well; count it once.
                if pushed.holds(&field, v) {
                    continue;
                }
                let mut d = Document::new();
                d.insert(field, v.clone());
                clauses.push(bson::Bson::Document(d));
            }
            let filter = match clauses.len() {
                0 => Document::new(),
                1 => match clauses.pop() {
                    Some(bson::Bson::Document(d)) => d,
                    _ => return None,
                },
                _ => bson::doc! { "$and": clauses },
            };
            let above: Vec<String> = columns
                .iter()
                .filter(|(key, _)| j.needed.contains(key) || project.is_none())
                .map(|(_, f)| stored(leaf, f))
                .collect::<Option<_>>()?;
            // The scan's target list: what the join and the rows above it
            // read. A WHERE conjunct run in the scan adds nothing to it,
            // unlike one left above the join.
            let carried = if above_filter.is_empty() {
                Some(
                    columns
                        .iter()
                        .filter(|(key, _)| JoinRows::carries(project, key))
                        .map(|(_, f)| stored(leaf, f))
                        .collect::<Option<_>>()?,
                )
            } else {
                None
            };
            Some(SideScan {
                leaf,
                columns,
                filter: std::rc::Rc::new(filter),
                above,
                carried,
            })
        };
        // A side: its scan, its key's stored field and the fields the scan
        // reads.
        let side = |s: &SideScan<'_>, key: &str| -> Option<JoinedSide> {
            let field = stored(s.leaf, &s.columns.iter().find(|(k, _)| k == key)?.1)?;
            let read = match &s.carried {
                Some(carried) => carried.clone(),
                None => {
                    let mut r = s.above.clone();
                    if !r.contains(&field) {
                        r.push(field.clone());
                    }
                    r
                }
            };
            Some(JoinedSide {
                key: key.to_string(),
                table: s.leaf.table.clone(),
                alias: key.split(secantus_pgplan::joins::SEP).next()?.to_string(),
                filter: std::rc::Rc::clone(&s.filter),
                field,
                read,
                above: s.above.clone(),
            })
        };
        let (left, right) = (scan(left)?, scan(right)?);
        let a = side(&left, left_key)?;
        let b = side(&right, right_key)?;
        let more: Vec<(JoinedSide, JoinedSide)> = rest
            .iter()
            .map(|(l, r)| Some((side(&left, l)?, side(&right, r)?)))
            .collect::<Option<_>>()?;
        // `clauselist_selectivity` over the join filter: the product of
        // its conjuncts'. An inequality is `DEFAULT_INEQ_SEL`; a `<>` is
        // `neqjoinsel`, one minus `eqjoinsel` over the two columns'
        // statistics (`eqjoinsel_inner` for an inner or a LEFT join), and
        // stays unpriced where either column was never analysed.
        let mut selectivity = 1.0_f64;
        for f in &filters {
            selectivity *= match &f.kind {
                secantus_pgplan::joins::FilterKind::NotEqual(l, r) => {
                    let stats = |k: &str| {
                        let s = side(&left, k).or_else(|| side(&right, k))?;
                        self.hash_side(
                            &s.table,
                            &Document::new(),
                            &s.field,
                            std::slice::from_ref(&s.field),
                            &[],
                        )
                    };
                    let (x, y) = (stats(l)?, stats(r)?);
                    if x.pg_type != y.pg_type {
                        return None;
                    }
                    fn key(s: &HashSide) -> secantus_pgplan::joincost::KeyStats<'_> {
                        secantus_pgplan::joincost::KeyStats {
                            ndistinct: s.stats.0,
                            default_ndistinct: false,
                            null_frac: s.stats.1,
                            mcv: &s.stats.2,
                        }
                    }
                    let eq = secantus_pgplan::joincost::eqjoinsel_inner(&key(&x), &key(&y));
                    1.0 - eq.clamp(0.0, 1.0)
                }
                _ => 1.0 / 3.0,
            };
        }
        let filter = JoinFilterCost {
            count: filters.len(),
            ops: filters.iter().map(|f| f.ops).sum(),
            selectivity,
        };
        Some((a, b, more, filter, left_join))
    }

    /// The Nested Loop of an inner join planned as joined rows whose WHERE
    /// fixes a join key to a constant (RP-376; RP-363 on the narrow path):
    /// the constant joins the keys' equivalence class, so each side is
    /// filtered by it (`generate_base_implied_equalities_const`) and no
    /// join clause is left. Each side is read through an index where
    /// `set_cheapest` keeps that path, else by its sequential scan; the
    /// plan is `cheapest_nest_loop`'s. Returns the plan, the left and right
    /// scans' and the join's estimates, the filters the class adds
    /// (`(alias, column)`), the outer key the WHERE fixed and the index
    /// each side reads (left, right).
    #[allow(clippy::type_complexity)]
    pub(crate) fn joined_implied_estimate(
        &self,
        sel: &secantus_pgplan::Select,
    ) -> Option<(
        secantus_pgplan::joincost::NestLoopPlan,
        Estimate,
        Estimate,
        Estimate,
        Vec<(String, String)>,
        (String, String),
        [Option<String>; 2],
    )> {
        let (mut a, mut b, more, filter, left_join) = self.joined_sides(sel)?;
        if !more.is_empty() || a.table == b.table || filter.count != 0 || left_join {
            return None;
        }
        let column = |p: &JoinedSide| {
            let def = self.lookup(&p.table)?;
            def.columns.iter().find(|c| c.field() == p.field).cloned()
        };
        let (ca, cb) = (column(&a)?, column(&b)?);
        if ca.pg_type != cb.pg_type {
            return None;
        }
        // The constant: one equality to it on either key, every such
        // equality the same value, never NULL.
        let (va, vb) = (
            key_constant(&a.filter, &a.field),
            key_constant(&b.filter, &b.field),
        );
        let value = match (&va, &vb) {
            (Some(Some(x)), Some(Some(y))) => {
                if secantus_pgplan::compare_values(x, y) != Some(std::cmp::Ordering::Equal) {
                    return None;
                }
                x.clone()
            }
            (Some(Some(x)), None) | (None, Some(Some(x))) => x.clone(),
            _ => return None,
        };
        // The side whose WHERE wrote the equality prints it; the class
        // adds it to the other (the scans' filters already hold it).
        let written = joined_conjuncts(&sel.filter)?;
        let wrote = |p: &JoinedSide| {
            written.iter().any(|(k, v)| {
                *k == p.key
                    && secantus_pgplan::compare_values(v, &value) == Some(std::cmp::Ordering::Equal)
            })
        };
        let (wa, wb) = (wrote(&a), wrote(&b));
        let from = match (wa, wb) {
            (true, _) => (a.alias.clone(), ca.name.clone()),
            (false, true) => (b.alias.clone(), cb.name.clone()),
            (false, false) => return None,
        };
        let mut added = Vec::new();
        for (p, c, has, w) in [
            (&mut a, &ca, va.is_some(), wa),
            (&mut b, &cb, vb.is_some(), wb),
        ] {
            if !w {
                added.push((p.alias.clone(), c.name.clone()));
            }
            if has {
                continue;
            }
            let mut eq = Document::new();
            eq.insert(p.field.clone(), value.clone());
            let own = std::rc::Rc::unwrap_or_clone(std::mem::take(&mut p.filter));
            p.filter = std::rc::Rc::new(if own.is_empty() {
                eq
            } else {
                bson::doc! { "$and": [bson::Bson::Document(own), eq] }
            });
        }
        let (sa, ia) = self.cheapest_filtered_scan(&a.table, &a.filter, &a.read)?;
        let (sb, ib) = self.cheapest_filtered_scan(&b.table, &b.filter, &b.read)?;
        let width =
            |p: &JoinedSide| match self.scan_estimate(&p.table, &Document::new(), &p.above)? {
                (w, None) => Some(w.3),
                _ => None,
            };
        let plan = secantus_pgplan::joincost::cheapest_nest_loop(sa, sb, 0.0);
        let rows = costsize::clamp_row_est(sa.2 * sb.2);
        Some((
            plan,
            sa,
            sb,
            (plan.startup, plan.total, rows, width(&a)? + width(&b)?),
            added,
            from,
            [ia, ib],
        ))
    }

    /// A filtered scan of user table `table` reading `read`: through an
    /// index where `set_cheapest` keeps that path (`index_scan_estimate`,
    /// RP-376), else its sequential scan -- only where no index leads with
    /// a filtered column, as a path the model does not price may win
    /// there. Returns the estimate and the index read, `None` for a Seq
    /// Scan.
    fn cheapest_filtered_scan(
        &self,
        table: &str,
        filter: &Document,
        read: &[String],
    ) -> Option<(Estimate, Option<String>)> {
        if !filter.is_empty() {
            for ix in self
                .index_relations()
                .iter()
                .filter(|ix| ix.table.name == table)
            {
                let key: Vec<String> = ix
                    .keys
                    .iter()
                    .map(|k| {
                        let i = usize::try_from(*k).ok()?.checked_sub(1)?;
                        Some(ix.table.columns.get(i)?.field())
                    })
                    .collect::<Option<_>>()?;
                if let Some(est) = self.index_scan_estimate(table, filter, read, &key) {
                    return Some((est, Some(ix.name.clone())));
                }
                if key.first().is_some_and(|lead| filter_names(filter, lead)) {
                    return None;
                }
            }
        }
        match self.scan_estimate(table, filter, read)? {
            (s, None) => Some((s, None)),
            _ => None,
        }
    }

    /// [`Self::hash_join_estimate`] for a two-table inner join planned on
    /// the narrow path (`Select::join`, RP-361): each side's scan reads
    /// the columns the select list takes from it and its join key, and
    /// carries the select list's above the join.
    pub(crate) fn join_select_hash_estimate(
        &self,
        sel: &secantus_pgplan::Select,
    ) -> Option<JoinEstimate> {
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
        // join key puts a constant in the key's equivalence class: an
        // inner join is then left with no join clause (RP-363's Nested
        // Loop, not this plan); a LEFT join whose OUTER key it fixes
        // filters the nullable side by the constant too
        // (`reconsider_outer_join_clauses`, RP-364) and keeps the ON as a
        // redundant clause, whose selectivity is 1 (`norm_selec` above 1).
        let (implied, by) = self.narrow_with_implied(j)?;
        let (filters, redundant) = match by {
            crate::NarrowImplied::Nothing => (j.filter.clone(), false),
            crate::NarrowImplied::InnerKey => (implied.filter.clone(), true),
            crate::NarrowImplied::NoJoinClause => return None,
        };
        if !redundant {
            for pred in &filters {
                if (pred.alias == a.0 && pred.col == a.1) || (pred.alias == b.0 && pred.col == b.1)
                {
                    return None;
                }
            }
        }
        // A WHERE on a LEFT join's nullable side never reaches this path:
        // the narrow planner refuses it, and the general one plans it.
        let left_join = j.left_join;
        let side_filter =
            |table: &str, alias: &str| self.narrow_side_filter(&filters, table, alias, true);
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
        hash_join_of(
            side(&j.left)?,
            side(&j.right)?,
            &[],
            left_join,
            redundant,
            JoinFilterCost::default(),
        )
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

    /// One side's WHERE predicates as its scan's filter (RP-362): the
    /// predicates naming `alias`, ANDed. `None` for a predicate this does
    /// not model (`NOT col`), or, unless `indexed` (the caller weighs the
    /// index paths, RP-369), a filtered table with an index, which an
    /// index scan may serve.
    fn narrow_side_filter(
        &self,
        filter: &[secantus_pgplan::JoinPred],
        table: &str,
        alias: &str,
        indexed: bool,
    ) -> Option<Document> {
        use secantus_pgplan::JoinOp;
        let def = self.lookup(table)?;
        let mut clauses = Vec::new();
        // A WHERE over a table with an index may be an index scan's.
        if !indexed
            && filter.iter().any(|p| p.alias == alias)
            && self
                .index_relations()
                .iter()
                .any(|ix| ix.table.name == table)
        {
            return None;
        }
        for pred in filter.iter().filter(|p| p.alias == alias) {
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
    }

    /// The filters an equivalence class adds to a narrow-path LEFT join
    /// whose WHERE fixes its outer key (RP-364): `(alias, column)` of the
    /// nullable side's key, and the alias whose WHERE fixed it.
    pub(crate) fn join_select_left_implied(
        &self,
        sel: &secantus_pgplan::Select,
    ) -> Option<LeftImplied> {
        let j = sel.join.as_ref()?;
        if !j.left_join {
            return None;
        }
        let (implied, by) = self.narrow_with_implied(j)?;
        if !matches!(by, crate::NarrowImplied::InnerKey) {
            return None;
        }
        let added = implied.filter[j.filter.len()..]
            .iter()
            .map(|p| (p.alias.clone(), p.col.clone()))
            .collect();
        // The outer key the WHERE fixed.
        let (a, b) = j.on.as_ref()?;
        let from = if a.0 == j.left.1 {
            a.clone()
        } else {
            b.clone()
        };
        Some((added, from))
    }

    /// The Nested Loop of a narrow-path inner join whose WHERE fixes a join
    /// key to a constant (RP-363): the constant is in the keys' equivalence
    /// class, so each side is filtered by it
    /// (`generate_base_implied_equalities_const`) and no join clause is
    /// left. Each side's sequential scan reads the columns the select list
    /// and the ON take from it; the plan is `cheapest_nest_loop`'s. Also
    /// returns the filters the class adds, `(alias, column)`.
    #[allow(clippy::type_complexity)]
    pub(crate) fn join_select_implied_estimate(
        &self,
        sel: &secantus_pgplan::Select,
    ) -> Option<(
        secantus_pgplan::joincost::NestLoopPlan,
        Estimate,
        Estimate,
        Estimate,
        Vec<(String, String)>,
    )> {
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
            || j.left_join
            || j.left_sub.is_some()
            || j.right_sub.is_some()
            || j.order.is_some()
            || j.exprs.iter().any(Option::is_some)
            || j.left.1 == j.right.1
        {
            return None;
        }
        let (a, b) = j.on.as_ref()?;
        let (implied, by) = self.narrow_with_implied(j)?;
        if !matches!(by, crate::NarrowImplied::NoJoinClause) {
            return None;
        }
        let side = |(table, alias): &(String, String)| -> Option<(Estimate, i64)> {
            let key = if a.0 == *alias {
                &a.1
            } else if b.0 == *alias {
                &b.1
            } else {
                return None;
            };
            let def = self.lookup(table)?;
            let above: Vec<String> = j
                .columns
                .iter()
                .filter(|(_, side, _)| side == alias)
                .map(|(_, _, c)| def.field_of(c))
                .collect::<Option<_>>()?;
            let mut read = above.clone();
            let field = def.field_of(key)?;
            if !read.contains(&field) {
                read.push(field);
            }
            let filter = self.narrow_side_filter(&implied.filter, table, alias, false)?;
            let (scan, None) = self.scan_estimate(table, &filter, &read)? else {
                return None;
            };
            let (width, None) = self.scan_estimate(table, &Document::new(), &above)? else {
                return None;
            };
            Some((scan, width.3))
        };
        let (sa, a_above) = side(&j.left)?;
        let (sb, b_above) = side(&j.right)?;
        let plan = secantus_pgplan::joincost::cheapest_nest_loop(sa, sb, 0.0);
        let rows = costsize::clamp_row_est(sa.2 * sb.2);
        let added = implied.filter[j.filter.len()..]
            .iter()
            .map(|p| (p.alias.clone(), p.col.clone()))
            .collect();
        Some((
            plan,
            sa,
            sb,
            (plan.startup, plan.total, rows, a_above + b_above),
            added,
        ))
    }

    /// One table of a hash join priced by `cost_hashjoin` (RP-330): its
    /// scan's estimate reading `read`, the width of `above` (the columns
    /// it carries above the join), and its key `field`'s statistics.
    /// `None` where the key column is not analysed.
    /// One analysed column's statistics as `eqjoinsel` reads them
    /// (`get_variable_numdistinct`, its null fraction and MCV list).
    /// `None` where the column was never measured or its distinct count is
    /// the 200 default, which the estimates here do not model.
    fn analysed_key(&self, table: &str, field: &str) -> Option<AnalysedKey> {
        let count = self
            .storage
            .count_matching(self.db(), table, &Document::new(), None)
            .ok()?;
        let pt = self.planner_table(table, count)?;
        let column = pt.def.columns.iter().find(|c| c.field() == field)?.clone();
        let analysed = pt.analyzed.as_ref()?.get(field)?.as_ref()?;
        let (stats, slots) = (analysed.stats, analysed.slots.clone());
        let unique = pt.unique.iter().any(|u| u == field);
        // `get_variable_numdistinct`: a unique column's distinct count
        // is its non-null rows; a negative `stadistinct` is a fraction
        // of the rows; none measured is the 200 default.
        let null_frac = f64::from(stats.null_frac);
        let stadistinct = if unique {
            -(1.0 - null_frac)
        } else {
            f64::from(stats.distinct)
        };
        let nd = if stadistinct > 0.0 {
            stadistinct
        } else if stadistinct < 0.0 {
            -stadistinct * pt.tuples
        } else {
            return None;
        };
        let ndistinct = costsize::clamp_row_est(nd.min(pt.tuples.max(1.0)));
        let mcv = slots.as_ref().map(|s| s.mcv.clone()).unwrap_or_default();
        Some(AnalysedKey {
            tuples: pt.tuples,
            column,
            stats,
            slots,
            unique,
            ndistinct,
            null_frac,
            mcv,
        })
    }

    fn hash_side(
        &self,
        table: &str,
        filter: &Document,
        field: &str,
        read: &[String],
        above: &[String],
    ) -> Option<HashSide> {
        let indexes = self.index_relations();
        let indexed = indexes.iter().any(|ix| ix.table.name == table);
        // A filtered side over a table with an index is read through the
        // index where `set_cheapest` keeps that path (RP-369): `cost_index`
        // as a lone scan of the table prices it. Where the comparison is
        // not modelled -- or a sequential scan may win, which
        // `index_scan_estimate` does not tell apart -- the side is not
        // priced.
        // A WHERE no index of the table reads leaves the sequential scan
        // its side's cheapest unparameterized path: an index's whole scan
        // costs more, and is kept only for its order, which the merge
        // paths weigh from `key_index` (RP-381).
        let index_reads_filter = || {
            let Some(quals) = costsize::conjuncts(filter) else {
                return true;
            };
            indexes.iter().any(|ix| {
                ix.table.name == table
                    && ix.keys.iter().any(|k| {
                        // An expression key may read any column.
                        *k == 0
                            || usize::try_from(*k)
                                .ok()
                                .and_then(|k| k.checked_sub(1))
                                .and_then(|i| ix.table.columns.get(i))
                                .is_some_and(|c| quals.iter().any(|(f, _)| *f == c.field()))
                    })
            })
        };
        let (scan, index) = if !filter.is_empty() && indexed && index_reads_filter() {
            let mut found = None;
            for ix in indexes.iter().filter(|ix| ix.table.name == table) {
                let key: Option<Vec<String>> = ix
                    .keys
                    .iter()
                    .map(|k| {
                        let i = usize::try_from(*k).ok()?.checked_sub(1)?;
                        Some(ix.table.columns.get(i)?.field())
                    })
                    .collect();
                if let Some(est) = self.index_scan_estimate(table, filter, read, &key?) {
                    found = Some((est, Some(ix.name.clone())));
                    break;
                }
            }
            found?
        } else {
            let (scan, None) = self.scan_estimate(table, filter, read)? else {
                return None;
            };
            (scan, None)
        };
        let (width, None) = self.scan_estimate(table, &Document::new(), above)? else {
            return None;
        };
        let key = self.analysed_key(table, field)?;
        let col = &key.column;
        let (nd, null_frac, mcv) = (key.ndistinct, key.null_frac, key.mcv.clone());
        let unique = key.unique;
        let (numeric, ordered) = self.column_key_order(col);
        // `innerrel_is_unique` from the side's own restriction: its WHERE
        // fixes every key column of a unique index to a constant, so the
        // scan returns at most one row and the join is `inner_unique`
        // with this side inner (RP-369).
        let mut fixed = Vec::new();
        crate::explain::constant_equalities(filter, &mut fixed);
        let one_row = indexes.iter().any(|ix| {
            ix.table.name == table
                && ix.unique
                && !ix.exclusion
                && !ix.keys.is_empty()
                && ix.keys.iter().all(|k| {
                    usize::try_from(*k)
                        .ok()
                        .and_then(|k| k.checked_sub(1))
                        .and_then(|i| ix.table.columns.get(i))
                        .is_some_and(|c| fixed.contains(&c.field()))
                })
        });
        let key_indexed = indexes.iter().any(|ix| {
            ix.table.name == table
                && ix
                    .keys
                    .first()
                    .and_then(|k| usize::try_from(*k).ok()?.checked_sub(1))
                    .and_then(|i| ix.table.columns.get(i))
                    .is_some_and(|c| c.field() == field)
        });
        let key_index = if key_indexed && index.is_none() {
            self.key_index(table, filter, field, &key, scan.2)
        } else {
            None
        };
        Some(HashSide {
            scan,
            index,
            one_row,
            above: width.3,
            pg_type: col.pg_type.clone(),
            stats: (nd, null_frac, mcv),
            unique,
            tuples: key.tuples,
            column: key.slots.clone().map(|sl| (key.stats, sl)),
            numeric,
            ordered,
            key_indexed,
            key_index,
        })
    }

    /// The join key `field`'s index over analysed table `table`, scanned
    /// with `filter` (RP-377): the one plain B-tree leading with it, sized
    /// and correlated as [`Self::index_scan_estimate`] does. `None` where
    /// another index leads with it, reads it past its first column or
    /// serves a WHERE column, or the index is partial or a hash.
    fn key_index(
        &self,
        table: &str,
        filter: &Document,
        field: &str,
        key: &AnalysedKey,
        rows: f64,
    ) -> Option<KeyIndex> {
        let count = self
            .storage
            .count_matching(self.db(), table, &Document::new(), None)
            .ok()?;
        let pt = self.planner_table(table, count)?;
        let stats = pt.analyzed.as_ref()?;
        let quals = costsize::conjuncts(filter)?;
        let mut indexes = self.cost_indexes(table, &pt.def, count as u64);
        let correlations = Self::stats_correlations(&pt.def, stats);
        let mut found: Option<costsize::Index> = None;
        for ix in &mut indexes {
            if quals.iter().any(|(f, _)| ix.columns.contains(f))
                || ix.columns[1..].iter().any(|c| c == field)
            {
                return None;
            }
            if ix.field != field {
                continue;
            }
            if ix.hash || ix.predicate.is_some() || ix.tuples.is_some() {
                return None;
            }
            // The primary key is listed both as the SQL key and as the
            // storage's own `_id` index: one relation in PostgreSQL.
            if let Some(f) = &found {
                if f.columns == ix.columns {
                    continue;
                }
                return None;
            }
            let c = correlations.get(field).copied().flatten()?;
            ix.correlation = if ix.columns.len() > 1 { c * 0.75 } else { c };
            if !ix.unique {
                if let Some(Some((pages, height))) = self.dedup_index_size(table, ix) {
                    ix.pages = pages;
                    ix.height = height;
                }
            }
            found = Some(ix.clone());
        }
        Some(KeyIndex {
            rel: costsize::Rel {
                pages: pt.pages,
                tuples: pt.tuples,
            },
            index: found?,
            quals,
            ndistinct: key.ndistinct,
            where_sel: rows / pt.tuples.max(1.0),
            allvisfrac: pt.allvisfrac,
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
    ) -> Option<(Estimate, Option<ScanIndex>, Estimate)> {
        if sel.residual.is_some()
            || sel.sub.is_some()
            || sel.series.is_some()
            || sel.join.is_some()
            || !sel.windows.is_empty()
            || !sel.order.is_empty()
            || !matches!(sel.distinct, secantus_pgplan::Distinct::None)
            || sel.with_ties
            || sel.casts.iter().any(Option::is_some)
        {
            return None;
        }
        let fields: Vec<String> = sel.columns.iter().map(|(_, f)| f.clone()).collect();
        // `preprocess_limit`: a constant LIMIT of 0 or less is estimated
        // as one row.
        let count = sel.limit.map_or(0, |n| n.max(1));
        // The scan is `get_cheapest_fractional_path`'s for the rows the
        // LIMIT and OFFSET read (RP-382).
        let want = PathWant {
            limit: Some((count + sel.offset.max(0)) as f64),
            order: Vec::new(),
        };
        let (estimate, index, _) = self.upper_scan(&sel.table, &sel.filter, &fields, &want)?;
        Some((estimate, index, limit_estimate(estimate, sel.offset, count)))
    }

    /// One table's scan under a Sort, a Limit or an Aggregate (RP-382):
    /// a table with no index, or none a WHERE reaches, its sequential scan
    /// ([`Self::scan_estimate`]); an indexed one with a WHERE, the path
    /// PostgreSQL keeps for `want` ([`Self::user_upper_path`]), and the
    /// index scans already in an ORDER BY's order. `None` where that is
    /// not modelled: an ORDER BY over an indexed table with no WHERE (a
    /// whole-index scan may give its order).
    pub(crate) fn upper_scan(
        &self,
        table: &str,
        filter: &Document,
        fields: &[String],
        want: &PathWant,
    ) -> Option<UpperScan> {
        let indexed = self
            .index_relations()
            .iter()
            .any(|ix| ix.table.name == table);
        if !indexed || (filter.is_empty() && want.order.is_empty()) {
            let (estimate, None) = self.scan_estimate(table, filter, fields)? else {
                return None;
            };
            return Some((estimate, None, Vec::new()));
        }
        let path = self.user_upper_path(table, filter, fields, want)?;
        let (estimate, index) = path.chosen?;
        Some((estimate, index, path.ordered))
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
        // Priced over the sequential scan only.
        let (scan, None, limit) = self.limit_scan_estimate(isel)? else {
            return None;
        };
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
        let defs = self.shared_table_defs().ok()?;
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
        self.scan_estimate_reading(table, filter, fields, &[])
    }

    /// [`Self::scan_estimate`] of a statement that also reads `reads`
    /// (the columns a computed select list reads), which an index-only
    /// scan's index must hold too (RP-378).
    pub(crate) fn scan_estimate_reading(
        &self,
        table: &str,
        filter: &Document,
        fields: &[String],
        reads: &[String],
    ) -> Option<ScanEstimate> {
        if self
            .shared_table_defs()
            .ok()?
            .iter()
            .any(|d| d.name == table)
        {
            if let Some(chosen) = self.user_index_path(table, filter, fields, reads) {
                return Some(chosen);
            }
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
    /// clause on a later key column, an index
    /// holding every column read (an index-only scan), an `OR`, or more
    /// than one index a clause serves (`choose_bitmap_and`).
    pub(crate) fn index_scan_estimate(
        &self,
        table: &str,
        filter: &Document,
        fields: &[String],
        key: &[String],
    ) -> Option<Estimate> {
        if filter.is_empty()
            || !self
                .shared_table_defs()
                .ok()?
                .iter()
                .any(|d| d.name == table)
        {
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
            // A range on the leading column is priced too (RP-371): its
            // selectivity reads the column's current ends from the index
            // (`get_actual_variable_range`, in `analyzed_where`).
            let covering = fields
                .iter()
                .chain(named.iter())
                .all(|f| ix.columns.contains(f));
            if covering {
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

    /// The path `set_cheapest` keeps for one user table's scan, when it is
    /// an index path (RP-339, RP-378): each index a clause reaches opens a
    /// plain Index Scan (`cost_index` over `btcostestimate`, its quals on
    /// later key columns index quals too), an Index Only Scan instead where
    /// the index holds every column the statement reads (`check_index_only`;
    /// `reads` adds the columns a computed select list reads), and one
    /// Bitmap Heap Scan over `choose_bitmap_and`'s pick of every bitmap
    /// path: each index's Bitmap Index Scan and each `OR` whose arms an
    /// index serves (`generate_bitmap_or_paths`, a BitmapOr)
    /// (`create_index_paths`). `None` -- the caller prices the sequential
    /// scan -- where the sequential scan wins, or where the choice is one
    /// this model does not print or price: a table never analysed, a
    /// chosen BitmapAnd, a BitmapOr arm that is not one index's scan of
    /// the whole arm, a partial index, or an index the clauses reach only
    /// past its leading column with no index that leads with them
    /// (PostgreSQL prices a full-index path for it, not modelled).
    /// [`Self::user_index_path`] for a user table, `None` for a catalog.
    pub(crate) fn user_index_path_reading(
        &self,
        table: &str,
        filter: &Document,
        fields: &[String],
        reads: &[String],
    ) -> Option<ScanEstimate> {
        if !self
            .shared_table_defs()
            .ok()?
            .iter()
            .any(|d| d.name == table)
        {
            return None;
        }
        self.user_index_path(table, filter, fields, reads)
    }

    fn user_index_path(
        &self,
        table: &str,
        filter: &Document,
        fields: &[String],
        reads: &[String],
    ) -> Option<ScanEstimate> {
        self.user_paths(table, filter, fields, reads, &PathWant::default())?
            .chosen
    }

    /// The scan of a user table, indexed and filtered, under a Sort, a
    /// Limit or an Aggregate (RP-382): `want` says which. `None` where
    /// the choice is not modelled (as [`Self::user_index_path`] lists, or
    /// an order an index gives only past an equality on its earlier
    /// columns, or only in part -- an Incremental Sort).
    pub(crate) fn user_upper_path(
        &self,
        table: &str,
        filter: &Document,
        fields: &[String],
        want: &PathWant,
    ) -> Option<UserPath> {
        if !self
            .shared_table_defs()
            .ok()?
            .iter()
            .any(|d| d.name == table)
        {
            return None;
        }
        let mut path = self.user_paths(table, filter, fields, &[], want)?;
        if path.chosen.is_none() {
            path.chosen = Some((self.user_seq_scan(table, filter, fields)?, None));
        }
        Some(path)
    }

    fn user_paths(
        &self,
        table: &str,
        filter: &Document,
        fields: &[String],
        reads: &[String],
        want: &PathWant,
    ) -> Option<UserPath> {
        if filter.is_empty() {
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
        let mut named = Vec::new();
        filter_fields(filter, &mut named)?;
        // Each index's name, by its key fields.
        let relations = self.index_relations();
        let name_of = |columns: &[String]| -> Option<String> {
            relations
                .iter()
                .filter(|r| r.table.name == table)
                .find(|r| {
                    let keys: Vec<String> = r
                        .keys
                        .iter()
                        .filter_map(|k| usize::try_from(*k).ok()?.checked_sub(1))
                        .filter_map(|i| r.table.columns.get(i))
                        .filter_map(|c| r.table.field_of(&c.name))
                        .collect();
                    keys == columns
                })
                .map(|r| r.name.clone())
        };
        let index_sel = |f: &str| w.index_selectivity.get(f).copied();
        let correlations = Self::stats_correlations(&pt.def, stats);
        let leads: Vec<String> = indexes.iter().map(|ix| ix.field.clone()).collect();
        // The columns the scan reads: its output, its WHERE, and what a
        // computed select list reads.
        let read: Vec<&String> = fields.iter().chain(named.iter()).chain(reads).collect();
        let mut serving: Vec<(String, costsize::Index)> = Vec::new();
        let mut index_only: Vec<bool> = Vec::new();
        // The indexes whose order is the ORDER BY's (`pathkeys_useful`):
        // each scanned whole or through its quals, for that order.
        let mut presorted: Vec<costsize::Index> = Vec::new();
        for ix in &mut indexes {
            if let Some(first) = want.order.first() {
                match ix.columns.iter().position(|c| c == first) {
                    // Its leading column, then the rest of the keys in
                    // turn; leading with the first key alone would be an
                    // Incremental Sort, not modelled.
                    Some(0) => {
                        if ix.hash || ix.predicate.is_some() || ix.tuples.is_some() {
                            return None;
                        }
                        if want.order.len() > ix.columns.len()
                            || want.order.iter().zip(&ix.columns).any(|(o, c)| o != c)
                        {
                            return None;
                        }
                        let mut ordered = ix.clone();
                        ordered.later = ordered.columns[1..]
                            .iter()
                            .filter(|f| named.contains(f))
                            .map(|f| (f.clone(), index_sel(f).unwrap_or(1.0)))
                            .collect();
                        if !self.setting_is("enable_indexonlyscan", "off")
                            && read.iter().all(|f| ordered.columns.contains(f))
                        {
                            ordered.visible = pt.allvisfrac;
                        }
                        let lead = ordered.columns.first()?.clone();
                        let c = correlations.get(&lead).copied().flatten()?;
                        ordered.correlation = if ordered.columns.len() > 1 {
                            c * 0.75
                        } else {
                            c
                        };
                        if !ordered.unique {
                            if let Some(Some((pages, height))) =
                                self.dedup_index_size(table, &ordered)
                            {
                                ordered.pages = pages;
                                ordered.height = height;
                            }
                        }
                        if !presorted.iter().any(|p| p.columns == ordered.columns) {
                            presorted.push(ordered);
                        }
                    }
                    // Past earlier columns an equality may fix
                    // (`pathkeys` dropping constants): not modelled.
                    Some(at) if ix.columns[..at].iter().all(|c| named.contains(c)) => {
                        return None;
                    }
                    _ => {}
                }
            }
            if !named.iter().any(|f| ix.columns.contains(f)) {
                continue;
            }
            if ix.predicate.is_some() || ix.tuples.is_some() {
                return None;
            }
            let lead = ix.columns.first()?.clone();
            if !named.contains(&lead) {
                // Named only past its leading column: PostgreSQL's path
                // through it reads the whole index (no bound qual in
                // `btcostestimate`) for the rows an index that leads with
                // those columns finds directly, so it never wins over that
                // one. With no such index it is not modelled.
                let later: Vec<&String> = named
                    .iter()
                    .filter(|f| ix.columns[1..].contains(f))
                    .collect();
                if later.iter().all(|f| leads.contains(f)) {
                    continue;
                }
                return None;
            }
            // The quals on its later key columns are index quals too.
            ix.later = ix.columns[1..]
                .iter()
                .filter(|f| named.contains(f))
                .map(|f| (f.clone(), index_sel(f).unwrap_or(1.0)))
                .collect();
            // An index holding every column read is scanned index-only,
            // skipping the heap pages the visibility map marks.
            // `check_index_only`, which `enable_indexonlyscan = off` stops:
            // the path is then a plain index scan.
            let only = !self.setting_is("enable_indexonlyscan", "off")
                && read.iter().all(|f| ix.columns.contains(f));
            if only {
                ix.visible = pt.allvisfrac;
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
            if !serving.iter().any(|(_, s)| s.columns == ix.columns) {
                serving.push((name_of(&ix.columns)?, ix.clone()));
                index_only.push(only);
            }
        }
        let rows = costsize::clamp_row_est(pt.tuples * w.selectivity);
        let rel = costsize::Rel {
            pages: pt.pages,
            tuples: pt.tuples,
        };
        let colsel = |f: &str| costsize::ColSel::Given(index_sel(f).unwrap_or(1.0));
        let quals = with_in_lengths(w.quals.clone());
        // An index-only scan reads `visible` of the heap's pages less; a
        // bitmap scan never is one.
        let plain = |ix: &costsize::Index| costsize::Index {
            visible: 0.0,
            ..ix.clone()
        };
        let scan_cost = |ix: &costsize::Index| {
            costsize::scan_paths(&rel, std::slice::from_ref(ix), &quals, &colsel, rows)
                .into_iter()
                .find_map(|(k, c)| matches!(k, costsize::ScanKind::Index(_)).then_some(c))
        };
        // Each index's bitmap path over its index quals (the clauses they
        // are, by position, for `choose_bitmap_and`).
        let clauses_of = |ix: &costsize::Index| -> Vec<usize> {
            quals
                .iter()
                .enumerate()
                .filter(|(_, (f, q))| ix.index_qual(f, q))
                .map(|(i, _)| i)
                .collect()
        };
        let bitmap_path = |ix: &costsize::Index| {
            costsize::bitmap_index_path(
                &rel,
                &plain(ix),
                &quals,
                colsel(&ix.field),
                rows,
                clauses_of(ix),
            )
        };
        let singles: Vec<Option<costsize::BitmapPath>> =
            serving.iter().map(|(_, ix)| bitmap_path(ix)).collect();
        let mut bitmaps: Vec<costsize::BitmapPath> = singles.iter().flatten().cloned().collect();
        // Each index's scan of one `OR` arm: its quals on the index's key
        // columns, the leading column's selectivity, and the later
        // columns' (`build_paths_for_OR` over the arm's clauses).
        let arm_scan = |ix: &costsize::Index, arm: &[crate::IndexQuals]| -> Option<ArmScan> {
            let lead = arm.iter().find(|s| s.field == ix.field)?;
            let mut quals = lead.quals.clone();
            let mut clauses = lead.clauses.clone();
            let mut later = Vec::new();
            for s in arm {
                if s.field != ix.field && ix.columns[1..].contains(&s.field) {
                    quals.extend(s.quals.iter().cloned());
                    clauses.extend(s.clauses.iter().copied());
                    later.push((s.field.clone(), s.selectivity));
                }
            }
            clauses.sort_unstable();
            clauses.dedup();
            let ix = costsize::Index { later, ..plain(ix) };
            Some((ix, quals, lead.selectivity, clauses))
        };
        // Each BitmapOr, and per arm the index whose scan it is (`None`
        // where the arm's pick is not one index's scan).
        let mut ors: Vec<(costsize::BitmapPath, Vec<Option<usize>>)> = Vec::new();
        for (_, arms) in &w.bitmap_ors {
            let mut chosen = Vec::with_capacity(arms.len());
            let mut arm_index = Vec::with_capacity(arms.len());
            for arm in arms {
                let mut paths = Vec::new();
                let mut owners = Vec::new();
                for (i, (_, ix)) in serving.iter().enumerate() {
                    let Some((aix, aquals, asel, clauses)) = arm_scan(ix, arm) else {
                        continue;
                    };
                    paths.push(costsize::bitmap_index_path(
                        &rel,
                        &aix,
                        &aquals,
                        costsize::ColSel::Given(asel),
                        rows,
                        clauses,
                    )?);
                    owners.push(i);
                }
                let pick = costsize::choose_bitmap_and(&rel, &paths, &quals)?;
                arm_index.push(
                    pick.index
                        .then(|| paths.iter().position(|p| *p == pick).map(|j| owners[j]))
                        .flatten(),
                );
                chosen.push(pick);
            }
            let pos = ors.len();
            let or = costsize::bitmap_or_path(&chosen, w.bitmap_ors[pos].0);
            bitmaps.push(or.clone());
            ors.push((or, arm_index));
        }
        let bitmap = if bitmaps.is_empty() {
            None
        } else {
            Some(costsize::choose_bitmap_and(&rel, &bitmaps, &quals)?)
        };
        // `set_cheapest` over every path, `disable_cost` added to a kind an
        // `enable_*` setting turns off.
        const DISABLE_COST: f64 = 1.0e10;
        let off = |name: &str| self.setting_is(name, "off");
        let disable = |(s, t): (f64, f64), on: bool| {
            if on {
                (s, t)
            } else {
                (s + DISABLE_COST, t + DISABLE_COST)
            }
        };
        enum Pick {
            Seq,
            Bitmap,
            Index(usize),
        }
        let mut best = (
            Pick::Seq,
            disable(
                costsize::seq_scan_path(&rel, &quals),
                !off("enable_seqscan"),
            ),
        );
        // Under a LIMIT, `get_cheapest_fractional_path`: the fraction of
        // the rows it reads, each path's startup plus that share of the
        // rest (`compare_fractional_path_costs`); a fraction outside
        // (0, 1) is the cheapest total.
        let fraction = want
            .limit
            .map(|n| n / rows)
            .filter(|f| *f > 0.0 && *f < 1.0);
        let mut consider = |pick: Pick, (s, t): (f64, f64)| {
            let (bs, bt) = best.1;
            let better = match fraction {
                Some(f) => {
                    let (c, bc) = (s + f * (t - s), bs + f * (bt - bs));
                    c < bc || (c == bc && (t < bt || (t == bt && s < bs)))
                }
                None => t < bt || (t == bt && s < bs),
            };
            if better {
                best = (pick, (s, t));
            }
        };
        if let Some(b) = &bitmap {
            consider(
                Pick::Bitmap,
                disable(
                    costsize::bitmap_heap(&rel, b, &quals),
                    !off("enable_bitmapscan"),
                ),
            );
        }
        for (i, (_, ix)) in serving.iter().enumerate() {
            if let Some(c) = scan_cost(ix) {
                consider(Pick::Index(i), disable(c, !off("enable_indexscan")));
            }
        }
        let (pick, (startup, total)) = best;
        let ordered: Vec<(f64, f64)> = presorted
            .iter()
            .map(|ix| costsize::ordered_index_scan_with(&rel, ix, &quals, colsel(&ix.field)))
            .collect();
        let index = match pick {
            Pick::Seq => {
                return Some(UserPath {
                    chosen: None,
                    ordered,
                })
            }
            Pick::Index(i) => ScanIndex {
                name: serving.get(i)?.0.clone(),
                only: index_only[i],
                bitmap: None,
                or_arms: Vec::new(),
            },
            Pick::Bitmap => {
                let b = bitmap?;
                if let Some(i) = singles.iter().position(|p| p.as_ref() == Some(&b)) {
                    let (name, ix) = serving.get(i)?;
                    let (itotal, isel) =
                        costsize::bitmap_index_scan(&rel, &plain(ix), &quals, colsel(&ix.field))?;
                    ScanIndex {
                        name: name.clone(),
                        only: false,
                        bitmap: Some((0.0, itotal, costsize::clamp_row_est(isel * pt.tuples), 0)),
                        or_arms: Vec::new(),
                    }
                } else {
                    // A BitmapOr of one index scan per arm; anything else
                    // (a BitmapAnd) is not printed here.
                    let (k, (or, owners)) = ors.iter().enumerate().find(|(_, (or, _))| *or == b)?;
                    let mut arms = Vec::with_capacity(owners.len());
                    for (owner, arm) in owners.iter().zip(&w.bitmap_ors.get(k)?.1) {
                        let (name, ix) = serving.get((*owner)?)?;
                        let (aix, aquals, asel, _) = arm_scan(ix, arm)?;
                        let (itotal, isel) = costsize::bitmap_index_scan(
                            &rel,
                            &aix,
                            &aquals,
                            costsize::ColSel::Given(asel),
                        )?;
                        arms.push((
                            name.clone(),
                            (0.0, itotal, costsize::clamp_row_est(isel * pt.tuples), 0),
                        ));
                    }
                    ScanIndex {
                        name: String::new(),
                        only: false,
                        bitmap: Some((
                            or.cost,
                            or.cost,
                            costsize::clamp_row_est(or.selectivity * pt.tuples),
                            0,
                        )),
                        or_arms: arms,
                    }
                }
            }
        };
        let width = output_width(&pt.def, Some(stats), fields)?;
        Some(UserPath {
            chosen: Some(((startup, total, rows, width), Some(index))),
            ordered,
        })
    }

    pub(crate) fn user_seq_scan(
        &self,
        table: &str,
        filter: &Document,
        fields: &[String],
    ) -> Option<Estimate> {
        self.user_seq_scan_with(table, filter, fields, &[], &[])
    }

    /// A user table's sequential scan whose WHERE runs per row, in part or
    /// whole, as comparisons of expressions with constants (RP-404): each
    /// such conjunct a `Filter` of its operators, selecting what
    /// `clauselist_selectivity` estimates for an expression with no
    /// statistics -- `DEFAULT_INEQ_SEL` a bound, a pair of bounds on one
    /// expression `DEFAULT_RANGE_INEQ_SEL`, an equality the reciprocal of
    /// `get_variable_numdistinct`'s guess (the table's rows under 200, else
    /// 200) -- times the lowered filter's selectivity. `None` where an
    /// index path competes, which is not modelled with such a filter: an
    /// index on an expression (whose statistics ANALYZE keeps), a lowered
    /// filter over a table with any index, or an index holding every column
    /// the scan reads (an index-only scan).
    pub(crate) fn residual_scan_estimate(
        &self,
        table: &str,
        filter: &Document,
        exprs: &[secantus_pgplan::ExprQual],
        fields: &[String],
        reads: &[String],
    ) -> Option<Estimate> {
        if exprs.is_empty()
            || !self
                .shared_table_defs()
                .ok()?
                .iter()
                .any(|d| d.name == table)
        {
            return None;
        }
        let relations = self.index_relations();
        let mut read: Vec<String> = fields.iter().chain(reads).cloned().collect();
        for q in exprs {
            read.extend(q.fields.iter().cloned());
        }
        for r in relations.iter().filter(|r| r.table.name == table) {
            if !filter.is_empty() || r.keys.contains(&0) {
                return None;
            }
            let held: Vec<String> = r
                .keys
                .iter()
                .chain(&r.include)
                .filter_map(|k| usize::try_from(*k).ok()?.checked_sub(1))
                .filter_map(|i| r.table.columns.get(i))
                .map(|c| c.field())
                .collect();
            if read.iter().all(|f| held.contains(f)) {
                return None;
            }
        }
        self.user_seq_scan_with(table, filter, fields, &[], exprs)
    }

    /// A user table's sequential scan whose WHERE is only array operators
    /// against constants (RP-367) and `constant OP ANY / ALL (array
    /// column)` (RP-368): each a `Filter`, selecting what `arraycontsel` /
    /// `scalararraysel` estimate over the column's element statistics.
    /// `None` over a catalog.
    pub(crate) fn array_qual_scan_estimate(
        &self,
        table: &str,
        quals: &[secantus_pgplan::ArrayQual],
        fields: &[String],
    ) -> Option<Estimate> {
        if quals.is_empty()
            || !self
                .shared_table_defs()
                .ok()?
                .iter()
                .any(|d| d.name == table)
        {
            return None;
        }
        self.user_seq_scan_with(table, &Document::new(), fields, quals, &[])
    }

    /// One array conjunct's selectivity (`arraycontsel`, or
    /// `scalararraysel` of a `ScalarArrayOpExpr` over the column). `None`
    /// over an analysed table whose column's statistics are not modelled,
    /// or a constant element with no key.
    fn array_qual_selectivity(
        &self,
        pt: &PlannerTable,
        q: &secantus_pgplan::ArrayQual,
    ) -> Option<f64> {
        use crate::array_selfuncs as af;
        let col = pt.def.columns.iter().find(|c| c.field() == q.field)?;
        let stats = match &pt.analyzed {
            None => af::ColumnStats::None,
            Some(columns) => {
                let a = columns.get(&q.field)?.as_ref()?;
                af::ColumnStats::Analysed {
                    null_frac: a.stats.null_frac,
                    slots: a.elements.as_ref(),
                }
            }
        };
        if let Some((use_or, containment)) = q.scalar {
            return match containment {
                Some(is_equality) => {
                    let key = self.array_element_keys(col, &q.elems)?;
                    Some(af::scalararraysel_containment(
                        key.first()?,
                        is_equality,
                        use_or,
                        &stats,
                    ))
                }
                None => af::scalararraysel_default(&q.op, use_or),
            };
        }
        let keys = self.array_element_keys(col, &q.elems)?;
        Some(af::arraycontsel(
            af::ArrayOp::of(&q.op)?,
            &keys,
            q.has_null,
            &stats,
        ))
    }

    /// The sort keys of an array column's element values, ascending and
    /// distinct, as ANALYZE keyed its MCELEM slot.
    fn array_element_keys(
        &self,
        col: &secantus_pgcatalog::Column,
        elems: &[Bson],
    ) -> Option<Vec<Vec<u8>>> {
        let e = crate::statistic::element_column(col)?;
        let mut keys = elems
            .iter()
            .map(|v| {
                Self::analyze_sample(&e, Some(v), true)
                    .flatten()
                    .map(|(k, _)| k)
            })
            .collect::<Option<Vec<_>>>()?;
        keys.sort();
        keys.dedup();
        Some(keys)
    }

    fn user_seq_scan_with(
        &self,
        table: &str,
        filter: &Document,
        fields: &[String],
        array_quals: &[secantus_pgplan::ArrayQual],
        expr_quals: &[secantus_pgplan::ExprQual],
    ) -> Option<Estimate> {
        let counted: std::cell::OnceCell<Option<usize>> = std::cell::OnceCell::new();
        let count = || {
            *counted.get_or_init(|| {
                self.storage
                    .count_matching(self.db(), table, &Document::new(), None)
                    .ok()
            })
        };
        let pt = self.planner_table_lazy(table, &count)?;
        let (sel, quals) = self.where_selectivity(table, &pt, filter, &count)?;
        let mut quals = with_in_lengths(quals);
        let mut sel = sel;
        for q in array_quals {
            sel *= self.array_qual_selectivity(&pt, q)?;
            quals.push((
                q.field.clone(),
                costsize::Qual::Filter(q.operators_per_row()),
            ));
        }
        if !expr_quals.is_empty() {
            sel *= expr_selectivity(expr_quals, pt.tuples)?;
            for q in expr_quals {
                quals.push((
                    String::new(),
                    costsize::Qual::Filter(f64::from(q.operators)),
                ));
            }
        }
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

    /// `clauselist_selectivity` of `filter` over user table `table`
    /// (`pt`), from its columns' statistics where it was analysed and
    /// PostgreSQL's defaults where not, and its quals as the cost model
    /// sees them; `count` counts the table's rows.
    fn where_selectivity(
        &self,
        table: &str,
        pt: &crate::PlannerTable,
        filter: &Document,
        count: &dyn Fn() -> Option<usize>,
    ) -> Option<(f64, Vec<(String, costsize::Qual)>)> {
        if filter.is_empty() {
            return Some((1.0, Vec::new()));
        }
        if let Some(stats) = &pt.analyzed {
            let indexes = self.cost_indexes(table, &pt.def, count()? as u64);
            let w = self.analyzed_where(
                table, &pt.def, &pt.unique, stats, pt.tuples, filter, &indexes,
            )?;
            return Some((w.selectivity, w.quals));
        }
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
        Some((
            hashagg::filter_selectivity(filter, &nd)?,
            costsize::conjuncts(filter)?,
        ))
    }

    /// `clause_selectivity` of a restriction clause over user table
    /// `table`, lowered to `filter` ([`Self::where_selectivity`]).
    fn restriction_selectivity(&self, table: &str, filter: &Document) -> Option<f64> {
        let counted: std::cell::OnceCell<Option<usize>> = std::cell::OnceCell::new();
        let count = || {
            *counted.get_or_init(|| {
                self.storage
                    .count_matching(self.db(), table, &Document::new(), None)
                    .ok()
            })
        };
        let pt = self.planner_table_lazy(table, &count)?;
        Some(self.where_selectivity(table, &pt, filter, &count)?.0)
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
                        elements: Self::stored_element_slots(entry),
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
                    elements: None,
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
                    later: Vec::new(),
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
                    or_arms: Vec::new(),
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
                    or_arms: Vec::new(),
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
        if !self
            .shared_table_defs()
            .ok()?
            .iter()
            .any(|d| d.name == table)
        {
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
/// Whether an index scan already in the ORDER BY's order, `(startup,
/// total)`, is the path `create_ordered_paths` keeps over `sort`, the Sort
/// of the cheapest path (RP-382): `add_path` compares their totals within
/// `STD_FUZZ_FACTOR`, a tie going to the cheaper startup.
pub(crate) fn presorted_wins((startup, total): (f64, f64), sort: Estimate) -> bool {
    const STD_FUZZ_FACTOR: f64 = 1.01;
    let (sort_startup, sort_total, _, _) = sort;
    if total * STD_FUZZ_FACTOR < sort_total {
        true
    } else if sort_total * STD_FUZZ_FACTOR < total {
        false
    } else {
        startup < sort_startup
    }
}

#[cfg(test)]
mod presorted_tests {
    use super::presorted_wins;

    /// PostgreSQL 15.19 sorts `n`'s bitmap scan (`Sort (cost=37.57..37.82)`)
    /// rather than reading `n_ab` whole in its order, and an index scan
    /// cheaper than the Sort by more than the fuzz wins (RP-382).
    #[test]
    fn the_cheaper_total_beyond_the_fuzz_keeps_its_path() {
        let sort = (37.57, 37.82, 99.0, 11);
        assert!(!presorted_wins((0.29, 211.0), sort));
        assert!(presorted_wins((0.29, 30.0), sort));
        // Within 1%: the cheaper startup.
        assert!(presorted_wins((0.29, 37.9), sort));
        assert!(!presorted_wins((40.0, 37.9), sort));
    }
}

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
/// The stored fields a computed select list reads (RP-378): an index-only
/// scan's index must hold them too.
pub(crate) fn computed_reads(sel: &secantus_pgplan::Select) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in sel.casts.iter().flatten() {
        if let secantus_pgplan::ColumnExpr::Row { fields, .. } = c {
            for (_, f, _) in fields {
                if !out.contains(f) {
                    out.push(f.clone());
                }
            }
        }
    }
    out
}

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

thread_local! {
    /// The EXPLAIN being estimated: its single relation's LIKE / ILIKE
    /// conjuncts, which the lowered filter keeps only as regexes (RP-404).
    static EXPLAIN_PATTERNS: std::cell::RefCell<Vec<secantus_pgplan::ScanPattern>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Holds an EXPLAIN's LIKE patterns until it is dropped.
pub(crate) struct PatternsGuard(());

impl Drop for PatternsGuard {
    fn drop(&mut self) {
        EXPLAIN_PATTERNS.with(|c| c.borrow_mut().clear());
    }
}

/// Make `patterns` (`ExplainOptions::scan_patterns`) what this EXPLAIN's
/// scans read a `$regex` clause as, until the guard is dropped.
pub(crate) fn explain_patterns(patterns: &[secantus_pgplan::ScanPattern]) -> PatternsGuard {
    EXPLAIN_PATTERNS.with(|c| *c.borrow_mut() = patterns.to_vec());
    PatternsGuard(())
}

/// The LIKE / ILIKE pattern behind a lowered `$regex` clause on `field`:
/// `(pattern, insensitive)`, or `None` where this EXPLAIN holds none that
/// lowers to it (a regex operator's, say).
pub(crate) fn explain_pattern(
    field: &str,
    regex: &str,
    insensitive: bool,
    negated: bool,
) -> Option<(String, bool)> {
    EXPLAIN_PATTERNS.with(|c| {
        c.borrow()
            .iter()
            .find(|p| {
                p.field == field
                    && p.regex == regex
                    && p.insensitive == insensitive
                    && p.negated == negated
            })
            .map(|p| (p.pattern.clone(), p.insensitive))
    })
}

/// `clauselist_selectivity` over comparisons of expressions with
/// constants, none with statistics (`examine_variable`): each expression a
/// column of `tuples` rows with no `pg_statistic` row, so two bounds on one
/// expression pair as a range does.
fn expr_selectivity(quals: &[secantus_pgplan::ExprQual], tuples: f64) -> Option<f64> {
    use secantus_pgplan::colstats::Column;
    let mut exprs: Vec<&str> = Vec::new();
    for q in quals {
        if !exprs.contains(&q.expr.as_str()) {
            exprs.push(&q.expr);
        }
    }
    let columns: Vec<Column> = exprs
        .iter()
        .map(|_| Column {
            stats: None,
            unique: false,
            boolean: false,
            numeric: false,
            ordered: false,
            string: false,
            c_collation: false,
            actual: None,
            tuples,
        })
        .collect();
    let clauses: Vec<(&Column, secantus_pgplan::colstats::Op, Vec<u8>)> = quals
        .iter()
        .map(|q| {
            let i = exprs.iter().position(|e| *e == q.expr)?;
            Some((&columns[i], q.op, Vec::new()))
        })
        .collect::<Option<_>>()?;
    secantus_pgplan::colstats::clauselist(&clauses)
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
/// One side of a two-table join planned as joined rows
/// ([`PgHandler::joined_sides`]).
struct JoinedSide {
    /// Its join key, as the joined rows name it (`alias` SEP `column`).
    key: String,
    table: String,
    alias: String,
    /// Its scan's filter, over stored fields, shared by every key of
    /// its table.
    filter: std::rc::Rc<Document>,
    /// Its join key's stored field.
    field: String,
    /// The fields its scan reads, and those it carries above the join.
    read: Vec<String>,
    above: Vec<String>,
}

/// The constant `filter` sets stored field `field` equal to, at its top
/// level or in a top-level `$and`: `None` where it sets none, `Some(None)`
/// where it sets several, NULL or one only a non-equality reaches.
fn key_constant(filter: &Document, field: &str) -> Option<Option<bson::Bson>> {
    let mut found: Vec<&bson::Bson> = Vec::new();
    let mut other = false;
    fn walk<'a>(d: &'a Document, field: &str, found: &mut Vec<&'a bson::Bson>, other: &mut bool) {
        for (k, v) in d {
            if k == "$and" {
                if let bson::Bson::Array(a) = v {
                    for x in a.iter().filter_map(bson::Bson::as_document) {
                        walk(x, field, found, other);
                    }
                }
            } else if k == field {
                match v {
                    bson::Bson::Document(op) if op.len() == 1 && op.contains_key("$eq") => {
                        found.push(op.get("$eq").expect("checked"));
                    }
                    bson::Bson::Document(_) | bson::Bson::RegularExpression(_) => *other = true,
                    v => found.push(v),
                }
            }
        }
    }
    walk(filter, field, &mut found, &mut other);
    let first = found.first()?;
    let same = found
        .iter()
        .all(|v| secantus_pgplan::compare_values(v, first) == Some(std::cmp::Ordering::Equal));
    if other || !same || matches!(first, bson::Bson::Null) {
        return Some(None);
    }
    Some(Some((*first).clone()))
}

/// The WHERE conjuncts the planner already pushed into a scan: its filter
/// itself, and the single-field conjuncts of its top-level `$and`, found
/// by field and value in constant time. Scanning the `$and` once per
/// conjunct above the join made EXPLAIN quadratic in the number of
/// conjuncts a client writes (RP-388).
struct PushedConjuncts<'f> {
    filter: &'f Document,
    by_key: HashMap<(&'f str, u64), Vec<&'f bson::Bson>>,
}

impl<'f> PushedConjuncts<'f> {
    fn new(filter: &'f Document) -> Self {
        let mut by_key: HashMap<(&'f str, u64), Vec<&'f bson::Bson>> = HashMap::new();
        if let Ok(items) = filter.get_array("$and") {
            for d in items.iter().filter_map(bson::Bson::as_document) {
                if let (1, Some((field, v))) = (d.len(), d.iter().next()) {
                    by_key
                        .entry((field.as_str(), bson_value_hash(v)))
                        .or_default()
                        .push(v);
                }
            }
        }
        Self { filter, by_key }
    }

    /// `filter` is `{field: v}` itself, or holds it as one of its
    /// top-level `$and` conjuncts.
    fn holds(&self, field: &str, v: &bson::Bson) -> bool {
        self.filter.get(field) == Some(v)
            || self
                .by_key
                .get(&(field, bson_value_hash(v)))
                .is_some_and(|found| found.contains(&v))
    }
}

/// A hash of a BSON value that agrees with `Bson`'s `==`: values it calls
/// equal hash alike (a document's fields in any order, as `IndexMap`
/// compares them, and `-0.0` as `0.0`). Kinds it does not look into hash
/// by kind alone, which is coarse but never wrong: `holds` compares the
/// values a hash finds.
fn bson_value_hash(v: &bson::Bson) -> u64 {
    use std::hash::{Hash, Hasher};
    fn feed(v: &bson::Bson, h: &mut std::hash::DefaultHasher) {
        std::mem::discriminant(v).hash(h);
        match v {
            bson::Bson::Double(f) => (if *f == 0.0 { 0 } else { f.to_bits() }).hash(h),
            bson::Bson::String(s) | bson::Bson::Symbol(s) | bson::Bson::JavaScriptCode(s) => {
                s.hash(h)
            }
            bson::Bson::Int32(i) => i.hash(h),
            bson::Bson::Int64(i) => i.hash(h),
            bson::Bson::Boolean(b) => b.hash(h),
            bson::Bson::DateTime(d) => d.timestamp_millis().hash(h),
            bson::Bson::Decimal128(d) => d.bytes().hash(h),
            bson::Bson::ObjectId(o) => o.hash(h),
            bson::Bson::Binary(b) => b.bytes.hash(h),
            bson::Bson::Array(items) => {
                items.len().hash(h);
                for item in items {
                    feed(item, h);
                }
            }
            bson::Bson::Document(d) => {
                d.len().hash(h);
                let mut fields: u64 = 0;
                for (k, x) in d {
                    let mut one = std::hash::DefaultHasher::new();
                    k.hash(&mut one);
                    feed(x, &mut one);
                    fields = fields.wrapping_add(one.finish());
                }
                fields.hash(h);
            }
            _ => {}
        }
    }
    let mut h = std::hash::DefaultHasher::new();
    feed(v, &mut h);
    h.finish()
}

struct HashSide {
    scan: Estimate,
    /// The index the scan reads, when it is an Index Scan (RP-369).
    index: Option<String>,
    /// Its WHERE fixes a unique index's columns: at most one row, so a
    /// join with it inner is `inner_unique` (RP-369).
    one_row: bool,
    above: i64,
    pg_type: String,
    stats: (f64, f64, Vec<(Vec<u8>, f32)>),
    unique: bool,
    tuples: f64,
    /// The key column's statistics and how its keys order, for the merge
    /// path's `mergejoinscansel` (RP-366).
    column: Option<(
        secantus_pgplan::hashagg::ColumnStats,
        secantus_pgplan::colstats::Slots,
    )>,
    numeric: bool,
    ordered: bool,
    /// A B-tree index leads with the key: a merge could read it presorted,
    /// which is not modelled.
    key_indexed: bool,
    /// That index, as `cost_index` sizes it over the analysed table, for
    /// the whole-index and parameterized scans a join on a unique key
    /// weighs (RP-377). `None` where it is not one plain B-tree.
    key_index: Option<KeyIndex>,
}

/// A join key's B-tree index over an analysed table (RP-377): the table
/// and index as `cost_index` reads them, the side's own WHERE quals (each
/// checked on every row fetched), its key's distinct count and the
/// table's rows its WHERE keeps.
struct KeyIndex {
    rel: secantus_pgplan::costsize::Rel,
    index: secantus_pgplan::costsize::Index,
    quals: Vec<(String, secantus_pgplan::costsize::Qual)>,
    ndistinct: f64,
    /// `scan.2 / tuples`: the WHERE's selectivity.
    where_sel: f64,
    /// The heap's all-visible fraction (`allvisfrac`), the pages an
    /// index-only scan does not fetch.
    allvisfrac: f64,
}

impl HashSide {
    /// Its key column's average width, for a Memoize keyed on it.
    fn key_index_width(&self) -> Option<i64> {
        self.column
            .as_ref()
            .map(|(stats, _)| i64::from(stats.width))
    }
}

/// [`PgHandler::nest_loop_estimate`]'s answer: the plan, the two scans'
/// and the join's estimates, the inner's parameterized index scan and the
/// preserved side's index path, where either is chosen.
pub(crate) type NestLoopEstimate = (
    secantus_pgplan::joincost::NestLoopPlan,
    Estimate,
    Estimate,
    Estimate,
    Option<ParamScan>,
    Option<ScanIndex>,
);

/// A nested loop's side as [`PgHandler::nest_loop_estimate`] reads it: its
/// table, each key with its stored field and SQL column, and the stored
/// fields its scan reads.
struct SideLeaf {
    table: String,
    keys: Vec<(String, String, String)>,
    read: Vec<String>,
}

/// An operand of a `<>` between joined keys, for `eqjoinsel` and
/// `var_eq_non_const`: its column's statistics, whether it is a cast of
/// the column -- an expression `examine_variable` finds no statistics for
/// -- and the type it compares as.
struct NeOperand {
    key: AnalysedKey,
    cast: bool,
    type_oid: i64,
}

impl NeOperand {
    /// `get_variable_numdistinct` of a cast key: no statistics, so the
    /// table's rows where fewer than `DEFAULT_NUM_DISTINCT`, else that
    /// default.
    fn expression_ndistinct(&self) -> (f64, bool) {
        let tuples = self.key.tuples;
        if tuples <= 0.0 || tuples >= 200.0 {
            (200.0, true)
        } else {
            (costsize::clamp_row_est(tuples), false)
        }
    }

    fn stats(&self) -> secantus_pgplan::joincost::KeyStats<'_> {
        if self.cast {
            let (ndistinct, default_ndistinct) = self.expression_ndistinct();
            secantus_pgplan::joincost::KeyStats {
                ndistinct,
                default_ndistinct,
                null_frac: 0.0,
                mcv: &[],
            }
        } else {
            secantus_pgplan::joincost::KeyStats {
                ndistinct: self.key.ndistinct,
                default_ndistinct: false,
                null_frac: self.key.null_frac,
                mcv: &self.key.mcv,
            }
        }
    }

    /// `var_eq_non_const` negated: the fraction of this side's rows
    /// unequal to a value of the other side's, a pseudo-constant here.
    fn not_equal_non_const(&self) -> f64 {
        let (selec, null_frac) = if self.cast {
            (1.0 / self.expression_ndistinct().0, 0.0)
        } else {
            let k = &self.key;
            let selec = if k.unique && k.tuples >= 1.0 {
                1.0 / k.tuples
            } else {
                let mut selec = 1.0 - k.null_frac;
                if k.ndistinct > 1.0 {
                    selec /= k.ndistinct;
                }
                // Never more than the most common value's frequency.
                if let Some((_, first)) = k.mcv.first() {
                    selec = selec.min(f64::from(*first));
                }
                selec
            };
            (selec, k.null_frac)
        };
        (1.0 - selec - null_frac).clamp(0.0, 1.0)
    }
}

/// `o`'s table, statistics and compared type. `None` where its column was
/// never analysed or its type is not a built-in one.
fn ne_operand(
    h: &PgHandler,
    key_columns: &[(String, String, String)],
    o: &secantus_pgplan::joins::CastKey,
) -> Option<NeOperand> {
    let (_, table, field) = key_columns.iter().find(|(c, _, _)| *c == o.key)?;
    let key = h.analysed_key(table, field)?;
    let type_oid = match o.cast {
        Some(t) => t,
        None => secantus_pgplan::pgtypes::oid_of_name(&key.column.pg_type)?,
    };
    Some(NeOperand {
        key,
        cast: o.cast.is_some(),
        type_oid,
    })
}

/// `clauselist_selectivity` of the join clauses `filters` with `varRelid`
/// `leaf`'s relation, as `get_parameterized_baserel_size` estimates the
/// rows of a scan of it parameterized by the other side (RP-354): each
/// clause a restriction of `leaf`, the other side's value a
/// pseudo-constant. An inequality is `DEFAULT_INEQ_SEL` (`scalarineqsel`
/// with no constant to compare against); a `<>` is `var_eq_non_const`
/// negated over `leaf`'s operand. `None` for any other clause.
fn param_selectivity(
    h: &PgHandler,
    leaf: &SideLeaf,
    filters: &[&secantus_pgplan::joins::JoinFilter],
    key_columns: &[(String, String, String)],
) -> Option<f64> {
    use secantus_pgplan::joins::{CastKey, FilterKind};
    let mine = |o: &CastKey| leaf.keys.iter().any(|(k, _, _)| *k == o.key);
    let mut sel = 1.0_f64;
    for f in filters {
        sel *= match &f.kind {
            FilterKind::Inequality => 1.0 / 3.0,
            FilterKind::NotEqual(..) | FilterKind::NotEqualCast(..) => {
                let (l, r) = match &f.kind {
                    FilterKind::NotEqual(l, r) => (
                        CastKey {
                            key: l.clone(),
                            cast: None,
                        },
                        CastKey {
                            key: r.clone(),
                            cast: None,
                        },
                    ),
                    FilterKind::NotEqualCast(l, r) => (l.clone(), r.clone()),
                    _ => unreachable!("matched above"),
                };
                let own = match (mine(&l), mine(&r)) {
                    (true, false) => l,
                    (false, true) => r,
                    _ => return None,
                };
                ne_operand(h, key_columns, &own)?.not_equal_non_const()
            }
            _ => return None,
        };
    }
    Some(sel)
}

/// A nested loop's inner index scan parameterized by the outer row
/// (RP-354): the index, the scan's alias, and the column whose join
/// clause is its `Index Cond`.
#[derive(Debug, Clone)]
pub(crate) struct ParamScan {
    pub index: String,
    pub alias: String,
    pub column: String,
    /// Read index-only: the index holds every column the side reads.
    pub index_only: bool,
}

enum ParamFound {
    None,
    Unmodelled,
    Scan(secantus_pgplan::joincost::ParamInner, ParamScan),
}

/// A comparison operator with its operands swapped.
fn commuted(op: &str) -> &str {
    match op {
        "<" => ">",
        "<=" => ">=",
        ">" => "<",
        ">=" => "<=",
        other => other,
    }
}

/// One analysed column's statistics (`PgHandler::analysed_key`).
struct AnalysedKey {
    tuples: f64,
    column: secantus_pgcatalog::Column,
    stats: secantus_pgplan::hashagg::ColumnStats,
    slots: Option<secantus_pgplan::colstats::Slots>,
    unique: bool,
    ndistinct: f64,
    null_frac: f64,
    mcv: Vec<(Vec<u8>, f32)>,
}

/// The most further equality clauses [`hash_join_of`] prices.
const MORE_HASH_CLAUSES: usize = 3;

/// `cost_hashjoin` over two priced sides, the hashed one PostgreSQL's
/// `add_path` keeps (RP-330, RP-359): the printed outer scan's, the hashed
/// scan's and the join's estimates, and whether the left table is hashed.
/// With `left_join`, `a` is the preserved side: the join keeps at least
/// its rows (`calc_joinrel_size_estimate`), and the swapped path is the
/// `Hash Right Join` PostgreSQL builds the other way round.
/// With `redundant`, the join clause is one an equivalence class with a
/// constant already enforces on both sides, so its selectivity is 1
/// (`clause_selectivity` of a clause whose `norm_selec` is above 1,
/// RP-364).
/// `more` holds the further equality clauses of a join on several (RP-374),
/// each as its two sides' key columns (`a`'s, `b`'s): `cost_hashjoin`
/// multiplies the clauses' selectivities (`clauselist_selectivity`), pays
/// each clause per built and per probed row, and buckets on the smallest
/// of their `estimate_hash_bucket_stats`; a side is `inner_unique` where
/// any one of its keys is unique.
fn hash_join_of(
    a: HashSide,
    b: HashSide,
    more: &[(HashSide, HashSide)],
    left_join: bool,
    redundant: bool,
    filter: JoinFilterCost,
) -> Option<JoinEstimate> {
    use secantus_pgplan::joincost::{self, JoinSide, KeyStats};
    if (filter.count != 0 && redundant)
        || a.pg_type != b.pg_type
        || more.len() > MORE_HASH_CLAUSES
        || more.iter().any(|(x, y)| x.pg_type != y.pg_type)
        || (!more.is_empty() && (left_join || redundant))
    {
        return None;
    }
    let multi = !more.is_empty();
    fn key(s: &HashSide) -> KeyStats<'_> {
        KeyStats {
            ndistinct: s.stats.0,
            default_ndistinct: false,
            null_frac: s.stats.1,
            mcv: &s.stats.2,
        }
    }
    let (ka, kb) = (key(&a), key(&b));
    let mut selectivity = if redundant {
        1.0
    } else {
        joincost::eqjoinsel_inner(&ka, &kb)
    };
    for (x, y) in more {
        selectivity *= joincost::eqjoinsel_inner(&key(x), &key(y));
    }
    let keys_a: Vec<KeyStats<'_>> = std::iter::once(key(&a))
        .chain(more.iter().map(|(x, _)| key(x)))
        .collect();
    let keys_b: Vec<KeyStats<'_>> = std::iter::once(key(&b))
        .chain(more.iter().map(|(_, y)| key(y)))
        .collect();
    let extra = joincost::ExtraKeys {
        n: more.len(),
        ..joincost::ExtraKeys::default()
    };
    // `innerrel_is_unique` over the join's clauses: one of them on a
    // unique column of the side makes it `inner_unique` when hashed.
    let unique_a = a.unique || more.iter().any(|(x, _)| x.unique);
    let unique_b = b.unique || more.iter().any(|(_, y)| y.unique);
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
        extra,
    };
    // A side unique on its one key is `inner_unique` when hashed, and
    // PostgreSQL then weighs a merge join reading its index presorted and a
    // nested loop probing it (RP-377): priced below over each side's
    // pathlist. An outer join or a join filter there is not modelled.
    let unique_key = !multi && (a.unique || b.unique);
    if unique_key && redundant {
        return None;
    }
    // A side its own WHERE cuts to one row is `inner_unique` as the inner
    // side of any path (RP-369): `unique` is read only on the inner one.
    let (outer, inner) = (
        JoinSide {
            unique: a.one_row || unique_a,
            ..join_side(&a)
        },
        JoinSide {
            unique: b.one_row || unique_b,
            ..join_side(&b)
        },
    );
    // The join filter (RP-375): `final_cost_hashjoin` charges its
    // `qp_qual_cost` on each tuple the hash clauses pass -- for a unique
    // inner side the outer rows that match, the filter's selectivity
    // among the semi-join factors (RP-381).
    let qp = filter.ops as f64 * costsize::CPU_OPERATOR_COST;
    let (startup, total) = joincost::hash_join_stats_filtered(
        &outer,
        &inner,
        selectivity,
        filter.selectivity,
        qp,
        &keys_b,
    );
    // PostgreSQL builds the path hashing the right relation first
    // (`add_paths_to_joinrel` with the left one outer) and then the
    // other way round; `add_path` keeps the second only when it is
    // cheaper beyond the fuzz factor (`compare_path_costs_fuzzily`,
    // 1.01), and `set_cheapest` then takes the cheaper total. The
    // plan printed follows it: with `swap`, the left table hashed
    // (RP-359).
    let (l_startup, l_total) = joincost::hash_join_stats_filtered(
        &inner,
        &outer,
        selectivity,
        filter.selectivity,
        qp,
        &keys_a,
    );
    // The join filter (RP-375): `final_cost_hashjoin` and
    // `final_cost_mergejoin` charge its `qp_qual_cost` on each pair the
    // join clauses match (`approx_tuple_count`), and each inequality
    // scales the join's rows by its selectivity (RP-379: a `<>` by
    // `neqjoinsel`'s).
    let matched = costsize::clamp_row_est(outer.rows * inner.rows * selectivity);
    let filter_qual = qp * matched;
    let mut rows = outer.rows * inner.rows * selectivity * filter.selectivity;
    if left_join {
        rows = rows.max(outer.rows);
    }
    let rows = costsize::clamp_row_est(rows);
    let width = a.above + b.above;
    // The merge paths (RP-366): each side sorted from its scan, the
    // clause's tuples `approx_tuple_count`'s (the hash join's too). Not
    // where a redundant clause is the only one (its merge is not
    // modelled), nor where an index leads with a key (a presorted input).
    // A merge on several clauses sorts on all their keys, whose
    // `mergejoinscansel` and sort are not modelled: it is not compared.
    let merges = if redundant || multi || a.key_indexed || b.key_indexed {
        None
    } else {
        fn column(s: &HashSide) -> Option<secantus_pgplan::colstats::Column<'_>> {
            let (stats, slots) = s.column.as_ref()?;
            Some(secantus_pgplan::colstats::Column {
                stats: Some((*stats, slots)),
                unique: s.unique,
                boolean: false,
                numeric: s.numeric,
                ordered: s.ordered,
                // A string key's place in a bin (`convert_string_to_scalar`)
                // is not read for a merge's scan fractions yet: such a merge
                // stays unpriced, as it was.
                string: false,
                c_collation: false,
                actual: None,
                tuples: s.tuples,
            })
        }
        let (ca, cb) = (column(&a)?, column(&b)?);
        let (mut ls, mut le, rs, re) = joincost::mergejoinscansel(&ca, &cb)?;
        // `initial_cost_mergejoin`: an outer join reads its preserved
        // input whole -- the LEFT join's left table, outer in the `JOIN_LEFT`
        // path and inner in the `JOIN_RIGHT` one.
        if left_join {
            ls = 0.0;
            le = 1.0;
        }
        let tuples = costsize::clamp_row_est(outer.rows * inner.rows * selectivity);
        let with_filter = |mut m: joincost::MergePlan| {
            m.total += filter_qual;
            m
        };
        Some((
            with_filter(joincost::merge_join_sorted(
                &outer,
                &inner,
                (ls, le, rs, re),
                tuples,
            )),
            with_filter(joincost::merge_join_sorted(
                &inner,
                &outer,
                (rs, re, ls, le),
                tuples,
            )),
        ))
    };
    // `add_paths_to_joinrel` with the left relation outer, then the other
    // way round: each time the merge path (`sort_inner_and_outer`) before
    // the hash path (`hash_inner_and_outer`), each through `add_path`
    // (`compare_path_costs_fuzzily` with the 1.01 fuzz -- total first, the
    // startup deciding only between fuzzily equal totals, startup costs not
    // being considered with no LIMIT -- and between fuzzily equal paths a
    // 1.0000000001 fuzz, RP-364); then `set_cheapest` takes the cheapest
    // total, a tie the cheaper startup.
    let mut candidates: Vec<(JoinPick, (f64, f64))> = Vec::new();
    if let Some((m_a, _)) = merges {
        candidates.push((
            JoinPick::Merge {
                b_outer: false,
                plan: m_a,
            },
            (m_a.startup, m_a.total),
        ));
    }
    candidates.push((JoinPick::Hash { swap: false }, (startup, total)));
    if let Some((_, m_b)) = merges {
        candidates.push((
            JoinPick::Merge {
                b_outer: true,
                plan: m_b,
            },
            (m_b.startup, m_b.total),
        ));
    }
    candidates.push((JoinPick::Hash { swap: true }, (l_startup, l_total)));
    let better = |new: (f64, f64), old: (f64, f64), fuzz: f64| -> Option<bool> {
        if new.1 > old.1 * fuzz {
            Some(false)
        } else if old.1 > new.1 * fuzz {
            Some(true)
        } else if new.0 > old.0 * fuzz {
            Some(false)
        } else if old.0 > new.0 * fuzz {
            Some(true)
        } else {
            None
        }
    };
    let mut kept: Vec<(JoinPick, (f64, f64))> = Vec::new();
    for (pick, cost) in candidates {
        // Against each kept path in turn: a better new path removes it, a
        // worse one is rejected.
        let mut accept = true;
        while let Some(old) = kept.first() {
            let wins = match better(cost, old.1, 1.01) {
                Some(w) => w,
                None => better(cost, old.1, 1.000_000_000_1) == Some(true),
            };
            if wins {
                kept.remove(0);
            } else {
                accept = false;
                break;
            }
        }
        if accept {
            kept.push((pick, cost));
        }
    }
    let (pick, (s0, t0)) = kept.into_iter().reduce(|best, c| {
        if c.1 .1 < best.1 .1 || (c.1 .1 == best.1 .1 && c.1 .0 < best.1 .0) {
            c
        } else {
            best
        }
    })?;
    // `match_unsorted_outer`'s nested loops are paths too, each pair
    // paying the join clause (`cpu_operator_cost` for its `=`). Over two
    // whole sequential scans they never come near; a side an index scan
    // cuts to a few rows (RP-369) can. Where one wins, or comes within
    // the fuzz factor, the plan is not this one: not priced.
    // The semi-join factors of an `inner_unique` inner side, as the hash
    // path's (`compute_semi_anti_join_factors` over the join clause).
    let semi = |s: &HashSide, unique: bool| {
        (s.one_row || unique).then(|| (selectivity, s.scan.2.max(1.0)))
    };
    let nest = joincost::cheapest_nest_loop_semi(
        a.scan,
        b.scan,
        costsize::CPU_OPERATOR_COST * (1.0 + more.len() as f64 + filter.ops as f64),
        [semi(&a, unique_a), semi(&b, unique_b)],
    );
    if nest.total <= t0 * 1.01 {
        return None;
    }
    if unique_key {
        let other = unique_key_other_paths(&a, &b, &outer, &inner, rows, left_join, filter)?;
        if other <= t0 * 1.01 {
            return None;
        }
    }
    let b_first = matches!(
        pick,
        JoinPick::Hash { swap: true } | JoinPick::Merge { b_outer: true, .. }
    );
    let (o, i, indexes) = if b_first {
        (b.scan, a.scan, [b.index, a.index])
    } else {
        (a.scan, b.scan, [a.index, b.index])
    };
    Some((o, i, (s0, t0, rows, width), pick, indexes))
}

/// The cheapest total of the merge joins and nested loops PostgreSQL's
/// `add_paths_to_joinrel` builds for `a` joined to `b` on one key, unique
/// on a side (RP-377): each side's pathlist as `set_base_rel_pathlists`
/// makes it -- its cheapest unparameterized scan, and for a side whose key
/// a B-tree leads with, that index read whole in the key's order and probed
/// by the other side's key (an index scan and a bitmap heap scan,
/// `get_loop_count` the other side's rows) -- through
/// `JoinRel::add_paths_general` with no hash path. `None` where a side's
/// key index is not modelled, or no such path is built.
fn unique_key_other_paths(
    a: &HashSide,
    b: &HashSide,
    side_a: &secantus_pgplan::joincost::JoinSide,
    side_b: &secantus_pgplan::joincost::JoinSide,
    rows: f64,
    left_join: bool,
    filter: JoinFilterCost,
) -> Option<f64> {
    use secantus_pgplan::joincost::{base_pathlist, InPath, JoinRel, MemoKey, RelPaths};
    // The join clause's own class (an edge's, `Search`'s numbering).
    const CLASS: usize = 2;
    let paths = |s: &HashSide, other: &HashSide, other_bit: u32| -> Option<Vec<InPath>> {
        let mut out = vec![InPath::scan(s.scan.0, s.scan.1, s.scan.2, None)];
        if !s.key_indexed {
            return Some(out);
        }
        let k = s.key_index.as_ref()?;
        let other_key = other.key_index_width()?;
        let (st, tt) = costsize::ordered_index_scan_with(
            &k.rel,
            &k.index,
            &k.quals,
            costsize::ColSel::Ndistinct(k.ndistinct),
        );
        out.push(InPath::scan(st, tt, s.scan.2, Some(CLASS)));
        // `get_parameterized_baserel_size`: the WHERE's selectivity and the
        // clause's (`var_eq_non_const`), at most the rows.
        let ppi_rows = costsize::clamp_row_est(k.rel.tuples * k.where_sel / k.ndistinct.max(1.0))
            .min(s.scan.2);
        let memo_key = MemoKey {
            ndistinct: other.stats.0,
            default: false,
            tuples: other.tuples,
            rel_rows: other.scan.2,
            width: other_key,
        };
        let param = |(startup, total): (f64, f64)| InPath {
            startup,
            total,
            class: None,
            rows: ppi_rows,
            req_outer: other_bit,
            rescan: (startup, total),
            indexed: true,
            memo_key: Some(memo_key),
        };
        let loops = other.scan.2;
        out.push(param(costsize::param_index_scan(
            &k.rel,
            &k.index,
            &k.quals,
            k.ndistinct,
            loops,
        )?));
        out.push(param(costsize::param_bitmap_heap_scan(
            &k.rel,
            &k.index,
            &k.quals,
            k.ndistinct,
            loops,
            ppi_rows,
        )?));
        Some(out)
    };
    let (pa, pb) = (paths(a, b, 2)?, paths(b, a, 1)?);
    let (ka, ta, sa) = base_pathlist(&pa, false)?;
    let (kb, tb, sb) = base_pathlist(&pb, false)?;
    let pa: Vec<InPath> = ka.iter().map(|&i| pa[i]).collect();
    let pb: Vec<InPath> = kb.iter().map(|&i| pb[i]).collect();
    fn rel_paths<'p>(
        side: &secantus_pgplan::joincost::JoinSide,
        paths: &'p [InPath],
        (cheapest_total, cheapest_startup): (usize, usize),
        s: &HashSide,
        bit: u32,
    ) -> RelPaths<'p> {
        RelPaths {
            side: *side,
            paths,
            cheapest_total,
            cheapest_startup,
            set: bit,
            width: s.scan.3,
            unique: side.unique,
            key_table: bit,
        }
    }
    let ra = rel_paths(side_a, &pa, (ta, sa), a, 1);
    let rb = rel_paths(side_b, &pb, (tb, sb), b, 2);
    let mut rel = JoinRel::without_hash(false);
    // The join filter, evaluated on each pair a path's clause passes (a
    // nested loop's every pair), RP-381.
    rel.set_filter_cost(filter.ops as f64 * costsize::CPU_OPERATOR_COST);
    rel.add_paths_general(
        0,
        Some(CLASS),
        Some(CLASS),
        (false, false),
        &|_| false,
        &ra,
        &rb,
        left_join,
        rows,
        0,
    );
    rel.pathlist()
        .iter()
        .filter(|p| p.req_outer == 0)
        .map(|p| p.total)
        .reduce(f64::min)
}

/// A priced two-table join: the printed outer scan's, the inner scan's and
/// the join's estimates, the path, and the index each scan reads (outer,
/// inner), `None` for a Seq Scan (RP-369).
/// An ON's conjuncts beside its join clauses, each an inequality or a
/// `<>` between the two tables (RP-375, RP-379): how many, the operators
/// they evaluate on each matched pair (`cost_qual_eval`), and their
/// selectivity (`clauselist_selectivity`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct JoinFilterCost {
    pub(crate) count: usize,
    pub(crate) ops: usize,
    pub(crate) selectivity: f64,
}

impl Default for JoinFilterCost {
    fn default() -> Self {
        Self {
            count: 0,
            ops: 0,
            selectivity: 1.0,
        }
    }
}

pub(crate) type JoinEstimate = (Estimate, Estimate, Estimate, JoinPick, [Option<String>; 2]);

/// The path a priced two-table join takes (RP-330 to RP-366): a hash join,
/// the left table hashed with `swap`; or a merge join of both sorted
/// inputs, the right table outer with `b_outer`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum JoinPick {
    Hash {
        swap: bool,
    },
    Merge {
        b_outer: bool,
        plan: secantus_pgplan::joincost::MergePlan,
    },
}

/// The filters an equivalence class adds to a LEFT join whose WHERE fixes
/// its outer key, `(alias, column)` each, and that outer key `(alias,
/// column)` (RP-364).
pub(crate) type LeftImplied = (Vec<(String, String)>, (String, String));

/// Whether a stored filter reads `field` anywhere (under `$and`, `$or`
/// and operators alike).
/// Does `filter` name `field` only to bound it -- `>`, `>=`, `<`, `<=`
/// against a constant, at its top or under `$and`? Such a qual is a
/// restriction of its scan (`distribute_restrictinfo_to_rels`) and leaves
/// the column's join clause in place, as an equality would not: that one
/// joins the key's equivalence class (RP-363).
/// `filter` is one bound of one column against a constant -- `{a: {$gt:
/// 990}}` -- the shape `clauselist_selectivity` defers as a range clause.
fn single_bound(filter: &Document) -> bool {
    match filter.iter().collect::<Vec<_>>().as_slice() {
        [(k, bson::Bson::Document(d))] if !k.starts_with('$') && d.len() == 1 => {
            bounds_only(filter, k)
        }
        _ => false,
    }
}

fn bounds_only(filter: &Document, field: &str) -> bool {
    filter.iter().all(|(k, v)| match (k.as_str(), v) {
        ("$and", bson::Bson::Array(items)) => items.iter().all(|x| match x {
            bson::Bson::Document(d) => bounds_only(d, field),
            _ => false,
        }),
        (k, bson::Bson::Document(d)) if k == field => {
            !d.is_empty()
                && d.iter().all(|(op, c)| {
                    matches!(op.as_str(), "$gt" | "$gte" | "$lt" | "$lte")
                        && !matches!(c, bson::Bson::Document(_) | bson::Bson::Array(_))
                })
        }
        (k, _) if k == field => false,
        (k, v) if k.starts_with('$') => !filter_names(&bson::doc! { "x": v.clone() }, field),
        (_, bson::Bson::Document(d)) => !filter_names(d, field),
        _ => true,
    })
}

fn filter_names(filter: &Document, field: &str) -> bool {
    filter.iter().any(|(k, v)| {
        k == field
            || match v {
                bson::Bson::Document(d) => filter_names(d, field),
                bson::Bson::Array(items) => items.iter().any(|x| match x {
                    bson::Bson::Document(d) => filter_names(d, field),
                    _ => false,
                }),
                _ => false,
            }
    })
}

/// `residual` is the equality `left = right` of two joined keys, either
/// way round, and nothing else: the select-level re-check of a comma
/// join's clause (RP-372).
/// `extra`: how many of the residual's conjuncts are the join's filter
/// (RP-375), which may read further keys.
fn is_key_equality(
    residual: &secantus_pgplan::ColumnExpr,
    pairs: &[(String, String)],
    extra: usize,
) -> bool {
    use pg_query::protobuf::node::Node as N;
    let secantus_pgplan::ColumnExpr::Row {
        expr,
        fields,
        params,
        ..
    } = residual
    else {
        return false;
    };
    let param = |x: Option<&pg_query::protobuf::Node>| match x.and_then(|x| x.node.as_ref()) {
        Some(N::ParamRef(p)) => usize::try_from(p.number).ok(),
        _ => None,
    };
    let key = |n: usize| {
        n.checked_sub(1)
            .and_then(|i| fields.get(i))
            .map(|f| f.0.as_str())
    };
    // One `=` of two joined keys: the pair it names.
    let equality = |node: &pg_query::protobuf::Node| -> Option<(&str, &str)> {
        let Some(N::AExpr(e)) = node.node.as_ref() else {
            return None;
        };
        let op_is_eq = matches!(e.name.as_slice(), [one]
            if matches!(one.node.as_ref(), Some(N::String(s)) if s.sval == "="));
        if e.kind != pg_query::protobuf::AExprKind::AexprOp as i32 || !op_is_eq {
            return None;
        }
        Some((
            key(param(e.lexpr.as_deref())?)?,
            key(param(e.rexpr.as_deref())?)?,
        ))
    };
    // The residual is the one equality, or an AND of one per pair (RP-374).
    let conjuncts: Vec<Option<(&str, &str)>> = match expr.node.as_ref() {
        Some(N::BoolExpr(b)) if b.boolop == pg_query::protobuf::BoolExprType::AndExpr as i32 => {
            b.args.iter().map(equality).collect()
        }
        _ => vec![equality(expr)],
    };
    if conjuncts.iter().filter(|c| c.is_none()).count() != extra {
        return false;
    }
    let found: Vec<(&str, &str)> = conjuncts.into_iter().flatten().collect();
    let mut keys: Vec<&str> = pairs
        .iter()
        .flat_map(|(l, r)| [l.as_str(), r.as_str()])
        .collect();
    keys.sort_unstable();
    keys.dedup();
    params.is_empty()
        && (fields.len() == keys.len() || (extra > 0 && fields.len() > keys.len()))
        && found.len() == pairs.len()
        && pairs.iter().all(|(l, r)| {
            found.iter().any(|&(x, y)| {
                (x, y) == (l.as_str(), r.as_str()) || (x, y) == (r.as_str(), l.as_str())
            })
        })
}

/// A joined rows' filter as its top-level conjuncts, each one joined-row
/// key against a value or an operator document (`{k: v}`, `{k: {$lt:
/// v}}`), flattening `$and`. Every key names one table, so each conjunct
/// can be pushed to that table's scan (RP-365). `None` for anything else
/// (an `$or`, an expression).
/// Is `e` one operator expression (no AND or OR) reading the columns of
/// two or more tables: a join clause, filtering no single scan?
fn joins_tables(e: &secantus_pgplan::ColumnExpr) -> bool {
    use secantus_pgplan::joins::SEP;
    let secantus_pgplan::ColumnExpr::Row { expr, fields, .. } = e else {
        return false;
    };
    if !matches!(expr.node, Some(pg_query::NodeEnum::AExpr(_))) {
        return false;
    }
    let mut aliases: Vec<&str> = Vec::new();
    for (key, _, _) in fields {
        let Some((alias, _)) = key.split_once(SEP) else {
            return false;
        };
        if !aliases.contains(&alias) {
            aliases.push(alias);
        }
    }
    aliases.len() >= 2
}

fn joined_conjuncts(filter: &Document) -> Option<Vec<(String, bson::Bson)>> {
    let mut out = Vec::new();
    for (k, v) in filter {
        if k == "$and" {
            let bson::Bson::Array(items) = v else {
                return None;
            };
            for item in items {
                let bson::Bson::Document(d) = item else {
                    return None;
                };
                out.extend(joined_conjuncts(d)?);
            }
        } else if k.starts_with('$') || !k.contains(secantus_pgplan::joins::SEP) {
            return None;
        } else {
            out.push((k.clone(), v.clone()));
        }
    }
    Some(out)
}

#[cfg(test)]
mod pushed_conjuncts_tests {
    use super::*;

    #[test]
    fn a_pushed_conjunct_is_found_by_the_values_bson_calls_equal() {
        // RP-388: the lookup that replaced a scan of the `$and` per
        // conjunct must answer as `==` did.
        let filter = bson::doc! { "$and": [
            { "y": 1 },
            { "y": { "$ne": 3 } },
            { "z": -0.0 },
            { "w": { "a": 1, "b": "x" } },
            { "y": 2, "z": 2 },
        ] };
        let pushed = PushedConjuncts::new(&filter);
        assert!(pushed.holds("y", &bson::Bson::Int32(1)));
        assert!(!pushed.holds("y", &bson::Bson::Int64(1)));
        assert!(!pushed.holds("z", &bson::Bson::Int32(1)));
        assert!(pushed.holds("y", &bson::Bson::Document(bson::doc! { "$ne": 3 })));
        assert!(!pushed.holds("y", &bson::Bson::Document(bson::doc! { "$ne": 4 })));
        // `==` calls 0.0 and -0.0 equal, and fields in any order.
        assert!(pushed.holds("z", &bson::Bson::Double(0.0)));
        assert!(pushed.holds("w", &bson::Bson::Document(bson::doc! { "b": "x", "a": 1 })));
        // A conjunct over two fields is not a single-field one.
        assert!(!pushed.holds("y", &bson::Bson::Int32(2)));
        // The filter itself, when it is the one conjunct.
        let one = bson::doc! { "y": 5 };
        assert!(PushedConjuncts::new(&one).holds("y", &bson::Bson::Int32(5)));
        assert!(!PushedConjuncts::new(&one).holds("y", &bson::Bson::Int32(6)));
    }
}

#[cfg(test)]
mod key_bound_tests {
    use super::bounds_only;
    use bson::doc;

    #[test]
    fn a_range_bounds_its_key_and_an_equality_does_not() {
        // A range on the key, alone or beside another column's filter,
        // restricts its scan and keeps the join clause (RP-403).
        assert!(bounds_only(&doc! { "a": { "$gt": 5 } }, "a"));
        assert!(bounds_only(
            &doc! { "$and": [{ "a": { "$gte": 3, "$lt": 20 } }, { "b": "v1" }] },
            "a"
        ));
        // An equality joins the key's equivalence class (RP-363).
        assert!(!bounds_only(&doc! { "a": 5 }, "a"));
        assert!(!bounds_only(&doc! { "a": { "$eq": 5 } }, "a"));
        assert!(!bounds_only(&doc! { "a": { "$in": [1, 2] } }, "a"));
        // The key under an OR is not a bound of the scan.
        assert!(!bounds_only(
            &doc! { "$or": [{ "a": { "$gt": 5 } }, { "b": "v1" }] },
            "a"
        ));
    }
}
