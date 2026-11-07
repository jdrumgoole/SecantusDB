//! What ANALYZE stores of a column beyond its distinct count -- the most
//! common values, the histogram and the correlation (`compute_scalar_stats`)
//! -- and the selectivity PostgreSQL 15's planner reads from them for a
//! WHERE (`var_eq_const`, `scalarineqsel`, `clauselist_selectivity`), for
//! the grouping order (RP-028).
//!
//! Values are compared as byte keys whose order is the type's (`int_key`
//! for the integers); a text key is its bytes, which is the C locale's
//! order and not every locale's, so a range or a pattern is read from a
//! text column's statistics only in a collation that compares by bytes
//! (`Column::ordered`), and only equality in any other.

use crate::hashagg::{ColumnStats, Sampled};

/// `DEFAULT_INEQ_SEL`, the selectivity of a range with no statistics.
pub const DEFAULT_INEQ_SEL: f64 = 0.333_333_333_333_333_3;
/// `DEFAULT_RANGE_INEQ_SEL`, a range bounded on both sides.
const DEFAULT_RANGE_INEQ_SEL: f64 = 0.005;
/// `DEFAULT_UNK_SEL`, `IS NULL` with no statistics (`IS NOT NULL` is its
/// complement, `DEFAULT_NOT_UNK_SEL`).
const DEFAULT_UNK_SEL: f64 = 0.005;

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
    scalar_slots_of(values, target, None)
}

/// What `analyze_mcv_list` needs of a sample smaller than its table: the
/// column's estimated distinct count (`stadistinct`), its null fraction,
/// and the table's rows.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub stadistinct: f64,
    pub null_frac: f64,
    pub totalrows: f64,
}

/// `analyze_mcv_list` (PostgreSQL 15.19's `analyze.c`): how many of the
/// `counts` (most common first) stay most common values. The least common
/// is dropped while it is not significantly more common than the values
/// left out of the list would be: its count must pass the selectivity
/// they would share plus two standard deviations of the hypergeometric
/// sampling, plus a half for continuity.
fn analyze_mcv_list(counts: &[usize], samplerows: f64, s: Sample) -> usize {
    let mut num_mcv = counts.len();
    if samplerows == s.totalrows || s.totalrows <= 1.0 {
        return num_mcv;
    }
    let ndistinct_table = if s.stadistinct < 0.0 {
        -s.stadistinct * s.totalrows
    } else {
        s.stadistinct
    };
    let mut sumcount: f64 = counts
        .iter()
        .take(num_mcv.saturating_sub(1))
        .map(|&c| c as f64)
        .sum();
    while num_mcv > 0 {
        let mut selec = (1.0 - sumcount / samplerows - s.null_frac).clamp(0.0, 1.0);
        let otherdistinct = ndistinct_table - (num_mcv - 1) as f64;
        if otherdistinct > 1.0 {
            selec /= otherdistinct;
        }
        let (big_n, n) = (s.totalrows, samplerows);
        let count = counts[num_mcv - 1] as f64;
        let k = big_n * count / n;
        let variance = n * k * (big_n - k) * (big_n - n) / (big_n * big_n * (big_n - 1.0));
        let stddev = variance.sqrt();
        if count > selec * samplerows + 2.0 * stddev + 0.5 {
            break;
        }
        num_mcv -= 1;
        if num_mcv == 0 {
            break;
        }
        sumcount -= counts[num_mcv - 1] as f64;
    }
    num_mcv
}

/// `compute_distinct_stats` (PostgreSQL 15.19's `analyze.c`), for a type
/// with `=` but no `<` (`aclitem[]`): the values are counted in its track
/// list of at most `2 * target` entries, in sample order -- a match moves
/// up past every entry it now outnumbers, a new value goes in at the head
/// of the count-1 entries (pushing the last one out of a full list). The
/// returned `stadistinct` is the one this function sets; the slots hold
/// only the most common values, as `compute_distinct_stats` has no `<` for
/// a histogram or a correlation. `values` holds every `None`-free key in
/// heap order, and `samplerows` / `totalrows` are the sample and the table.
pub fn distinct_slots_of(
    values: &[Sampled],
    target: usize,
    null_frac: f64,
    totalrows: f64,
) -> (Slots, f32) {
    let samplerows = values.len() as f64;
    let track_max = 2 * target;
    let mut track: Vec<(&[u8], usize)> = Vec::new();
    let mut nonnull_cnt = 0usize;
    for (key, _) in values.iter().flatten() {
        nonnull_cnt += 1;
        let mut firstcount1 = track.len();
        let mut found = None;
        for (j, (k, count)) in track.iter().enumerate() {
            if *k == key.as_slice() {
                found = Some(j);
                break;
            }
            if j < firstcount1 && *count == 1 {
                firstcount1 = j;
            }
        }
        match found {
            Some(mut j) => {
                track[j].1 += 1;
                while j > 0 && track[j].1 > track[j - 1].1 {
                    track.swap(j, j - 1);
                    j -= 1;
                }
            }
            None => {
                if track.len() < track_max {
                    track.push((key.as_slice(), 1));
                }
                let last = track.len() - 1;
                if firstcount1 < track.len() {
                    for j in (firstcount1 + 1..=last).rev() {
                        track[j] = track[j - 1];
                    }
                    track[firstcount1] = (key.as_slice(), 1);
                }
            }
        }
    }
    if nonnull_cnt == 0 {
        return (Slots::default(), 0.0);
    }
    let nmultiple = track.iter().filter(|t| t.1 > 1).count();
    let summultiple: usize = track.iter().filter(|t| t.1 > 1).map(|t| t.1).sum();
    let mut stadistinct: f64 = if nmultiple == 0 {
        -(1.0 - null_frac)
    } else if track.len() < track_max && nmultiple == track.len() {
        track.len() as f64
    } else {
        // Haas and Stokes' Duj1, clamped to [d, N].
        let f1 = (nonnull_cnt - summultiple) as f64;
        let d = f1 + nmultiple as f64;
        let n = nonnull_cnt as f64;
        let big_n = totalrows * (1.0 - null_frac);
        let mut x = if big_n > 0.0 {
            (n * d) / ((n - f1) + f1 * n / big_n)
        } else {
            0.0
        };
        x = x.max(d).min(big_n);
        (x + 0.5).floor()
    };
    if stadistinct > 0.1 * totalrows {
        stadistinct = -(stadistinct / totalrows);
    }
    // float4 as pg_statistic stores it.
    let stadistinct = stadistinct as f32;
    let mut num_mcv = target;
    if track.len() < track_max && stadistinct > 0.0 && track.len() <= num_mcv {
        num_mcv = track.len();
    } else {
        num_mcv = num_mcv.min(track.len());
        if num_mcv > 0 {
            let counts: Vec<usize> = track[..num_mcv].iter().map(|t| t.1).collect();
            num_mcv = analyze_mcv_list(
                &counts,
                samplerows,
                Sample {
                    stadistinct: f64::from(stadistinct),
                    null_frac,
                    totalrows,
                },
            );
        }
    }
    let mcv = track[..num_mcv]
        .iter()
        .map(|(k, c)| (k.to_vec(), (*c as f64 / samplerows) as f32))
        .collect();
    (
        Slots {
            mcv,
            histogram: Vec::new(),
            correlation: None,
            mcv_complete: nmultiple <= num_mcv,
        },
        stadistinct,
    )
}

/// `scalar_slots` over a sample smaller than its table (`sample`): the
/// most common values are those `analyze_mcv_list` keeps, unless every
/// distinct value was seen more than once and fits the list.
pub fn scalar_slots_of(values: &[Sampled], target: usize, sample: Option<Sample>) -> Slots {
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
    // `compute_scalar_stats`: every value kept when all were seen twice or
    // more and fit; otherwise those `analyze_mcv_list` keeps.
    if let Some(sample) = sample {
        let keep_all =
            track.len() == ndistinct && sample.stadistinct > 0.0 && track.len() <= target;
        if !keep_all {
            let counts: Vec<usize> = track.iter().map(|t| t.0).collect();
            track.truncate(analyze_mcv_list(&counts, samplerows, sample));
        }
    }
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
    /// Its keys are not numbers but order bytewise as the column's
    /// comparison does: a string in a collation that compares by bytes
    /// (`C`, `POSIX`, `C.UTF-8`), whose most common values a range is
    /// compared with.
    pub ordered: bool,
    /// Its keys are a string's bytes (`text`, `varchar`, `name`,
    /// `"char"`), which `convert_string_to_scalar` places inside a
    /// histogram bin when the column is `ordered`.
    pub string: bool,
    /// Its collation is `C` or `POSIX` (`lc_collate_is_c`; `C.UTF-8` is
    /// not): `make_greater_string` then needs no suffix, and ILIKE's fixed
    /// prefix stops only at an ASCII letter.
    pub c_collation: bool,
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
        // A position inside a histogram bin is read from a number or, in a
        // bytewise collation, a string (`convert_to_scalar`); any other
        // ordered column only compares its most common values, which needs
        // no histogram.
        if !self.numeric && !(self.ordered && (self.string || slots.histogram.len() < 2)) {
            return None;
        }
        let holds = |v: &[u8]| holds(v, key, isgt, iseq);
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
            let (val, low, high) = if self.numeric {
                (
                    int_of_key(key)? as f64,
                    int_of_key(&hv[i - 1])? as f64,
                    int_of_key(&hv[i])? as f64,
                )
            } else if self.ordered && self.string {
                // `convert_string_datum`: a bytewise collation's
                // `strxfrm` is the string itself.
                convert_string_to_scalar(key, &hv[i - 1], &hv[i])
            } else {
                return None;
            };
            let binfrac = if high <= low {
                0.5
            } else if val <= low {
                0.0
            } else if val >= high {
                1.0
            } else {
                let binfrac = (val - low) / (high - low);
                if binfrac.is_nan() || !(0.0..=1.0).contains(&binfrac) {
                    0.5
                } else {
                    binfrac
                }
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

    /// `scalararraysel` for `col = ANY (keys)` (an `IN` list of constants):
    /// each element's `var_eq_const`, summed as disjoint when the sum is a
    /// probability, else combined as independent (`s1 + s2 - s1 * s2`).
    pub fn in_sel(&self, keys: &[Vec<u8>]) -> Option<f64> {
        let (mut s1, mut disjoint) = (0.0f64, 0.0f64);
        for key in keys {
            let s2 = self.eq_sel(key, false)?;
            s1 = s1 + s2 - s1 * s2;
            disjoint += s2;
        }
        Some(if (0.0..=1.0).contains(&disjoint) {
            disjoint
        } else {
            s1
        })
    }

    /// `scalararraysel` for `col <> ALL (keys)` (a `NOT IN` list): each
    /// element's negated `var_eq_const`, taken as disjoint exclusions
    /// (`1 + sum(s2 - 1)`) when that is a probability, else multiplied.
    pub fn not_in_sel(&self, keys: &[Vec<u8>]) -> Option<f64> {
        let (mut s1, mut disjoint) = (1.0f64, 1.0f64);
        for key in keys {
            let s2 = self.eq_sel(key, true)?;
            s1 *= s2;
            disjoint += s2 - 1.0;
        }
        Some(if (0.0..=1.0).contains(&disjoint) {
            disjoint
        } else {
            s1
        })
    }

    /// `nulltestsel`: the null fraction for `IS NULL`, its complement for
    /// `IS NOT NULL`; `DEFAULT_UNK_SEL` with no statistics.
    pub fn null_sel(&self, is_null: bool) -> f64 {
        let freq_null = match self.stats {
            Some((s, _)) => f64::from(s.null_frac),
            None => DEFAULT_UNK_SEL,
        };
        if is_null {
            freq_null
        } else {
            1.0 - freq_null
        }
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

    /// `patternsel_common` (PostgreSQL 15.19's `like_support.c`) for a
    /// LIKE or ILIKE, or their negations, of this column against a
    /// constant pattern with the default escape (RP-404). A pattern that
    /// is its own fixed prefix is `var_eq_const` of it. Otherwise the
    /// histogram's inner entries the pattern matches
    /// (`histogram_selectivity`, at least 10 of them), blended under 100
    /// with the fixed prefix's range (`prefix_selectivity`) times the rest
    /// of the pattern's guess (`like_selectivity`), clamped to
    /// `[0.0001, 0.9999]`, scaled to the rows the MCVs and NULLs leave,
    /// plus the MCVs the pattern matches. A negation is the complement
    /// less the NULLs. The caller passes only a string column, whose
    /// LIKE operator `patternsel` knows (`text`, `varchar`, `name`).
    /// `None` where the statistics are not modelled: a string not in a
    /// bytewise collation, an MCV list whose contents depend on an order
    /// not reproduced, a value not UTF-8.
    pub fn pattern_sel(&self, pattern: &str, insensitive: bool, negated: bool) -> Option<f64> {
        if self.stats.is_some() && !self.ordered {
            return None;
        }
        let nullfrac = self.null_frac();
        let (prefix, rest_selec, status) =
            like_fixed_prefix(pattern.as_bytes(), insensitive, self.c_collation);
        let matches = |v: &[u8]| -> Option<bool> {
            crate::like_match::like_match(
                std::str::from_utf8(v).ok()?,
                pattern,
                Some('\\'),
                insensitive,
            )
            .ok()
        };
        let result = if status == PrefixStatus::Exact {
            self.eq_sel(&prefix, false)?
        } else {
            let slots = self.stats.map(|(_, slots)| slots);
            // `histogram_selectivity(..., 10, 1, ...)`.
            let hist_size = slots.map_or(0, |s| s.histogram.len());
            let mut selec = match slots {
                Some(s) if hist_size >= 10 => {
                    let mut nmatch = 0usize;
                    for v in &s.histogram[1..hist_size - 1] {
                        if matches(v)? {
                            nmatch += 1;
                        }
                    }
                    nmatch as f64 / (hist_size - 2) as f64
                }
                _ => -1.0,
            };
            if hist_size < 100 {
                let prefixsel = if status == PrefixStatus::Partial {
                    self.prefix_sel(&prefix)?
                } else {
                    1.0
                };
                let heursel = prefixsel * rest_selec;
                selec = if selec < 0.0 {
                    heursel
                } else {
                    let hist_weight = hist_size as f64 / 100.0;
                    selec * hist_weight + heursel * (1.0 - hist_weight)
                };
            }
            // Neither tiny nor near-certain is believed.
            selec = selec.clamp(0.0001, 0.9999);
            // `mcv_selectivity`.
            let (mut mcv_selec, mut sumcommon) = (0.0, 0.0);
            if let Some(s) = slots {
                if !self.numeric && !s.mcv_complete {
                    return None;
                }
                for (v, f) in &s.mcv {
                    if matches(v)? {
                        mcv_selec += f64::from(*f);
                    }
                    sumcommon += f64::from(*f);
                }
            }
            selec *= 1.0 - nullfrac - sumcommon;
            selec + mcv_selec
        };
        let result = if negated {
            1.0 - result - nullfrac
        } else {
            result
        };
        Some(result.clamp(0.0, 1.0))
    }

    /// `prefix_selectivity`: the histogram's share of `col >= prefix AND
    /// col < greater`, `greater` the least string `make_greater_string`
    /// finds above every string starting with the prefix, paired as a range
    /// is (`hi + lo - 1`), and never less than `col = prefix`.
    /// `DEFAULT_MATCH_SEL` with no histogram.
    fn prefix_sel(&self, prefix: &[u8]) -> Option<f64> {
        let Some((_, slots)) = self.stats else {
            return Some(DEFAULT_MATCH_SEL);
        };
        let ge = |v: &[u8]| holds(v, prefix, true, true);
        let mut prefixsel = self.histogram_selectivity(slots, prefix, true, true, &ge)?;
        if prefixsel < 0.0 {
            return Some(DEFAULT_MATCH_SEL);
        }
        if let Some(greater) = make_greater_string(prefix, self.c_collation) {
            let lt = |v: &[u8]| holds(v, &greater, false, false);
            let topsel = self.histogram_selectivity(slots, &greater, false, false, &lt)?;
            prefixsel = topsel + prefixsel - 1.0;
        }
        Some(prefixsel.max(self.eq_sel(prefix, false)?))
    }
}

/// Whether the comparison `v OP key` holds, `OP` being `>` / `>=` (`isgt`)
/// or `<` / `<=`, with equality when `iseq`, over keys that order as their
/// type does.
fn holds(v: &[u8], key: &[u8], isgt: bool, iseq: bool) -> bool {
    match (isgt, iseq) {
        (true, true) => v >= key,
        (true, false) => v > key,
        (false, true) => v <= key,
        (false, false) => v < key,
    }
}

/// `DEFAULT_MATCH_SEL`, a pattern match with no statistics to read.
const DEFAULT_MATCH_SEL: f64 = 0.005;
/// `like_selectivity`'s guesses: a fixed character, `_`, `%`.
const FIXED_CHAR_SEL: f64 = 0.20;
const ANY_CHAR_SEL: f64 = 0.9;
const FULL_WILDCARD_SEL: f64 = 5.0;

/// `Pattern_Prefix_Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrefixStatus {
    None,
    Partial,
    Exact,
}

/// `like_fixed_prefix` over a pattern's bytes, with the default escape
/// (`\`): the literal prefix before the first wildcard -- for ILIKE before
/// the first character whose case can vary too (`pattern_char_isalpha`:
/// an ASCII letter, and in a collation that is not `C` any byte of a
/// multibyte character) -- the selectivity of the rest
/// (`like_selectivity`), and whether the prefix is the whole pattern.
fn like_fixed_prefix(
    patt: &[u8],
    insensitive: bool,
    c_ctype: bool,
) -> (Vec<u8>, f64, PrefixStatus) {
    let isalpha = |c: u8| c.is_ascii_alphabetic() || (!c_ctype && c >= 0x80);
    let mut prefix = Vec::with_capacity(patt.len());
    let mut pos = 0;
    while pos < patt.len() {
        if patt[pos] == b'%' || patt[pos] == b'_' {
            break;
        }
        if patt[pos] == b'\\' {
            pos += 1;
            if pos >= patt.len() {
                break;
            }
        }
        if insensitive && isalpha(patt[pos]) {
            break;
        }
        prefix.push(patt[pos]);
        pos += 1;
    }
    let rest = like_selectivity(&patt[pos.min(patt.len())..]);
    let status = if pos >= patt.len() {
        PrefixStatus::Exact
    } else if !prefix.is_empty() {
        PrefixStatus::Partial
    } else {
        PrefixStatus::None
    };
    (prefix, rest, status)
}

/// `like_selectivity`: a leading run of wildcards counts nothing, then each
/// fixed character `FIXED_CHAR_SEL`, each `_` `ANY_CHAR_SEL` and each `%`
/// `FULL_WILDCARD_SEL`, the product capped at 1.
fn like_selectivity(patt: &[u8]) -> f64 {
    let mut sel = 1.0f64;
    let mut pos = patt
        .iter()
        .position(|&c| c != b'%' && c != b'_')
        .unwrap_or(patt.len());
    while pos < patt.len() {
        match patt[pos] {
            b'%' => sel *= FULL_WILDCARD_SEL,
            b'_' => sel *= ANY_CHAR_SEL,
            b'\\' => {
                pos += 1;
                if pos >= patt.len() {
                    break;
                }
                sel *= FIXED_CHAR_SEL;
            }
            _ => sel *= FIXED_CHAR_SEL,
        }
        pos += 1;
    }
    sel.min(1.0)
}

/// `make_greater_string` for a string in a bytewise collation: the
/// prefix with its last character incremented (`pg_utf8_increment`), or,
/// when that character has no successor, the last character dropped and
/// the one before incremented, until the result sorts after the prefix --
/// after the prefix with the collation's greatest of `Z`, `z`, `y` and `9`
/// appended (`z` by bytes), outside the `C` collation. `None` when no
/// character can be incremented.
fn make_greater_string(prefix: &[u8], c_collation: bool) -> Option<Vec<u8>> {
    let mut work = prefix.to_vec();
    let mut len = work.len();
    let cmp: Vec<u8> = if c_collation || len == 0 {
        prefix.to_vec()
    } else {
        let mut c = prefix.to_vec();
        c.push(b'z');
        c
    };
    while len > 0 {
        // `pg_mbcliplen`: the last character's bytes.
        let start = (0..len)
            .rev()
            .find(|&i| work[i] & 0xC0 != 0x80)
            .unwrap_or(0);
        let charlen = len - start;
        while utf8_increment(&mut work[start..len]) {
            if cmp.as_slice() < &work[..len] {
                work.truncate(len);
                return Some(work);
            }
        }
        len -= charlen;
    }
    None
}

/// `pg_utf8_increment`: the next character's bytes, in place, the last
/// byte first (left at its limit when it overflows into the one before);
/// `false` when none follows.
fn utf8_increment(ch: &mut [u8]) -> bool {
    let n = ch.len();
    if !(1..=4).contains(&n) {
        return false;
    }
    if n == 4 && ch[3] < 0xBF {
        ch[3] += 1;
        return true;
    }
    if n >= 3 && ch[2] < 0xBF {
        ch[2] += 1;
        return true;
    }
    if n >= 2 {
        let limit = match ch[0] {
            0xED => 0x9F,
            0xF4 => 0x8F,
            _ => 0xBF,
        };
        if ch[1] < limit {
            ch[1] += 1;
            return true;
        }
    }
    if matches!(ch[0], 0x7F | 0xDF | 0xEF | 0xF4) {
        return false;
    }
    ch[0] += 1;
    true
}

/// `convert_string_to_scalar`: a value and its histogram bin's bounds as
/// fractions in one base. The base spans the bytes the bounds use, widened
/// to all of `A`-`Z`, `a`-`z` or `0`-`9` where it reaches into one, and to
/// printable ASCII when under ten; the prefix all three share is dropped
/// first.
fn convert_string_to_scalar(value: &[u8], lobound: &[u8], hibound: &[u8]) -> (f64, f64, f64) {
    let at = |s: &[u8], i: usize| s.get(i).copied().unwrap_or(0);
    let mut rangelo = i32::from(at(hibound, 0));
    let mut rangehi = rangelo;
    for &c in lobound.iter().chain(hibound) {
        rangelo = rangelo.min(i32::from(c));
        rangehi = rangehi.max(i32::from(c));
    }
    for (lo, hi) in [(b'A', b'Z'), (b'a', b'z'), (b'0', b'9')] {
        let (lo, hi) = (i32::from(lo), i32::from(hi));
        if rangelo <= hi && rangehi >= lo {
            rangelo = rangelo.min(lo);
            rangehi = rangehi.max(hi);
        }
    }
    if rangehi - rangelo < 9 {
        rangelo = i32::from(b' ');
        rangehi = 127;
    }
    let mut skip = 0;
    while at(lobound, skip) != 0
        && at(lobound, skip) == at(hibound, skip)
        && at(lobound, skip) == at(value, skip)
    {
        skip += 1;
    }
    let tail = |s: &[u8]| s.get(skip..).unwrap_or(&[]).to_vec();
    (
        convert_one_string_to_scalar(&tail(value), rangelo, rangehi),
        convert_one_string_to_scalar(&tail(lobound), rangelo, rangehi),
        convert_one_string_to_scalar(&tail(hibound), rangelo, rangehi),
    )
}

/// `convert_one_string_to_scalar`: the first twelve bytes as digits of a
/// fraction in base `rangehi - rangelo + 1`, a byte outside the range
/// counting as one step beyond it.
fn convert_one_string_to_scalar(value: &[u8], rangelo: i32, rangehi: i32) -> f64 {
    let base = f64::from(rangehi - rangelo + 1);
    let (mut num, mut denom) = (0.0f64, base);
    for &c in value.iter().take(12) {
        let ch = i32::from(c).clamp(rangelo - 1, rangehi + 1);
        num += f64::from(ch - rangelo) / denom;
        denom *= base;
    }
    num
}

/// `clauselist_selectivity_or`: the arms' selectivities combined as
/// independent events, `s1 + s2 - s1 * s2`.
pub fn or_sel(arms: &[f64]) -> f64 {
    arms.iter().fold(0.0, |s1, s2| s1 + s2 - s1 * s2)
}

/// `clauselist_selectivity` over a conjunction of `(column, op, key)`
/// clauses: each clause's selectivity multiplied in, except that a column's
/// lower and upper bounds pair into `hi + lo - 1` (plus its null fraction,
/// which both exclude), a tiny or negative pair being clamped as
/// PostgreSQL clamps it, and a bound with no statistics making the pair
/// `DEFAULT_RANGE_INEQ_SEL`; of two bounds on one side the smaller counts.
/// `None` when a clause's selectivity is not modelled.
pub fn clauselist(clauses: &[(&Column, Op, Vec<u8>)]) -> Option<f64> {
    clauselist_with(clauses, &[])
}

/// `clauselist`, with `others` -- the selectivities of clauses that are not
/// comparisons with a constant (an `IN` list, a null test, an `OR`) --
/// multiplied in as they are.
pub fn clauselist_with(clauses: &[(&Column, Op, Vec<u8>)], others: &[f64]) -> Option<f64> {
    match (clauses, others) {
        ([(col, op, key)], []) => return col.clause_sel(*op, key),
        ([], [only]) => return Some(*only),
        _ => {}
    }
    let mut s1: f64 = others.iter().product();
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

    #[test]
    fn distinct_stats_follow_the_track_list() {
        // A new value goes in at the head of the count-1 entries, so 6
        // overtakes 5 and an equal count does not move 5 back up.
        let (slots, distinct) = distinct_slots_of(&ints([5, 6, 6, 5].into_iter()), 10, 0.0, 4.0);
        assert_eq!(distinct, -0.5);
        assert_eq!(slots.mcv, vec![(int_key(6), 0.5), (int_key(5), 0.5)]);
        // One singleton: Duj1 gives 3, over a tenth of the rows.
        let mut values = ints([1, 2, 2, 3, 3, 3].into_iter());
        values.push(None);
        let (slots, distinct) = distinct_slots_of(&values, 10, 1.0 / 7.0, 7.0);
        assert_eq!(distinct, (-3.0f64 / 7.0) as f32);
        assert_eq!(
            slots.mcv.iter().map(|m| m.0.clone()).collect::<Vec<_>>(),
            keys(&[3, 2, 1])
        );
        assert!(slots.histogram.is_empty() && slots.correlation.is_none());
    }

    /// PostgreSQL 15.19's row estimates (`EXPLAIN`, C locale) over 30
    /// analysed rows of `repeat('x', g)`, g from 1 to 30: thirty distinct
    /// strings, so no MCVs and a histogram of all thirty, under the 100
    /// entries past which `patternsel` reads the histogram alone (RP-404).
    #[test]
    fn patterns_and_text_ranges_match_postgres() {
        let values: Vec<Sampled> = (1..=30)
            .map(|g| Some(("x".repeat(g).into_bytes(), g + 1)))
            .collect();
        let stats = crate::hashagg::analyze_column(&values, -1);
        let slots = scalar_slots(&values, 100);
        let col = Column {
            stats: Some((stats, &slots)),
            unique: false,
            boolean: false,
            numeric: false,
            ordered: true,
            string: true,
            c_collation: true,
            actual: None,
            tuples: 30.0,
        };
        let rows = |sel: f64| crate::costsize::clamp_row_est(sel * 30.0);
        for (pattern, insensitive, negated, expected) in [
            ("x%", false, false, 30.0),
            ("xxx%", false, false, 29.0),
            ("%x", false, false, 13.0),
            ("xx_x%", false, false, 29.0),
            ("x%", false, true, 1.0),
            ("xxxxx", false, false, 1.0),
            ("X%", true, false, 30.0),
            ("y%", false, false, 1.0),
        ] {
            let sel = col.pattern_sel(pattern, insensitive, negated).unwrap();
            assert_eq!(rows(sel), expected, "{pattern} {insensitive} {negated}");
        }
        let gt = col.clause_sel(Op::Gt, b"xxxx").unwrap();
        assert_eq!(rows(gt), 27.0);
        let lt = col.clause_sel(Op::Lt, b"xxx").unwrap();
        assert_eq!(rows(lt), 1.0);
    }

    /// `like_fixed_prefix` and `like_selectivity`: the prefix stops at the
    /// first wildcard (for ILIKE at a letter too), a leading wildcard
    /// counts nothing, and a pattern with no wildcard is exact.
    #[test]
    fn like_prefixes_are_postgres() {
        let (p, rest, status) = like_fixed_prefix(b"ab%c", false, true);
        assert_eq!((p.as_slice(), status), (&b"ab"[..], PrefixStatus::Partial));
        assert!((rest - FIXED_CHAR_SEL).abs() < 1e-12);
        let (p, rest, status) = like_fixed_prefix(b"%x_", false, true);
        assert_eq!((p.as_slice(), status), (&b""[..], PrefixStatus::None));
        assert!((rest - FIXED_CHAR_SEL * ANY_CHAR_SEL).abs() < 1e-12);
        let (p, _, status) = like_fixed_prefix(br"a\%b", false, true);
        assert_eq!((p.as_slice(), status), (&b"a%b"[..], PrefixStatus::Exact));
        let (p, rest, status) = like_fixed_prefix(b"1a%", true, true);
        assert_eq!((p.as_slice(), status), (&b"1"[..], PrefixStatus::Partial));
        assert_eq!(rest, 1.0);
        // Outside the C collation a multibyte character's case may vary.
        let (p, _, _) = like_fixed_prefix("1é%".as_bytes(), true, false);
        assert_eq!(p, b"1");
        let (p, _, _) = like_fixed_prefix("1é%".as_bytes(), true, true);
        assert_eq!(p, "1é".as_bytes());
    }

    /// `make_greater_string` increments the last character
    /// (`pg_utf8_increment`), dropping one that has no successor.
    #[test]
    fn greater_strings_are_postgres() {
        assert_eq!(make_greater_string(b"x", true).unwrap(), b"y");
        assert_eq!(make_greater_string(b"x", false).unwrap(), b"y");
        assert_eq!(make_greater_string(b"az", false).unwrap(), b"a{");
        assert_eq!(make_greater_string(b"a\x7f", true).unwrap(), b"b");
        assert_eq!(make_greater_string(b"\x7f", true), None);
        assert_eq!(
            make_greater_string("é".as_bytes(), true).unwrap(),
            "ê".as_bytes()
        );
    }

    /// `convert_string_to_scalar`: `b` between `a` and `c` in base 26, the
    /// range widened to the lower-case letters.
    #[test]
    fn strings_convert_in_the_letters_base() {
        let (v, lo, hi) = convert_string_to_scalar(b"b", b"a", b"c");
        assert_eq!((v, lo, hi), (1.0 / 26.0, 0.0, 2.0 / 26.0));
        // The prefix all three share is dropped first.
        let (v, lo, hi) = convert_string_to_scalar(b"w0500", b"w0491", b"w0501");
        assert!(lo < v && v < hi);
    }

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
            ordered: false,
            string: false,
            c_collation: false,
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
            ordered: false,
            string: false,
            c_collation: false,
            actual: None,
            tuples: 300.0,
        };
        assert_eq!(
            rows(g.clause_sel(Op::Eq, &int_key(3)).unwrap(), 300.0),
            28.0
        );
    }

    /// PostgreSQL 15.19's row estimates over 300 analysed rows: `g in (1, 2,
    /// 3)` with g = (i * 7) % 11 keeps 82 (the three MCV frequencies summed);
    /// over (g a fifth NULL, else (i * 3) % 7; h = i % 4), `g = 3 or h = 1`
    /// keeps 101, `g is null` 60 and `g is not null` 240.
    #[test]
    fn in_or_and_null_tests_match_postgres() {
        let column = |values: Vec<Sampled>| {
            let stats = crate::hashagg::analyze_column(&values, 4);
            (stats, scalar_slots(&values, 100))
        };
        fn model((stats, slots): &(ColumnStats, Slots)) -> Column<'_> {
            Column {
                stats: Some((*stats, slots)),
                unique: false,
                boolean: false,
                numeric: true,
                ordered: false,
                string: false,
                c_collation: false,
                actual: None,
                tuples: 300.0,
            }
        }
        let rows = |sel: f64| crate::costsize::clamp_row_est(sel * 300.0);
        let g = column(ints((1..=300).map(|i| (i * 7) % 11)));
        let keys: Vec<Vec<u8>> = [1, 2, 3].iter().map(|&n| int_key(n)).collect();
        assert_eq!(rows(model(&g).in_sel(&keys).unwrap()), 82.0);
        let gn: Vec<Sampled> = (1..=300)
            .map(|i| (i % 5 != 0).then(|| (int_key((i * 3) % 7), 0)))
            .collect();
        let gn = column(gn);
        let h = column(ints((1..=300).map(|i| i % 4)));
        let arms = [
            model(&gn).clause_sel(Op::Eq, &int_key(3)).unwrap(),
            model(&h).clause_sel(Op::Eq, &int_key(1)).unwrap(),
        ];
        assert_eq!(rows(or_sel(&arms)), 101.0);
        assert_eq!(rows(model(&gn).null_sel(true)), 60.0);
        assert_eq!(rows(model(&gn).null_sel(false)), 240.0);
    }
}
