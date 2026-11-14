//! A `find` mongod 8.2.11 runs in SBE: under `queryFramework: "sbe"` a
//! query setting puts its framework control at `trySbeEngine`, and a
//! `find` whose filter and projection SBE compiles runs there rather than
//! in the classic engine (RM-418).
//!
//! Built here: the collection scan, its filter (`filter.rs`), skip and
//! limit, as `buildCollScan` and `buildLimitSkip` build them, and a simple
//! projection over them (`buildProjectionSimple`, RM-543): an inclusion or
//! exclusion of top-level fields. The plan's result is the scanned record,
//! or the projected one. A sorted or index-served `find` is not built
//! (`None`), nor a projection of a path, a computed field or a `$` operator.

use super::expr::Expr;
use super::group::{scan, Front, Scanned, Slots};
use super::stage::Stage;
use super::Plan;
use bson::{doc, Bson, Document};
use std::collections::BTreeSet;

/// A projection of top-level fields, as the planner keeps it.
struct Projection {
    /// The fields kept (an inclusion) or dropped (an exclusion), in the
    /// projection's order; an inclusion keeps `_id` last unless it names it.
    fields: Vec<String>,
    inclusion: bool,
    /// The `PROJECTION_SIMPLE` node's `transformBy`.
    transform_by: Document,
}

fn truth(v: &Bson) -> Option<bool> {
    Some(match v {
        Bson::Boolean(b) => *b,
        Bson::Int32(n) => *n != 0,
        Bson::Int64(n) => *n != 0,
        Bson::Double(d) if !d.is_nan() => *d != 0.0,
        _ => return None,
    })
}

fn projection(spec: &Document) -> Option<Projection> {
    let mut named: Vec<(String, bool)> = Vec::new();
    for (k, v) in spec {
        if k.is_empty() || k.starts_with('$') || k.contains('.') {
            return None;
        }
        named.push((k.clone(), truth(v)?));
    }
    let id = named.iter().find(|(k, _)| k == "_id").map(|(_, b)| *b);
    let others: Vec<bool> = named
        .iter()
        .filter(|(k, _)| k != "_id")
        .map(|(_, b)| *b)
        .collect();
    let inclusion = match (others.first(), id) {
        (Some(&first), _) => {
            if others.iter().any(|b| *b != first) {
                return None;
            }
            first
        }
        (None, Some(b)) => b,
        (None, None) => return None,
    };
    // An exclusion that keeps `_id` by name: not measured.
    if !inclusion && id == Some(true) {
        return None;
    }
    let mut transform_by: Document = named
        .iter()
        .map(|(k, b)| (k.clone(), Bson::Boolean(*b)))
        .collect();
    let mut fields: Vec<String> = named
        .iter()
        .filter(|(_, b)| *b == inclusion)
        .map(|(k, _)| k.clone())
        .collect();
    if inclusion && id.is_none() {
        transform_by.insert("_id", true);
        fields.push("_id".to_string());
    }
    Some(Projection {
        fields,
        inclusion,
        transform_by,
    })
}

/// The plan for the `find` whose query is `front` and whose projection is
/// `spec` (`None` or empty: none), over the collection whose UUID is `uuid`.
pub(super) fn build(uuid: &str, front: &Front, spec: Option<&Document>) -> Option<Plan> {
    let mut slots = Slots(0);
    let Some(spec) = spec.filter(|p| !p.is_empty()) else {
        let Scanned {
            root,
            record,
            env,
            query_plan,
            ..
        } = scan(front, uuid, Default::default(), &mut slots)?;
        return Some(Plan {
            root,
            result: record,
            env,
            query_plan,
            eof: false,
        });
    };
    let p = projection(spec)?;
    // A projection under a skip and a limit both: not measured.
    if front.skip > 0 && front.limit > 0 {
        return None;
    }
    // One field kept: `newBsonObj` of the field the scan reads.
    let single = p.inclusion && p.fields.len() == 1;
    let read: BTreeSet<String> = if single {
        p.fields.iter().cloned().collect()
    } else {
        BTreeSet::new()
    };
    // The projection sits over the skip and under the limit: the scan
    // builds the skip, the limit goes on top here.
    let under = Front {
        parsed: front.parsed.clone(),
        filter: front.filter.clone(),
        skip: front.skip,
        limit: 0,
    };
    let Scanned {
        root,
        record,
        fields,
        mut env,
        query_plan,
    } = scan(&under, uuid, read, &mut slots)?;
    let value = if single {
        let field = &p.fields[0];
        let slot = fields.iter().find(|(n, _)| n == field)?.1;
        Expr::func(
            "newBsonObj",
            vec![
                Expr::Const(Some(Bson::String(field.clone()))),
                Expr::Slot(slot),
            ],
        )
    } else {
        Expr::func(
            "makeBsonObj",
            vec![
                Expr::ObjSpec {
                    fields: p.fields.clone(),
                    open: !p.inclusion,
                },
                Expr::Slot(record),
            ],
        )
    };
    let node = under.nodes() + 1;
    let result = slots.next();
    let mut root = Stage::Project {
        node,
        binds: vec![(result, value)],
        input: Box::new(root),
    };
    let mut query_plan = doc! {
        "stage": "PROJECTION_SIMPLE",
        "planNodeId": node as i32,
        "transformBy": p.transform_by,
        "inputStage": query_plan,
    };
    if front.limit > 0 {
        let limit = slots.next();
        let at = (limit, front.limit);
        env.push((limit, front.limit.to_string()));
        root = Stage::LimitSkip {
            node: node + 1,
            limit: Some(at),
            skip: None,
            input: Box::new(root),
        };
        query_plan = doc! {
            "stage": "LIMIT",
            "planNodeId": (node + 1) as i32,
            "limitAmount": super::number(front.limit),
            "inputStage": query_plan,
        };
    }
    Some(Plan {
        root,
        result,
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
        projected(f, None)
    }

    fn projected(f: &Front, p: Option<Document>) -> (String, String) {
        let plan = build("u", f, p.as_ref()).expect("built");
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
        let plan = build("u", &front(doc! { "x": { "$gt": 0 } }, 0, 2), None).unwrap();
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

    fn find(raw: Document, parsed: Document, skip: i64, limit: i64) -> Front {
        Front {
            filter: crate::explain::sbe::Filter::parse_find(&parsed, &raw),
            parsed,
            skip,
            limit,
        }
    }

    const SCAN_X: &str = "[1] scan s1 s2 none none none none none none [s3 = x] @\"u\" true false ";

    /// An `$in`'s `InList`, bound after the scan's fields and before a
    /// limit, in the command's order (measured 8.2.11, 2026-11-14).
    #[test]
    fn in_plans_are_mongods() {
        let member = |l: u32, s: u32, list: u32| {
            format!("traverseF(s{s}, lambda(l{l}.0) {{ isMember(move(l{l}.0), s{list}) }}, false)")
        };
        assert_eq!(
            slot_based(&find(
                doc! { "x": { "$in": [2, 1, 1] } },
                doc! { "x": { "$in": [1, 2] } },
                0,
                0
            )),
            (
                "$$RESULT=s1 env: { s4 = InList(2 1 1) }".to_string(),
                format!("[1] filter {{{}}} \n{SCAN_X}", member(2, 3, 4))
            )
        );
        assert_eq!(
            slot_based(&find(
                doc! { "x": { "$in": [2.0, 1.5, 3_i64, -0.0, 1e20, "a b", true] } },
                doc! { "x": { "$in": [1.5, 2.0, 3_i64, -0.0, 1e20, "a b", true] } },
                0,
                2
            ))
            .0,
            "$$RESULT=s1 env: { s4 = InList(2.0 1.5 3 -0.0 1e+20 \"a b\" true), s5 = 2 }"
        );
        let both = slot_based(&find(
            doc! { "x": { "$in": [1, 2] }, "y": { "$in": [3, 4] } },
            doc! { "$and": [{ "x": { "$in": [1, 2] } }, { "y": { "$in": [3, 4] } }] },
            0,
            0,
        ));
        assert_eq!(
            both.0,
            "$$RESULT=s1 env: { s5 = InList(1 2), s6 = InList(3 4) }"
        );
        assert!(both.1.starts_with(&format!(
            "[1] filter {{({} && {})}}",
            member(3, 3, 5),
            member(4, 4, 6)
        )));
        // A merged skip and limit: the skip's slot first, the list's next.
        assert_eq!(
            slot_based(&find(
                doc! { "x": { "$in": [1, 2] } },
                doc! { "x": { "$in": [1, 2] } },
                1,
                2
            ))
            .0,
            "$$RESULT=s2 env: { s1 = 1, s5 = InList(1 2), s6 = 2 }"
        );
        // A null in the list takes another program: not built.
        assert!(crate::explain::sbe::Filter::parse_find(
            &doc! { "x": { "$in": [null, 1] } },
            &doc! { "x": { "$in": [null, 1] } }
        )
        .is_none());
    }

    /// The projections mongod 8.2.11 builds over the scan (2026-11-14).
    #[test]
    fn projection_plans_are_mongods() {
        let x1 = || find(doc! { "x": 1 }, doc! { "x": { "$eq": 1 } }, 0, 0);
        let filter_at = |s: u32| {
            format!("[1] filter {{traverseF(s{s}, lambda(l2.0) {{ ((move(l2.0) == 1) ?: false) }}, false)}} \n")
        };
        let filter = filter_at(3);
        for (spec, program) in [
            (
                doc! { "y": 1 },
                "makeBsonObj(MakeObjSpec([y, _id], Closed, RetNothing, 0), s1)",
            ),
            (
                doc! { "_id": 1, "z": 1, "y": 1 },
                "makeBsonObj(MakeObjSpec([_id, z, y], Closed, RetNothing, 0), s1)",
            ),
            (
                doc! { "_id": 0, "z": 1, "y": 1 },
                "makeBsonObj(MakeObjSpec([z, y], Closed, RetNothing, 0), s1)",
            ),
            (
                doc! { "y": 0, "_id": 0 },
                "makeBsonObj(MakeObjSpec([y, _id], Open, RetInput, 0), s1)",
            ),
            (
                doc! { "_id": 0 },
                "makeBsonObj(MakeObjSpec([_id], Open, RetInput, 0), s1)",
            ),
            (doc! { "_id": 0, "x": 1 }, "newBsonObj(\"x\", s3)"),
        ] {
            assert_eq!(
                projected(&x1(), Some(spec.clone())),
                (
                    "$$RESULT=s4 env: {  }".to_string(),
                    format!("[2] project [s4 = {program}] \n{filter}{SCAN_X}")
                ),
                "{spec}"
            );
        }
        // One field of its own: the scan reads it before the filter's.
        assert_eq!(
            projected(&x1(), Some(doc! { "_id": 0, "y": 1 })),
            (
                "$$RESULT=s5 env: {  }".to_string(),
                format!(
                    "[2] project [s5 = newBsonObj(\"y\", s3)] \n{}\
                     [1] scan s1 s2 none none none none none none [s3 = y, s4 = x] @\"u\" true false ",
                    filter_at(4)
                )
            )
        );
        let closed = "makeBsonObj(MakeObjSpec([y, _id], Closed, RetNothing, 0), s1)";
        // Under a limit, over a skip.
        let limited = find(doc! { "x": 1 }, doc! { "x": { "$eq": 1 } }, 0, 2);
        assert_eq!(
            projected(&limited, Some(doc! { "y": 1 })),
            (
                "$$RESULT=s4 env: { s5 = 2 }".to_string(),
                format!("[3] limit s5 \n[2] project [s4 = {closed}] \n{filter}{SCAN_X}")
            )
        );
        let skipped = find(doc! { "x": 1 }, doc! { "x": { "$eq": 1 } }, 1, 0);
        assert_eq!(
            projected(&skipped, Some(doc! { "y": 1 })),
            (
                "$$RESULT=s5 env: { s4 = 1 }".to_string(),
                format!("[3] project [s5 = {closed}] \n[2] limitskip none s4 \n{filter}{SCAN_X}")
            )
        );
        let plan = build("u", &x1(), Some(&doc! { "_id": 0, "y": 1 })).unwrap();
        let qp = plan.winning_plan(false);
        let qp = qp.get_document("queryPlan").unwrap();
        assert_eq!(qp.get_str("stage"), Ok("PROJECTION_SIMPLE"));
        assert_eq!(
            qp.get_document("transformBy").unwrap(),
            &doc! { "_id": false, "y": true }
        );
        // Not built: a path, a computed field, a mixed spec.
        for spec in [
            doc! { "d.e": 1 },
            doc! { "y": "$x" },
            doc! { "y": 1, "z": 0 },
        ] {
            assert!(build("u", &x1(), Some(&spec)).is_none(), "{spec}");
        }
    }
}
