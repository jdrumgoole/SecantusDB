//! A hashed set of DISTINCT / GROUP BY identities.
//!
//! The identities are what `group_key_ident` reduces a value to (numerics by
//! their scale-free sort key, every NaN one value, `-0` as `0`), and two of
//! them are the same group exactly when they are `==`. The de-duplication
//! used to test membership with `Vec::contains`, which is that equality but
//! quadratic in the number of distinct rows (batch 58). This set keeps the
//! SAME equality -- `==` decides, inside a bucket -- and only uses a hash to
//! pick the bucket, so the answer cannot change: the hash is built to be
//! coarser than `==` (equal values always hash alike), which is the one
//! property a bucket needs.
//!
//! `Bson` has no `Hash`; `hash_bson` walks the value. A float is hashed by a
//! normalised bit pattern (`-0` as `0`, every NaN alike) because `==` treats
//! `0.0` and `-0.0` as equal inside an array or a document, where
//! `group_key_ident` does not reach.

use bson::Bson;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, Hasher};

/// A key the set can hold: hashed consistently with its own `==`.
pub(crate) trait IdentHash: PartialEq {
    fn ident_hash<H: Hasher>(&self, h: &mut H);
}

fn hash_f64<H: Hasher>(d: f64, h: &mut H) {
    let bits = if d.is_nan() {
        u64::MAX
    } else if d == 0.0 {
        0
    } else {
        d.to_bits()
    };
    bits.hash(h);
}

/// Hash one `Bson` so that `a == b` implies equal hashes.
pub(crate) fn hash_bson<H: Hasher>(v: &Bson, h: &mut H) {
    std::mem::discriminant(v).hash(h);
    match v {
        Bson::Double(d) => hash_f64(*d, h),
        Bson::String(s) => s.hash(h),
        Bson::Int32(n) => n.hash(h),
        Bson::Int64(n) => n.hash(h),
        Bson::Boolean(b) => b.hash(h),
        Bson::Array(a) => {
            a.len().hash(h);
            for e in a {
                hash_bson(e, h);
            }
        }
        Bson::Document(d) => {
            d.len().hash(h);
            for (k, e) in d {
                k.hash(h);
                hash_bson(e, h);
            }
        }
        Bson::Decimal128(d) => d.bytes().hash(h),
        Bson::Binary(b) => b.bytes.hash(h),
        Bson::DateTime(t) => t.timestamp_millis().hash(h),
        Bson::ObjectId(o) => o.bytes().hash(h),
        Bson::Timestamp(t) => (t.time, t.increment).hash(h),
        Bson::Symbol(s) | Bson::JavaScriptCode(s) => s.hash(h),
        Bson::RegularExpression(r) => (&r.pattern, &r.options).hash(h),
        // Everything else hashes by its kind alone: still consistent with
        // `==` (equal values share a kind), merely a coarser bucket.
        _ => {}
    }
}

impl IdentHash for Bson {
    fn ident_hash<H: Hasher>(&self, h: &mut H) {
        hash_bson(self, h)
    }
}

impl<T: IdentHash> IdentHash for Option<T> {
    fn ident_hash<H: Hasher>(&self, h: &mut H) {
        match self {
            None => 0u8.hash(h),
            Some(v) => {
                1u8.hash(h);
                v.ident_hash(h)
            }
        }
    }
}

impl<T: IdentHash> IdentHash for Vec<T> {
    fn ident_hash<H: Hasher>(&self, h: &mut H) {
        self.len().hash(h);
        for v in self {
            v.ident_hash(h);
        }
    }
}

impl<A: IdentHash, B: IdentHash> IdentHash for (A, B) {
    fn ident_hash<H: Hasher>(&self, h: &mut H) {
        self.0.ident_hash(h);
        self.1.ident_hash(h);
    }
}

/// The set: buckets by hash, membership by `==`.
pub(crate) struct DistinctSet<K> {
    state: std::collections::hash_map::RandomState,
    buckets: HashMap<u64, Vec<K>>,
}

impl<K: IdentHash> DistinctSet<K> {
    pub(crate) fn new() -> Self {
        Self {
            state: Default::default(),
            buckets: HashMap::new(),
        }
    }

    /// Add `k`; true when it was not there yet (the row is the group's first).
    pub(crate) fn insert(&mut self, k: K) -> bool {
        let mut h = self.state.build_hasher();
        k.ident_hash(&mut h);
        let bucket = self.buckets.entry(h.finish()).or_default();
        if bucket.contains(&k) {
            false
        } else {
            bucket.push(k);
            true
        }
    }
}

/// A multiset of identities, for INTERSECT / EXCEPT: buckets by hash, a
/// count per member, membership by `==` (as `DistinctSet`).
pub(crate) struct IdentCounts<K> {
    state: std::collections::hash_map::RandomState,
    buckets: HashMap<u64, Vec<(K, usize)>>,
}

impl<K: IdentHash> IdentCounts<K> {
    pub(crate) fn new() -> Self {
        Self {
            state: Default::default(),
            buckets: HashMap::new(),
        }
    }

    fn hash_of(&self, k: &K) -> u64 {
        let mut h = self.state.build_hasher();
        k.ident_hash(&mut h);
        h.finish()
    }

    pub(crate) fn add(&mut self, k: K) {
        let bucket = self.buckets.entry(self.hash_of(&k)).or_default();
        match bucket.iter_mut().find(|(m, _)| *m == k) {
            Some((_, n)) => *n += 1,
            None => bucket.push((k, 1)),
        }
    }

    /// Whether `k` has a member left; `consume` takes one of it away.
    pub(crate) fn take(&mut self, k: &K, consume: bool) -> bool {
        let h = self.hash_of(k);
        let Some(bucket) = self.buckets.get_mut(&h) else {
            return false;
        };
        match bucket.iter_mut().find(|(m, n)| m == k && *n > 0) {
            Some((_, n)) => {
                if consume {
                    *n -= 1;
                }
                true
            }
            None => false,
        }
    }
}

/// Identities numbered in the order first seen, for GROUP BY: buckets by
/// hash, membership by `==` (as `DistinctSet`).
pub(crate) struct IdentIndex<K> {
    state: std::collections::hash_map::RandomState,
    buckets: HashMap<u64, Vec<(K, usize)>>,
    len: usize,
}

impl<K: IdentHash> IdentIndex<K> {
    pub(crate) fn new() -> Self {
        Self {
            state: Default::default(),
            buckets: HashMap::new(),
            len: 0,
        }
    }

    /// The number of `k`: `Ok` an existing one, `Err` the new one assigned.
    pub(crate) fn find_or_add(&mut self, k: K) -> Result<usize, usize> {
        let mut h = self.state.build_hasher();
        k.ident_hash(&mut h);
        let bucket = self.buckets.entry(h.finish()).or_default();
        if let Some((_, i)) = bucket.iter().find(|(m, _)| *m == k) {
            return Ok(*i);
        }
        let i = self.len;
        self.len += 1;
        bucket.push((k, i));
        Err(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_values_are_one_member() {
        let mut s: DistinctSet<Vec<Option<Bson>>> = DistinctSet::new();
        assert!(s.insert(vec![Some(Bson::Int32(1)), None]));
        assert!(!s.insert(vec![Some(Bson::Int32(1)), None]));
        assert!(s.insert(vec![Some(Bson::Int64(1)), None]));
        assert!(s.insert(vec![None, None]));
        assert!(!s.insert(vec![None, None]));
    }

    #[test]
    fn signed_zero_inside_an_array_is_one_member() {
        // `==` calls these equal, so the hash must too.
        let mut s: DistinctSet<Option<Bson>> = DistinctSet::new();
        assert!(s.insert(Some(Bson::Array(vec![Bson::Double(0.0)]))));
        assert!(!s.insert(Some(Bson::Array(vec![Bson::Double(-0.0)]))));
    }

    #[test]
    fn many_distinct_values() {
        let mut s: DistinctSet<Option<Bson>> = DistinctSet::new();
        for i in 0..10_000 {
            assert!(s.insert(Some(Bson::Int64(i))));
        }
        for i in 0..10_000 {
            assert!(!s.insert(Some(Bson::Int64(i))));
        }
    }
}
