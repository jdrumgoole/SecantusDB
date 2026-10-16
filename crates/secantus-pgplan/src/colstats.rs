//! What ANALYZE stores of a column beyond its distinct count -- the most
//! common values, the histogram and the correlation (`compute_scalar_stats`)
//! -- and the selectivity PostgreSQL 15's planner reads from them for a
//! WHERE (`var_eq_const`, `scalarineqsel`, `clauselist_selectivity`), for
//! the grouping order (RP-028).
//!
//! Values are compared as byte keys whose order is the type's (`int_key`
//! for the integers); a text key is its bytes, which is the C locale's
//! order and not every locale's, so only equality is read from a text
//! column's statistics.

use crate::hashagg::{ColumnStats, Sampled};

/// `DEFAULT_INEQ_SEL`, the selectivity of a range with no statistics.
pub const DEFAULT_INEQ_SEL: f64 = 0.333_333_333_333_333_3;
/// `DEFAULT_RANGE_INEQ_SEL`, a range bounded on both sides.
const DEFAULT_RANGE_INEQ_SEL: f64 = 0.005;

/// An integer's key: big-endian with the sign bit flipped, so the bytes
/// sort as the numbers do.
pub fn int_key(n: i64) -> Vec<u8> {
    ((n as u64) ^ (1 << 63)).to_be_bytes().to_vec()
}

/// The integer an `int_key` holds.
pub fn int_of_key(key: &[u8]) -> Option<i64> {
    let bytes: [u8; 8] = key.try_into().ok()?;
    Some((u64::from_be_bytes(bytes) ^ (1 << 63)) as i64)
}

/// The slots `compute_scalar_stats` fills besides the scalars: the most
/// common values with their frequencies (`STATISTIC_KIND_MCV`, most common
/// first), the histogram's bounds (`STATISTIC_KIND_HISTOGRAM`) and the
/// correlation of the column's order with the heap's
/// (`STATISTIC_KIND_CORRELATION`, `None` under two values).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Slots {
    pub mcv: Vec<(Vec<u8>, f32)>,
    pub histogram: Vec<Vec<u8>>,
    pub correlation: Option<f32>,
    /// Every value that repeats is in the MCV list: then which values it
    /// holds does not depend on how ties in count are ordered.
    pub mcv_complete: bool,
}

/// `compute_scalar_stats`' slots from a sample that is the whole table,
/// in heap order, with `target` the column's statistics target (the MCV
/// list's and the histogram's size). The values are sorted by key, ties in
/// sample order; a value that repeats competes for the MCV list by its
/// count, an equal count leaving the earlier value ahead; the whole list is
/// kept (`analyze_mcv_list` keeps all of a full sample). The histogram is
/// the remaining values at even steps, first and last included, when at
/// least two distinct values remain.
pub fn scalar_slots(values: &[Sampled], target: usize) -> Slots {
    let samplerows = values.len() as f64;
    let mut items: Vec<(&[u8], usize)> = values
        .iter()
        .flatten()
        .enumerate()
        .map(|(tupno, (key, _))| (key.as_slice(), tupno))
        .collect();
    items.sort();
    let n = items.len();
    let mut slots = Slots::default();
    if n == 0 {
        return slots;
    }
    // (count, first position) of the values kept, most common first.
    let mut track: Vec<(usize, usize)> = Vec::new();
    let (mut ndistinct, mut nmultiple, mut xysum) = (0usize, 0usize, 0f64);
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n && items[j].0 == items[i].0 {
            xysum += j as f64 * items[j].1 as f64;
            j += 1;
        }
        let dups = j - i;
        ndistinct += 1;
        if dups > 1 {
            nmultiple += 1;
            if track.len() < target || track.last().is_some_and(|t| dups > t.0) {
                if track.len() < target {
                    track.push((0, 0));
                }
                let mut pos = track.len() - 1;
                while pos > 0 && dups > track[pos - 1].0 {
                    track[pos] = track[pos - 1];
                    pos -= 1;
                }
                track[pos] = (dups, i);
            }
        }
        i = j;
    }
    slots.mcv_complete = nmultiple <= target;
    slots.mcv = track
        .iter()
        .map(|&(count, first)| (items[first].0.to_vec(), (count as f64 / samplerows) as f32))
        .collect();
    let mut num_hist = ndistinct - track.len();
    if num_hist > target {
        num_hist = target + 1;
    }
    if num_hist >= 2 {
        let mut kept = track.clone();
        kept.sort_by_key(|t| t.1);
        let mut rest: Vec<&[u8]> = Vec::with_capacity(n);
        let mut next = kept.iter().peekable();
        let mut src = 0;
        while src < n {
            if let Some(&&(count, first)) = next.peek() {
                if src >= first {
                    src = first + count;
                    next.next();
                    continue;
                }
            }
            rest.push(items[src].0);
            src += 1;
        }
        let nvals = rest.len();
        let (delta, deltafrac) = ((nvals - 1) / (num_hist - 1), (nvals - 1) % (num_hist - 1));
        let (mut pos, mut posfrac) = (0, 0);
        for _ in 0..num_hist {
            slots.histogram.push(rest[pos].to_vec());
            pos += delta;
            posfrac += deltafrac;
            if posfrac >= num_hist - 1 {
                pos += 1;
                posfrac -= num_hist - 1;
            }
        }
    }
    if n > 1 {
        let nf = n as f64;
        let xsum = (nf - 1.0) * nf / 2.0;
        let x2sum = (nf - 1.0) * nf * (2.0 * nf - 1.0) / 6.0;
        slots.correlation = Some(((nf * xysum - xsum * xsum) / (nf * x2sum - xsum * xsum)) as f32);
    }
    slots
}

/// A comparison of a column with a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// One column as the selectivity functions see it.
#[derive(Debug, Clone)]
pub struct Column<'a> {
    /// Its `pg_statistic` row, `None` when ANALYZE stored none.
    pub stats: Option<(ColumnStats, &'a Slots)>,
    /// A unique index covers it alone (`vardata->isunique`).
    pub unique: bool,
    /// A boolean (`get_variable_numdistinct` without statistics: 2).
    pub boolean: bool,
    /// Its keys order as the histogram does and convert to numbers
    /// (`convert_to_scalar`): the integers.
    pub numeric: bool,
    /// The column's current least and greatest value, when a B-tree index
    /// leads with it (`get_actual_variable_range`).
    pub actual: Option<(Vec<u8>, Vec<u8>)>,
    /// `rel->tuples`.
    pub tuples: f64,
}

impl Column<'_> {
    fn numdistinct(&self) -> f64 {
        match &self.stats {
            Some((s, _)) => crate::hashagg::analyzed_numdistinct(s, self.unique, self.tuples),
            None if self.boolean => 2.0,
            None if self.unique || self.tuples < 200.0 => {
                crate::costsize::clamp_row_est(self.tuples)
            }
            None => 200.0,
        }
    }

    fn null_frac(&self) -> f64 {
        self.stats.map_or(0.0, |(s, _)| f64::from(s.null_frac))
    }

    /// `var_eq_const` (negated for `<>`). `None` where the MCV list's
    /// contents depend on an order this module does not reproduce.
    pub fn eq_sel(&self, key: &[u8], negate: bool) -> Option<f64> {
        let nullfrac = self.null_frac();
        let mut selec = if self.unique && self.tuples >= 1.0 {
            1.0 / self.tuples
        } else if let Some((_, slots)) = self.stats {
            if !self.numeric && !slots.mcv_complete {
                return None;
            }
            match slots.mcv.iter().find(|(v, _)| v.as_slice() == key) {
                Some((_, freq)) => f64::from(*freq),
                None => {
                    let sumcommon: f64 = slots.mcv.iter().map(|(_, f)| f64::from(*f)).sum();
                    let mut selec = (1.0 - sumcommon - nullfrac).clamp(0.0, 1.0);
                    let otherdistinct = self.numdistinct() - slots.mcv.len() as f64;
                    if otherdistinct > 1.0 {
                        selec /= otherdistinct;
                    }
                    if let Some((_, least)) = slots.mcv.last() {
                        selec = selec.min(f64::from(*least));
                    }
                    selec
                }
            }
        } else {
            1.0 / self.numdistinct()
        };
        if negate {
            selec = 1.0 - selec - nullfrac;
        }
        Some(selec.clamp(0.0, 1.0))
    }

    /// `scalarineqsel`: the MCVs that satisfy the comparison, plus the
    /// histogram's share (`ineq_histogram_selectivity`) of the rest; a third
    /// with no statistics. `None` for a column whose values are not
    /// `numeric`.
    pub fn ineq_sel(&self, key: &[u8], isgt: bool, iseq: bool) -> Option<f64> {
        let Some((stats, slots)) = self.stats else {
            return Some(DEFAULT_INEQ_SEL);
        };
        if !self.numeric {
            return None;
        }
        let holds = |v: &[u8]| match (isgt, iseq) {
            (true, true) => v >= key,
            (true, false) => v > key,
            (false, true) => v <= key,
            (false, false) => v < key,
        };
        let mut mcv_selec = 0.0;
        let mut sumcommon = 0.0;
        for (v, f) in &slots.mcv {
            if holds(v) {
                mcv_selec += f64::from(*f);
            }
            sumcommon += f64::from(*f);
        }
        let hist_selec = self.histogram_selectivity(slots, key, isgt, iseq, &holds)?;
        let mut selec = 1.0 - f64::from(stats.null_frac) - sumcommon;
        selec *= if hist_selec >= 0.0 { hist_selec } else { 0.5 };
        selec += mcv_selec;
        Some(selec.clamp(0.0, 1.0))
    }

    fn histogram_selectivity(
        &self,
        slots: &Slots,
        key: &[u8],
        isgt: bool,
        iseq: bool,
        holds: &dyn Fn(&[u8]) -> bool,
    ) -> Option<f64> {
        let nvalues = slots.histogram.len();
        if nvalues < 2 {
            return Some(-1.0);
        }
        let mut hv = slots.histogram.clone();
        let mut have_end = false;
        if nvalues == 2 {
            if let Some((lo, hi)) = &self.actual {
                hv[0] = lo.clone();
                hv[1] = hi.clone();
                have_end = true;
            }
        }
        let (mut lobound, mut hibound) = (0usize, nvalues);
        while lobound < hibound {
            let probe = (lobound + hibound) / 2;
            if nvalues > 2 && (probe == 0 || probe == nvalues - 1) {
                have_end = self.actual.is_some();
                if let Some((lo, hi)) = &self.actual {
                    hv[probe] = if probe == 0 { lo.clone() } else { hi.clone() };
                }
            }
            let mut ltcmp = holds(&hv[probe]);
            if isgt {
                ltcmp = !ltcmp;
            }
            if ltcmp {
                lobound = probe + 1;
            } else {
                hibound = probe;
            }
        }
        let histfrac = if lobound == 0 {
            0.0
        } else if lobound >= nvalues {
            1.0
        } else {
            let i = lobound;
            let mut eq_selec = 0.0;
            if i == 1 || isgt == iseq {
                let otherdistinct = self.numdistinct() - slots.mcv.len() as f64;
                if otherdistinct > 1.0 {
                    eq_selec = 1.0 / otherdistinct;
                }
            }
            let val = int_of_key(key)? as f64;
            let low = int_of_key(&hv[i - 1])? as f64;
            let high = int_of_key(&hv[i])? as f64;
            let binfrac = if high <= low {
                0.5
            } else if val <= low {
                0.0
            } else if val >= high {
                1.0
            } else {
                (val - low) / (high - low)
            };
            let mut histfrac = ((i - 1) as f64 + binfrac) / (nvalues - 1) as f64;
            if i == 1 {
                histfrac += eq_selec * (1.0 - binfrac);
            }
            if isgt == iseq {
                histfrac -= eq_selec;
            }
            histfrac
        };
        let mut hist_selec = if isgt { 1.0 - histfrac } else { histfrac };
        if have_end {
            hist_selec = hist_selec.clamp(0.0, 1.0);
        } else {
            let cutoff = 0.01 / (nvalues - 1) as f64;
            hist_selec = hist_selec.clamp(cutoff, 1.0 - cutoff);
        }
        Some(hist_selec)
    }

    /// One clause's selectivity (`clause_selectivity`).
    pub fn clause_sel(&self, op: Op, key: &[u8]) -> Option<f64> {
        match op {
            Op::Eq => self.eq_sel(key, false),
            Op::Ne => self.eq_sel(key, true),
            Op::Lt => self.ineq_sel(key, false, false),
            Op::Le => self.ineq_sel(key, false, true),
            Op::Gt => self.ineq_sel(key, true, false),
            Op::Ge => self.ineq_sel(key, true, true),
        }
    }
}

/// `clauselist_selectivity` over a conjunction of `(column, op, key)`
/// clauses: each clause's selectivity multiplied in, except that a column's
/// lower and upper bounds pair into `hi + lo - 1` (plus its null fraction,
/// which both exclude), a tiny or negative pair being clamped as
/// PostgreSQL clamps it, and a bound with no statistics making the pair
/// `DEFAULT_RANGE_INEQ_SEL`; of two bounds on one side the smaller counts.
/// `None` when a clause's selectivity is not modelled.
pub fn clauselist(clauses: &[(&Column, Op, Vec<u8>)]) -> Option<f64> {
    if let [(col, op, key)] = clauses {
        return col.clause_sel(*op, key);
    }
    let mut s1 = 1.0;
    // (column, lobound, hibound), newest first as `addRangeClause` links
    // them.
    let mut ranges: Vec<(&Column, Option<f64>, Option<f64>)> = Vec::new();
    for (col, op, key) in clauses {
        let s2 = col.clause_sel(*op, key)?;
        let lower = match op {
            Op::Gt | Op::Ge => true,
            Op::Lt | Op::Le => false,
            _ => {
                s1 *= s2;
                continue;
            }
        };
        let slot = match ranges.iter_mut().find(|r| std::ptr::eq(r.0, *col)) {
            Some(r) => r,
            None => {
                ranges.insert(0, (col, None, None));
                &mut ranges[0]
            }
        };
        let bound = if lower { &mut slot.1 } else { &mut slot.2 };
        *bound = Some(bound.map_or(s2, |b| b.min(s2)));
    }
    for (col, lo, hi) in ranges {
        s1 *= match (lo, hi) {
            (Some(lo), Some(hi)) if lo == DEFAULT_INEQ_SEL || hi == DEFAULT_INEQ_SEL => {
                DEFAULT_RANGE_INEQ_SEL
            }
            (Some(lo), Some(hi)) => {
                let s2 = hi + lo - 1.0 + col.null_frac();
                if s2 > 0.0 {
                    s2
                } else if s2 < -0.01 {
                    DEFAULT_RANGE_INEQ_SEL
                } else {
                    1.0e-10
                }
            }
            (Some(s), None) | (None, Some(s)) => s,
            (None, None) => 1.0,
        };
    }
    Some(s1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ints(vals: impl Iterator<Item = i64>) -> Vec<Sampled> {
        vals.map(|n| Some((int_key(n), 0))).collect()
    }

    fn keys(vals: &[i64]) -> Vec<Vec<u8>> {
        vals.iter().map(|&n| int_key(n)).collect()
    }

    /// PostgreSQL 15.19, `analyze` over 3,000 rows, then `pg_stats`:
    /// `(i * 7) % 11` keeps all 11 values as MCVs, the more common first
    /// and equal counts in value order, correlation 0.08999909; `i % 150`
    /// keeps the first 100 of its 150 equally common values, the histogram
    /// holding the other 50 (correlation 0.054661117); `i` has no MCVs and
    /// a histogram of every 30th value, correlation 1; and 2,996 rows of
    /// 100 with 1 to 4 once each keep 100 (0.99866664) and 1 2 3 4.
    #[test]
    fn slots_are_what_postgres_stores() {
        let g = scalar_slots(&ints((1..=3000).map(|i| (i * 7) % 11)), 100);
        let order: Vec<i64> = g.mcv.iter().map(|(k, _)| int_of_key(k).unwrap()).collect();
        assert_eq!(order, [1, 2, 3, 5, 6, 7, 9, 10, 0, 4, 8]);
        assert_eq!(g.mcv[0].1, 0.091);
        assert_eq!(g.mcv[10].1, 0.090_666_67);
        assert!(g.histogram.is_empty());
        assert_eq!(g.correlation, Some(0.089_999_09));

        let h = scalar_slots(&ints((1..=3000).map(|i| i % 150)), 100);
        assert_eq!(
            h.mcv.iter().map(|m| m.0.clone()).collect::<Vec<_>>(),
            keys(&(0..100).collect::<Vec<_>>())
        );
        assert_eq!(h.mcv[0].1, 0.006_666_667);
        assert_eq!(h.histogram, keys(&(100..150).collect::<Vec<_>>()));
        assert_eq!(h.correlation, Some(0.054_661_117));
        assert!(!h.mcv_complete);

        let k = scalar_slots(&ints(1..=3000), 100);
        assert!(k.mcv.is_empty());
        let bounds: Vec<i64> = std::iter::once(1)
            .chain((1..=100).map(|b| b * 30))
            .collect();
        assert_eq!(k.histogram, keys(&bounds));
        assert_eq!(k.correlation, Some(1.0));

        let m = scalar_slots(&ints((1..=3000).map(|i| if i < 5 { i } else { 100 })), 100);
        assert_eq!(m.mcv, vec![(int_key(100), 0.998_666_64)]);
        assert_eq!(m.histogram, keys(&[1, 2, 3, 4]));
        assert_eq!(m.correlation, Some(1.0));
    }

    /// PostgreSQL 15.19's row estimates (`EXPLAIN`) over analysed tables
    /// (k int primary key, g int), g = (i * 7) % 11: over 300 rows `k > 5`
    /// keeps 295, `k > 5 and k < 30` 24 and `g = 3` 28; over 3,000 rows
    /// `k < 10` keeps 9. k's histogram ends are read from its index.
    #[test]
    fn estimates_match_postgres() {
        let column = |n: i64, f: &dyn Fn(i64) -> i64| {
            let values = ints((1..=n).map(f));
            (
                crate::hashagg::analyze_column(&values, 4),
                scalar_slots(&values, 100),
            )
        };
        let (k300, k300_slots) = column(300, &|i| i);
        let k = |stats, slots, tuples: f64| Column {
            stats: Some((stats, slots)),
            unique: true,
            boolean: false,
            numeric: true,
            actual: Some((int_key(1), int_key(tuples as i64))),
            tuples,
        };
        let k300 = k(k300, &k300_slots, 300.0);
        let rows = |sel: f64, tuples: f64| crate::costsize::clamp_row_est(sel * tuples);
        assert_eq!(
            rows(k300.clause_sel(Op::Gt, &int_key(5)).unwrap(), 300.0),
            295.0
        );
        let pair = clauselist(&[(&k300, Op::Gt, int_key(5)), (&k300, Op::Lt, int_key(30))]);
        assert_eq!(rows(pair.unwrap(), 300.0), 24.0);
        let (k3000, k3000_slots) = column(3000, &|i| i);
        let k3000 = k(k3000, &k3000_slots, 3000.0);
        assert_eq!(
            rows(k3000.clause_sel(Op::Lt, &int_key(10)).unwrap(), 3000.0),
            9.0
        );
        let (g, g_slots) = column(300, &|i| (i * 7) % 11);
        let g = Column {
            stats: Some((g, &g_slots)),
            unique: false,
            boolean: false,
            numeric: true,
            actual: None,
            tuples: 300.0,
        };
        assert_eq!(
            rows(g.clause_sel(Op::Eq, &int_key(3)).unwrap(), 300.0),
            28.0
        );
    }
}
