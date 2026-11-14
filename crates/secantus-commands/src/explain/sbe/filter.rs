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
//! for a `find` (`Filter::parse_find`), also an `$in` of such values but
//! null: `isMember` against an `InList` the plan's environment binds, its
//! values in the command's own order (the `parsedQuery` sorts them).
//! Anything else returns `None` and the pipeline is explained classically.

use super::expr::{BinOp, Expr, Slot};
use bson::{Bson, Document};
use std::collections::BTreeSet;

/// What a leaf tests its path's values with.
#[derive(Clone, Debug)]
enum Test {
    Compare(BinOp, Bson),
    /// `$in`: the values as the command gave them.
    In(Vec<Bson>),
}

/// One comparison of the filter.
#[derive(Clone, Debug)]
struct Leaf {
    path: Vec<String>,
    test: Test,
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
    let (name, value) = cond.iter().next().filter(|_| cond.len() == 1)?;
    if name == "$in" {
        let values = raw_in(raw?, path)?;
        let printable = values
            .iter()
            .all(|v| comparable(v) && super::print::list_value(v).is_some());
        return printable.then_some(Leaf {
            path: parts,
            test: Test::In(values),
        });
    }
    let op = op_of(name)?;
    comparable(value).then(|| Leaf {
        path: parts,
        test: Test::Compare(op, value.clone()),
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
    /// outside what is built here.
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
        2 * self.lambdas()
    }

    fn lambdas(&self) -> u32 {
        self.leaves.iter().map(|l| l.path.len() as u32).sum()
    }

    /// The program, reading each top-level field from `slot_of`; each
    /// `$in`'s list takes the next slot of `slots`, in the program's order.
    pub fn program(
        &self,
        slot_of: &dyn Fn(&str) -> Option<Slot>,
        slots: &mut dyn FnMut() -> Slot,
    ) -> Option<Expr> {
        let mut frame = self.lambdas();
        let mut built = Vec::new();
        for l in &self.leaves {
            let top = Expr::Slot(slot_of(&l.path[0])?);
            let test: Box<dyn Fn(Expr) -> Expr> = match &l.test {
                Test::In(values) => {
                    let list = Expr::InList(slots(), values.clone());
                    Box::new(move |v| Expr::func("isMember", vec![v, list.clone()]))
                }
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
            built.push(traverse(top, l, &*test, 0, &mut frame));
        }
        let mut out = built.pop()?;
        while let Some(prev) = built.pop() {
            out = Expr::bin(BinOp::LogicAnd, prev, out);
        }
        Some(out)
    }
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
        expression(&f.program(&slot, &mut || 9).unwrap())
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
    }

    /// What is not built here is left to the classic explain.
    #[test]
    fn other_filters_are_not_built() {
        for parsed in [
            doc! {},
            doc! { "a": { "$eq": null } },
            doc! { "a": { "$in": [1, 2] } },
            doc! { "a": { "$not": { "$eq": 2 } } },
            doc! { "a.0": { "$gt": 1 } },
            doc! { "a": { "$gt": f64::NAN } },
            doc! { "$or": [{ "a": { "$eq": 1 } }, { "b": { "$eq": 1 } }] },
        ] {
            assert!(Filter::parse(&parsed).is_none(), "{parsed}");
        }
    }
}
