//! Which input of an INTERSECT a group's row comes from (RP-692).
//!
//! Two rows of one group can differ in what a client sees -- `'xy '::bpchar`
//! and `'xy'::text` are one group of a `bpchar` column, `1.0` and `1.000`
//! one group of a `numeric` one -- so the row an INTERSECT or EXCEPT returns
//! for a group is part of its answer. PostgreSQL 15.19's SetOp returns, for
//! each group, the FIRST row its first input gave (`nodeSetOp.c`: the hash
//! entry is made from the first input's tuple, and a sorted SetOp's group
//! starts with it), repeated as many times as the group is output: `select
//! x from (values (1.0), (1.00)) v(x) intersect all select 1 ...` is `1.0`
//! twice.
//!
//! Which input is first is the planner's choice (`generate_nonunion_paths`,
//! `prepunion.c`): an EXCEPT's left one always; an INTERSECT's, the one
//! estimated to have FEWER groups. The Append lists that input first. A
//! HashSetOp builds its table from it alone, so a group's row is its first
//! row there; a sorted SetOp (`choose_hashed_setop` prices the two) sorts
//! the Append's rows on the columns only, the flag being no sort key, so a
//! group's row is the one PostgreSQL's quicksort leaves first among its
//! ties (`tuplesort`), and the groups come out in that order.
//!
//! A HashSetOp returns its groups in its hash table's order
//! (`setop_retrieve_hash_table`), each group's rows together: the table is
//! the one a HashAggregate builds (`hashagg::hash_agg_order`), started at
//! the plan's group estimate, which only the first input's rows insert
//! into -- the second input's look a group up and add none (RP-842).
//!
//! The estimates are PostgreSQL's: a leaf's groups are `estimate_num_groups`
//! over its select list on its own row estimate, and a nested set
//! operation's are its rows, as are a DISTINCT leaf's. An input this
//! server does not estimate -- a join with a WHERE, a WHERE the
//! selectivity model does not cover, a FROM subquery other than VALUES
//! or a plain select over one user table, a catalog -- leaves the left input first, as before (RP-841).

use super::*;
use pgwire::api::results::FieldInfo;
use secantus_pgplan::{costsize, hashagg};

/// `random_page_cost` and `seq_page_cost` at their defaults, as the other
/// estimates here take them.
const RANDOM_PAGE_COST: f64 = 4.0;
const SEQ_PAGE_COST: f64 = 1.0;
const BLCKSZ: f64 = 8192.0;
/// `MAXALIGN(SizeofMinimalTupleHeader)` and `MAXALIGN(SizeofHeapTupleHeader)`.
const MINIMAL_TUPLE_HEADER: f64 = 16.0;
const HEAP_TUPLE_HEADER: f64 = 24.0;
/// `sizeof(TupleHashEntryData)` and the aset `CHUNKHDRSZ` of `nodeAgg.c`.
const TUPLE_HASH_ENTRY: f64 = 24.0;
const CHUNKHDRSZ: f64 = 16.0;
const HASHAGG_PARTITION_FACTOR: f64 = 1.50;
const HASHAGG_MIN_PARTITIONS: f64 = 4.0;
const HASHAGG_MAX_PARTITIONS: f64 = 1024.0;
const HASHAGG_READ_BUFFER_SIZE: f64 = BLCKSZ;
const HASHAGG_WRITE_BUFFER_SIZE: f64 = BLCKSZ;

fn maxalign(n: f64) -> f64 {
    (n / 8.0).ceil() * 8.0
}

/// An input's rows and groups, as `recurse_set_operations` estimates them.
#[derive(Debug, Clone, Copy, PartialEq)]
struct InputEstimate {
    rows: f64,
    groups: f64,
}

/// How PostgreSQL 15.19 runs an INTERSECT or EXCEPT: which input its
/// Append lists first, and whether its SetOp hashes or sorts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SetOpPlan {
    /// The right input is listed first (an INTERSECT whose left input has
    /// more groups).
    pub(crate) right_first: bool,
    /// The SetOp sorts the Append's rows on the columns and walks the
    /// groups in that order; otherwise it hashes the first input's.
    pub(crate) sorted: bool,
    /// The buckets a HashSetOp's table starts with: the plan's `numGroups`,
    /// truncated to a `long` (`create_setop_plan`), and no more than hash
    /// memory holds of `TupleHashEntryData` (`BuildTupleHashTableExt`).
    /// `None` when the inputs are not estimated.
    pub(crate) nbuckets: Option<u64>,
}

impl PgHandler {
    /// The plan `generate_nonunion_paths` makes for `set`, an INTERSECT or
    /// EXCEPT whose output columns are `schema` (see the module doc). An
    /// input this server does not estimate leaves the written order,
    /// hashed.
    pub(crate) fn setop_plan(
        &self,
        set: &secantus_pgplan::SetOpSelect,
        schema: &[FieldInfo],
    ) -> SetOpPlan {
        let written = SetOpPlan {
            right_first: false,
            sorted: false,
            nbuckets: None,
        };
        let (Some(left), Some(right)) = (
            self.setop_input_estimate(&set.left),
            self.setop_input_estimate(&set.right),
        ) else {
            return written;
        };
        let except = matches!(set.kind, secantus_pgplan::SetOpKind::Except);
        // An EXCEPT's left input is first; an INTERSECT's, the one with
        // fewer groups, `dLeftGroups <= dRightGroups` keeping the written
        // order.
        let right_first = !except && left.groups > right.groups;
        let (groups, output) = if except {
            (left.groups, if set.all { left.rows } else { left.groups })
        } else {
            let groups = left.groups.min(right.groups);
            (
                groups,
                if set.all {
                    left.rows.min(right.rows)
                } else {
                    groups
                },
            )
        };
        let hashed = self
            .chooses_hashed_setop(schema, left.rows + right.rows, groups, output, set)
            .unwrap_or(true);
        let hash_mem = self.hash_mem_limit(self.kb_setting("work_mem", 4096));
        let nbuckets = (groups as u64)
            .min((hash_mem / TUPLE_HASH_ENTRY) as u64)
            .max(1);
        SetOpPlan {
            right_first,
            sorted: !hashed,
            nbuckets: Some(nbuckets),
        }
    }

    /// `recurse_set_operations`' `pNumGroups` of one input, with its rows;
    /// `None` where this server does not estimate the input.
    fn setop_input_estimate(&self, stmt: &secantus_pgplan::Statement) -> Option<InputEstimate> {
        use secantus_pgplan::{SetOpKind, Statement};
        match stmt {
            // A nested set operation's groups are its rows (`*pNumGroups =
            // rel->rows`). A UNION's rows are its inputs' together, with or
            // without ALL (`make_union_unique` takes the Append's rows as
            // its groups); an INTERSECT's or EXCEPT's are
            // `generate_nonunion_paths`' `dNumOutputRows`.
            Statement::SetOp(inner) => {
                if inner.limit.is_some() || inner.offset != 0 {
                    return None;
                }
                let l = self.setop_input_estimate(&inner.left)?;
                let r = self.setop_input_estimate(&inner.right)?;
                let rows = match inner.kind {
                    SetOpKind::Union => l.rows + r.rows,
                    SetOpKind::Intersect => {
                        if inner.all {
                            l.rows.min(r.rows)
                        } else {
                            l.groups.min(r.groups)
                        }
                    }
                    SetOpKind::Except => {
                        if inner.all {
                            l.rows
                        } else {
                            l.groups
                        }
                    }
                };
                Some(InputEstimate { rows, groups: rows })
            }
            // A select with no FROM is one row, and one group: a constant
            // or a call over no Var is not a grouping column
            // (`estimate_num_groups`).
            Statement::SelectConstant(sc) if sc.source.is_none() => Some(InputEstimate {
                rows: 1.0,
                groups: 1.0,
            }),
            // A set-returning function's rows are not a VALUES list
            // (`typmods` is `None`): PostgreSQL estimates those otherwise.
            Statement::ValuesConstant(vc) => {
                vc.typmods.as_ref()?;
                Some(values_groups(&vc.types, vc.rows.len() as f64))
            }
            Statement::Select(sel) => self.select_input_estimate(sel),
            // An aggregated input's groups are its rows (`*pNumGroups =
            // subpath->rows` under `hasAggs` or a GROUP BY): a plain
            // aggregate's one row, or `estimate_num_groups` of the
            // GROUP BY's columns over one user table's scan.
            Statement::Aggregate(agg) => {
                if agg.sub.is_some()
                    || agg.join.is_some()
                    || agg.series.is_some()
                    || agg.grouping_sets.is_some()
                    || agg.limit.is_some()
                    || agg.offset != 0
                    || agg.distinct
                    || agg.group_by.iter().any(|k| k.expr.is_some())
                {
                    return None;
                }
                let rows = if agg.group_by.is_empty() {
                    1.0
                } else {
                    let fields: Vec<String> =
                        agg.group_by.iter().map(|k| k.field.clone()).collect();
                    self.distinct_groups(&agg.table, &agg.filter, &fields)?
                };
                // `cost_agg` scales the groups by the HAVING's selectivity.
                let rows = match &agg.having {
                    Some(h) => costsize::clamp_row_est(rows * having_selectivity(h, false)?),
                    None => rows,
                };
                Some(InputEstimate { rows, groups: rows })
            }
            _ => None,
        }
    }

    /// A leaf SELECT's estimate: over one user table's stored columns, or
    /// over a FROM VALUES list, with nothing above the scan that changes
    /// its rows or groups.
    fn select_input_estimate(&self, sel: &secantus_pgplan::Select) -> Option<InputEstimate> {
        // A DISTINCT input's groups are its rows (`*pNumGroups =
        // subpath->rows` under a `distinctClause`): the Unique's or
        // HashAggregate's `estimate_num_groups` over the select list, which
        // is the leaf's own group estimate.
        if matches!(sel.distinct, secantus_pgplan::Distinct::All) {
            let mut plain = sel.clone();
            plain.distinct = secantus_pgplan::Distinct::None;
            let e = self.select_input_estimate(&plain)?;
            return Some(InputEstimate {
                rows: e.groups,
                groups: e.groups,
            });
        }
        // A comma join's residual is its re-checked key equality, which
        // `join_input_estimate` reads itself.
        let comma_join = sel.join.is_none() && sel.sub.is_some();
        if (sel.residual.is_some() && !comma_join)
            || sel.series.is_some()
            || !sel.windows.is_empty()
            || sel.limit.is_some()
            || sel.offset != 0
            || sel.with_ties
            || !matches!(sel.distinct, secantus_pgplan::Distinct::None)
            || sel.casts.iter().any(Option::is_some)
        {
            return None;
        }
        let fields: Vec<String> = sel.columns.iter().map(|(_, f)| f.clone()).collect();
        if let Some((rows, groups)) = self.join_input_estimate(sel) {
            return Some(InputEstimate { rows, groups });
        }
        if sel.residual.is_some() {
            return None;
        }
        if let Some(sub) = &sel.sub {
            // `(VALUES ...) v(x)` is pulled up into the leaf as its
            // RTE_VALUES, whose Vars `get_variable_numdistinct` takes as
            // unique; a set-returning function's rows are not a VALUES
            // list (`typmods` is `None`).
            //
            // A plain subquery over a user table is pulled up
            // (`pull_up_simple_subquery`): the leaf is the inner select,
            // reading the stored columns the outer one names.
            let vc = match sub.plan.as_ref() {
                secantus_pgplan::Statement::ValuesConstant(vc) => vc,
                secantus_pgplan::Statement::Select(inner) => {
                    if !sel.filter.is_empty() || inner.sub.is_some() || inner.lock.is_some() {
                        return None;
                    }
                    let columns = fields
                        .iter()
                        .map(|f| {
                            let i = sub.def.columns.iter().position(|c| &c.field() == f)?;
                            if inner.casts.get(i).is_some_and(Option::is_some) {
                                return None;
                            }
                            inner.columns.get(i).cloned()
                        })
                        .collect::<Option<Vec<_>>>()?;
                    let mut pulled = inner.clone();
                    pulled.casts = vec![None; columns.len()];
                    pulled.sort_input = vec![false; columns.len()];
                    pulled.columns = columns;
                    pulled.order.clear();
                    return self.select_input_estimate(&pulled);
                }
                _ => return None,
            };
            vc.typmods.as_ref()?;
            if !sel.filter.is_empty() {
                return None;
            }
            let types = fields
                .iter()
                .map(|f| {
                    let i = sub.def.columns.iter().position(|c| &c.field() == f)?;
                    vc.types.get(i).cloned()
                })
                .collect::<Option<Vec<_>>>()?;
            return Some(values_groups(&types, vc.rows.len() as f64));
        }
        let (_, _, rows, _) = self.user_seq_scan(&sel.table, &sel.filter, &[])?;
        let groups = self.distinct_groups(&sel.table, &sel.filter, &fields)?;
        Some(InputEstimate { rows, groups })
    }

    /// `choose_hashed_setop` (`prepunion.c`): whether the SetOp hashes,
    /// over an Append of `input_rows` rows whose result has `groups`
    /// groups and `output` rows. `None` where a column's type has neither
    /// a hash nor a B-tree default operator class this server can name.
    fn chooses_hashed_setop(
        &self,
        schema: &[FieldInfo],
        input_rows: f64,
        groups: f64,
        output: f64,
        set: &secantus_pgplan::SetOpSelect,
    ) -> Option<bool> {
        let (mut can_hash, mut can_sort) = (true, true);
        for f in schema {
            let (hash, sort) = default_opclasses(f.datatype())?;
            can_hash &= hash;
            can_sort &= sort;
        }
        match (can_hash, can_sort) {
            (true, true) => {}
            (true, false) => return Some(true),
            (false, true) => return Some(false),
            (false, false) => return None,
        }
        if self.setting_is("enable_hashagg", "off") {
            return Some(false);
        }
        // The Append's target list: each column at its type's average
        // width under the result's typmod, plus the int4 flag column
        // (`generate_append_tlist`, `set_pathtarget_cost_width`).
        let width = schema
            .iter()
            .map(|f| scan_estimate::type_width(f.datatype().name(), f.type_modifier()))
            .sum::<i64>()
            + 4;
        let work_mem = self.kb_setting("work_mem", 4096);
        let hash_mem = self.hash_mem_limit(work_mem);
        let hashentrysize = maxalign(width as f64) + MINIMAL_TUPLE_HEADER;
        if hashentrysize * groups > hash_mem {
            return Some(false);
        }
        let ncols = schema.len() as f64;
        // The input's own cost is common to both and cancels; start at 0.
        let hashed = hashed_agg_cost(input_rows, width, groups, ncols, hash_mem);
        let (sort_startup, sort_total, _, _) =
            scan_estimate::sort_estimate((0.0, 0.0, input_rows, width), work_mem);
        // `cost_group`: one comparison per column of each input row.
        let sorted = (
            sort_startup,
            sort_total + costsize::CPU_OPERATOR_COST * input_rows * ncols,
        );
        // The top-level tuple fraction: a LIMIT's count (and offset) over
        // the output rows, none under an ORDER BY (`grouping_planner`).
        let mut fraction = 0.0;
        if set.order.is_empty() {
            if let Some(count) = set.limit {
                fraction = (count as f64 + set.offset as f64).max(0.0);
            }
        }
        if fraction >= 1.0 {
            fraction /= output;
        }
        Some(compare_fractional_path_costs(hashed, sorted, fraction) == Ordering::Less)
    }

    /// `get_hash_memory_limit`: `work_mem` x `hash_mem_multiplier`, in
    /// bytes, truncated to a `Size`.
    fn hash_mem_limit(&self, work_mem_kb: usize) -> f64 {
        let multiplier = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get("hash_mem_multiplier")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(2.0);
        (work_mem_kb as f64 * multiplier * 1024.0).floor()
    }
}

/// `clauselist_selectivity` of a HAVING over aggregates, `negated` under
/// a NOT, which `negate_clause` pushes down (`NOT max(x) > 0` is `max(x)
/// <= 0`). An aggregate or an expression over the groups has no
/// statistics: a comparison with a constant is `DEFAULT_INEQ_SEL` for an
/// inequality, `1 / DEFAULT_NUM_DISTINCT` for `=` (`var_eq_const`) and its
/// complement for `<>`, a null test `DEFAULT_UNK_SEL` or its complement.
/// `None` for a grouping column's test, which `subquery_planner` moves to
/// the WHERE, a comparison with NULL, or an operator not listed.
fn having_selectivity(h: &secantus_pgplan::Having, negated: bool) -> Option<f64> {
    use secantus_pgplan::{Having, OutputCol};
    const DEFAULT_INEQ_SEL: f64 = 1.0 / 3.0;
    const DEFAULT_EQ_SEL: f64 = 1.0 / 200.0;
    const DEFAULT_UNK_SEL: f64 = 0.005;
    let over_groups = |c: &OutputCol| matches!(c, OutputCol::Agg(_) | OutputCol::Expr(_));
    Some(match h {
        Having::Compare { subject, op, value } => {
            if !over_groups(subject) || matches!(value, Bson::Null) {
                return None;
            }
            match (op.as_str(), negated) {
                ("<" | "<=" | ">" | ">=", _) => DEFAULT_INEQ_SEL,
                ("=", false) | ("<>", true) => DEFAULT_EQ_SEL,
                ("<>", false) | ("=", true) => 1.0 - DEFAULT_EQ_SEL,
                _ => return None,
            }
        }
        Having::IsNull {
            subject,
            negated: n,
        } => {
            if !over_groups(subject) {
                return None;
            }
            if *n != negated {
                1.0 - DEFAULT_UNK_SEL
            } else {
                DEFAULT_UNK_SEL
            }
        }
        Having::Not(inner) => having_selectivity(inner, !negated)?,
        // `clauselist_selectivity`'s product; an OR is
        // `clause_selectivity`'s `s1 + s2 - s1 * s2`, term by term.
        Having::And(terms) | Having::Or(terms) => {
            let and = matches!(h, Having::And(_)) != negated;
            // Two inequalities over one subject are a range pair
            // (`addRangeClause`), not modelled.
            if and {
                let mut seen = Vec::new();
                for t in terms {
                    if let Having::Compare { subject, op, .. } = t {
                        if matches!(op.as_str(), "<" | "<=" | ">" | ">=") {
                            if seen.contains(&subject) {
                                return None;
                            }
                            seen.push(subject);
                        }
                    }
                }
            }
            let mut sel = if and { 1.0 } else { 0.0 };
            for t in terms {
                let s2 = having_selectivity(t, negated)?;
                sel = if and { sel * s2 } else { sel + s2 - sel * s2 };
            }
            sel
        }
    })
}

/// The groups of a VALUES list's columns of `types` over `rows` rows:
/// `get_variable_numdistinct` takes a VALUES column as unique, a boolean
/// as two values, and `estimate_num_groups` clamps their product to the
/// rows.
fn values_groups(types: &[String], rows: f64) -> InputEstimate {
    let keys: Vec<hashagg::KeyStat> = types
        .iter()
        .map(|t| {
            if t == "bool" || t == "boolean" {
                hashagg::KeyStat::Bool
            } else {
                hashagg::KeyStat::Unique
            }
        })
        .collect();
    InputEstimate {
        rows,
        groups: hashagg::estimate_groups(&keys, rows, rows) as f64,
    }
}

/// Whether `ty` has a default hash and a default B-tree operator class
/// (`grouping_is_hashable`, `grouping_is_sortable`), from PostgreSQL 15's
/// `pg_opclass`. An array is its element's (`array_ops` needs the
/// element's own); a type of a binary-coercible input type is that type's
/// (`varchar` is `text_ops`', a `reg*` type `oid_ops`', `cidr` `inet_ops`').
/// A type made in the database -- an enum, a range, a composite -- has
/// `anyenum`'s, `anyrange`'s or `record`'s, which are both.
fn default_opclasses(ty: &Type) -> Option<(bool, bool)> {
    if let postgres_types::Kind::Array(elem) = ty.kind() {
        return default_opclasses(elem);
    }
    let oid = i64::from(ty.oid());
    if oid >= 16384 {
        return Some((true, true));
    }
    let oid = match oid {
        1043 => 25,
        650 => 869,
        24 | 2202 | 2203 | 2204 | 2205 | 2206 | 3734 | 3769 | 4089 | 4096 | 4191 => 26,
        o => o,
    };
    static CLASSES: OnceLock<Vec<(i64, i64)>> = OnceLock::new();
    let classes = CLASSES.get_or_init(|| {
        system_catalogs::builtin_opclasses()
            .into_iter()
            .filter(|c| c.default)
            .map(|c| (c.method, c.intype))
            .collect()
    });
    const BTREE: i64 = 403;
    const HASH: i64 = 405;
    let has = |method: i64| classes.iter().any(|&(m, t)| m == method && t == oid);
    let (hash, sort) = (has(HASH), has(BTREE));
    (hash || sort).then_some((hash, sort))
}

/// `cost_agg` with `AGG_HASHED`, no aggregates and an input costing
/// nothing: `(startup, total)`, with the disk and CPU cost of the batches
/// it spills when its groups do not fit `hash_mem` (PostgreSQL 15).
fn hashed_agg_cost(
    input_rows: f64,
    width: i64,
    groups: f64,
    ncols: f64,
    hash_mem: f64,
) -> (f64, f64) {
    let mut startup = costsize::CPU_OPERATOR_COST * ncols * input_rows;
    let mut total = startup + costsize::CPU_TUPLE_COST * groups;
    // `hash_agg_entry_size(0, width, 0)`: the entry and its tuple's chunk.
    let entry = TUPLE_HASH_ENTRY + CHUNKHDRSZ + MINIMAL_TUPLE_HEADER + width as f64;
    let (mem_limit, ngroups_limit, partitions) = hash_agg_set_limits(entry, groups, hash_mem);
    let nbatches = (groups * entry / mem_limit)
        .max(groups / ngroups_limit)
        .ceil()
        .max(1.0);
    let partitions = partitions.max(2.0);
    let depth = (nbatches.ln() / partitions.ln()).ceil();
    // `relation_byte_size`, over BLCKSZ; each level writes and reads every
    // tuple, at twice the cost a Sort pays.
    let pages = input_rows * (maxalign(width as f64) + HEAP_TUPLE_HEADER) / BLCKSZ;
    let written = pages * depth * 2.0;
    let read = pages * depth * 2.0;
    startup += written * RANDOM_PAGE_COST;
    total += written * RANDOM_PAGE_COST + read * SEQ_PAGE_COST;
    let spill = depth * input_rows * 2.0 * costsize::CPU_TUPLE_COST;
    (startup + spill, total + spill)
}

/// `hash_agg_set_limits` (`nodeAgg.c`): the memory a hashed aggregate may
/// fill, the groups that fit it, and the partitions it spills into (0
/// when it is not expected to spill).
fn hash_agg_set_limits(entry: f64, groups: f64, hash_mem: f64) -> (f64, f64, f64) {
    if groups * entry <= hash_mem {
        return (hash_mem, (hash_mem / entry).floor(), 0.0);
    }
    let partitions = hash_choose_num_partitions(groups, entry, hash_mem);
    let partition_mem = HASHAGG_READ_BUFFER_SIZE + HASHAGG_WRITE_BUFFER_SIZE * partitions;
    let mem_limit = if hash_mem > 4.0 * partition_mem {
        hash_mem - partition_mem
    } else {
        (hash_mem * 0.75).floor()
    };
    let ngroups_limit = if mem_limit > entry {
        (mem_limit / entry).floor()
    } else {
        1.0
    };
    (mem_limit, ngroups_limit, partitions)
}

/// `hash_choose_num_partitions` with no hash bits used: a power of two.
fn hash_choose_num_partitions(groups: f64, entry: f64, hash_mem: f64) -> f64 {
    let partition_limit = (hash_mem * 0.25 - HASHAGG_READ_BUFFER_SIZE) / HASHAGG_WRITE_BUFFER_SIZE;
    let wanted = HASHAGG_PARTITION_FACTOR * groups * entry;
    let dpartitions = (1.0 + wanted / hash_mem)
        .min(partition_limit)
        .clamp(HASHAGG_MIN_PARTITIONS, HASHAGG_MAX_PARTITIONS);
    // `my_log2`: ceil(log2(n)) of the truncated count.
    let n = dpartitions.trunc() as u64;
    let bits = if n <= 1 {
        0
    } else {
        64 - (n - 1).leading_zeros()
    };
    f64::from(1u32 << bits.min(31))
}

/// `compare_fractional_path_costs` of two `(startup, total)` costs: by
/// total, then startup, unless `fraction` is a fraction of the rows.
fn compare_fractional_path_costs(a: (f64, f64), b: (f64, f64), fraction: f64) -> Ordering {
    if fraction <= 0.0 || fraction >= 1.0 {
        return a
            .1
            .partial_cmp(&b.1)
            .unwrap_or(Ordering::Equal)
            .then(a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
    }
    let ca = a.0 + fraction * (a.1 - a.0);
    let cb = b.0 + fraction * (b.1 - b.0);
    ca.partial_cmp(&cb).unwrap_or(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A HAVING over 300 groups scaled as PostgreSQL 15.19's HashAggregate
    /// estimate shows it (`group by k having ...` over 3,000 analysed rows).
    #[test]
    fn having_selectivity_is_postgres() {
        use secantus_pgplan::{Having, OutputCol};
        let cmp = |op: &str, slot: usize| Having::Compare {
            subject: OutputCol::Agg(slot),
            op: op.into(),
            value: Bson::Int32(5),
        };
        let null = |negated| Having::IsNull {
            subject: OutputCol::Agg(0),
            negated,
        };
        let rows =
            |h: &Having| having_selectivity(h, false).map(|s| costsize::clamp_row_est(300.0 * s));
        let sel = |h: &Having| having_selectivity(h, false).unwrap();
        let close = |a: f64, b: f64| (a - b).abs() < 1e-12;
        assert_eq!(rows(&cmp(">", 0)), Some(100.0));
        // 300 x 0.005 = 1.5 is 2 and 300 x 0.995 = 298.5 is 298 there, by
        // `rint` (RP-1023), so these compare the selectivities.
        assert!(close(sel(&cmp("=", 0)), 0.005));
        assert!(close(sel(&cmp("<>", 0)), 0.995));
        assert!(close(sel(&null(false)), 0.005));
        assert!(close(sel(&null(true)), 0.995));
        assert_eq!(
            rows(&Having::And(vec![cmp(">", 0), cmp("<", 1)])),
            Some(33.0)
        );
        assert_eq!(
            rows(&Having::Or(vec![cmp(">", 0), cmp("<", 1)])),
            Some(167.0)
        );
        // `NOT max(x) > 0` is `max(x) <= 0`.
        assert_eq!(rows(&Having::Not(Box::new(cmp(">", 0)))), Some(100.0));
        // A range pair over one aggregate is not modelled.
        assert_eq!(rows(&Having::And(vec![cmp(">", 0), cmp("<", 0)])), None);
        // A grouping column's test moves to the WHERE.
        let group = Having::Compare {
            subject: OutputCol::Group(0),
            op: ">".into(),
            value: Bson::Int32(5),
        };
        assert_eq!(rows(&group), None);
    }

    #[test]
    fn a_few_groups_hash_without_spilling() {
        // 1851 rows into one group of width 36: no spill, so
        // cpu_operator_cost per row and cpu_tuple_cost for the group.
        let (s, t) = hashed_agg_cost(1851.0, 36, 1.0, 1.0, 8_388_608.0);
        assert!((s - 4.6275).abs() < 1e-9, "{s}");
        assert!((t - 4.6375).abs() < 1e-9, "{t}");
    }

    #[test]
    fn groups_past_hash_mem_spill_in_partitions() {
        // 200,000 groups of 92 bytes each exceed 8 MB of hash_mem: the
        // partitions are a power of two, and the spill is priced.
        let entry = 24.0 + 16.0 + 16.0 + 36.0;
        let (mem, ngroups, parts) = hash_agg_set_limits(entry, 200_000.0, 8_388_608.0);
        assert_eq!(parts, 4.0);
        assert_eq!(mem, 8_388_608.0 - 8192.0 * 5.0);
        assert_eq!(ngroups, (mem / entry).floor());
        let (_, spilled) = hashed_agg_cost(200_000.0, 36, 200_000.0, 1.0, 8_388_608.0);
        let (_, fits) = hashed_agg_cost(200_000.0, 36, 200_000.0, 1.0, 1e12);
        assert!(spilled > fits);
    }

    #[test]
    fn a_values_column_is_unique_and_a_boolean_two() {
        let g = values_groups(&["numeric".into()], 3.0);
        assert_eq!((g.rows, g.groups), (3.0, 3.0));
        let g = values_groups(&["bool".into()], 5.0);
        assert_eq!(g.groups, 2.0);
    }

    #[test]
    fn money_and_bit_sort_but_do_not_hash() {
        assert_eq!(default_opclasses(&Type::MONEY), Some((false, true)));
        assert_eq!(default_opclasses(&Type::BIT), Some((false, true)));
        assert_eq!(default_opclasses(&Type::XID), Some((true, false)));
        assert_eq!(default_opclasses(&Type::VARCHAR), Some((true, true)));
        assert_eq!(default_opclasses(&Type::NUMERIC_ARRAY), Some((true, true)));
        assert_eq!(default_opclasses(&Type::MONEY_ARRAY), Some((false, true)));
    }

    #[test]
    fn a_fraction_compares_the_cost_of_that_share() {
        let hashed = (10.0, 11.0);
        let sorted = (1.0, 20.0);
        assert_eq!(
            compare_fractional_path_costs(hashed, sorted, 0.0),
            Ordering::Less
        );
        assert_eq!(
            compare_fractional_path_costs(hashed, sorted, 0.1),
            Ordering::Greater
        );
    }
}
