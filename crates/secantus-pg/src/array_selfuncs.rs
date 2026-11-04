//! PostgreSQL 15's `array_selfuncs.c`: the selectivity of an array
//! column's `@>`, `<@` and `&&` against a constant (`arraycontsel`), and
//! of `constant = ANY / ALL (array column)` as a containment
//! (`scalararraysel_containment`), over the column's `compute_array_stats`
//! slots when ANALYZE recorded them (RP-368).
//!
//! Elements are compared by their sort keys, which order as the element
//! type's default B-tree comparison (`element_compare`). The arithmetic
//! keeps PostgreSQL's types: `float4` where it uses `float`, `float8`
//! where it uses `Selectivity` or `double`.

/// `DEFAULT_CONTAIN_SEL`.
const DEFAULT_CONTAIN_SEL: f64 = 0.005;

/// `EFFORT` in `mcelem_array_contained_selec`.
const EFFORT: usize = 100;

/// A column's `STATISTIC_KIND_MCELEM` slot -- its most common elements,
/// ascending, and their frequencies followed by the least and greatest
/// frequency and the NULL-element frequency -- and its
/// `STATISTIC_KIND_DECHIST` numbers, the last the average.
#[derive(Debug, Clone, Default)]
pub(crate) struct ElementSlots {
    pub mcelem: Vec<Vec<u8>>,
    pub numbers: Vec<f32>,
    pub hist: Vec<f32>,
}

/// The array operator `mcelem_array_selec` separates its cases by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArrayOp {
    Contains,
    Contained,
    Overlap,
}

impl ArrayOp {
    pub(crate) fn of(op: &str) -> Option<Self> {
        Some(match op {
            "@>" => ArrayOp::Contains,
            "<@" => ArrayOp::Contained,
            "&&" => ArrayOp::Overlap,
            _ => return None,
        })
    }
}

/// What `examine_variable` found for the column: no `pg_statistic` row,
/// or its null fraction and element slots (`None` with no MCELEM slot).
pub(crate) enum ColumnStats<'a> {
    None,
    Analysed {
        null_frac: f32,
        slots: Option<&'a ElementSlots>,
    },
}

/// `calc_arraycontsel` and `arraycontsel`'s clamp: `column OP constant`,
/// the constant's distinct non-NULL elements' keys ascending (`elems`).
pub(crate) fn arraycontsel(
    op: ArrayOp,
    elems: &[Vec<u8>],
    has_null: bool,
    stats: &ColumnStats,
) -> f64 {
    let selec = match stats {
        ColumnStats::Analysed { null_frac, slots } => {
            let s = match slots {
                // For `@>` and `&&` the histogram is not read.
                Some(sl) => mcelem_array_selec(
                    elems,
                    has_null,
                    Some(sl),
                    if op == ArrayOp::Contained {
                        &sl.hist
                    } else {
                        &[]
                    },
                    op,
                ),
                None => mcelem_array_selec(elems, has_null, None, &[], op),
            };
            // MCE stats count only non-null rows.
            s * (1.0 - f64::from(*null_frac))
        }
        ColumnStats::None => mcelem_array_selec(elems, has_null, None, &[], op),
    };
    selec.clamp(0.0, 1.0)
}

/// `scalararraysel_containment`: `constant = ANY / ALL (column)` (or
/// `<>`), the constant's key `elem`, `use_or` for ANY.
pub(crate) fn scalararraysel_containment(
    elem: &[u8],
    is_equality: bool,
    use_or: bool,
    stats: &ColumnStats,
) -> f64 {
    // `<>` swaps ANY and ALL, and its result is inverted.
    let use_or = if is_equality { use_or } else { !use_or };
    let one = [elem.to_vec()];
    let without = |op| match op {
        ArrayOp::Contains => mcelem_array_contain_overlap_selec(None, &one, op),
        _ => mcelem_array_contained_selec(None, &one, &[]),
    };
    let op = if use_or {
        ArrayOp::Contains
    } else {
        ArrayOp::Contained
    };
    let mut selec = match stats {
        ColumnStats::Analysed { null_frac, slots } => {
            let s = match slots {
                Some(sl) if use_or => mcelem_array_contain_overlap_selec(Some(sl), &one, op),
                Some(sl) => mcelem_array_contained_selec(Some(sl), &one, &sl.hist),
                None => without(op),
            };
            s * (1.0 - f64::from(*null_frac))
        }
        ColumnStats::None => without(op),
    };
    if !is_equality {
        selec = 1.0 - selec;
    }
    selec.clamp(0.0, 1.0)
}

/// `scalararraysel` of `constant OP ANY / ALL (column)` for an operator
/// it cannot treat as containment: the operator's restriction estimator
/// against a non-constant (its default, as no variable is found), combined
/// over `estimate_array_length`'s 10 elements. `None` for an operator
/// whose estimator is not one of the six.
pub(crate) fn scalararraysel_default(op: &str, use_or: bool) -> Option<f64> {
    let s2 = match op {
        "=" => DEFAULT_CONTAIN_SEL,
        "<>" => 1.0 - DEFAULT_CONTAIN_SEL,
        "<" | ">" | "<=" | ">=" => 1.0 / 3.0,
        _ => return None,
    };
    let mut s1: f64 = if use_or { 0.0 } else { 1.0 };
    for _ in 0..10 {
        if use_or {
            s1 = s1 + s2 - s1 * s2;
        } else {
            s1 *= s2;
        }
    }
    Some(s1.clamp(0.0, 1.0))
}

/// `mcelem_array_selec`: a NULL element makes `@>` select nothing; the
/// elements are already sorted and distinct.
fn mcelem_array_selec(
    elems: &[Vec<u8>],
    has_null: bool,
    slots: Option<&ElementSlots>,
    hist: &[f32],
    op: ArrayOp,
) -> f64 {
    if has_null && op == ArrayOp::Contains {
        return 0.0;
    }
    match op {
        ArrayOp::Contains | ArrayOp::Overlap => {
            mcelem_array_contain_overlap_selec(slots, elems, op)
        }
        ArrayOp::Contained => mcelem_array_contained_selec(slots, elems, hist),
    }
}

/// The MCELEM slot's values and numbers, the numbers dropped unless there
/// are three more of them than values.
fn usable(slots: Option<&ElementSlots>) -> (&[Vec<u8>], Option<&[f32]>) {
    match slots {
        Some(s) => (
            &s.mcelem,
            (s.numbers.len() == s.mcelem.len() + 3).then_some(s.numbers.as_slice()),
        ),
        None => (&[], None),
    }
}

/// `mcelem_array_contain_overlap_selec`.
fn mcelem_array_contain_overlap_selec(
    slots: Option<&ElementSlots>,
    elems: &[Vec<u8>],
    op: ArrayOp,
) -> f64 {
    let (mcelem, numbers) = usable(slots);
    let minfreq: f32 = match numbers {
        Some(n) => n[mcelem.len()],
        None => 2.0 * DEFAULT_CONTAIN_SEL as f32,
    };
    let mut selec: f64 = if op == ArrayOp::Contains { 1.0 } else { 0.0 };
    let mut mcelem_index = 0;
    for (i, e) in elems.iter().enumerate() {
        if i > 0 && elems[i - 1] == *e {
            continue;
        }
        // The smallest MCELEM >= this element (a binary search finds the
        // same one as the linear walk).
        let mut matched = false;
        while mcelem_index < mcelem.len() {
            match mcelem[mcelem_index].as_slice().cmp(e.as_slice()) {
                std::cmp::Ordering::Less => mcelem_index += 1,
                ord => {
                    matched = ord == std::cmp::Ordering::Equal;
                    break;
                }
            }
        }
        let elem_selec = match numbers {
            Some(n) if matched => {
                let f = f64::from(n[mcelem_index]);
                mcelem_index += 1;
                f
            }
            _ => DEFAULT_CONTAIN_SEL.min(f64::from(minfreq / 2.0)),
        };
        if op == ArrayOp::Contains {
            selec *= elem_selec;
        } else {
            selec = selec + elem_selec - selec * elem_selec;
        }
        selec = selec.clamp(0.0, 1.0);
    }
    selec
}

/// `mcelem_array_contained_selec`: `column <@ constant`, over the
/// distinct-element-count histogram `hist` (its last entry the average).
fn mcelem_array_contained_selec(
    slots: Option<&ElementSlots>,
    elems: &[Vec<u8>],
    hist: &[f32],
) -> f64 {
    let (mcelem, Some(numbers)) = usable(slots) else {
        return DEFAULT_CONTAIN_SEL;
    };
    if hist.len() < 3 {
        return DEFAULT_CONTAIN_SEL;
    }
    let nmcelem = mcelem.len();
    let minfreq = numbers[nmcelem];
    let nullelem_freq = numbers[nmcelem + 2];
    let avg_count = hist[hist.len() - 1];
    let mut rest: f32 = avg_count;
    let mut mult: f32 = 1.0;
    let mut elem_selec: Vec<f32> = Vec::with_capacity(elems.len());
    let mut mcelem_index = 0;
    for (i, e) in elems.iter().enumerate() {
        if i > 0 && elems[i - 1] == *e {
            continue;
        }
        let mut matched = false;
        while mcelem_index < nmcelem {
            match mcelem[mcelem_index].as_slice().cmp(e.as_slice()) {
                std::cmp::Ordering::Less => {
                    mult *= 1.0 - numbers[mcelem_index];
                    rest -= numbers[mcelem_index];
                    mcelem_index += 1;
                }
                ord => {
                    matched = ord == std::cmp::Ordering::Equal;
                    break;
                }
            }
        }
        if matched {
            elem_selec.push(numbers[mcelem_index]);
            rest -= numbers[mcelem_index];
            mcelem_index += 1;
        } else {
            elem_selec.push((DEFAULT_CONTAIN_SEL as f32).min(minfreq / 2.0));
        }
    }
    while mcelem_index < nmcelem {
        mult *= 1.0 - numbers[mcelem_index];
        rest -= numbers[mcelem_index];
        mcelem_index += 1;
    }
    // `mult *= exp(-rest)`: a double product stored back in a float.
    mult = (f64::from(mult) * (-f64::from(rest)).exp()) as f32;
    let mut unique_nitems = elem_selec.len();
    if nmcelem + unique_nitems > 0 && unique_nitems > EFFORT * nmcelem / (nmcelem + unique_nitems) {
        let b = nmcelem as f64;
        let n = ((b * b + 4.0 * EFFORT as f64 * b).sqrt() - b) / 2.0;
        // `float_compare_desc`.
        elem_selec.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        unique_nitems = n as usize;
    }
    let dist = calc_distr(
        &elem_selec[..unique_nitems.min(elem_selec.len())],
        unique_nitems,
        0.0,
    );
    let mcelem_dist = calc_distr(&numbers[..nmcelem], unique_nitems, rest);
    let hist_part = calc_hist(&hist[..hist.len() - 1], unique_nitems);
    let mut selec: f32 = 0.0;
    for i in 0..=unique_nitems {
        if mcelem_dist[i] > 0.0 {
            selec += hist_part[i] * mult * dist[i] / mcelem_dist[i];
        }
    }
    selec *= 1.0 - nullelem_freq;
    f64::from(selec).clamp(0.0, 1.0)
}

/// `calc_hist`: the probability of each distinct-element count `0..=n`
/// from the histogram's bounds.
fn calc_hist(hist: &[f32], n: usize) -> Vec<f32> {
    let nhist = hist.len();
    let mut hist_part = vec![0.0f32; n + 1];
    let mut i = 0usize;
    let mut prev_interval: f32 = 0.0;
    let frac: f32 = 1.0 / (nhist as f32 - 1.0);
    for (k, part) in hist_part.iter_mut().enumerate() {
        let mut count = 0;
        while i < nhist && hist[i] <= k as f32 {
            count += 1;
            i += 1;
        }
        if count > 0 {
            let next_interval = if i < nhist {
                hist[i] - hist[i - 1]
            } else {
                0.0
            };
            let mut val = (count - 1) as f32;
            if next_interval > 0.0 {
                val += 0.5 / next_interval;
            }
            if prev_interval > 0.0 {
                val += 0.5 / prev_interval;
            }
            *part = frac * val;
            prev_interval = next_interval;
        } else if prev_interval > 0.0 {
            *part = frac / prev_interval;
        }
    }
    hist_part
}

/// `calc_distr`: of independent events with probabilities `p`, the
/// probability that exactly `k` occur, `k` in `0..=m`, convolved with a
/// Poisson distribution of the rare ones (`rest`) when it is above
/// `DEFAULT_CONTAIN_SEL`. A count no row of the matrix reached is 0
/// (PostgreSQL leaves it unset; the callers never read one).
fn calc_distr(p: &[f32], m: usize, rest: f32) -> Vec<f32> {
    let mut row = vec![0.0f32; m + 1];
    let mut prev_row = vec![0.0f32; m + 1];
    row[0] = 1.0;
    for i in 1..=p.len() {
        let t = p[i - 1];
        std::mem::swap(&mut row, &mut prev_row);
        let mut j = 0;
        while j <= i && j <= m {
            let mut val = 0.0f32;
            if j < i {
                val += prev_row[j] * (1.0 - t);
            }
            if j > 0 {
                val += prev_row[j - 1] * t;
            }
            row[j] = val;
            j += 1;
        }
    }
    if f64::from(rest) > DEFAULT_CONTAIN_SEL {
        std::mem::swap(&mut row, &mut prev_row);
        row.iter_mut().for_each(|v| *v = 0.0);
        let mut t = (-f64::from(rest)).exp() as f32;
        for i in 0..=m {
            for j in 0..=(m - i) {
                row[j + i] += prev_row[j] * t;
            }
            t *= rest / (i + 1) as f32;
        }
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_statistics_each_element_is_the_default() {
        let one = [vec![1u8]];
        let s = arraycontsel(ArrayOp::Contains, &one, false, &ColumnStats::None);
        assert_eq!(s, 0.005);
        // `1 = ANY (a)` is `'{1}' <@ a`'s estimate; `= ALL` the contained
        // default; `<> ALL` one minus `= ANY`.
        assert_eq!(
            scalararraysel_containment(&[1], true, true, &ColumnStats::None),
            0.005
        );
        assert_eq!(
            scalararraysel_containment(&[1], true, false, &ColumnStats::None),
            0.005
        );
        assert_eq!(
            scalararraysel_containment(&[1], false, false, &ColumnStats::None),
            0.995
        );
    }

    #[test]
    fn a_common_element_selects_its_frequency_less_the_nulls() {
        let slots = ElementSlots {
            mcelem: vec![vec![1], vec![2]],
            numbers: vec![0.228, 0.2, 0.2, 0.228, 0.0],
            hist: vec![1.0, 2.0, 2.0, 1.9],
        };
        let stats = ColumnStats::Analysed {
            null_frac: 0.5,
            slots: Some(&slots),
        };
        let s = arraycontsel(ArrayOp::Contains, &[vec![1]], false, &stats);
        assert!((s - f64::from(0.228f32) * 0.5).abs() < 1e-12);
        // An element not among them: half the least frequency, at most
        // the default.
        let s = arraycontsel(ArrayOp::Contains, &[vec![9]], false, &stats);
        assert!((s - 0.005 * 0.5).abs() < 1e-12);
    }

    #[test]
    fn other_operators_combine_the_default_over_ten_elements() {
        let s = scalararraysel_default("<", true).unwrap();
        assert!((s - (1.0 - (2.0f64 / 3.0).powi(10))).abs() < 1e-12);
        assert!(scalararraysel_default("~", true).is_none());
    }
}
