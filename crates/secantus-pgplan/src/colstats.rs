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
    /// constant pattern with the default escape (RP-404), or with `regex`
    /// a regex match (`~`, `~*`, `!~`, `!~*`), whose fixed prefix and rest
    /// are `regex_fixed_prefix`'s ([`regex_fixed_prefix`]). A pattern that
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
    /// not reproduced, a value not UTF-8, a regex whose prefix is not
    /// modelled.
    pub fn pattern_sel(
        &self,
        pattern: &str,
        insensitive: bool,
        negated: bool,
        regex: bool,
    ) -> Option<f64> {
        if self.stats.is_some() && !self.ordered {
            return None;
        }
        let nullfrac = self.null_frac();
        let (prefix, rest_selec, status) = if regex {
            regex_fixed_prefix(pattern, insensitive, self.c_collation)?
        } else {
            like_fixed_prefix(pattern.as_bytes(), insensitive, self.c_collation)
        };
        // The regex a regex match runs as (`eval_pattern_match_const`).
        let compiled = if regex {
            Some(
                regex::RegexBuilder::new(&crate::scalar::pg_regex_source(pattern))
                    .case_insensitive(insensitive)
                    .build()
                    .ok()?,
            )
        } else {
            None
        };
        let matches = |v: &[u8]| -> Option<bool> {
            let v = std::str::from_utf8(v).ok()?;
            match &compiled {
                Some(re) => Some(re.is_match(v)),
                None => crate::like_match::like_match(v, pattern, Some('\\'), insensitive).ok(),
            }
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

/// `regex_selectivity`'s guesses beyond `like_selectivity`'s: a bracket
/// expression, and a quantifier.
const CHAR_RANGE_SEL: f64 = 0.25;
const PARTIAL_WILDCARD_SEL: f64 = 2.0;

/// `regex_fixed_prefix` over a regex (an ARE) with the case sensitivity
/// its operator gives it: the fixed prefix `pg_regprefix` reads from the
/// compiled regex ([`regex_prefix`]), the selectivity of the rest
/// (`regex_selectivity` of the whole pattern, the prefix's characters
/// discounted), and whether the prefix is the whole match. `None` where
/// the regex's prefix is not modelled.
fn regex_fixed_prefix(
    patt: &str,
    insensitive: bool,
    c_ctype: bool,
) -> Option<(Vec<u8>, f64, PrefixStatus)> {
    let (prefix, status) = regex_prefix(patt, insensitive, c_ctype)?;
    let rest = match status {
        PrefixStatus::Exact => 1.0,
        PrefixStatus::Partial => regex_selectivity(patt.as_bytes(), prefix.len()),
        PrefixStatus::None => regex_selectivity(patt.as_bytes(), 0),
    };
    Some((prefix.into_bytes(), rest, status))
}

/// One atom of a regex, as [`regex_prefix`] reads it, with its bounds.
#[derive(Debug)]
enum ReAtom {
    /// One character: a state with one out-arc whose color, outside a
    /// case-insensitive match of a cased letter, is that character alone.
    Char(char),
    /// A set of more than one character (`.`, a bracket, `\d`): a state
    /// whose out-arcs are of several colors, or one color of several.
    Set,
    /// A parenthesized group's alternatives.
    Group(Vec<Vec<QuantifiedAtom>>),
}

/// A [`ReAtom`] and its quantifier's bounds.
type QuantifiedAtom = (ReAtom, u32, Option<u32>);

/// `pg_regprefix`'s walk of a regex's NFA (`findprefix` in PostgreSQL
/// 15.19's `regprefix.c`), read from the regex's text: a regex anchored by
/// a leading `^` contributes each character a state reaches by one
/// out-arc of a one-character color, stopping at the first state with a
/// choice of characters (a set, an optional or repeated atom, a cased
/// letter matched case-insensitively); a group's alternatives add the
/// first character they all start with, if they do, and stop the walk.
/// An atom bounded `{0}` is no atom. The prefix is exact when a `$` ends
/// the regex right after it, and status `None` (`REG_NOMATCH`) when the
/// regex is not anchored, alternates at its top, or fixes no first
/// character. `None` for a regex whose NFA this does not model: a group
/// repeated other than at most once, a constraint or anchor anywhere but
/// a leading `^` and a trailing `$`, a back reference, a numeric
/// character escape, embedded options or a director.
fn regex_prefix(patt: &str, insensitive: bool, c_ctype: bool) -> Option<(String, PrefixStatus)> {
    let chars: Vec<char> = patt.chars().collect();
    if patt.starts_with("***") || patt.starts_with("(?") {
        return None;
    }
    // An alternation at the top: the `pre` state's rainbow, or BOS arcs
    // to more than one state.
    if regex_has_bare(&chars, '|', true) {
        return Some((String::new(), PrefixStatus::None));
    }
    if chars.first() != Some(&'^') {
        // The `pre` state's rainbow: not anchored. A `^` further in
        // anchors some branch, which is not modelled.
        return if regex_has_bare(&chars, '^', false) {
            None
        } else {
            Some((String::new(), PrefixStatus::None))
        };
    }
    let exact_end = chars.len() >= 2 && chars[chars.len() - 1] == '$' && {
        let backslashes = chars[1..chars.len() - 1]
            .iter()
            .rev()
            .take_while(|c| **c == '\\')
            .count();
        backslashes % 2 == 0
    };
    let body = &chars[1..chars.len() - usize::from(exact_end)];
    let mut pos = 0;
    let alternatives = regex_atoms(body, &mut pos)?;
    if pos != body.len() {
        return None;
    }
    // The `pre` state's BOS arcs lead to more than one state.
    let [atoms] = alternatives.as_slice() else {
        return Some((String::new(), PrefixStatus::None));
    };
    let case_varies = |c: char| {
        if c_ctype {
            c.is_ascii_alphabetic()
        } else {
            c.to_lowercase().ne(std::iter::once(c)) || c.to_uppercase().ne(std::iter::once(c))
        }
    };
    // Whether the walk went through every atom (`Some(true)`) or stopped
    // at a choice (`Some(false)`).
    fn walk(
        atoms: &[QuantifiedAtom],
        prefix: &mut String,
        insensitive: bool,
        case_varies: &dyn Fn(char) -> bool,
    ) -> Option<bool> {
        // The character every path through `atoms` starts with.
        fn first_char(atoms: &[QuantifiedAtom]) -> Option<char> {
            match atoms.iter().find(|a| a.2 != Some(0))? {
                (ReAtom::Char(c), min, _) if *min > 0 => Some(*c),
                (ReAtom::Group(alts), 1, Some(1)) => match alts.as_slice() {
                    [only] => first_char(only),
                    _ => None,
                },
                _ => None,
            }
        }
        for (atom, min, max) in atoms {
            if *max == Some(0) {
                continue;
            }
            match atom {
                ReAtom::Char(c) => {
                    if insensitive && case_varies(*c) {
                        return Some(false);
                    }
                    match max {
                        // An exact count: that many states in a row.
                        Some(n) if n == min => prefix.extend(std::iter::repeat_n(*c, *n as usize)),
                        // `+`, `{m,}`: one, then the loop's choice.
                        None if *min > 0 => {
                            prefix.push(*c);
                            return Some(false);
                        }
                        // Optional, or a bounded range: a choice at once.
                        _ => return Some(false),
                    }
                }
                ReAtom::Set => return Some(false),
                ReAtom::Group(_) if *min == 0 => return Some(false),
                ReAtom::Group(alts) if *max == Some(*min) => match alts.as_slice() {
                    [inner] => {
                        for _ in 0..*min {
                            if !walk(inner, prefix, insensitive, case_varies)? {
                                return Some(false);
                            }
                        }
                    }
                    // Out-arcs of one color to several states: its
                    // character, and no further.
                    _ => {
                        let firsts: Vec<Option<char>> =
                            alts.iter().map(|a| first_char(a)).collect();
                        if let Some(Some(c)) = firsts.first() {
                            if firsts.iter().all(|f| *f == Some(*c))
                                && !(insensitive && case_varies(*c))
                            {
                                prefix.push(*c);
                            }
                        }
                        return Some(false);
                    }
                },
                ReAtom::Group(_) => return None,
            }
        }
        Some(true)
    }
    let mut prefix = String::new();
    let whole = walk(atoms, &mut prefix, insensitive, &case_varies)?;
    let status = if whole && exact_end {
        PrefixStatus::Exact
    } else if !prefix.is_empty() {
        PrefixStatus::Partial
    } else {
        PrefixStatus::None
    };
    Some((prefix, status))
}

/// Whether `c` appears in `chars` outside a bracket expression and not
/// escaped, with `top` outside parentheses too.
fn regex_has_bare(chars: &[char], c: char, top: bool) -> bool {
    let mut i = 0;
    let mut depth = 0usize;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            x if x == c && !(top && depth > 0) => return true,
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '[' => {
                i += 1;
                if chars.get(i) == Some(&'^') {
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '[' && matches!(chars.get(i + 1), Some(':' | '.' | '=')) {
                        let close = chars[i + 1];
                        i += 2;
                        while i + 1 < chars.len() && !(chars[i] == close && chars[i + 1] == ']') {
                            i += 1;
                        }
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// The alternatives of a regex's text from `*pos` to its end or the `)`
/// closing the group it is inside, each its atoms with their quantifiers'
/// bounds. `None` for what [`regex_prefix`] does not model.
fn regex_atoms(chars: &[char], pos: &mut usize) -> Option<Vec<Vec<QuantifiedAtom>>> {
    let mut alternatives = Vec::new();
    let mut atoms: Vec<QuantifiedAtom> = Vec::new();
    while *pos < chars.len() {
        let c = chars[*pos];
        let atom = match c {
            ')' => break,
            '|' => {
                alternatives.push(std::mem::take(&mut atoms));
                *pos += 1;
                continue;
            }
            '^' | '$' => return None,
            '*' | '+' | '?' | '{' => {
                let (min, max) = regex_quantifier(chars, pos)?;
                let last = atoms.last_mut()?;
                if (last.1, last.2) != (1, Some(1)) {
                    return None;
                }
                (last.1, last.2) = (min, max);
                // A non-greedy quantifier's NFA is the greedy one's.
                if chars.get(*pos) == Some(&'?') {
                    *pos += 1;
                }
                if matches!(chars.get(*pos), Some('*' | '+' | '?' | '{')) {
                    return None;
                }
                continue;
            }
            '(' => {
                *pos += 1;
                if chars.get(*pos) == Some(&'?') {
                    if chars.get(*pos + 1) != Some(&':') {
                        return None;
                    }
                    *pos += 2;
                }
                let inner = regex_atoms(chars, pos)?;
                if chars.get(*pos) != Some(&')') {
                    return None;
                }
                *pos += 1;
                atoms.push((ReAtom::Group(inner), 1, Some(1)));
                continue;
            }
            '.' => {
                *pos += 1;
                ReAtom::Set
            }
            '[' => regex_bracket(chars, pos)?,
            '\\' => {
                let e = *chars.get(*pos + 1)?;
                *pos += 2;
                match e {
                    'd' | 'D' | 's' | 'S' | 'w' | 'W' => ReAtom::Set,
                    'a' => ReAtom::Char('\u{7}'),
                    'b' => ReAtom::Char('\u{8}'),
                    'B' => ReAtom::Char('\\'),
                    'e' => ReAtom::Char('\u{1b}'),
                    'f' => ReAtom::Char('\u{c}'),
                    'n' => ReAtom::Char('\n'),
                    'r' => ReAtom::Char('\r'),
                    't' => ReAtom::Char('\t'),
                    'v' => ReAtom::Char('\u{b}'),
                    e if !e.is_alphanumeric() => ReAtom::Char(e),
                    _ => return None,
                }
            }
            c => {
                *pos += 1;
                ReAtom::Char(c)
            }
        };
        atoms.push((atom, 1, Some(1)));
    }
    alternatives.push(atoms);
    Some(alternatives)
}

/// The bounds of the quantifier at `*pos` (`*`, `+`, `?`, `{m}`, `{m,}`,
/// `{m,n}`), `*pos` moved past it.
fn regex_quantifier(chars: &[char], pos: &mut usize) -> Option<(u32, Option<u32>)> {
    let c = chars[*pos];
    *pos += 1;
    Some(match c {
        '*' => (0, None),
        '+' => (1, None),
        '?' => (0, Some(1)),
        _ => {
            let close = chars[*pos..].iter().position(|c| *c == '}')? + *pos;
            let inner: String = chars[*pos..close].iter().collect();
            *pos = close + 1;
            let number = |s: &str| -> Option<u32> {
                (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| s.parse().ok())
                    .flatten()
            };
            match inner.split_once(',') {
                None => {
                    let m = number(&inner)?;
                    (m, Some(m))
                }
                Some((m, "")) => (number(m)?, None),
                Some((m, n)) => {
                    let (m, n) = (number(m)?, number(n)?);
                    if n < m {
                        return None;
                    }
                    (m, Some(n))
                }
            }
        }
    })
}

/// The bracket expression at `*pos`, `*pos` moved past it: one character
/// when it lists exactly one, a [`ReAtom::Set`] when it is negated or
/// holds more (a character class or a range among them); `None` for a
/// collating element, an equivalence class or an escape, not modelled.
fn regex_bracket(chars: &[char], pos: &mut usize) -> Option<ReAtom> {
    *pos += 1;
    let negated = chars.get(*pos) == Some(&'^');
    if negated {
        *pos += 1;
    }
    let mut members: Vec<char> = Vec::new();
    let mut class = false;
    let mut first = true;
    loop {
        let c = *chars.get(*pos)?;
        if c == ']' && !first {
            *pos += 1;
            break;
        }
        first = false;
        match (c, chars.get(*pos + 1)) {
            ('[', Some(':')) => {
                let rest = &chars[*pos + 2..];
                let end = rest.windows(2).position(|w| w == [':', ']'])?;
                *pos += 2 + end + 2;
                class = true;
            }
            ('[', Some('.' | '=')) | ('\\', _) => return None,
            (lo, Some('-')) if chars.get(*pos + 2).is_some_and(|c| *c != ']') => {
                let hi = chars[*pos + 2];
                if hi == '\\' || hi == '[' {
                    return None;
                }
                if lo == hi {
                    if !members.contains(&lo) {
                        members.push(lo);
                    }
                } else {
                    class = true;
                }
                *pos += 3;
            }
            _ => {
                if !members.contains(&c) {
                    members.push(c);
                }
                *pos += 1;
            }
        }
    }
    Some(match (negated, class, members.as_slice()) {
        (false, false, [c]) => ReAtom::Char(*c),
        (false, false, []) => return None,
        _ => ReAtom::Set,
    })
}

/// `regex_selectivity_sub`: each fixed character `FIXED_CHAR_SEL`, each
/// bracket expression `CHAR_RANGE_SEL` (its complement when negated),
/// each `.` `ANY_CHAR_SEL`, each quantifier `PARTIAL_WILDCARD_SEL`, a
/// parenthesized item its own contents' selectivity, and the alternatives
/// of a `|` summed, the result capped at 1. Read over the pattern's bytes.
fn regex_selectivity_sub(patt: &[u8]) -> f64 {
    let mut sel = 1.0f64;
    let mut paren_depth = 0usize;
    let mut paren_pos = 0usize;
    let mut pos = 0usize;
    let len = patt.len();
    while pos < len {
        match patt[pos] {
            b'(' => {
                if paren_depth == 0 {
                    paren_pos = pos;
                }
                paren_depth += 1;
            }
            b')' if paren_depth > 0 => {
                paren_depth -= 1;
                if paren_depth == 0 {
                    sel *= regex_selectivity_sub(&patt[paren_pos + 1..pos]);
                }
            }
            b'|' if paren_depth == 0 => {
                sel += regex_selectivity_sub(&patt[pos + 1..]);
                break;
            }
            b'[' => {
                let mut negclass = false;
                pos += 1;
                if patt.get(pos) == Some(&b'^') {
                    negclass = true;
                    pos += 1;
                }
                if patt.get(pos) == Some(&b']') {
                    pos += 1;
                }
                while pos < len && patt[pos] != b']' {
                    pos += 1;
                }
                if paren_depth == 0 {
                    sel *= if negclass {
                        1.0 - CHAR_RANGE_SEL
                    } else {
                        CHAR_RANGE_SEL
                    };
                }
            }
            b'.' => {
                if paren_depth == 0 {
                    sel *= ANY_CHAR_SEL;
                }
            }
            b'*' | b'?' | b'+' => {
                if paren_depth == 0 {
                    sel *= PARTIAL_WILDCARD_SEL;
                }
            }
            b'{' => {
                while pos < len && patt[pos] != b'}' {
                    pos += 1;
                }
                if paren_depth == 0 {
                    sel *= PARTIAL_WILDCARD_SEL;
                }
            }
            b'\\' => {
                pos += 1;
                if pos >= len {
                    break;
                }
                if paren_depth == 0 {
                    sel *= FIXED_CHAR_SEL;
                }
            }
            _ => {
                if paren_depth == 0 {
                    sel *= FIXED_CHAR_SEL;
                }
            }
        }
        pos += 1;
    }
    sel.min(1.0)
}

/// `regex_selectivity`: [`regex_selectivity_sub`] of the pattern less a
/// trailing unescaped `$`, or, with none, times `FULL_WILDCARD_SEL` for the
/// implied trailing wildcard; a fixed prefix of `fixed_prefix_len` bytes
/// discounted (`FIXED_CHAR_SEL` each), and clamped to `[0, 1]`.
fn regex_selectivity(patt: &[u8], fixed_prefix_len: usize) -> f64 {
    let len = patt.len();
    let mut sel = if len > 0 && patt[len - 1] == b'$' && (len == 1 || patt[len - 2] != b'\\') {
        regex_selectivity_sub(&patt[..len - 1])
    } else {
        regex_selectivity_sub(patt) * FULL_WILDCARD_SEL
    };
    if fixed_prefix_len > 0 {
        let prefixsel = FIXED_CHAR_SEL.powi(i32::try_from(fixed_prefix_len).unwrap_or(i32::MAX));
        if prefixsel > 0.0 {
            sel /= prefixsel;
        }
    }
    sel.clamp(0.0, 1.0)
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
            let sel = col
                .pattern_sel(pattern, insensitive, negated, false)
                .unwrap();
            assert_eq!(rows(sel), expected, "{pattern} {insensitive} {negated}");
        }
        let gt = col.clause_sel(Op::Gt, b"xxxx").unwrap();
        assert_eq!(rows(gt), 27.0);
        let lt = col.clause_sel(Op::Lt, b"xxx").unwrap();
        assert_eq!(rows(lt), 1.0);
    }

    /// `regex_fixed_prefix`'s prefixes, each measured on PostgreSQL 15.19
    /// as the index condition a `~` (or `~*`) over a `C`-collated indexed
    /// column takes (`c >= prefix AND c < greater`, or `c = prefix` when
    /// exact); `None` where the walk is not modelled.
    #[test]
    fn regex_prefixes_match_postgres() {
        use PrefixStatus::{Exact, None as NoMatch, Partial};
        for (pattern, insensitive, expected) in [
            ("^abc", false, Some(("abc", Partial))),
            ("^abc", true, Some(("", NoMatch))),
            ("^abc$", false, Some(("abc", Exact))),
            ("^ab*c", false, Some(("a", Partial))),
            ("^ab+c", false, Some(("ab", Partial))),
            ("^ab{2}c", false, Some(("abbc", Partial))),
            ("^ab{2,3}c", false, Some(("a", Partial))),
            ("^ab{1,2}c", false, Some(("a", Partial))),
            ("^ab{0,1}c", false, Some(("a", Partial))),
            ("^ab{1,}c", false, Some(("ab", Partial))),
            ("^ab{3,}c", false, Some(("ab", Partial))),
            ("^ab{3,3}c", false, Some(("abbbc", Partial))),
            ("^ab{1,1}c", false, Some(("abc", Partial))),
            ("^a(bc){2}d", false, Some(("abcbcd", Partial))),
            ("^a(b){2}", false, Some(("abb", Partial))),
            ("^a(b)c+", false, Some(("abc", Partial))),
            ("^ab{0}c", false, Some(("ac", Partial))),
            ("^a{1}b", false, Some(("ab", Partial))),
            ("^a(bc)d", false, Some(("abcd", Partial))),
            ("^a(?:bc)d", false, Some(("abcd", Partial))),
            ("^(a)", false, Some(("a", Partial))),
            ("^a(bc)*d", false, Some(("a", Partial))),
            ("^a(b|c)", false, Some(("a", Partial))),
            ("^a(bc|bd)", false, Some(("ab", Partial))),
            ("^a(b|bd)", false, Some(("ab", Partial))),
            ("^a((b)c|bd)", false, Some(("ab", Partial))),
            ("^a(bc|d)", false, Some(("a", Partial))),
            ("^a(|b)", false, Some(("a", Partial))),
            ("^a(b?c|bd)", false, Some(("a", Partial))),
            ("^a(b|c)$", false, Some(("a", Partial))),
            ("^a[b]c", false, Some(("abc", Partial))),
            ("^[a-a]b", false, Some(("ab", Partial))),
            ("^[]]x", false, Some(("]x", Partial))),
            ("^[.]x", false, Some((".x", Partial))),
            ("^a[bc]", false, Some(("a", Partial))),
            ("^a[[:digit:]]", false, Some(("a", Partial))),
            ("^a.c", false, Some(("a", Partial))),
            (r"^a\.c", false, Some(("a.c", Partial))),
            (r"^a\dc", false, Some(("a", Partial))),
            (r"^a\tb", false, Some(("a\tb", Partial))),
            (r"^a\Bb", false, Some(("a\\b", Partial))),
            ("^ab*?c", false, Some(("a", Partial))),
            ("^ab?", false, Some(("a", Partial))),
            ("^a?b", false, Some(("", NoMatch))),
            ("^aB", false, Some(("aB", Partial))),
            ("^1x2", false, Some(("1x2", Partial))),
            ("^1x2", true, Some(("1", Partial))),
            ("^1[x]", true, Some(("1", Partial))),
            ("^12$", true, Some(("12", Exact))),
            (r"^1\$", false, Some(("1$", Partial))),
            ("^$", false, Some(("", Exact))),
            ("^", false, Some(("", NoMatch))),
            ("abc", false, Some(("", NoMatch))),
            ("^ab|^ac", false, Some(("", NoMatch))),
            ("^a|b", false, Some(("", NoMatch))),
            ("a|^b", false, Some(("", NoMatch))),
            ("^a$|^b$", false, Some(("", NoMatch))),
            // Not modelled: a group repeated at least once (`^(ab)+c` is
            // `a`, `^a(b)+` is `ab`), an anchor inside, a back reference.
            ("^(ab)+c", false, None),
            ("(^a)", false, None),
            ("^a\\1", false, None),
        ] {
            let got = regex_prefix(pattern, insensitive, true);
            let got = got.as_ref().map(|(p, s)| (p.as_str(), *s));
            assert_eq!(got, expected, "{pattern} {insensitive}");
        }
    }

    /// `regex_selectivity`, transcribed from PostgreSQL 15.19's
    /// `like_support.c`: an implied trailing wildcard without a `$`, the
    /// fixed prefix discounted, the result clamped to 1.
    #[test]
    fn regex_selectivity_follows_postgres() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-12;
        // `w01`: three fixed characters and the trailing wildcard.
        assert!(close(regex_selectivity(b"w01", 0), 0.2f64.powi(3) * 5.0));
        // A trailing `$` drops the wildcard; an escaped one does not.
        assert!(close(regex_selectivity(b"ab$", 0), 0.04));
        assert!(close(regex_selectivity(br"a\$", 0), 0.04 * 5.0));
        // Brackets, `.`, quantifiers, groups and alternatives.
        assert!(close(
            regex_selectivity(b"a[bc].x*$", 0),
            0.2 * 0.25 * 0.9 * 0.2 * 2.0
        ));
        assert!(close(regex_selectivity(b"[^a]$", 0), 0.75));
        assert!(close(regex_selectivity(b"(ab)c$", 0), 0.04 * 0.2));
        assert!(close(regex_selectivity(b"ab|c$", 0), 0.04 + 0.2));
        // `^abcd` with its prefix `abcd` discounted: the `^`, a fixed
        // character, and the wildcard.
        assert!(close(regex_selectivity(b"^abcd", 4), 1.0));
        assert!(close(regex_selectivity(b"^abcd.x$", 4), 0.2 * 0.9 * 0.2));
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
