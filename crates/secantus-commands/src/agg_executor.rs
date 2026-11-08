//! Whether an `aggregate` cursor still holds mongod's query executor after
//! its first batch, so that dropping an index the executor reads kills the
//! cursor (175 on the next `getMore`).
//!
//! mongod pushes the leading stages of a pipeline into its query executor:
//! `$match`, `$sort`, `$skip`, `$limit`, `$project` and `$unset` go to the
//! query layer, then `$group` and `$lookup` (with the stages that may follow
//! them there) to the slot-based engine. When nothing is left over, the
//! cursor IS the executor, and a dropped index kills it. Otherwise the
//! stages left over pull from the executor through a cursor stage, which
//! reads it in batches -- 32 documents at first -- and lets the executor go
//! once it has read to the end. A cursor whose executor was let go outlives
//! the index. Measured on mongod 8.2.11 (2026-10-16), each shape over 5, 20,
//! 30-33, 50, 100 and 3000 documents, `batchSize: 2`, the index dropped
//! after the first two documents:
//!
//! * nothing left over (`$match`; `+ $skip`, `$limit`, `$project`,
//!   `$unset`, a `$sort`; `+ $group` of an accumulator other than
//!   `$first` / `$last`; `+ $lookup`; `$lookup` then `$sort`; `$group`
//!   then `$match` / `$group` / `$lookup`): killed at every size;
//! * a stage left over that reads all its input before handing on a
//!   document (`$group`, `$sort`, `$bucket`, `$bucketAuto`,
//!   `$sortByCount`, `$facet`): never killed;
//! * only streaming stages left over (`$addFields`, `$unwind`,
//!   `$replaceRoot`, `$setWindowFields`, `$unionWith`, a `$group` that
//!   rides a DISTINCT_SCAN, ...): killed when the executor hands over 32
//!   documents or more, not when it hands over 31.
//!
//! Not modelled: a classic stage that filters so much the cursor stage
//! reads a second, larger batch before the first reply.

use bson::{doc, Bson, Document};

use crate::CommandContext;

/// The cursor stage's first batch (mongod's
/// `internalDocumentSourceCursorInitialBatchSize`, measured as the 31/32
/// boundary above).
const FIRST_LOAD: i64 = 32;

/// What the stages of `pipeline` leave of the executor.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Executor {
    /// The cursor is the executor.
    Held,
    /// A stage left over drains the executor before the first reply.
    Drained,
    /// Held while the first `n` stages hand over at least [`FIRST_LOAD`]
    /// documents.
    HeldIfLarge(usize),
}

fn name(stage: &Document) -> &str {
    stage.keys().next().map_or("", String::as_str)
}

/// Stages that read all their input before handing on the first document.
fn blocks(stage: &Document) -> bool {
    matches!(
        name(stage),
        "$group"
            | "$sort"
            | "$bucket"
            | "$bucketAuto"
            | "$sortByCount"
            | "$facet"
            | "$count"
            | "$sample"
    )
}

/// A `$group` mongod answers from a DISTINCT_SCAN: `_id` a field path that
/// leads an index, every accumulator a `$first` / `$last` of a field path.
/// It streams, one document per key.
fn distinct_group(stage: &Document, leads: &dyn Fn(&str) -> bool) -> bool {
    let Ok(spec) = stage.get_document("$group") else {
        return false;
    };
    let path = |v: &Bson| match v {
        Bson::String(p) if p.starts_with('$') && !p.starts_with("$$") => Some(p[1..].to_string()),
        _ => None,
    };
    let field = match spec.get("_id") {
        Some(Bson::Document(d)) if d.len() == 1 => d.values().next().and_then(path),
        Some(other) => path(other),
        None => None,
    };
    let Some(field) = field else {
        return false;
    };
    let firsts = spec.iter().filter(|(k, _)| *k != "_id").all(|(_, acc)| {
        acc.as_document()
            .and_then(|d| d.iter().next())
            .is_some_and(|(op, arg)| (op == "$first" || op == "$last") && path(arg).is_some())
    });
    firsts && leads(&field)
}

/// What `pipeline` (with its `$match`es hoisted as mongod hoists them)
/// leaves of the executor. `leads(field)`: an index on the collection leads
/// with `field`.
pub(crate) fn executor(pipeline: &[Document], leads: &dyn Fn(&str) -> bool) -> Executor {
    let mut i = 0;
    while i < pipeline.len()
        && matches!(
            name(&pipeline[i]),
            "$match" | "$sort" | "$skip" | "$limit" | "$project" | "$unset"
        )
    {
        i += 1;
    }
    if pipeline.get(i).is_some_and(|s| distinct_group(s, leads)) {
        // The group streams from the scan: what the scan hands over is one
        // document per key.
        let rest = &pipeline[i + 1..];
        return if rest.iter().any(blocks) {
            Executor::Drained
        } else {
            Executor::HeldIfLarge(i + 1)
        };
    }
    // The slot-based engine takes `$group` and `$lookup`, and after them
    // the stages measured to follow them there.
    let mut last: Option<&str> = None;
    while let Some(stage) = pipeline.get(i) {
        let pushed = match (name(stage), last) {
            ("$group", _) => true,
            ("$lookup", _) => stage
                .get_document("$lookup")
                .is_ok_and(|l| !l.contains_key("pipeline") && l.contains_key("localField")),
            ("$match", Some(_)) => true,
            ("$sort", Some("$lookup")) => true,
            _ => false,
        };
        if !pushed {
            break;
        }
        match name(stage) {
            "$group" => last = Some("$group"),
            "$lookup" => last = Some("$lookup"),
            _ => {}
        }
        i += 1;
    }
    let rest = &pipeline[i..];
    if rest.is_empty() {
        Executor::Held
    } else if rest.iter().any(blocks) {
        Executor::Drained
    } else {
        Executor::HeldIfLarge(i)
    }
}

/// Whether the cursor of `aggregate` command `cmd` over collection `coll`
/// still holds its executor after the first batch. `pipeline`: the
/// command's stages (a view's resolved in front).
pub(crate) fn holds_executor(
    ctx: &mut CommandContext,
    cmd: &Document,
    coll: &str,
    pipeline: &[Bson],
) -> bool {
    let stages: Vec<Document> = crate::explain::hoist::hoist_matches(pipeline)
        .into_iter()
        .filter_map(|s| match s {
            Bson::Document(d) => Some(d),
            _ => None,
        })
        .collect();
    let indexes = match ctx.storage() {
        Ok(storage) => storage.list_indexes(&ctx.db_name, coll).unwrap_or_default(),
        Err(_) => return true,
    };
    let leads = |field: &str| {
        indexes.iter().any(|ix| {
            ix.get_document("key")
                .ok()
                .and_then(|k| k.keys().next())
                .is_some_and(|k| k == field)
        })
    };
    let n = match executor(&stages, &leads) {
        Executor::Held => return true,
        Executor::Drained => return false,
        Executor::HeldIfLarge(n) => n,
    };
    // How many documents the first `n` stages hand over, up to the first
    // load.
    let mut prefix: Vec<Bson> = stages[..n].iter().cloned().map(Bson::Document).collect();
    prefix.push(Bson::Document(doc! { "$limit": FIRST_LOAD }));
    let mut count_cmd = doc! {
        "aggregate": coll,
        "pipeline": prefix,
        "cursor": { "batchSize": FIRST_LOAD + 1 },
    };
    for key in ["collation", "let"] {
        if let Some(v) = cmd.get(key) {
            count_cmd.insert(key, v.clone());
        }
    }
    let Ok(reply) =
        crate::explain::order::uncounted(|| crate::aggregate::aggregate_inline(&count_cmd, ctx))
    else {
        return true;
    };
    let Ok(cursor) = reply.get_document("cursor") else {
        return true;
    };
    if let Some(id) = cursor.get_i64("id").ok().filter(|id| *id != 0) {
        if let Ok(cursors) = ctx.cursors() {
            cursors.kill(&[id]);
        }
    }
    cursor
        .get_array("firstBatch")
        .map_or(true, |b| b.len() as i64 >= FIRST_LOAD)
}

/// The bytes a cursor-stage buffer stops at.
const LOAD_BYTES: usize = 4 * 1024 * 1024;

/// What the stages read of their input to give `pulls` documents of
/// output: some number of documents, or everything to its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reads {
    Some(usize),
    All,
}

/// How many buffers mongod's cursor stage loads from its executor while
/// `stages` (the pipeline after the stages the query layer took) give
/// `pulls` documents. `sizes`: the BSON size of each document the executor
/// hands over, in order, before the `$skip` and `$limit` it takes from
/// `stages`. `limited`: the executor already took a `$limit`. `None`: what
/// the stages read depends on the documents (a `$match`, an `$unwind`, a
/// `$setWindowFields` window), which is not followed here.
///
/// mongod 8.2.11: the `$skip`s and the least `$limit` that only stages
/// passing each document on stand before are moved into the executor
/// (`extractSkipForPushdown`, `extractLimitForPushdown`). Its cursor stage
/// (`DocumentSourceCursor::loadBatch`) then loads a buffer of 32 documents
/// at first and twice as many after each that filled by count -- without a
/// count when the executor took a `$limit` -- and stops a buffer once it
/// holds more than 4 MB; a load that reaches the end of the executor is its
/// last. Asking for the document after the last one loads again.
pub(crate) fn cursor_loads(
    stages: &[Bson],
    sizes: &[usize],
    limited: bool,
    pulls: usize,
) -> Option<usize> {
    let count = |stage: &Bson, name: &str| {
        stage
            .as_document()
            .and_then(|d| d.get(name))
            .and_then(crate::util::as_i64)
            .and_then(|n| usize::try_from(n).ok())
    };
    let (mut skip, mut limit) = (0usize, None::<usize>);
    let mut left: Vec<&Bson> = Vec::new();
    let mut moving = true;
    for (i, stage) in stages.iter().enumerate() {
        if moving {
            if let Some(k) = count(stage, "$skip") {
                skip = skip.saturating_add(k);
                continue;
            }
            if let Some(k) = count(stage, "$limit") {
                let k = k.saturating_add(skip);
                limit = Some(limit.map_or(k, |l| l.min(k)));
                continue;
            }
            moving = swaps_with_skip_and_limit(stage, stages.get(i + 1));
        }
        left.push(stage);
    }
    let sizes = &sizes[..limit.map_or(sizes.len(), |l| l.min(sizes.len()))];
    let sizes = &sizes[skip.min(sizes.len())..];
    let mut cap = if limited || limit.is_some() {
        usize::MAX
    } else {
        FIRST_LOAD as usize
    };
    let mut reads = Reads::Some(pulls);
    for stage in left.into_iter().rev() {
        let (name, spec) = stage.as_document().and_then(|d| d.iter().next())?;
        let n = || crate::util::as_i64(spec).and_then(|n| usize::try_from(n).ok());
        reads = match (name.as_str(), reads) {
            (_, Reads::Some(0)) => Reads::Some(0),
            (secantus_core::sbe::ENGINE_STAGE, r) => r,
            // A `$limit` asks for no more once it has passed its count.
            ("$limit", Reads::All) => Reads::Some(n()?),
            ("$limit", Reads::Some(p)) => Reads::Some(p.min(n()?)),
            ("$skip", Reads::Some(p)) => Reads::Some(p + n()?),
            (
                "$project" | "$addFields" | "$set" | "$unset" | "$replaceRoot" | "$replaceWith"
                | "$lookup" | "$graphLookup" | "$unionWith",
                r,
            ) => r,
            ("$setWindowFields" | "$_internalSetWindowFields", Reads::Some(_))
                if whole_input_window(spec) =>
            {
                Reads::All
            }
            (_, Reads::Some(_)) if blocks_bson(stage) => Reads::All,
            (_, Reads::All) => Reads::All,
            _ => return None,
        };
    }
    let wanted = match reads {
        Reads::Some(p) if p <= sizes.len() => Some(p),
        _ => None,
    };
    let (mut at, mut loads) = (0usize, 0usize);
    loop {
        if wanted.is_some_and(|w| at >= w) {
            return Some(loads);
        }
        loads += 1;
        let (mut count, mut bytes) = (0usize, 0usize);
        loop {
            let Some(size) = sizes.get(at) else {
                return Some(loads);
            };
            at += 1;
            count += 1;
            bytes += size;
            if count >= cap {
                cap = cap.saturating_mul(2);
                break;
            }
            if bytes > LOAD_BYTES {
                break;
            }
        }
    }
}

/// mongod's `canSwapWithSkippingOrLimitingStage`: a stage that hands on
/// each document it is given, one for one. A `$lookup` / `$graphLookup`
/// that takes in the `$unwind` after it does not.
fn swaps_with_skip_and_limit(stage: &Bson, next: Option<&Bson>) -> bool {
    match name_of(stage) {
        "$project"
        | "$addFields"
        | "$set"
        | "$unset"
        | "$replaceRoot"
        | "$replaceWith"
        | secantus_core::sbe::ENGINE_STAGE => true,
        name @ ("$lookup" | "$graphLookup") => {
            let as_field = stage
                .as_document()
                .and_then(|d| d.get_document(name).ok())
                .and_then(|spec| spec.get_str("as").ok());
            let unwound = next
                .and_then(Bson::as_document)
                .and_then(|d| match d.get("$unwind") {
                    Some(Bson::String(path)) => Some(path.clone()),
                    Some(Bson::Document(spec)) => spec.get_str("path").ok().map(str::to_string),
                    _ => None,
                });
            !matches!((as_field, unwound), (Some(a), Some(u)) if u == format!("${a}"))
        }
        _ => false,
    }
}

fn blocks_bson(stage: &Bson) -> bool {
    stage.as_document().is_some_and(blocks) || matches!(name_of(stage), "$out" | "$merge")
}

fn name_of(stage: &Bson) -> &str {
    stage
        .as_document()
        .and_then(|d| d.keys().next())
        .map_or("", String::as_str)
}

/// A `$setWindowFields` over one partition with an output whose window
/// reaches its end: it reads everything before its first document.
fn whole_input_window(spec: &Bson) -> bool {
    let Some(spec) = spec.as_document() else {
        return false;
    };
    if spec.contains_key("partitionBy") {
        return false;
    }
    let Ok(output) = spec.get_document("output") else {
        return false;
    };
    // The functions that take no window, each of which reads at most a
    // bounded distance ahead.
    const UNWINDOWED: &[&str] = &[
        "$rank",
        "$denseRank",
        "$documentNumber",
        "$shift",
        "$locf",
        "$linearFill",
        "$expMovingAvg",
    ];
    output.values().filter_map(Bson::as_document).any(|out| {
        let function = out.keys().find(|k| *k != "window").map(String::as_str);
        match out.get_document("window") {
            Ok(window) => ["documents", "range"].iter().any(|kind| {
                window
                    .get_array(kind)
                    .ok()
                    .and_then(|b| b.get(1))
                    .is_some_and(|upper| matches!(upper, Bson::String(s) if s == "unbounded"))
            }),
            // A windowed function with no window: the whole partition.
            Err(_) => function.is_some_and(|f| !UNWINDOWED.contains(&f)),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stages(v: &[Document]) -> Vec<Document> {
        v.to_vec()
    }

    fn none(_: &str) -> bool {
        false
    }

    fn on_a(f: &str) -> bool {
        f == "a"
    }

    #[test]
    fn a_pipeline_the_executor_takes_whole_is_held() {
        let m = doc! { "$match": { "a": { "$gte": 0 } } };
        for p in [
            vec![m.clone()],
            vec![m.clone(), doc! { "$skip": 1 }],
            vec![
                m.clone(),
                doc! { "$project": { "z": { "$add": ["$a", 1] } } },
            ],
            vec![m.clone(), doc! { "$group": { "_id": "$g" } }],
            vec![
                m.clone(),
                doc! { "$group": { "_id": "$g" } },
                doc! { "$match": { "_id": 1 } },
            ],
            vec![
                m.clone(),
                doc! { "$lookup": { "from": "o", "localField": "x", "foreignField": "x", "as": "j" } },
                doc! { "$sort": { "g": 1 } },
            ],
            vec![
                m.clone(),
                doc! { "$group": { "_id": "$a", "n": { "$sum": 1 } } },
            ],
        ] {
            assert_eq!(executor(&stages(&p), &on_a), Executor::Held, "{p:?}");
        }
    }

    #[test]
    fn a_blocking_stage_left_over_drains_it() {
        let m = doc! { "$match": { "a": { "$gte": 0 } } };
        for p in [
            vec![
                m.clone(),
                doc! { "$group": { "_id": "$g" } },
                doc! { "$sort": { "_id": 1 } },
            ],
            vec![
                m.clone(),
                doc! { "$addFields": { "z": 1 } },
                doc! { "$group": { "_id": "$g" } },
            ],
            vec![
                m.clone(),
                doc! { "$bucketAuto": { "groupBy": "$a", "buckets": 2 } },
            ],
            vec![m.clone(), doc! { "$facet": { "f": [] } }],
        ] {
            assert_eq!(executor(&stages(&p), &none), Executor::Drained, "{p:?}");
        }
    }

    #[test]
    fn streaming_stages_left_over_hold_it_when_large() {
        let m = doc! { "$match": { "a": { "$gte": 0 } } };
        let p = vec![m.clone(), doc! { "$addFields": { "z": 1 } }];
        assert_eq!(executor(&p, &none), Executor::HeldIfLarge(1));
        let g = vec![
            m.clone(),
            doc! { "$group": { "_id": "$g" } },
            doc! { "$unwind": "$p" },
        ];
        assert_eq!(executor(&g, &none), Executor::HeldIfLarge(2));
        // A `$group` over a DISTINCT_SCAN streams, one document per key.
        let d = vec![m, doc! { "$group": { "_id": "$a" } }];
        assert_eq!(executor(&d, &on_a), Executor::HeldIfLarge(2));
    }

    fn b(d: Document) -> Bson {
        Bson::Document(d)
    }

    /// The loads measured on mongod 8.2.11 (2026-11-07) for 300 documents
    /// of 1 KB behind a sort that spilled: `sortSpills` was 13 times these.
    #[test]
    fn the_cursor_stage_loads_as_mongod_does() {
        let sizes = vec![1060; 300];
        let af = b(doc! { "$addFields": { "z": 1 } });
        let group = b(doc! { "$group": { "_id": "$k" } });
        let window = b(doc! { "$setWindowFields": {
        "sortBy": { "n": 1 }, "output": { "t": { "$sum": "$n" } } } });
        let cases: Vec<(Vec<Bson>, usize, usize)> = vec![
            // A first batch of 101: 32 + 64 + 128 documents read.
            (vec![af.clone()], 101, 3),
            (vec![af.clone()], 10, 1),
            (vec![af.clone()], 40, 2),
            // All 300, and the end found: 32 + 64 + 128 + 76.
            (vec![af.clone()], 301, 4),
            (vec![af.clone(), group.clone()], 8, 4),
            (vec![window.clone()], 102, 4),
            // The `$skip` moves into the executor, which hands over 200.
            (vec![af.clone(), b(doc! { "$skip": 100 })], 101, 3),
            (vec![af.clone(), b(doc! { "$skip": 150 })], 101, 3),
            // A `$limit` in the executor lifts the count: one buffer.
            (vec![af.clone(), b(doc! { "$limit": 50 })], 51, 1),
            (
                vec![
                    af.clone(),
                    b(doc! { "$skip": 10 }),
                    b(doc! { "$limit": 150 }),
                ],
                101,
                1,
            ),
        ];
        for (stages, pulls, loads) in cases {
            assert_eq!(
                cursor_loads(&stages, &sizes, false, pulls),
                Some(loads),
                "{stages:?} {pulls}"
            );
        }
        // A `$limit` the executor took before the stages.
        assert_eq!(
            cursor_loads(std::slice::from_ref(&af), &sizes[..200], true, 101),
            Some(1)
        );
        // 32 documents fill the first buffer; the end is a second load.
        assert_eq!(
            cursor_loads(std::slice::from_ref(&window), &sizes[..32], false, 33),
            Some(2)
        );
        assert_eq!(cursor_loads(&[window], &sizes[..33], false, 34), Some(2));
        // A buffer stops past 4 MB, without doubling.
        let big = vec![1 << 20; 10];
        assert_eq!(
            cursor_loads(std::slice::from_ref(&group), &big, true, 1),
            Some(3)
        );
        assert_eq!(cursor_loads(&[group], &big, false, 1), Some(3));
    }

    #[test]
    fn reads_that_depend_on_the_documents_are_not_followed() {
        let sizes = vec![1060; 300];
        for stage in [
            doc! { "$unwind": "$s" },
            doc! { "$match": { "k": 1 } },
            doc! { "$setWindowFields": { "sortBy": { "n": 1 }, "output": { "r": { "$rank": {} } } } },
            doc! { "$setWindowFields": {
            "partitionBy": "$k", "sortBy": { "n": 1 }, "output": { "t": { "$sum": "$n" } } } },
        ] {
            assert_eq!(
                cursor_loads(&[b(stage.clone())], &sizes, false, 101),
                None,
                "{stage:?}"
            );
        }
        // Read to the end, they read everything.
        let then_group = [
            b(doc! { "$match": { "k": 1 } }),
            b(doc! { "$group": { "_id": "$k" } }),
        ];
        assert_eq!(cursor_loads(&then_group, &sizes, false, 2), Some(4));
    }
}
