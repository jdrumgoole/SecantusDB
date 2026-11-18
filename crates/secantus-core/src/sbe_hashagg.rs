//! The SBE `$group`'s memory check and spill accounting, as mongod 8.2's
//! `HashAggBaseStage` keeps them (`hashagg_base.cpp`, RM-612).
//!
//! mongod does not add up what its hash table holds. At a checkpoint it
//! takes the row the last document went to -- its key row and its
//! accumulator row, each `MaterializedRow::memUsageForSorter` -- and
//! multiplies it by the number of groups; it spills when that reaches
//! `internalQuerySlotBasedExecutionHashAggApproxMemoryUseInBytesBeforeSpill`.
//! Checkpoints come at a pace it sets from how fast that estimate grew
//! ([`MemoryCheck`]). A spill counts, for each group, the bytes of the
//! record it writes: the accumulator row as `serializeForSorter` writes it
//! and the type bits of the key string the key becomes (the key string
//! itself is the record id, not counted).
//!
//! Every size below is mongod's SBE value layout, measured on 8.2.11
//! (2026-11-18): one group of one document under a budget of 1 spills it
//! alone, giving the record's bytes; the largest budget at which it still
//! spills is its row's size.

use bson::Bson;

use crate::group::{Acc, ConstSum, NElemKind, Num, TopNKind, Width};
use crate::memlimit;

/// `sizeof(MaterializedRow)`: a pointer and a count.
const ROW: usize = 16;
/// `sizeof(TypeTags) + sizeof(Value)`, every value's own part.
const VALUE: usize = memlimit::SBE_VALUE;
/// `sizeof(value::Array)`: its vector.
const ARRAY: usize = 24;
/// `sizeof(value::ArraySet)`: the absl hash set and its functors (measured).
const ARRAY_SET: usize = 48;
/// `sizeof(value::Object)`: three vectors.
const OBJECT: usize = 72;

/// `internalQuerySlotBasedExecutionHashAggMemoryUseCheckMargin`.
const CHECK_MARGIN: f64 = 0.7;
/// `internalQuerySlotBasedExecutionHashAggMemoryCheckPerAdvanceAtMost`.
const CHECK_AT_MOST: i64 = 2;
/// `internalQuerySlotBasedExecutionHashAggMemoryCheckPerAdvanceAtLeast`.
const CHECK_AT_LEAST: i64 = 1024;

/// One accumulator as the `$group` compiled it.
pub(crate) struct AccSpec<'a> {
    pub(crate) op: &'a str,
    pub(crate) arg: &'a Bson,
}

/// `HashAggBaseStage::MemoryCheckData` and
/// `checkMemoryUsageAndSpillIfNecessary`.
pub(crate) struct MemoryCheck {
    frequency: i64,
    next: i64,
    counter: i64,
    last: i64,
}

impl Default for MemoryCheck {
    fn default() -> Self {
        MemoryCheck {
            frequency: CHECK_AT_MOST.min(CHECK_AT_LEAST),
            next: 0,
            counter: 0,
            last: 0,
        }
    }
}

impl MemoryCheck {
    /// One more document went into the table, which now holds `groups`
    /// groups. `row` measures the group it went to and is only called at a
    /// checkpoint. True when the table must spill (the caller then starts a
    /// fresh check, as `spill(mcd)` resets it).
    pub(crate) fn should_spill(
        &mut self,
        groups: usize,
        row: impl FnOnce() -> usize,
        budget: usize,
    ) -> bool {
        self.counter += 1;
        if self.counter < self.next {
            return false;
        }
        let wide = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
        let budget = wide(budget);
        let estimate = wide(groups).saturating_mul(wide(row()));
        if estimate >= budget {
            *self = MemoryCheck::default();
            return true;
        }
        let gain = (estimate - self.last) as f64 / self.counter as f64;
        let candidate = if gain > 0.1 {
            // A double converted to `long`, as C++ does: toward zero.
            (CHECK_MARGIN * (budget - estimate) as f64 / gain) as i64
        } else {
            self.next.saturating_mul(2)
        };
        self.next = self.frequency.min(CHECK_AT_MOST.max(candidate));
        self.last = estimate;
        self.counter = 0;
        self.frequency = self.frequency.saturating_mul(2).min(CHECK_AT_LEAST);
        false
    }
}

/// A value's two sizes: `getApproximateSize` (memory) and what
/// `serializeValue` writes (spill).
#[derive(Clone, Copy, Default)]
struct Sized {
    mem: usize,
    disk: usize,
}

impl std::ops::Add for Sized {
    type Output = Sized;
    fn add(self, o: Sized) -> Sized {
        Sized {
            mem: self.mem + o.mem,
            disk: self.disk + o.disk,
        }
    }
}

impl std::iter::Sum for Sized {
    fn sum<I: Iterator<Item = Sized>>(it: I) -> Sized {
        it.fold(Sized::default(), |a, b| a + b)
    }
}

/// A scalar of `disk` serialized bytes and no memory beyond its own part.
fn shallow(disk: usize) -> Sized {
    Sized { mem: VALUE, disk }
}

/// Missing (`Nothing`): its tag alone on disk.
fn nothing() -> Sized {
    shallow(1)
}

fn int32() -> Sized {
    shallow(1 + 4)
}

fn int64() -> Sized {
    shallow(1 + 8)
}

fn boolean() -> Sized {
    shallow(1 + 1)
}

/// A value taken from a document. Strings of up to seven bytes are small
/// (inline, written as a C string); documents and arrays stay BSON.
fn value(v: &Bson) -> Sized {
    let mem = memlimit::sbe_size(v);
    let disk = match v {
        Bson::Null | Bson::Undefined | Bson::MinKey | Bson::MaxKey => 1,
        Bson::Int32(_) => 1 + 4,
        Bson::Int64(_) | Bson::Double(_) | Bson::DateTime(_) | Bson::Timestamp(_) => 1 + 8,
        Bson::Boolean(_) => 1 + 1,
        Bson::Decimal128(_) => 1 + 16,
        Bson::String(s) if s.len() <= memlimit::SBE_SMALL_STR => 1 + s.len() + 1,
        Bson::String(s) | Bson::Symbol(s) | Bson::JavaScriptCode(s) => 1 + 4 + s.len(),
        Bson::Document(d) => 1 + memlimit::bson_len(d),
        Bson::Array(a) => 1 + memlimit::array_len(a),
        Bson::ObjectId(_) => 1 + 12,
        Bson::Binary(b) => 1 + 4 + 1 + b.bytes.len(),
        Bson::RegularExpression(r) => 1 + r.pattern.len() + 1 + r.options.len() + 1,
        Bson::JavaScriptCodeWithScope(c) => 1 + 4 + c.code.len() + memlimit::bson_len(&c.scope),
        Bson::DbPointer(_) => 1 + memlimit::value_len(v),
    };
    Sized { mem, disk }
}

fn maybe(v: Option<&Bson>) -> Sized {
    v.map_or_else(nothing, value)
}

/// `value::Array` of `items`: a count on disk.
fn array(items: impl IntoIterator<Item = Sized>) -> Sized {
    let inner: Sized = items.into_iter().sum();
    Sized {
        mem: VALUE + ARRAY + inner.mem,
        disk: 1 + 8 + inner.disk,
    }
}

/// `value::ArraySet` of `items`: a collator flag and a count on disk.
fn array_set(items: impl IntoIterator<Item = Sized>) -> Sized {
    let inner: Sized = items.into_iter().sum();
    Sized {
        mem: VALUE + ARRAY_SET + inner.mem,
        disk: 1 + 1 + 8 + inner.disk,
    }
}

/// `value::Object` of `fields`: a count, then each name as a C string.
fn object<'a>(fields: impl IntoIterator<Item = (&'a str, &'a Bson)>) -> Sized {
    let mut s = Sized {
        mem: VALUE + OBJECT,
        disk: 1 + 8,
    };
    for (name, v) in fields {
        let v = value(v);
        s.mem += name.len() + v.mem;
        s.disk += name.len() + 1 + v.disk;
    }
    s
}

/// `aggDoubleDoubleSum`'s state: the total's type (an int32 while every
/// number was one, then 8 bytes), the sum and its addend, and the decimal
/// total once a decimal arrived.
fn double_double(width: Width, decimal: bool) -> Sized {
    let tag = if width == Width::Int {
        int32()
    } else {
        int64()
    };
    let mut parts = vec![tag, int64(), int64()];
    if decimal {
        parts.push(Sized {
            mem: VALUE + 16,
            disk: 1 + 16,
        });
    }
    array(parts)
}

fn sum_state(num: Option<&Num>) -> Sized {
    match num {
        None => double_double(Width::Int, false),
        Some(Num::Sum { width, decimal, .. }) => double_double(*width, decimal.is_some()),
        Some(Num::Const(c)) => double_double(
            match c {
                ConstSum::Int(_) => Width::Int,
                ConstSum::Long(_) => Width::Long,
                ConstSum::Double(_) => Width::Double,
            },
            false,
        ),
    }
}

/// The N accumulators' state: the values kept, then the start index, the
/// most kept, the memory used and its limit, and whether it is a `$group`'s.
fn n_state(values: Sized) -> Sized {
    array([values, int64(), int64(), int32(), int32(), boolean()])
}

/// The slots one accumulator takes in the accumulator row.
fn acc_slots(spec: &AccSpec, acc: &Acc) -> Sized {
    match acc {
        Acc::Count(_) => int64(),
        // `{$sum: 1}` is a count; every other `$sum` sums.
        Acc::Sum(_) if spec.op == "$sum" && matches!(spec.arg, Bson::Int32(1)) => int64(),
        Acc::Sum(n) => sum_state(Some(n)),
        // `$avg` is a sum and a count, in two slots.
        Acc::Avg(state) => sum_state(state.as_ref().map(|(n, _)| n)) + int64(),
        Acc::Min(v) | Acc::Max(v) => maybe(v.as_ref()),
        Acc::First(v) | Acc::Last(v) => maybe(v.as_ref().and_then(|v| v.as_ref())),
        Acc::Push(c) | Acc::ConcatArrays(c) => array([array(c.items.iter().map(value)), int64()]),
        Acc::AddToSet(c) | Acc::SetUnion(c) => {
            array([array_set(c.items.iter().map(value)), int64()])
        }
        Acc::MergeObjects(d) => object(d.iter().map(|(k, v)| (k.as_str(), v))),
        // The count, the mean and the sum of squared differences.
        Acc::StdDev { .. } => array([int64(), int64(), int64()]),
        Acc::NElem { kind, n, vals, .. } => {
            let n = n.unwrap_or(vals.len());
            let kept: Vec<Bson> = match kind {
                NElemKind::First => vals.iter().take(n).cloned().collect(),
                NElemKind::Last => vals[vals.len() - n.min(vals.len())..].to_vec(),
                NElemKind::Max | NElemKind::Min => crate::group::sbe_min_max_n(
                    vals.iter()
                        .filter(|v| !matches!(v, Bson::Null | Bson::Undefined))
                        .cloned()
                        .collect(),
                    n,
                    matches!(kind, NElemKind::Max),
                ),
            };
            n_state(array(kept.iter().map(value)))
        }
        Acc::TopN {
            kind,
            n,
            dirs,
            items,
            ..
        } => {
            let n = match kind {
                TopNKind::Top | TopNKind::Bottom => 1,
                TopNKind::TopN | TopNKind::BottomN => n.unwrap_or(items.len()),
            };
            let top = matches!(kind, TopNKind::Top | TopNKind::TopN);
            let kept = crate::group::sbe_top_bottom_n(items.clone(), n, top, dirs);
            // Each kept document is its sort key (an array of them for a
            // compound `sortBy`) and its output.
            n_state(array(kept.iter().map(|(key, out)| {
                let key = match key.as_slice() {
                    [one] => value(one),
                    many => array(many.iter().map(value)),
                };
                array([key, value(out)])
            })))
        }
        // Neither runs in SBE.
        Acc::Percentile { .. } | Acc::Script => nothing(),
    }
}

/// The key row's values: one slot per field of an `_id` document of
/// expressions, else the one key.
fn key_slots<'a>(id_expr: &Bson, key: &'a Bson) -> Vec<Option<&'a Bson>> {
    match (crate::group::id_fields(id_expr), key) {
        (Some(fields), Bson::Document(d)) => fields.iter().map(|(name, _)| d.get(*name)).collect(),
        _ => vec![Some(key)],
    }
}

/// `memUsageForSorter` of a group's key row and accumulator row: what
/// mongod's memory check takes for one group.
pub(crate) fn row_size(id_expr: &Bson, key: &Bson, specs: &[AccSpec], accs: &[Acc]) -> usize {
    let keys: usize = key_slots(id_expr, key)
        .into_iter()
        .map(|k| maybe(k).mem)
        .sum();
    let vals: usize = specs
        .iter()
        .zip(accs)
        .map(|(s, a)| acc_slots(s, a).mem)
        .sum();
    ROW + keys + ROW + vals
}

/// The bytes a group's spilled record counts (`spillRowToDisk`): the
/// accumulator row's count and values, then the key string's type bits.
pub(crate) fn spilled_bytes(id_expr: &Bson, key: &Bson, specs: &[AccSpec], accs: &[Acc]) -> usize {
    let vals: usize = specs
        .iter()
        .zip(accs)
        .map(|(s, a)| acc_slots(s, a).disk)
        .sum();
    let mut bits = TypeBits::default();
    for k in key_slots(id_expr, key).into_iter().flatten() {
        bits.value(k);
    }
    // The record id ends with a long, the spill's running counter.
    bits.push2(0b01);
    8 + vals + bits.size()
}

/// `key_string::TypeBits`: the bits that tell a number's type, and a
/// string from a symbol, which the key string does not order by.
#[derive(Default)]
struct TypeBits {
    bits: Vec<bool>,
}

impl TypeBits {
    fn push(&mut self, bit: bool) {
        self.bits.push(bit);
    }

    fn push2(&mut self, v: u8) {
        self.push(v & 0b10 != 0);
        self.push(v & 0b01 != 0);
    }

    fn push_n(&mut self, v: u32, n: u32) {
        for pos in (0..n).rev() {
            self.push((v >> pos) & 1 != 0);
        }
    }

    fn value(&mut self, v: &Bson) {
        match v {
            Bson::Int32(_) => self.push2(0b00),
            Bson::Int64(_) => self.push2(0b01),
            // A negative zero is V1's five-bit `kV1NegativeDoubleZero`.
            Bson::Double(d) if *d == 0.0 && d.is_sign_negative() => self.push_n(0b11000, 5),
            Bson::Double(_) => self.push2(0b10),
            Bson::Decimal128(d) => self.decimal(d),
            Bson::String(_) => self.push(false),
            Bson::Symbol(_) => self.push(true),
            Bson::Document(d) => d.values().for_each(|v| self.value(v)),
            Bson::Array(a) => a.iter().for_each(|v| self.value(v)),
            Bson::JavaScriptCodeWithScope(c) => {
                // The code is written as a string, then the scope.
                self.push(false);
                c.scope.values().for_each(|v| self.value(v));
            }
            _ => {}
        }
    }

    /// `_appendNumberDecimal`: a zero is `appendDecimalZero` of its biased
    /// exponent (plus 6144 when negative); a NaN or infinity two bits; any
    /// other the two bits and the exponent's low six.
    fn decimal(&mut self, d: &bson::Decimal128) {
        let bytes = d.bytes();
        let hi = u64::from_le_bytes(bytes[8..16].try_into().expect("16 bytes"));
        let lo = u64::from_le_bytes(bytes[0..8].try_into().expect("16 bytes"));
        let negative = hi >> 63 == 1;
        let combination = (hi >> 58) & 0x1f;
        if combination >= 0x1e {
            // Infinity (0b11110) or NaN (0b11111).
            self.push2(0b11);
            return;
        }
        let (exponent, coefficient_hi) = if (hi >> 61) & 0b11 == 0b11 {
            // The large-coefficient form, never canonical: a zero.
            (((hi >> 47) & 0x3fff) as u32, 0)
        } else {
            (((hi >> 49) & 0x3fff) as u32, hi & ((1 << 49) - 1))
        };
        if coefficient_hi == 0 && lo == 0 {
            let which = exponent + if negative { 6144 } else { 0 };
            self.push_n((which >> 12) + 0x1a, 5);
            self.push_n(which & 0xfff, 12);
            return;
        }
        self.push2(0b11);
        self.push_n(exponent & 0x3f, 6);
    }

    /// `TypeBits::getSize`: one byte when every bit is zero, or when they fit
    /// in one byte that leaves its high bit clear; else a size byte (five for
    /// more than 127 bytes) and the bytes.
    fn size(&self) -> usize {
        if !self.bits.iter().any(|b| *b) {
            return 1;
        }
        let len = self.bits.len().div_ceil(8);
        let high_bit = self.bits.get(7).copied().unwrap_or(false);
        if len == 1 && !high_bit {
            1
        } else if len > 127 {
            len + 5
        } else {
            len + 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::{doc, Decimal128};
    use std::str::FromStr;

    fn spec<'a>(op: &'a str, arg: &'a Bson) -> AccSpec<'a> {
        AccSpec { op, arg }
    }

    /// Records and rows measured on mongod 8.2.11 (2026-11-18).
    #[test]
    fn sizes_are_mongods() {
        let id = Bson::String("$n".into());
        let key = Bson::Int32(0);
        assert_eq!(row_size(&id, &key, &[], &[]), 41);
        assert_eq!(spilled_bytes(&id, &key, &[], &[]), 9);

        let arg = Bson::String("$v".into());
        let s = [spec("$push", &arg)];
        let mut acc = crate::group::new_acc("$push").unwrap();
        if let Acc::Push(c) = &mut acc {
            c.items.push(Bson::Int32(1));
        }
        assert_eq!(row_size(&id, &key, &s, std::slice::from_ref(&acc)), 125);
        assert_eq!(spilled_bytes(&id, &key, &s, std::slice::from_ref(&acc)), 41);

        let one = Bson::Int32(1);
        let s = [spec("$sum", &one)];
        let acc = crate::group::new_acc("$sum").unwrap();
        assert_eq!(row_size(&id, &key, &s, std::slice::from_ref(&acc)), 50);
        assert_eq!(spilled_bytes(&id, &key, &s, std::slice::from_ref(&acc)), 18);
    }

    #[test]
    fn type_bits_are_mongods() {
        let id = Bson::String("$n".into());
        let size = |k: Bson| spilled_bytes(&id, &k, &[], &[]) - 8;
        assert_eq!(size(Bson::Int32(-1)), 1);
        assert_eq!(size(Bson::Double(-0.0)), 1);
        assert_eq!(size(Bson::Document(doc! {"a": 1, "b": 2.5, "c": "s"})), 1);
        assert_eq!(
            size(Bson::Document(
                doc! {"a": 1.5, "b": 2.5, "c": 3.5, "d": 4.5}
            )),
            3
        );
        assert_eq!(
            size(Bson::Decimal128(Decimal128::from_str("1.5").unwrap())),
            3
        );
    }

    /// 300 groups of a 1000-byte `$push` under a 50000-byte budget: mongod
    /// 8.2.11 spills 7 times.
    #[test]
    fn checkpoints_are_mongods() {
        let mut check = MemoryCheck::default();
        let (mut groups, mut spills) = (0, 0);
        for _ in 0..300 {
            groups += 1;
            if check.should_spill(groups, || 1130, 50_000) {
                spills += 1;
                groups = 0;
            }
        }
        assert_eq!((spills + usize::from(groups > 0)), 7);
    }
}
