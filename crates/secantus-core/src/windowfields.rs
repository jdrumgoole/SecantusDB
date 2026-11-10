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
    set_window_fields_each(spec, docs, vars, None, None, &mut |d| {
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
///
/// `reads`: when given, how many of `docs` mongod's partition iterator has
/// read, in the order the stage takes them, once it has answered each
/// output (RM-326).
pub(crate) fn set_window_fields_each(
    spec: &Bson,
    docs: Vec<Document>,
    vars: &Document,
    downstream: Option<&std::collections::BTreeSet<String>>,
    reads: Option<&mut Vec<usize>>,
    sink: &mut dyn FnMut(Document) -> R<()>,
) -> R<()> {
    window_fields_each(spec, docs, vars, downstream, false, reads, sink)
}

/// The refusal `$setWindowFields` (or, with `presorted`,
/// `$_internalSetWindowFields`) makes from its spec alone, or `None`.
///
/// mongod makes these when it parses the stage, so they come before anything
/// a document can raise -- before the `$sort` the stage runs behind refuses a
/// document with parallel arrays (`$documentNumber` over a two-field
/// `sortBy` is 5371602 whatever the documents; measured 8.2.1, 2026-11-08,
/// RM-332). The stage is run over no documents, and only a refusal made
/// without one (a `bare` one) is kept: a shape this server defers on is the
/// stage's to report when it runs.
pub fn set_window_fields_spec_refusal(spec: &Bson, presorted: bool) -> Option<Fallback> {
    match window_fields_each(
        spec,
        Vec::new(),
        &Document::new(),
        None,
        presorted,
        None,
        &mut |_| Ok(()),
    ) {
        Err(f) if f.is_bare() => Some(f),
        _ => None,
    }
}

/// `$_internalSetWindowFields`, the stage mongod 8.2 parses
/// `$setWindowFields` into behind a `$sort` on its partition and `sortBy`
/// (and runs when a pipeline names it): the documents are taken in the
/// order they come, and a partition is a run of them with one key -- a key
/// seen again after another starts a new partition (measured 8.2.1,
/// 2026-11-08, RM-328).
pub(crate) fn internal_set_window_fields_each(
    spec: &Bson,
    docs: Vec<Document>,
    vars: &Document,
    downstream: Option<&std::collections::BTreeSet<String>>,
    reads: Option<&mut Vec<usize>>,
    sink: &mut dyn FnMut(Document) -> R<()>,
) -> R<()> {
    window_fields_each(spec, docs, vars, downstream, true, reads, sink)
}

fn window_fields_each(
    spec: &Bson,
    docs: Vec<Document>,
    vars: &Document,
    downstream: Option<&std::collections::BTreeSet<String>>,
    presorted: bool,
    mut reads: Option<&mut Vec<usize>>,
    sink: &mut dyn FnMut(Document) -> R<()>,
) -> R<()> {
    let spec = spec.as_document().ok_or(Fallback::Defer)?;
    let partition_by = spec.get("partitionBy");
    let sort_by = match spec.get("sortBy") {
        None => None,
        Some(Bson::Document(d)) => Some(d),
        // Measured 8.2.11, 2026-11-06: a non-document `sortBy` is refused by
        // the command parser, naming the type it got.
        Some(other) => {
            return Err(Fallback::mongo(
                14,
                format!(
                    "BSON field '{}.sortBy' is the wrong type '{}', expected type 'object'",
                    if presorted {
                        "$_internalSetWindowFields"
                    } else {
                        "$setWindowFields"
                    },
                    crate::query::bson_type_name(other)
                ),
            )
            .bare())
        }
    };
    // An EMPTY `output` is accepted: mongod answers the partition's documents
    // unchanged, in sortBy order when there is one (measured 8.2.11,
    // 2026-11-06), so it runs the ordinary machinery with no output fields.
    let output = spec
        .get("output")
        .and_then(Bson::as_document)
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
                // Measured 8.2.11, 2026-11-06.
                return Err(Fallback::mongo(
                    9,
                    "Cannot specify two functions in window function spec",
                )
                .bare());
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
            // Measured 8.2.1 and 8.2.12, 2026-11-08: all three read the sort
            // key, and mongod refuses one without a single-field `sortBy`
            // before it reads a document.
            if sort_by.is_none_or(|sb| sb.len() != 1) {
                return Err(Fallback::mongo(
                    5371602,
                    format!(
                        "{op} must be specified with a top level sortBy expression with \
                         exactly one element"
                    ),
                )
                .bare());
            }
        } else if op == "$shift" {
            // Position-based (like the rank funcs): requires a sortBy, no window,
            // and an `{output, by: <int>, default?}` spec.
            let spec = arg.as_document().ok_or(Fallback::Defer)?;
            // Measured 8.2.11, 2026-11-06: the `window` refusal comes first --
            // a spec with both a window and no sortBy names the window.
            if window.is_some() {
                return Err(Fallback::mongo(9, "$shift does not accept a 'window' field").bare());
            }
            if sort_by.is_none() {
                return Err(Fallback::mongo(9, "'$shift' requires a sortBy").bare());
            }
            if !spec.contains_key("output") {
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
            // No window ($linearFill's x-axis is checked when the vector is
            // built). Measured 8.2.11, 2026-11-06: the `window` refusal comes
            // before the sortBy one, and $linearFill -- unlike $locf -- needs a
            // top-level sortBy of exactly one element.
            if window.is_some() {
                return Err(
                    Fallback::mongo(9, format!("'window' field is not allowed in {op}")).bare(),
                );
            }
            if op == "$linearFill" && sort_by.is_none_or(|sb| sb.len() != 1) {
                return Err(Fallback::mongo(
                    605001,
                    "$linearFill must be specified with a top level sortBy expression with \
                     exactly one element",
                )
                .bare());
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
        } else if op == "$mergeObjects" || op == "$accumulator" {
            // $group-only accumulators. Measured 8.2.11, 2026-11-06: mongod
            // refuses them as window functions by name, as it does an unknown
            // one.
            return Err(Fallback::mongo(9, format!("Unrecognized window function, {op}")).bare());
        } else if !group::is_accumulator(op) {
            // Not an accumulator at all, so not a window function either --
            // mongod names it (measured 8.2.11, 2026-11-06: every accumulator
            // but $mergeObjects and $accumulator IS a window function).
            return Err(Fallback::mongo(9, format!("Unrecognized window function, {op}")).bare());
        } else {
            group::new_acc(op)?; // an accumulator this server cannot run yet → defer
        }
        compiled.push(OutField {
            field,
            op,
            arg,
            window,
        });
    }

    // Range-based bounds need one ascending sortBy field, and mongod says so
    // before it reads a document: both refusals fire on an empty collection
    // (measured 8.2.11, 2026-11-06).
    if compiled
        .iter()
        .any(|c| c.window.is_some_and(|w| w.contains_key("range")))
    {
        match sort_by {
            None => return Err(Fallback::Defer),
            Some(sb) if sb.len() != 1 => {
                return Err(Fallback::mongo(
                    5339902,
                    "Range-based bounds require sortBy a single field",
                )
                .bare())
            }
            Some(sb) => {
                let (_, dir) = sb.iter().next().unwrap();
                if !matches!(dir, Bson::Int32(1) | Bson::Int64(1)) {
                    return Err(Fallback::mongo(
                        8947401,
                        "Range-based bounds require an ascending sortBy",
                    )
                    .bare());
                }
            }
        }
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
    let mut last_key: Option<Vec<u8>> = None;
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
        let key = crate::bsondoc::to_vec(&wrap).map_err(|_| Fallback::Defer)?;
        if presorted {
            // A partition ends where the key changes.
            if last_key.as_ref() != Some(&key) {
                partitions.push(Vec::new());
                keys.push(key_val);
                last_key = Some(key);
            }
            partitions.last_mut().expect("pushed above").push(i);
            continue;
        }
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
    if !presorted && keys.iter().all(order::is_comparable) {
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
    let sizer = match sort_by {
        Some(sort_by) if needs_rank => CacheSize::SortKey {
            partition_by,
            sort_by,
            vars,
        },
        _ => CacheSize::Fields(cached.as_ref()),
    };
    let mut spill = CacheSpill::default();

    // mongod emits PARTITION BY PARTITION, in partition-key order (above),
    // and within each partition in `sortBy` order -- not the input order.
    // Wrong order is wrong RESULTS as soon as a `$limit` follows, so this is
    // not cosmetic. `sorted_slots` yields exactly that sequence.
    let ordered = partitions
        .iter()
        .map(|part| sorted_slots(part, &docs, sort_by.filter(|_| !presorted)))
        .collect::<R<Vec<_>>>()?;
    let mut stats = WindowMemory {
        functions: vec![0; compiled.len()],
        total: 0,
    };
    // The documents of the partitions before this one, and the most the
    // partition iterator has read so far (RM-326).
    let (mut base, mut read) = (0usize, 0usize);
    for (pi, slots) in ordered.iter().enumerate() {
        let token = match partition_by {
            Some(by) => {
                let key = |slots: &[usize]| {
                    crate::memlimit::classic_size(&partition_key(&docs[slots[0]], by, vars))
                };
                PartitionToken {
                    key: slots.first().map_or(0, |_| key(slots)),
                    next: ordered
                        .get(pi + 1)
                        .filter(|next| !next.is_empty())
                        .map(|next| (sizer.of(&docs[next[0]]), key(next))),
                }
            }
            None => PartitionToken::default(),
        };
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
        // A plain (unitless) range window reads its bounds off the sortBy
        // value, and mongod refuses a non-numeric one per partition, naming
        // the type it found: `Invalid range: Expected the sortBy field to be a
        // number, but it was null` under the executor wrapper (measured 8.2.11,
        // 2026-11-06 -- an empty collection answers nothing at all, so this is
        // execution-time, not parse-time).
        if range_vals.is_none()
            && range_dates.is_none()
            && compiled.iter().any(|c| {
                c.window
                    .is_some_and(|w| w.contains_key("range") && !w.contains_key("unit"))
            })
        {
            if let Some(sb) = sort_by.filter(|sb| sb.len() == 1) {
                let (field, _) = sb.iter().next().unwrap();
                if let Some(bad) = slots
                    .iter()
                    .map(|&i| field_value(&docs[i], field))
                    .find(|v| as_number(v).is_none())
                {
                    return Err(Fallback::mongo(
                        5429414,
                        format!(
                            "Invalid range: Expected the sortBy field to be a number, but it \
                             was {}",
                            crate::query::bson_type_name(&bad)
                        ),
                    )
                    .exec());
                }
            }
        }
        // mongod's partition cache over this partition: held to its budget,
        // spilled or refused (RM-211).
        run_cache(
            &compiled,
            &slots,
            &docs,
            &sizer,
            ranks.as_ref(),
            token,
            range_vals.as_deref(),
            range_dates.as_deref(),
            vars,
            &mut spill,
            &mut stats,
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
            if let Some(reads) = reads.as_deref_mut() {
                // mongod's iterator reads what each function's window asks
                // for, the next partition's first document when one runs
                // off the end, and, as it advances past the document it
                // answered, the next one (measured 8.2.11, 2026-11-10: a
                // running total read 33 for a batch of 32, a `[-1, 2]`
                // window 33 for 31, a partition's total its partition and
                // one more).
                let mut asked = pos as i64 + 1;
                for c in &compiled {
                    let (_, hi) = reach(c, pos, n, range_vals.as_deref(), range_dates.as_deref())?;
                    asked = asked.max(hi);
                }
                let asked = usize::try_from(asked).unwrap_or(0).min(n);
                read = read.max(base + asked + 1);
                reads.push(read);
            }
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
        base += n;
    }
    let wide = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
    crate::memlimit::record_window(|| crate::memlimit::WindowStats {
        max_function_memory: compiled
            .iter()
            .zip(&stats.functions)
            .map(|(c, m)| (c.field.to_string(), wide(*m)))
            .collect(),
        max_total_memory: wide(stats.total),
        spills: spill.spills,
        spilled_records: spill.records,
        spilled_bytes: spill.bytes,
        spilled_data_storage_size: spill.stored,
    });
    spill.finish()
}

/// The most a stage's functions, and the stage as a whole, held at once
/// (`maxFunctionMemoryUsageBytes`, `maxTotalMemoryUsageBytes`; RM-328).
struct WindowMemory {
    functions: Vec<usize>,
    total: usize,
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
/// (of `n`): the first it must still hold, and the last index it asks for --
/// past `n - 1` when the window runs off the partition's end, which makes
/// the iterator read the next partition's first document to find that end.
/// Measured on mongod 8.2.11 (2026-10-26, `maxTotalMemoryUsageBytes` and
/// the budget bisected over a hundred documents): a window that drops
/// documents needs the one answered and those its upper end reaches
/// (`[-5, 5]`: six); `$first` / `$last` and a `range` window hold their
/// whole window and the document leaving it, and a `range` window reads
/// one past its end; a window with no upper end reads the partition and
/// past it; `$shift` back holds the document leaving its endpoint, as
/// `$first` does. A rank function's window is
/// `["unbounded", "current"]`: it reads only the document it answers
/// (`ExpressionFromRankAccumulator`; 8.2.1 and 8.2.12, RM-327).
fn reach(
    c: &OutField,
    pos: usize,
    n: usize,
    range_vals: Option<&[f64]>,
    range_dates: Option<&[f64]>,
) -> R<(usize, i64)> {
    let last = (n - 1) as i64;
    let p = pos as i64;
    let clamp = |i: i64| i.clamp(0, last) as usize;
    if RANK_FUNCS.contains(&c.op) || matches!(c.op, "$expMovingAvg" | "$locf") {
        return Ok((pos, p));
    }
    if c.op == "$linearFill" {
        return Ok((pos, last));
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
        // `$shift` reads its endpoint (`[by, by]`); back, it holds the
        // document leaving it too, as `$first` does (`kEndpoints` expires
        // what lies left of the endpoint it found the time before).
        return Ok(if by > 0 {
            (pos, p + by)
        } else {
            (clamp(p + by - 1), p)
        });
    }
    let Some(w) = c.window else {
        return Ok((pos, last + 1));
    };
    let range = w.contains_key("range");
    let (lo, hi) = if range {
        range_window_bounds(pos, n, w, range_vals, range_dates)?
    } else {
        window_bounds(pos, n, Some(w))?
    };
    let upper = w
        .get("documents")
        .or_else(|| w.get("range"))
        .and_then(Bson::as_array)
        .and_then(|b| b.get(1));
    let current = matches!(upper, Some(Bson::String(s)) if s == "current");
    let hi = match upper {
        Some(Bson::String(s)) if s == "unbounded" => last + 1,
        // `"current"` is the document answered: a `range` window looks no
        // further for its end (`getEndpointsRangeBased`; RM-404).
        _ if current => p,
        _ if range => hi + 1,
        // A document window asks for its upper bound, which `window_bounds`
        // clamps to the partition.
        Some(Bson::Int32(k)) => p + i64::from(*k),
        Some(Bson::Int64(k)) => p + k,
        _ => hi,
    };
    // A window with no lower end runs an accumulator that never lets a value
    // go (`WindowFunctionExecNonRemovable[Range]`; `$first`, `$last` and
    // `$derivative` run their own executors), so it holds only what it has
    // still to read. A `documents` one reads its upper end and lets go of
    // what lies left of it (`kDefaultSequential`): `[-4]` keeps the four
    // documents behind the one answered. A `range` one lets go of what lies
    // left of the right endpoint it found answering the document before
    // (`kRightEndpoint`), nothing while its window is empty (macOS 8.2.1
    // and Linux 8.2.12, RM-404: `["unbounded", 4]` over values two apart
    // holds four documents, `["unbounded", -9]` seven).
    if !window_removes(Some(w)) && !matches!(c.op, "$first" | "$last" | "$derivative") {
        let keep = if range {
            match pos.checked_sub(1) {
                Some(before) if current => before,
                Some(before) => {
                    let (lo, hi) = range_window_bounds(before, n, w, range_vals, range_dates)?;
                    if hi >= lo {
                        clamp(hi).min(pos)
                    } else {
                        0
                    }
                }
                None => 0,
            }
        } else {
            match upper {
                Some(Bson::Int32(k)) if *k < 0 => clamp(p + i64::from(*k)),
                Some(Bson::Int64(k)) if *k < 0 => clamp(p + k),
                _ => pos,
            }
        };
        return Ok((keep, hi.max(p)));
    }
    if range || (matches!(c.op, "$first" | "$last") && window_removes(Some(w))) {
        return Ok((clamp((lo - 1).min(p)), hi.max(p)));
    }
    Ok((pos, hi.max(p)))
}

/// What one window function holds while `pos` is answered, as mongod's
/// tracker counts it (measured 8.2.11, `maxFunctionMemoryUsageBytes`): a
/// window that drops documents runs a removable function of a fixed size;
/// one that only grows runs the `$group` accumulator over what it has read,
/// counted as `$group` counts it; a rank function holds its accumulator;
/// `$first` / `$last` and the other position functions hold nothing of
/// their own.
enum FunctionMemory {
    Nothing,
    /// `AccumulatorRank` / `AccumulatorDenseRank` /
    /// `AccumulatorDocumentNumber`: `sizeof(*this)`, and for `$rank` and
    /// `$denseRank` the last distinct sort key, beyond the `Value` held
    /// inline (the build's own, `memlimit::LAYOUT`: Linux 8.2.12 376, 368,
    /// 368; macOS arm64 32 less).
    Rank {
        base: usize,
        keeps_key: bool,
    },
    /// A removable function: its own size, and each value in its window --
    /// once for a value a `Value` holds inline, twice for one on the heap,
    /// and `$push` every value twice (Linux x86-64 8.2.11, five ints in a
    /// `[-5, 5]` window: `$sum` 376, `$avg` 384, `$push` 320, `$stdDevPop`
    /// 552; its own size is the build's, `memlimit::LAYOUT`: macOS arm64
    /// counts 8 more for `$sum` and `$avg`, 16 for `$stdDevPop`, and 32
    /// less for `$push`).
    Removable {
        base: usize,
        sizes: Vec<Option<usize>>,
        /// The window it answered last, as `(from, upto)`.
        last: Option<(usize, usize)>,
    },
    Growing {
        acc: group::Acc,
        fed: usize,
    },
    /// `AccumulatorExpMovingAvg`: its own size alone, whatever it averages
    /// (136, measured macOS 8.2.1 over ints, decimals and missing values;
    /// Linux 8.2.12's least budgets agree; RM-404).
    ExpMovingAvg,
    /// `AccumulatorLocf`: its own size (128), and what the last value it
    /// carries holds beyond its inline `Value` -- nothing for an int, a
    /// string's buffer (measured macOS 8.2.1; Linux 8.2.12's least budgets
    /// agree; RM-404).
    Locf {
        last: Option<usize>,
        fed: usize,
    },
}

/// `AccumulatorExpMovingAvg`'s size (RM-404).
const EXP_MOVING_AVG: usize = 136;
/// `AccumulatorLocf`'s size, before the value it carries (RM-404).
const LOCF: usize = 128;

impl FunctionMemory {
    fn of(c: &OutField) -> R<FunctionMemory> {
        match c.op {
            "$rank" => {
                return Ok(FunctionMemory::Rank {
                    base: crate::memlimit::LAYOUT.rank,
                    keeps_key: true,
                })
            }
            "$denseRank" => {
                return Ok(FunctionMemory::Rank {
                    base: crate::memlimit::LAYOUT.dense_rank,
                    keeps_key: true,
                })
            }
            "$documentNumber" => {
                return Ok(FunctionMemory::Rank {
                    base: crate::memlimit::LAYOUT.document_number,
                    keeps_key: false,
                })
            }
            _ => {}
        }
        match c.op {
            "$expMovingAvg" => return Ok(FunctionMemory::ExpMovingAvg),
            "$locf" => return Ok(FunctionMemory::Locf { last: None, fed: 0 }),
            _ => {}
        }
        if matches!(c.op, "$first" | "$last")
            || PREFIX_OPS.contains(&c.op)
            || COVARIANCE_OPS.contains(&c.op)
            || matches!(c.op, "$shift" | "$derivative" | "$integral")
        {
            return Ok(FunctionMemory::Nothing);
        }
        if window_removes(c.window) {
            return Ok(FunctionMemory::Removable {
                base: match c.op {
                    "$avg" => crate::memlimit::LAYOUT.removable_avg,
                    "$push" => crate::memlimit::LAYOUT.removable_push,
                    "$stdDevPop" | "$stdDevSamp" => crate::memlimit::LAYOUT.removable_std_dev,
                    "$min" | "$max" => crate::memlimit::LAYOUT.removable_min_max,
                    _ => crate::memlimit::LAYOUT.removable,
                },
                sizes: Vec::new(),
                last: None,
            });
        }
        Ok(FunctionMemory::Growing {
            acc: group::new_acc(c.op)?,
            fed: 0,
        })
    }

    /// The bytes held once the window covers indices `from..=upto` of
    /// `slots`, answering `pos`, and the most held on the way there. A
    /// removable function takes in the values entering its window before
    /// it lets go of those leaving (`WindowFunctionExecRemovableDocument`
    /// and `...Range::update`, r8.2.1), so for a moment it holds both
    /// windows: mongod's tracker keeps that as its maximum, while its
    /// budget is checked once the update is done (measured 8.2.1 and
    /// 8.2.11: a `[-5, 5]` `$sum` of ints peaks at 12 values).
    #[allow(clippy::too_many_arguments)]
    fn held(
        &mut self,
        c: &OutField,
        pos: usize,
        (from, upto): (usize, usize),
        slots: &[usize],
        docs: &[Document],
        vars: &Document,
        sizer: &CacheSize,
        ranks: Option<&Ranks>,
    ) -> R<(usize, usize)> {
        Ok(match self {
            FunctionMemory::Nothing => (0, 0),
            FunctionMemory::Rank { base, keeps_key } => {
                // The key the accumulator kept: the first of the documents
                // tied with this one.
                let first_tied = ranks.map_or(pos, |r| r.rank[pos] as usize - 1);
                let held = match sizer.sort_key(&docs[slots[first_tied]]) {
                    Some(key) if *keeps_key => {
                        *base + crate::memlimit::classic_size(&key) - crate::memlimit::VALUE
                    }
                    _ => *base,
                };
                (held, held)
            }
            FunctionMemory::Removable { base, sizes, last } => {
                if sizes.is_empty() {
                    sizes.resize(slots.len(), None);
                }
                let mut value_size = |i: usize| -> usize {
                    if let Some(s) = sizes[i] {
                        return s;
                    }
                    let v = if c.op == "$count" {
                        Bson::Int32(1)
                    } else {
                        expressions::evaluate(&docs[slots[i]], c.arg, vars).unwrap_or(Bson::Null)
                    };
                    let size = crate::memlimit::classic_size(&v);
                    // `$min` / `$max` keep every value that is not nullish in
                    // their multiset, beside the executor's queue of all
                    // (`WindowFunctionMinMaxCommon::add`; RM-404).
                    let min_max = matches!(c.op, "$min" | "$max")
                        && !matches!(v, Bson::Null | Bson::Undefined);
                    let s = if size > 16 || c.op == "$push" || min_max {
                        2 * size
                    } else {
                        size
                    };
                    sizes[i] = Some(s);
                    s
                };
                let end = slots.len() - 1;
                let mut held = *base;
                for i in from..=upto.min(end) {
                    held += value_size(i);
                }
                // The values leaving, still held while the entering ones
                // went in: only when some entered.
                let mut peak = held;
                if let Some((was_from, was_upto)) = *last {
                    if upto.min(end) > was_upto.min(end) {
                        for i in was_from..=was_upto.min(end) {
                            if i < from || i > upto {
                                peak += value_size(i);
                            }
                        }
                    }
                }
                *last = Some((from, upto));
                (held, peak)
            }
            FunctionMemory::Growing { acc, fed } => {
                while *fed <= upto && *fed < slots.len() {
                    group::apply_acc(acc, c.arg, &docs[slots[*fed]], vars)?;
                    *fed += 1;
                }
                let held = group::acc_memory_classic(acc);
                (held, held)
            }
            FunctionMemory::ExpMovingAvg => (EXP_MOVING_AVG, EXP_MOVING_AVG),
            FunctionMemory::Locf { last, fed } => {
                while *fed <= pos && *fed < slots.len() {
                    let v = expressions::evaluate(&docs[slots[*fed]], c.arg, vars)
                        .unwrap_or(Bson::Null);
                    if !matches!(v, Bson::Null | Bson::Undefined) {
                        *last = Some(crate::memlimit::classic_size(&v) - crate::memlimit::VALUE);
                    }
                    *fed += 1;
                }
                let held = LOCF + last.unwrap_or(0);
                (held, held)
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
    /// How many times the cache spilled, and what it wrote.
    spills: i64,
    records: i64,
    bytes: i64,
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
            crate::bsondoc::write(d, &mut buf).map_err(|e| io("encode a document for", &e))?;
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
        self.spills += 1;
        self.records += docs.len() as i64;
        self.bytes += bytes;
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

/// How mongod's partition cache counts a document
/// (`Document::getApproximateSize`).
enum CacheSize<'a> {
    /// Cut down to the top-level fields this stage and the ones after it
    /// read (`None`: whole documents).
    Fields(Option<&'a std::collections::BTreeSet<String>>),
    /// A rank function reads the sort key `$sort` attaches as metadata
    /// (`ExpressionInternalRawSortKey`): mongod caches whole documents, each
    /// carrying that key -- the partition key first when there is one, then
    /// the `sortBy` value, an array when there are two (measured 8.2.1 and
    /// 8.2.12, RM-327).
    SortKey {
        partition_by: Option<&'a Bson>,
        sort_by: &'a Document,
        vars: &'a Document,
    },
}

impl CacheSize<'_> {
    fn of(&self, doc: &Document) -> usize {
        match self {
            CacheSize::Fields(Some(f)) => crate::memlimit::fields_size(doc, f),
            CacheSize::Fields(None) => crate::memlimit::document_size(doc),
            CacheSize::SortKey { .. } => {
                let key = self.sort_key(doc).unwrap_or(Bson::Null);
                crate::memlimit::document_size(doc)
                    + crate::memlimit::SORT_KEY_METADATA
                    + crate::memlimit::classic_size(&key)
            }
        }
    }

    /// The sort key `$sort` attaches to `doc`, when the cache keeps it.
    fn sort_key(&self, doc: &Document) -> Option<Bson> {
        let CacheSize::SortKey {
            partition_by,
            sort_by,
            vars,
        } = self
        else {
            return None;
        };
        let mut parts = Vec::with_capacity(sort_by.len() + 1);
        if let Some(by) = partition_by {
            parts.push(partition_key(doc, by, vars));
        }
        for (field, dir) in sort_by.iter() {
            let desc = matches!(dir, Bson::Int32(-1) | Bson::Int64(-1));
            parts.push(order::sort_repr(field_value(doc, field), desc));
        }
        Some(if parts.len() == 1 {
            parts.pop().expect("one part")
        } else {
            Bson::Array(parts)
        })
    }
}

/// `doc`'s partition key as `PartitionKeyComparator` holds it: missing is
/// null.
fn partition_key(doc: &Document, by: &Bson, vars: &Document) -> Bson {
    match expressions::evaluate(doc, by, vars) {
        Ok(Bson::Undefined) | Err(_) => Bson::Null,
        Ok(v) => v,
    }
}

/// What mongod's `PartitionIterator` counts besides its cache
/// (`getNextPartitionStateSize`), for one partition: the partition key its
/// comparator holds, and -- once a window asks past the partition's end --
/// the next partition's first document, which it read to find that end,
/// with the comparator now holding that partition's key. Nothing when
/// there is no `partitionBy` (measured 8.2.1 and 8.2.12, RM-327).
#[derive(Clone, Copy, Default)]
struct PartitionToken {
    key: usize,
    /// The next partition's first document and key, if there is one.
    next: Option<(usize, usize)>,
}

impl PartitionToken {
    fn bytes(&self, read_next: bool) -> usize {
        match self.next {
            Some((doc, key)) if read_next => doc + key,
            _ => self.key,
        }
    }

    /// The most it holds at once: reading the next partition's first
    /// document, the comparator holds that partition's key before it lets
    /// this one's go (`maxTotalMemoryUsageBytes`, measured 8.2.1,
    /// 2026-11-08: a partition of whole-partition `$first` peaks at its
    /// documents, the next one's first and both keys).
    fn peak(&self, read_next: bool) -> usize {
        match self.next {
            Some(_) if read_next => self.bytes(true) + self.key,
            _ => self.key,
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
/// (5643011). The cache's own check counts the cache alone; the stage's
/// counts the iterator's `token` too, and its message what is left once the
/// cache is let go: the functions and the token.
#[allow(clippy::too_many_arguments)]
fn run_cache(
    compiled: &[OutField],
    slots: &[usize],
    docs: &[Document],
    sizer: &CacheSize,
    ranks: Option<&Ranks>,
    token: PartitionToken,
    range_vals: Option<&[f64]>,
    range_dates: Option<&[f64]>,
    vars: &Document,
    spill: &mut CacheSpill,
    stats: &mut WindowMemory,
) -> R<()> {
    let n = slots.len();
    if n == 0 {
        return Ok(());
    }
    let last = n as i64 - 1;
    let budget = crate::memlimit::budgets().window_fields;
    let size = |i: usize| sizer.of(&docs[slots[i]]);
    let mut read_next = false;
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
    // mongod's tracker keeps its largest total at every change, so it sees
    // what the cache held for a moment: a document read before the one
    // behind it is let go, and a cache over its budget before it spills
    // (`maxTotalMemoryUsageBytes`, measured 8.2.1, 2026-11-08).
    let read_to = |cache: &mut Cache,
                   upto: usize,
                   spill: &mut CacheSpill,
                   outside: usize,
                   total: &mut usize|
     -> R<()> {
        while cache.next <= upto && cache.next < n {
            let s = size(cache.next);
            cache.held.push_back((cache.next, s));
            cache.used += s;
            cache.next += 1;
            *total = (*total).max(cache.used + outside);
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
    let mut outside_before = token.bytes(false);
    let mut held_before = vec![0usize; compiled.len()];
    for pos in 0..n {
        let mut ahead = pos as i64;
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
        read_to(
            &mut cache,
            ahead.min(last) as usize,
            spill,
            outside_before,
            &mut stats.total,
        )?;
        read_next |= ahead > last;
        // The functions update one after another: while one holds its
        // peak, those before it hold their new windows and those after it
        // their old ones.
        let mut functions_held = 0usize;
        let mut not_yet: usize = held_before.iter().sum();
        for ((((f, c), window), max), before) in functions
            .iter_mut()
            .zip(compiled)
            .zip(&reads)
            .zip(stats.functions.iter_mut())
            .zip(held_before.iter_mut())
        {
            let (held, peak) = f.held(c, pos, *window, slots, docs, vars, sizer, ranks)?;
            *max = (*max).max(peak);
            not_yet -= *before;
            stats.total = stats
                .total
                .max(cache.used + functions_held + peak + not_yet + token.peak(read_next));
            *before = held;
            functions_held += held;
        }
        let outside = functions_held + token.bytes(read_next);
        stats.total = stats
            .total
            .max(cache.used + functions_held + token.peak(read_next));
        outside_before = outside;
        if cache.used + outside > budget {
            if crate::memlimit::disk_allowed() {
                spill_all(&mut cache, spill)?;
            }
            // mongod names what is left once it lets the cache go: the
            // functions and the iterator's token (measured 8.2.1, 8.2.12).
            if cache.used + outside > budget {
                return Err(Fallback::mongo(
                    5414201,
                    format!(
                        "Exceeded memory limit in DocumentSourceSetWindowFields, used {outside} \
                         bytes but max allowed is {budget}"
                    ),
                ));
            }
        }
        // Advance: read the next document, then let go of what the next
        // answer does not need.
        if pos + 1 < n {
            read_to(&mut cache, pos + 1, spill, outside, &mut stats.total)?;
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

/// Per-slot exponential moving average over the sorted partition:
/// `ema[0] = input[0]`; `ema[i] = input[i]*alpha + ema[i-1]*(1-alpha)`.
///
/// Measured on mongod 8.2.11 and 8.2.1, 2026-11-06 (RM-225). Two things that
/// are not obvious from the output:
///
/// - An input no number reads -- null, missing, a string, a bool, an array --
///   is SKIPPED: the average does not move and the row answers the average as
///   it stands, which is null before the first numeric input. Not an error,
///   not a zero.
/// - The average is carried in **Decimal128 throughout**, and printed as a
///   double until a Decimal128 input appears; from that row on the decimal is
///   what comes out, carrying the scale the decimal arithmetic accumulated.
///   That is the only model that reproduces mongod's scales: `N: 3` over a
///   7.0 average and a Decimal128 2.5 answers `4.75` (the average entered as
///   `7.0`, exponent -1), while `alpha: 0.5` over the same data answers 30
///   decimals (an `alpha` double converts at 15 significant digits, so the
///   average has carried exponent -15 since the first row and the multiply
///   takes it to -30). A double INPUT converts at 15 significant digits too,
///   which is why `[Decimal128("1"), 4.0]` under `N: 3` answers
///   `2.500000000000000`.
///
/// The double output path is unchanged IEEE arithmetic, so the common case
/// stays bit-for-bit with the oracle; the decimal accumulator runs beside it.
fn ema_values(
    slots: &[usize],
    docs: &[Document],
    spec: &Document,
    vars: &Document,
) -> R<Vec<Bson>> {
    ema_alpha(spec)?; // validates exactly-one-of N/alpha and their ranges
    let alpha_dec = ema_alpha_decimal(spec)?;
    let one_minus_dec = decimal::add(
        &decimal::parse("1").ok_or(Fallback::Defer)?,
        &decimal::neg(&alpha_dec),
    )
    .ok_or(Fallback::Defer)?;
    let input = spec.get("input").ok_or(Fallback::Defer)?;
    let mut out = Vec::with_capacity(slots.len());
    let mut prev: Option<f64> = None;
    let mut prev_dec: Option<decimal::Dec> = None;
    let mut decimal_out = false;
    for &i in slots {
        let v = expressions::evaluate(&docs[i], input, vars)?;
        // `as_number` does not read a Decimal128, so ask about the type first.
        let decimal_in = matches!(v, Bson::Decimal128(_));
        let numeric = decimal_in || as_number(&v).is_some();
        if !numeric {
            // The row answers the average unchanged -- null if there is not
            // one yet.
            out.push(match (&prev, &prev_dec, decimal_out) {
                (_, Some(d), true) => decimal::to_bson(d).ok_or(Fallback::Defer)?,
                (Some(p), _, false) => Bson::Double(*p),
                _ => Bson::Null,
            });
            continue;
        }
        decimal_out |= decimal_in;
        // The decimal accumulator, which every row advances.
        let xd = decimal::from_bson(&v).ok_or(Fallback::Defer)?;
        prev_dec = Some(match &prev_dec {
            None => xd,
            Some(p) => decimal::add(
                &decimal::mul(&xd, &alpha_dec).ok_or(Fallback::Defer)?,
                &decimal::mul(p, &one_minus_dec).ok_or(Fallback::Defer)?,
            )
            .ok_or(Fallback::Defer)?,
        });
        let avg = prev_dec.as_ref().expect("just set");
        if decimal_out {
            out.push(decimal::to_bson(avg).ok_or(Fallback::Defer)?);
        } else {
            // mongod's double answer is the DECIMAL average converted, not a
            // double computation: `{N: 2}` over `[10, 0.1]` answers `3.4`,
            // where the same steps in IEEE doubles answer
            // `3.400000000000001` (14 of 1,170 swept shapes differed that way
            // before this; 0 after -- 2026-11-06).
            let d = decimal::to_string(avg)
                .parse::<f64>()
                .map_err(|_| Fallback::Defer)?;
            prev = Some(d);
            out.push(Bson::Double(d));
        }
    }
    Ok(out)
}

/// The smoothing factor as mongod builds it for the decimal accumulator: `N`
/// divides in Decimal128 (so `2/(N+1)` is exact when it terminates and 34
/// digits when it does not), while an `alpha` double converts at 15
/// significant digits. The two give different result scales, which is why the
/// double [`ema_alpha`] cannot stand in here (measured 2026-11-06).
fn ema_alpha_decimal(spec: &Document) -> R<decimal::Dec> {
    if let Some(n) = spec.get("N") {
        let n = match n {
            Bson::Int32(n) if *n >= 1 => i64::from(*n),
            Bson::Int64(n) if *n >= 1 => *n,
            _ => return Err(Fallback::Defer),
        };
        let two = decimal::parse("2").ok_or(Fallback::Defer)?;
        let denom = decimal::parse(&(n + 1).to_string()).ok_or(Fallback::Defer)?;
        decimal::div(&two, &denom).ok_or(Fallback::Defer)
    } else {
        match spec.get("alpha") {
            Some(a @ Bson::Double(_)) => decimal::from_bson(a).ok_or(Fallback::Defer),
            _ => Err(Fallback::Defer),
        }
    }
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
        "$expMovingAvg" => ema_values(slots, docs, arg.as_document().ok_or(Fallback::Defer)?, vars),
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

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::memlimit::{self, Budgets};
    use bson::{bson, doc};

    fn docs() -> Vec<Document> {
        (0..100i32)
            .map(|i| doc! {"_id": i, "k": i % 7, "n": i * 2, "s": "x".repeat(300 + i as usize)})
            .collect()
    }

    /// `$setWindowFields` over `docs()` under `budget`, with `allowDiskUse:
    /// false` and a `{_id: 1, t: 1}` projection after it.
    fn run(spec: &Bson, budget: usize) -> R<()> {
        let _b = memlimit::with_budgets(Budgets {
            window_fields: budget,
            ..Default::default()
        });
        let _d = memlimit::allow_disk_use(false);
        let downstream = ["_id", "t"].map(String::from).into_iter().collect();
        set_window_fields_each(
            spec,
            docs(),
            &Document::new(),
            Some(&downstream),
            None,
            &mut |_| Ok(()),
        )
    }

    /// The least budget `spec` runs under, and the refusal one byte below.
    fn least(spec: Bson) -> (usize, String) {
        let (mut lo, mut hi) = (1, 400_000);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if run(&spec, mid).is_ok() {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        let refusal = run(&spec, lo).expect_err("refused below the least budget");
        (hi, format!("{refusal:?}"))
    }

    fn window(output: Bson, partitioned: bool) -> Bson {
        let mut spec = doc! {"sortBy": {"n": 1}, "output": {"t": output}};
        if partitioned {
            spec.insert("partitionBy", "$k");
        }
        Bson::Document(spec)
    }

    /// What mongod 8.2.11's partition iterator has read once it answers
    /// each output (2026-11-10, RM-326): one ahead of a rank or running
    /// total, the window's upper end, and a partition's total its whole
    /// partition and the next one's first document.
    #[test]
    fn a_window_reads_what_its_iterator_asks_for() {
        let docs: Vec<Document> = (0..7)
            .map(|i| doc! {"_id": i, "k": i / 3, "n": i})
            .collect();
        let reads = |output: Bson, partitioned: bool| {
            let mut reads = Vec::new();
            set_window_fields_each(
                &window(output, partitioned),
                docs.clone(),
                &Document::new(),
                None,
                Some(&mut reads),
                &mut |_| Ok(()),
            )
            .unwrap();
            reads
        };
        assert_eq!(reads(bson!({"$rank": {}}), false), [2, 3, 4, 5, 6, 7, 8]);
        let ahead = bson!({"$sum": "$n", "window": {"documents": [-1, 2]}});
        assert_eq!(reads(ahead, false), [3, 4, 5, 6, 7, 8, 8]);
        assert_eq!(reads(bson!({"$sum": "$n"}), false), [8; 7]);
        assert_eq!(reads(bson!({"$sum": "$n"}), true), [4, 4, 4, 7, 7, 7, 8]);
        assert_eq!(reads(bson!({"$rank": {}}), true), [2, 3, 4, 5, 6, 7, 8]);
    }

    /// `$_internalSetWindowFields` takes its input as it comes: a partition
    /// is a run of one key (mongod 8.2.1, 2026-11-08, RM-328).
    #[test]
    fn the_internal_stage_partitions_runs_of_a_key() {
        let docs: Vec<Document> = [1, 2, 1, 2, 2, 1]
            .iter()
            .enumerate()
            .map(|(i, k)| doc! {"_id": i as i32, "k": *k, "n": i as i32})
            .collect();
        let spec = Bson::Document(doc! {
            "partitionBy": "$k",
            "sortBy": {"n": -1},
            "output": {"t": {"$sum": "$n"}},
        });
        let mut out = Vec::new();
        internal_set_window_fields_each(&spec, docs, &Document::new(), None, None, &mut |d| {
            out.push(d);
            Ok(())
        })
        .unwrap();
        let totals: Vec<(i32, i32)> = out
            .iter()
            .map(|d| (d.get_i32("_id").unwrap(), d.get_i32("t").unwrap()))
            .collect();
        assert_eq!(totals, [(0, 0), (1, 1), (2, 2), (3, 7), (4, 7), (5, 5)]);
    }

    /// What `explain` lists for the stage: each function's most and the
    /// stage's, with the moment the cache passes its budget before it
    /// spills (mongod 8.2.1, 2026-11-08, RM-328).
    #[test]
    fn a_window_reports_its_memory_at_its_peak() {
        let spec = window(bson!({"$first": "$n"}), false);
        let (_, report) = memlimit::captured(|| {
            set_window_fields_each(&spec, docs(), &Document::new(), None, None, &mut |_| Ok(()))
        });
        let total: usize = docs().iter().map(memlimit::document_size).sum();
        let stats = &report.windows[0];
        assert_eq!(stats.max_function_memory, [("t".to_string(), 0)]);
        assert_eq!(stats.max_total_memory, total as i64);
        assert_eq!(stats.spills, 0);
        let (_, report) = memlimit::captured(|| {
            let _b = memlimit::with_budgets(Budgets {
                window_fields: 10_000,
                ..Default::default()
            });
            set_window_fields_each(&spec, docs(), &Document::new(), None, None, &mut |_| Ok(()))
        });
        let stats = &report.windows[0];
        assert!(stats.spills > 0);
        assert!(stats.max_total_memory > 10_000, "{stats:?}");
    }

    /// Linux mongod 8.2.12 (x86-64 and arm64), 2026-11-08 (RM-327).
    #[test]
    fn a_rank_caches_whole_documents_with_their_sort_key() {
        let (least, refusal) = least(window(bson!({"$rank": {}}), false));
        assert_eq!(least, 1619);
        assert!(refusal.contains("5643011") && refusal.contains("Current memory: 1619 bytes"));
    }

    #[test]
    fn a_partitioned_window_counts_the_next_partitions_first_document() {
        let (least, refusal) = least(window(bson!({"$sum": "$n"}), true));
        // The function's own size: Linux's 176 bytes (2304), macOS arm64's
        // 184 (2312, RM-403).
        let sum = crate::memlimit::LAYOUT.sum;
        assert_eq!(least, 2128 + sum);
        // What is left once the cache goes: the function, the partition
        // key's 16 and the next partition's first document's 132.
        let used = format!("used {} bytes", sum + 16 + 132);
        assert!(
            refusal.contains("5414201") && refusal.contains(&used),
            "{refusal}"
        );
    }

    /// Linux mongod 8.2.12: 1222 and 12885; macOS arm64 8.2.1, whose
    /// removable `$sum` is 8 bytes larger and `$push` 32 smaller: 1230 and
    /// 12853 (RM-403).
    #[test]
    fn windows_with_no_lower_end_hold_what_they_have_still_to_read() {
        // RM-404 (macOS 8.2.1, Linux 8.2.12): each document cached is
        // `{_id, n}` with mongod's `Document` overhead; `n` runs two apart.
        let fields = ["_id", "n"].map(String::from).into_iter().collect();
        let doc = memlimit::fields_size(&docs()[0], &fields);
        let layout = memlimit::LAYOUT;
        let sum = |w: Bson| least(window(bson!({"$sum": "$n", "window": w}), false)).0;
        // `range`: from the right end found answering the one before, to
        // the document past this one's.
        assert_eq!(
            sum(bson!({"range": ["unbounded", 4]})),
            4 * doc + layout.sum
        );
        assert_eq!(
            sum(bson!({"range": ["unbounded", 0]})),
            3 * doc + layout.sum
        );
        assert_eq!(
            sum(bson!({"range": ["unbounded", -9]})),
            7 * doc + layout.sum
        );
        assert_eq!(
            sum(bson!({"range": ["unbounded", "current"]})),
            2 * doc + layout.sum
        );
        // `documents`: from its upper end.
        assert_eq!(
            sum(bson!({"documents": ["unbounded", -4]})),
            5 * doc + layout.sum
        );
        // A function that counts itself, besides the document answered.
        let ema = bson!({"$expMovingAvg": {"input": "$n", "N": 2}});
        assert_eq!(least(window(ema, false)).0, doc + EXP_MOVING_AVG);
        assert_eq!(least(window(bson!({"$locf": "$n"}), false)).0, doc + LOCF);
        // `$min` / `$max`: each int in the queue and in the multiset.
        let max = bson!({"$max": "$n", "window": {"documents": [-3, 3]}});
        assert_eq!(
            least(window(max, false)).0,
            4 * doc + layout.removable_min_max + 7 * 2 * memlimit::VALUE
        );
    }

    #[test]
    fn removable_windows_keep_their_budgets() {
        let layout = crate::memlimit::LAYOUT;
        let sum = bson!({"$sum": "$n", "window": {"documents": [-5, 5]}});
        assert_eq!(least(window(sum, false)).0, 926 + layout.removable);
        let push = bson!({"$push": "$s", "window": {"documents": [-5, 5]}});
        assert_eq!(least(window(push, false)).0, 12725 + layout.removable_push);
    }

    /// 8.2.1 and 8.2.12, 2026-11-08 (RM-405).
    #[test]
    fn a_rank_function_needs_one_sort_field() {
        for spec in [
            bson!({"output": {"t": {"$documentNumber": {}}}}),
            bson!({"sortBy": {"k": 1, "n": 1}, "output": {"t": {"$rank": {}}}}),
        ] {
            let err = format!("{:?}", run(&spec, memlimit::MAX_BYTES).unwrap_err());
            assert!(err.contains("5371602"), "{err}");
        }
    }
}
