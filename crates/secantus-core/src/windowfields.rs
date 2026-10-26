//! `$setWindowFields` (5.0+): partition + sort + per-row windowed accumulators.
//! A storage-free transform, a bounded port of
//! `aggregate._stage_set_window_fields`.
//!
//! Supported: `partitionBy` (an expression, or absent → one partition), an
//! optional `sortBy`, and an `output` map of `{field: {<accumulator>: arg,
//! window?: {...}}}`. The three rank functions (`$rank` / `$denseRank` /
//! `$documentNumber`), the position-based `$shift` (`{output, by, default?}`),
//! the prefix-accumulated `$expMovingAvg` (`{input, N|alpha}`), the gap-fill
//! `$locf` / `$linearFill` (`<expr>`), the window `$derivative` / `$integral`
//! (`{input, unit?}` — slope / trapezoidal area over the sortBy x-axis; a
//! fixed-duration `unit` scales the x-axis against a date sortBy so the rate is
//! *per unit*), plus every
//! `$group` accumulator the shared `group` module knows
//! (`$sum`/`$avg`/`$min`/`$max`/`$first`/`$last`/`$push`/`$addToSet`/
//! `$count`) evaluate over document-based windows (`documents: [lo, hi]`, with
//! `"unbounded"` / `"current"` / integer bounds; a missing/empty window → the
//! whole partition, matching mongod's default) and value-based windows
//! (`range: [lo, hi]` over a single ascending numeric sortBy — rows whose value
//! is in `[cur+lo, cur+hi]`, with `"unbounded"` / `"current"` / numeric bounds).
//! A range window with a fixed-duration time `unit` (`week`/`day`/`hour`/
//! `minute`/`second`/`millisecond`) offsets against a date sortBy's epoch millis.
//!
//! Defers (`Err(Fallback::Defer)` → Python) on: range windows or `$derivative` / `$integral`
//! with a variable-length `unit` (`month`/`quarter`/`year`), a non-ascending /
//! multi-field / non-numeric sortBy, a `unit` without a date sortBy, any
//! unsupported accumulator, a non-document/empty `output`, an unsortable
//! `sortBy` value, and a `partitionBy` expression the evaluator can't reproduce.
//! Output preserves the *original* input order (only per-partition ordering
//! drives the windows).

use std::cmp::Ordering;
use std::collections::HashMap;

use bson::{Bson, Document};

use crate::fallback::Fallback;
use crate::summation::DoubleDoubleSum;
use crate::{decimal, expressions, group, order, paths};

/// Carries `Fallback` rather than `()` so an error the expression engine NAMED
/// survives to the client instead of being flattened into the generic
/// "not supported by the Rust server". `Fallback::Defer` is the same
/// "cannot reproduce this" signal the unit `()` used to be.
type R<T> = Result<T, Fallback>;

const RANK_FUNCS: [&str; 3] = ["$rank", "$denseRank", "$documentNumber"];
/// Ops whose output is a per-slot vector precomputed once over the sorted
/// partition (prefix accumulation / gap-fill), rather than a window accumulator.
const PREFIX_OPS: [&str; 3] = ["$expMovingAvg", "$locf", "$linearFill"];

/// The covariances, which run their own removable state whatever the window.
const COVARIANCE_OPS: [&str; 2] = ["$covariancePop", "$covarianceSamp"];

/// The window functions mongod runs as removable state over a window that
/// drops documents.
const REMOVABLE_OPS: [&str; 4] = ["$sum", "$avg", "$stdDevPop", "$stdDevSamp"];

fn field_value(doc: &Document, field: &str) -> Bson {
    paths::get_path(doc, field).cloned().unwrap_or(Bson::Null)
}

struct OutField<'a> {
    field: &'a str,
    op: &'a str,
    arg: &'a Bson,
    window: Option<&'a Document>,
}

/// `$setWindowFields` over `docs`, every output document collected.
pub fn set_window_fields_stage(
    spec: &Bson,
    docs: Vec<Document>,
    vars: &Document,
) -> R<Vec<Document>> {
    let mut out = Vec::with_capacity(docs.len());
    set_window_fields_each(spec, docs, vars, None, &mut |d| {
        out.push(d);
        Ok(())
    })?;
    Ok(out)
}

/// `$setWindowFields` over `docs`, each output document handed to `sink`
/// as it is made, partition by partition in `sortBy` order -- so the stages
/// after it can run on one document at a time, as mongod's do, instead of
/// every document holding its window's values at once (RM-211: an unbounded
/// `$push` over 110 MB of documents held 110 copies of it).
///
/// `downstream`: the top-level fields the stages after this one read, or
/// `None` when they need whole documents. With the fields this stage reads
/// it is what mongod keeps of each document in the partition's cache, which
/// is held to 100 MiB (`internalDocumentSourceSetWindowFieldsMaxMemoryBytes`):
/// past it, with `allowDiskUse: false`, mongod refuses with 5643011, and
/// otherwise spills the cache. Only a window that reads to the end of its
/// partition is counted -- one with an `"unbounded"` upper bound, or none --
/// which holds the whole partition; a bounded window holds less, and is not
/// refused here.
pub(crate) fn set_window_fields_each(
    spec: &Bson,
    docs: Vec<Document>,
    vars: &Document,
    downstream: Option<&std::collections::BTreeSet<String>>,
    sink: &mut dyn FnMut(Document) -> R<()>,
) -> R<()> {
    let spec = spec.as_document().ok_or(Fallback::Defer)?;
    let partition_by = spec.get("partitionBy");
    let sort_by = match spec.get("sortBy") {
        None => None,
        Some(Bson::Document(d)) => Some(d),
        Some(_) => return Err(Fallback::Defer),
    };
    let output = spec
        .get("output")
        .and_then(Bson::as_document)
        .filter(|o| !o.is_empty())
        .ok_or(Fallback::Defer)?;

    // Compile the output fields: each is `{<$accumulator>: arg, window?: {...}}`.
    let mut compiled: Vec<OutField> = Vec::new();
    for (field, field_spec) in output {
        let fs = field_spec.as_document().ok_or(Fallback::Defer)?;
        let window = match fs.get("window") {
            None => None,
            Some(Bson::Document(w)) => Some(w),
            Some(_) => return Err(Fallback::Defer),
        };
        // The accumulator is the single `$`-prefixed key (the optional `window`
        // key is the only other key mongod allows here).
        let mut acc: Option<(&String, &Bson)> = None;
        for (k, v) in fs {
            if !k.starts_with('$') {
                continue;
            }
            if acc.is_some() {
                return Err(Fallback::Defer); // more than one accumulator
            }
            acc = Some((k, v));
        }
        let (op, arg) = acc.ok_or(Fallback::Defer)?;
        let op = op.as_str();
        if RANK_FUNCS.contains(&op) {
            // Rank functions take no argument and no window; `$rank`/`$denseRank`
            // additionally require a `sortBy`.
            let arg_empty =
                matches!(arg, Bson::Null) || matches!(arg, Bson::Document(d) if d.is_empty());
            if !arg_empty || window.is_some() {
                return Err(Fallback::Defer);
            }
            if (op == "$rank" || op == "$denseRank") && sort_by.is_none() {
                return Err(Fallback::Defer);
            }
        } else if op == "$shift" {
            // Position-based (like the rank funcs): requires a sortBy, no window,
            // and an `{output, by: <int>, default?}` spec.
            let spec = arg.as_document().ok_or(Fallback::Defer)?;
            if window.is_some() || sort_by.is_none() || !spec.contains_key("output") {
                return Err(Fallback::Defer);
            }
            if !matches!(spec.get("by"), Some(Bson::Int32(_)) | Some(Bson::Int64(_))) {
                return Err(Fallback::Defer);
            }
        } else if op == "$expMovingAvg" {
            // Prefix accumulation: requires a sortBy, no window, `{input, N|alpha}`.
            let spec = arg.as_document().ok_or(Fallback::Defer)?;
            if window.is_some() || sort_by.is_none() || !spec.contains_key("input") {
                return Err(Fallback::Defer);
            }
            ema_alpha(spec)?; // validates exactly-one-of N/alpha and their ranges
        } else if op == "$locf" || op == "$linearFill" {
            // Gap-fill over the sorted partition; `arg` is the input expression.
            // No window; requires a sortBy ($linearFill also a numeric x-axis,
            // checked when the vector is built).
            if window.is_some() || sort_by.is_none() {
                return Err(Fallback::Defer);
            }
        } else if op == "$derivative" || op == "$integral" {
            // Window operators over the sortBy value (x) and input (y). Requires a
            // sortBy; a time `unit` scales the x-axis against a date sortBy
            // (validated in `ts_window_value` where the partition values are known).
            let spec = arg.as_document().ok_or(Fallback::Defer)?;
            if sort_by.is_none() || !spec.contains_key("input") {
                return Err(Fallback::Defer);
            }
            // Measured 8.2.11, 2026-10-20: this answered over the whole
            // partition.
            if op == "$derivative" && window.is_none() {
                return Err(
                    Fallback::mongo(9, "$derivative requires explicit window bounds").bare(),
                );
            }
        } else if COVARIANCE_OPS.contains(&op) {
            // `[x, y]`, evaluated per document; any window.
        } else if op == "$mergeObjects" {
            // $mergeObjects is a $group-only accumulator; mongod rejects it as a
            // window function (FailedToParse) — defer so Python raises the code.
            return Err(Fallback::Defer);
        } else {
            group::new_acc(op)?; // reject unsupported accumulator (incl. time-series) → defer
        }
        compiled.push(OutField {
            field,
            op,
            arg,
            window,
        });
    }

    let needs_rank = compiled.iter().any(|c| RANK_FUNCS.contains(&c.op));
    // Range windows, `$linearFill`, and `$derivative`/`$integral` all need the
    // single-ascending-numeric sortBy values (window bounds / interp / rate x-axis).
    let needs_range = compiled.iter().any(|c| {
        c.window.is_some_and(|w| w.contains_key("range"))
            || matches!(c.op, "$linearFill" | "$derivative" | "$integral")
    });

    // Partition; each partition holds the *original* indices of its members
    // (in input order), and the key it was found under.
    let mut partitions: Vec<Vec<usize>> = Vec::new();
    let mut keys: Vec<Bson> = Vec::new();
    let mut key_index: HashMap<Vec<u8>, usize> = HashMap::new();
    for (i, doc) in docs.iter().enumerate() {
        let key_val = match partition_by {
            Some(by) => expressions::evaluate(doc, by, vars)?,
            None => Bson::Null,
        };
        if matches!(key_val, Bson::Array(_)) {
            return Err(Fallback::mongo(
                14,
                "An expression used to partition cannot evaluate to value of type array",
            )
            .exec());
        }
        let mut wrap = Document::new();
        wrap.insert("k", crate::collation::fold_active(key_val.clone()));
        let key = bson::to_vec(&wrap).map_err(|_| Fallback::Defer)?;
        let idx = *key_index.entry(key).or_insert_with(|| {
            partitions.push(Vec::new());
            keys.push(key_val);
            partitions.len() - 1
        });
        partitions[idx].push(i);
    }
    // mongod sorts by the partition key first (`$setWindowFields` is a
    // `$sort` on the key and `sortBy`), so the partitions come out in key
    // order -- under the collation, null and missing as one -- and stay in
    // the order they were found when keys compare equal. Measured 8.2.11,
    // 2026-10-16, on five specs with and without a collation. (An earlier
    // note here recorded first-seen order; it was not re-measured then.)
    if keys.iter().all(order::is_comparable) {
        let mut by_key: Vec<usize> = (0..partitions.len()).collect();
        by_key.sort_by(|&a, &b| order::cmp(&keys[a], &keys[b]));
        let mut taken: Vec<Option<Vec<usize>>> = partitions.into_iter().map(Some).collect();
        partitions = by_key
            .iter()
            .map(|&i| taken[i].take().unwrap_or_default())
            .collect();
    }

    // What of each document the partition cache holds.
    let cached = cache_fields(spec, &compiled, downstream);
    let mut spill = CacheSpill::default();

    // mongod emits PARTITION BY PARTITION, in partition-key order (above),
    // and within each partition in `sortBy` order -- not the input order.
    // Wrong order is wrong RESULTS as soon as a `$limit` follows, so this is
    // not cosmetic. `sorted_slots` yields exactly that sequence.
    let ordered = partitions
        .iter()
        .map(|part| sorted_slots(part, &docs, sort_by))
        .collect::<R<Vec<_>>>()?;
    for slots in &ordered {
        let slots = slots.clone();
        let n = slots.len();
        let ranks = if needs_rank {
            Some(compute_ranks(&slots, &docs, sort_by))
        } else {
            None
        };
        // Range windows resolve bounds against the single ascending numeric
        // sortBy value; `None` (multi-field / descending / non-numeric) makes a
        // range window defer.
        let range_vals = if needs_range {
            range_values(&slots, &docs, sort_by)
        } else {
            None
        };
        // Epoch-millis of a date sortBy — the x-axis for a time-`unit` range
        // window (mutually exclusive with the numeric `range_vals`).
        let range_dates = if needs_range {
            range_dates(&slots, &docs, sort_by)
        } else {
            None
        };
        // mongod's partition cache over this partition: held to its budget,
        // spilled or refused (RM-211).
        run_cache(
            &compiled,
            &slots,
            &docs,
            cached.as_ref(),
            range_vals.as_deref(),
            range_dates.as_deref(),
            vars,
            &mut spill,
        )?;
        // $expMovingAvg / $locf / $linearFill each produce a per-slot vector —
        // precompute once per partition per output field (parallel to `compiled`).
        let sort_field = sort_by
            .filter(|sb| sb.len() == 1)
            .and_then(|sb| sb.keys().next())
            .map(String::as_str);
        let mut prefix_vecs: Vec<Option<Vec<Bson>>> = Vec::with_capacity(compiled.len());
        for c in &compiled {
            prefix_vecs.push(if PREFIX_OPS.contains(&c.op) {
                Some(window_vector(c.op, &slots, &docs, c.arg, sort_field, vars)?)
            } else {
                None
            });
        }
        // `$sum` / `$avg` / `$stdDevPop` / `$stdDevSamp` over a window that
        // drops documents run mongod's removable state along the partition; a
        // whole-partition or `unbounded`-lower window takes the accumulator
        // path below, as on mongod.
        let mut std_dev_vecs: Vec<Option<Vec<Bson>>> = Vec::with_capacity(compiled.len());
        for c in &compiled {
            std_dev_vecs.push(
                if COVARIANCE_OPS.contains(&c.op)
                    || c.op == "$integral"
                    || (REMOVABLE_OPS.contains(&c.op) && window_removes(c.window))
                {
                    Some(removable_window(
                        c,
                        &slots,
                        &docs,
                        range_vals.as_deref(),
                        range_dates.as_deref(),
                        sort_field,
                        vars,
                    )?)
                } else {
                    None
                },
            );
        }
        // The last window each accumulator output was computed over, and its
        // value: a window that does not move (the whole partition, say) is
        // computed once, not once per document.
        let mut last_window: Vec<Option<(i64, i64, Bson)>> = vec![None; compiled.len()];
        for (pos, &orig_i) in slots.iter().enumerate() {
            let mut out_doc = docs[orig_i].clone();
            for (ci, c) in compiled.iter().enumerate() {
                let value = if RANK_FUNCS.contains(&c.op) {
                    let r = ranks.as_ref().unwrap();
                    Bson::Int32(match c.op {
                        "$documentNumber" => r.doc_number[pos],
                        "$rank" => r.rank[pos],
                        _ => r.dense[pos], // "$denseRank"
                    })
                } else if PREFIX_OPS.contains(&c.op) {
                    prefix_vecs[ci].as_ref().unwrap()[pos].clone()
                } else if let Some(v) = &std_dev_vecs[ci] {
                    v[pos].clone()
                } else if c.op == "$shift" {
                    shift_value(
                        c.arg.as_document().ok_or(Fallback::Defer)?,
                        pos,
                        &slots,
                        &docs,
                        vars,
                    )?
                } else if c.op == "$derivative" {
                    let (low, high) = if c.window.is_some_and(|w| w.contains_key("range")) {
                        range_window_bounds(
                            pos,
                            n,
                            c.window.unwrap(),
                            range_vals.as_deref(),
                            range_dates.as_deref(),
                        )?
                    } else {
                        window_bounds(pos, n, c.window)?
                    };
                    derivative_value(
                        c.arg.as_document().ok_or(Fallback::Defer)?,
                        low,
                        high,
                        &slots,
                        &docs,
                        sort_field.ok_or(Fallback::Defer)?,
                        vars,
                    )?
                } else {
                    let (low, high) = if c.window.is_some_and(|w| w.contains_key("range")) {
                        range_window_bounds(
                            pos,
                            n,
                            c.window.unwrap(),
                            range_vals.as_deref(),
                            range_dates.as_deref(),
                        )?
                    } else {
                        window_bounds(pos, n, c.window)?
                    };
                    match &last_window[ci] {
                        Some((l, h, v)) if *l == low && *h == high => v.clone(),
                        _ => {
                            let mut acc = group::new_acc(c.op)?;
                            if high >= low {
                                for &s in &slots[low as usize..=high as usize] {
                                    group::apply_acc(&mut acc, c.arg, &docs[s], vars)?;
                                }
                            }
                            let v = group::finalize_window_value(acc)?;
                            last_window[ci] = Some((low, high, v.clone()));
                            v
                        }
                    }
                };
                paths::set_path(&mut out_doc, c.field, value).map_err(|_| Fallback::Defer)?;
            }
            sink(out_doc)?;
        }
    }
    spill.finish()
}

/// The top-level fields of each document the partition cache keeps (`None`:
/// whole documents): what this stage reads, and what the stages after it do.
fn cache_fields(
    spec: &Document,
    compiled: &[OutField],
    downstream: Option<&std::collections::BTreeSet<String>>,
) -> Option<std::collections::BTreeSet<String>> {
    let mut fields = downstream?.clone();
    if let Some(by) = spec.get("partitionBy") {
        if !group::collect_fields(by, &mut fields) {
            return None;
        }
    }
    if let Some(Bson::Document(sb)) = spec.get("sortBy") {
        for k in sb.keys() {
            fields.insert(k.split('.').next().unwrap_or(k).to_string());
        }
    }
    for c in compiled {
        if !group::collect_fields(c.arg, &mut fields) {
            return None;
        }
    }
    Some(fields)
}

/// The documents mongod's partition iterator needs for `c` to answer `pos`
/// (of `n`): the first it must still hold and the last it must have read.
/// Measured on mongod 8.2.11 (2026-10-26, `maxTotalMemoryUsageBytes` and
/// the budget bisected over a hundred documents): a window that drops
/// documents needs the one answered and those its upper end reaches
/// (`[-5, 5]`: six); `$first` / `$last` and a `range` window hold their
/// whole window and the document leaving it, and a `range` window reads
/// one past its end; a window with no upper end reads the partition;
/// `$rank` reads one ahead, `$shift` back reads two.
fn reach(
    c: &OutField,
    pos: usize,
    n: usize,
    range_vals: Option<&[f64]>,
    range_dates: Option<&[f64]>,
) -> R<(usize, usize)> {
    let last = (n - 1) as i64;
    let p = pos as i64;
    let clamp = |i: i64| i.clamp(0, last) as usize;
    if RANK_FUNCS.contains(&c.op) {
        return Ok((pos, clamp(p + 2)));
    }
    if matches!(c.op, "$expMovingAvg" | "$locf") {
        return Ok((pos, pos));
    }
    if c.op == "$linearFill" {
        return Ok((pos, clamp(last)));
    }
    if c.op == "$shift" {
        let by = c
            .arg
            .as_document()
            .and_then(|d| d.get("by"))
            .and_then(|b| match b {
                Bson::Int32(i) => Some(i64::from(*i)),
                Bson::Int64(i) => Some(*i),
                _ => None,
            })
            .unwrap_or(0);
        return Ok(if by > 0 {
            (pos, clamp(p + by))
        } else {
            (clamp(p + by), clamp(p + 2))
        });
    }
    let Some(w) = c.window else {
        return Ok((pos, clamp(last)));
    };
    let range = w.contains_key("range");
    let (lo, hi) = if range {
        range_window_bounds(pos, n, w, range_vals, range_dates)?
    } else {
        window_bounds(pos, n, Some(w))?
    };
    let unbounded_hi = w
        .get("documents")
        .or_else(|| w.get("range"))
        .and_then(Bson::as_array)
        .is_some_and(|b| matches!(b.get(1), Some(Bson::String(s)) if s == "unbounded"));
    let hi = if unbounded_hi {
        last
    } else if range {
        hi + 1
    } else {
        hi
    };
    if range || (matches!(c.op, "$first" | "$last") && window_removes(Some(w))) {
        return Ok((clamp((lo - 1).min(p)), clamp(hi.max(p))));
    }
    Ok((pos, clamp(hi.max(p))))
}

/// What one window function holds while `pos` is answered, as mongod's
/// tracker counts it (measured 8.2.11, `maxFunctionMemoryUsageBytes`): a
/// window that drops documents runs a removable function of a fixed size;
/// one that only grows runs the `$group` accumulator over what it has read,
/// counted as `$group` counts it; `$first` / `$last` and the position
/// functions hold nothing of their own.
enum FunctionMemory {
    Nothing,
    /// A removable function: its own size, and each value in its window --
    /// once for a value a `Value` holds inline, twice for one on the heap,
    /// and `$push` every value twice (Linux x86-64 8.2.11, five ints in a
    /// `[-5, 5]` window: `$sum` 376, `$avg` 384, `$push` 320, `$stdDevPop`
    /// 552; macOS arm64 counts 24 more for `$sum`).
    Removable {
        base: usize,
        sizes: Vec<Option<usize>>,
    },
    Growing {
        acc: group::Acc,
        fed: usize,
    },
}

impl FunctionMemory {
    fn of(c: &OutField) -> R<FunctionMemory> {
        if matches!(c.op, "$first" | "$last")
            || RANK_FUNCS.contains(&c.op)
            || PREFIX_OPS.contains(&c.op)
            || COVARIANCE_OPS.contains(&c.op)
            || matches!(c.op, "$shift" | "$derivative" | "$integral")
        {
            return Ok(FunctionMemory::Nothing);
        }
        if window_removes(c.window) {
            return Ok(FunctionMemory::Removable {
                base: match c.op {
                    "$avg" => 304,
                    "$push" => 160,
                    "$stdDevPop" | "$stdDevSamp" => 472,
                    _ => 296,
                },
                sizes: Vec::new(),
            });
        }
        Ok(FunctionMemory::Growing {
            acc: group::new_acc(c.op)?,
            fed: 0,
        })
    }

    /// The bytes held once the window covers indices `from..=upto` of `slots`.
    fn held(
        &mut self,
        c: &OutField,
        (from, upto): (usize, usize),
        slots: &[usize],
        docs: &[Document],
        vars: &Document,
    ) -> R<usize> {
        Ok(match self {
            FunctionMemory::Nothing => 0,
            FunctionMemory::Removable { base, sizes } => {
                if sizes.is_empty() {
                    sizes.resize(slots.len(), None);
                }
                let mut held = *base;
                for i in from..=upto.min(slots.len() - 1) {
                    let size = match sizes[i] {
                        Some(s) => s,
                        None => {
                            let v = if c.op == "$count" {
                                Bson::Int32(1)
                            } else {
                                expressions::evaluate(&docs[slots[i]], c.arg, vars)
                                    .unwrap_or(Bson::Null)
                            };
                            let size = crate::memlimit::classic_size(&v);
                            let s = if size > 16 || c.op == "$push" {
                                2 * size
                            } else {
                                size
                            };
                            sizes[i] = Some(s);
                            s
                        }
                    };
                    held += size;
                }
                held
            }
            FunctionMemory::Growing { acc, fed } => {
                while *fed <= upto && *fed < slots.len() {
                    group::apply_acc(acc, c.arg, &docs[slots[*fed]], vars)?;
                    *fed += 1;
                }
                group::acc_memory_classic(acc)
            }
        })
    }
}

/// What spilling the partition cache wrote, across the stage's partitions.
#[derive(Default)]
struct CacheSpill {
    file: Option<(std::path::PathBuf, std::io::BufWriter<std::fs::File>)>,
    /// The spill table's size so far.
    stored: i64,
}

impl CacheSpill {
    /// Write `docs` (each with the 32 bytes of metadata mongod's
    /// `toBsonWithMetaData` adds for a sorted document; measured 8.2.11) to
    /// the stage's spill table, a file here.
    fn write(&mut self, docs: &[&Document]) -> R<()> {
        use std::io::Write;
        let io = |what: &str, e: &dyn std::fmt::Display| {
            Fallback::mongo(
                1,
                format!("$setWindowFields could not {what} its spill file: {e}"),
            )
        };
        if self.file.is_none() {
            let dir = crate::memlimit::spill_dir();
            std::fs::create_dir_all(&dir).map_err(|e| io("create the directory for", &e))?;
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = dir.join(format!(
                "secantus-window-{}-{}.bson",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let file = std::fs::File::create(&path).map_err(|e| io("create", &e))?;
            self.file = Some((path, std::io::BufWriter::new(file)));
        }
        let (_, out) = self.file.as_mut().expect("opened above");
        let mut bytes = 0i64;
        for d in docs {
            let mut buf = Vec::new();
            d.to_writer(&mut buf)
                .map_err(|e| io("encode a document for", &e))?;
            out.write_all(&buf).map_err(|e| io("write", &e))?;
            bytes += buf.len() as i64 + 32;
        }
        out.flush().map_err(|e| io("write", &e))?;
        let stored = out
            .get_ref()
            .metadata()
            .map_err(|e| io("measure", &e))?
            .len() as i64;
        crate::memlimit::record_window_spill(docs.len() as i64, bytes, stored - self.stored);
        self.stored = stored;
        Ok(())
    }

    /// Drop the spill table.
    fn finish(mut self) -> R<()> {
        if let Some((path, out)) = self.file.take() {
            drop(out);
            std::fs::remove_file(&path).map_err(|e| {
                Fallback::mongo(
                    1,
                    format!("$setWindowFields could not remove its spill file: {e}"),
                )
            })?;
        }
        Ok(())
    }
}

impl Drop for CacheSpill {
    fn drop(&mut self) {
        if let Some((path, _)) = self.file.take() {
            if let Err(e) = std::fs::remove_file(&path) {
                eprintln!(
                    "secantus: could not remove $setWindowFields spill file {}: {e}",
                    path.display()
                );
            }
        }
    }
}

/// mongod's partition cache (`SpillableDeque`) over `slots`, as
/// `DocumentSourceInternalSetWindowFields::doGetNext` drives it: for each
/// document it reads what the windows need, answers, counts its cache and
/// its functions together -- spilling the cache if that is over the budget,
/// refusing (5414201) if it still is -- then advances, reading the next
/// document and dropping those no window needs any more. Each document read
/// is counted at its size with only the fields kept; past the budget the
/// cache spills all it holds, or with `allowDiskUse: false` refuses
/// (5643011).
#[allow(clippy::too_many_arguments)]
fn run_cache(
    compiled: &[OutField],
    slots: &[usize],
    docs: &[Document],
    fields: Option<&std::collections::BTreeSet<String>>,
    range_vals: Option<&[f64]>,
    range_dates: Option<&[f64]>,
    vars: &Document,
    spill: &mut CacheSpill,
) -> R<()> {
    let n = slots.len();
    if n == 0 {
        return Ok(());
    }
    let budget = crate::memlimit::budgets().window_fields;
    let size = |i: usize| match fields {
        Some(f) => crate::memlimit::fields_size(&docs[slots[i]], f),
        None => crate::memlimit::document_size(&docs[slots[i]]),
    };
    let mut functions = compiled
        .iter()
        .map(FunctionMemory::of)
        .collect::<R<Vec<_>>>()?;
    struct Cache {
        held: std::collections::VecDeque<(usize, usize)>,
        used: usize,
        next: usize,
    }
    let mut cache = Cache {
        held: std::collections::VecDeque::new(),
        used: 0,
        next: 0,
    };
    let spill_all = |cache: &mut Cache, spill: &mut CacheSpill| -> R<()> {
        if !cache.held.is_empty() {
            let out: Vec<&Document> = cache.held.iter().map(|(i, _)| &docs[slots[*i]]).collect();
            spill.write(&out)?;
            cache.held.clear();
        }
        cache.used = 0;
        Ok(())
    };
    let read_to = |cache: &mut Cache, upto: usize, spill: &mut CacheSpill| -> R<()> {
        while cache.next <= upto && cache.next < n {
            let s = size(cache.next);
            cache.held.push_back((cache.next, s));
            cache.used += s;
            cache.next += 1;
            if cache.used > budget {
                if !crate::memlimit::disk_allowed() {
                    let used = cache.used;
                    return Err(Fallback::mongo(
                        5643011,
                        format!(
                            "Exceeded max memory. Current memory: {used} bytes. Max allowed \
                             memory: {budget} bytes. Set 'allowDiskUse: true' to spill to disk"
                        ),
                    ));
                }
                spill_all(cache, spill)?;
            }
        }
        Ok(())
    };
    for pos in 0..n {
        let mut ahead = pos;
        let mut reads = Vec::with_capacity(compiled.len());
        for c in compiled {
            ahead = ahead.max(reach(c, pos, n, range_vals, range_dates)?.1);
            // A function reads its window, not the iterator's look-ahead.
            let (from, upto) = match c.window {
                None => (0, n as i64 - 1),
                Some(w) if w.contains_key("range") => {
                    range_window_bounds(pos, n, w, range_vals, range_dates)?
                }
                Some(w) => window_bounds(pos, n, Some(w))?,
            };
            reads.push((from.max(0) as usize, upto.max(0) as usize));
        }
        read_to(&mut cache, ahead, spill)?;
        let mut functions_held = 0usize;
        for ((f, c), window) in functions.iter_mut().zip(compiled).zip(&reads) {
            functions_held += f.held(c, *window, slots, docs, vars)?;
        }
        if cache.used + functions_held > budget {
            if crate::memlimit::disk_allowed() {
                spill_all(&mut cache, spill)?;
            }
            // mongod names what its functions hold, the cache being its own
            // tracker's (measured 8.2.11).
            if cache.used + functions_held > budget {
                return Err(Fallback::mongo(
                    5414201,
                    format!(
                        "Exceeded memory limit in DocumentSourceSetWindowFields, used {functions_held} \
                         bytes but max allowed is {budget}"
                    ),
                ));
            }
        }
        // Advance: read the next document, then let go of what the next
        // answer does not need.
        if pos + 1 < n {
            read_to(&mut cache, pos + 1, spill)?;
            let mut keep = pos + 1;
            for c in compiled {
                keep = keep.min(reach(c, pos + 1, n, range_vals, range_dates)?.0);
            }
            while cache.held.front().is_some_and(|(i, _)| *i < keep) {
                let (_, s) = cache.held.pop_front().expect("a front");
                cache.used -= s;
            }
        }
    }
    Ok(())
}

/// The partition's original indices, reordered by `sortBy` (BSON order, stable,
/// reversed multi-pass so earlier fields win; ties keep input order). No
/// `sortBy` → input order unchanged.
fn sorted_slots(part: &[usize], docs: &[Document], sort_by: Option<&Document>) -> R<Vec<usize>> {
    let mut slots = part.to_vec();
    let Some(sb) = sort_by else {
        return Ok(slots);
    };
    let fields: Vec<(&String, bool)> = sb
        .iter()
        .map(|(f, dir)| (f, matches!(dir, Bson::Int32(-1) | Bson::Int64(-1))))
        .collect();
    for (f, _) in &fields {
        if part
            .iter()
            .any(|&i| !order::is_sortable(&field_value(&docs[i], f)))
        {
            return Err(Fallback::Defer); // order::cmp's precondition
        }
    }
    for (field, desc) in fields.iter().rev() {
        slots.sort_by(|&a, &b| {
            // An array sorts by its smallest element (largest, descending).
            let o = order::cmp(
                &order::sort_repr(field_value(&docs[a], field), *desc),
                &order::sort_repr(field_value(&docs[b], field), *desc),
            );
            if *desc {
                o.reverse()
            } else {
                o
            }
        });
    }
    Ok(slots)
}

struct Ranks {
    doc_number: Vec<i32>,
    rank: Vec<i32>,
    dense: Vec<i32>,
}

/// Per-slot rank vectors over a sorted partition. `$documentNumber` is the
/// 1-indexed slot; `$rank` restarts at the tie-group's first slot (gaps on
/// ties); `$denseRank` increments once per distinct sort key (no gaps). Ties are
/// consecutive slots whose `sortBy` field values all compare `Equal`.
fn compute_ranks(slots: &[usize], docs: &[Document], sort_by: Option<&Document>) -> Ranks {
    let n = slots.len();
    let mut doc_number = vec![0i32; n];
    let mut rank = vec![0i32; n];
    let mut dense = vec![0i32; n];
    if n == 0 {
        return Ranks {
            doc_number,
            rank,
            dense,
        };
    }
    doc_number[0] = 1;
    rank[0] = 1;
    dense[0] = 1;
    let mut cur_rank = 1i32;
    let mut cur_dense = 1i32;
    for i in 1..n {
        doc_number[i] = i as i32 + 1;
        let tied = match sort_by {
            Some(sb) => sb.iter().all(|(f, dir)| {
                let desc = matches!(dir, Bson::Int32(-1) | Bson::Int64(-1));
                order::cmp(
                    &order::sort_repr(field_value(&docs[slots[i]], f), desc),
                    &order::sort_repr(field_value(&docs[slots[i - 1]], f), desc),
                ) == Ordering::Equal
            }),
            None => false,
        };
        if !tied {
            cur_rank = i as i32 + 1;
            cur_dense += 1;
        }
        rank[i] = cur_rank;
        dense[i] = cur_dense;
    }
    Ranks {
        doc_number,
        rank,
        dense,
    }
}

/// Inclusive `(low, high)` document-window indices into the sorted partition for
/// the row at `slot`. Missing/empty `window` or one without `documents` → the
/// whole partition. Bounds: `"unbounded"` → partition edge, `"current"` →
/// `slot`, integer `b` → `slot + b`; anything else (incl. bool) → defer.
/// `high < low` signals an empty window (caller uses the empty value). Range
/// windows are routed to [`range_window_bounds`] before this is reached.
/// Whether mongod runs this window through a removable executor: a
/// `documents` or `range` window whose lower bound is not `unbounded`.
fn window_removes(window: Option<&Document>) -> bool {
    let Some(w) = window else {
        return false;
    };
    match w.get("documents").or_else(|| w.get("range")) {
        Some(Bson::Array(b)) if b.len() == 2 => {
            !matches!(&b[0], Bson::String(s) if s == "unbounded")
        }
        _ => false,
    }
}

/// A removable window function per position, moving one state along the
/// partition as mongod's removable executors do
/// (`window_function_exec_removable_{document,range}.cpp`, 8.2): the window
/// is an index interval that never moves back; the documents past its old
/// upper end are added, in order, then as many values as its lower end
/// moved are dropped from the front of the queue. A window that skips ahead
/// adds the documents it skipped and drops them again, as mongod does, and
/// an empty window drops everything.
fn removable_window(
    c: &OutField,
    slots: &[usize],
    docs: &[Document],
    range_vals: Option<&[f64]>,
    range_dates: Option<&[f64]>,
    sort_field: Option<&str>,
    vars: &Document,
) -> R<Vec<Bson>> {
    let n = slots.len();
    let mut values: Vec<Bson> = Vec::with_capacity(n);
    for &s in slots {
        values.push(if c.op == "$integral" {
            // `[sortBy value, input]`, the point mongod's `$integral` adds.
            let spec = c.arg.as_document().ok_or(Fallback::Defer)?;
            let input = spec.get("input").ok_or(Fallback::Defer)?;
            Bson::Array(vec![
                field_value(&docs[s], sort_field.ok_or(Fallback::Defer)?),
                expressions::evaluate(&docs[s], input, vars)?,
            ])
        } else {
            expressions::evaluate(&docs[s], c.arg, vars)?
        });
    }
    let mut state = match c.op {
        "$sum" => Removable::Sum(RemovableSum::new()),
        "$avg" => Removable::Avg(RemovableSum::new(), 0),
        "$covariancePop" | "$covarianceSamp" => {
            Removable::Covariance(Box::new(Covariance::new(c.op == "$covarianceSamp")))
        }
        "$integral" => Removable::Integral(Box::new(Integral::new(
            c.arg.as_document().ok_or(Fallback::Defer)?,
        )?)),
        _ => Removable::StdDev(RemovableStdDev::new(c.op == "$stdDevSamp")),
    };
    let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut last: Option<(i64, i64)> = None;
    let mut out = Vec::with_capacity(n);
    for pos in 0..n {
        let (lo, hi) = if c.window.is_some_and(|w| w.contains_key("range")) {
            range_window_bounds(pos, n, c.window.unwrap(), range_vals, range_dates)?
        } else {
            window_bounds(pos, n, c.window)?
        };
        let current = (hi >= lo).then_some((lo, hi));
        let (add, remove) = match (last, current) {
            (None, None) => (None, 0),
            (None, Some(w)) => (Some(w), 0),
            (Some((ol, ou)), None) => (None, ou - ol + 1),
            (Some((ol, ou)), Some((l, u))) => ((ou < u).then_some((ou + 1, u)), (l - ol).max(0)),
        };
        if let Some((a, b)) = add {
            for i in a..=b {
                state.update(&values[i as usize], 1)?;
                queue.push_back(i as usize);
            }
        }
        for _ in 0..remove {
            if let Some(i) = queue.pop_front() {
                state.update(&values[i], -1)?;
            }
        }
        last = current;
        out.push(state.value()?);
    }
    Ok(out)
}

enum Removable {
    StdDev(RemovableStdDev),
    Covariance(Box<Covariance>),
    Integral(Box<Integral>),
    Sum(RemovableSum),
    /// `WindowFunctionAvg`: the sum and a count of the numeric values.
    Avg(RemovableSum, i64),
}

impl Removable {
    fn update(&mut self, v: &Bson, q: i64) -> R<()> {
        match self {
            Removable::StdDev(s) => s.update(v, q),
            Removable::Covariance(s) if q > 0 => s.add(v),
            Removable::Covariance(s) => s.remove(v),
            Removable::Integral(s) if q > 0 => s.add(v),
            Removable::Integral(s) => s.remove(v),
            Removable::Sum(s) => s.update(v, q),
            Removable::Avg(s, count) => {
                if is_numeric(v) {
                    s.update(v, q)?;
                    *count += q;
                }
                Ok(())
            }
        }
    }

    fn value(&mut self) -> R<Bson> {
        match self {
            Removable::StdDev(s) => Ok(s.value()),
            Removable::Covariance(s) => s.value(),
            Removable::Integral(s) => s.value(),
            Removable::Sum(s) => s.value(),
            Removable::Avg(s, count) => {
                if *count == 0 {
                    return Ok(Bson::Null);
                }
                let n = *count;
                Ok(match s.value()? {
                    Bson::Int32(i) => Bson::Double(f64::from(i) / n as f64),
                    Bson::Int64(i) => Bson::Double(i as f64 / n as f64),
                    Bson::Double(d) if d.is_finite() => Bson::Double(d / n as f64),
                    Bson::Decimal128(d) => {
                        let x = decimal::from_bson(&Bson::Decimal128(d)).ok_or(Fallback::Defer)?;
                        if matches!(x, decimal::Dec::Fin { .. }) {
                            decimal::to_bson(&decimal::div_int(&x, n).ok_or(Fallback::Defer)?)
                                .ok_or(Fallback::Defer)?
                        } else {
                            Bson::Decimal128(d)
                        }
                    }
                    other => other,
                })
            }
        }
    }
}

/// mongod's `WindowFunctionCovariance` (`window_function_covariance.cpp`,
/// r8.2.1), transcribed: Welford's online update over two windowed means
/// and a removable sum of co-moments, with mongod's own value arithmetic.
/// A value that is not a two-number array is ignored, and NaN and the
/// infinities go straight to the co-moment sum.
struct Covariance {
    samp: bool,
    count: i64,
    mean_x: Removable,
    mean_y: Removable,
    cxy: RemovableSum,
}

impl Covariance {
    fn new(samp: bool) -> Self {
        Covariance {
            samp,
            count: 0,
            mean_x: Removable::Avg(RemovableSum::new(), 0),
            mean_y: Removable::Avg(RemovableSum::new(), 0),
            cxy: RemovableSum::new(),
        }
    }

    fn pair(v: &Bson) -> Option<(&Bson, &Bson)> {
        match v {
            Bson::Array(a) if a.len() == 2 && is_numeric(&a[0]) && is_numeric(&a[1]) => {
                Some((&a[0], &a[1]))
            }
            _ => None,
        }
    }

    fn arith(op: &str, a: &Bson, b: &Bson) -> R<Bson> {
        value_arith(op, a, b)
    }

    fn add(&mut self, v: &Bson) -> R<()> {
        let Some((x, y)) = Self::pair(v) else {
            return Ok(());
        };
        if let Some(special) = non_finite_pair(x, y) {
            return self.cxy.update(&special, 1);
        }
        self.count += 1;
        let delta_x = Self::arith("$subtract", x, &self.mean_x.value()?)?;
        self.mean_x.update(x, 1)?;
        self.mean_y.update(y, 1)?;
        let delta_y = Self::arith("$subtract", y, &self.mean_y.value()?)?;
        self.cxy
            .update(&Self::arith("$multiply", &delta_x, &delta_y)?, 1)
    }

    fn remove(&mut self, v: &Bson) -> R<()> {
        let Some((x, y)) = Self::pair(v) else {
            return Ok(());
        };
        if let Some(special) = non_finite_pair(x, y) {
            return self.cxy.update(&special, -1);
        }
        self.count -= 1;
        if self.count == 0 {
            *self = Covariance::new(self.samp);
            return Ok(());
        }
        self.mean_x.update(x, -1)?;
        let delta_x = Self::arith("$subtract", x, &self.mean_x.value()?)?;
        let delta_y = Self::arith("$subtract", y, &self.mean_y.value()?)?;
        self.cxy
            .update(&Self::arith("$multiply", &delta_x, &delta_y)?, -1)?;
        self.mean_y.update(y, -1)
    }

    fn value(&mut self) -> R<Bson> {
        if self.count == 1 && !self.samp {
            return Ok(Bson::Double(0.0));
        }
        let adjusted = if self.samp {
            self.count - 1
        } else {
            self.count
        };
        if adjusted <= 0 {
            return Ok(Bson::Null);
        }
        Ok(match self.cxy.value()? {
            Bson::Decimal128(d) => Self::arith(
                "$divide",
                &Bson::Decimal128(d),
                &Bson::Double(adjusted as f64),
            )?,
            Bson::Int32(n) => Bson::Double(f64::from(n) / adjusted as f64),
            Bson::Int64(n) => Bson::Double(n as f64 / adjusted as f64),
            Bson::Double(d) => Bson::Double(d / adjusted as f64),
            other => other,
        })
    }
}

/// `exec::expression::evaluate<op>` on two values: the engine's own
/// arithmetic, so the types (a date difference, a decimal, a null) behave as
/// mongod's do in the window functions that call it.
fn value_arith(op: &str, a: &Bson, b: &Bson) -> R<Bson> {
    let mut args = Document::new();
    args.insert(
        op,
        vec![
            Bson::Document(bson::doc! {"$literal": a.clone()}),
            Bson::Document(bson::doc! {"$literal": b.clone()}),
        ],
    );
    // Every window function runs per document; nothing here is folded.
    expressions::evaluate(&Document::new(), &Bson::Document(args), &Document::new())
        .map_err(|f| f.with_folded(false))
}

fn is_nan_value(v: &Bson) -> bool {
    crate::query::is_nan_bson(v)
}

/// mongod's `WindowFunctionIntegral` (`window_function_integral.cpp`,
/// r8.2.1), transcribed: a removable sum of trapezoids between consecutive
/// `[x, y]` points, divided by the unit's milliseconds at the end.
struct Integral {
    values: std::collections::VecDeque<Bson>,
    sum: RemovableSum,
    nan: i64,
    unit_ms: Option<i64>,
}

impl Integral {
    fn new(spec: &Document) -> R<Self> {
        let unit_ms = match spec.get("unit") {
            None => None,
            Some(Bson::String(u)) => Some(window_unit_ms(u).ok_or(Fallback::Defer)?),
            Some(_) => return Err(Fallback::Defer),
        };
        Ok(Integral {
            values: std::collections::VecDeque::new(),
            sum: RemovableSum::new(),
            nan: 0,
            unit_ms,
        })
    }

    fn point(v: &Bson) -> (&Bson, &Bson) {
        match v {
            Bson::Array(a) if a.len() == 2 => (&a[0], &a[1]),
            _ => (&Bson::Null, &Bson::Null),
        }
    }

    fn check(&self, v: &Bson) -> R<()> {
        let (x, y) = Self::point(v);
        if !(is_numeric(y) && (is_numeric(x) || matches!(x, Bson::DateTime(_)))) {
            return Err(Fallback::mongo(
                5423900,
                "The input value of $integral window function must be a vector of 2 value, the \
                 first value must be numeric or date type and the second must be numeric.",
            ));
        }
        match self.unit_ms {
            Some(_) if !matches!(x, Bson::DateTime(_)) => Err(Fallback::mongo(
                5423901,
                "$integral with 'unit' expects the sortBy field to be a Date",
            )),
            None if !is_numeric(x) => Err(Fallback::mongo(
                5423902,
                "$integral (with no 'unit') expects the sortBy field to be numeric",
            )),
            _ => Ok(()),
        }
    }

    /// `integralOfTwoPointsByTrapezoidalRule`.
    fn trapezoid(pre: &Bson, new: &Bson) -> R<Bson> {
        let (px, py) = Self::point(pre);
        let (nx, ny) = Self::point(new);
        if [px, py, nx, ny].into_iter().any(is_nan_value) {
            return Ok(Bson::Int32(0));
        }
        let both_dates = matches!(px, Bson::DateTime(_)) && matches!(nx, Bson::DateTime(_));
        if !(both_dates || (is_numeric(px) && is_numeric(nx))) {
            return Ok(Bson::Int32(0));
        }
        let delta = value_arith("$subtract", nx, px)?;
        let sum_y = value_arith("$add", ny, py)?;
        let area = value_arith("$multiply", &sum_y, &delta)?;
        value_arith("$divide", &area, &Bson::Double(2.0))
    }

    fn add(&mut self, v: &Bson) -> R<()> {
        self.check(v)?;
        let (x, y) = Self::point(v);
        if is_nan_value(x) || is_nan_value(y) {
            self.nan += 1;
        }
        if let Some(back) = self.values.back() {
            let piece = Self::trapezoid(back, v)?;
            self.sum.update(&piece, 1)?;
        }
        self.values.push_back(v.clone());
        Ok(())
    }

    fn remove(&mut self, v: &Bson) -> R<()> {
        self.check(v)?;
        let (x, y) = Self::point(v);
        if is_nan_value(x) || is_nan_value(y) {
            self.nan -= 1;
        }
        self.values.pop_front();
        if let Some(front) = self.values.front() {
            let piece = Self::trapezoid(v, front)?;
            self.sum.update(&piece, -1)?;
        }
        Ok(())
    }

    fn value(&mut self) -> R<Bson> {
        if self.values.is_empty() {
            return Ok(Bson::Null);
        }
        if self.nan > 0 {
            return Ok(Bson::Double(f64::NAN));
        }
        let total = self.sum.value()?;
        match self.unit_ms {
            Some(ms) => value_arith("$divide", &total, &Bson::Int64(ms)),
            None => Ok(total),
        }
    }
}

/// `$derivative` (`window_function_exec_derivative.cpp`, r8.2.1): rise over
/// run between the window's two ends, in mongod's value arithmetic, so a
/// date run is in milliseconds and the unit scales the quotient afterwards.
/// A zero run is null.
fn derivative_value(
    spec: &Document,
    low: i64,
    high: i64,
    slots: &[usize],
    docs: &[Document],
    sort_field: &str,
    vars: &Document,
) -> R<Bson> {
    if high < low {
        return Ok(Bson::Null);
    }
    let unit_ms = match spec.get("unit") {
        None => None,
        Some(Bson::String(u)) => Some(window_unit_ms(u).ok_or(Fallback::Defer)?),
        Some(_) => return Err(Fallback::Defer),
    };
    let input = spec.get("input").ok_or(Fallback::Defer)?;
    let (left, right) = (&docs[slots[low as usize]], &docs[slots[high as usize]]);
    let (left_time, right_time) = (
        field_value(left, sort_field),
        field_value(right, sort_field),
    );
    let dates = matches!(left_time, Bson::DateTime(_)) && matches!(right_time, Bson::DateTime(_));
    if unit_ms.is_some() && !dates {
        return Err(Fallback::mongo(
            5624900,
            "$derivative with 'unit' expects the sortBy field to be a Date",
        ));
    }
    if unit_ms.is_none() {
        if matches!(left_time, Bson::DateTime(_)) || matches!(right_time, Bson::DateTime(_)) {
            return Err(Fallback::mongo(
                5624901,
                "$derivative where the sortBy is a Date requires an 'unit'",
            ));
        }
        if !(is_numeric(&left_time) && is_numeric(&right_time)) {
            return Err(Fallback::mongo(
                5624902,
                "$derivative (with no 'unit') expects the sortBy field to be numeric",
            ));
        }
    }
    let run = value_arith("$subtract", &right_time, &left_time)?;
    let rise = value_arith(
        "$subtract",
        &expressions::evaluate(right, input, vars)?,
        &expressions::evaluate(left, input, vars)?,
    )?;
    if matches!(rise, Bson::Null | Bson::Undefined) {
        return Err(Fallback::mongo(
            5624903,
            "$derivative input must not be null or missing",
        ));
    }
    let result = match value_arith("$divide", &rise, &run) {
        Ok(v) => v,
        // A zero run is no error: a `[-1, 0]` window at a partition's start.
        Err(f) if f.as_mongo().is_some_and(|(code, _)| code == 2) => return Ok(Bson::Null),
        Err(f) => return Err(f),
    };
    match unit_ms {
        Some(ms) => value_arith("$multiply", &result, &Bson::Int64(ms)),
        None => Ok(result),
    }
}

/// `convertNonFiniteInputValue`: the one value a pair holding NaN or an
/// infinity adds to the co-moment sum. `None` for a finite pair.
fn non_finite_pair(x: &Bson, y: &Bson) -> Option<Bson> {
    let (mut pos, mut neg, mut nan, mut decimal) = (0, 0, 0, false);
    for v in [x, y] {
        match v {
            Bson::Double(d) if d.is_nan() => nan += 1,
            Bson::Double(d) if *d == f64::INFINITY => pos += 1,
            Bson::Double(d) if *d == f64::NEG_INFINITY => neg += 1,
            Bson::Decimal128(_) => {
                decimal = true;
                match decimal::from_bson(v) {
                    Some(decimal::Dec::Nan) => nan += 1,
                    Some(decimal::Dec::Inf(sign)) if sign < 0 => neg += 1,
                    Some(decimal::Dec::Inf(_)) => pos += 1,
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if nan + pos + neg == 0 {
        return None;
    }
    let special = if nan > 0 || pos * neg > 0 {
        decimal::Dec::Nan
    } else if pos > 0 {
        decimal::Dec::Inf(1)
    } else {
        decimal::Dec::Inf(-1)
    };
    Some(if decimal {
        decimal::to_bson(&special).unwrap_or(Bson::Double(f64::NAN))
    } else {
        Bson::Double(match special {
            decimal::Dec::Inf(1) => f64::INFINITY,
            decimal::Dec::Inf(_) => f64::NEG_INFINITY,
            _ => f64::NAN,
        })
    })
}

fn is_numeric(v: &Bson) -> bool {
    matches!(
        v,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
    )
}

/// mongod's `RemovableSum` (`window_function_sum.{h,cpp}`, 8.2): an
/// `AccumulatorSum` that a removed value is added to negated, with NaN and
/// the infinities counted apart, and the answer narrowed back to the types
/// still in the window (the sum's own type only ever widens).
struct RemovableSum {
    sum: group::Num,
    pos_inf: i64,
    neg_inf: i64,
    nan: i64,
    doubles: i64,
    decimals: i64,
}

impl RemovableSum {
    fn new() -> Self {
        RemovableSum {
            sum: group::Num::zero(),
            pos_inf: 0,
            neg_inf: 0,
            nan: 0,
            doubles: 0,
            decimals: 0,
        }
    }

    fn process(&mut self, v: &Bson) -> R<()> {
        self.sum = std::mem::replace(&mut self.sum, group::Num::zero()).add(v)?;
        Ok(())
    }

    fn update(&mut self, v: &Bson, q: i64) -> R<()> {
        match v {
            Bson::Int32(i) => {
                if *i == i32::MIN && q == -1 {
                    self.process(&Bson::Int32(i32::MAX))?;
                    self.process(&Bson::Int32(1))
                } else {
                    self.process(&Bson::Int32(i * q as i32))
                }
            }
            Bson::Int64(i) => {
                if *i == i64::MIN && q == -1 {
                    self.process(&Bson::Int64(i64::MAX))?;
                    self.process(&Bson::Int32(1))
                } else {
                    self.process(&Bson::Int64(i * q))
                }
            }
            Bson::Double(d) => {
                self.doubles += q;
                if d.is_nan() {
                    self.nan += q;
                } else if *d == f64::INFINITY {
                    self.pos_inf += q;
                } else if *d == f64::NEG_INFINITY {
                    self.neg_inf += q;
                } else {
                    self.process(&Bson::Double(d * q as f64))?;
                }
                Ok(())
            }
            Bson::Decimal128(_) => {
                self.decimals += q;
                match decimal::from_bson(v).ok_or(Fallback::Defer)? {
                    decimal::Dec::Nan => self.nan += q,
                    decimal::Dec::Inf(sign) if sign > 0 => self.pos_inf += q,
                    decimal::Dec::Inf(_) => self.neg_inf += q,
                    d => {
                        let d = if q == -1 { decimal::neg(&d) } else { d };
                        self.process(&decimal::to_bson(&d).ok_or(Fallback::Defer)?)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn value(&self) -> R<Bson> {
        let special = |dec: &str, dbl: f64| -> R<Bson> {
            Ok(if self.decimals > 0 {
                decimal::to_bson(&decimal::parse(dec).ok_or(Fallback::Defer)?)
                    .ok_or(Fallback::Defer)?
            } else {
                Bson::Double(dbl)
            })
        };
        if self.nan > 0 || (self.pos_inf > 0 && self.neg_inf > 0) {
            return special("NaN", f64::NAN);
        }
        if self.pos_inf > 0 {
            return special("Infinity", f64::INFINITY);
        }
        if self.neg_inf > 0 {
            return special("-Infinity", f64::NEG_INFINITY);
        }
        let int_or_long = |v: i64| i32::try_from(v).map_or(Bson::Int64(v), Bson::Int32);
        Ok(match self.sum.clone().into_bson()? {
            Bson::Decimal128(d) if self.decimals == 0 => {
                let x = decimal::from_bson(&Bson::Decimal128(d)).ok_or(Fallback::Defer)?;
                let as_double = || -> R<Bson> {
                    Ok(Bson::Double(
                        d.to_string().parse::<f64>().map_err(|_| Fallback::Defer)?,
                    ))
                };
                if self.doubles > 0 {
                    as_double()?
                } else if let Some(v) = integral_i64(&x) {
                    int_or_long(v)
                } else {
                    as_double()?
                }
            }
            Bson::Double(d) if self.doubles == 0 && d >= i64::MIN as f64 && d < i64::MAX as f64 => {
                int_or_long(d.round() as i64)
            }
            Bson::Int64(v) => int_or_long(v),
            other => other,
        })
    }
}

/// `Decimal128::toLong` without a signalling flag: an integral decimal in
/// `i64`'s range, `i64::MIN` included.
fn integral_i64(d: &decimal::Dec) -> Option<i64> {
    let decimal::Dec::Fin { sign, coeff, exp } = d else {
        return None;
    };
    if !decimal::is_integral(d) {
        return None;
    }
    let mut acc: i128 = 0;
    let digits = coeff.len() as i64 + i64::from(*exp);
    if digits > 20 {
        return coeff.iter().all(|&c| c == 0).then_some(0);
    }
    for (i, &c) in coeff.iter().enumerate() {
        // Digits a negative exponent drops are zeros (the value is integral).
        if (i as i64) < coeff.len() as i64 + i64::from((*exp).min(0)) {
            acc = acc * 10 + i128::from(c);
        }
    }
    for _ in 0..(*exp).max(0) {
        acc *= 10;
    }
    i64::try_from(acc * i128::from(*sign)).ok()
}

/// mongod's `WindowFunctionStdDev` (`window_function_stddev.h`, 8.2): a
/// running sum and `m2`, both compensated sums, updated by Welford's step
/// in the count-scaled form. On arm64 mongod's `count * x - sum` is a fused
/// multiply-add, and so is this one.
struct RemovableStdDev {
    sum: group::Num,
    m2: DoubleDoubleSum,
    count: i64,
    nonfinite: i64,
    samp: bool,
}

impl RemovableStdDev {
    fn new(samp: bool) -> Self {
        RemovableStdDev {
            sum: group::Num::zero(),
            m2: DoubleDoubleSum::default(),
            count: 0,
            nonfinite: 0,
            samp,
        }
    }

    fn update(&mut self, v: &Bson, q: i64) -> R<()> {
        let x = match v {
            Bson::Int32(i) => f64::from(*i),
            Bson::Int64(i) => *i as f64,
            Bson::Double(d) => *d,
            // `coerceToDouble`: the nearest double.
            Bson::Decimal128(d) => d.to_string().parse::<f64>().map_err(|_| Fallback::Defer)?,
            _ => return Ok(()),
        };
        if !x.is_finite() {
            self.nonfinite += q;
            return Ok(());
        }
        if self.count == 0 {
            self.count = 1;
            self.sum = std::mem::replace(&mut self.sum, group::Num::zero()).add(v)?;
            return Ok(());
        }
        if self.count + q == 0 {
            // mongod's `reset()`, which also forgets the non-finite values
            // still in the window: their later removals take the count
            // below zero, and the window answers a number again.
            self.sum = group::Num::zero();
            self.m2 = DoubleDoubleSum::default();
            self.count = 0;
            self.nonfinite = 0;
            return Ok(());
        }
        let sum = match self.sum.clone().into_bson()? {
            Bson::Int32(i) => f64::from(i),
            Bson::Int64(i) => i as f64,
            Bson::Double(d) => d,
            Bson::Decimal128(d) => d.to_string().parse::<f64>().map_err(|_| Fallback::Defer)?,
            _ => return Err(Fallback::Defer),
        };
        // Two roundings, as mongod's Linux x86-64 build computes it -- the
        // exemplar (Joe, 2026-10-21). Its macOS arm64 build fuses this into
        // one rounding (an FMA), and the last digit differs (RM-231).
        #[allow(clippy::suboptimal_flops)]
        let delta = self.count as f64 * x - sum;
        self.count += q;
        let qf = q as f64;
        self.sum =
            std::mem::replace(&mut self.sum, group::Num::zero()).add(&Bson::Double(x * qf))?;
        self.m2
            .add_double(delta * delta * qf / (self.count * (self.count - q)) as f64);
        Ok(())
    }

    fn value(&mut self) -> Bson {
        // A window holding a NaN or an infinity has no deviation.
        if self.nonfinite > 0 {
            return Bson::Null;
        }
        let adjusted = if self.samp {
            self.count - 1
        } else {
            self.count
        };
        if adjusted <= 0 {
            return Bson::Null;
        }
        let m2 = self.m2.get_double();
        if m2 < 0.0 || (!self.samp && self.count == 1) {
            // mongod resets a negative `m2` (rounding) and answers int 0.
            self.m2 = DoubleDoubleSum::default();
            return Bson::Int32(0);
        }
        Bson::Double((m2 / adjusted as f64).sqrt())
    }
}

fn window_bounds(slot: usize, n: usize, window: Option<&Document>) -> R<(i64, i64)> {
    let last = n as i64 - 1;
    let bounds = match window.and_then(|w| w.get("documents")) {
        None => return Ok((0, last)),
        Some(Bson::Array(b)) if b.len() == 2 => b,
        Some(_) => return Err(Fallback::Defer),
    };
    let resolve = |b: &Bson, is_lower: bool| -> R<i64> {
        match b {
            Bson::String(s) if s == "unbounded" => Ok(if is_lower { 0 } else { last }),
            Bson::String(s) if s == "current" => Ok(slot as i64),
            Bson::Int32(i) => Ok(slot as i64 + *i as i64),
            Bson::Int64(i) => Ok(slot as i64 + *i),
            _ => Err(Fallback::Defer), // bool / non-int / other string → Python raises
        }
    };
    let low = resolve(&bounds[0], true)?.max(0);
    let high = resolve(&bounds[1], false)?.min(last);
    Ok((low, high))
}

/// `$shift` — the `output` expression evaluated on the row `by` positions away in
/// the sorted partition, or `default` (a constant, evaluated on the current row)
/// / `null` when that position is out of the partition, or when `output`
/// evaluates to MISSING there: mongod 8.2.11 answers the default for a shifted
/// row whose `$x` is absent, and keeps an explicit `null` (RM-226).
fn shift_value(
    spec: &Document,
    pos: usize,
    slots: &[usize],
    docs: &[Document],
    vars: &Document,
) -> R<Bson> {
    let by = match spec.get("by") {
        Some(Bson::Int32(n)) => *n as i64,
        Some(Bson::Int64(n)) => *n,
        _ => return Err(Fallback::Defer),
    };
    let output = spec.get("output").ok_or(Fallback::Defer)?;
    let idx = pos as i64 + by;
    if idx >= 0 && (idx as usize) < slots.len() {
        let v = expressions::evaluate_or_missing(&docs[slots[idx as usize]], output, vars)?;
        if !matches!(v, Bson::Undefined) {
            return Ok(v);
        }
    }
    match spec.get("default") {
        Some(default) => expressions::evaluate(&docs[slots[pos]], default, vars),
        None => Ok(Bson::Null),
    }
}

/// `$expMovingAvg` smoothing factor: `2/(N+1)` from a positive-int `N`, or a
/// given `alpha` in (0, 1). Exactly one must be present, else `Err(Fallback::Defer)` (defer).
fn ema_alpha(spec: &Document) -> R<f64> {
    let has_n = spec.contains_key("N");
    let has_alpha = spec.contains_key("alpha");
    if has_n == has_alpha {
        return Err(Fallback::Defer);
    }
    if has_n {
        let n = match spec.get("N") {
            Some(Bson::Int32(n)) if *n >= 1 => *n as f64,
            Some(Bson::Int64(n)) if *n >= 1 => *n as f64,
            _ => return Err(Fallback::Defer),
        };
        Ok(2.0 / (n + 1.0))
    } else {
        match spec.get("alpha") {
            Some(Bson::Double(a)) if *a > 0.0 && *a < 1.0 => Ok(*a),
            _ => Err(Fallback::Defer), // int alpha can't be in (0,1); non-number defers
        }
    }
}

/// Per-slot exponential moving average over the sorted partition, mirroring
/// `_compute_ema`: `ema[0] = input[0]`; `ema[i] = input[i]*alpha +
/// ema[i-1]*(1-alpha)`. Same IEEE-double ops as the oracle → bit-for-bit match.
fn ema_values(slots: &[usize], docs: &[Document], spec: &Document, vars: &Document) -> R<Vec<f64>> {
    let alpha = ema_alpha(spec)?;
    let input = spec.get("input").ok_or(Fallback::Defer)?;
    let mut out = Vec::with_capacity(slots.len());
    let mut prev: Option<f64> = None;
    for &i in slots {
        let v = expressions::evaluate(&docs[i], input, vars)?;
        let x = as_number(&v).ok_or(Fallback::Defer)?; // non-numeric input -> defer
        let ema = match prev {
            None => x,
            Some(p) => x * alpha + p * (1.0 - alpha),
        };
        out.push(ema);
        prev = Some(ema);
    }
    Ok(out)
}

/// Per-slot output vector for the prefix/gap-fill window operators, mirroring
/// `_compute_window_vector`. `$expMovingAvg` needs `arg` as a `{input, N|alpha}`
/// doc; `$locf` / `$linearFill` take `arg` as the input expression directly.
fn window_vector(
    op: &str,
    slots: &[usize],
    docs: &[Document],
    arg: &Bson,
    sort_field: Option<&str>,
    vars: &Document,
) -> R<Vec<Bson>> {
    match op {
        "$expMovingAvg" => {
            Ok(
                ema_values(slots, docs, arg.as_document().ok_or(Fallback::Defer)?, vars)?
                    .into_iter()
                    .map(Bson::Double)
                    .collect(),
            )
        }
        "$locf" => locf_values(slots, docs, arg, vars),
        "$linearFill" => {
            linear_fill_values(slots, docs, arg, sort_field.ok_or(Fallback::Defer)?, vars)
        }
        _ => Err(Fallback::Defer),
    }
}

/// `$locf` — last observation of `expr` carried forward over the sorted
/// partition; leading nulls (before the first non-null) stay null.
fn locf_values(slots: &[usize], docs: &[Document], expr: &Bson, vars: &Document) -> R<Vec<Bson>> {
    let mut out = Vec::with_capacity(slots.len());
    let mut last: Option<Bson> = None;
    for &i in slots {
        let v = expressions::evaluate(&docs[i], expr, vars)?;
        if matches!(v, Bson::Null) {
            out.push(last.clone().unwrap_or(Bson::Null));
        } else {
            last = Some(v.clone());
            out.push(v);
        }
    }
    Ok(out)
}

/// `$linearFill` (`window_function_exec_linear_fill.cpp`, r8.2.1): each
/// null between two known points is `y1 + (x - x1) * ((y2 - y1) / (x2 - x1))`
/// in mongod's value arithmetic, so a date sortBy interpolates by its
/// millisecond differences. Leading and trailing nulls stay null.
fn linear_fill_values(
    slots: &[usize],
    docs: &[Document],
    expr: &Bson,
    sort_field: &str,
    vars: &Document,
) -> R<Vec<Bson>> {
    let mut ys: Vec<Bson> = Vec::with_capacity(slots.len());
    let mut xs: Vec<Bson> = Vec::with_capacity(slots.len());
    let mut last: Option<Bson> = None;
    let datelike = |v: &Bson| {
        matches!(
            v,
            Bson::DateTime(_) | Bson::Timestamp(_) | Bson::ObjectId(_)
        )
    };
    for &i in slots {
        let y = expressions::evaluate(&docs[i], expr, vars)?;
        if !(is_numeric(&y) || matches!(y, Bson::Null | Bson::Undefined)) {
            return Err(Fallback::mongo(
                14,
                format!(
                    "Value to be filled must be numeric or nullish, but found {}",
                    crate::query::bson_type_name(&y)
                ),
            ));
        }
        let x = field_value(&docs[i], sort_field);
        if !(is_numeric(&x) || datelike(&x)) {
            return Err(Fallback::mongo(
                14,
                format!(
                    "Value of the sortBy field must be numeric or a date, but found {}",
                    crate::query::bson_type_name(&x)
                ),
            ));
        }
        if let Some(prev) = &last {
            if order::cmp(prev, &x) == Ordering::Equal {
                return Err(Fallback::mongo(
                    6050106,
                    "There can be no repeated values in the sort field in the same partition",
                ));
            }
            if !((datelike(prev) && datelike(&x)) || (is_numeric(prev) && is_numeric(&x))) {
                return Err(Fallback::mongo(
                    14,
                    format!(
                        "Conflicting sort value types, previously received type {}, but found {}",
                        crate::query::bson_type_name(prev),
                        crate::query::bson_type_name(&x)
                    ),
                ));
            }
        }
        last = Some(x.clone());
        xs.push(x);
        ys.push(y);
    }
    let known: Vec<usize> = (0..ys.len())
        .filter(|&i| !matches!(ys[i], Bson::Null | Bson::Undefined))
        .collect();
    let mut out = ys.clone();
    for w in known.windows(2) {
        let (a, b) = (w[0], w[1]);
        for i in (a + 1)..b {
            let slope = value_arith(
                "$divide",
                &value_arith("$subtract", &ys[b], &ys[a])?,
                &value_arith("$subtract", &xs[b], &xs[a])?,
            )?;
            let offset = value_arith(
                "$multiply",
                &value_arith("$subtract", &xs[i], &xs[a])?,
                &slope,
            )?;
            out[i] = value_arith("$add", &ys[a], &offset)?;
        }
    }
    // A null that no known point bounds stays null, a missing one included.
    for v in out.iter_mut() {
        if matches!(v, Bson::Undefined) {
            *v = Bson::Null;
        }
    }
    Ok(out)
}

/// A numeric BSON value as `f64` (int / long / double, never bool / Decimal128 —
/// matching the pure `_is_number`). `None` otherwise.
fn as_number(v: &Bson) -> Option<f64> {
    match v {
        Bson::Int32(n) => Some(*n as f64),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(d) => Some(*d),
        _ => None,
    }
}

/// Fixed-duration `unit` → milliseconds, for `range` windows with a time `unit`.
/// Variable-length `month` / `quarter` / `year` (and any unknown unit) → `None`,
/// which makes the range window defer to Python. Mirrors `_WINDOW_UNIT_MS`.
fn window_unit_ms(unit: &str) -> Option<i64> {
    match unit {
        "week" => Some(604_800_000),
        "day" => Some(86_400_000),
        "hour" => Some(3_600_000),
        "minute" => Some(60_000),
        "second" => Some(1_000),
        "millisecond" => Some(1),
        _ => None,
    }
}

/// The per-slot sortBy values for a range window: `Some` only for a single
/// **ascending** sort field whose values are all numeric (the shape range
/// windows support); anything else → `None`, which makes a range window defer.
fn range_values(
    slots: &[usize],
    docs: &[Document],
    sort_by: Option<&Document>,
) -> Option<Vec<f64>> {
    let sb = sort_by?;
    if sb.len() != 1 {
        return None;
    }
    let (field, dir) = sb.iter().next().unwrap();
    if !matches!(dir, Bson::Int32(1) | Bson::Int64(1)) {
        return None; // ascending only
    }
    slots
        .iter()
        .map(|&i| as_number(&field_value(&docs[i], field)))
        .collect()
}

/// The per-slot epoch-millis of a single **ascending** `DateTime` sortBy field —
/// the x-axis a time-`unit` range window offsets against. `None` unless the sort
/// is a single ascending field whose values are *all* dates. Mirrors the Python
/// `range_date_ms` branch; kept separate from `range_values` (raw dates) so the
/// non-range window ops keep deferring on a non-numeric sortBy.
fn range_dates(slots: &[usize], docs: &[Document], sort_by: Option<&Document>) -> Option<Vec<f64>> {
    let sb = sort_by?;
    if sb.len() != 1 {
        return None;
    }
    let (field, dir) = sb.iter().next().unwrap();
    if !matches!(dir, Bson::Int32(1) | Bson::Int64(1)) {
        return None; // ascending only
    }
    let raw: Vec<Bson> = slots
        .iter()
        .map(|&i| field_value(&docs[i], field))
        .collect();
    if raw.is_empty() || !raw.iter().all(|v| matches!(v, Bson::DateTime(_))) {
        return None;
    }
    raw.iter()
        .map(|v| match v {
            Bson::DateTime(dt) => Some(dt.timestamp_millis() as f64),
            _ => None,
        })
        .collect()
}

/// Inclusive `(low, high)` value-window indices: rows whose (ascending) sortBy
/// value falls in `[cur + lo, cur + hi]`. Bounds: `"unbounded"` → open,
/// `"current"` → `cur`, numeric offset → `cur + b * unit_ms`. A fixed-duration
/// time `unit` scales the offset and pins the x-axis to the date sortBy's epoch
/// millis (`range_dates`). mongod's rule holds both ways: `unit` requires a date
/// sortBy, a date sortBy requires `unit`. A variable-length `unit`
/// (month/quarter/year), a missing single-ascending sort, or a non-numeric bound
/// → defer.
fn range_window_bounds(
    slot: usize,
    n: usize,
    window: &Document,
    range_vals: Option<&[f64]>,
    range_dates: Option<&[f64]>,
) -> R<(i64, i64)> {
    let (unit_ms, vals): (f64, &[f64]) = match window.get("unit") {
        Some(Bson::String(u)) => {
            // `unit` requires a date sortBy.
            (
                window_unit_ms(u).ok_or(Fallback::Defer)? as f64,
                range_dates.ok_or(Fallback::Defer)?,
            )
        }
        Some(_) => return Err(Fallback::Defer), // non-string unit → defer
        None => {
            // A date sortBy requires a `unit`.
            if range_dates.is_some() {
                return Err(Fallback::Defer);
            }
            (1.0, range_vals.ok_or(Fallback::Defer)?)
        }
    };
    let bounds = match window.get("range") {
        Some(Bson::Array(b)) if b.len() == 2 => b,
        _ => return Err(Fallback::Defer),
    };
    let cur = vals[slot];
    let edge = |b: &Bson| -> R<Option<f64>> {
        match b {
            Bson::String(s) if s == "unbounded" => Ok(None),
            Bson::String(s) if s == "current" => Ok(Some(cur)),
            _ => as_number(b)
                .map(|x| Some(cur + x * unit_ms))
                .ok_or(Fallback::Defer),
        }
    };
    let lo = edge(&bounds[0])?;
    let hi = edge(&bounds[1])?;
    let mut low = 0i64;
    while (low as usize) < n && lo.is_some_and(|l| vals[low as usize] < l) {
        low += 1;
    }
    let mut high = n as i64 - 1;
    while high >= 0 && hi.is_some_and(|h| vals[high as usize] > h) {
        high -= 1;
    }
    Ok((low, high))
}
