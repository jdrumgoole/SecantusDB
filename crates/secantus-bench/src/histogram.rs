//! Sparse log-linear latency histogram, in microseconds.
//!
//! Latency is accumulated into buckets rather than a sample array because
//! histograms **merge by adding counts**: that is what lets one report combine
//! every worker thread across both client droplets into a single set of
//! percentiles without shipping raw samples between machines.
//!
//! Layout: 64 sub-buckets per power-of-two octave, so the worst-case bucket is
//! ~1.6% wide — comfortably finer than the run-to-run noise of a network
//! benchmark, and far cheaper than keeping millions of samples.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

const SUB_BITS: u32 = 6;
const SUB_COUNT: u64 = 1 << SUB_BITS;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Histogram {
    /// Bucket index -> count. `BTreeMap` so percentile walks are already
    /// ordered and the serialised form is stable.
    pub counts: BTreeMap<u64, u64>,
    pub total: u64,
    pub min_us: f64,
    pub max_us: f64,
    pub sum_us: f64,
}

impl Histogram {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn index_for(us: f64) -> u64 {
        let v = if us < 1.0 { 1u64 } else { us as u64 };
        let octave = 63 - v.leading_zeros() as u64;
        if octave < SUB_BITS as u64 {
            return v;
        }
        let sub = (v >> (octave - SUB_BITS as u64)) & (SUB_COUNT - 1);
        (octave - SUB_BITS as u64 + 1) * SUB_COUNT + sub
    }

    /// Midpoint of the bucket, in microseconds.
    pub fn value_for(index: u64) -> f64 {
        if index < SUB_COUNT {
            return index as f64;
        }
        let octave = index / SUB_COUNT + SUB_BITS as u64 - 1;
        let sub = index % SUB_COUNT;
        let shift = octave - SUB_BITS as u64;
        (((SUB_COUNT + sub) << shift) as f64) + ((1u64 << shift) as f64) / 2.0
    }

    pub fn record(&mut self, us: f64) {
        *self.counts.entry(Self::index_for(us)).or_insert(0) += 1;
        if self.total == 0 || us < self.min_us {
            self.min_us = us;
        }
        if us > self.max_us {
            self.max_us = us;
        }
        self.total += 1;
        self.sum_us += us;
    }

    pub fn merge(&mut self, other: &Histogram) {
        for (idx, n) in &other.counts {
            *self.counts.entry(*idx).or_insert(0) += n;
        }
        if other.total > 0 {
            if self.total == 0 || other.min_us < self.min_us {
                self.min_us = other.min_us;
            }
            if other.max_us > self.max_us {
                self.max_us = other.max_us;
            }
        }
        self.total += other.total;
        self.sum_us += other.sum_us;
    }

    /// Latency in **milliseconds** at percentile `p` (0-100).
    pub fn percentile(&self, p: f64) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        let target = p / 100.0 * self.total as f64;
        let mut seen = 0u64;
        for (idx, n) in &self.counts {
            seen += n;
            if seen as f64 >= target {
                return Self::value_for(*idx) / 1000.0;
            }
        }
        self.max_us / 1000.0
    }

    pub fn mean_ms(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.sum_us / self.total as f64 / 1000.0
        }
    }

    pub fn summary(&self) -> LatencySummary {
        LatencySummary {
            count: self.total,
            mean_ms: round3(self.mean_ms()),
            min_ms: round3(self.min_us / 1000.0),
            p50_ms: round3(self.percentile(50.0)),
            p90_ms: round3(self.percentile(90.0)),
            p99_ms: round3(self.percentile(99.0)),
            p999_ms: round3(self.percentile(99.9)),
            max_ms: round3(self.max_us / 1000.0),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LatencySummary {
    pub count: u64,
    pub mean_ms: f64,
    pub min_ms: f64,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p99_ms: f64,
    pub p999_ms: f64,
    pub max_ms: f64,
}

pub fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

pub fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_error_stays_under_two_percent() {
        for us in [
            1u64, 2, 63, 64, 65, 100, 999, 1_000, 12_345, 1_000_000, 60_000_000,
        ] {
            let back = Histogram::value_for(Histogram::index_for(us as f64));
            let err = (back - us as f64).abs() / us as f64;
            assert!(err < 0.02, "us={us} back={back} err={err}");
        }
    }

    #[test]
    fn percentiles_track_a_known_distribution() {
        let mut hist = Histogram::new();
        let mut samples: Vec<f64> = Vec::new();
        for i in 0..100_000u64 {
            // Deterministic spread over [1000, 11000) us.
            let v = 1000.0 + (i % 10_000) as f64;
            hist.record(v);
            samples.push(v);
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for p in [50.0, 90.0, 99.0] {
            let exact = samples[(p / 100.0 * samples.len() as f64) as usize] / 1000.0;
            let got = hist.percentile(p);
            assert!(
                (got - exact).abs() / exact < 0.02,
                "p{p}: got {got} exact {exact}"
            );
        }
    }

    #[test]
    fn merge_is_additive() {
        let (mut a, mut b, mut whole) = (Histogram::new(), Histogram::new(), Histogram::new());
        for i in 1..=2000u64 {
            if i % 2 == 1 {
                a.record(i as f64);
            } else {
                b.record(i as f64);
            }
            whole.record(i as f64);
        }
        a.merge(&b);
        assert_eq!(a.total, whole.total);
        assert_eq!(a.counts, whole.counts);
        assert_eq!(a.min_us, whole.min_us);
        assert_eq!(a.max_us, whole.max_us);
        assert_eq!(a.percentile(50.0), whole.percentile(50.0));
    }

    #[test]
    fn empty_histogram_summarises_to_zero() {
        let hist = Histogram::new();
        assert_eq!(hist.summary().count, 0);
        assert_eq!(hist.percentile(99.0), 0.0);
    }

    #[test]
    fn json_round_trip_preserves_the_summary() {
        let mut hist = Histogram::new();
        for v in [12.0, 340.0, 5600.0] {
            hist.record(v);
        }
        let text = serde_json::to_string(&hist).unwrap();
        let clone: Histogram = serde_json::from_str(&text).unwrap();
        assert_eq!(clone.total, hist.total);
        assert_eq!(clone.summary().p99_ms, hist.summary().p99_ms);
    }
}
