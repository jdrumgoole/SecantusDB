//! `mongod`'s BSONColumn encoder: the compressed column format (BSON binary
//! subtype 7) a time-series bucket keeps each data field in.
//!
//! A transcription of `mongod` 8.2.11's `BSONColumnBuilder`
//! (`src/mongo/bson/column/bsoncolumnbuilder.cpp`), its `Simple8bBuilder`
//! (`simple8b_builder.h`, `simple8b_helpers.h`) and `Simple8bTypeUtil`
//! (`simple8b_type_util.cpp`), so a column holds the bytes `mongod` writes,
//! not merely bytes that decode to the same values. The order of every
//! choice is `mongod`'s: when a run of deltas ends, when a double is
//! rescaled, when an object stream is interleaved and in which order its
//! control blocks are written. `mongod` stores what `finalize` would give
//! after every insert (`intermediate` saves and restores the builder around
//! a finalize), so one builder fed every measurement of a bucket gives the
//! bucket's bytes.
//!
//! Elements go in as their type byte and value bytes (the element without
//! its field name).

/// One element: its BSON type byte and its value bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Element<'a> {
    pub ty: u8,
    pub value: &'a [u8],
}

const EOO: u8 = 0x00;
const DOUBLE: u8 = 0x01;
const STRING: u8 = 0x02;
const OBJECT: u8 = 0x03;
const ARRAY: u8 = 0x04;
const BINARY: u8 = 0x05;
const UNDEFINED: u8 = 0x06;
const OID: u8 = 0x07;
const BOOLEAN: u8 = 0x08;
const DATE: u8 = 0x09;
const NULL: u8 = 0x0A;
const CODE: u8 = 0x0D;
const INT32: u8 = 0x10;
const TIMESTAMP: u8 = 0x11;
const INT64: u8 = 0x12;
const DECIMAL: u8 = 0x13;
const MIN_KEY: u8 = 0xFF;
const MAX_KEY: u8 = 0x7F;

/// An empty BSON document.
const EMPTY_DOC: [u8; 5] = [5, 0, 0, 0, 0];

fn uses_delta_of_delta(ty: u8) -> bool {
    matches!(ty, OID | DATE | TIMESTAMP)
}

fn uses_128bit(ty: u8) -> bool {
    matches!(ty, DECIMAL | BINARY | STRING | CODE)
}

fn is_obj(ty: u8) -> bool {
    ty == OBJECT || ty == ARRAY
}

// ---------------------------------------------------------------------------
// Simple-8b tables (`simple8b_helpers.h`).

const BASE_SELECTOR: u8 = 0;
const SEVEN_SELECTOR: u8 = 1;
const EIGHT_SELECTOR_SMALL: u8 = 2;
const EIGHT_SELECTOR_LARGE: u8 = 3;
const RLE_SELECTOR: u64 = 15;
const MAX_RLE_COUNT: u32 = 16;
const RLE_MULTIPLIER: u32 = 120;
const SELECTOR_BITS: u8 = 4;
const NIBBLE_SHIFT_SIZE: u8 = 4;

const MAX_SELECTOR: [u8; 4] = [14, 9, 7, 13];
const MIN_SELECTOR: [u8; 4] = [1, 1, 1, 8];
const DATA_BITS: [u8; 4] = [60, 56, 56, 56];
const TRAILING_ZERO_BIT_SIZE: [u8; 4] = [0, 4, 4, 5];
const TRAILING_ZEROS_MAX_COUNT: [u8; 4] = [0, 15, 60, 124];
const BASE_SELECTOR_TO_SHIFT_SIZE: [u8; 15] = [0, 0, 0, 0, 0, 0, 0, 4, 4, 0, 0, 0, 0, 0, 0];
const INVALID_SELECTOR: u8 = 0xff;
const SELECTOR_TO_EXTENSION: [[u8; 16]; 2] = [
    [
        0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ],
    [0, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 0xff, 0xff],
];
const EXTENSION_TO_BASE_SELECTOR: [[u8; 16]; 4] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [7; 16],
    [8; 16],
    [8; 16],
];

const fn mask(bits: u32) -> u64 {
    (1u64 << bits) - 1
}

const DECODE_MASK: [[u64; 16]; 4] = [
    [
        0,
        1,
        mask(2),
        mask(3),
        mask(4),
        mask(5),
        mask(6),
        mask(7),
        mask(8),
        mask(10),
        mask(12),
        mask(15),
        mask(20),
        mask(30),
        mask(60),
        1,
    ],
    [
        0,
        mask(6),
        mask(7),
        mask(8),
        mask(9),
        mask(11),
        mask(14),
        mask(18),
        mask(28),
        mask(56),
        0,
        0,
        0,
        0,
        0,
        0,
    ],
    [
        0,
        mask(8),
        mask(9),
        mask(11),
        mask(14),
        mask(18),
        mask(28),
        mask(56),
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ],
    [
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        mask(9),
        mask(11),
        mask(14),
        mask(18),
        mask(28),
        mask(56),
        0,
        0,
    ],
];
const BITS_PER_INT_FOR_SELECTOR: [[u8; 16]; 4] = [
    [64, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 15, 20, 30, 60, 64],
    [64, 2, 3, 4, 5, 7, 10, 14, 24, 52, 64, 64, 64, 64, 64, 64],
    [64, 4, 5, 7, 10, 14, 24, 52, 0, 0, 64, 64, 64, 64, 64, 64],
    [64, 0, 0, 0, 0, 0, 0, 0, 4, 6, 9, 13, 23, 51, 64, 64],
];
const INTS_STORE_FOR_SELECTOR: [[u8; 16]; 4] = [
    [0, 60, 30, 20, 15, 12, 10, 8, 7, 6, 5, 4, 3, 2, 1, 0],
    [0, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 0, 0, 0, 0],
    [0, 7, 6, 5, 4, 3, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 6, 5, 4, 3, 2, 1, 0, 0],
];
const MIN_DATA_BITS: [u8; 4] = [1, 2, 4, 4];

fn selector_index(ints_needed: u8, ext: u8) -> u8 {
    let row = &INTS_STORE_FOR_SELECTOR[ext as usize];
    let lo = MIN_SELECTOR[ext as usize];
    let hi = MAX_SELECTOR[ext as usize];
    (lo..hi)
        .find(|&i| ints_needed >= row[i as usize])
        .unwrap_or(hi)
}

/// How many values a Simple-8b block holds (`simple8b_internal::blockCount`).
fn block_count(encoded: u64) -> u32 {
    let mut selector = (encoded & 0xF) as u8;
    let ext = ((encoded >> SELECTOR_BITS) & 0xF) as u8;
    if u64::from(selector) == RLE_SELECTOR {
        return (u32::from(ext) + 1) * RLE_MULTIPLIER;
    }
    let mut ext_type = BASE_SELECTOR;
    if selector == 7 || selector == 8 {
        ext_type = SELECTOR_TO_EXTENSION[(selector - 7) as usize][ext as usize];
        if ext_type == INVALID_SELECTOR {
            return 0;
        }
        if ext_type != BASE_SELECTOR {
            selector = ext;
        }
    }
    u32::from(INTS_STORE_FOR_SELECTOR[ext_type as usize][selector as usize])
}

// ---------------------------------------------------------------------------
// Simple-8b builder (`simple8b_builder.h`). One type serves both of
// `mongod`'s instantiations: `wide` is `Simple8bBuilder<uint128_t>`, the rest
// `Simple8bBuilder<uint64_t>`, whose bit counts differ at the edges.

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pending {
    val: Option<u128>,
    bit_count: [u8; 4],
    trailing_zeros: [u8; 4],
}

#[derive(Debug, Clone)]
struct Simple8b {
    wide: bool,
    rle_count: u32,
    last_value_in_prev_word: Option<u128>,
    curr_max_bit_len: [u8; 4],
    last_valid_extension_type: u8,
    selector_possible: [bool; 4],
    pending: Vec<Pending>,
}

type BlockWriter<'w> = dyn FnMut(u64, &Simple8b) + 'w;

fn bits_without_leading_zeros_64(v: u64) -> u8 {
    (64 - v.wrapping_add(1).leading_zeros()) as u8
}

impl Simple8b {
    fn new(wide: bool) -> Self {
        Simple8b {
            wide,
            rle_count: 0,
            last_value_in_prev_word: Some(0),
            curr_max_bit_len: MIN_DATA_BITS,
            last_valid_extension_type: 0,
            selector_possible: [true; 4],
            pending: Vec::new(),
        }
    }

    /// The values still pending, in order: the RLE repeats first.
    fn values(&self) -> Vec<Option<u128>> {
        let mut out = vec![self.last_value_in_prev_word; self.rle_count as usize];
        out.extend(self.pending.iter().map(|p| p.val));
        out
    }

    fn count_bits(&self, v: u128) -> u8 {
        if !self.wide {
            return bits_without_leading_zeros_64(v as u64);
        }
        let high = (v >> 64) as u64;
        if high == 0 {
            let low = v as u64;
            if low == u64::MAX {
                return 65;
            }
            return bits_without_leading_zeros_64(low);
        }
        (128 - high.leading_zeros()) as u8
    }

    fn trailing_zeros(&self, v: u128) -> u8 {
        let low = v as u64;
        let high = (v >> 64) as u64;
        if self.wide && low == 0 && high != 0 {
            return (high.trailing_zeros() + 64) as u8;
        }
        if low == 0 {
            0
        } else {
            low.trailing_zeros() as u8
        }
    }

    fn calculate_pending(&self, value: u128) -> Option<Pending> {
        let max = if self.wide {
            u128::MAX
        } else {
            u128::from(u64::MAX)
        };
        if value == max {
            return None;
        }
        let tz = self.trailing_zeros(value);
        let bits = self.count_bits(value);
        let tz7 = tz.min(TRAILING_ZEROS_MAX_COUNT[1]);
        let mut m7 = bits - tz7;
        let tz8s = (tz.min(TRAILING_ZEROS_MAX_COUNT[2]) / NIBBLE_SHIFT_SIZE) * NIBBLE_SHIFT_SIZE;
        let mut m8s = bits - tz8s;
        let tz8l = (tz.min(TRAILING_ZEROS_MAX_COUNT[3]) / NIBBLE_SHIFT_SIZE) * NIBBLE_SHIFT_SIZE;
        let mut m8l = bits - tz8l;
        if tz == TRAILING_ZEROS_MAX_COUNT[1] {
            m7 = self.count_bits(value >> tz);
        } else if tz == TRAILING_ZEROS_MAX_COUNT[2] {
            m8s = self.count_bits(value >> tz);
        }
        if self.wide && tz == TRAILING_ZEROS_MAX_COUNT[3] {
            m8l = self.count_bits(value >> tz);
        }
        if bits > DATA_BITS[0]
            && m7 + TRAILING_ZERO_BIT_SIZE[1] > DATA_BITS[1]
            && m8s + TRAILING_ZERO_BIT_SIZE[2] > DATA_BITS[2]
            && m8l + TRAILING_ZERO_BIT_SIZE[3] > DATA_BITS[3]
        {
            return None;
        }
        Some(Pending {
            val: Some(value),
            bit_count: [bits, m7, m8s, m8l],
            trailing_zeros: [0, tz7, tz8s, tz8l],
        })
    }

    fn rle_possible(&self) -> bool {
        self.pending.is_empty() || self.rle_count != 0
    }

    fn initialize_rle_from(&mut self, other: &Simple8b) {
        if other.rle_possible() {
            self.last_value_in_prev_word = other.last_value_in_prev_word;
        }
    }

    fn append(&mut self, value: u128, w: &mut BlockWriter<'_>) -> bool {
        if self.rle_possible() {
            if self.last_value_in_prev_word == Some(value) {
                self.rle_count += 1;
                self.last_valid_extension_type = 0;
                self.selector_possible = [true; 4];
                return true;
            }
            self.handle_rle_termination(w);
        }
        self.append_value(value, true, w)
    }

    fn skip(&mut self, w: &mut BlockWriter<'_>) {
        if self.rle_possible() {
            if self.last_value_in_prev_word.is_none() {
                self.rle_count += 1;
                self.last_valid_extension_type = 0;
                self.selector_possible = [true; 4];
                return;
            }
            self.handle_rle_termination(w);
        }
        self.append_skip(true, w);
    }

    fn flush(&mut self, w: &mut BlockWriter<'_>) {
        self.handle_rle_termination(w);
        if !self.pending.is_empty() {
            while !self.pending.is_empty() {
                let word = self.encode_largest_possible_word(self.last_valid_extension_type);
                w(word, self);
            }
            self.rle_count = 0;
        }
    }

    fn append_value(&mut self, value: u128, try_rle: bool, w: &mut BlockWriter<'_>) -> bool {
        let Some(pv) = self.calculate_pending(value) else {
            return false;
        };
        if self.fits_in_current_word(&pv) {
            self.pending.push(pv);
            self.update_state(&pv);
        } else {
            let last = self.pending.last().copied();
            loop {
                let word = self.encode_largest_possible_word(self.last_valid_extension_type);
                w(word, self);
                if self.fits_in_current_word(&pv) {
                    break;
                }
            }
            if try_rle && self.pending.is_empty() && last.map(|p| p.val) == Some(Some(value)) {
                self.rle_count = 1;
                self.last_value_in_prev_word = Some(value);
                self.last_valid_extension_type = 0;
                self.selector_possible = [true; 4];
            } else {
                self.pending.push(pv);
                self.update_state(&pv);
            }
        }
        true
    }

    fn append_skip(&mut self, try_rle: bool, w: &mut BlockWriter<'_>) {
        if let Some(last) = self.pending.last().copied() {
            let last_is_skip = last.val.is_none();
            let probe = Pending {
                val: None,
                bit_count: MIN_DATA_BITS,
                trailing_zeros: [0; 4],
            };
            if !self.fits_in_current_word(&probe) {
                let word = self.encode_largest_possible_word(self.last_valid_extension_type);
                w(word, self);
                self.last_valid_extension_type = BASE_SELECTOR;
            }
            if self.pending.is_empty() && last_is_skip && try_rle {
                self.rle_count = 1;
                self.last_value_in_prev_word = None;
                return;
            }
        }
        self.pending.push(Pending {
            val: None,
            bit_count: [0; 4],
            trailing_zeros: [0; 4],
        });
    }

    fn handle_rle_termination(&mut self, w: &mut BlockWriter<'_>) {
        self.append_rle_encoding(w);
        while self.rle_count > 0 {
            match self.last_value_in_prev_word {
                None => self.append_skip(false, w),
                Some(v) => {
                    self.append_value(v, false, w);
                }
            }
            self.rle_count -= 1;
        }
        self.last_value_in_prev_word = Some(0);
        if self.pending.is_empty() {
            self.last_valid_extension_type = 0;
            self.selector_possible = [true; 4];
        }
    }

    fn append_rle_encoding(&mut self, w: &mut BlockWriter<'_>) {
        let mut create = |this: &mut Simple8b, count: u32| {
            let word = RLE_SELECTOR | (u64::from(count - 1) << SELECTOR_BITS);
            this.rle_count -= RLE_MULTIPLIER * count;
            w(word, this);
        };
        let mut count = self.rle_count / RLE_MULTIPLIER;
        if count >= 1 {
            while count > MAX_RLE_COUNT {
                create(self, MAX_RLE_COUNT);
                count -= MAX_RLE_COUNT;
            }
            create(self, count);
        }
    }

    fn fits_in_current_word(&mut self, value: &Pending) -> bool {
        for i in 0..4u8 {
            if self.selector_possible[i as usize] && self.fits_with_selector_type(value, i) {
                return true;
            }
        }
        false
    }

    fn fits_with_selector_type(&mut self, value: &Pending, ext: u8) -> bool {
        let e = ext as usize;
        let bits = u64::from(self.curr_max_bit_len[e].max(value.bit_count[e]))
            + u64::from(TRAILING_ZERO_BIT_SIZE[e]);
        let needed = bits * (self.pending.len() as u64 + 1);
        if u64::from(DATA_BITS[e]) < needed {
            self.selector_possible[e] = false;
            return false;
        }
        self.last_valid_extension_type = ext;
        true
    }

    fn encode_largest_possible_word(&mut self, ext: u8) -> u64 {
        let selector = selector_index(self.pending.len() as u8, ext);
        let coded = INTS_STORE_FOR_SELECTOR[ext as usize][selector as usize] as usize;
        let word = self.encode(selector, ext);
        self.pending.drain(..coded.min(self.pending.len()));
        self.curr_max_bit_len = MIN_DATA_BITS;
        let rest = std::mem::take(&mut self.pending);
        for p in &rest {
            self.update_state(p);
        }
        self.pending = rest;
        self.selector_possible = [true; 4];
        word
    }

    fn encode(&self, selector: u8, ext: u8) -> u64 {
        let (e, s) = (ext as usize, selector as usize);
        let base = EXTENSION_TO_BASE_SELECTOR[e][s];
        let shift_extension = BASE_SELECTOR_TO_SHIFT_SIZE[base as usize];
        let mut word = u64::from(base);
        let bits_per_int = BITS_PER_INT_FOR_SELECTOR[e][s];
        let coded = INTS_STORE_FOR_SELECTOR[e][s];
        let unshifted_mask = DECODE_MASK[e][s];
        let tz_bits = TRAILING_ZERO_BIT_SIZE[e];
        for i in 0..coded {
            let shift = u32::from(bits_per_int + tz_bits) * u32::from(i)
                + u32::from(SELECTOR_BITS + shift_extension);
            let Some(p) = self.pending.get(i as usize) else {
                break;
            };
            let cur = match p.val {
                None => unshifted_mask,
                Some(v) => match ext {
                    SEVEN_SELECTOR => {
                        let tz = p.trailing_zeros[e];
                        u64::from(tz) | ((v >> tz) as u64).wrapping_shl(u32::from(tz_bits))
                    }
                    EIGHT_SELECTOR_SMALL | EIGHT_SELECTOR_LARGE => {
                        let tz = p.trailing_zeros[e] / NIBBLE_SHIFT_SIZE;
                        u64::from(tz)
                            | ((v >> (tz * NIBBLE_SHIFT_SIZE)) as u64)
                                .wrapping_shl(u32::from(tz_bits))
                    }
                    _ => v as u64,
                },
            };
            word |= cur.wrapping_shl(shift);
        }
        if ext != BASE_SELECTOR {
            word |= u64::from(selector) << SELECTOR_BITS;
        }
        word
    }

    fn update_state(&mut self, p: &Pending) {
        for i in 0..4 {
            self.curr_max_bit_len[i] = self.curr_max_bit_len[i].max(p.bit_count[i]);
        }
    }
}

// ---------------------------------------------------------------------------
// Value encodings (`simple8b_type_util.cpp`).

const MEMORY_AS_INTEGER: u8 = 5;
const SCALE_MULTIPLIER: [f64; 5] = [1.0, 10.0, 100.0, 10000.0, 100000000.0];
const CONTROL_FOR_SCALE_INDEX: [u8; 6] = [0x90, 0xA0, 0xB0, 0xC0, 0xD0, 0x80];
const SAFE_LONG_AS_DOUBLE: f64 = 9007199254740992.0; // 2^53

fn encode_int64(v: i64) -> u64 {
    ((v as u64) << 1) ^ ((v >> 63) as u64)
}

fn decode_int64(v: u64) -> i64 {
    ((v >> 1) ^ (!(v & 1)).wrapping_add(1)) as i64
}

fn encode_int128(v: i128) -> u128 {
    ((v as u128) << 1) ^ ((v >> 127) as u128)
}

fn encode_object_id(oid: &[u8]) -> i64 {
    let b = [oid[11], oid[3], oid[10], oid[2], oid[9], oid[1], oid[0], 0];
    i64::from_le_bytes(b)
}

fn encode_double(val: f64, scale_index: u8) -> Option<i64> {
    if scale_index == MEMORY_AS_INTEGER {
        return Some(val.to_bits() as i64);
    }
    let m = SCALE_MULTIPLIER[scale_index as usize];
    let scaled = val * m;
    if !(-SAFE_LONG_AS_DOUBLE..=SAFE_LONG_AS_DOUBLE).contains(&scaled) {
        return None;
    }
    let encoded = scaled.round() as i64;
    if (encoded as f64 / m).to_bits() != val.to_bits() {
        return None;
    }
    Some(encoded)
}

fn decode_double(v: i64, scale_index: u8) -> f64 {
    if scale_index == MEMORY_AS_INTEGER {
        return f64::from_bits(v as u64);
    }
    v as f64 / SCALE_MULTIPLIER[scale_index as usize]
}

/// The double at the lowest scale that holds it exactly.
fn scale_and_encode_double(value: f64, min_scale_index: u8) -> (i64, u8) {
    let mut i = min_scale_index;
    loop {
        if let Some(e) = encode_double(value, i) {
            return (e, i);
        }
        i += 1;
    }
}

fn encode_string(s: &[u8]) -> Option<i128> {
    if s.len() > 16 || s.first() == Some(&0) {
        return None;
    }
    let mut arr = [0u8; 16];
    for (i, b) in s.iter().rev().enumerate() {
        arr[i] = *b;
    }
    Some(i128::from_le_bytes(arr))
}

fn encode_binary(data: &[u8]) -> Option<i128> {
    if data.len() > 16 {
        return None;
    }
    let mut arr = [0u8; 16];
    arr[..data.len()].copy_from_slice(data);
    Some(i128::from_le_bytes(arr))
}

fn read_i32(v: &[u8]) -> i32 {
    i32::from_le_bytes([v[0], v[1], v[2], v[3]])
}

fn read_i64(v: &[u8]) -> i64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&v[..8]);
    i64::from_le_bytes(b)
}

fn read_f64(v: &[u8]) -> f64 {
    f64::from_bits(read_i64(v) as u64)
}

/// A string or code value's characters (without length or terminator).
fn string_bytes(v: &[u8]) -> &[u8] {
    let len = read_i32(v).max(1) as usize;
    &v[4..4 + len - 1]
}

/// A binary value's subtype and bytes.
fn binary_parts(v: &[u8]) -> (usize, u8, &[u8]) {
    let len = read_i32(v).max(0) as usize;
    (len, v[4], &v[5..5 + len])
}

// ---------------------------------------------------------------------------
// The control bytes and the per-stream encoding state
// (`EncodingState` in `bsoncolumnbuilder.cpp`).

/// Where a stream's finished control blocks are reported: the interleaved
/// mode collects them to merge the streams, the regular mode ignores them.
type ControlBlockWriter<'w> = dyn FnMut(usize, usize) + 'w;

fn increment_simple8b_count(
    buf: &mut Vec<u8>,
    control_offset: &mut Option<usize>,
    scale_index: u8,
    cbw: &mut ControlBlockWriter<'_>,
) -> Option<usize> {
    let control = CONTROL_FOR_SCALE_INDEX[scale_index as usize];
    let (pos, count) = match *control_offset {
        None => {
            buf.push(0);
            let pos = buf.len() - 1;
            *control_offset = Some(pos);
            (pos, 0u8)
        }
        Some(pos) => {
            if buf[pos] & 0xF0 != control {
                cbw(pos, buf.len() - pos);
                *control_offset = None;
                increment_simple8b_count(buf, control_offset, scale_index, cbw);
                return None;
            }
            (pos, (buf[pos] & 0x0F) + 1)
        }
    };
    buf[pos] = control | (count & 0x0F);
    if count + 1 == 16 {
        *control_offset = None;
        return Some(pos);
    }
    None
}

/// `Simple8bBlockWriter64`: writes a block and, for doubles, remembers the
/// last value the block holds.
#[allow(clippy::too_many_arguments)]
fn write_block64(
    block: u64,
    view: &Simple8b,
    buf: &mut Vec<u8>,
    control_offset: &mut Option<usize>,
    scale_index: u8,
    ty: u8,
    prev_encoded64: i64,
    last_value_in_prev_block: &mut f64,
    cbw: &mut ControlBlockWriter<'_>,
) {
    let full = increment_simple8b_count(buf, control_offset, scale_index, cbw);
    buf.extend_from_slice(&block.to_le_bytes());
    if let Some(f) = full {
        cbw(f, buf.len() - f);
    }
    if ty != DOUBLE {
        return;
    }
    let mut current = prev_encoded64;
    for v in view.values().iter().rev().flatten() {
        current = current.wrapping_sub(decode_int64(*v as u64));
    }
    *last_value_in_prev_block = decode_double(current, scale_index);
}

fn write_block128(
    block: u64,
    buf: &mut Vec<u8>,
    control_offset: &mut Option<usize>,
    cbw: &mut ControlBlockWriter<'_>,
) {
    let full = increment_simple8b_count(buf, control_offset, MEMORY_AS_INTEGER, cbw);
    buf.extend_from_slice(&block.to_le_bytes());
    if let Some(f) = full {
        cbw(f, buf.len() - f);
    }
}

#[derive(Debug, Clone)]
struct Encoder64 {
    s8: Simple8b,
    prev_delta: i64,
    prev_encoded64: i64,
    last_value_in_prev_block: f64,
    scale_index: u8,
}

/// The output a stream writes to: its buffer, its open control byte and
/// where finished control blocks are reported.
struct Out<'a, 'w> {
    buf: &'a mut Vec<u8>,
    control_offset: &'a mut Option<usize>,
    cbw: &'a mut ControlBlockWriter<'w>,
}

impl Encoder64 {
    fn new() -> Self {
        Encoder64 {
            s8: Simple8b::new(false),
            prev_delta: 0,
            prev_encoded64: 0,
            last_value_in_prev_block: 0.0,
            scale_index: MEMORY_AS_INTEGER,
        }
    }

    fn initialize(&mut self, elem: Element<'_>) {
        match elem.ty {
            DOUBLE => {
                self.last_value_in_prev_block = read_f64(elem.value);
                (self.prev_encoded64, self.scale_index) =
                    scale_and_encode_double(self.last_value_in_prev_block, 0);
            }
            OID => self.prev_encoded64 = encode_object_id(elem.value),
            _ => {}
        }
    }

    fn append(&mut self, ty: u8, value: u64, out: &mut Out<'_, '_>) -> bool {
        let Encoder64 {
            s8,
            scale_index,
            prev_encoded64,
            last_value_in_prev_block,
            ..
        } = self;
        let (scale, prev) = (*scale_index, *prev_encoded64);
        s8.append(u128::from(value), &mut |block, view| {
            write_block64(
                block,
                view,
                out.buf,
                out.control_offset,
                scale,
                ty,
                prev,
                last_value_in_prev_block,
                out.cbw,
            )
        })
    }

    fn skip(&mut self, ty: u8, out: &mut Out<'_, '_>) {
        let Encoder64 {
            s8,
            scale_index,
            prev_encoded64,
            last_value_in_prev_block,
            ..
        } = self;
        let (scale, prev) = (*scale_index, *prev_encoded64);
        s8.skip(&mut |block, view| {
            write_block64(
                block,
                view,
                out.buf,
                out.control_offset,
                scale,
                ty,
                prev,
                last_value_in_prev_block,
                out.cbw,
            )
        })
    }

    fn flush(&mut self, ty: u8, out: &mut Out<'_, '_>) {
        let Encoder64 {
            s8,
            scale_index,
            prev_encoded64,
            last_value_in_prev_block,
            ..
        } = self;
        let (scale, prev) = (*scale_index, *prev_encoded64);
        s8.flush(&mut |block, view| {
            write_block64(
                block,
                view,
                out.buf,
                out.control_offset,
                scale,
                ty,
                prev,
                last_value_in_prev_block,
                out.cbw,
            )
        })
    }

    fn append_delta(
        &mut self,
        elem: Element<'_>,
        previous: Element<'_>,
        out: &mut Out<'_, '_>,
    ) -> bool {
        let mut possible = true;
        let mut value: i64 = 0;
        match elem.ty {
            DOUBLE => {
                return self.append_double(read_f64(elem.value), read_f64(previous.value), out);
            }
            INT32 => {
                value = i64::from(read_i32(elem.value))
                    .wrapping_sub(i64::from(read_i32(previous.value)))
            }
            INT64 | TIMESTAMP | DATE => {
                value = read_i64(elem.value).wrapping_sub(read_i64(previous.value))
            }
            OID => {
                possible = elem.value[4..9] == previous.value[4..9];
                if possible {
                    let cur = encode_object_id(elem.value);
                    value = cur.wrapping_sub(self.prev_encoded64);
                    self.prev_encoded64 = cur;
                }
            }
            BOOLEAN => value = i64::from(elem.value[0] != 0) - i64::from(previous.value[0] != 0),
            UNDEFINED | NULL => value = 0,
            _ => possible = false,
        }
        if uses_delta_of_delta(elem.ty) {
            let current = value;
            value = current.wrapping_sub(self.prev_delta);
            self.prev_delta = current;
        }
        if possible {
            return self.append(elem.ty, encode_int64(value), out);
        }
        false
    }

    /// `_tryRescalePending`: the pending deltas at the larger scale, when
    /// that fits without finishing a block.
    fn try_rescale_pending(&self, encoded: i64, new_scale_index: u8) -> Option<Simple8b> {
        let mut prev = encode_double(self.last_value_in_prev_block, self.scale_index)?;
        let mut prev_rescaled = encode_double(self.last_value_in_prev_block, new_scale_index)?;
        let mut possible = true;
        let mut builder = Simple8b::new(false);
        builder.initialize_rle_from(&self.s8);
        for pending in self.s8.values() {
            let Some(p) = pending else {
                builder.skip(&mut |_, _| possible = false);
                continue;
            };
            prev = prev.wrapping_add(decode_int64(p as u64));
            let rescaled = encode_double(decode_double(prev, self.scale_index), new_scale_index)?;
            let appended = builder.append(
                u128::from(encode_int64(rescaled.wrapping_sub(prev_rescaled))),
                &mut |_, _| possible = false,
            );
            if !appended || !possible {
                return None;
            }
            prev_rescaled = rescaled;
        }
        let appended = builder.append(
            u128::from(encode_int64(encoded.wrapping_sub(prev_rescaled))),
            &mut |_, _| possible = false,
        );
        if !appended || !possible {
            return None;
        }
        Some(builder)
    }

    fn append_double(&mut self, value: f64, previous: f64, out: &mut Out<'_, '_>) -> bool {
        let (mut encoded, mut scale) = scale_and_encode_double(value, self.scale_index);
        if scale != self.scale_index {
            if let Some(rescaled) = self.try_rescale_pending(encoded, scale) {
                self.s8 = rescaled;
                self.prev_encoded64 = encoded;
                self.scale_index = scale;
                return true;
            }
            self.flush(DOUBLE, out);
            if let Some(pos) = *out.control_offset {
                let len = out.buf.len() - pos;
                (out.cbw)(pos, len);
            }
            *out.control_offset = None;
            let prev_scale_index;
            (self.prev_encoded64, prev_scale_index) = scale_and_encode_double(previous, scale);
            if scale != prev_scale_index {
                (encoded, scale) = scale_and_encode_double(value, prev_scale_index);
                (self.prev_encoded64, _) = scale_and_encode_double(previous, scale);
            }
            self.scale_index = scale;
        }
        let before = out.buf.len();
        if !self.append(
            DOUBLE,
            encode_int64(encoded.wrapping_sub(self.prev_encoded64)),
            out,
        ) {
            return false;
        }
        if out.buf.len() == before {
            self.prev_encoded64 = encoded;
            return true;
        }
        // A block was written: start again from the lowest scale and re-add
        // what is still pending, which raises the scale as far as needed.
        let prev_scale = self.scale_index;
        (self.prev_encoded64, self.scale_index) =
            scale_and_encode_double(self.last_value_in_prev_block, 0);
        let mut fresh = Simple8b::new(false);
        fresh.initialize_rle_from(&self.s8);
        let old = std::mem::replace(&mut self.s8, fresh);
        let mut prev = self.last_value_in_prev_block;
        let mut prev_encoded = encode_double(prev, prev_scale).unwrap_or(0);
        for pending in old.values() {
            match pending {
                Some(p) => {
                    prev_encoded = prev_encoded.wrapping_add(decode_int64(p as u64));
                    let val = decode_double(prev_encoded, prev_scale);
                    self.append_double(val, prev, out);
                    prev = val;
                }
                None => self.skip(DOUBLE, out),
            }
        }
        true
    }
}

#[derive(Debug, Clone)]
struct Encoder128 {
    s8: Simple8b,
    prev_encoded128: Option<i128>,
}

impl Encoder128 {
    fn new() -> Self {
        Encoder128 {
            s8: Simple8b::new(true),
            prev_encoded128: None,
        }
    }

    fn initialize(&mut self, elem: Element<'_>) {
        self.prev_encoded128 = match elem.ty {
            STRING | CODE => encode_string(string_bytes(elem.value)),
            BINARY => encode_binary(binary_parts(elem.value).2),
            DECIMAL => Some(i128::from_le_bytes(
                elem.value[..16].try_into().unwrap_or([0; 16]),
            )),
            _ => None,
        };
    }

    fn append(&mut self, value: u128, out: &mut Out<'_, '_>) -> bool {
        self.s8.append(value, &mut |block, _| {
            write_block128(block, out.buf, out.control_offset, out.cbw)
        })
    }

    fn skip(&mut self, out: &mut Out<'_, '_>) {
        self.s8
            .skip(&mut |block, _| write_block128(block, out.buf, out.control_offset, out.cbw))
    }

    fn flush(&mut self, out: &mut Out<'_, '_>) {
        self.s8
            .flush(&mut |block, _| write_block128(block, out.buf, out.control_offset, out.cbw))
    }

    fn append_encoded(&mut self, encoded: i128, out: &mut Out<'_, '_>) -> bool {
        // With no encodable previous a 0 would read as a repeat of it.
        if self.prev_encoded128.is_none() && encoded == 0 {
            return false;
        }
        let delta = encoded.wrapping_sub(self.prev_encoded128.unwrap_or(0));
        let appended = self.append(encode_int128(delta), out);
        self.prev_encoded128 = Some(encoded);
        appended
    }

    fn append_delta(
        &mut self,
        elem: Element<'_>,
        previous: Element<'_>,
        out: &mut Out<'_, '_>,
    ) -> bool {
        match elem.ty {
            STRING | CODE => match encode_string(string_bytes(elem.value)) {
                Some(e) => self.append_encoded(e, out),
                None => false,
            },
            BINARY => {
                let (len, subtype, data) = binary_parts(elem.value);
                let (plen, psubtype, _) = binary_parts(previous.value);
                if len != plen || subtype != psubtype {
                    return false;
                }
                match encode_binary(data) {
                    Some(e) => self.append_encoded(e, out),
                    None => false,
                }
            }
            DECIMAL => match elem.value[..16].try_into() {
                Ok(b) => self.append_encoded(i128::from_le_bytes(b), out),
                Err(_) => false,
            },
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
enum Encoder {
    E64(Encoder64),
    E128(Encoder128),
}

impl Encoder {
    fn append(&mut self, ty: u8, value: u64, out: &mut Out<'_, '_>) -> bool {
        match self {
            Encoder::E64(e) => e.append(ty, value, out),
            Encoder::E128(e) => e.append(u128::from(value), out),
        }
    }

    fn skip(&mut self, ty: u8, out: &mut Out<'_, '_>) {
        match self {
            Encoder::E64(e) => e.skip(ty, out),
            Encoder::E128(e) => e.skip(out),
        }
    }

    fn flush(&mut self, ty: u8, out: &mut Out<'_, '_>) {
        match self {
            Encoder::E64(e) => e.flush(ty, out),
            Encoder::E128(e) => e.flush(out),
        }
    }

    fn append_delta(
        &mut self,
        elem: Element<'_>,
        previous: Element<'_>,
        out: &mut Out<'_, '_>,
    ) -> bool {
        match self {
            Encoder::E64(e) => e.append_delta(elem, previous, out),
            Encoder::E128(e) => e.append_delta(elem, previous, out),
        }
    }
}

/// One stream of values: the whole column in regular mode, one scalar path
/// of the reference object in interleaved mode.
#[derive(Debug, Clone)]
struct EncodingState {
    encoder: Encoder,
    prev_ty: u8,
    prev_value: Vec<u8>,
    control_offset: Option<usize>,
}

impl EncodingState {
    fn new() -> Self {
        EncodingState {
            encoder: Encoder::E64(Encoder64::new()),
            prev_ty: EOO,
            prev_value: Vec::new(),
            control_offset: None,
        }
    }

    fn store_previous(&mut self, elem: Element<'_>) {
        self.prev_ty = elem.ty;
        self.prev_value.clear();
        self.prev_value.extend_from_slice(elem.value);
    }

    fn initialize_from_previous(&mut self) {
        let prev = Element {
            ty: self.prev_ty,
            value: &self.prev_value,
        };
        self.encoder = if uses_128bit(prev.ty) {
            let mut e = Encoder128::new();
            e.initialize(prev);
            Encoder::E128(e)
        } else {
            let mut e = Encoder64::new();
            e.initialize(prev);
            Encoder::E64(e)
        };
    }

    fn write_literal_from_previous(&mut self, buf: &mut Vec<u8>, cbw: &mut ControlBlockWriter<'_>) {
        if let Some(pos) = self.control_offset {
            cbw(pos, buf.len() - pos);
        }
        buf.push(self.prev_ty);
        buf.push(0);
        buf.extend_from_slice(&self.prev_value);
        let size = self.prev_value.len() + 2;
        cbw(buf.len() - size, size);
        self.control_offset = None;
        self.initialize_from_previous();
    }

    fn append(&mut self, elem: Element<'_>, buf: &mut Vec<u8>, cbw: &mut ControlBlockWriter<'_>) {
        if self.prev_ty != elem.ty {
            self.store_previous(elem);
            let mut out = Out {
                buf,
                control_offset: &mut self.control_offset,
                cbw,
            };
            self.encoder.flush(elem.ty, &mut out);
            self.write_literal_from_previous(buf, cbw);
            return;
        }
        let previous_value = std::mem::take(&mut self.prev_value);
        let previous = Element {
            ty: self.prev_ty,
            value: &previous_value,
        };
        let ty = elem.ty;
        let mut out = Out {
            buf,
            control_offset: &mut self.control_offset,
            cbw,
        };
        let same = !uses_delta_of_delta(ty) && elem == previous;
        let mut compressed = same;
        if same {
            self.encoder.append(ty, 0, &mut out);
        }
        if !compressed {
            compressed = self.encoder.append_delta(elem, previous, &mut out);
        }
        self.prev_value = previous_value;
        self.store_previous(elem);
        if !compressed {
            let mut out = Out {
                buf,
                control_offset: &mut self.control_offset,
                cbw,
            };
            self.encoder.flush(ty, &mut out);
            self.write_literal_from_previous(buf, cbw);
        }
    }

    fn skip(&mut self, buf: &mut Vec<u8>, cbw: &mut ControlBlockWriter<'_>) {
        let before = buf.len();
        let ty = self.prev_ty;
        let mut out = Out {
            buf,
            control_offset: &mut self.control_offset,
            cbw,
        };
        self.encoder.skip(ty, &mut out);
        if before != buf.len() && ty == DOUBLE {
            if let Encoder::E64(e) = &mut self.encoder {
                if e.s8.values().iter().all(Option::is_none) {
                    (e.prev_encoded64, e.scale_index) =
                        scale_and_encode_double(e.last_value_in_prev_block, 0);
                }
            }
        }
    }

    fn flush(&mut self, buf: &mut Vec<u8>, cbw: &mut ControlBlockWriter<'_>) {
        let ty = self.prev_ty;
        let mut out = Out {
            buf,
            control_offset: &mut self.control_offset,
            cbw,
        };
        self.encoder.flush(ty, &mut out);
        if let Some(pos) = self.control_offset {
            cbw(pos, buf.len() - pos);
        }
    }
}

// ---------------------------------------------------------------------------
// Raw BSON documents, for the interleaved mode.

#[derive(Debug, Clone, Copy)]
struct RawElem<'a> {
    ty: u8,
    name: &'a [u8],
    value: &'a [u8],
}

fn cstring_len(b: &[u8]) -> usize {
    b.iter().position(|&c| c == 0).map_or(b.len(), |p| p + 1)
}

fn value_size(ty: u8, v: &[u8]) -> usize {
    let int = |o: usize| v.get(o..o + 4).map_or(0, |b| read_i32(b).max(0) as usize);
    match ty {
        DOUBLE | DATE | TIMESTAMP | INT64 => 8,
        STRING | CODE | 0x0E => 4 + int(0),
        OBJECT | ARRAY | 0x0F => int(0),
        BINARY => 5 + int(0),
        OID => 12,
        BOOLEAN => 1,
        0x0B => {
            let a = cstring_len(v);
            a + cstring_len(&v[a.min(v.len())..])
        }
        0x0C => 4 + int(0) + 12,
        INT32 => 4,
        DECIMAL => 16,
        _ => 0,
    }
}

/// The elements of a document's bytes.
fn elements(doc: &[u8]) -> Vec<RawElem<'_>> {
    let mut out = Vec::new();
    let end = (read_i32(doc).max(5) as usize).min(doc.len()) - 1;
    let mut i = 4;
    while i < end && doc[i] != 0 {
        let ty = doc[i];
        let name_len = cstring_len(&doc[i + 1..]);
        let name = &doc[i + 1..i + name_len];
        let start = i + 1 + name_len;
        let size = value_size(ty, &doc[start..]).min(end - start);
        out.push(RawElem {
            ty,
            name,
            value: &doc[start..start + size],
        });
        i = start + size;
    }
    out
}

fn is_empty_doc(v: &[u8]) -> bool {
    read_i32(v) <= 5
}

/// `_traverseUntilEmptyObj`: calls `f` for every scalar; true at an empty
/// object.
fn traverse_until_empty_obj(
    obj: &[u8],
    f: &mut dyn FnMut(RawElem<'_>, Option<RawElem<'_>>),
) -> bool {
    for e in elements(obj) {
        if is_obj(e.ty) {
            if traverse_until_empty_obj(e.value, f) {
                return true;
            }
        } else {
            f(e, None);
        }
    }
    is_empty_doc(obj)
}

fn has_empty_obj(obj: &[u8]) -> bool {
    traverse_until_empty_obj(obj, &mut |_, _| {})
}

fn contains_scalars(obj: &[u8]) -> bool {
    elements(obj)
        .iter()
        .any(|e| !is_obj(e.ty) || contains_scalars(e.value))
}

fn traverse_lock_step_inner(
    reference: &[u8],
    obj: &[u8],
    f: &mut dyn FnMut(RawElem<'_>, Option<RawElem<'_>>),
) -> (usize, usize, bool) {
    let elems = elements(obj);
    let mut it = 0;
    for r in elements(reference) {
        if is_obj(r.ty) {
            let matched = it < elems.len() && r.name == elems[it].name;
            if matched {
                let o = elems[it];
                if o.ty != r.ty || is_empty_doc(r.value) != is_empty_doc(o.value) {
                    return (it, elems.len(), false);
                }
                it += 1;
                let (sub_it, sub_len, ok) = traverse_lock_step_inner(r.value, o.value, f);
                if !ok || sub_it != sub_len {
                    return (it, elems.len(), false);
                }
            } else if traverse_until_empty_obj(r.value, f) {
                return (it, elems.len(), false);
            }
        } else {
            let same = it < elems.len() && r.name == elems[it].name;
            if same && is_obj(elems[it].ty) {
                return (it, elems.len(), false);
            }
            if same {
                f(r, Some(elems[it]));
                it += 1;
            } else {
                f(r, None);
            }
        }
    }
    (it, elems.len(), it == elems.len())
}

/// `traverseLockStep`: whether `obj` fits `reference`, calling `f` with
/// each of the reference's scalars and `obj`'s value there, if any.
fn traverse_lock_step(
    reference: &[u8],
    obj: &[u8],
    f: &mut dyn FnMut(RawElem<'_>, Option<RawElem<'_>>),
) -> bool {
    let (it, len, ok) = traverse_lock_step_inner(reference, obj, f);
    ok && it == len
}

/// A document being built from raw elements.
struct DocBuilder {
    body: Vec<u8>,
    names: Vec<Vec<u8>>,
}

impl DocBuilder {
    fn new() -> Self {
        DocBuilder {
            body: Vec::new(),
            names: Vec::new(),
        }
    }

    fn has_field(&self, name: &[u8]) -> bool {
        self.names.iter().any(|n| n == name)
    }

    fn append(&mut self, ty: u8, name: &[u8], value: &[u8]) {
        self.body.push(ty);
        self.body.extend_from_slice(name);
        self.body.push(0);
        self.body.extend_from_slice(value);
        self.names.push(name.to_vec());
    }

    fn done(self) -> Vec<u8> {
        let len = (self.body.len() + 5) as i32;
        let mut out = len.to_le_bytes().to_vec();
        out.extend_from_slice(&self.body);
        out.push(0);
        out
    }
}

fn merge_inner(builder: &mut DocBuilder, reference: &[u8], obj: &[u8]) -> bool {
    let refs = elements(reference);
    let its = elements(obj);
    let (mut ri, mut oi) = (0, 0);
    while ri < refs.len() && oi < its.len() {
        let (r, o) = (refs[ri], its[oi]);
        if r.name == o.name {
            let (ro, oo) = (is_obj(r.ty), is_obj(o.ty));
            if ro && oo && r.ty == o.ty {
                if is_empty_doc(r.value) != is_empty_doc(o.value) || builder.has_field(r.name) {
                    return false;
                }
                let mut sub = DocBuilder::new();
                if !merge_inner(&mut sub, r.value, o.value) {
                    return false;
                }
                builder.append(r.ty, r.name, &sub.done());
            } else if ro || oo {
                return false;
            } else {
                if builder.has_field(r.name) {
                    return false;
                }
                builder.append(r.ty, r.name, r.value);
            }
            ri += 1;
            oi += 1;
            continue;
        }
        if !its[oi + 1..].iter().any(|e| e.name == r.name) {
            if (is_obj(r.ty) && has_empty_obj(r.value)) || builder.has_field(r.name) {
                return false;
            }
            builder.append(r.ty, r.name, r.value);
            ri += 1;
        } else {
            if (is_obj(o.ty) && has_empty_obj(o.value)) || builder.has_field(o.name) {
                return false;
            }
            builder.append(o.ty, o.name, o.value);
            oi += 1;
        }
    }
    for e in refs[ri..].iter().chain(its[oi..].iter()) {
        if (is_obj(e.ty) && has_empty_obj(e.value)) || builder.has_field(e.name) {
            return false;
        }
        builder.append(e.ty, e.name, e.value);
    }
    true
}

/// `mergeObj`: `reference` with `obj`'s new fields merged in, when the
/// fields both have are in the same order.
fn merge_obj(reference: &[u8], obj: &[u8]) -> Option<Vec<u8>> {
    let mut builder = DocBuilder::new();
    merge_inner(&mut builder, reference, obj).then(|| builder.done())
}

/// How many values a control block holds.
fn num_elems_for_control_byte(control: &[u8]) -> u32 {
    let c = control[0];
    if c & 0xE0 == 0 || c == MIN_KEY || c == MAX_KEY {
        return 1;
    }
    let blocks = usize::from(c & 0x0F) + 1;
    (0..blocks)
        .filter_map(|i| control.get(1 + i * 8..9 + i * 8))
        .map(|b| block_count(u64::from_le_bytes(b.try_into().unwrap_or([0; 8]))))
        .sum()
}

// ---------------------------------------------------------------------------
// The builder.

#[derive(Debug, Clone)]
struct SubObjState {
    state: EncodingState,
    buffer: Vec<u8>,
    control_blocks: Vec<(usize, usize)>,
}

impl SubObjState {
    fn new() -> Self {
        SubObjState {
            state: EncodingState::new(),
            buffer: Vec::new(),
            control_blocks: Vec::new(),
        }
    }

    fn append(&mut self, elem: Element<'_>) {
        let blocks = &mut self.control_blocks;
        self.state.append(elem, &mut self.buffer, &mut |off, size| {
            blocks.push((off, size))
        });
    }

    fn skip(&mut self) {
        let blocks = &mut self.control_blocks;
        self.state
            .skip(&mut self.buffer, &mut |off, size| blocks.push((off, size)));
    }

    fn flush(&mut self) {
        let blocks = &mut self.control_blocks;
        self.state
            .flush(&mut self.buffer, &mut |off, size| blocks.push((off, size)));
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Mode {
    DeterminingReference,
    Appending,
}

#[derive(Debug, Clone)]
struct Interleaved {
    mode: Mode,
    states: Vec<SubObjState>,
    reference: Vec<u8>,
    reference_type: u8,
    buffered: Vec<Vec<u8>>,
}

impl Interleaved {
    fn new(obj: &[u8], ty: u8) -> Self {
        Interleaved {
            mode: Mode::DeterminingReference,
            states: Vec::new(),
            reference: obj.to_vec(),
            reference_type: ty,
            buffered: vec![obj.to_vec()],
        }
    }
}

#[derive(Debug, Clone)]
enum State {
    Regular(EncodingState),
    Interleaved(Interleaved),
}

/// `BSONColumnBuilder`: append each value (or a skip), then `finalize`.
#[derive(Debug, Clone)]
pub struct ColumnBuilder {
    state: State,
    buf: Vec<u8>,
}

impl Default for ColumnBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn noop(_: usize, _: usize) {}

impl ColumnBuilder {
    pub fn new() -> Self {
        ColumnBuilder {
            state: State::Regular(EncodingState::new()),
            buf: Vec::new(),
        }
    }

    /// Append one value; `None` is a measurement without the field.
    pub fn append(&mut self, elem: Option<Element<'_>>) {
        let Some(elem) = elem else {
            self.skip();
            return;
        };
        if elem.ty == EOO {
            self.skip();
            return;
        }
        if !is_obj(elem.ty) || is_empty_doc(elem.value) {
            if matches!(self.state, State::Interleaved(_)) {
                self.flush_sub_obj_mode();
            }
            self.regular_append(elem);
            return;
        }
        self.append_obj(elem);
    }

    fn regular(&mut self) -> &mut EncodingState {
        if !matches!(self.state, State::Regular(_)) {
            self.state = State::Regular(EncodingState::new());
        }
        match &mut self.state {
            State::Regular(r) => r,
            State::Interleaved(_) => unreachable!("set to regular above"),
        }
    }

    fn regular_append(&mut self, elem: Element<'_>) {
        let mut buf = std::mem::take(&mut self.buf);
        self.regular().append(elem, &mut buf, &mut noop);
        self.buf = buf;
    }

    pub fn skip(&mut self) {
        match &mut self.state {
            State::Regular(r) => r.skip(&mut self.buf, &mut noop),
            State::Interleaved(il) => {
                if has_empty_obj(&il.reference) {
                    self.flush_sub_obj_mode();
                    self.skip();
                    return;
                }
                if il.mode == Mode::DeterminingReference {
                    il.buffered.push(EMPTY_DOC.to_vec());
                } else {
                    for s in &mut il.states {
                        s.skip();
                    }
                }
            }
        }
    }

    fn append_obj(&mut self, elem: Element<'_>) {
        let (ty, obj) = (elem.ty, elem.value);
        let scalars = contains_scalars(obj);
        let State::Interleaved(il) = &mut self.state else {
            if scalars {
                self.start_determine_sub_obj_reference(obj, ty);
            } else {
                self.regular_append(elem);
            }
            return;
        };
        if ty != il.reference_type {
            self.flush_sub_obj_mode();
            if scalars {
                self.start_determine_sub_obj_reference(obj, ty);
            } else {
                self.regular_append(elem);
            }
            return;
        }
        if il.mode == Mode::DeterminingReference {
            let mut n: usize = 0;
            if !traverse_lock_step(&il.reference, obj, &mut |_, _| n += 1) {
                match merge_obj(&il.reference, obj) {
                    Some(merged) => il.reference = merged,
                    None => {
                        self.flush_sub_obj_mode();
                        if scalars {
                            self.state = State::Interleaved(Interleaved::new(obj, ty));
                        } else {
                            self.regular_append(elem);
                        }
                        return;
                    }
                }
            }
            if n * 2 >= il.buffered.len() {
                il.buffered.push(obj.to_vec());
                return;
            }
            self.finish_determine_sub_obj_reference();
        }
        if !self.append_sub_elements(obj) {
            if scalars {
                self.start_determine_sub_obj_reference(obj, ty);
            } else {
                self.regular_append(elem);
            }
        }
    }

    fn append_sub_elements(&mut self, obj: &[u8]) -> bool {
        let State::Interleaved(il) = &mut self.state else {
            return false;
        };
        let mut flat: Vec<Option<(u8, Vec<u8>)>> = Vec::new();
        let reference = il.reference.clone();
        if !traverse_lock_step(&reference, obj, &mut |_, e| {
            flat.push(e.map(|e| (e.ty, e.value.to_vec())))
        }) {
            self.flush_sub_obj_mode();
            return false;
        }
        for (e, s) in flat.iter().zip(il.states.iter_mut()) {
            match e {
                Some((ty, value)) => s.append(Element { ty: *ty, value }),
                None => s.skip(),
            }
        }
        true
    }

    fn start_determine_sub_obj_reference(&mut self, obj: &[u8], ty: u8) {
        let mut buf = std::mem::take(&mut self.buf);
        self.regular().flush(&mut buf, &mut noop);
        self.buf = buf;
        self.state = State::Interleaved(Interleaved::new(obj, ty));
    }

    fn finish_determine_sub_obj_reference(&mut self) {
        let State::Interleaved(il) = &mut self.state else {
            return;
        };
        self.buf.push(if il.reference_type == OBJECT {
            0xF1
        } else {
            0xF2
        });
        self.buf.extend_from_slice(&il.reference);
        let reference = il.reference.clone();
        let first = il
            .buffered
            .first()
            .cloned()
            .unwrap_or_else(|| EMPTY_DOC.to_vec());
        let states = &mut il.states;
        traverse_lock_step(&reference, &first, &mut |r, e| {
            let mut s = SubObjState::new();
            s.state.store_previous(Element {
                ty: r.ty,
                value: r.value,
            });
            s.state.initialize_from_previous();
            match e {
                Some(e) => s.append(Element {
                    ty: e.ty,
                    value: e.value,
                }),
                None => s.skip(),
            }
            states.push(s);
        });
        il.mode = Mode::Appending;
        let rest: Vec<Vec<u8>> = il.buffered.drain(..).skip(1).collect();
        for b in rest {
            self.append_sub_elements(&b);
        }
    }

    fn flush_sub_obj_mode(&mut self) {
        if let State::Interleaved(il) = &self.state {
            if il.mode == Mode::DeterminingReference {
                self.finish_determine_sub_obj_reference();
            }
        }
        let State::Interleaved(il) = &mut self.state else {
            return;
        };
        for s in &mut il.states {
            s.flush();
        }
        // Write the streams' control blocks in the order a decoder reads
        // them: next from the stream that has produced the fewest values,
        // the lowest index on a tie.
        let mut active: Vec<(u32, usize, usize)> = (0..il.states.len())
            .filter(|&i| !il.states[i].control_blocks.is_empty())
            .map(|i| (0, i, 0))
            .collect();
        while let Some(pos) = (0..active.len()).min_by_key(|&k| (active[k].0, active[k].1)) {
            let (written, index, block) = active[pos];
            let s = &il.states[index];
            let (off, size) = s.control_blocks[block];
            let control = &s.buffer[off..off + size];
            self.buf.extend_from_slice(control);
            if block + 1 == s.control_blocks.len() {
                active.remove(pos);
                continue;
            }
            active[pos] = (
                written + num_elems_for_control_byte(control),
                index,
                block + 1,
            );
        }
        self.buf.push(EOO);
        self.state = State::Regular(EncodingState::new());
    }

    /// The column's bytes.
    pub fn finalize(mut self) -> Vec<u8> {
        match &mut self.state {
            State::Regular(r) => r.flush(&mut self.buf, &mut noop),
            State::Interleaved(_) => self.flush_sub_obj_mode(),
        }
        self.buf.push(EOO);
        self.buf
    }
}

pub use decoding::{decode, DecodeError, Value};

/// The decoder, apart from the builder's own helpers.
mod decoding {
    use super::*;

    // ---------------------------------------------------------------------------
    // Decoding: `BSONColumn::Iterator` (`bsoncolumn.cpp`) and
    // `Simple8b<T>::Iterator` (`simple8b.h`), mongod 8.2.11. Each decoded value
    // is an element without its field name; `None` is a skip (a measurement
    // without the field).

    /// One decoded value: its BSON type byte and its value bytes.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Value {
        pub ty: u8,
        pub value: Vec<u8>,
    }

    /// Why a column does not decode: `mongod`'s assertion code and message.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DecodeError {
        pub code: i32,
        pub message: &'static str,
    }

    fn invalid(code: i32) -> DecodeError {
        DecodeError {
            code,
            message: "Invalid BSON Column encoding",
        }
    }

    const INTERLEAVED_LEGACY: u8 = 0xF0;
    const INTERLEAVED: u8 = 0xF1;
    const INTERLEAVED_ARRAY_ROOT: u8 = 0xF2;

    fn is_literal_control(control: u8) -> bool {
        (control & 0xE0) == 0 || control == MIN_KEY || control == MAX_KEY
    }

    fn is_interleaved_control(control: u8) -> bool {
        matches!(
            control,
            INTERLEAVED_LEGACY | INTERLEAVED | INTERLEAVED_ARRAY_ROOT
        )
    }

    fn scale_index_for_control(control: u8) -> Option<u8> {
        match control >> 4 {
            0b1000 => Some(5),
            0b1001 => Some(0),
            0b1010 => Some(1),
            0b1011 => Some(2),
            0b1100 => Some(3),
            0b1101 => Some(4),
            _ => None,
        }
    }

    fn cstring_len(b: &[u8]) -> Option<usize> {
        b.iter().position(|&c| c == 0).map(|n| n + 1)
    }

    fn le_i32_at(b: &[u8], at: usize) -> Option<i32> {
        b.get(at..at + 4)
            .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    /// The size of a value of type `ty` at the start of `b`.
    fn value_size(ty: u8, b: &[u8]) -> Option<usize> {
        let n = match ty {
            DOUBLE | DATE | TIMESTAMP | INT64 => 8,
            STRING | CODE | 0x0E => 4 + usize::try_from(le_i32_at(b, 0)?).ok()?,
            OBJECT | ARRAY | 0x0F => usize::try_from(le_i32_at(b, 0)?).ok()?,
            BINARY => 5 + usize::try_from(le_i32_at(b, 0)?).ok()?,
            UNDEFINED | NULL | MIN_KEY | MAX_KEY => 0,
            OID => 12,
            BOOLEAN => 1,
            0x0B => {
                let p = cstring_len(b)?;
                p + cstring_len(b.get(p..)?)?
            }
            0x0C => 4 + usize::try_from(le_i32_at(b, 0)?).ok()? + 12,
            INT32 => 4,
            DECIMAL => 16,
            _ => return None,
        };
        (n <= b.len()).then_some(n)
    }

    /// `Simple8b<T>::Iterator`: the values of a run of Simple-8b blocks.
    /// `wide` is the `uint128_t` instantiation.
    #[derive(Debug, Clone)]
    struct S8Iter {
        wide: bool,
        blocks: Vec<u64>,
        at: usize,
        current: u64,
        value: Option<u128>,
        mask: u64,
        rle_remaining: u32,
        shift: u32,
        bits_per_value: u32,
        count_mask: u64,
        count_bits: u32,
        count_multiplier: u32,
        selector: u8,
        ext: u8,
    }

    const TRAILING_ZEROS_MASK: [u64; 4] = [0, 0xF, 0xF, 0x1F];
    const TRAILING_ZEROS_MULTIPLIER: [u32; 4] = [0, 1, 4, 4];

    impl S8Iter {
        fn new(wide: bool, bytes: &[u8], previous: Option<u128>) -> Result<Self, DecodeError> {
            let blocks = bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| u64::from_le_bytes(*c))
                .collect();
            let mut it = S8Iter {
                wide,
                blocks,
                at: 0,
                current: 0,
                value: previous,
                mask: 0,
                rle_remaining: 0,
                shift: 0,
                bits_per_value: 0,
                count_mask: 0,
                count_bits: 0,
                count_multiplier: 0,
                selector: 0,
                ext: 0,
            };
            if !it.blocks.is_empty() {
                it.load_block()?;
            }
            Ok(it)
        }

        fn more(&self) -> bool {
            self.at != self.blocks.len()
        }

        fn load_block(&mut self) -> Result<(), DecodeError> {
            self.current = self.blocks[self.at];
            self.selector = (self.current & 0xF) as u8;
            if self.selector == 0 {
                return Err(DecodeError {
                    code: 8787300,
                    message: "invalid selector 0",
                });
            }
            let selector_ext = ((self.current >> SELECTOR_BITS) & 0xF) as u8;
            if u64::from(self.selector) == RLE_SELECTOR {
                self.shift = 65;
                self.rle_remaining = (u32::from(selector_ext) + 1) * RLE_MULTIPLIER - 1;
                return Ok(());
            }
            self.ext = BASE_SELECTOR;
            let mut extension_bits = 0;
            if self.selector == 7 || self.selector == 8 {
                self.ext =
                    SELECTOR_TO_EXTENSION[(self.selector - 7) as usize][selector_ext as usize];
                if self.ext == INVALID_SELECTOR {
                    return Err(DecodeError {
                        code: 8787301,
                        message: "invalid extended selector",
                    });
                }
                if self.ext != BASE_SELECTOR {
                    self.selector = selector_ext;
                }
                extension_bits = 4;
            }
            let (e, s) = (self.ext as usize, self.selector as usize);
            self.mask = DECODE_MASK[e][s];
            self.count_mask = TRAILING_ZEROS_MASK[e];
            self.count_bits = u32::from(TRAILING_ZERO_BIT_SIZE[e]);
            self.count_multiplier = TRAILING_ZEROS_MULTIPLIER[e];
            self.bits_per_value = u32::from(BITS_PER_INT_FOR_SELECTOR[e][s]) + self.count_bits;
            self.shift = u32::from(SELECTOR_BITS) + extension_bits;
            self.rle_remaining = 0;
            self.load_value();
            Ok(())
        }

        fn load_value(&mut self) {
            let value = (self.current >> self.shift) & self.mask;
            if value == self.mask {
                self.value = None;
                return;
            }
            let trailing = value & self.count_mask;
            let mut zeros = trailing as u32 * self.count_multiplier;
            let v = value >> self.count_bits;
            self.value = Some(if self.wide {
                u128::from(v).wrapping_shl(zeros)
            } else {
                zeros %= 64;
                u128::from(v.wrapping_shl(zeros))
            });
        }

        /// `operator++`.
        fn advance(&mut self) -> Result<(), DecodeError> {
            if !self.more() {
                return Ok(());
            }
            if self.rle_remaining > 0 {
                self.rle_remaining -= 1;
                return Ok(());
            }
            self.shift += self.bits_per_value;
            if self.shift + self.bits_per_value > 64 {
                self.at += 1;
                if self.at == self.blocks.len() {
                    self.rle_remaining = 0;
                    self.shift = 0;
                    return Ok(());
                }
                return self.load_block();
            }
            self.load_value();
            Ok(())
        }
    }

    fn decode_int128(v: u128) -> i128 {
        ((v >> 1) ^ (!(v & 1)).wrapping_add(1)) as i128
    }

    #[derive(Debug, Clone)]
    enum Decoder {
        D64 {
            pos: Option<S8Iter>,
            last_encoded: i64,
            last_encoded_dod: i64,
            scale_index: u8,
            delta_of_delta: bool,
        },
        D128 {
            pos: Option<S8Iter>,
            last_encoded: i128,
        },
    }

    impl Decoder {
        fn d64() -> Self {
            Decoder::D64 {
                pos: None,
                last_encoded: 0,
                last_encoded_dod: 0,
                scale_index: MEMORY_AS_INTEGER,
                delta_of_delta: false,
            }
        }

        /// `pos.valid() && (++pos).more()`: advance a live block reader and say
        /// whether it still holds a value.
        fn next_in_block(&mut self) -> Result<bool, DecodeError> {
            let pos = match self {
                Decoder::D64 { pos, .. } | Decoder::D128 { pos, .. } => pos,
            };
            match pos {
                Some(p) => {
                    p.advance()?;
                    Ok(p.more())
                }
                None => Ok(false),
            }
        }
    }

    /// `DecodingState`: one stream's decoder and the last value it produced.
    #[derive(Debug, Clone)]
    struct DecodingState {
        decoder: Decoder,
        last: Option<Value>,
    }

    impl DecodingState {
        fn new() -> Self {
            DecodingState {
                decoder: Decoder::d64(),
                last: None,
            }
        }

        /// `loadUncompressed`: a literal sets the decoder up for the deltas
        /// that follow it.
        fn load_uncompressed(&mut self, v: Value) {
            let ty = v.ty;
            if uses_128bit(ty) {
                let last_encoded = match ty {
                    STRING | CODE => {
                        let s = &v.value[4..v.value.len().saturating_sub(1)];
                        encode_string(s).unwrap_or(0)
                    }
                    BINARY => {
                        let data = &v.value[5..];
                        encode_binary(data).unwrap_or(0)
                    }
                    _ => {
                        let low = read_i64(&v.value[..8]) as u64;
                        let high = read_i64(&v.value[8..16]) as u64;
                        ((u128::from(high) << 64) | u128::from(low)) as i128
                    }
                };
                self.decoder = Decoder::D128 {
                    pos: None,
                    last_encoded,
                };
            } else {
                let dod = uses_delta_of_delta(ty);
                let encoded: i64 = match ty {
                    OID => encode_object_id(&v.value),
                    DATE | INT64 | TIMESTAMP => read_i64(&v.value),
                    BOOLEAN => i64::from(v.value[0] != 0),
                    INT32 => i64::from(read_i32(&v.value)),
                    _ => 0,
                };
                self.decoder = Decoder::D64 {
                    pos: None,
                    last_encoded: if dod { 0 } else { encoded },
                    last_encoded_dod: if dod { encoded } else { 0 },
                    scale_index: MEMORY_AS_INTEGER,
                    delta_of_delta: dod,
                };
            }
            self.last = Some(v);
        }

        /// `loadControl`: a literal, or a run of Simple-8b blocks whose first
        /// value is returned. Gives the value and the bytes consumed.
        fn load_control(&mut self, buf: &[u8]) -> Result<(Option<Value>, usize), DecodeError> {
            let control = buf[0];
            if is_literal_control(control) {
                if buf.len() < 2 || buf[1] != 0 {
                    return Err(invalid(6067602));
                }
                let size = value_size(control, &buf[2..]).ok_or_else(|| invalid(6067602))?;
                let v = Value {
                    ty: control,
                    value: buf[2..2 + size].to_vec(),
                };
                self.load_uncompressed(v.clone());
                return Ok((Some(v), 2 + size));
            }
            let blocks = usize::from(control & 0x0F) + 1;
            let size = 8 * blocks;
            if size + 1 >= buf.len() {
                return Err(invalid(6067608));
            }
            let bytes = &buf[1..1 + size];
            let scale = scale_index_for_control(control);
            let last_ty = self.last.as_ref().map(|v| v.ty);
            match &mut self.decoder {
                Decoder::D64 {
                    pos,
                    last_encoded,
                    scale_index,
                    ..
                } => {
                    let s = scale.ok_or(DecodeError {
                        code: 6067606,
                        message: "Invalid control byte in BSON Column",
                    })?;
                    *scale_index = s;
                    if last_ty == Some(DOUBLE) {
                        let last = read_f64(&self.last.as_ref().expect("double").value);
                        *last_encoded = encode_double(last, s).ok_or(DecodeError {
                            code: 6067607,
                            message: "Invalid double encoding in BSON Column",
                        })?;
                    } else if s != MEMORY_AS_INTEGER {
                        return Err(DecodeError {
                            code: 8827800,
                            message: "Unexpected control for type in BSONColumn",
                        });
                    }
                    let previous = match pos {
                        Some(p) => p.value,
                        None => Some(0),
                    };
                    *pos = Some(S8Iter::new(false, bytes, previous)?);
                }
                Decoder::D128 { pos, .. } => {
                    if scale != Some(MEMORY_AS_INTEGER) {
                        return Err(DecodeError {
                            code: 8838600,
                            message: "Invalid control byte in BSON Column",
                        });
                    }
                    let previous = match pos {
                        Some(p) => p.value,
                        None => Some(0),
                    };
                    *pos = Some(S8Iter::new(true, bytes, previous)?);
                }
            }
            Ok((self.load_delta()?, size + 1))
        }

        /// `loadDelta`: the value at the block reader's position.
        fn load_delta(&mut self) -> Result<Option<Value>, DecodeError> {
            let Some(last) = self.last.clone() else {
                return Ok(None);
            };
            match &mut self.decoder {
                Decoder::D64 {
                    pos,
                    last_encoded,
                    last_encoded_dod,
                    scale_index,
                    delta_of_delta,
                } => {
                    let Some(delta) = pos.as_ref().and_then(|p| p.value) else {
                        return Ok(None);
                    };
                    let delta = delta as u64;
                    if !*delta_of_delta && delta == 0 {
                        return Ok(Some(last));
                    }
                    *last_encoded = last_encoded.wrapping_add(decode_int64(delta));
                    if *delta_of_delta {
                        *last_encoded_dod = last_encoded_dod.wrapping_add(*last_encoded);
                    }
                    let write = if *delta_of_delta {
                        *last_encoded_dod
                    } else {
                        *last_encoded
                    };
                    let value = match last.ty {
                        DOUBLE => decode_double(write, *scale_index).to_le_bytes().to_vec(),
                        OID => {
                            let b = write.to_le_bytes();
                            let mut oid = last.value.clone();
                            oid[0] = b[6];
                            oid[1] = b[5];
                            oid[2] = b[3];
                            oid[3] = b[1];
                            oid[9] = b[4];
                            oid[10] = b[2];
                            oid[11] = b[0];
                            oid
                        }
                        DATE | INT64 | TIMESTAMP => write.to_le_bytes().to_vec(),
                        BOOLEAN => vec![u8::from(write != 0)],
                        INT32 => (write as i32).to_le_bytes().to_vec(),
                        _ => {
                            return Err(DecodeError {
                                code: 6785500,
                                message: "Invalid delta in BSON Column encoding",
                            })
                        }
                    };
                    let v = Value { ty: last.ty, value };
                    self.last = Some(v.clone());
                    Ok(Some(v))
                }
                Decoder::D128 { pos, last_encoded } => {
                    let Some(delta) = pos.as_ref().and_then(|p| p.value) else {
                        return Ok(None);
                    };
                    if delta == 0 {
                        return Ok(Some(last));
                    }
                    *last_encoded = last_encoded.wrapping_add(decode_int128(delta));
                    let bytes = (*last_encoded as u128).to_le_bytes();
                    let value = match last.ty {
                        STRING | CODE => {
                            let end = bytes.iter().rposition(|&c| c != 0).map_or(0, |i| i + 1);
                            let mut s: Vec<u8> = bytes[..end].to_vec();
                            s.reverse();
                            let mut out = ((s.len() + 1) as i32).to_le_bytes().to_vec();
                            out.extend_from_slice(&s);
                            out.push(0);
                            out
                        }
                        BINARY => {
                            let len = usize::try_from(read_i32(&last.value)).unwrap_or(usize::MAX);
                            if len > 16 {
                                return Err(DecodeError {
                                    code: 8412601,
                                    message:
                                        "BinData length should not exceed 16 in a delta encoding",
                                });
                            }
                            let mut out = last.value[..5].to_vec();
                            out.extend_from_slice(&bytes[..len]);
                            out
                        }
                        DECIMAL => bytes.to_vec(),
                        _ => {
                            return Err(DecodeError {
                                code: 8412600,
                                message: "Invalid delta in BSON Column encoding",
                            })
                        }
                    };
                    let v = Value { ty: last.ty, value };
                    self.last = Some(v.clone());
                    Ok(Some(v))
                }
            }
        }
    }

    /// A node of an interleaved reference object: a scalar with its stream,
    /// or a sub-object whose fields follow.
    #[derive(Debug, Clone)]
    enum RefNode {
        Scalar {
            name: Vec<u8>,
            state: usize,
        },
        Object {
            name: Vec<u8>,
            ty: u8,
            allow_empty: bool,
            children: Vec<RefNode>,
        },
    }

    /// The elements of a BSON document's bytes: `(type, name, value)`.
    /// A document element: its type, its name and its value bytes.
    type Field<'a> = (u8, &'a [u8], &'a [u8]);

    fn elements(doc: &[u8]) -> Result<Vec<Field<'_>>, DecodeError> {
        let bad = || invalid(6067610);
        let len = usize::try_from(le_i32_at(doc, 0).ok_or_else(bad)?).map_err(|_| bad())?;
        if len < 5 || len > doc.len() {
            return Err(bad());
        }
        let mut out = Vec::new();
        let mut at = 4;
        while at < len - 1 {
            let ty = doc[at];
            let name_len = cstring_len(&doc[at + 1..len]).ok_or_else(bad)?;
            let name = &doc[at + 1..at + name_len];
            let vstart = at + 1 + name_len;
            let size = value_size(ty, &doc[vstart..len]).ok_or_else(bad)?;
            out.push((ty, name, &doc[vstart..vstart + size]));
            at = vstart + size;
        }
        Ok(out)
    }

    /// `BSONObjTraversal` over the reference object: one stream per scalar,
    /// each set up from the reference's own value (`loadUncompressed`).
    fn reference_nodes(
        doc: &[u8],
        arrays: bool,
        states: &mut Vec<DecodingState>,
    ) -> Result<Vec<RefNode>, DecodeError> {
        let mut out = Vec::new();
        for (ty, name, value) in elements(doc)? {
            if ty == OBJECT || (arrays && ty == ARRAY) {
                out.push(RefNode::Object {
                    name: name.to_vec(),
                    ty,
                    allow_empty: is_empty_doc(value),
                    children: reference_nodes(value, arrays, states)?,
                });
            } else {
                let mut s = DecodingState::new();
                s.load_uncompressed(Value {
                    ty,
                    value: value.to_vec(),
                });
                states.push(s);
                out.push(RefNode::Scalar {
                    name: name.to_vec(),
                    state: states.len() - 1,
                });
            }
        }
        Ok(out)
    }

    fn write_element(out: &mut Vec<u8>, ty: u8, name: &[u8], value: &[u8]) {
        out.push(ty);
        out.extend_from_slice(name);
        out.push(0);
        out.extend_from_slice(value);
    }

    struct Interleaved {
        states: Vec<DecodingState>,
        nodes: Vec<RefNode>,
        root_ty: u8,
    }

    enum Step {
        /// A scalar produced its next value (or a skip).
        Value(Option<Value>),
        /// The streams are exhausted and the next control is EOO.
        End,
    }

    /// The iterator's interleaved increment for one scalar stream.
    fn interleaved_scalar(
        state: &mut DecodingState,
        buf: &[u8],
        control: &mut usize,
    ) -> Result<Step, DecodeError> {
        if state.decoder.next_in_block()? {
            return Ok(Step::Value(state.load_delta()?));
        }
        let Some(&c) = buf.get(*control) else {
            return Err(invalid(6067602));
        };
        if c == EOO {
            *control += 1;
            return Ok(Step::End);
        }
        let (v, size) = state.load_control(&buf[*control..])?;
        *control += size;
        Ok(Step::Value(v))
    }

    /// Write one interleaved object's fields under `nodes` into `out`; `None`
    /// when the first scalar met the end of the interleaved section.
    fn interleaved_fields(
        nodes: &[RefNode],
        states: &mut [DecodingState],
        buf: &[u8],
        control: &mut usize,
        processed: &mut usize,
        out: &mut Vec<u8>,
    ) -> Result<bool, DecodeError> {
        for node in nodes {
            match node {
                RefNode::Scalar { name, state } => {
                    match interleaved_scalar(&mut states[*state], buf, control)? {
                        Step::End => return Ok(false),
                        Step::Value(Some(v)) => {
                            write_element(out, v.ty, name, &v.value);
                        }
                        Step::Value(None) => {}
                    }
                    *processed += 1;
                }
                RefNode::Object {
                    name,
                    ty,
                    allow_empty,
                    children,
                } => {
                    let mut sub = Vec::new();
                    let more =
                        interleaved_fields(children, states, buf, control, processed, &mut sub)?;
                    if !more {
                        return Ok(false);
                    }
                    if sub.is_empty() && !allow_empty {
                        continue;
                    }
                    let mut value = ((sub.len() + 5) as i32).to_le_bytes().to_vec();
                    value.extend_from_slice(&sub);
                    value.push(0);
                    write_element(out, *ty, name, &value);
                }
            }
        }
        Ok(true)
    }

    /// Decode a column (the bytes of a BSON binary of subtype 7) into its
    /// values, in order: `mongod`'s `BSONColumn` iterator.
    pub fn decode(buf: &[u8]) -> Result<Vec<Option<Value>>, DecodeError> {
        if buf.is_empty() {
            return Err(invalid(6067609));
        }
        let mut out: Vec<Option<Value>> = Vec::new();
        let mut control = 0usize;
        let mut regular = DecodingState::new();
        let mut interleaved: Option<Interleaved> = None;
        loop {
            if let Some(il) = &mut interleaved {
                let mut fields = Vec::new();
                let mut processed = 0;
                let more = interleaved_fields(
                    &il.nodes,
                    &mut il.states,
                    buf,
                    &mut control,
                    &mut processed,
                    &mut fields,
                )?;
                if !more {
                    if processed != 0 {
                        return Err(invalid(6067604));
                    }
                    // The first stream met the end; the rest must be spent.
                    for s in il.states.iter_mut().skip(1) {
                        if s.decoder.next_in_block()? {
                            return Err(invalid(8902201));
                        }
                    }
                    // Back to regular mode, the last object as the previous value.
                    regular = DecodingState::new();
                    regular.last = out.last().cloned().flatten();
                    interleaved = None;
                    continue;
                }
                out.push(if fields.is_empty() {
                    None
                } else {
                    let mut value = ((fields.len() + 5) as i32).to_le_bytes().to_vec();
                    value.extend_from_slice(&fields);
                    value.push(0);
                    Some(Value {
                        ty: il.root_ty,
                        value,
                    })
                });
                continue;
            }
            if regular.decoder.next_in_block()? {
                out.push(regular.load_delta()?);
                continue;
            }
            let Some(&c) = buf.get(control) else {
                return Err(invalid(6067602));
            };
            if c == EOO {
                if control + 1 != buf.len() {
                    return Err(invalid(7482200));
                }
                return Ok(out);
            }
            if is_interleaved_control(c) {
                let reference = &buf[control + 1..];
                let mut states = Vec::new();
                let nodes = reference_nodes(reference, c != INTERLEAVED_LEGACY, &mut states)?;
                if states.is_empty() {
                    return Err(invalid(6067610));
                }
                let ref_len = le_i32_at(reference, 0).unwrap_or(0) as usize;
                control += 1 + ref_len;
                if control >= buf.len() || buf[control] == EOO {
                    return Err(invalid(9232700));
                }
                interleaved = Some(Interleaved {
                    states,
                    nodes,
                    root_ty: if c == INTERLEAVED_ARRAY_ROOT {
                        ARRAY
                    } else {
                        OBJECT
                    },
                });
                continue;
            }
            let (v, size) = regular.load_control(&buf[control..])?;
            control += size;
            out.push(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn int32s(values: &[Option<i32>]) -> String {
        let raw: Vec<Option<[u8; 4]>> = values.iter().map(|v| v.map(i32::to_le_bytes)).collect();
        let mut b = ColumnBuilder::new();
        for v in &raw {
            b.append(v.as_ref().map(|v| Element {
                ty: INT32,
                value: v,
            }));
        }
        hex(&b.finalize())
    }

    // Bytes read from `mongod` 8.2.11's `system.buckets` (2026-11-09).
    #[test]
    fn small_integer_runs_are_mongods_bytes() {
        assert_eq!(
            int32s(&[Some(0), Some(1), Some(2), Some(3)]),
            "100000000000802c0000020020000000"
        );
        assert_eq!(
            int32s(&[Some(1), None, Some(3), None, None]),
            "10000100000080fbff2700fcffffff00"
        );
        assert_eq!(
            int32s(&[Some(5); 10]),
            "1000050000008107000000000000000e0000000000000000"
        );
    }

    fn column(values: &[(u8, Vec<u8>)]) -> String {
        let mut b = ColumnBuilder::new();
        for (ty, v) in values {
            b.append(Some(Element { ty: *ty, value: v }));
        }
        hex(&b.finalize())
    }

    fn string(s: &str) -> (u8, Vec<u8>) {
        let mut v = ((s.len() + 1) as i32).to_le_bytes().to_vec();
        v.extend_from_slice(s.as_bytes());
        v.push(0);
        (STRING, v)
    }

    #[test]
    fn doubles_are_scaled_as_mongod_scales_them() {
        let d = |x: f64| (DOUBLE, x.to_le_bytes().to_vec());
        assert_eq!(
            column(&[d(1.5), d(2.5), d(3.25), d(1.0)]),
            "0100000000000000f83fb08c0c009600101c0000"
        );
        assert_eq!(
            column(&[d(0.1), d(0.2), d(0.3)]),
            "01009a9999999999b93fa02d0000000800000000"
        );
    }

    #[test]
    fn short_strings_are_128_bit_deltas() {
        assert_eq!(
            column(&[string("a"), string("b"), string("ab"), string("abc")]),
            "0200020000006100812d000000000803002e20200c0000000000"
        );
        // Longer than 16 bytes: a literal each.
        let (x, y) = ("x".repeat(20), "y".repeat(20));
        let literal = |c: &str| format!("020015000000{}00", c.repeat(20));
        let want = format!("{}{}00", literal("78"), literal("79"));
        assert_eq!(column(&[string(&x), string(&y)]), want);
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// A value as a column element: its type byte and value bytes.
    fn element(v: &bson::Bson) -> (u8, Vec<u8>) {
        let bytes = bson::to_vec(&bson::doc! {"": v.clone()}).unwrap();
        (bytes[4], bytes[6..bytes.len() - 1].to_vec())
    }

    /// Encode `values` (`None` a skip), decode the bytes, and compare.
    fn round_trip(label: &str, values: &[Option<bson::Bson>]) {
        let raw: Vec<Option<(u8, Vec<u8>)>> =
            values.iter().map(|v| v.as_ref().map(element)).collect();
        let mut b = ColumnBuilder::new();
        for v in &raw {
            b.append(v.as_ref().map(|(ty, value)| Element { ty: *ty, value }));
        }
        let bytes = b.finalize();
        let got = decode(&bytes).unwrap_or_else(|e| panic!("{label}: {e:?}"));
        let want: Vec<Option<Value>> = raw
            .into_iter()
            .map(|v| v.map(|(ty, value)| Value { ty, value }))
            .collect();
        assert_eq!(got.len(), want.len(), "{label}: count");
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g, w, "{label}: value {i} of {}", hex(&bytes));
        }
    }

    fn some(values: Vec<bson::Bson>) -> Vec<Option<bson::Bson>> {
        values.into_iter().map(Some).collect()
    }

    #[test]
    fn mongods_bytes_decode_to_their_values() {
        let ints = |v: &[Option<i32>]| -> Vec<Option<Value>> {
            v.iter()
                .map(|x| {
                    x.map(|n| Value {
                        ty: INT32,
                        value: n.to_le_bytes().to_vec(),
                    })
                })
                .collect()
        };
        assert_eq!(
            decode(&unhex("100000000000802c0000020020000000")).unwrap(),
            ints(&[Some(0), Some(1), Some(2), Some(3)])
        );
        assert_eq!(
            decode(&unhex("10000100000080fbff2700fcffffff00")).unwrap(),
            ints(&[Some(1), None, Some(3), None, None])
        );
        assert_eq!(
            decode(&unhex("1000050000008107000000000000000e0000000000000000")).unwrap(),
            ints(&[Some(5); 10])
        );
        let s = |x: &str| {
            let (ty, value) = string(x);
            Some(Value { ty, value })
        };
        assert_eq!(
            decode(&unhex(
                "0200020000006100812d000000000803002e20200c0000000000"
            ))
            .unwrap(),
            vec![s("a"), s("b"), s("ab"), s("abc")]
        );
        let obj = |n: i32| {
            let mut v = vec![12, 0, 0, 0, INT32, b'a', 0];
            v.extend_from_slice(&n.to_le_bytes());
            v.push(0);
            Some(Value {
                ty: OBJECT,
                value: v,
            })
        };
        assert_eq!(
            decode(&unhex("f10c0000001061000100000000800d000000080000000000")).unwrap(),
            vec![obj(1), obj(2)]
        );
    }

    #[test]
    fn every_kind_of_column_round_trips() {
        use bson::{doc, oid::ObjectId, Binary, Bson, DateTime, Decimal128, Timestamp};
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(88);
        let mut series: Vec<(&str, Vec<Option<Bson>>)> = vec![
            ("ints", some((0..4).map(Bson::Int32).collect())),
            (
                "ints_wide",
                some(
                    [1, 1000, -5, 7, 7, 7, i32::MAX, i32::MIN, 0]
                        .into_iter()
                        .map(Bson::Int32)
                        .collect(),
                ),
            ),
            ("rle", some(vec![Bson::Int32(5); 500])),
            (
                "rle_then_change",
                some(
                    std::iter::repeat_n(5, 130)
                        .chain([6])
                        .chain(std::iter::repeat_n(5, 250))
                        .map(Bson::Int32)
                        .collect(),
                ),
            ),
            (
                "count_limit",
                some((0..1100).map(|i| Bson::Int32(i % 7)).collect()),
            ),
            (
                "int64",
                some(
                    (0..40)
                        .map(|_| Bson::Int64(rng.random_range(-(1i64 << 62)..(1i64 << 62))))
                        .collect(),
                ),
            ),
            (
                "int64_extremes",
                some(vec![
                    Bson::Int64(i64::MAX),
                    Bson::Int64(i64::MIN),
                    Bson::Int64(0),
                ]),
            ),
            (
                "doubles",
                some(
                    [1.5, 2.5, 3.25, 1.0, 0.1, 0.2, 0.3, 1.0001, 7.0]
                        .into_iter()
                        .map(Bson::Double)
                        .collect(),
                ),
            ),
            (
                "doubles_odd",
                some(
                    [
                        0.0,
                        -0.0,
                        f64::INFINITY,
                        f64::NEG_INFINITY,
                        f64::NAN,
                        1e300,
                        5e-324,
                        3.0,
                    ]
                    .into_iter()
                    .map(Bson::Double)
                    .collect(),
                ),
            ),
            (
                "doubles_random",
                some(
                    (0..80)
                        .map(|_| Bson::Double(rng.random_range(-1e6..1e6)))
                        .collect(),
                ),
            ),
            (
                "doubles_money",
                some(
                    (0..90)
                        .map(|_| {
                            Bson::Double((rng.random_range(0.0..500.0f64) * 100.0).round() / 100.0)
                        })
                        .collect(),
                ),
            ),
            (
                "strings",
                some(
                    [
                        "a",
                        "b",
                        "ab",
                        "abc",
                        "",
                        "xxxxxxxxxxxxxxxx",
                        "yyyyyyyyyyyyyyyyy",
                        "abcd",
                    ]
                    .into_iter()
                    .map(|s| Bson::String(s.into()))
                    .collect(),
                ),
            ),
            (
                "strings_nul",
                some(vec![
                    Bson::String("\u{0}a".into()),
                    Bson::String("b".into()),
                    Bson::String("\u{0}b".into()),
                ]),
            ),
            (
                "code",
                some(vec![
                    Bson::JavaScriptCode("f()".into()),
                    Bson::JavaScriptCode("g()".into()),
                ]),
            ),
            (
                "bools_nulls",
                some(vec![
                    Bson::Boolean(true),
                    Bson::Boolean(false),
                    Bson::Boolean(true),
                    Bson::Null,
                    Bson::Null,
                    Bson::Undefined,
                    Bson::MinKey,
                    Bson::MaxKey,
                    Bson::MaxKey,
                ]),
            ),
            (
                "dates",
                some(
                    (0..30)
                        .map(|_| {
                            Bson::DateTime(DateTime::from_millis(
                                1_767_225_600_000 + rng.random_range(0..10_000_000),
                            ))
                        })
                        .collect(),
                ),
            ),
            (
                "oids",
                some(
                    (0..40u32)
                        .map(|i| {
                            let mut b = [0u8; 12];
                            b[..4].copy_from_slice(&0x6500_0000u32.to_be_bytes());
                            b[4..9].copy_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
                            b[9..].copy_from_slice(&(i * 3).to_be_bytes()[1..]);
                            Bson::ObjectId(ObjectId::from_bytes(b))
                        })
                        .collect(),
                ),
            ),
            (
                "decimals",
                some(
                    (0..25)
                        .map(|_| {
                            let s = format!("{}", rng.random_range(0..1_000_000) as f64 / 100.0);
                            Bson::Decimal128(s.parse::<Decimal128>().unwrap())
                        })
                        .collect(),
                ),
            ),
            (
                "binary",
                some(
                    (0..20u8)
                        .map(|i| {
                            Bson::Binary(Binary {
                                subtype: bson::spec::BinarySubtype::Generic,
                                bytes: vec![i, i + 1, i.wrapping_mul(3)],
                            })
                        })
                        .chain([Bson::Binary(Binary {
                            subtype: bson::spec::BinarySubtype::Generic,
                            bytes: vec![b'x'; 20],
                        })])
                        .collect(),
                ),
            ),
            (
                "timestamps",
                some(
                    (0..30)
                        .map(|i| {
                            Bson::Timestamp(Timestamp {
                                time: 1_700_000_000 + i / 3,
                                increment: i % 3,
                            })
                        })
                        .collect(),
                ),
            ),
            (
                "regex",
                some(vec![
                    Bson::RegularExpression(bson::Regex {
                        pattern: "a+".into(),
                        options: String::new(),
                    }),
                    Bson::RegularExpression(bson::Regex {
                        pattern: "a+".into(),
                        options: String::new(),
                    }),
                    Bson::RegularExpression(bson::Regex {
                        pattern: "b".into(),
                        options: "i".into(),
                    }),
                ]),
            ),
            (
                "objects",
                some(
                    (0..30)
                        .map(|i: i32| {
                            Bson::Document(doc! {"a": i, "b": "s".repeat((i % 3) as usize)})
                        })
                        .collect(),
                ),
            ),
            (
                "objects_growing",
                some(
                    (0..6)
                        .flat_map(|_| {
                            [
                                doc! {"a": 1},
                                doc! {"a": 2, "b": 1},
                                doc! {"b": 2},
                                doc! {"a": 3, "c": {"d": 1}},
                            ]
                        })
                        .map(Bson::Document)
                        .collect(),
                ),
            ),
            (
                "objects_nested",
                some(
                    (0..25)
                        .map(|i| {
                            Bson::Document(
                                doc! {"x": {"y": i, "z": [i, i + 1]}, "w": i as f64 * 0.5},
                            )
                        })
                        .collect(),
                ),
            ),
            (
                "objects_empty",
                some(vec![
                    Bson::Document(doc! {}),
                    Bson::Document(doc! {"a": {}}),
                    Bson::Document(doc! {"a": 1}),
                    Bson::Document(doc! {}),
                ]),
            ),
            (
                "objects_to_scalar",
                some(vec![
                    Bson::Document(doc! {"a": 1}),
                    Bson::Document(doc! {"a": 2}),
                    Bson::Document(doc! {"a": {"b": 1}}),
                    Bson::Int32(3),
                    Bson::Document(doc! {"a": 3}),
                ]),
            ),
            (
                "arrays",
                some(
                    (0..20)
                        .map(|i| Bson::Array(vec![Bson::Int32(i), Bson::Int32(i + 1)]))
                        .chain([
                            Bson::Array(vec![Bson::Int32(1)]),
                            Bson::Array(vec![]),
                            Bson::Array((1..5).map(Bson::Int32).collect()),
                        ])
                        .collect(),
                ),
            ),
        ];
        let skips = |v: Vec<Option<Bson>>, every: usize| -> Vec<Option<Bson>> {
            v.into_iter()
                .enumerate()
                .map(|(i, x)| if i % every == 1 { None } else { x })
                .collect()
        };
        let mut with_skips = Vec::new();
        for (label, values) in &series {
            with_skips.push((*label, skips(values.clone(), 3)));
        }
        series.push(("sparse", {
            let mut v = vec![Some(Bson::Int32(1)), None, Some(Bson::Int32(3)), None, None];
            v.extend(std::iter::repeat_n(None, 70));
            v.push(Some(Bson::Int32(9)));
            v
        }));
        series.push(("skips_first", {
            let mut v: Vec<Option<Bson>> = std::iter::repeat_n(None, 65).collect();
            v.extend([Some(Bson::Double(2.5)), Some(Bson::Double(3.5))]);
            v
        }));
        series.push((
            "mixed_types",
            some(vec![
                Bson::Int32(1),
                Bson::String("x".into()),
                Bson::Double(2.5),
                Bson::Document(doc! {"a": 1}),
                Bson::Int64(4),
                Bson::Document(doc! {"a": 2}),
                Bson::Null,
            ]),
        ));
        for (label, values) in series.iter().chain(with_skips.iter()) {
            round_trip(label, values);
        }
    }

    #[test]
    fn a_damaged_column_is_refused() {
        assert_eq!(decode(&[]).unwrap_err().code, 6067609);
        // Values, but no closing EOO.
        assert!(decode(&unhex("1000000000")).is_err());
        // Bytes after the closing EOO.
        assert_eq!(
            decode(&unhex("10000000000000ff")).unwrap_err().code,
            7482200
        );
        // A Simple-8b run that runs past the end.
        assert!(decode(&unhex("10000000000080")).is_err());
    }

    #[test]
    fn objects_are_interleaved() {
        let obj = |n: i32| {
            let mut v = vec![12, 0, 0, 0, INT32, b'a', 0];
            v.extend_from_slice(&n.to_le_bytes());
            v.push(0);
            (OBJECT, v)
        };
        assert_eq!(
            column(&[obj(1), obj(2)]),
            "f10c0000001061000100000000800d000000080000000000"
        );
    }
}
