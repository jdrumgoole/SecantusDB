//! The slot-based plan mongod 8.2.11 runs for a pipeline whose `$group` it
//! pushes into the query (RM-071): `explainVersion` "2", a `winningPlan` of
//! `{queryPlan, slotBasedPlan}`, and `executionStats` made of SBE stages.
//!
//! The plan is built as mongod's stage builders build it (`gen_group.cpp`,
//! `gen_accumulator.cpp`, `gen_coll_scan.cpp`), printed as its debug printer
//! prints it, and RUN over the collection's documents, so every count in
//! `executionStats` comes from executing the slot program.
//!
//! What is built here: a `$group` (or the `$group` a `$count` is) over a
//! collection scan with no sort, hint or collation, whose filter is
//! comparisons and their `$and`s (`filter.rs`), with a skip and a limit
//! when the group reads no field; whose `_id` and accumulator arguments
//! are field paths, `$$ROOT` or constants, and whose accumulators are
//! `$sum`, `$count`, `$avg`, `$first`, `$last`, `$min`, `$max`, `$push` and
//! `$addToSet`. And a `find` a `queryFramework: "sbe"` query setting puts
//! in SBE (RM-418, `find.rs`): the same collection scan, filter, skip and
//! limit, with a simple projection over them. Everything else SBE runs is still
//! explained in the classic shape (`tasks/backlog-rust-mongodb.md`, RM-071,
//! RM-543).

mod expr;
mod filter;
mod find;
mod group;
mod print;
mod stage;

use bson::{doc, Bson, Document};
use expr::Slot;
pub(crate) use filter::Filter;
pub(crate) use group::Front;
use stage::{Counted, Stage};

/// A built slot-based plan.
pub(crate) struct Plan {
    root: Stage,
    result: Slot,
    /// The constants the plan reads from slots (its `$in` lists, skip and
    /// limit), as its environment prints them.
    env: Vec<(Slot, String)>,
    /// The `queryPlan`: the query solution the stages were built from.
    query_plan: Document,
    /// The collection does not exist: `limit 0` over `coscan`.
    eof: bool,
}

/// The plan for `group` (a `$group` spec) over `front`, a collection scan
/// of the collection whose UUID is `uuid`; `last`: nothing classic follows,
/// so the result is a BSON object (`newBsonObj`) rather than an SBE one
/// (`newObj`). `None` when the group is outside what is built here.
pub(crate) fn group_plan(group: &Document, uuid: &str, last: bool, front: &Front) -> Option<Plan> {
    group::build(group, uuid, last, front)
}

/// The plan for a `find` whose query is `front` and whose projection is
/// `projection`, a collection scan of the collection whose UUID is `uuid`.
/// `None` when its filter or projection is outside what is built here.
pub(crate) fn find_plan(uuid: &str, front: &Front, projection: Option<&Document>) -> Option<Plan> {
    find::build(uuid, front, projection)
}

/// The plan mongod builds over a collection that does not exist.
pub(crate) fn eof_plan() -> Plan {
    Plan {
        root: Stage::Scan {
            node: 1,
            record: 1,
            record_id: 0,
            fields: vec![],
            uuid: String::new(),
        },
        result: 1,
        env: vec![],
        query_plan: doc! { "stage": "EOF", "planNodeId": 1, "type": "nonExistentNamespace" },
        eof: true,
    }
}

/// A count as `BSONObjBuilder::appendNumber` stores it: an int32 when it
/// fits.
pub(super) fn number(n: i64) -> Bson {
    match i32::try_from(n) {
        Ok(small) => Bson::Int32(small),
        Err(_) => Bson::Int64(n),
    }
}

/// `internalDocumentSourceCursorInitialBatchSize`: the first batch a
/// `$cursor` stage loads; each later one is twice the one before.
const INITIAL_BATCH: i64 = 32;

/// How a classic stage after the plan pulls from it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Pull {
    /// Nothing classic follows: the plan runs once to the end.
    Whole,
    /// A `find`'s plan: run to the end, each stage reported as it ran (a
    /// limit stops what is under it short of EOF).
    Find,
    /// A `$cursor` loads batches until the stages after it stop pulling:
    /// at a `$limit` right after it, or once it has handed over `handed`
    /// when that is less than the plan returns; else at the plan's end.
    Batches { limit: Option<i64>, handed: i64 },
}

/// What the `$cursor` stage's batches took from a plan that returns `n`:
/// the documents produced, the number of batches loaded, and whether the
/// plan was asked past its last document (a batch came back short). A batch
/// that comes back full is followed by another unless the stages after the
/// cursor have their `demand`, so a plan that returns exactly a batch's
/// worth is asked once more and answers nothing.
fn batches(n: i64, demand: Option<i64>) -> (i64, i64, bool) {
    let (mut got, mut loads, mut size) = (0_i64, 0_i64, INITIAL_BATCH);
    loop {
        let this = size.min(n - got);
        got += this;
        loads += 1;
        if this < size {
            return (got, loads, true);
        }
        if demand.is_some_and(|d| got >= d) {
            return (got, loads, false);
        }
        size *= 2;
    }
}

impl Plan {
    /// `winningPlan`'s `queryPlan` and `slotBasedPlan`.
    pub fn winning_plan(&self, is_cached: bool) -> Document {
        let (slots, stages) = if self.eof {
            (
                "$$RESULT=s1 env: { s1 = Nothing (nothing) }".to_string(),
                "[1] limit 0ll \n[1] coscan ".to_string(),
            )
        } else {
            let env: Vec<String> = self
                .env
                .iter()
                .map(|(s, v)| format!("s{s} = {v}"))
                .collect();
            (
                format!("$$RESULT=s{} env: {{ {} }}", self.result, env.join(", ")),
                print::stages(&self.root),
            )
        };
        doc! {
            "isCached": is_cached,
            "queryPlan": self.query_plan.clone(),
            "slotBasedPlan": { "slots": slots, "stages": stages },
        }
    }

    /// Run the plan over `docs` and report it as `executionStats`; returns
    /// the stats and what the plan produced.
    pub fn execution_stats(
        &self,
        docs: &[Document],
        pull: Pull,
        all_plans: bool,
    ) -> (Document, i64) {
        let (stages, produced, examined) = if self.eof {
            // Under a `$cursor`, saved and restored around its one batch.
            let (saved, restored) = match pull {
                Pull::Whole | Pull::Find => (0, 0),
                Pull::Batches { .. } => {
                    let (_, loads, _) = batches(0, None);
                    (loads + 1, loads)
                }
            };
            let stages = doc! {
                "stage": "limit",
                "planNodeId": 1,
                "nReturned": 0_i32,
                "executionTimeMillisEstimate": 0_i32,
                "opens": 1_i32,
                "closes": 1_i32,
                "saveState": number(saved),
                "restoreState": number(restored),
                "isEOF": 1_i32,
                "limit": 0_i32,
                "inputStage": {
                    "stage": "coscan",
                    "planNodeId": 1,
                    "nReturned": 0_i32,
                    "executionTimeMillisEstimate": 0_i32,
                    "opens": 1_i32,
                    "closes": 1_i32,
                    "saveState": number(saved),
                    "restoreState": number(restored),
                    "isEOF": 0_i32,
                },
            };
            (stages, 0, 0)
        } else {
            let (rows, counted) = self.root.run(docs, None);
            let n = rows.len() as i64;
            let (produced, saves, eof) = match pull {
                Pull::Whole | Pull::Find => (n, None, true),
                Pull::Batches { limit, handed } => {
                    let demand = limit.or((handed < n).then_some(handed));
                    let (got, loads, eof) = batches(n, demand);
                    // Saved once when the cursor is prepared and after each
                    // batch, restored before each batch.
                    (got, Some((loads + 1, loads)), eof)
                }
            };
            // What the scan at the bottom read.
            let mut scan = &counted;
            while let Some(below) = scan.input.as_deref() {
                scan = below;
            }
            let examined = scan.reads;
            let pulled = Pulled { produced, eof };
            let pulled = (!matches!(pull, Pull::Find)).then_some(&pulled);
            let stages = stats_doc(&self.root, &counted, pulled, saves);
            (stages, produced, examined)
        };
        let mut out = doc! {
            "executionSuccess": true,
            "nReturned": number(produced),
            "executionTimeMillis": 0_i32,
            "totalKeysExamined": 0_i32,
            "totalDocsExamined": number(examined),
            "executionStages": stages,
        };
        if all_plans {
            out.insert("allPlansExecution", Bson::Array(vec![]));
        }
        (out, produced)
    }
}

/// How far the plan's top was pulled.
struct Pulled {
    produced: i64,
    /// The plan was asked past its last document.
    eof: bool,
}

/// One stage's `executionStages` entry. The group and the stages above it
/// return what the `$cursor` pulled; what is under the group ran to the end
/// before it returned anything (`pulled` is `None`).
fn stats_doc(
    stage: &Stage,
    counted: &Counted,
    pulled: Option<&Pulled>,
    saves: Option<(i64, i64)>,
) -> Document {
    let returned = pulled.map_or(counted.returned, |p| counted.returned.min(p.produced));
    let eof = pulled.map_or(counted.eof, |p| p.eof);
    let (saved, restored) = saves.unwrap_or((0, 0));
    let mut out = doc! {
        "stage": stage.name(),
        "planNodeId": stage.node() as i32,
        "nReturned": number(returned),
        "executionTimeMillisEstimate": 0_i32,
        "opens": 1_i32,
        "closes": 1_i32,
        "saveState": number(saved),
        "restoreState": number(restored),
        "isEOF": i32::from(eof),
    };
    print::specific_stats(stage, counted, &mut out);
    if let (Some(input), Some(below)) = (stage.input(), counted.input.as_deref()) {
        let under = if matches!(stage, Stage::Group { .. }) {
            None
        } else {
            pulled
        };
        out.insert("inputStage", stats_doc(input, below, under, saves));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::batches;

    /// The `$cursor` batches mongod 8.2.11 loads from a pushed-down
    /// `$group` (`saveState` - 1 of its stages), measured 2026-11-08.
    #[test]
    fn cursor_batches_double() {
        for (n, demand, loads, got) in [
            (0, None, 1, 0),
            (10, None, 1, 10),
            (31, None, 1, 31),
            (32, None, 2, 32),
            (33, None, 2, 33),
            (96, None, 3, 96),
            (97, None, 3, 97),
            (300, None, 4, 300),
            (5000, None, 8, 5000),
            (32, Some(40), 2, 32),
            (96, Some(40), 2, 96),
            (300, Some(40), 2, 96),
            (5000, Some(3), 1, 32),
            (10, Some(3), 1, 10),
        ] {
            let (g, l, _) = batches(n, demand);
            assert_eq!((g, l), (got, loads), "n={n} demand={demand:?}");
        }
        // Two full batches satisfy a `$limit: 40` over 96: never asked again.
        assert!(!batches(96, Some(40)).2);
        assert!(batches(97, None).2);
    }
}
