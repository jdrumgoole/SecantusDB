//! A `find` mongod 8.2.11 runs in SBE: under `queryFramework: "sbe"` a
//! query setting puts its framework control at `trySbeEngine`, and a
//! `find` whose filter and projection SBE compiles runs there rather than
//! in the classic engine (RM-418).
//!
//! Built here: the collection scan, its filter (`filter.rs`), skip and
//! limit, as `buildCollScan` and `buildLimitSkip` build them, and a simple
//! projection over them (`buildProjectionSimple`, RM-543): an inclusion or
//! exclusion of top-level fields. The plan's result is the scanned record,
//! or the projected one. A sort on top-level fields is built
//! (`build_sorted`, RM-543). An index-served `find` is not built (`None`),
//! nor a sort on a path, nor a projection of a path, a
//! computed field or a `$` operator.
//!
//! The same plan is what mongod runs for a `distinct`, and for an
//! `aggregate` whose stages it takes into the query, under
//! `queryFramework: "sbe"` (RM-542); the `$addFields` / `$set` stages of
//! constants and top-level field paths after the query are built as the
//! `PROJECTION_DEFAULT`s they become: a `project` binding each constant to
//! a slot, then `makeBsonObj` over the record (measured 8.2.11,
//! 2026-11-14).

use super::expr::{BinOp, Expr};
use super::group::{arg_of, scan, Arg, Front, Scanned, Slots};
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
        seek: None,
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

/// The plan for the `find` whose query is `front`, sorted on `keys`
/// (top-level fields, each `true` when descending), with `limit` (0: none)
/// as the sort's top-k: `buildSort` puts a `project` of each key's
/// `getSortKeyAsc` / `getSortKeyDesc` and a `sort` over the scan, both the
/// `SORT` node's, the scan reading the keys' fields (in name order) before
/// the filter's. Two keys or more add the parallel-arrays check, the
/// project's last slot: an `||` of two `isArray`s for two keys, a sum of
/// `<=>`s for more (measured 8.2.11, 2026-11-28). A skip alone is a
/// `limitskip none` over the sort, its own `SKIP` node, its constant in
/// the slot after the sort's (measured 8.2.11, 2026-11-28). A skip and a
/// limit both make the sort's top-k `skip + limit`, printed as a `let` of
/// the two slots (the limit's, then the skip's) guarded by `typeMatch`,
/// its frame the one after the filter's, and the skip a `limitskip none`
/// over the sort on the skip's slot, with no limit stage (measured 8.2.11,
/// 2026-11-28). The skip's slot comes before the limit's, as on Linux. `None` for a `seek`.
pub(super) fn build_sorted(uuid: &str, front: &Front, keys: &[(String, bool)]) -> Option<Plan> {
    if front.seek.is_some()
        || keys.is_empty()
        || keys.iter().any(|(f, _)| f.is_empty() || f.contains('.'))
    {
        return None;
    }
    let under = Front {
        parsed: front.parsed.clone(),
        filter: front.filter.clone(),
        skip: 0,
        limit: 0,
        seek: None,
    };
    let mut slots = Slots(0);
    let read: BTreeSet<String> = keys.iter().map(|(f, _)| f.clone()).collect();
    if read.len() != keys.len() {
        return None;
    }
    let Scanned {
        root,
        record,
        fields,
        mut env,
        query_plan,
    } = scan(&under, uuid, read, &mut slots)?;
    let mut sources = Vec::new();
    for (field, _) in keys {
        sources.push(fields.iter().find(|(n, _)| n == field)?.1);
    }
    let node = under.nodes() + 1;
    let mut binds = Vec::new();
    let mut sort_keys = Vec::new();
    for ((_, desc), source) in keys.iter().zip(&sources) {
        let key = slots.next();
        let get = if *desc {
            "getSortKeyDesc"
        } else {
            "getSortKeyAsc"
        };
        binds.push((key, Expr::func(get, vec![Expr::Slot(*source)])));
        sort_keys.push((key, *desc));
    }
    if sources.len() > 1 {
        let is_array = |s: u32| {
            Expr::fill_empty(
                Expr::func("isArray", vec![Expr::Slot(s)]),
                Expr::Const(Some(Bson::Boolean(false))),
            )
        };
        let fail = Expr::func(
            "fail",
            vec![
                Expr::Const(Some(Bson::Int32(2))),
                Expr::Const(Some(Bson::String(
                    "cannot sort with keys that are parallel arrays".into(),
                ))),
            ],
        );
        let ok = if let [a, b] = sources.as_slice() {
            Expr::bin(
                BinOp::LogicOr,
                Expr::Not(Box::new(is_array(*a))),
                Expr::Not(Box::new(is_array(*b))),
            )
        } else {
            let count = sources
                .iter()
                .map(|s| {
                    Expr::bin(
                        BinOp::Cmp3w,
                        is_array(*s),
                        Expr::Const(Some(Bson::Boolean(false))),
                    )
                })
                .reduce(|l, r| Expr::bin(BinOp::Add, l, r))?;
            Expr::bin(BinOp::Lte, count, Expr::Const(Some(Bson::Int32(1))))
        };
        binds.push((slots.next(), Expr::bin(BinOp::LogicOr, ok, fail)));
    }
    let project = Stage::Project {
        node,
        binds,
        input: Box::new(root),
    };
    // The skip's slot before the limit's, as Linux mongod numbers them
    // (8.2.11 and 8.2.12 on Linux x86-64, the exemplar build: `env: { s6 =
    // <skip>, s7 = <limit> }`); macOS arm64's 8.2.11 numbers the limit's
    // first (measured 2026-11-28, PR #3151).
    let skip = (front.skip > 0).then(|| {
        let s = slots.next();
        env.push((s, front.skip.to_string()));
        (s, front.skip)
    });
    let limit = (front.limit > 0).then(|| {
        let s = slots.next();
        env.push((s, front.limit.to_string()));
        (s, front.limit)
    });
    let (limit, limit_expr) = match (limit, skip) {
        (Some((l, n)), Some((s, k))) => {
            let frame = 2 + under.filter.as_ref().map_or(0, |f| f.frames());
            let sum = Expr::bin(BinOp::Add, Expr::Slot(l), Expr::Slot(s));
            let guarded = Expr::if_(
                Expr::TypeMatch(Box::new(Expr::local(frame, false)), 262_144),
                Expr::func("makeOwn", vec![Expr::local(frame, true)]),
                Expr::Int64(i64::MAX),
            );
            (
                Some((l, n.saturating_add(k))),
                Some(Expr::let_(frame, sum, guarded)),
            )
        }
        (limit, _) => (limit, None),
    };
    let root = Stage::Sort {
        node,
        keys: sort_keys,
        outputs: vec![record],
        limit,
        limit_expr,
        input: Box::new(project),
    };
    let mut pattern = Document::new();
    for (field, desc) in keys {
        pattern.insert(field.clone(), if *desc { -1 } else { 1 });
    }
    let mut sort = doc! {
        "stage": "SORT",
        "planNodeId": node as i32,
        "sortPattern": pattern,
        "memLimit": 104_857_600_i32,
    };
    if let Some((_, n)) = limit {
        sort.insert("limitAmount", super::number(n));
    }
    sort.insert("type", "simple");
    sort.insert("inputStage", query_plan);
    let (root, query_plan) = if let Some((s, _)) = skip {
        let root = Stage::LimitSkip {
            node: node + 1,
            limit: None,
            skip: Some((s, front.skip)),
            input: Box::new(root),
        };
        let plan = doc! {
            "stage": "SKIP",
            "planNodeId": (node + 1) as i32,
            "skipAmount": super::number(front.skip),
            "inputStage": sort,
        };
        (root, plan)
    } else {
        (root, sort)
    };
    Some(Plan {
        root,
        result: record,
        env,
        query_plan,
        eof: false,
    })
}

/// The plan for a query with `adds` (`$addFields` specs, in order) over
/// it. Each stage is its own `PROJECTION_DEFAULT` node, binding its
/// constants there; the last makes the one object they all set. `None`
/// when a field is not a new top-level name, or a value not a scalar
/// constant or a top-level field path of the scanned document.
pub(super) fn build_added(uuid: &str, front: &Front, adds: &[Document]) -> Option<Plan> {
    let mut stages: Vec<Vec<(String, Arg)>> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for add in adds {
        if add.is_empty() {
            return None;
        }
        let mut args = Vec::new();
        for (name, v) in add {
            if name == "_id" || name.starts_with('$') || name.contains('.') || names.contains(name)
            {
                return None;
            }
            match arg_of(v)? {
                // A field an earlier stage set is not the scan's.
                Arg::Field(f) if names.contains(&f) => return None,
                a @ (Arg::Const(_) | Arg::Field(_)) => args.push((name.clone(), a)),
                _ => return None,
            }
        }
        names.extend(args.iter().map(|(n, _)| n.clone()));
        stages.push(args);
    }
    if stages.is_empty() {
        return None;
    }
    let fields: BTreeSet<String> = stages
        .iter()
        .flatten()
        .filter_map(|(_, a)| match a {
            Arg::Field(f) => Some(f.clone()),
            _ => None,
        })
        .collect();
    // With two fields read, the order of their scan slots is not measured.
    if fields.len() > 1 {
        return None;
    }
    let mut slots = Slots(0);
    let Scanned {
        mut root,
        record,
        fields: scan_fields,
        env,
        mut query_plan,
    } = scan(front, uuid, fields, &mut slots)?;
    let mut node = front.nodes();
    let mut values = Vec::new();
    for args in &stages {
        node += 1;
        let mut consts = Vec::new();
        let mut transform = Document::new();
        for (name, a) in args {
            match a {
                Arg::Const(c) => {
                    let s = slots.next();
                    consts.push((s, Expr::Const(Some(c.clone()))));
                    values.push(Expr::Slot(s));
                    transform.insert(name.clone(), doc! { "$const": c.clone() });
                }
                Arg::Field(f) => {
                    let s = scan_fields.iter().find(|(n, _)| n == f)?.1;
                    values.push(Expr::Slot(s));
                    transform.insert(name.clone(), Bson::String(format!("${f}")));
                }
                _ => return None,
            }
        }
        if !consts.is_empty() {
            root = Stage::Project {
                node,
                binds: consts,
                input: Box::new(root),
            };
        }
        query_plan = doc! {
            "stage": "PROJECTION_DEFAULT",
            "planNodeId": node as i32,
            "transformBy": transform,
            "inputStage": query_plan,
        };
    }
    let result = slots.next();
    let mut make = vec![Expr::SetSpec(names), Expr::Slot(record)];
    make.extend(values);
    root = Stage::Project {
        node,
        binds: vec![(result, Expr::func("makeBsonObj", make))],
        input: Box::new(root),
    };
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
            seek: None,
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
            seek: None,
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
        // A null in the list folds the value to null, and is built
        // (RM-071, measured 2026-11-27); on a dotted path it is not.
        assert!(crate::explain::sbe::Filter::parse_find(
            &doc! { "x": { "$in": [null, 1] } },
            &doc! { "x": { "$in": [null, 1] } }
        )
        .is_some());
        assert!(crate::explain::sbe::Filter::parse_find(
            &doc! { "x.y": { "$in": [null, 1] } },
            &doc! { "x.y": { "$in": [null, 1] } }
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

    fn added(f: &Front, add: bson::Document) -> (String, String) {
        let plan = super::build_added("u", f, &[add]).expect("built");
        let sbp = plan.winning_plan(false);
        let sbp = sbp.get_document("slotBasedPlan").unwrap();
        (
            sbp.get_str("slots").unwrap().to_string(),
            sbp.get_str("stages").unwrap().to_string(),
        )
    }

    /// Two `$addFields`: a node each, one object made at the last.
    #[test]
    fn two_add_fields_make_one_object() {
        let f = front(doc! { "x": { "$eq": 1 } }, 0, 0);
        let plan = super::build_added("u", &f, &[doc! { "z": 1 }, doc! { "w": 1 }]).unwrap();
        let wp = plan.winning_plan(false);
        let stages = wp
            .get_document("slotBasedPlan")
            .unwrap()
            .get_str("stages")
            .unwrap();
        assert_eq!(
            stages,
            "[3] project [s6 = makeBsonObj(MakeObjSpec([z = Set(0), w = Set(1)], Open, NewObj, 0), s1, s4, s5)] \n\
             [3] project [s5 = 1] \n\
             [2] project [s4 = 1] \n\
             [1] filter {traverseF(s3, lambda(l2.0) { ((move(l2.0) == 1) ?: false) }, false)} \n\
             [1] scan s1 s2 none none none none none none [s3 = x] @\"u\" true false "
        );
        let qp = wp.get_document("queryPlan").unwrap();
        assert_eq!(qp.get_i32("planNodeId"), Ok(3));
        assert_eq!(
            qp.get_document("transformBy").unwrap(),
            &doc! { "w": { "$const": 1 } }
        );
        let inner = qp.get_document("inputStage").unwrap();
        assert_eq!(inner.get_str("stage"), Ok("PROJECTION_DEFAULT"));
        assert_eq!(inner.get_i32("planNodeId"), Ok(2));
    }

    /// The slot programs mongod 8.2.11 runs an `aggregate` of a `$match`
    /// and an `$addFields` with under `queryFramework: "sbe"`, measured
    /// 2026-11-14 (RM-542).
    #[test]
    fn add_fields_plans_are_mongods() {
        let eq1 = || doc! { "x": { "$eq": 1 } };
        let filter =
            "[1] filter {traverseF(s3, lambda(l2.0) { ((move(l2.0) == 1) ?: false) }, false)} \n";
        assert_eq!(
            added(&front(eq1(), 0, 0), doc! { "z": 1 }),
            (
                "$$RESULT=s5 env: {  }".to_string(),
                format!(
                    "[2] project [s5 = makeBsonObj(MakeObjSpec([z = Set(0)], Open, NewObj, 0), s1, s4)] \n\
                     [2] project [s4 = 1] \n{filter}\
                     [1] scan s1 s2 none none none none none none [s3 = x] @\"u\" true false "
                )
            )
        );
        assert_eq!(
            added(&front(doc! {}, 0, 0), doc! { "z": 1 }).1,
            "[2] project [s4 = makeBsonObj(MakeObjSpec([z = Set(0)], Open, NewObj, 0), s1, s3)] \n\
             [2] project [s3 = 1] \n\
             [1] scan s1 s2 none none none none none none [] @\"u\" true false "
        );
        assert_eq!(
            added(&front(eq1(), 0, 0), doc! { "z": "$y" }).1,
            "[2] project [s5 = makeBsonObj(MakeObjSpec([z = Set(0)], Open, NewObj, 0), s1, s3)] \n\
             [1] filter {traverseF(s4, lambda(l2.0) { ((move(l2.0) == 1) ?: false) }, false)} \n\
             [1] scan s1 s2 none none none none none none [s3 = y, s4 = x] @\"u\" true false "
        );
        assert_eq!(
            added(&front(eq1(), 0, 2), doc! { "z": 1 }).1,
            format!(
                "[3] project [s6 = makeBsonObj(MakeObjSpec([z = Set(0)], Open, NewObj, 0), s1, s5)] \n\
                 [3] project [s5 = 1] \n\
                 [2] limit s4 \n{filter}\
                 [1] scan s1 s2 none none none none none none [s3 = x] @\"u\" true false "
            )
        );
    }

    /// A sort on one top-level field: `sort` over a `project` of its key,
    /// the key's field read first (mongod 8.2.11, measured 2026-11-26).
    #[test]
    fn sort_plans_are_mongods() {
        let gt1 = || front(doc! { "x": { "$gt": 1 } }, 0, 0);
        let sorted = |f: &Front, field: &str, desc: bool| {
            let plan = super::build_sorted("u", f, &[(field.to_string(), desc)]).expect("built");
            let sbp = plan.winning_plan(false);
            let sbp = sbp.get_document("slotBasedPlan").unwrap();
            (
                sbp.get_str("slots").unwrap().to_string(),
                sbp.get_str("stages").unwrap().to_string(),
            )
        };
        assert_eq!(
            sorted(&gt1(), "y", false),
            (
                "$$RESULT=s1 env: {  }".to_string(),
                "[2] sort [s5] [asc] [s1] \n\
                 [2] project [s5 = getSortKeyAsc(s3)] \n\
                 [1] filter {traverseF(s4, lambda(l2.0) { ((move(l2.0) > 1) ?: false) }, false)} \n\
                 [1] scan s1 s2 none none none none none none [s3 = y, s4 = x] @\"u\" true false "
                    .to_string()
            )
        );
        assert_eq!(
            sorted(&front(doc! { "x": { "$gt": 1 } }, 0, 2), "y", true),
            (
                "$$RESULT=s1 env: { s6 = 2 }".to_string(),
                "[2] sort [s5] [desc] [s1] s6 \n\
                 [2] project [s5 = getSortKeyDesc(s3)] \n\
                 [1] filter {traverseF(s4, lambda(l2.0) { ((move(l2.0) > 1) ?: false) }, false)} \n\
                 [1] scan s1 s2 none none none none none none [s3 = y, s4 = x] @\"u\" true false "
                    .to_string()
            )
        );
        let keys = |k: &[(&str, bool)]| -> Vec<(String, bool)> {
            k.iter().map(|(f, d)| (f.to_string(), *d)).collect()
        };
        let multi = |f: &Front, k: &[(&str, bool)]| {
            let plan = super::build_sorted("u", f, &keys(k)).expect("built");
            let sbp = plan.winning_plan(false);
            let sbp = sbp.get_document("slotBasedPlan").unwrap();
            sbp.get_str("stages").unwrap().to_string()
        };
        // Two keys and three (mongod 8.2.11, measured 2026-11-28).
        assert_eq!(
            multi(&gt1(), &[("z", false), ("y", true)]),
            "[2] sort [s6, s7] [asc, desc] [s1] \n\
             [2] project [s6 = getSortKeyAsc(s4), s7 = getSortKeyDesc(s3), s8 = ((!((isArray(s4) ?: false)) || !((isArray(s3) ?: false))) || fail(2, \"cannot sort with keys that are parallel arrays\"))] \n\
             [1] filter {traverseF(s5, lambda(l2.0) { ((move(l2.0) > 1) ?: false) }, false)} \n\
             [1] scan s1 s2 none none none none none none [s3 = y, s4 = z, s5 = x] @\"u\" true false "
        );
        let a1 = front(doc! { "a": { "$eq": 1 } }, 0, 0);
        let three = multi(&a1, &[("z", false), ("y", false), ("b", true)]);
        assert!(three.contains(
            "s10 = ((((((isArray(s5) ?: false) <=> false) + ((isArray(s4) ?: false) <=> false)) + ((isArray(s3) ?: false) <=> false)) <= 1) || fail(2, "
        ), "{three}");
        // A skip: `limitskip none` over the sort, its slot after the sort's.
        assert_eq!(
            sorted(&front(doc! { "x": { "$gt": 1 } }, 1, 0), "y", false),
            (
                "$$RESULT=s1 env: { s6 = 1 }".to_string(),
                "[3] limitskip none s6 \n\
                 [2] sort [s5] [asc] [s1] \n\
                 [2] project [s5 = getSortKeyAsc(s3)] \n\
                 [1] filter {traverseF(s4, lambda(l2.0) { ((move(l2.0) > 1) ?: false) }, false)} \n\
                 [1] scan s1 s2 none none none none none none [s3 = y, s4 = x] @\"u\" true false "
                    .to_string()
            )
        );
        // A skip and a limit: the sort's top-k a `let` of their sum, the
        // frame after the filter's, and no limit stage over the skip, the
        // skip's slot first (Linux mongod 8.2.11 and 8.2.12, 2026-11-28).
        let (slots, stages) = sorted(&front(doc! { "x": { "$gt": 1 } }, 1, 2), "y", false);
        assert_eq!(slots, "$$RESULT=s1 env: { s6 = 1, s7 = 2 }");
        assert_eq!(
            stages,
            "[3] limitskip none s6 \n\
             [2] sort [s5] [asc] [s1] \n    \
             let [\n        \
             l4.0 = (s7 + s6) \n    \
             ] \n    \
             in \n        \
             if typeMatch(l4.0, 262144) \n        \
             then makeOwn(move(l4.0)) \n        \
             else 9223372036854775807ll \n\
             \n\
             [2] project [s5 = getSortKeyAsc(s3)] \n\
             [1] filter {traverseF(s4, lambda(l2.0) { ((move(l2.0) > 1) ?: false) }, false)} \n\
             [1] scan s1 s2 none none none none none none [s3 = y, s4 = x] @\"u\" true false "
        );
        // Not built: a path.
        assert!(super::build_sorted("u", &gt1(), &[("d.e".to_string(), false)]).is_none());
    }
}
