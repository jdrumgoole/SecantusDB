//! PostgreSQL's in-memory sort, transcribed so that ties come out where
//! PostgreSQL leaves them.

use std::cmp::Ordering;

/// PostgreSQL's quicksort (`sort_template.h`, Bentley & McIlroy): an
/// insertion sort below seven elements, a pass that returns at once on
/// sorted input, then median-of-three (ninther above forty) partitioning
/// with the equal keys swapped to the ends. Not stable, and transcribed so
/// that tied rows come out where PostgreSQL's do.
pub fn pg_qsort<T>(a: &mut [T], cmp: &mut dyn FnMut(&T, &T) -> Ordering) {
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
