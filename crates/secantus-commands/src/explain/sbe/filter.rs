//! The filter mongod 8.2.11 puts over a collection scan it runs in SBE,
//! built as `generateFilter` (`gen_filter.cpp`) builds it.
//!
//! The filter is the query as the planner canonicalised it (`parsedQuery`):
//! an `$and` is right-nested `&&`s in the canonical order of its children,
//! and a comparison on a path is a `traverseF` per path component, each
//! level's lambda reading the field of the one before with `getField`, the
//! innermost one comparing (`((move(l) > c) ?: false)`).
//!
//! Lambda frames: generating the program takes one frame id per lambda
//! before the lambdas take theirs, so with `n` lambdas they are numbered
//! `n + 1`, `n + 2`, ... in the order the program prints them (measured
//! 2026-11-08: one comparison is `l2.0`, an `$and` of three `l4.0` to
//! `l6.0`, `d.e` `l3.0` and `l4.0`).
//!
//! Built here: `$eq`, `$lt`, `$lte`, `$gt` and `$gte` of a number (not NaN),
//! a string or a boolean, on a path of named fields, and `$and`s of those;
//! given the command's filter (`Filter::parse_find`), also an `$in` of such values (and
//! null, on a one-field path, with the value folded as `$eq: null` folds it): `isMember` against an `InList` the plan's environment binds, its
//! values in the command's own order (the `parsedQuery` sorts them). An
//! `$eq: null` on a one-field path (`typeMatch` folding undefined and
//! Nothing to null) and the `$not: {$eq: c}` a `$ne` canonicalises to
//! (`!(traverseF(...))`) are built too (RM-071), and `$exists` on a one-field
//! path (`exists(s)` of its slot; `$exists: false` is `!(exists(s))`).
//! Anything else returns `None` and the pipeline is explained classically.

use super::expr::{BinOp, Expr, Slot};
use bson::{Bson, Document};
use std::collections::BTreeSet;

/// `getBSONTypeMask(Undefined)`: the mask `typeMatch` folds to null in an
/// `$eq: null`.
const UNDEFINED_MASK: u32 = 1024;

/// What a leaf tests its path's values with.
#[derive(Clone, Debug)]
enum Test {
    Compare(BinOp, Bson),
    /// `$in`: the values as the command gave them. One holding null tests
    /// the value folded as `$eq: null` folds it (Nothing and undefined
    /// read as null), on a one-field path only.
    In(Vec<Bson>),
    /// `$eq: null`: null, undefined or missing (`generateComparison`'s
    /// null case, `typeMatch` of undefined folding it to null).
    Null,
    /// `$exists: true`: `exists(s)` of the field's slot, no traversal; a
    /// `$exists: false` is its `$not`. One-field path only.
    Exists,
}

/// One comparison of the filter.
#[derive(Clone, Debug)]
struct Leaf {
    path: Vec<String>,
    test: Test,
    /// Under a `$not` (a `$ne` canonicalises to `$not: {$eq}`): the
    /// traversal's answer negated, `!(traverseF(...))`.
    negated: bool,
}

impl Leaf {
    /// A null test on a dotted path: `null_traverse`'s shape.
    fn dotted_null(&self) -> bool {
        matches!(self.test, Test::Null) && self.path.len() > 1
    }

    /// The frame ids generating the program takes before the lambdas take
    /// theirs. An `exists` leaf has no lambda but takes one all the same; a
    /// dotted null test takes `2n - 1` for `n` components (measured
    /// 2026-11-28: `d.e` alone numbers its lambdas from `l4`, `d.e.f` from
    /// `l6`).
    fn taken(&self) -> u32 {
        let n = self.path.len() as u32;
        if self.dotted_null() {
            2 * n - 1
        } else {
            n
        }
    }

    /// The frame ids its program numbers: a lambda per component, and a
    /// dotted null test's `let` per level strictly inside the path.
    fn used(&self) -> u32 {
        let n = self.path.len() as u32;
        if self.dotted_null() {
            2 * n - 2
        } else {
            n
        }
    }
}

/// A filter, ready to be given its slots.
#[derive(Clone, Debug)]
pub(crate) struct Filter {
    /// The conjuncts in canonical order; one for a lone comparison.
    leaves: Vec<Leaf>,
}

fn op_of(name: &str) -> Option<BinOp> {
    Some(match name {
        "$eq" => BinOp::Eq,
        "$lt" => BinOp::Lt,
        "$lte" => BinOp::Lte,
        "$gt" => BinOp::Gt,
        "$gte" => BinOp::Gte,
        _ => return None,
    })
}

/// The constants a comparison here takes: what the printer reproduces.
fn comparable(v: &Bson) -> bool {
    match v {
        Bson::Double(d) => !d.is_nan(),
        Bson::Int32(_) | Bson::Int64(_) | Bson::Boolean(_) => true,
        Bson::String(s) => !s.contains(['"', '\\']) && !s.chars().any(char::is_control),
        _ => false,
    }
}

fn leaf(clause: &Document, raw: Option<&Document>) -> Option<Leaf> {
    let (path, cond) = clause.iter().next().filter(|_| clause.len() == 1)?;
    if path.starts_with('$') {
        return None;
    }
    let parts: Vec<String> = path.split('.').map(str::to_string).collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let Bson::Document(cond) = cond else {
        return None;
    };
    let (mut name, mut value) = cond.iter().next().filter(|_| cond.len() == 1)?;
    let mut negated = false;
    if name == "$not" {
        // Only the `$not: {$eq: c}` a `$ne` becomes (measured 2026-11-14).
        let Bson::Document(inner) = value else {
            return None;
        };
        let (n, v) = inner.iter().next().filter(|_| inner.len() == 1)?;
        if n != "$eq" && n != "$exists" {
            return None;
        }
        (name, value, negated) = (n, v, true);
    }
    if name == "$exists" {
        // The planner canonicalises `$exists: false` to `$not: {$exists:
        // true}`; measured on a one-field path only (2026-11-28).
        return (matches!(value, Bson::Boolean(true)) && parts.len() == 1).then_some(Leaf {
            path: parts,
            test: Test::Exists,
            negated,
        });
    }
    if name == "$eq" && matches!(value, Bson::Null) {
        // A dotted path's null test reads the levels above it as well
        // (`null_traverse`, measured 2026-11-28).
        return Some(Leaf {
            path: parts,
            test: Test::Null,
            negated,
        });
    }
    if name == "$in" {
        if negated {
            return None;
        }
        let values = raw_in(raw?, path)?;
        // A null member: measured on a one-field path only (2026-11-27); a
        // dotted path's test reads the levels above it as well.
        let null_ok = parts.len() == 1;
        let printable = values.iter().all(|v| {
            (matches!(v, Bson::Null) && null_ok)
                || (comparable(v) && super::print::list_value(v).is_some())
        });
        return printable.then_some(Leaf {
            path: parts,
            test: Test::In(values),
            negated,
        });
    }
    let op = op_of(name)?;
    comparable(value).then(|| Leaf {
        path: parts,
        test: Test::Compare(op, value.clone()),
        negated,
    })
}

/// The one `$in` on `path` in the command's filter `raw`, at its top level
/// or in a top-level `$and`; `None` when there is not exactly one.
fn raw_in(raw: &Document, path: &str) -> Option<Vec<Bson>> {
    let mut found = Vec::new();
    let mut look = |d: &Document| {
        if let Some(Bson::Document(cond)) = d.get(path) {
            if let Some(Bson::Array(values)) = cond.get("$in") {
                found.push(values.clone());
            }
        }
    };
    look(raw);
    if let Some(Bson::Array(clauses)) = raw.get("$and") {
        for c in clauses {
            if let Bson::Document(c) = c {
                look(c);
            }
        }
    }
    (found.len() == 1).then(|| found.remove(0))
}

impl Filter {
    /// The filter of `parsed` (a `parsedQuery`); `None` when it is empty or
    /// outside what is built here; the tests' form, without an `$in`.
    #[cfg(test)]
    pub fn parse(parsed: &Document) -> Option<Filter> {
        Self::parse_with(parsed, None)
    }

    /// The filter of a `find` whose command filter is `raw` and whose
    /// `parsedQuery` is `parsed`: an `$in` is built too.
    pub fn parse_find(parsed: &Document, raw: &Document) -> Option<Filter> {
        Self::parse_with(parsed, Some(raw))
    }

    fn parse_with(parsed: &Document, raw: Option<&Document>) -> Option<Filter> {
        let leaves = match parsed.get("$and") {
            Some(Bson::Array(clauses)) if parsed.len() == 1 => clauses
                .iter()
                .map(|c| leaf(c.as_document()?, raw))
                .collect::<Option<Vec<Leaf>>>()?,
            _ => vec![leaf(parsed, raw)?],
        };
        Some(Filter { leaves })
    }

    /// The top-level fields the filter reads, sorted.
    pub fn fields(&self) -> BTreeSet<String> {
        self.leaves.iter().map(|l| l.path[0].clone()).collect()
    }

    /// How many frame ids generating the program takes: two per lambda.
    pub fn frames(&self) -> u32 {
        self.leaves.iter().map(|l| l.taken() + l.used()).sum()
    }

    fn lambdas(&self) -> u32 {
        self.leaves.iter().map(Leaf::taken).sum()
    }

    /// The program, reading each top-level field from `slot_of`; each
    /// `$in`'s list takes the next slot of `slots`, in the program's order.
    /// `root` is the scan's record slot, which a dotted null test reads
    /// (`None` where that shape is unmeasured: the program is not built).
    pub fn program(
        &self,
        root: Option<Slot>,
        slot_of: &dyn Fn(&str) -> Option<Slot>,
        slots: &mut dyn FnMut() -> Slot,
    ) -> Option<Expr> {
        let mut frame = self.lambdas();
        let mut built = Vec::new();
        for l in &self.leaves {
            let top = Expr::Slot(slot_of(&l.path[0])?);
            if let Test::Exists = l.test {
                // A frame id is taken for it all the same (measured
                // 2026-11-28: beside one `$in`, the `$in`'s lambda is `l3.0`).
                let e = Expr::func("exists", vec![top]);
                built.push(if l.negated { Expr::Not(Box::new(e)) } else { e });
                continue;
            }
            if l.dotted_null() {
                let t = null_traverse(top, Expr::Slot(root?), &l.path, 0, &mut frame);
                built.push(if l.negated { Expr::Not(Box::new(t)) } else { t });
                continue;
            }
            let test: Box<dyn Fn(Expr) -> Expr> = match &l.test {
                Test::In(values) => {
                    let list = Expr::InList(slots(), values.clone());
                    let nullish = values.iter().any(|v| matches!(v, Bson::Null));
                    Box::new(move |v| {
                        let v = if nullish { fold_null(v) } else { v };
                        Expr::func("isMember", vec![v, list.clone()])
                    })
                }
                // `((if (typeMatch(l, 1024) ?: true) then null else
                // move(l) == null) ?: false)`: Nothing and undefined read
                // as null.
                Test::Null => Box::new(|v| {
                    Expr::fill_empty(
                        Expr::bin(BinOp::Eq, fold_null(v), Expr::Const(Some(Bson::Null))),
                        Expr::Const(Some(Bson::Boolean(false))),
                    )
                }),
                Test::Exists => unreachable!("an `exists` leaf is built above"),
                Test::Compare(op, value) => {
                    let (op, value) = (*op, value.clone());
                    Box::new(move |v| {
                        Expr::fill_empty(
                            Expr::bin(op, v, Expr::Const(Some(value.clone()))),
                            Expr::Const(Some(Bson::Boolean(false))),
                        )
                    })
                }
            };
            let t = traverse(top, l, &*test, 0, &mut frame);
            built.push(if l.negated { Expr::Not(Box::new(t)) } else { t });
        }
        let mut out = built.pop()?;
        while let Some(prev) = built.pop() {
            out = Expr::bin(BinOp::LogicAnd, prev, out);
        }
        Some(out)
    }
}

/// `Test::Null`'s comparison of a lambda's local:
/// `((if (typeMatch(l, 1024) ?: true) then null else move(l) == null) ?: false)`.
fn null_test(v: Expr) -> Expr {
    Expr::fill_empty(
        Expr::bin(BinOp::Eq, fold_null(v), Expr::Const(Some(Bson::Null))),
        Expr::Const(Some(Bson::Boolean(false))),
    )
}

/// `(f(e) ?: false)`.
fn guard(f: &'static str, e: Expr) -> Expr {
    Expr::fill_empty(
        Expr::func(f, vec![e]),
        Expr::Const(Some(Bson::Boolean(false))),
    )
}

/// The null test of a dotted path, as mongod 8.2.11 prints it (measured
/// 2026-11-28 on `d.e` and `d.e.f`): level `depth`'s value `input` is
/// traversed only when it is an object or an array, and otherwise the path
/// is missing, which matches null unless its `parent` is an array:
///
/// `if (typeMatch(x, 24) ?: false) then traverseF(x, lambda(l) { if
/// (isObject(l) ?: false) then <next> else false }, false) else
/// !((isArray(parent) ?: false))`
///
/// where `<next>` is `traverseF(getField(move(l), "<leaf>"), lambda(m) {
/// <null test of m> }, false)` before the last component, and otherwise
/// the same shape again over `let [k = getField(l, "<next>")]`, with `l`
/// the parent. At the top `x` is the field's slot and the parent the record.
fn null_traverse(
    input: Expr,
    parent: Expr,
    path: &[String],
    depth: usize,
    frame: &mut u32,
) -> Expr {
    *frame += 1;
    let mine = *frame;
    let next = &path[depth + 1];
    let inner = if depth + 2 == path.len() {
        *frame += 1;
        let leaf = *frame;
        Expr::func(
            "traverseF",
            vec![
                Expr::func(
                    "getField",
                    vec![
                        Expr::local(mine, true),
                        Expr::Const(Some(Bson::String(next.clone()))),
                    ],
                ),
                Expr::Lambda {
                    frame: leaf,
                    body: Box::new(null_test(Expr::local(leaf, true))),
                },
                Expr::Const(Some(Bson::Boolean(false))),
            ],
        )
    } else {
        *frame += 1;
        let bound = *frame;
        let body = null_traverse(
            Expr::local(bound, true),
            Expr::local(mine, true),
            path,
            depth + 1,
            frame,
        );
        Expr::let_(
            bound,
            Expr::func(
                "getField",
                vec![
                    Expr::local(mine, false),
                    Expr::Const(Some(Bson::String(next.clone()))),
                ],
            ),
            body,
        )
    };
    let lambda_body = Expr::if_(
        guard("isObject", Expr::local(mine, false)),
        inner,
        Expr::Const(Some(Bson::Boolean(false))),
    );
    let test_input = match &input {
        Expr::Local { frame, .. } => Expr::local(*frame, false),
        other => other.clone(),
    };
    Expr::if_(
        Expr::fill_empty(
            Expr::TypeMatch(Box::new(test_input), 24),
            Expr::Const(Some(Bson::Boolean(false))),
        ),
        Expr::func(
            "traverseF",
            vec![
                input,
                Expr::Lambda {
                    frame: mine,
                    body: Box::new(lambda_body),
                },
                Expr::Const(Some(Bson::Boolean(false))),
            ],
        ),
        Expr::Not(Box::new(guard("isArray", parent))),
    )
}

/// `if (typeMatch(l, 1024) ?: true) then null else move(l)`: the lambda's
/// value with Nothing and undefined read as null.
fn fold_null(v: Expr) -> Expr {
    let Expr::Local { frame, .. } = v else {
        unreachable!("a traversal's lambda reads its local")
    };
    let read = Expr::Local {
        frame,
        moved: false,
    };
    let nullish = Expr::fill_empty(
        Expr::TypeMatch(Box::new(read), UNDEFINED_MASK),
        Expr::Const(Some(Bson::Boolean(true))),
    );
    Expr::If(
        Box::new(nullish),
        Box::new(Expr::Const(Some(Bson::Null))),
        Box::new(v),
    )
}

/// `traverseF` over `input`, the field `leaf.path[depth]` of the level
/// above; frames are taken outermost first.
fn traverse(
    input: Expr,
    leaf: &Leaf,
    test: &dyn Fn(Expr) -> Expr,
    depth: usize,
    frame: &mut u32,
) -> Expr {
    *frame += 1;
    let mine = *frame;
    let local = Expr::Local {
        frame: mine,
        moved: true,
    };
    let body = if depth + 1 == leaf.path.len() {
        test(local)
    } else {
        let field = Expr::func(
            "getField",
            vec![
                local,
                Expr::Const(Some(Bson::String(leaf.path[depth + 1].clone()))),
            ],
        );
        traverse(field, leaf, test, depth + 1, frame)
    };
    Expr::func(
        "traverseF",
        vec![
            input,
            Expr::Lambda {
                frame: mine,
                body: Box::new(body),
            },
            Expr::Const(Some(Bson::Boolean(false))),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::Filter;
    use crate::explain::sbe::print::expression;
    use bson::doc;

    fn text(parsed: bson::Document, slots: &[(&str, u32)]) -> String {
        let f = Filter::parse(&parsed).unwrap();
        let slot = |n: &str| slots.iter().find(|(k, _)| *k == n).map(|(_, s)| *s);
        expression(&f.program(Some(1), &slot, &mut || 9).unwrap())
    }

    /// The filters mongod 8.2.11 prints, measured 2026-11-08.
    #[test]
    fn filters_print_as_mongod_prints_them() {
        assert_eq!(
            text(doc! { "a": { "$gt": 1 } }, &[("a", 3)]),
            "traverseF(s3, lambda(l2.0) { ((move(l2.0) > 1) ?: false) }, false)"
        );
        assert_eq!(
            text(
                doc! { "$and": [{ "b": { "$lt": 2 } }, { "a": { "$gt": 1 } }, { "s": { "$gt": "0" } }] },
                &[("a", 3), ("b", 4), ("s", 5)]
            ),
            "(traverseF(s4, lambda(l4.0) { ((move(l4.0) < 2) ?: false) }, false) && \
             (traverseF(s3, lambda(l5.0) { ((move(l5.0) > 1) ?: false) }, false) && \
             traverseF(s5, lambda(l6.0) { ((move(l6.0) > \"0\") ?: false) }, false)))"
        );
        assert_eq!(
            text(doc! { "d.e": { "$gt": 1 } }, &[("d", 3)]),
            "traverseF(s3, lambda(l3.0) { traverseF(getField(move(l3.0), \"e\"), \
             lambda(l4.0) { ((move(l4.0) > 1) ?: false) }, false) }, false)"
        );
        assert_eq!(
            text(doc! { "a": { "$gt": 1.5 } }, &[("a", 3)]),
            "traverseF(s3, lambda(l2.0) { ((move(l2.0) > 1.5L) ?: false) }, false)"
        );
        assert_eq!(
            text(doc! { "t": { "$eq": true } }, &[("t", 3)]),
            "traverseF(s3, lambda(l2.0) { ((move(l2.0) == true) ?: false) }, false)"
        );
        // RM-071, measured 2026-11-14.
        assert_eq!(
            text(doc! { "a": { "$not": { "$eq": 2 } } }, &[("a", 3)]),
            "!(traverseF(s3, lambda(l2.0) { ((move(l2.0) == 2) ?: false) }, false))"
        );
        assert_eq!(
            text(doc! { "a": { "$not": { "$eq": null } } }, &[("a", 3)]),
            "!(traverseF(s3, lambda(l2.0) { ((\n    if (typeMatch(l2.0, 1024) ?: true) \n    \
             then null \n    else move(l2.0) \n== null) ?: false) }, false))"
        );
        assert_eq!(
            text(doc! { "a": { "$eq": null } }, &[("a", 3)]),
            "traverseF(s3, lambda(l2.0) { ((\n    if (typeMatch(l2.0, 1024) ?: true) \n    \
             then null \n    else move(l2.0) \n== null) ?: false) }, false)"
        );
    }

    /// An `$in` holding null folds the value as `$eq: null` does; measured
    /// against mongod 8.2.11 on 2026-11-27 (RM-071). A dotted one is not
    /// built.
    #[test]
    fn an_in_holding_null_folds_the_value() {
        let f = doc! { "a": { "$in": [null, 1] } };
        let built = Filter::parse_find(&f, &f).unwrap();
        let slot = |n: &str| (n == "a").then_some(3);
        assert_eq!(
            expression(&built.program(Some(1), &slot, &mut || 4).unwrap()),
            "traverseF(s3, lambda(l2.0) { isMember(\n    if (typeMatch(l2.0, 1024) ?: true) \n    \
             then null \n    else move(l2.0) \n, s4) }, false)"
        );
        let dotted = doc! { "x.y": { "$in": [null, 1] } };
        assert!(Filter::parse_find(&dotted, &dotted).is_none());
    }

    /// `$exists` on a one-field path reads the field's slot with no
    /// traversal, and still takes a frame id; measured against mongod
    /// 8.2.11 on 2026-11-28 (RM-071). A dotted one is not built.
    #[test]
    fn exists_reads_the_slot() {
        assert_eq!(
            text(doc! { "c": { "$exists": true } }, &[("c", 3)]),
            "exists(s3)"
        );
        assert_eq!(
            text(doc! { "c": { "$not": { "$exists": true } } }, &[("c", 3)]),
            "!(exists(s3))"
        );
        assert_eq!(
            text(
                doc! { "$and": [{ "a": { "$gt": 2 } }, { "c": { "$not": { "$exists": true } } }] },
                &[("a", 3), ("c", 4)]
            ),
            "(traverseF(s3, lambda(l3.0) { ((move(l3.0) > 2) ?: false) }, false) && !(exists(s4)))"
        );
        assert!(Filter::parse(&doc! { "c.d": { "$exists": true } }).is_none());
    }

    /// A null test on a dotted path traverses only objects and arrays,
    /// and a missing level matches unless its parent is an array; measured
    /// against mongod 8.2.11 on 2026-11-28 (RM-071), whose plans print
    /// these (whitespace aside).
    #[test]
    fn a_dotted_null_test_guards_each_level() {
        let flat = |s: &str| s.split_whitespace().collect::<String>();
        assert_eq!(
            flat(&text(doc! { "d.e": { "$eq": null } }, &[("d", 3)])),
            flat(
                "if (typeMatch(s3, 24) ?: false) then traverseF(s3, lambda(l4.0) { \
                 if (isObject(l4.0) ?: false) then traverseF(getField(move(l4.0), \"e\"), \
                 lambda(l5.0) { ((if (typeMatch(l5.0, 1024) ?: true) then null else move(l5.0) \
                 == null) ?: false) }, false) else false }, false) else !((isArray(s1) ?: false))"
            )
        );
        assert_eq!(
            flat(&text(doc! { "d.e.f": { "$eq": null } }, &[("d", 3)])),
            flat(
                "if (typeMatch(s3, 24) ?: false) then traverseF(s3, lambda(l6.0) { \
                 if (isObject(l6.0) ?: false) then let [l7.0 = getField(l6.0, \"e\")] in \
                 if (typeMatch(l7.0, 24) ?: false) then traverseF(move(l7.0), lambda(l8.0) { \
                 if (isObject(l8.0) ?: false) then traverseF(getField(move(l8.0), \"f\"), \
                 lambda(l9.0) { ((if (typeMatch(l9.0, 1024) ?: true) then null else move(l9.0) \
                 == null) ?: false) }, false) else false }, false) \
                 else !((isArray(move(l6.0)) ?: false)) else false }, false) \
                 else !((isArray(s1) ?: false))"
            )
        );
        let and = flat(&text(
            doc! { "$and": [{ "d.e": { "$eq": null } }, { "a": { "$gt": 1 } }] },
            &[("a", 3), ("d", 4)],
        ));
        assert!(
            and.contains("lambda(l5.0)") && and.contains("lambda(l7.0){((move(l7.0)>1)"),
            "{and}"
        );
    }

    /// What is not built here is left to the classic explain.
    #[test]
    fn other_filters_are_not_built() {
        for parsed in [
            doc! {},
            doc! { "a": { "$in": [1, 2] } },
            doc! { "a": { "$not": { "$gt": 2 } } },
            doc! { "a.0": { "$gt": 1 } },
            doc! { "a": { "$gt": f64::NAN } },
            doc! { "$or": [{ "a": { "$eq": 1 } }, { "b": { "$eq": 1 } }] },
        ] {
            assert!(Filter::parse(&parsed).is_none(), "{parsed}");
        }
    }
}
