//! mongod's Boolean expression simplifier over a parsed query
//! (`internalQueryEnableBooleanExpressionsSimplifier`, on by default in
//! 8.2; `src/mongo/db/query/boolean_simplification/` and
//! `expression_simplifier.cpp`).
//!
//! mongod reads the optimised match expression as a tree of `$and` / `$or`
//! / `$nor` over leaf predicates, gives each distinct leaf a bit, and
//! rewrites the tree through its disjunctive normal form: a `Maxterm` is a
//! set of `Minterm`s, each a pair of bitsets (which predicates the term
//! names, and the value each must have), and `Quine-McCluskey` merges the
//! terms that differ in one bit. What it prints is the result, when the
//! result is smaller than the tree it started from.
//!
//! Measured on mongod 8.2.11 (2026-11-24, `tools/probes/boolean_simplifier.py`
//! and the cases in `tests/backlog/test_rm_found_2026_11_23_rm699.py`):
//!
//! * a leaf is a predicate on a path (its `$not` is the same bit, negated),
//!   and two leaves are one bit when they are equal as mongod compares them
//!   (`1` and `1.0`; an `$in`'s values sorted); the first one met names the
//!   bit in the printed result;
//! * an `$or` that names a bit both ways is `$alwaysTrue`, an `$and` that
//!   does so is `$alwaysFalse`, whatever else it holds (the two leaves of
//!   one node share a bitset, which cannot hold both);
//! * a node's DNF is its children's, combined (an `$or`: their union; an
//!   `$and`: their product, a term that needs a bit both ways dropped), then
//!   a term that another term absorbs (`a` over `a & b`) is dropped, and
//!   then Quine-McCluskey merges terms that differ in one bit (`a & b` and
//!   `a & !b` are `a`), over and over. A term that merging made does not
//!   absorb the terms it was made beside (`a & c` stays beside `c`), which
//!   is why the result is not the minimal cover;
//! * with `internalQueryDoNotOpenContainedOrsInSimplifier` (true), a tree
//!   whose root is an `$and` comes back with the predicates every term
//!   shares taken out of its `$or`: `c & (a | b)`, not `(a & c) | (b & c)`;
//!   a tree whose root is an `$or` comes back as the terms;
//! * the result replaces the tree only when it is SMALLER (a node, a leaf, a
//!   `$not` each count one), by `internalQueryMaxSizeFactorToSimplify` (1.0);
//! * more than `internalQueryMaximumNumberOfUniquePredicatesToSimplify` (64)
//!   bits, or more than `internalQueryMaximumNumberOfMintermsInSimplifier`
//!   (1,000) terms at any step, leave the tree as it was.
//!
//! This is the simplifier as measured, not a port of mongod's source: the
//! seeded random trees of `tools/probes/boolean_simplifier.py` differ from
//! mongod on about 0.3 percent (RM-778).
//!
//! The result is not optimised again (an `$or` of equalities is not folded
//! into an `$in`), only sorted as every `parsedQuery` is.

use bson::{doc, Bson, Document};
use std::collections::BTreeSet;

/// `internalQueryMaximumNumberOfUniquePredicatesToSimplify`.
const MAX_PREDICATES: usize = 64;
/// `internalQueryMaximumNumberOfMintermsInSimplifier`.
const MAX_MINTERMS: usize = 1000;

/// One node of the tree mongod simplifies.
#[derive(Clone, Debug, PartialEq)]
enum Node {
    /// `$alwaysTrue`, printed `{}`.
    True,
    /// `$alwaysFalse`.
    False,
    /// Predicate number `.0`, required to hold (`true`) or to fail.
    Leaf(usize, bool),
    And(Vec<Node>),
    Or(Vec<Node>),
    /// `$nor`: none of its children holds.
    Nor(Vec<Node>),
}

/// A `Minterm`: the bits a term names, and the value each must have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Term {
    mask: u64,
    vals: u64,
}

type Maxterm = BTreeSet<Term>;

/// The leaf predicates met so far, each once.
#[derive(Default)]
struct Predicates {
    leaves: Vec<Document>,
}

impl Predicates {
    fn index(&mut self, leaf: Document) -> Option<usize> {
        if let Some(i) = self
            .leaves
            .iter()
            .position(|l| super::parsed::same_clause(l, &leaf))
        {
            return Some(i);
        }
        if self.leaves.len() == MAX_PREDICATES {
            return None;
        }
        self.leaves.push(leaf);
        Some(self.leaves.len() - 1)
    }
}

/// Whether `d` is a leaf mongod will not read as a bit: a clause that
/// needs its own index or whose evaluation has side channels.
fn opaque(d: &Document) -> bool {
    d.iter().any(|(k, v)| {
        matches!(k.as_str(), "$text" | "$where")
            || v.as_document().is_some_and(|p| {
                p.keys()
                    .any(|op| matches!(op.as_str(), "$near" | "$nearSphere"))
            })
    })
}

fn read(d: &Document, preds: &mut Predicates) -> Option<Node> {
    if d.is_empty() {
        return Some(Node::True);
    }
    if d.len() == 1 {
        let (key, value) = d.iter().next()?;
        match (key.as_str(), value) {
            ("$and" | "$or" | "$nor", Bson::Array(items)) => {
                let children = items
                    .iter()
                    .map(|i| read(i.as_document()?, preds))
                    .collect::<Option<Vec<_>>>()?;
                return Some(match key.as_str() {
                    "$and" => Node::And(children),
                    "$or" => Node::Or(children),
                    _ => Node::Nor(children),
                });
            }
            ("$alwaysFalse", _) => return Some(Node::False),
            ("$alwaysTrue", _) => return Some(Node::True),
            (path, Bson::Document(pred)) if !path.starts_with('$') => {
                if let (1, Some(Bson::Document(inner))) = (pred.len(), pred.get("$not")) {
                    let mut positive = Document::new();
                    positive.insert(path, inner.clone());
                    if opaque(&positive) {
                        return None;
                    }
                    return Some(Node::Leaf(preds.index(positive)?, false));
                }
            }
            _ => {}
        }
    }
    if opaque(d) {
        return None;
    }
    Some(Node::Leaf(preds.index(d.clone())?, true))
}

fn negate(n: &Node) -> Node {
    match n {
        Node::True => Node::False,
        Node::False => Node::True,
        Node::Leaf(i, p) => Node::Leaf(*i, !p),
        Node::And(c) => Node::Or(c.iter().map(negate).collect()),
        Node::Or(c) => Node::And(c.iter().map(negate).collect()),
        Node::Nor(c) => Node::Or(c.clone()),
    }
}

/// mongod's size of a tree: a node and a leaf count one each (a `$not`
/// of a leaf is no node of its own).
fn size(n: &Node) -> usize {
    match n {
        Node::True | Node::False => 1,
        Node::Leaf(..) => 1,
        Node::And(c) | Node::Or(c) | Node::Nor(c) => 1 + c.iter().map(size).sum::<usize>(),
    }
}

/// The constants a node's own leaves settle: an `$or` naming a bit both
/// ways is true, an `$and` is false, and a constant child decides or leaves
/// its parent (`$alwaysTrue` in an `$and`).
fn settle(n: &Node) -> Node {
    let (children, is_and) = match n {
        Node::And(c) => (c, true),
        Node::Or(c) => (c, false),
        Node::Nor(c) => {
            return match settle(&Node::Or(c.clone())) {
                Node::True => Node::False,
                Node::False => Node::True,
                Node::Or(rest) => Node::Nor(rest),
                other => negate(&other),
            };
        }
        other => return other.clone(),
    };
    let (identity, decisive) = if is_and {
        (Node::True, Node::False)
    } else {
        (Node::False, Node::True)
    };
    let mut kept: Vec<Node> = Vec::new();
    let mut seen: Vec<(usize, bool)> = Vec::new();
    for child in children {
        let child = settle(child);
        if child == decisive {
            return decisive;
        }
        if child == identity {
            continue;
        }
        if let Node::Leaf(i, p) = child {
            if seen.iter().any(|&(j, q)| j == i && q != p) {
                return decisive;
            }
            seen.push((i, p));
        }
        kept.push(child);
    }
    if kept.is_empty() {
        return identity;
    }
    if is_and {
        Node::And(kept)
    } else {
        Node::Or(kept)
    }
}

/// `a` absorbs `b`: every bit `a` names, `b` names the same way.
fn absorbs(a: Term, b: Term) -> bool {
    a != b && a.mask & b.mask == a.mask && b.vals & a.mask == a.vals
}

fn absorb(terms: &Maxterm) -> Maxterm {
    terms
        .iter()
        .filter(|t| !terms.iter().any(|o| absorbs(*o, **t)))
        .copied()
        .collect()
}

/// Quine-McCluskey as mongod runs it: terms with one mask that differ in
/// one bit merge into a term without that bit, and what was merged goes.
fn quine_mccluskey(mut terms: Maxterm) -> Maxterm {
    loop {
        let list: Vec<Term> = terms.iter().copied().collect();
        let mut merged = Maxterm::new();
        let mut used: BTreeSet<Term> = BTreeSet::new();
        for (i, x) in list.iter().enumerate() {
            for y in &list[i + 1..] {
                if x.mask != y.mask {
                    continue;
                }
                let diff = x.vals ^ y.vals;
                if diff.count_ones() == 1 {
                    merged.insert(Term {
                        mask: x.mask & !diff,
                        vals: x.vals & !diff,
                    });
                    used.insert(*x);
                    used.insert(*y);
                }
            }
        }
        if merged.is_empty() {
            return terms;
        }
        terms = terms.difference(&used).copied().collect();
        terms.extend(merged);
    }
}

fn simplified(terms: Maxterm, and: bool) -> Option<Maxterm> {
    if terms.len() > MAX_MINTERMS {
        return None;
    }
    let merged = quine_mccluskey(absorb(&terms));
    // An `$and`'s product is absorbed again once merged; an `$or`'s union
    // is not (`a & c` stays beside the `c` that `c & d` and `c & !d` made).
    Some(if and { absorb(&merged) } else { merged })
}

/// A node's DNF, simplified at every node.
fn dnf(n: &Node) -> Option<Maxterm> {
    match n {
        Node::True => Some([Term { mask: 0, vals: 0 }].into_iter().collect()),
        Node::False => Some(Maxterm::new()),
        Node::Leaf(i, p) => Some(
            [Term {
                mask: 1 << i,
                vals: u64::from(*p) << i,
            }]
            .into_iter()
            .collect(),
        ),
        Node::Nor(c) => dnf(&Node::And(c.iter().map(negate).collect())),
        Node::Or(c) => {
            let mut all = Maxterm::new();
            for child in c {
                all.extend(dnf(child)?);
                if all.len() > MAX_MINTERMS {
                    return None;
                }
            }
            simplified(all, false)
        }
        Node::And(c) => {
            let mut all: Maxterm = [Term { mask: 0, vals: 0 }].into_iter().collect();
            for child in c {
                let theirs = dnf(child)?;
                let mut next = Maxterm::new();
                for x in &all {
                    for y in &theirs {
                        let both = x.mask & y.mask;
                        if (x.vals ^ y.vals) & both != 0 {
                            continue;
                        }
                        next.insert(Term {
                            mask: x.mask | y.mask,
                            vals: x.vals | y.vals,
                        });
                    }
                    if next.len() > MAX_MINTERMS {
                        return None;
                    }
                }
                all = next;
            }
            simplified(all, true)
        }
    }
}

/// The literals of a term, by bit.
fn literals(t: Term) -> Vec<Node> {
    (0..MAX_PREDICATES)
        .filter(|i| t.mask >> i & 1 == 1)
        .map(|i| Node::Leaf(i, t.vals >> i & 1 == 1))
        .collect()
}

fn conjunction(mut lits: Vec<Node>) -> Node {
    match lits.len() {
        0 => Node::True,
        1 => lits.pop().expect("one"),
        _ => Node::And(lits),
    }
}

/// The tree for a result: the terms ORed; with `factor`, the predicates
/// every term shares ANDed outside.
fn tree(terms: &Maxterm, factor: bool) -> Node {
    let list: Vec<Term> = terms.iter().copied().collect();
    if list.is_empty() {
        return Node::False;
    }
    let common = if factor && list.len() > 1 {
        let mask = list.iter().fold(u64::MAX, |m, t| m & t.mask);
        // A bit common to every term, named the same way in each.
        let first = list[0];
        let same = list.iter().fold(mask, |m, t| m & !(t.vals ^ first.vals));
        Term {
            mask: same,
            vals: first.vals & same,
        }
    } else {
        Term { mask: 0, vals: 0 }
    };
    let rest: Vec<Node> = list
        .iter()
        .map(|t| {
            conjunction(literals(Term {
                mask: t.mask & !common.mask,
                vals: t.vals & !common.mask,
            }))
        })
        .collect();
    let body = if rest.len() == 1 {
        rest.into_iter().next().expect("one")
    } else {
        Node::Or(rest)
    };
    if common.mask == 0 {
        return body;
    }
    let mut parts = literals(common);
    parts.push(body);
    Node::And(parts)
}

fn write(n: &Node, preds: &Predicates) -> Document {
    match n {
        Node::True => Document::new(),
        Node::False => doc! { "$alwaysFalse": 1 },
        Node::Leaf(i, true) => preds.leaves[*i].clone(),
        Node::Leaf(i, false) => {
            let positive = &preds.leaves[*i];
            match positive.iter().next() {
                Some((path, inner)) if positive.len() == 1 && !path.starts_with('$') => {
                    let mut out = Document::new();
                    out.insert(path.clone(), doc! { "$not": inner.clone() });
                    out
                }
                _ => doc! { "$nor": [Bson::Document(positive.clone())] },
            }
        }
        Node::And(c) => {
            doc! { "$and": c.iter().map(|x| Bson::Document(write(x, preds))).collect::<Vec<_>>() }
        }
        Node::Or(c) => {
            doc! { "$or": c.iter().map(|x| Bson::Document(write(x, preds))).collect::<Vec<_>>() }
        }
        Node::Nor(c) => {
            doc! { "$nor": c.iter().map(|x| Bson::Document(write(x, preds))).collect::<Vec<_>>() }
        }
    }
}

/// `parsed` (a `parsedQuery`'s tree) simplified as mongod simplifies it,
/// or `None` where mongod leaves the tree as it is.
pub(crate) fn simplify(parsed: &Document) -> Option<Document> {
    let mut preds = Predicates::default();
    let original = read(parsed, &mut preds)?;
    if !matches!(original, Node::And(_) | Node::Or(_) | Node::Nor(_)) {
        return None;
    }
    let settled = settle(&original);
    let result = match settled {
        Node::True | Node::False => settled.clone(),
        _ => {
            let terms = dnf(&settled)?;
            tree(&terms, matches!(settled, Node::And(_)))
        }
    };
    if size(&result) >= size(&original) {
        return None;
    }
    Some(write(&result, &preds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absorption() {
        let d = doc! { "$or": [
            { "$and": [{ "a": { "$eq": 1 } }, { "b": { "$eq": 1 } }] },
            { "a": { "$eq": 1 } },
        ] };
        assert_eq!(simplify(&d), Some(doc! { "a": { "$eq": 1 } }));
    }

    #[test]
    fn contradiction() {
        let d = doc! { "$and": [{ "a": { "$eq": 1 } }, { "a": { "$not": { "$eq": 1 } } }] };
        assert_eq!(simplify(&d), Some(doc! { "$alwaysFalse": 1 }));
    }

    #[test]
    fn no_gain_is_unchanged() {
        let d = doc! { "$or": [
            { "$and": [{ "a": { "$eq": 1 } }, { "b": { "$eq": 1 } }] },
            { "c": { "$eq": 1 } },
        ] };
        assert_eq!(simplify(&d), None);
    }
}
