//! EXPLAIN ANALYZE's counts below a join (RP-386): each child's `actual
//! rows` and `loops`, a parameterized inner index-only scan's `Heap
//! Fetches`, a nested loop's `Rows Removed by Join Filter` and a Hash
//! node's `Buckets / Batches / Memory Usage`, as PostgreSQL 15.19's
//! instrumentation counts them while it runs the plan.
//!
//! This server does not run the plan node by node, so each count is
//! derived from the rows each scan reads, and checked against the rows
//! the statement produced: a plan this cannot account for exactly keeps
//! its children unannotated, as before, rather than print a guess.

use super::*;
use crate::explain::{HashInfo, PlanNode};

/// `HJTUPLE_OVERHEAD`: `MAXALIGN(sizeof(HashJoinTupleData))`.
const HJTUPLE_OVERHEAD: usize = 16;
/// `MAXALIGN(SizeofMinimalTupleHeader)`.
const MINIMAL_HEADER: usize = 16;
/// `sizeof(HashJoinTuple)`, a bucket's pointer.
const BUCKET_POINTER: usize = 8;
/// `MaxAllocSize`.
const MAX_ALLOC: usize = 0x3fff_ffff;

/// A plain scan leaf of the plan: its table and the alias its join names.
fn leaf_relation(n: &PlanNode) -> Option<(String, String)> {
    if !n.children.is_empty() {
        return None;
    }
    let prop = |k: &str| n.props.iter().find(|(p, _)| p == k).map(|(_, v)| v.clone());
    let table = prop("Relation Name")?;
    let alias = prop("Alias").unwrap_or_else(|| table.clone());
    Some((table, alias))
}

/// A Seq Scan leaf with no quals: it returns every row of its table.
fn bare_seq_scan(n: &PlanNode) -> Option<(String, String)> {
    if !n.name.starts_with("Seq Scan on ") || !n.details.is_empty() {
        return None;
    }
    leaf_relation(n)
}

/// A Seq Scan leaf with no quals, or with only a Filter (RP-390).
fn filtered_seq_scan(n: &PlanNode) -> Option<(String, String)> {
    match n.details.as_slice() {
        [] => bare_seq_scan(n),
        [(label, _)] if label == "Filter" && n.name.starts_with("Seq Scan on ") => leaf_relation(n),
        _ => None,
    }
}

/// A join key's value as a hash table compares it; `None` for NULL, and
/// for a type this does not compare.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Key {
    Int(i64),
    Float(u64),
    Text(String),
}

fn key_of(v: Option<&Bson>) -> Option<Option<Key>> {
    Some(match v {
        None | Some(Bson::Null) => None,
        Some(Bson::Int32(n)) => Some(Key::Int(i64::from(*n))),
        Some(Bson::Int64(n)) => Some(Key::Int(*n)),
        Some(Bson::Double(d)) if d.fract() == 0.0 && d.abs() < 9.0e15 => Some(Key::Int(*d as i64)),
        Some(Bson::Double(d)) if !d.is_nan() => Some(Key::Float((*d + 0.0).to_bits())),
        Some(Bson::String(s)) => Some(Key::Text(s.clone())),
        Some(_) => return None,
    })
}

/// A number an index condition compares; `None` for NULL or a non-number.
fn number(v: Option<&Bson>) -> Option<f64> {
    match v? {
        Bson::Int32(n) => Some(f64::from(*n)),
        Bson::Int64(n) if n.unsigned_abs() < (1 << 53) => Some(*n as f64),
        Bson::Double(d) if !d.is_nan() => Some(*d),
        _ => None,
    }
}

/// `%.0f` of `total / loops`, as `ExplainNode` prints a rescanned node's
/// rows.
fn per_loop(total: usize, loops: usize) -> usize {
    let avg = total as f64 / loops as f64;
    format!("{avg:.0}").parse().unwrap_or(0)
}

/// `ExecChooseHashTableSize`'s bucket count for a single-batch table of
/// `ntuples` rows `width` wide, `None` where it would take several
/// batches (not modelled).
fn initial_buckets(ntuples: f64, width: i64, hash_table_bytes: usize) -> Option<usize> {
    let ntuples = if ntuples <= 0.0 { 1000.0 } else { ntuples };
    let width = usize::try_from(width).ok()?;
    let tupsize = HJTUPLE_OVERHEAD + MINIMAL_HEADER + width.div_ceil(8) * 8;
    let inner_rel_bytes = ntuples * tupsize as f64;
    // The skew table's 2% (`SKEW_HASH_MEM_PERCENT`) comes off the budget
    // where a skew MCV fits in it.
    let skew_bytes = hash_table_bytes * 2 / 100;
    let skew_mcvs = skew_bytes / (tupsize + 8 * 8 + 4 + 16);
    let hash_table_bytes = if skew_mcvs > 0 {
        hash_table_bytes - skew_bytes
    } else {
        hash_table_bytes
    };
    let mut max_pointers = (hash_table_bytes / BUCKET_POINTER).min(MAX_ALLOC / BUCKET_POINTER);
    let pow2 = max_pointers.next_power_of_two();
    if max_pointers != pow2 {
        max_pointers = pow2 / 2;
    }
    max_pointers = max_pointers.min((i32::MAX as usize) / 2 + 1);
    let dbuckets = ntuples.ceil().min(max_pointers as f64) as usize;
    let nbuckets = dbuckets.max(1024).next_power_of_two();
    let bucket_bytes = BUCKET_POINTER * nbuckets;
    if inner_rel_bytes + bucket_bytes as f64 > hash_table_bytes as f64 {
        return None;
    }
    Some(nbuckets)
}

/// The hash table `ExecHashTableInsert` builds from tuples of these
/// `t_len`s, then `MultiExecPrivateHash`'s bucket array: `None` where it
/// would have to add batches.
fn build_hash(lens: &[usize], nbuckets: usize, space_allowed: usize) -> Option<HashInfo> {
    let mut optimal = nbuckets;
    let mut used = 0usize;
    let mut peak = 0usize;
    for (i, len) in lens.iter().enumerate() {
        let ntuples = i + 1;
        if ntuples > optimal
            && optimal <= (i32::MAX as usize) / 2
            && optimal * 2 <= MAX_ALLOC / BUCKET_POINTER
        {
            optimal *= 2;
        }
        used += HJTUPLE_OVERHEAD + len;
        peak = peak.max(used);
        if used + optimal * BUCKET_POINTER > space_allowed {
            return None;
        }
    }
    used += optimal * BUCKET_POINTER;
    peak = peak.max(used);
    Some(HashInfo {
        buckets: optimal,
        original_buckets: nbuckets,
        batches: 1,
        original_batches: 1,
        peak_bytes: peak,
    })
}

impl PgHandler {
    /// Annotate a join's children with what running it counted (RP-386).
    /// `actual` is the rows the join itself returned.
    pub(crate) fn analyze_join_children(
        &self,
        tree: &mut PlanNode,
        sel: &secantus_pgplan::Select,
        actual: usize,
    ) {
        if tree.children.len() != 2 {
            return;
        }
        if tree.name.starts_with("Hash") && tree.name.ends_with(" Join") {
            self.analyze_hash_join(tree, sel, actual);
        } else if tree.name.starts_with("Nested Loop") {
            if tree.children[1].param_cond.is_some() {
                self.analyze_param_nest_loop(tree, actual);
            } else {
                self.analyze_plain_nest_loop(tree, sel, actual);
            }
        }
    }

    fn defined_table(&self, table: &str) -> Option<TableDef> {
        self.all_table_defs()
            .ok()?
            .into_iter()
            .find(|d| d.name == table)
    }

    /// A Hash Join of two unfiltered Seq Scans on one key, its only qual.
    fn analyze_hash_join(&self, tree: &mut PlanNode, sel: &secantus_pgplan::Select, actual: usize) {
        if tree.details.len() != 1 || tree.details[0].0 != "Hash Cond" {
            return;
        }
        let hash = &tree.children[1];
        if hash.name != "Hash" || hash.children.len() != 1 {
            return;
        }
        let Some((outer_table, outer_alias)) = bare_seq_scan(&tree.children[0]) else {
            return;
        };
        let Some((inner_table, inner_alias)) = bare_seq_scan(&hash.children[0]) else {
            return;
        };
        let Some((_, _, plan_rows, plan_width)) = hash.children[0].estimate else {
            return;
        };
        let Some((outer_key, inner_key, read)) =
            self.hash_join_reads(sel, &outer_alias, &inner_alias)
        else {
            return;
        };
        let (Ok(outer_docs), Ok(inner_docs)) =
            (self.table_docs(&outer_table), self.table_docs(&inner_table))
        else {
            return;
        };
        // An empty side stops the join before the other is read
        // (`ExecHashJoinImpl`'s empty-outer and empty-hash shortcuts).
        if outer_docs.is_empty() || inner_docs.is_empty() {
            return;
        }
        let Some(def) = self.defined_table(&inner_table) else {
            return;
        };
        // A RIGHT join keeps the inner side's NULL keys in the table
        // (`keepNulls`); the others do not hash them.
        let right = tree.name == "Hash Right Join";
        let left = tree.name == "Hash Left Join";
        if !(right || left || tree.name == "Hash Join") {
            return;
        }
        let mut counts: std::collections::HashMap<Key, usize> = std::collections::HashMap::new();
        let mut lens = Vec::new();
        let mut inner_nulls = 0usize;
        for d in &inner_docs {
            let Some(k) = key_of(d.get(&inner_key)) else {
                return;
            };
            match k {
                Some(k) => *counts.entry(k).or_default() += 1,
                None if right => inner_nulls += 1,
                None => continue,
            }
            let Some(len) = heap::minimal_tuple_len(&def, d, &read) else {
                return;
            };
            lens.push(len);
        }
        // The rows the join returns from these sides, which must be the
        // rows it did return: otherwise a scan read fewer rows than its
        // table holds, and these counts are not the plan's.
        let mut joined = 0usize;
        let mut matched: std::collections::HashSet<Key> = std::collections::HashSet::new();
        for d in &outer_docs {
            let Some(k) = key_of(d.get(&outer_key)) else {
                return;
            };
            let c = k.as_ref().and_then(|k| counts.get(k)).copied().unwrap_or(0);
            joined += if left { c.max(1) } else { c };
            if c > 0 {
                matched.extend(k);
            }
        }
        if right {
            joined += inner_nulls
                + counts
                    .iter()
                    .filter(|(k, _)| !matched.contains(*k))
                    .map(|(_, c)| c)
                    .sum::<usize>();
        }
        if joined != actual {
            return;
        }
        let multiplier = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get("hash_mem_multiplier")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(2.0);
        let work_mem = self.kb_setting("work_mem", 4096) * 1024;
        let space_allowed = (work_mem as f64 * multiplier).min(usize::MAX as f64 / 2.0) as usize;
        let Some(nbuckets) = initial_buckets(plan_rows, plan_width, space_allowed) else {
            return;
        };
        let Some(info) = build_hash(&lens, nbuckets, space_allowed) else {
            return;
        };
        tree.children[0].actual_rows = Some(outer_docs.len());
        let hash = &mut tree.children[1];
        hash.actual_rows = Some(lens.len());
        hash.hash_info = Some(info);
        hash.children[0].actual_rows = Some(inner_docs.len());
    }

    /// A Nested Loop over an unfiltered Seq Scan whose inner side is an
    /// index scan parameterized by the outer row, its Index Cond the only
    /// qual of the two (RP-354's plan).
    fn analyze_param_nest_loop(&self, tree: &mut PlanNode, actual: usize) {
        if !tree.details.is_empty() || tree.inner_unique {
            return;
        }
        let left = match tree.name.as_str() {
            "Nested Loop" => false,
            "Nested Loop Left Join" => true,
            _ => return,
        };
        let Some((outer_table, outer_alias)) = bare_seq_scan(&tree.children[0]) else {
            return;
        };
        let inner = &tree.children[1];
        let index_only = inner.name.starts_with("Index Only Scan using ");
        if !(index_only || inner.name.starts_with("Index Scan using ")) {
            return;
        }
        let Some((inner_table, _)) = leaf_relation(inner) else {
            return;
        };
        let [(label, cond)] = inner.details.as_slice() else {
            return;
        };
        if label != "Index Cond" {
            return;
        }
        // `(column op alias.column)`, as `move_param_conds` writes it.
        let Some(body) = cond.strip_prefix('(').and_then(|c| c.strip_suffix(')')) else {
            return;
        };
        let mut parts = body.split(' ');
        let (Some(col), Some(op), Some(other), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return;
        };
        let Some((alias, outer_col)) = other.split_once('.') else {
            return;
        };
        if alias != outer_alias {
            return;
        }
        let holds = |inner: f64, outer: f64| match op {
            "=" => inner == outer,
            "<" => inner < outer,
            "<=" => inner <= outer,
            ">" => inner > outer,
            ">=" => inner >= outer,
            _ => false,
        };
        if !["=", "<", "<=", ">", ">="].contains(&op) {
            return;
        }
        let (Some(outer_def), Some(inner_def)) = (
            self.defined_table(&outer_table),
            self.defined_table(&inner_table),
        ) else {
            return;
        };
        let (Some(outer_field), Some(inner_field)) =
            (outer_def.field_of(outer_col), inner_def.field_of(col))
        else {
            return;
        };
        if !self.has_heap(&inner_table)
            || self
                .with_heap_read(&inner_table, |h| h.has_dead_entries())
                .unwrap_or(true)
        {
            return;
        }
        let (Ok(outer_docs), Ok(inner_docs)) =
            (self.table_docs(&outer_table), self.table_docs(&inner_table))
        else {
            return;
        };
        if outer_docs.is_empty() {
            return;
        }
        // Each inner row's key and whether its heap page is all-visible.
        let mut keyed: Vec<(f64, i64)> = Vec::new();
        for d in &inner_docs {
            match d.get(&inner_field) {
                None | Some(Bson::Null) => continue,
                v => match (number(v), heap::ctid_of(d)) {
                    (Some(n), Some(c)) => keyed.push((n, c)),
                    _ => return,
                },
            }
        }
        let Ok(visible) = self.with_heap_read(&inner_table, |h| {
            keyed
                .iter()
                .map(|(_, c)| h.all_visible(*c))
                .collect::<Vec<bool>>()
        }) else {
            return;
        };
        let mut total = 0usize;
        let mut fetches = 0usize;
        let mut joined = 0usize;
        for d in &outer_docs {
            let v = match d.get(&outer_field) {
                // A NULL parameter makes the index scan return nothing.
                None | Some(Bson::Null) => {
                    joined += usize::from(left);
                    continue;
                }
                v => match number(v) {
                    Some(n) => n,
                    None => return,
                },
            };
            let mut here = 0usize;
            for ((k, _), vis) in keyed.iter().zip(&visible) {
                if holds(*k, v) {
                    here += 1;
                    fetches += usize::from(!vis);
                }
            }
            total += here;
            joined += if left { here.max(1) } else { here };
        }
        if joined != actual {
            return;
        }
        let loops = outer_docs.len();
        tree.children[0].actual_rows = Some(loops);
        let inner = &mut tree.children[1];
        inner.actual_rows = Some(per_loop(total, loops));
        inner.actual_loops = Some(loops);
        if index_only {
            inner.heap_fetches = Some(fetches);
        }
    }

    /// The rows a join's Seq Scan of `table` returns, and the rows it
    /// read: its table's, less those its Filter rejects. `None` where the
    /// scan's filter cannot be found, or a row cannot be matched.
    fn scan_rows(
        &self,
        sel: &secantus_pgplan::Select,
        scan: &PlanNode,
        table: &str,
        alias: &str,
    ) -> Option<(Vec<Document>, usize)> {
        let docs = self.table_docs(table).ok()?;
        let read = docs.len();
        if scan.details.is_empty() {
            return Some((docs, read));
        }
        let filter = self.join_leaf_scan_filter(sel, alias)?;
        // A Filter the plan prints must be a filter this found.
        if filter.is_empty() {
            return None;
        }
        let empty = Document::new();
        let in_sets = secantus_core::query::InSets::prepare(&filter);
        let _in_sets = secantus_core::query::InSetsGuard::install(&in_sets);
        let mut kept = Vec::new();
        for d in docs {
            if secantus_core::query::matches(&d, &filter, &empty, None).ok()? {
                kept.push(d);
            }
        }
        Some((kept, read))
    }

    /// A LEFT join's Join Filter, run over every pair as `ExecNestLoop`'s
    /// `ExecQual` runs it: the pairs it kept, and the rows the join
    /// returns (each outer row's kept pairs, or its one null-extended
    /// row). A pair counts as kept only where the qual is true; false and
    /// NULL both reject it. `None` for a qual this cannot plan over the
    /// two relations, or one that errors on a pair.
    fn left_filter_pairs(
        &self,
        cond: &str,
        outer: (&str, &str, &[Document]),
        inner: (&str, &str, &[Document]),
    ) -> Option<(usize, usize)> {
        let outer_def = self.defined_table(outer.0)?;
        let inner_def = self.defined_table(inner.0)?;
        let qual = secantus_pgplan::plan_pair_expression(
            cond,
            (outer.1, &outer_def),
            (inner.1, &inner_def),
        )
        .ok()?;
        let prefixed = |docs: &[Document], prefix: &str| -> Vec<Document> {
            docs.iter()
                .map(|d| {
                    d.iter()
                        .map(|(k, v)| (format!("{prefix}{k}"), v.clone()))
                        .collect()
                })
                .collect()
        };
        let outer_rows = prefixed(outer.2, secantus_pgplan::PAIR_OUTER_PREFIX);
        let inner_rows = prefixed(inner.2, secantus_pgplan::PAIR_INNER_PREFIX);
        let (mut kept, mut joined) = (0usize, 0usize);
        for o in &outer_rows {
            let mut here = 0usize;
            for i in &inner_rows {
                let mut pair = o.clone();
                pair.extend(i.clone());
                let holds = secantus_pgplan::apply_row_expr(&qual, &pair).ok()?;
                here += usize::from(matches!(holds, Bson::Boolean(true)));
            }
            kept += here;
            joined += here.max(1);
        }
        Some((kept, joined))
    }

    /// A Nested Loop of two unfiltered Seq Scans, the inner one under a
    /// Materialize or rescanned bare, its qual (if any) a Join Filter.
    fn analyze_plain_nest_loop(
        &self,
        tree: &mut PlanNode,
        sel: &secantus_pgplan::Select,
        actual: usize,
    ) {
        if tree.inner_unique {
            return;
        }
        let filtered = match tree.details.as_slice() {
            [] => false,
            [(label, _)] if label == "Join Filter" => true,
            _ => return,
        };
        let left = match tree.name.as_str() {
            "Nested Loop" => false,
            "Nested Loop Left Join" => true,
            _ => return,
        };
        let Some((outer_table, outer_alias)) = filtered_seq_scan(&tree.children[0]) else {
            return;
        };
        let inner = &tree.children[1];
        let material = inner.name == "Materialize";
        let scan = if material {
            match inner.children.as_slice() {
                [only] => only,
                _ => return,
            }
        } else {
            inner
        };
        let Some((inner_table, inner_alias)) = filtered_seq_scan(scan) else {
            return;
        };
        // Each scan's rows: those of its table its Filter keeps
        // (`ExecScan`'s `nfiltered1` counts the rest, RP-390).
        let Some((outer_docs, outer_read)) =
            self.scan_rows(sel, &tree.children[0], &outer_table, &outer_alias)
        else {
            return;
        };
        let Some((inner_docs, inner_read)) = self.scan_rows(sel, scan, &inner_table, &inner_alias)
        else {
            return;
        };
        let (n_outer, n_inner) = (outer_docs.len(), inner_docs.len());
        if n_outer == 0 || n_inner == 0 {
            return;
        }
        let pairs = n_outer * n_inner;
        // The pairs the filter kept (`ExecNestLoop`'s `nfiltered1` counts
        // the rest). An inner join returns exactly those; a LEFT join adds
        // a null-extended row for each outer row that kept none, so its
        // filter is evaluated per pair here to tell the two apart.
        let kept = if !filtered {
            if left || actual != pairs {
                return;
            }
            pairs
        } else if left {
            let cond = tree.details[0].1.clone();
            let Some((kept, joined)) = self.left_filter_pairs(
                &cond,
                (&outer_table, &outer_alias, &outer_docs),
                (&inner_table, &inner_alias, &inner_docs),
            ) else {
                return;
            };
            if joined != actual {
                return;
            }
            kept
        } else {
            if actual > pairs {
                return;
            }
            actual
        };
        let removed =
            |read: usize, kept: usize, n: &PlanNode| (!n.details.is_empty()).then(|| read - kept);
        tree.children[0].removed_by_filter = removed(outer_read, n_outer, &tree.children[0]);
        tree.children[0].actual_rows = Some(n_outer);
        if filtered {
            tree.join_filter_removed = Some(pairs - kept);
        }
        let inner = &mut tree.children[1];
        inner.actual_rows = Some(n_inner);
        inner.actual_loops = Some(n_outer);
        if material {
            // A Materialize reads its input once.
            let scan = &mut inner.children[0];
            scan.removed_by_filter = removed(inner_read, n_inner, scan);
            scan.actual_rows = Some(n_inner);
        } else {
            // Rescanned bare, it filters its table on every loop: the
            // per-loop count `ExplainNode` prints is one loop's.
            inner.removed_by_filter = removed(inner_read, n_inner, inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1000 rows of `(int, 'vN')` (a 23-byte minimal tuple) in a fresh
    /// table: PostgreSQL 15.19 prints `Buckets: 1024  Batches: 1  Memory
    /// Usage: 47kB` (1000 x (16 + 23) + 1024 x 8 = 47192 bytes).
    #[test]
    fn a_thousand_narrow_rows_take_47kb() {
        let space = 4096 * 1024 * 2;
        let buckets = initial_buckets(1000.0, 6, space).expect("one batch");
        assert_eq!(buckets, 1024);
        let info = build_hash(&[23; 1000], buckets, space).expect("one batch");
        assert_eq!(info.buckets, 1024);
        assert_eq!(info.peak_bytes, 47192);
        assert_eq!(info.text(), "Buckets: 1024  Batches: 1  Memory Usage: 47kB");
    }

    /// More rows than the estimate: the buckets double while it builds.
    #[test]
    fn an_underestimate_grows_the_buckets() {
        let space = 4096 * 1024 * 2;
        let info = build_hash(&[23; 3000], 1024, space).expect("one batch");
        assert_eq!(info.buckets, 4096);
        assert_eq!(
            info.text(),
            format!(
                "Buckets: 4096 (originally 1024)  Batches: 1  Memory Usage: {}kB",
                (3000 * 39 + 4096 * 8usize).div_ceil(1024)
            )
        );
    }

    /// A table past the memory budget would add batches: not modelled.
    #[test]
    fn a_table_past_hash_mem_is_refused() {
        assert_eq!(initial_buckets(1_000_000.0, 100, 64 * 1024), None);
        assert_eq!(build_hash(&[100; 2000], 1024, 64 * 1024), None);
    }

    #[test]
    fn a_rescanned_nodes_rows_are_the_rounded_average() {
        assert_eq!(per_loop(499_500, 1000), 500);
        assert_eq!(per_loop(10, 4), 2);
        assert_eq!(per_loop(14, 4), 4);
    }
}
