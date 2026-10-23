//! Which part of a pipeline mongod runs in its slot-based engine (SBE), and
//! the switch the evaluator reads to answer as SBE does there.
//!
//! mongod 8.2 has two execution engines, and they do not agree: SBE fails
//! `$mod` by zero with 4848403 where the classic engine says 16610, answers
//! `$regexMatch` with no regex `null` where classic says `false`, and so on
//! (RM-280). Which engine runs a stage is decided by
//! `sbe_pushdown.cpp`'s `findSbeCompatibleStagesForPushdown` under the
//! default framework control, `trySbeRestricted`:
//!
//! - the find layer takes the leading `$match`es, a `$sort`, `$skip` /
//!   `$limit`, and one inclusion or computed `$project`;
//! - after it, only `$group` and `$lookup` are pushed down, one after
//!   another while each is compatible;
//! - nothing is pushed unless at least one of those is, and then the find
//!   layer runs in SBE too, provided its filter and projection are
//!   compatible.
//!
//! A stage is compatible when every expression in it is (`COMPATIBLE`; the
//! others mark the expression context `notCompatible` when they parse).
//! Every rule here was measured on mongod 8.2.11 with `explain`
//! (`explainVersion` "2" is SBE), 2026-10-23.

use crate::fallback::Fallback;
use bson::{Bson, Document};
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// Whether the stage evaluating now runs in SBE.
pub fn active() -> bool {
    ACTIVE.with(Cell::get)
}

/// Set the engine for the stages that follow; returns the previous one.
pub fn set_active(on: bool) -> bool {
    ACTIVE.with(|a| a.replace(on))
}

/// Puts the previous engine back when dropped.
pub struct Scope(bool);

impl Drop for Scope {
    fn drop(&mut self) {
        set_active(self.0);
    }
}

/// Run what follows in `on`'s engine until the guard drops.
pub fn enter(on: bool) -> Scope {
    Scope(set_active(on))
}

/// The marker stage a caller puts in a pipeline where the engine changes:
/// `{"$_secantusEngine": true}` starts SBE, `false` returns to classic.
pub const ENGINE_STAGE: &str = "$_secantusEngine";

/// The expression operators SBE compiles. Any other operator, anywhere in a
/// stage, keeps that stage in the classic engine.
const COMPATIBLE: &[&str] = &[
    "$abs",
    "$acos",
    "$acosh",
    "$add",
    "$and",
    "$anyElementTrue",
    "$arrayToObject",
    "$asin",
    "$asinh",
    "$atan",
    "$atan2",
    "$atanh",
    "$bsonSize",
    "$ceil",
    "$cmp",
    "$concat",
    "$concatArrays",
    "$cond",
    "$const",
    "$cos",
    "$cosh",
    "$dateAdd",
    "$dateDiff",
    "$dateFromParts",
    "$dateFromString",
    "$dateSubtract",
    "$dateToParts",
    "$dateToString",
    "$dateTrunc",
    "$dayOfMonth",
    "$dayOfWeek",
    "$dayOfYear",
    "$degreesToRadians",
    "$divide",
    "$eq",
    "$exp",
    "$floor",
    "$gt",
    "$gte",
    "$hour",
    "$ifNull",
    "$in",
    "$indexOfBytes",
    "$indexOfCP",
    "$isArray",
    "$isNumber",
    "$isoDayOfWeek",
    "$isoWeek",
    "$isoWeekYear",
    "$let",
    "$literal",
    "$ln",
    "$log10",
    "$lt",
    "$lte",
    "$ltrim",
    "$millisecond",
    "$minute",
    "$mod",
    "$month",
    "$multiply",
    "$ne",
    "$not",
    "$objectToArray",
    "$or",
    "$pow",
    "$radiansToDegrees",
    "$rand",
    "$range",
    "$regexFind",
    "$regexFindAll",
    "$regexMatch",
    "$replaceOne",
    "$reverseArray",
    "$round",
    "$rtrim",
    "$second",
    "$setDifference",
    "$setEquals",
    "$setIntersection",
    "$setIsSubset",
    "$setUnion",
    "$sigmoid",
    "$sin",
    "$sinh",
    "$sortArray",
    "$sqrt",
    "$strLenBytes",
    "$strLenCP",
    "$strcasecmp",
    "$substr",
    "$substrBytes",
    "$substrCP",
    "$subtract",
    "$switch",
    "$tan",
    "$tanh",
    "$toLower",
    "$toUpper",
    "$trim",
    "$trunc",
    "$tsIncrement",
    "$tsSecond",
    "$week",
    "$year",
];

/// Accumulators SBE does not run: a `$group` holding one stays classic.
const CLASSIC_ACCUMULATORS: &[&str] = &["$accumulator", "$median", "$percentile"];

/// Whether SBE compiles every operator in `expr`.
pub fn expression_compatible(expr: &Bson) -> bool {
    match expr {
        Bson::Array(items) => items.iter().all(expression_compatible),
        Bson::Document(d) => match d.keys().next() {
            Some(op) if op.starts_with('$') => {
                if !COMPATIBLE.contains(&op.as_str()) {
                    return false;
                }
                if matches!(op.as_str(), "$literal" | "$const") {
                    return true;
                }
                d.values().all(expression_compatible)
            }
            _ => d.values().all(expression_compatible),
        },
        _ => true,
    }
}

/// Whether a `$match` filter compiles: its `$expr`s, and no `$jsonSchema`.
fn filter_compatible(filter: &Document) -> bool {
    filter.iter().all(|(k, v)| match k.as_str() {
        "$expr" => expression_compatible(v),
        "$jsonSchema" => false,
        "$and" | "$or" | "$nor" => match v {
            Bson::Array(items) => items.iter().all(|i| match i {
                Bson::Document(d) => filter_compatible(d),
                _ => true,
            }),
            _ => true,
        },
        _ => true,
    })
}

fn projection_compatible(spec: &Document) -> bool {
    spec.values().all(|v| match v {
        Bson::Document(d) if d.keys().next().is_some_and(|k| !k.starts_with('$')) => {
            projection_compatible(d)
        }
        other => expression_compatible(other),
    })
}

/// A projection value that is an inclusion or exclusion flag, or a
/// sub-projection of flags.
fn is_flag(v: &Bson) -> bool {
    match v {
        Bson::Boolean(_)
        | Bson::Int32(_)
        | Bson::Int64(_)
        | Bson::Double(_)
        | Bson::Decimal128(_) => true,
        Bson::Document(d) if d.keys().next().is_some_and(|k| !k.starts_with('$')) => {
            d.values().all(is_flag)
        }
        _ => false,
    }
}

fn falsy(v: &Bson) -> bool {
    match v {
        Bson::Boolean(b) => !b,
        Bson::Int32(n) => *n == 0,
        Bson::Int64(n) => *n == 0,
        Bson::Double(f) => *f == 0.0,
        Bson::Decimal128(d) => d.to_string().trim_start_matches('-') == "0",
        _ => false,
    }
}

/// A `$project` the find layer takes: anything but an exclusion, and behind
/// a `$sort` nothing computed.
fn absorbable_projection(spec: &Document, after_sort: bool) -> bool {
    fn excludes(spec: &Document, top: bool) -> bool {
        spec.iter().any(|(k, v)| match v {
            Bson::Document(d) if d.keys().next().is_some_and(|k| !k.starts_with('$')) => {
                excludes(d, false)
            }
            v => falsy(v) && !(top && k == "_id"),
        })
    }
    let only_id_excluded = spec.len() == 1 && spec.get("_id").is_some_and(falsy);
    let computed = spec.values().any(|v| !is_flag(v));
    !excludes(spec, true) && !only_id_excluded && !(after_sort && computed)
}

fn accumulator_compatible(acc: &Bson) -> bool {
    let Bson::Document(d) = acc else {
        return true;
    };
    let Some((op, arg)) = d.iter().next() else {
        return true;
    };
    if CLASSIC_ACCUMULATORS.contains(&op.as_str()) {
        return false;
    }
    match (op.as_str(), arg) {
        // `sortBy` is a sort pattern, not an expression.
        ("$top" | "$bottom" | "$topN" | "$bottomN", Bson::Document(spec)) => spec
            .iter()
            .filter(|(k, _)| k.as_str() != "sortBy")
            .all(|(_, v)| expression_compatible(v)),
        _ => expression_compatible(arg),
    }
}

fn group_compatible(spec: &Document) -> bool {
    spec.iter().all(|(k, v)| {
        if k == "_id" {
            expression_compatible(v)
        } else {
            accumulator_compatible(v)
        }
    })
}

fn stage(s: &Bson) -> Option<(&str, &Bson)> {
    let d = s.as_document().filter(|d| d.len() == 1)?;
    d.iter().next().map(|(k, v)| (k.as_str(), v))
}

/// Whether `stage` is pushed down after the find layer, and whether it ends
/// the chain (a stage mongod expands into a `$group` and a stage SBE does not
/// take: `$bucket`'s and `$sortByCount`'s `$sort`, `$count`'s `$project`).
fn pushed(
    name: &str,
    spec: &Bson,
    next: Option<&Bson>,
    foreign_is_view: &dyn Fn(&str) -> bool,
) -> Option<bool> {
    match (name, spec) {
        ("$group", Bson::Document(g)) => group_compatible(g).then_some(false),
        ("$count", _) => Some(true),
        ("$sortByCount", e) => expression_compatible(e).then_some(true),
        ("$bucket", Bson::Document(b)) => {
            let group_by = b.get("groupBy").is_none_or(expression_compatible);
            let output = match b.get("output") {
                Some(Bson::Document(o)) => o.values().all(accumulator_compatible),
                _ => true,
            };
            (group_by && output).then_some(true)
        }
        ("$lookup", Bson::Document(l)) => {
            if l.contains_key("pipeline")
                || !l.contains_key("localField")
                || !l.contains_key("foreignField")
            {
                return None;
            }
            if let Some(Bson::String(from)) = l.get("from") {
                if foreign_is_view(from) {
                    return None;
                }
            }
            // A following `$unwind` of the joined array is absorbed into the
            // `$lookup`, which then stays classic.
            let as_path = l.get_str("as").unwrap_or_default();
            let unwinds_it = next.and_then(stage).is_some_and(|(n, s)| {
                n == "$unwind"
                    && match s {
                        Bson::String(p) => p.strip_prefix('$') == Some(as_path),
                        Bson::Document(u) => {
                            u.get_str("path").ok().and_then(|p| p.strip_prefix('$'))
                                == Some(as_path)
                        }
                        _ => false,
                    }
            });
            (!unwinds_it).then_some(false)
        }
        _ => None,
    }
}

/// How many leading stages of `pipeline` (as mongod reorders its front) run
/// in SBE: the find layer and the chain pushed down after it. `None` when the
/// whole pipeline runs in the classic engine.
pub fn pushdown(pipeline: &[Bson], foreign_is_view: &dyn Fn(&str) -> bool) -> Option<usize> {
    let mut at = 0;
    let mut layer_ok = true;
    let name_at = |i: usize| pipeline.get(i).and_then(stage);
    while let Some(("$match", spec)) = name_at(at) {
        layer_ok &= spec.as_document().is_some_and(filter_compatible);
        at += 1;
    }
    let mut sorted = false;
    if let Some(("$sort", _)) = name_at(at) {
        at += 1;
        sorted = true;
        // A `$match` behind the `$sort` moves ahead of it.
        while let Some(("$match", spec)) = name_at(at) {
            layer_ok &= spec.as_document().is_some_and(filter_compatible);
            at += 1;
        }
    }
    let skip_limit = |at: &mut usize| {
        while let Some(("$skip" | "$limit", _)) = name_at(*at) {
            *at += 1;
        }
    };
    skip_limit(&mut at);
    if let Some(("$project", Bson::Document(p))) = name_at(at) {
        if absorbable_projection(p, sorted) {
            layer_ok &= projection_compatible(p);
            at += 1;
            skip_limit(&mut at);
        }
    }
    let layer = at;
    while let Some((name, spec)) = name_at(at) {
        match pushed(name, spec, pipeline.get(at + 1), foreign_is_view) {
            Some(ends) => {
                at += 1;
                if ends {
                    break;
                }
            }
            None => break,
        }
    }
    (at > layer && layer_ok).then_some(at)
}

/// `pipeline` with [`ENGINE_STAGE`] markers putting its stages `start..end`
/// in SBE, for an executor that reads the marker.
pub fn mark(pipeline: Vec<Bson>, start: usize, end: usize) -> Vec<Bson> {
    let mut out = Vec::with_capacity(pipeline.len() + 2);
    for (i, s) in pipeline.into_iter().enumerate() {
        if i == start {
            out.push(Bson::Document(bson::doc! { ENGINE_STAGE: true }));
        }
        if i == end {
            out.push(Bson::Document(bson::doc! { ENGINE_STAGE: false }));
        }
        out.push(s);
    }
    if out.len() <= end + 1 {
        out.push(Bson::Document(bson::doc! { ENGINE_STAGE: false }));
    }
    out
}

/// The checks mongod's SBE stage builder makes while it builds the pushed
/// stages, before any document is read: so they fail on an empty collection
/// too, and their errors have no executor wrapper (measured 8.2.11,
/// 2026-10-23). `stages` are those [`pushdown`] puts in SBE.
pub fn build_check(stages: &[Bson], vars: &Document) -> Result<(), Fallback> {
    let b = Build { vars };
    for s in stages {
        let Some((name, spec)) = stage(s) else {
            continue;
        };
        match (name, spec) {
            ("$match", Bson::Document(f)) => b.filter(f)?,
            ("$project", Bson::Document(p)) => b.projection(p)?,
            ("$group", Bson::Document(g)) => {
                for (k, v) in g {
                    if k == "_id" {
                        b.expr(v)?;
                    } else {
                        b.accumulator(v)?;
                    }
                }
            }
            ("$sortByCount", e) => b.expr(e)?,
            ("$bucket", Bson::Document(d)) => {
                if let Some(g) = d.get("groupBy") {
                    b.expr(g)?;
                }
                if let Some(Bson::Document(o)) = d.get("output") {
                    for v in o.values() {
                        b.accumulator(v)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

struct Build<'a> {
    vars: &'a Document,
}

impl Build<'_> {
    fn constant(&self, e: &Bson) -> Option<Bson> {
        let bound: Vec<String> = self.vars.keys().cloned().collect();
        if !crate::aggregate::is_constant_expression(e, &bound) {
            return None;
        }
        crate::expressions::evaluate(&Document::new(), e, self.vars).ok()
    }

    fn filter(&self, f: &Document) -> Result<(), Fallback> {
        for (k, v) in f {
            match (k.as_str(), v) {
                ("$expr", e) => self.expr(e)?,
                ("$and" | "$or" | "$nor", Bson::Array(items)) => {
                    for i in items {
                        if let Bson::Document(d) = i {
                            self.filter(d)?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn projection(&self, p: &Document) -> Result<(), Fallback> {
        for v in p.values() {
            match v {
                Bson::Document(d) if d.keys().next().is_some_and(|k| !k.starts_with('$')) => {
                    self.projection(d)?
                }
                other => self.expr(other)?,
            }
        }
        Ok(())
    }

    fn accumulator(&self, acc: &Bson) -> Result<(), Fallback> {
        let Bson::Document(d) = acc else {
            return Ok(());
        };
        let Some((op, arg)) = d.iter().next() else {
            return Ok(());
        };
        if let (
            "$firstN" | "$lastN" | "$maxN" | "$minN" | "$topN" | "$bottomN",
            Bson::Document(spec),
        ) = (op.as_str(), arg)
        {
            for (k, v) in spec {
                if k != "sortBy" && k != "n" {
                    self.expr(v)?;
                }
            }
            if let Some(n) = spec.get("n").and_then(|n| self.constant(n)) {
                let positive = match n {
                    Bson::Int32(x) => x > 0,
                    Bson::Int64(x) => x > 0,
                    Bson::Double(f) => {
                        f.is_finite() && f.trunc() == f && (1.0..9.2e18).contains(&f)
                    }
                    Bson::Decimal128(_) => {
                        crate::decimal::decimal_to_i64_exact(&n).is_some_and(|x| x > 0)
                    }
                    _ => false,
                };
                if !positive {
                    return Err(Fallback::mongo(
                        7548606,
                        "parameter 'n' must be coercible to a positive 64-bit integer",
                    )
                    .bare());
                }
            }
            return Ok(());
        }
        if let ("$top" | "$bottom", Bson::Document(spec)) = (op.as_str(), arg) {
            for (k, v) in spec {
                if k != "sortBy" {
                    self.expr(v)?;
                }
            }
            return Ok(());
        }
        self.expr(arg)
    }

    /// Children first, as the stage builder visits them.
    fn expr(&self, e: &Bson) -> Result<(), Fallback> {
        match e {
            Bson::Array(items) => items.iter().try_for_each(|i| self.expr(i)),
            Bson::Document(d) => {
                let Some((op, arg)) = d.iter().next().filter(|(k, _)| k.starts_with('$')) else {
                    return d.values().try_for_each(|v| self.expr(v));
                };
                if matches!(op.as_str(), "$literal" | "$const") {
                    return Ok(());
                }
                match arg {
                    Bson::Document(a) => a.values().try_for_each(|v| self.expr(v))?,
                    other => self.expr(other)?,
                }
                self.operator(op, arg)
            }
            _ => Ok(()),
        }
    }

    fn operator(&self, op: &str, arg: &Bson) -> Result<(), Fallback> {
        let bare = |code: i32, msg: String| Err(Fallback::mongo(code, msg).bare());
        let Bson::Document(spec) = arg else {
            return Ok(());
        };
        let named = spec.keys().next().is_some_and(|k| !k.starts_with('$'));
        match op {
            "$year" | "$month" | "$dayOfMonth" | "$hour" | "$minute" | "$second"
            | "$millisecond" | "$dayOfWeek" | "$dayOfYear" | "$week" | "$isoWeek"
            | "$isoWeekYear" | "$isoDayOfWeek"
                if named =>
            {
                // Any other constant the classic optimizer has refused already
                // (40517, 40485), even with no collection; null it lets by.
                if let Some(tz) = spec.get("timezone").and_then(|t| self.constant(t)) {
                    if matches!(tz, Bson::Null | Bson::Undefined) {
                        return bare(
                            5157900,
                            format!("{op} parameter 'timezone' must be a string"),
                        );
                    }
                }
            }
            // A constant timezone of these two the classic optimizer has
            // checked first.
            "$dateToString" => {
                if let Some(f) = spec.get("format").and_then(|f| self.constant(f)) {
                    match f {
                        Bson::Null | Bson::Undefined => {}
                        Bson::String(f) => {
                            if let Some(err) = crate::expressions::to_string_format_error(&f) {
                                return Err(err.bare());
                            }
                        }
                        _ => {
                            return bare(
                                4997902,
                                "$dateToString parameter 'format' must be a string".into(),
                            )
                        }
                    }
                }
            }
            "$dateFromString" => {
                if let Some(f) = spec.get("format").and_then(|f| self.constant(f)) {
                    match f {
                        Bson::Null | Bson::Undefined => {}
                        Bson::String(f) => {
                            if let Err((code, msg)) = crate::timelib::validate_format(&f) {
                                return bare(code, msg);
                            }
                        }
                        _ => {
                            return bare(
                                4997802,
                                "$dateFromString requires that 'format' be a string".into(),
                            )
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn p(stages: Vec<Document>) -> Vec<Bson> {
        stages.into_iter().map(Bson::Document).collect()
    }

    fn no_view(_: &str) -> bool {
        false
    }

    #[test]
    fn measured_pushdown() {
        let g = doc! {"$group": {"_id": "$a"}};
        let cases: Vec<(Vec<Document>, Option<usize>)> = vec![
            (vec![g.clone()], Some(1)),
            (vec![doc! {"$match": {"a": 1}}, g.clone()], Some(2)),
            (
                vec![doc! {"$match": {"$expr": {"$toInt": "$a"}}}, g.clone()],
                None,
            ),
            (
                vec![
                    doc! {"$sort": {"a": 1}},
                    doc! {"$project": {"a": 1}},
                    g.clone(),
                ],
                Some(3),
            ),
            (
                vec![
                    doc! {"$project": {"a": 1}},
                    doc! {"$sort": {"a": 1}},
                    g.clone(),
                ],
                None,
            ),
            (vec![doc! {"$project": {"a": 0}}, g.clone()], None),
            (vec![doc! {"$project": {"_id": 0}}, g.clone()], None),
            (vec![doc! {"$project": {"_id": 1}}, g.clone()], Some(2)),
            (
                vec![
                    doc! {"$sort": {"a": 1}},
                    doc! {"$project": {"x": {"$add": ["$a", 1]}}},
                    g.clone(),
                ],
                None,
            ),
            (
                vec![
                    doc! {"$sort": {"a": 1}},
                    doc! {"$match": {"a": 1}},
                    g.clone(),
                ],
                Some(3),
            ),
            (vec![doc! {"$addFields": {"x": 1}}, g.clone()], None),
            (
                vec![doc! {"$limit": 3}, doc! {"$match": {"a": 1}}, g.clone()],
                None,
            ),
            (
                vec![doc! {"$project": {"a": 1}}, doc! {"$limit": 2}, g.clone()],
                Some(3),
            ),
            (vec![doc! {"$match": {"a": 1}}], None),
            (vec![g.clone(), g.clone()], Some(2)),
            (
                vec![g.clone(), doc! {"$group": {"_id": {"$toInt": "$_id"}}}],
                Some(1),
            ),
            (
                vec![doc! {"$group": {"_id": {"$size": "$a"}}}, g.clone()],
                None,
            ),
            (
                vec![g.clone(), doc! {"$sort": {"_id": 1}}, g.clone()],
                Some(1),
            ),
            (vec![doc! {"$count": "n"}, g.clone()], Some(1)),
            (
                vec![
                    doc! {"$lookup": {"from": "f", "localField": "a", "foreignField": "k", "as": "j"}},
                ],
                Some(1),
            ),
            (
                vec![
                    doc! {"$lookup": {"from": "f", "localField": "a", "foreignField": "k", "as": "j"}},
                    doc! {"$unwind": "$j"},
                ],
                None,
            ),
            (
                vec![doc! {"$lookup": {"from": "f", "as": "j", "pipeline": []}}],
                None,
            ),
            (
                vec![
                    doc! {"$group": {"_id": null, "m": {"$median": {"input": "$a", "method": "approximate"}}}},
                ],
                None,
            ),
            (
                vec![doc! {"$bucketAuto": {"groupBy": "$a", "buckets": 2}}],
                None,
            ),
        ];
        for (stages, want) in cases {
            assert_eq!(pushdown(&p(stages.clone()), &no_view), want, "{stages:?}");
        }
    }

    #[test]
    fn a_view_keeps_lookup_classic() {
        let l = p(vec![
            doc! {"$lookup": {"from": "vw", "localField": "a", "foreignField": "k", "as": "j"}},
        ]);
        assert_eq!(pushdown(&l, &|c| c == "vw"), None);
    }
}
