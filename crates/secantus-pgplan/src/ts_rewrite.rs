//! `ts_rewrite`: PostgreSQL's `tsquery_rewrite.c` and the `QTNode` helpers
//! it rides on (`tsquery_util.c`), transcribed.
//!
//! The rewrite works on PostgreSQL's n-ary form of a query: AND / OR runs
//! flattened (`QTNTernary`) and every node's children sorted into one
//! canonical order (`QTNSort`), then turned back into binary operators
//! (`QTNBinary`). That order is what the output shows, so it is reproduced
//! exactly: an operand sorts by the legacy CRC-32 of its text (as a signed
//! int32), then its bytes; operators before operands; a higher operator
//! number first. A node's child 0 is its RIGHT operand, as in PostgreSQL's
//! flat polish layout.

use crate::fts::Query;
use crate::Result;
use std::cmp::Ordering;

const OP_NOT: u8 = 1;
const OP_AND: u8 = 2;
const OP_OR: u8 = 3;
const OP_PHRASE: u8 = 4;

#[derive(Debug, Clone)]
enum Item {
    Val {
        lex: String,
        prefix: bool,
        weight: u8,
        crc: i32,
    },
    Opr {
        op: u8,
        distance: u16,
    },
}

/// `QTNode`.
#[derive(Debug, Clone)]
struct Qtn {
    item: Item,
    child: Vec<Qtn>,
    sign: u32,
    nochange: bool,
}

/// The CRC-32 table (`pg_crc32_table`, polynomial 0xEDB88320).
fn crc_table() -> &'static [u32; 256] {
    static T: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u32; 256];
        for (n, slot) in t.iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    })
}

/// `INIT` / `COMP` / `FIN_LEGACY_CRC32`: the reflected table driven the
/// "normal" way round, which is what tsquery operands are stamped with.
fn legacy_crc32(bytes: &[u8]) -> u32 {
    let t = crc_table();
    let mut crc: u32 = 0xFFFF_FFFF;
    for b in bytes {
        crc = t[(((crc >> 24) ^ u32::from(*b)) & 0xFF) as usize] ^ (crc << 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// `QT2QTN`.
fn from_query(q: &Query) -> Qtn {
    let opr = |op: u8, distance: u16, child: Vec<Qtn>| {
        let sign = child.iter().fold(0, |s, c| s | c.sign);
        Qtn {
            item: Item::Opr { op, distance },
            child,
            sign,
            nochange: false,
        }
    };
    match q {
        Query::Val {
            lex,
            prefix,
            weight,
        } => {
            let crc = legacy_crc32(lex.as_bytes()) as i32;
            Qtn {
                item: Item::Val {
                    lex: lex.clone(),
                    prefix: *prefix,
                    weight: *weight,
                    crc,
                },
                child: Vec::new(),
                sign: 1u32 << ((crc as u32) % 32),
                nochange: false,
            }
        }
        // A stored query carries no stop-word placeholders.
        Query::Stop => opr(OP_AND, 0, Vec::new()),
        Query::Not(x) => opr(OP_NOT, 0, vec![from_query(x)]),
        Query::And(l, r) => opr(OP_AND, 0, vec![from_query(r), from_query(l)]),
        Query::Or(l, r) => opr(OP_OR, 0, vec![from_query(r), from_query(l)]),
        Query::Phrase(l, r, d) => opr(OP_PHRASE, *d, vec![from_query(r), from_query(l)]),
    }
}

/// `QTN2QT` over a binary tree.
fn to_query(n: Qtn) -> Query {
    match n.item {
        Item::Val {
            lex,
            prefix,
            weight,
            ..
        } => Query::Val {
            lex,
            prefix,
            weight,
        },
        Item::Opr { op, distance } => {
            let mut it = n.child.into_iter();
            let right = Box::new(to_query(it.next().expect("an operand")));
            if op == OP_NOT {
                return Query::Not(right);
            }
            let left = Box::new(to_query(it.next().expect("two operands")));
            match op {
                OP_AND => Query::And(left, right),
                OP_OR => Query::Or(left, right),
                _ => Query::Phrase(left, right, distance),
            }
        }
    }
}

/// `QTNodeCompare`.
fn compare(a: &Qtn, b: &Qtn) -> Ordering {
    match (&a.item, &b.item) {
        // QI_OPR (2) sorts before QI_VAL (1): the higher type first.
        (Item::Opr { .. }, Item::Val { .. }) => Ordering::Less,
        (Item::Val { .. }, Item::Opr { .. }) => Ordering::Greater,
        (
            Item::Opr {
                op: ao,
                distance: ad,
            },
            Item::Opr {
                op: bo,
                distance: bd,
            },
        ) => {
            if ao != bo {
                return bo.cmp(ao);
            }
            if a.child.len() != b.child.len() {
                return b.child.len().cmp(&a.child.len());
            }
            for (x, y) in a.child.iter().zip(&b.child) {
                let r = compare(x, y);
                if r != Ordering::Equal {
                    return r;
                }
            }
            if *ao == OP_PHRASE && ad != bd {
                return bd.cmp(ad);
            }
            Ordering::Equal
        }
        (
            Item::Val {
                lex: al, crc: ac, ..
            },
            Item::Val {
                lex: bl, crc: bc, ..
            },
        ) => {
            if ac != bc {
                return bc.cmp(ac);
            }
            al.as_bytes().cmp(bl.as_bytes())
        }
    }
}

/// `QTNSort`. PostgreSQL's `qsort` is an insertion sort below seven
/// elements, so a stable sort agrees with it wherever ties can be told
/// apart (operands differing only in weight or prefix).
fn sort(n: &mut Qtn) {
    let Item::Opr { op, .. } = n.item else { return };
    for c in &mut n.child {
        sort(c);
    }
    if n.child.len() > 1 && op != OP_PHRASE {
        n.child.sort_by(compare);
    }
}

/// `QTNEq`.
fn eq(a: &Qtn, b: &Qtn) -> bool {
    a.sign == b.sign && compare(a, b) == Ordering::Equal
}

/// `QTNTernary`: flatten nested runs of one AND / OR.
fn ternary(n: &mut Qtn) {
    let Item::Opr { op, .. } = n.item else { return };
    for c in &mut n.child {
        ternary(c);
    }
    if op != OP_AND && op != OP_OR {
        return;
    }
    let mut out = Vec::with_capacity(n.child.len());
    for c in std::mem::take(&mut n.child) {
        match c.item {
            Item::Opr { op: cop, .. } if cop == op => out.extend(c.child),
            _ => out.push(c),
        }
    }
    n.child = out;
}

/// `QTNBinary`: back to two operands per operator, in PostgreSQL's order.
fn binary(n: &mut Qtn) {
    let Item::Opr { op, .. } = n.item else { return };
    for c in &mut n.child {
        binary(c);
    }
    while n.child.len() > 2 {
        let c0 = n.child[0].clone();
        let c1 = n.child[1].clone();
        let sign = c0.sign | c1.sign;
        n.child[0] = Qtn {
            item: Item::Opr { op, distance: 0 },
            child: vec![c0, c1],
            sign,
            nochange: false,
        };
        let last = n.child.pop().expect("more than two");
        n.child[1] = last;
    }
}

fn clear_nochange(n: &mut Qtn) {
    n.nochange = false;
    for c in &mut n.child {
        clear_nochange(c);
    }
}

fn marked(subs: &Qtn) -> Qtn {
    let mut s = subs.clone();
    s.nochange = true;
    s
}

/// `findeq`: replace `node` by `subs` where it equals `ex`, or -- AND / OR
/// being commutative -- where `ex`'s children are a subset of its own.
fn findeq(mut node: Qtn, ex: &Qtn, subs: Option<&Qtn>) -> Option<Qtn> {
    if node.sign & ex.sign != ex.sign || node.nochange {
        return Some(node);
    }
    match (&node.item, &ex.item) {
        (Item::Opr { op: no, .. }, Item::Opr { op: eo, .. }) => {
            if no != eo {
                return Some(node);
            }
            if node.child.len() == ex.child.len() {
                if eq(&node, ex) {
                    return subs.map(marked);
                }
            } else if node.child.len() > ex.child.len() && !ex.child.is_empty() {
                let mut matched = vec![false; node.child.len()];
                let mut nmatched = 0;
                let (mut i, mut j) = (0, 0);
                while i < node.child.len() && j < ex.child.len() {
                    match compare(&node.child[i], &ex.child[j]) {
                        Ordering::Equal => {
                            matched[i] = true;
                            nmatched += 1;
                            i += 1;
                            j += 1;
                        }
                        Ordering::Less => i += 1,
                        Ordering::Greater => break,
                    }
                }
                if nmatched == ex.child.len() {
                    let kept = std::mem::take(&mut node.child)
                        .into_iter()
                        .zip(matched)
                        .filter(|(_, m)| !m)
                        .map(|(c, _)| c);
                    node.child = kept.collect();
                    if let Some(s) = subs {
                        node.child.push(marked(s));
                    }
                    sort(&mut node);
                }
            }
            Some(node)
        }
        (Item::Val { crc: nc, .. }, Item::Val { crc: ec, .. }) => {
            if nc != ec || !eq(&node, ex) {
                Some(node)
            } else {
                subs.map(marked)
            }
        }
        _ => Some(node),
    }
}

/// `dofindsubquery`: match at the node, else in its children, dropping
/// emptied subtrees and an operator left with one operand (but a NOT).
fn findsubquery(root: Qtn, ex: &Qtn, subs: Option<&Qtn>) -> Option<Qtn> {
    let mut root = findeq(root, ex, subs)?;
    let Item::Opr { op, .. } = root.item else {
        return Some(root);
    };
    if root.nochange {
        return Some(root);
    }
    root.child = std::mem::take(&mut root.child)
        .into_iter()
        .filter_map(|c| findsubquery(c, ex, subs))
        .collect();
    match root.child.len() {
        0 => None,
        1 if op != OP_NOT => root.child.pop(),
        _ => Some(root),
    }
}

fn prepared(q: &Query) -> Qtn {
    let mut t = from_query(q);
    ternary(&mut t);
    sort(&mut t);
    t
}

fn finish(tree: Option<Qtn>) -> Option<Query> {
    tree.map(|mut t| {
        binary(&mut t);
        to_query(t)
    })
}

/// `ts_rewrite(query, target, substitute)` (`tsquery_rewrite`). An empty
/// query or target leaves the query as it is, unsorted.
pub(crate) fn rewrite(
    query: &Option<Query>,
    target: &Option<Query>,
    subs: &Option<Query>,
) -> Option<Query> {
    let (Some(q), Some(ex)) = (query, target) else {
        return query.clone();
    };
    let subs = subs.as_ref().map(from_query);
    finish(findsubquery(prepared(q), &prepared(ex), subs.as_ref()))
}

/// `ts_rewrite(query, select)` (`tsquery_rewrite_query`): each row of
/// `(target, substitute)` applied in turn, the tree re-flattened and
/// re-sorted after each; a row with a NULL is skipped, and the rows stop
/// once nothing is left.
pub(crate) fn rewrite_rows(
    query: &Option<Query>,
    rows: impl IntoIterator<Item = Result<Option<(Option<Query>, Option<Query>)>>>,
) -> Result<Option<Query>> {
    let Some(q) = query else {
        return Ok(None);
    };
    let mut tree = Some(prepared(q));
    for row in rows {
        let Some(t) = tree.take() else { break };
        let Some((ex, subs)) = row? else {
            tree = Some(t);
            continue;
        };
        let Some(ex) = ex else {
            tree = Some(t);
            continue;
        };
        let subs = subs.as_ref().map(from_query);
        tree = findsubquery(t, &prepared(&ex), subs.as_ref());
        if let Some(t) = &mut tree {
            clear_nochange(t);
            ternary(t);
            sort(t);
        }
    }
    Ok(finish(tree))
}
