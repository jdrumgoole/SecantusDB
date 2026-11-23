//! mongod's t-digest, the algorithm behind an approximate `$median` /
//! `$percentile` accumulator (`src/mongo/db/pipeline/percentile_algo_tdigest.{h,cpp}`,
//! r8.2.1): the `k2` scaling function, `delta` 2000
//! (`internalQueryTdigestDelta`), a buffer flushed every `3 * delta` finite
//! values, and linear interpolation between centroid means. Ported line for
//! line, including what reads as a quirk (`_max` starts at `DBL_MIN`, the
//! smallest positive double), because the answer depends on it.

/// `internalQueryTdigestDelta`'s default.
pub(crate) const DELTA: f64 = 2000.0;
/// `bufferCoeff * delta`.
const MAX_BUFFER: usize = 3 * 2000;

#[derive(Clone, Copy, Debug)]
struct Centroid {
    weight: f64,
    mean: f64,
}

impl Centroid {
    fn add(&mut self, other: Centroid) {
        if other.weight < self.weight {
            self.weight += other.weight;
            self.mean += (other.mean - self.mean) * (other.weight / self.weight);
        } else {
            let new_weight = self.weight + other.weight;
            self.mean = other.mean + (self.mean - other.mean) * (self.weight / new_weight);
            self.weight = new_weight;
        }
    }

    fn add_value(&mut self, value: f64) {
        self.weight += 1.0;
        self.mean += (value - self.mean) / self.weight;
    }
}

fn k2(q: f64, delta: f64) -> f64 {
    let sqrt_of2_recip = 1.0 / 2.0f64.sqrt();
    if q <= 0.5 {
        return delta * sqrt_of2_recip * q.sqrt();
    }
    delta - delta * sqrt_of2_recip * (1.0 - q).sqrt()
}

fn k2_inverse(s: f64, delta: f64) -> f64 {
    let r = s / delta;
    if r <= 0.5 {
        return 2.0 * r * r;
    }
    let b = 1.0 - r;
    1.0 - 2.0 * b * b
}

fn k2_limit(q: f64, delta: f64) -> f64 {
    k2_inverse(1.0 + k2(q, delta), delta)
}

/// `PercentileAlgorithm::computeTrueRank` (an `int` there).
fn true_rank(n: i64, p: f64) -> i64 {
    if p >= 1.0 {
        return n - 1;
    }
    (((n as f64) * p).ceil() as i32 as i64 - 1).max(0)
}

pub(crate) struct TDigest {
    buffer: Vec<f64>,
    centroids: Vec<Centroid>,
    n: i64,
    neg_inf: i64,
    pos_inf: i64,
    min: f64,
    max: f64,
}

impl TDigest {
    pub(crate) fn new() -> Self {
        TDigest {
            buffer: Vec::new(),
            centroids: Vec::new(),
            n: 0,
            neg_inf: 0,
            pos_inf: 0,
            min: f64::MAX,
            // `std::numeric_limits<double>::min()`: the smallest positive normal.
            max: f64::MIN_POSITIVE,
        }
    }

    /// `TDigest::incorporate(double)`.
    pub(crate) fn incorporate(&mut self, input: f64) {
        if input.is_nan() {
            return;
        }
        if input.is_infinite() {
            if input < 0.0 {
                self.neg_inf += 1;
            } else {
                self.pos_inf += 1;
            }
            return;
        }
        self.buffer.push(input);
        if self.buffer.len() >= MAX_BUFFER {
            self.flush_buffer();
        }
    }

    fn flush_buffer(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        let mut sorted = std::mem::take(&mut self.buffer);
        sorted.sort_by(|a, b| a.partial_cmp(b).expect("NaN never buffered"));
        self.merge_sorted(&sorted);
    }

    fn merge_sorted(&mut self, sorted: &[f64]) {
        if sorted.is_empty() {
            return;
        }
        self.n += sorted.len() as i64;
        self.min = self.min.min(sorted[0]);
        self.max = self.max.max(sorted[sorted.len() - 1]);
        let n = self.n as f64;
        let old = std::mem::take(&mut self.centroids);
        let mut temp: Vec<Centroid> = Vec::with_capacity(old.len() + sorted.len());
        let (mut ic, mut is) = (0usize, 0usize);
        let mut w: i64 = 0;
        let limit = |w: i64| n * k2_limit(w as f64 / n, DELTA);
        while ic < old.len() && is < sorted.len() {
            let mut cur = if old[ic].mean > sorted[is] {
                is += 1;
                Centroid {
                    weight: 1.0,
                    mean: sorted[is - 1],
                }
            } else {
                ic += 1;
                old[ic - 1]
            };
            let q_limit = limit(w);
            while ic < old.len() && is < sorted.len() {
                if old[ic].mean > sorted[is] {
                    if w as f64 + cur.weight + 1.0 <= q_limit {
                        cur.add_value(sorted[is]);
                        is += 1;
                    } else {
                        break;
                    }
                } else if w as f64 + cur.weight + old[ic].weight <= q_limit {
                    cur.add(old[ic]);
                    ic += 1;
                } else {
                    break;
                }
            }
            temp.push(cur);
            w += cur.weight as i64;
        }
        while ic < old.len() {
            let mut cur = old[ic];
            ic += 1;
            let q_limit = limit(w);
            while ic < old.len() && w as f64 + cur.weight + old[ic].weight <= q_limit {
                cur.add(old[ic]);
                ic += 1;
            }
            temp.push(cur);
            w += cur.weight as i64;
        }
        while is < sorted.len() {
            let mut cur = Centroid {
                weight: 1.0,
                mean: sorted[is],
            };
            is += 1;
            let q_limit = limit(w);
            while is < sorted.len() && w as f64 + cur.weight + 1.0 <= q_limit {
                cur.add_value(sorted[is]);
                is += 1;
            }
            temp.push(cur);
            w += cur.weight as i64;
        }
        debug_assert_eq!(w, self.n, "a t-digest merge lost or duplicated inputs");
        self.centroids = temp;
    }

    fn min_value(&self) -> f64 {
        if self.neg_inf > 0 {
            f64::NEG_INFINITY
        } else {
            self.min
        }
    }

    fn max_value(&self) -> f64 {
        if self.pos_inf > 0 {
            f64::INFINITY
        } else {
            self.max
        }
    }

    /// `TDigest::computePercentile`: None when nothing was incorporated.
    pub(crate) fn percentile(&mut self, p: f64) -> Option<f64> {
        self.flush_buffer();
        if self.centroids.is_empty() && self.neg_inf == 0 && self.pos_inf == 0 {
            return None;
        }
        if p >= 1.0 {
            return Some(self.max_value());
        } else if p <= 0.0 {
            return Some(self.min_value());
        }
        let mut rank = true_rank(self.n + self.pos_inf + self.neg_inf, p);
        if self.neg_inf > 0 && rank < self.neg_inf {
            return Some(f64::NEG_INFINITY);
        } else if self.pos_inf > 0 && rank >= self.n + self.neg_inf {
            return Some(f64::INFINITY);
        }
        rank -= self.neg_inf;
        let c = &self.centroids;
        let rank = rank as f64;
        let mut i = 0usize;
        let mut r = 0.0;
        while i < c.len() {
            r += c[i].weight;
            if r > rank {
                break;
            }
            i += 1;
        }
        if c[i].weight == 1.0 {
            return Some(c[i].mean);
        }
        let w_cur = c[i].weight;
        let (left, right, w_left, w_right, doubled_inner_rank);
        if r - rank >= w_cur / 2.0 {
            left = if i == 0 { self.min } else { c[i - 1].mean };
            right = c[i].mean;
            w_left = if i == 0 { 0.0 } else { c[i - 1].weight };
            w_right = w_cur;
            doubled_inner_rank = w_left + 2.0 * w_right - 2.0 * (r - 1.0 - rank);
        } else {
            let last = i == c.len() - 1;
            left = c[i].mean;
            right = if last { self.max } else { c[i + 1].mean };
            w_left = w_cur;
            w_right = if last { 0.0 } else { c[i + 1].weight };
            doubled_inner_rank = w_left - 2.0 * (r - 1.0 - rank);
        }
        let inner_p = doubled_inner_rank / (w_left + w_right);
        let res = left + (right - left) * inner_p;
        Some(if res > right { right } else { res })
    }
}

#[cfg(test)]
mod tests {
    use super::TDigest;

    #[test]
    fn fewer_values_than_a_buffer_answer_the_exact_rank() {
        let mut d = TDigest::new();
        for v in [3.0, 1.0, 2.0, 5.0, 4.0] {
            d.incorporate(v);
        }
        assert_eq!(d.percentile(0.5), Some(3.0));
        assert_eq!(d.percentile(0.0), Some(1.0));
        assert_eq!(d.percentile(1.0), Some(5.0));
    }

    #[test]
    fn infinities_are_counted_not_merged() {
        let mut d = TDigest::new();
        d.incorporate(f64::NEG_INFINITY);
        d.incorporate(f64::INFINITY);
        d.incorporate(f64::INFINITY);
        assert_eq!(d.percentile(0.1), Some(f64::NEG_INFINITY));
        assert_eq!(d.percentile(0.9), Some(f64::INFINITY));
        assert_eq!(TDigest::new().percentile(0.5), None);
    }

    /// mongod 8.2.11's answers (RM-720), over `(i * 7919) % 30011 * 0.37`.
    #[test]
    fn a_flushed_digest_interpolates_as_mongods() {
        let median = |count: usize| {
            let mut d = TDigest::new();
            for i in 0..count {
                d.incorporate(((i * 7919) % 30011) as f64 * 0.37);
            }
            d.percentile(0.5).unwrap()
        };
        assert_eq!(median(5999), 5552.219999999999);
        assert_eq!(median(20_000), 5551.609969919785);
    }
}
