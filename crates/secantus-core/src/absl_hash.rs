//! abseil's string hash and `flat_hash_map` iteration order, as mongod 8.2
//! vendors them (abseil `20250512.1`, the `20250512.1-mongo-1` fork).
//!
//! mongod builds some documents by iterating an absl hash map, so the field
//! order it writes is the map's slot order. The one this port exists for is
//! the document an upsert seeds from its query's equalities:
//! `UpdateDriver::populateDocumentWithQueryFields` collects them in a
//! `pathsupport::EqualityMatches`, a `StringDataMap` (an
//! `absl::flat_hash_map` hashed with `absl::Hash<absl::string_view>`), and
//! `addEqualitiesToDoc` sets each path in that map's iteration order. absl
//! makes the order non-deterministic on purpose, from two seeds:
//!
//! * the hash seed is the address of a static (`MixingHashState::kSeed`), so
//!   address-space layout randomisation gives every process its own;
//! * every table draws a 16-bit per-table seed when it first allocates, from
//!   a thread-local counter that starts at the counter's own address and steps
//!   by `0xad53` (`NextSeed`).
//!
//! No fixed order can equal a given mongod's, so this ports the mechanism
//! (Joe, 2026-11-12, RM-059): the same hash, the same seeds taken the same
//! way, and the same table -- its control bytes, probing, growth and the
//! single-group shuffle -- so the orders it gives are the orders mongod can
//! give, distributed as mongod distributes them.
//!
//! The group width is the one detail that differs between mongod builds: 16
//! (SSE2) on x86-64, 8 (NEON) on arm64. Linux x86-64 mongod is the exemplar,
//! so the server uses 16; the tests check both against the real library's
//! orders (`tests/absl_hash_order.rs`).

use std::cell::Cell;

/// `MixingHashState::kMul`.
const K_MUL: u64 = 0xdcb2_2ca6_8cb1_34ed;

/// `MixingHashState::kStaticRandomData`: the fractional digits of pi.
const K_STATIC_RANDOM_DATA: [u64; 5] = [
    0x243f_6a88_85a3_08d3,
    0x1319_8a2e_0370_7344,
    0xa409_3822_299f_31d0,
    0x082e_fa98_ec4e_6c89,
    0x4528_21e6_38d0_1377,
];

/// `hash_internal::PiecewiseChunkSize()`.
const PIECEWISE_CHUNK_SIZE: usize = 1024;

/// The group width of mongod's Linux x86-64 build (`GroupSse2Impl::kWidth`).
pub const EXEMPLAR_GROUP_WIDTH: usize = 16;

/// `MixingHashState::kSeed`: absl's hash seed is this static's address.
static K_SEED: usize = 0;

/// The process's hash seed, taken as absl takes it: the address of a static,
/// which address-space layout randomisation moves from process to process.
pub fn process_seed() -> u64 {
    std::ptr::addr_of!(K_SEED) as usize as u64
}

thread_local! {
    /// `NextSeed`'s `thread_local uint16_t seed`, which starts at its own
    /// address. `None` until this thread's first table.
    static NEXT_SEED: Cell<Option<u16>> = const { Cell::new(None) };
}

/// `NextSeed()`: the next per-table seed on this thread.
fn next_seed() -> u16 {
    NEXT_SEED.with(|cell| {
        let start = cell
            .get()
            .unwrap_or(cell as *const Cell<Option<u16>> as usize as u16);
        let seed = start.wrapping_add(0xad53);
        cell.set(Some(seed));
        seed
    })
}

fn mix(lhs: u64, rhs: u64) -> u64 {
    let m = u128::from(lhs) * u128::from(rhs);
    ((m >> 64) as u64) ^ (m as u64)
}

fn weak_mix(lhs: u64, rhs: u64) -> u64 {
    (lhs ^ rhs).wrapping_mul(K_MUL).swap_bytes()
}

fn read8(p: &[u8]) -> u64 {
    u64::from_le_bytes(p[..8].try_into().expect("eight bytes"))
}

fn read4(p: &[u8]) -> u32 {
    u32::from_le_bytes(p[..4].try_into().expect("four bytes"))
}

/// `LowLevelHashLenGt32`, with `seed` the process seed.
fn low_level_hash_len_gt32(data: &[u8], seed: u64) -> u64 {
    let salt = &K_STATIC_RANDOM_DATA;
    let mut len = data.len();
    let mut ptr = 0usize;
    let mut current = seed ^ salt[0] ^ len as u64;
    let last_32 = len - 32;
    let mix32 = |p: &[u8], state: u64| {
        let cs0 = mix(read8(p) ^ salt[1], read8(&p[8..]) ^ state);
        let cs1 = mix(read8(&p[16..]) ^ salt[2], read8(&p[24..]) ^ state);
        cs0 ^ cs1
    };
    if len > 64 {
        let (mut dup0, mut dup1, mut dup2) = (current, current, current);
        loop {
            let p = &data[ptr..];
            current = mix(read8(p) ^ salt[1], read8(&p[8..]) ^ current);
            dup0 = mix(read8(&p[16..]) ^ salt[2], read8(&p[24..]) ^ dup0);
            dup1 = mix(read8(&p[32..]) ^ salt[3], read8(&p[40..]) ^ dup1);
            dup2 = mix(read8(&p[48..]) ^ salt[4], read8(&p[56..]) ^ dup2);
            ptr += 64;
            len -= 64;
            if len <= 64 {
                break;
            }
        }
        current = (current ^ dup0) ^ dup1.wrapping_add(dup2);
    }
    if len > 32 {
        current = mix32(&data[ptr..], current);
    }
    mix32(&data[last_32..], current)
}

/// `MixingHashState::CombineContiguousImpl` for a 64-bit `size_t`.
fn combine_contiguous(state: u64, data: &[u8], seed: u64) -> u64 {
    let len = data.len();
    if len <= 8 {
        let v = if len >= 4 {
            (u64::from(read4(data)) << 32) | u64::from(read4(&data[len - 4..]))
        } else if len > 0 {
            u64::from(
                (u32::from(data[0]) << 16)
                    | (u32::from(data[len / 2]) << 8)
                    | u32::from(data[len - 1]),
            )
        } else {
            return state;
        };
        return weak_mix(state, v);
    }
    if len <= 16 {
        let low = read8(data);
        let high = read8(&data[len - 8..]);
        return mix(state ^ low, K_MUL ^ high);
    }
    if len <= 32 {
        let m0 = mix(
            read8(data) ^ K_STATIC_RANDOM_DATA[1],
            read8(&data[8..]) ^ state,
        );
        let tail = &data[len - 16..];
        let m1 = mix(
            read8(tail) ^ K_STATIC_RANDOM_DATA[3],
            read8(&tail[8..]) ^ state,
        );
        return m0 ^ m1;
    }
    if len <= PIECEWISE_CHUNK_SIZE {
        return mix(state ^ low_level_hash_len_gt32(data, seed), K_MUL);
    }
    // `CombineLargeContiguousImpl64`.
    let mut state = state;
    let mut rest = data;
    while rest.len() >= PIECEWISE_CHUNK_SIZE {
        let chunk = &rest[..PIECEWISE_CHUNK_SIZE];
        state = mix(state ^ low_level_hash_len_gt32(chunk, seed), K_MUL);
        rest = &rest[PIECEWISE_CHUNK_SIZE..];
    }
    combine_contiguous(state, rest, seed)
}

/// `absl::Hash<absl::string_view>{}(s)` under hash seed `seed`: the bytes,
/// then the length as a `WeaklyMixedInteger`.
pub fn hash_string(s: &[u8], seed: u64) -> u64 {
    combine_contiguous(seed, s, seed).wrapping_add(0x57 + s.len() as u64)
}

const EMPTY: i8 = -128;
const SENTINEL: i8 = -1;

fn is_full(c: i8) -> bool {
    c >= 0
}

/// `kEmpty` or `kDeleted`. (No table here erases, so there is no `kDeleted`.)
fn is_empty_or_deleted(c: i8) -> bool {
    c < SENTINEL
}

/// `probe_seq`.
struct ProbeSeq {
    mask: usize,
    offset: usize,
    index: usize,
}

impl ProbeSeq {
    fn new(h1: usize, mask: usize) -> Self {
        ProbeSeq {
            mask,
            offset: h1 & mask,
            index: 0,
        }
    }
    fn offset_at(&self, i: usize) -> usize {
        (self.offset + i) & self.mask
    }
    fn next(&mut self, width: usize) {
        self.index += width;
        self.offset = (self.offset + self.index) & self.mask;
    }
}

/// An absl `raw_hash_set` with no small-object optimisation (mongod's
/// `StringDataMap` slot -- a `StringData` and a pointer -- is too big for it),
/// reduced to what decides where an element lives: the control bytes and
/// which inserted element each slot holds. Elements are inserted, never
/// erased.
struct SwissTable {
    width: usize,
    capacity: usize,
    size: usize,
    growth_left: usize,
    seed: u16,
    /// The per-table seed to take instead of `NextSeed()`'s (tests).
    fixed_seed: Option<u16>,
    /// `capacity + 1 + (width - 1)` bytes: the slots' control bytes, the
    /// sentinel, then the clones of the first `width - 1`.
    ctrl: Vec<i8>,
    /// The element (an index into the caller's keys) and its hash, per slot.
    slots: Vec<Option<(usize, u64)>>,
}

impl SwissTable {
    fn new(width: usize, fixed_seed: Option<u16>) -> Self {
        SwissTable {
            width,
            capacity: 0,
            size: 0,
            growth_left: 0,
            seed: 0,
            fixed_seed,
            ctrl: Vec::new(),
            slots: Vec::new(),
        }
    }

    fn h1(&self, hash: u64) -> usize {
        ((hash >> 7) as usize) ^ usize::from(self.seed)
    }

    fn h2(hash: u64) -> i8 {
        (hash & 0x7f) as i8
    }

    /// `CapacityToGrowth`.
    fn capacity_to_growth(&self, capacity: usize) -> usize {
        if self.width == 8 && capacity == 7 {
            return 6;
        }
        capacity - capacity / 8
    }

    fn is_single_group(&self, capacity: usize) -> bool {
        capacity <= self.width
    }

    /// The byte at `i`, reading past the array as `kEmpty`: a small table's
    /// bytes after its clones are dummy `kEmpty` (`is_small`).
    fn ctrl_at(ctrl: &[i8], i: usize) -> i8 {
        ctrl.get(i).copied().unwrap_or(EMPTY)
    }

    /// `SetCtrl`: the byte and, for the first `width - 1`, its clone.
    fn set_ctrl(&mut self, i: usize, h: i8) {
        self.ctrl[i] = h;
        let cloned = self.width - 1;
        let mirror = (i.wrapping_sub(cloned) & self.capacity) + (cloned & self.capacity);
        self.ctrl[mirror] = h;
    }

    /// `find_first_non_full_from_h1`.
    fn find_first_non_full(ctrl: &[i8], h1: usize, capacity: usize, width: usize) -> usize {
        let mut seq = ProbeSeq::new(h1, capacity);
        if is_empty_or_deleted(ctrl[seq.offset]) {
            return seq.offset;
        }
        loop {
            if let Some(i) =
                (0..width).find(|&i| is_empty_or_deleted(Self::ctrl_at(ctrl, seq.offset + i)))
            {
                return seq.offset_at(i);
            }
            seq.next(width);
            assert!(seq.index <= capacity, "absl model: full table");
        }
    }

    /// Insert element `id` with `hash`, unless `same` says it is already in.
    fn insert(&mut self, id: usize, hash: u64, same: impl Fn(usize) -> bool) {
        let target = if self.capacity == 0 {
            None
        } else {
            // `find_or_prepare_insert_non_soo`.
            let h2 = Self::h2(hash);
            let mut seq = ProbeSeq::new(self.h1(hash), self.capacity);
            loop {
                for i in 0..self.width {
                    let at = seq.offset_at(i);
                    if Self::ctrl_at(&self.ctrl, seq.offset + i) == h2 {
                        if let Some((other, _)) = self.slots[at] {
                            if same(other) {
                                return;
                            }
                        }
                    }
                }
                if let Some(i) =
                    (0..self.width).find(|&i| Self::ctrl_at(&self.ctrl, seq.offset + i) == EMPTY)
                {
                    break Some(seq.offset_at(i));
                }
                seq.next(self.width);
                assert!(seq.index <= self.capacity, "absl model: full table");
            }
        };
        match target {
            // `PrepareInsertNonSoo`, with growth left and nothing deleted.
            Some(at) if self.growth_left > 0 => {
                self.size += 1;
                self.growth_left -= 1;
                self.set_ctrl(at, Self::h2(hash));
                self.slots[at] = Some((id, hash));
            }
            _ => self.grow_and_insert(id, hash),
        }
    }

    /// `GrowToNextCapacityAndPrepareInsert`.
    fn grow_and_insert(&mut self, id: usize, hash: u64) {
        let width = self.width;
        let old_capacity = self.capacity;
        let new_capacity = old_capacity * 2 + 1;
        let old_ctrl = std::mem::take(&mut self.ctrl);
        let old_slots = std::mem::take(&mut self.slots);
        self.capacity = new_capacity;
        self.ctrl = vec![EMPTY; new_capacity + width];
        self.slots = vec![None; new_capacity];
        self.ctrl[new_capacity] = SENTINEL;
        let h2 = Self::h2(hash);
        let offset = if old_capacity == 0 {
            // `InitializeSingleElementControlBytes`, then the table's seed.
            self.seed = self.fixed_seed.unwrap_or_else(next_seed);
            0
        } else if self.is_single_group(new_capacity) {
            // `GrowIntoSingleGroupShuffleControlBytes`: every old element
            // moves up one slot, and the new one goes first or last.
            for (i, slot) in old_slots.into_iter().enumerate() {
                self.ctrl[i + 1] = old_ctrl[i];
                self.slots[i + 1] = slot;
            }
            if (hash ^ u64::from(self.seed)) & 1 == 1 {
                0
            } else {
                new_capacity - 1
            }
        } else {
            self.transfer_to_next_capacity(&old_ctrl, old_slots, old_capacity);
            Self::find_first_non_full(&self.ctrl, self.h1(hash), new_capacity, width)
        };
        self.rebuild_clones();
        self.set_ctrl(offset, h2);
        self.slots[offset] = Some((id, hash));
        self.size += 1;
        self.growth_left = self.capacity_to_growth(new_capacity) - self.size;
    }

    /// The sentinel and the clones of the first `width - 1` bytes, as
    /// `InitializeMirroredControlBytes` (and the shuffle) leave them.
    fn rebuild_clones(&mut self) {
        let capacity = self.capacity;
        self.ctrl[capacity] = SENTINEL;
        for i in 0..self.width - 1 {
            self.ctrl[capacity + 1 + i] = if i < capacity { self.ctrl[i] } else { EMPTY };
        }
    }

    /// `GrowToNextCapacity`: `transfer_unprobed_elements_to_next_capacity_fn`,
    /// then the probed elements in the order they were met
    /// (`DecodeAndInsertToTable`; the overflow buffer and the marked
    /// elements keep that order).
    fn transfer_to_next_capacity(
        &mut self,
        old_ctrl: &[i8],
        mut old_slots: Vec<Option<(usize, u64)>>,
        old_capacity: usize,
    ) {
        let width = self.width;
        let new_capacity = self.capacity;
        let mut probed: Vec<(i8, usize, usize)> = Vec::new();
        for old_index in 0..old_capacity {
            if !is_full(old_ctrl[old_index]) {
                continue;
            }
            let (_, hash) = old_slots[old_index].expect("a full slot holds an element");
            let h1 = self.h1(hash);
            let h2 = Self::h2(hash);
            match self.try_find_new_index_without_probing(h1, old_index, old_capacity) {
                Some(new_index) => {
                    self.ctrl[new_index] = h2;
                    self.slots[new_index] = old_slots[old_index].take();
                }
                None => probed.push((h2, old_index, h1)),
            }
        }
        self.rebuild_clones();
        for (h2, old_index, h1) in probed {
            let new_index = Self::find_first_non_full(&self.ctrl, h1, new_capacity, width);
            self.set_ctrl(new_index, h2);
            self.slots[new_index] = old_slots[old_index].take();
        }
    }

    /// `TryFindNewIndexWithoutProbing`.
    fn try_find_new_index_without_probing(
        &self,
        h1: usize,
        old_index: usize,
        old_capacity: usize,
    ) -> Option<usize> {
        let new_capacity = self.capacity;
        let mut index_diff = old_index.wrapping_sub(h1);
        let in_floating_group_index = index_diff & (self.width - 1);
        index_diff = index_diff.wrapping_sub(in_floating_group_index);
        if index_diff & old_capacity == 0 {
            return Some(h1.wrapping_add(in_floating_group_index) & new_capacity);
        }
        if h1 & old_capacity >= old_index {
            return None;
        }
        let offset = h1 & new_capacity;
        // `MaskNonFull`: the high bit set. Only the part of the new table
        // already laid out is read, and it holds no sentinel yet.
        (0..self.width)
            .find(|&i| self.ctrl[offset + i] < 0)
            .map(|i| offset + i)
    }

    /// The elements in iteration order: slot order.
    fn order(&self) -> Vec<usize> {
        self.slots.iter().flatten().map(|&(id, _)| id).collect()
    }
}

/// The order an absl `flat_hash_map<StringData, _>` iterates `keys` after
/// they are inserted in the order given, under hash seed `hash_seed` and a
/// table whose first allocation drew `table_seed`. Returns indexes into
/// `keys`; a repeated key keeps its first place.
pub fn flat_hash_map_order_with(
    keys: &[&[u8]],
    hash_seed: u64,
    table_seed: u16,
    group_width: usize,
) -> Vec<usize> {
    assert!(
        group_width == 8 || group_width == 16,
        "absl groups are 8 or 16 wide"
    );
    let mut table = SwissTable::new(group_width, Some(table_seed));
    for (id, key) in keys.iter().enumerate() {
        let hash = hash_string(key, hash_seed);
        table.insert(id, hash, |other| keys[other] == *key);
    }
    table.order()
}

/// The order mongod's `StringDataMap` iterates `keys` after they are inserted
/// in the order given: this process's hash seed, and the next per-table seed
/// on this thread, taken as absl takes them. Returns indexes into `keys`.
pub fn string_data_map_order(keys: &[&str]) -> Vec<usize> {
    let hash_seed = process_seed();
    let mut table = SwissTable::new(EXEMPLAR_GROUP_WIDTH, None);
    for (id, key) in keys.iter().enumerate() {
        let hash = hash_string(key.as_bytes(), hash_seed);
        table.insert(id, hash, |other| keys[other] == *key);
    }
    table.order()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_hashes_to_the_seed_plus_the_length_mix() {
        assert_eq!(hash_string(b"", 1000), 1000 + 0x57);
    }

    /// Four keys inserted a, b, c, d can only come out in four orders, and
    /// every one of them is reached.
    #[test]
    fn four_keys_reach_exactly_the_four_shuffle_orders() {
        let keys: Vec<&[u8]> = vec![b"a", b"b", b"c", b"d"];
        let mut seen = std::collections::BTreeSet::new();
        for s in 0..64u64 {
            for t in 0..8u16 {
                seen.insert(flat_hash_map_order_with(
                    &keys,
                    s.wrapping_mul(0x9e37_79b9_7f4a_7c15),
                    t,
                    16,
                ));
            }
        }
        let expected: std::collections::BTreeSet<Vec<usize>> = [
            vec![1, 0, 2, 3],
            vec![3, 1, 0, 2],
            vec![2, 0, 1, 3],
            vec![3, 2, 0, 1],
        ]
        .into_iter()
        .collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn per_table_seeds_step_by_0xad53_on_a_thread() {
        let a = next_seed();
        let b = next_seed();
        assert_eq!(b, a.wrapping_add(0xad53));
    }
}
