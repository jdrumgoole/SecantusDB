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

/// A scan's estimate, and the index it reads when the cheapest path is a
/// plain index scan (`None` for a sequential scan).
pub(crate) type ScanEstimate = (Estimate, Option<String>);

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
    let typlen = secantus_pgplan::pgtypes::oid_of_name(pg_type)
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
/// (`cost_resultscan`: `cpu_tuple_cost` for its row, no startup), its width
/// the select list's (`set_pathtarget_cost_width`: each expression's type's
/// `get_typavgwidth`). A constant WHERE is a one-time filter that leaves
/// the row estimate at one. Only a select list of constants is priced:
/// any other expression adds its own evaluation cost (`cost_qual_eval`),
/// which is not modelled here, and a FROM function is a Function Scan.
pub(crate) fn result_estimate(sc: &secantus_pgplan::SelectConstant) -> Option<Estimate> {
    if sc.source.is_some() {
        return None;
    }
    let mut width = 0;
    for (_, col, pg_type, typmod) in &sc.columns {
        if !matches!(col, secantus_pgplan::ConstCol::Value(_)) {
            return None;
        }
        width += type_width(pg_type, *typmod);
    }
    Some((0.0, costsize::CPU_TUPLE_COST, 1.0, width))
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
    /// cheaper (`set_cheapest`). Not priced: a range on an indexed column
    /// (`get_actual_variable_range` reads the index), a clause on an
    /// index's later column, an index holding every column read (an
    /// index-only scan), an `OR` over indexed columns (a BitmapOr), and a
    /// bitmap heap scan winning (its node is not built).
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
        let (relpages, reltuples, _) = catalog_meta::initdb_class_size(oid)?;
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
            let covering = fields
                .iter()
                .chain(named.iter())
                .all(|f| ix.columns.contains(f));
            if later || ranged || covering || !w.bitmap_ors.is_empty() {
                return None;
            }
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
                    visible: 0.0,
                    tuples: None,
                    predicate: None,
                    hash: false,
                },
            ));
        }
        // Two indexes' bitmap paths may be ANDed (`choose_bitmap_and`),
        // which is not costed here.
        if indexes.len() > 1 {
            return None;
        }
        let paths: Vec<costsize::Index> = indexes.iter().map(|(_, ix)| ix.clone()).collect();
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
            costsize::ScanKind::Index(i) => Some(indexes.get(i)?.0.clone()),
            // A bitmap heap scan, whose node is not built here.
            costsize::ScanKind::Bitmap(_) => return None,
        };
        Some(((startup, total, rows, width), index))
    }
}

/// `clamp_row_est`: a row count rounded, and never below one.
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
