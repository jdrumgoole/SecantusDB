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
}
