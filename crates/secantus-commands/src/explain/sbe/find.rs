//! A `find` mongod 8.2.11 runs in SBE: under `queryFramework: "sbe"` a
//! query setting puts its framework control at `trySbeEngine`, and a
//! `find` whose filter and projection SBE compiles runs there rather than
//! in the classic engine (RM-418).
//!
//! Built here: the collection scan, its filter (`filter.rs`), skip and
//! limit, as `buildCollScan` and `buildLimitSkip` build them; the plan's
//! result is the scanned record itself. A sorted, projected or
//! index-served `find` is not built (`None`).

use super::group::{scan, Front, Scanned, Slots};
use super::Plan;

pub(super) fn build(uuid: &str, front: &Front) -> Option<Plan> {
    let mut slots = Slots(0);
    let Scanned {
        root,
        record,
        env,
        query_plan,
        ..
    } = scan(front, uuid, Default::default(), &mut slots)?;
    Some(Plan {
        root,
        result: record,
        env,
        query_plan,
        eof: false,
    })
}

#[cfg(test)]
mod tests {
    use super::build;
    use crate::explain::sbe::{Filter, Front};
    use bson::{doc, Document};

    fn front(parsed: Document, skip: i64, limit: i64) -> Front {
        Front {
            filter: Filter::parse(&parsed),
            parsed,
            skip,
            limit,
        }
    }

    fn slot_based(f: &Front) -> (String, String) {
        let plan = build("u", f).expect("built");
        let sbp = plan.winning_plan(false);
        let sbp = sbp.get_document("slotBasedPlan").unwrap();
        (
            sbp.get_str("slots").unwrap().to_string(),
            sbp.get_str("stages").unwrap().to_string(),
        )
    }

    /// The slot programs mongod 8.2.11 runs a `find` with under
    /// `queryFramework: "sbe"`, measured 2026-11-10.
    #[test]
    fn find_plans_are_mongods() {
        let gt0 = || doc! { "x": { "$gt": 0 } };
        let filter =
            "[1] filter {traverseF(s3, lambda(l2.0) { ((move(l2.0) > 0) ?: false) }, false)} \n";
        let scan = "[1] scan s1 s2 none none none none none none [s3 = x] @\"u\" true false ";
        assert_eq!(
            slot_based(&front(doc! {}, 0, 0)),
            (
                "$$RESULT=s1 env: {  }".to_string(),
                "[1] scan s1 s2 none none none none none none [] @\"u\" true false ".to_string()
            )
        );
        assert_eq!(
            slot_based(&front(gt0(), 0, 2)),
            (
                "$$RESULT=s1 env: { s4 = 2 }".to_string(),
                format!("[2] limit s4 \n{filter}{scan}")
            )
        );
        assert_eq!(
            slot_based(&front(gt0(), 1, 0)),
            (
                "$$RESULT=s1 env: { s4 = 1 }".to_string(),
                format!("[2] limitskip none s4 \n{filter}{scan}")
            )
        );
        // A skip under a limit takes its slot first.
        assert_eq!(
            slot_based(&front(gt0(), 1, 2)),
            (
                "$$RESULT=s2 env: { s1 = 1, s5 = 2 }".to_string(),
                "[3] limitskip s5 s1 \n\
                 [1] filter {traverseF(s4, lambda(l2.0) { ((move(l2.0) > 0) ?: false) }, false)} \n\
                 [1] scan s2 s3 none none none none none none [s4 = x] @\"u\" true false "
                    .to_string()
            )
        );
    }

    /// A limit stops the filter and the scan short of EOF; the limit itself
    /// is at EOF (measured 8.2.11: five documents, `{x: {$gt: 0}}`, limit 2).
    #[test]
    fn a_limit_stops_what_is_under_it() {
        let docs: Vec<Document> = (0..5).map(|i| doc! { "x": i }).collect();
        let plan = build("u", &front(doc! { "x": { "$gt": 0 } }, 0, 2)).unwrap();
        let (stats, n) = plan.execution_stats(&docs, crate::explain::sbe::Pull::Find, false);
        assert_eq!(n, 2);
        let top = stats.get_document("executionStages").unwrap();
        assert_eq!(top.get_i32("isEOF"), Ok(1));
        let filter = top.get_document("inputStage").unwrap();
        assert_eq!(filter.get_i32("nReturned"), Ok(2));
        assert_eq!(filter.get_i32("numTested"), Ok(3));
        assert_eq!(filter.get_i32("isEOF"), Ok(0));
        let scan = filter.get_document("inputStage").unwrap();
        assert_eq!(scan.get_i32("numReads"), Ok(3));
        assert_eq!(scan.get_i32("isEOF"), Ok(0));
        assert_eq!(stats.get_i32("totalDocsExamined"), Ok(3));
    }
}
