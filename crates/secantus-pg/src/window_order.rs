//! The order PostgreSQL 15 evaluates a query's windows in, and the row
//! order that leaves behind.
//!
//! PostgreSQL stacks one WindowAgg per window clause, each over a Sort by
//! the clause's PARTITION BY keys then its ORDER BY keys (`create_one_window_
//! path`), skipping the Sort when the rows already arrive in an order that
//! starts with those keys. The clauses go in the order `select_active_windows`
//! sorts them into with `common_prefix_cmp`, so a query with windows and no
//! ORDER BY of its own returns its rows in the LAST sort's order -- which a
//! client sees. Ties go however PostgreSQL's quicksort (`sort_template.h`)
//! leaves them, so that is transcribed too: it also decides which of two tied
//! rows a `row_number()` numbers first.

use std::cmp::Ordering;

use secantus_pgplan::{Nulls, OrderKey, WindowItem};

/// Does `a` name the same key expression as `b` (whatever its direction)?
fn same_expr(a: &OrderKey, b: &OrderKey) -> bool {
    match (&a.expr, &b.expr) {
        (None, None) => a.field == b.field,
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// A sort key: its sortgroupref, whether it is descending, whether NULLs go
/// first, and the key the executor sorts by.
#[derive(Clone)]
struct Clause {
    sortref: usize,
    desc: bool,
    nulls_first: bool,
    key: OrderKey,
}

/// One window clause: the windows that share its PARTITION BY / ORDER BY,
/// and its `uniqueOrder` (the partition keys, then the order keys not among
/// them).
pub(crate) struct WindowClause {
    pub(crate) windows: Vec<usize>,
    unique: Vec<Clause>,
    /// The pathkeys its Sort produces: `unique` without a second key over an
    /// expression already sorted on (`make_pathkeys_for_sortclauses` keeps
    /// one pathkey per equivalence class).
    pub(crate) sort: Vec<OrderKey>,
}

/// The windows grouped into clauses, in the order PostgreSQL runs them.
///
/// `query_order` is the statement's own ORDER BY, whose keys are given
/// sortgroup refs before any window's (`transformSelectStmt` transforms the
/// sort clause first); a window then numbers its ORDER BY keys before its
/// PARTITION BY keys (`transformWindowDefinitions`), and a key expression
/// already numbered keeps its number.
pub(crate) fn window_clauses(
    windows: &[WindowItem],
    query_order: &[OrderKey],
) -> Vec<WindowClause> {
    let mut refs: Vec<OrderKey> = Vec::new();
    let mut sortref = |k: &OrderKey| -> usize {
        match refs.iter().position(|r| same_expr(r, k)) {
            Some(i) => i + 1,
            None => {
                refs.push(k.clone());
                refs.len()
            }
        }
    };
    for k in query_order {
        sortref(k);
    }
    // The definitions in the parser's order (named ones first); `sort_by_key`
    // is stable, so inline ones keep their order of appearance.
    let mut defs: Vec<usize> = (0..windows.len()).collect();
    defs.sort_by_key(|i| windows[*i].def_order);
    let mut clauses: Vec<WindowClause> = Vec::new();
    for wi in defs {
        let w = &windows[wi];
        let order: Vec<Clause> = w
            .order_by
            .iter()
            .map(|k| Clause {
                sortref: sortref(k),
                desc: !k.ascending,
                nulls_first: k.nulls == Nulls::First,
                key: k.clone(),
            })
            .collect();
        // A PARTITION BY key also in the ORDER BY takes that key's direction
        // (`transformGroupClause` copies the sort operator it finds there);
        // otherwise it sorts ascending, NULLs last.
        let partition: Vec<Clause> = w
            .partition_by
            .iter()
            .map(|k| {
                let r = sortref(k);
                match order.iter().find(|o| o.sortref == r) {
                    Some(o) => o.clone(),
                    None => Clause {
                        sortref: r,
                        desc: false,
                        nulls_first: false,
                        key: OrderKey {
                            ascending: true,
                            nulls: Nulls::Last,
                            jsonb: false,
                            ..k.clone()
                        },
                    },
                }
            })
            .collect();
        let same = |a: &Clause, b: &Clause| {
            a.sortref == b.sortref && a.desc == b.desc && a.nulls_first == b.nulls_first
        };
        let mut unique = partition.clone();
        for o in order {
            if !unique.iter().any(|u| same(u, &o)) {
                unique.push(o);
            }
        }
        // Order-equivalent windows are one clause, sorted for once.
        let key = |c: &[Clause]| -> Vec<(usize, bool, bool)> {
            c.iter()
                .map(|c| (c.sortref, c.desc, c.nulls_first))
                .collect()
        };
        if let Some(c) = clauses.iter_mut().find(|c| key(&c.unique) == key(&unique)) {
            c.windows.push(wi);
            continue;
        }
        let mut sort: Vec<OrderKey> = Vec::new();
        let mut seen: Vec<usize> = Vec::new();
        for c in &unique {
            if !seen.contains(&c.sortref) {
                seen.push(c.sortref);
                sort.push(OrderKey {
                    ascending: !c.desc,
                    nulls: if c.nulls_first {
                        Nulls::First
                    } else {
                        Nulls::Last
                    },
                    ..c.key.clone()
                });
            }
        }
        clauses.push(WindowClause {
            windows: vec![wi],
            unique,
            sort,
        });
    }
    // `common_prefix_cmp`: key by key, the higher sortgroupref first, then
    // the descending one (its `>` operator has the higher oid), then NULLS
    // FIRST; a clause that is a prefix of another goes after it. PostgreSQL
    // sorts these few with its qsort, an insertion sort below seven -- stable.
    let cmp = |a: &WindowClause, b: &WindowClause| -> Ordering {
        for (x, y) in a.unique.iter().zip(&b.unique) {
            let o = y
                .sortref
                .cmp(&x.sortref)
                .then(y.desc.cmp(&x.desc))
                .then(y.nulls_first.cmp(&x.nulls_first));
            if o != Ordering::Equal {
                return o;
            }
        }
        b.unique.len().cmp(&a.unique.len())
    };
    pg_qsort(&mut clauses, &mut |a, b| cmp(a, b));
    clauses
}

/// Is `want` a prefix of the order the rows are already in?
pub(crate) fn presorted(want: &[OrderKey], have: &[OrderKey]) -> bool {
    want.len() <= have.len()
        && want
            .iter()
            .zip(have)
            .all(|(w, h)| same_expr(w, h) && w.ascending == h.ascending && w.nulls == h.nulls)
}

/// PostgreSQL's quicksort (`sort_template.h`, Bentley & McIlroy): an
/// insertion sort below seven elements, a pass that returns at once on
/// sorted input, then median-of-three (ninther above forty) partitioning
/// with the equal keys swapped to the ends. Not stable, and transcribed so
/// that tied rows come out where PostgreSQL's do.
pub(crate) fn pg_qsort<T>(a: &mut [T], cmp: &mut dyn FnMut(&T, &T) -> Ordering) {
    let mut lo = 0usize;
    let mut n = a.len();
    loop {
        let s = &mut a[lo..lo + n];
        if n < 7 {
            for pm in 1..n {
                let mut pl = pm;
                while pl > 0 && cmp(&s[pl - 1], &s[pl]) == Ordering::Greater {
                    s.swap(pl, pl - 1);
                    pl -= 1;
                }
            }
            return;
        }
        if (1..n).all(|pm| cmp(&s[pm - 1], &s[pm]) != Ordering::Greater) {
            return;
        }
        let med3 =
            |s: &[T], a: usize, b: usize, c: usize, cmp: &mut dyn FnMut(&T, &T) -> Ordering| {
                if cmp(&s[a], &s[b]) == Ordering::Less {
                    if cmp(&s[b], &s[c]) == Ordering::Less {
                        b
                    } else if cmp(&s[a], &s[c]) == Ordering::Less {
                        c
                    } else {
                        a
                    }
                } else if cmp(&s[b], &s[c]) == Ordering::Greater {
                    b
                } else if cmp(&s[a], &s[c]) == Ordering::Less {
                    a
                } else {
                    c
                }
            };
        let mut pm = n / 2;
        if n > 7 {
            let mut pl = 0;
            let mut pn = n - 1;
            if n > 40 {
                let d = n / 8;
                pl = med3(s, pl, pl + d, pl + 2 * d, cmp);
                pm = med3(s, pm - d, pm, pm + d, cmp);
                pn = med3(s, pn - 2 * d, pn - d, pn, cmp);
            }
            pm = med3(s, pl, pm, pn, cmp);
        }
        s.swap(0, pm);
        let (mut pa, mut pb) = (1usize, 1usize);
        // `pc` / `pd` run down from the end; kept as one-past to stay
        // unsigned (`pc + 1` is the C's `pc`).
        let (mut pc, mut pd) = (n, n);
        loop {
            while pb < pc {
                let r = cmp(&s[pb], &s[0]);
                if r == Ordering::Greater {
                    break;
                }
                if r == Ordering::Equal {
                    s.swap(pa, pb);
                    pa += 1;
                }
                pb += 1;
            }
            while pb < pc {
                let r = cmp(&s[pc - 1], &s[0]);
                if r == Ordering::Less {
                    break;
                }
                if r == Ordering::Equal {
                    s.swap(pc - 1, pd - 1);
                    pd -= 1;
                }
                pc -= 1;
            }
            if pb >= pc {
                break;
            }
            s.swap(pb, pc - 1);
            pb += 1;
            pc -= 1;
        }
        // In the C's terms pc = pc' - 1 and pd = pd' - 1.
        let d1 = pa.min(pb - pa);
        for i in 0..d1 {
            s.swap(i, pb - d1 + i);
        }
        let d1 = (pd - pc).min(n - pd);
        for i in 0..d1 {
            s.swap(pb + i, n - d1 + i);
        }
        let d1 = pb - pa;
        let d2 = pd - pc;
        if d1 <= d2 {
            if d1 > 1 {
                pg_qsort(&mut s[..d1], cmp);
            }
            if d2 > 1 {
                lo += n - d2;
                n = d2;
                continue;
            }
        } else {
            if d2 > 1 {
                pg_qsort(&mut s[n - d2..], cmp);
            }
            if d1 > 1 {
                n = d1;
                continue;
            }
        }
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_qsort_sorts() {
        let mut seed = 12345u64;
        for n in [0usize, 1, 5, 7, 8, 30, 41, 100, 1000] {
            let mut v: Vec<u32> = (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    ((seed >> 33) % 17) as u32
                })
                .collect();
            let mut want = v.clone();
            want.sort();
            pg_qsort(&mut v, &mut |a, b| a.cmp(b));
            assert_eq!(v, want, "n = {n}");
        }
    }
}
