//! The order a PostgreSQL 15 HashAggregate emits its groups in.
//!
//! A `GROUP BY` or `DISTINCT` that PostgreSQL plans as a HashAggregate
//! returns its groups in the order of its hash table, and an `ORDER BY`
//! above it sorts that order -- so the groups' order with no `ORDER BY`, and
//! which of two tied groups comes first with one, are the hash table's. This
//! is that table, transcribed: `TupleHashTableHash_internal`
//! (`execGrouping.c`) for the key hash, and `simplehash.h`'s create,
//! insert (Robin Hood), grow and iterate for where each group lands and the
//! order they are read back. The number of buckets the table starts with is
//! the planner's estimate of the group count (`hash_choose_num_buckets`),
//! which the caller works out.

use super::*;

/// `murmurhash32` (`common/hashfn.h`): the finaliser over the combined key.
fn murmurhash32(data: u32) -> u32 {
    let mut h = data;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h
}

/// A group's key hash: each column's hash combined by rotating left one bit
/// and XORing, a NULL contributing nothing, then `murmurhash32` (the IV is 0
/// outside a parallel worker). `None` when a key type is not hashed here.
fn group_hash(key: &[Option<Bson>], types: &[String]) -> Option<u32> {
    let mut hashkey: u32 = 0;
    for (value, ty) in key.iter().zip(types) {
        hashkey = hashkey.rotate_left(1);
        match value {
            None | Some(Bson::Null) => {}
            Some(v) => hashkey ^= crate::hashfn::plain_hash(v, ty)?,
        }
    }
    Some(murmurhash32(hashkey))
}

const FILLFACTOR: f64 = 0.9;
const GROW_MAX_DIB: u32 = 25;
const GROW_MAX_MOVE: u32 = 150;
const GROW_MIN_FILLFACTOR: f64 = 0.1;

/// simplehash's table: each bucket holds a group's index and hash.
struct Table {
    data: Vec<Option<(usize, u32)>>,
    members: usize,
    grow_threshold: f64,
}

impl Table {
    /// `SH_CREATE`: `nelements / fillfactor`, truncated, at least 2, rounded
    /// up to a power of two.
    fn new(nelements: u64) -> Table {
        let size = ((nelements as f64) / FILLFACTOR) as u64;
        Table::with_size(size)
    }

    fn with_size(size: u64) -> Table {
        let size = size.max(2).next_power_of_two() as usize;
        Table {
            data: vec![None; size],
            members: 0,
            grow_threshold: size as f64 * FILLFACTOR,
        }
    }

    fn mask(&self) -> u32 {
        (self.data.len() - 1) as u32
    }

    fn distance(&self, optimal: u32, bucket: u32) -> u32 {
        if optimal <= bucket {
            bucket - optimal
        } else {
            self.data.len() as u32 + bucket - optimal
        }
    }

    /// `SH_GROW`: a table of `newsize`, the old entries re-placed by linear
    /// probing, starting from the first old bucket that is empty or holds an
    /// entry at its optimal position.
    fn grow(&mut self, newsize: u64) {
        let old = std::mem::take(&mut self.data);
        let fresh = Table::with_size(newsize);
        self.data = fresh.data;
        self.grow_threshold = fresh.grow_threshold;
        let mask = self.mask();
        let mut start = 0;
        for (i, entry) in old.iter().enumerate() {
            match entry {
                None => {
                    start = i;
                    break;
                }
                Some((_, hash)) if (hash & mask) as usize == i => {
                    start = i;
                    break;
                }
                _ => {}
            }
        }
        let mut copy = start;
        for _ in 0..old.len() {
            if let Some((group, hash)) = old[copy] {
                let mut cur = hash & mask;
                while self.data[cur as usize].is_some() {
                    cur = (cur + 1) & mask;
                }
                self.data[cur as usize] = Some((group, hash));
            }
            copy += 1;
            if copy >= old.len() {
                copy = 0;
            }
        }
    }

    /// `SH_INSERT_HASH_INTERNAL` for a key not yet in the table (each group
    /// is inserted once, at its first row).
    fn insert(&mut self, group: usize, hash: u32) {
        'restart: loop {
            if self.members as f64 >= self.grow_threshold {
                self.grow(self.data.len() as u64 * 2);
            }
            let mask = self.mask();
            let start = hash & mask;
            let mut cur = start;
            let mut insertdist = 0u32;
            loop {
                match self.data[cur as usize] {
                    None => {
                        self.data[cur as usize] = Some((group, hash));
                        self.members += 1;
                        return;
                    }
                    Some((_, curhash)) => {
                        let curdist = self.distance(curhash & mask, cur);
                        if insertdist > curdist {
                            // Shift the run from `cur` forward to the next
                            // empty bucket, then take `cur`.
                            let mut empty = cur;
                            let mut emptydist = 0u32;
                            loop {
                                empty = (empty + 1) & mask;
                                if self.data[empty as usize].is_none() {
                                    break;
                                }
                                emptydist += 1;
                                if emptydist > GROW_MAX_MOVE
                                    && (self.members as f64 / self.data.len() as f64)
                                        >= GROW_MIN_FILLFACTOR
                                {
                                    self.grow_threshold = 0.0;
                                    continue 'restart;
                                }
                            }
                            let mut at = empty;
                            while at != cur {
                                let prev = (at + mask) & mask;
                                self.data[at as usize] = self.data[prev as usize];
                                at = prev;
                            }
                            self.data[cur as usize] = Some((group, hash));
                            self.members += 1;
                            return;
                        }
                    }
                }
                cur = (cur + 1) & mask;
                insertdist += 1;
                if insertdist > GROW_MAX_DIB
                    && (self.members as f64 / self.data.len() as f64) >= GROW_MIN_FILLFACTOR
                {
                    self.grow_threshold = 0.0;
                    continue 'restart;
                }
            }
        }
    }

    /// `SH_START_ITERATE` / `SH_ITERATE`: from the first empty bucket,
    /// backwards, all the way round.
    fn order(&self) -> Vec<usize> {
        let size = self.data.len() as u32;
        let mask = self.mask();
        let start = self.data.iter().position(Option::is_none).unwrap_or(0) as u32;
        let mut out = Vec::with_capacity(self.members);
        let mut cur = start;
        loop {
            let elem = self.data[cur as usize];
            cur = (cur + size - 1) & mask;
            if let Some((group, _)) = elem {
                out.push(group);
            }
            if cur == start {
                break;
            }
        }
        out
    }
}

/// The order PostgreSQL's HashAggregate emits `groups` in: each group's key
/// values (one per grouping column, of `types`), in the order the groups'
/// first rows arrive; `nbuckets` is the planner's group estimate (at least
/// 1). The answer is the groups' indexes, in emitted order. `None` when a
/// key's type is not one this module hashes as PostgreSQL does.
pub fn hash_agg_order(
    groups: &[Vec<Option<Bson>>],
    types: &[String],
    nbuckets: u64,
) -> Option<Vec<usize>> {
    let mut table = Table::new(nbuckets.max(1));
    for (i, key) in groups.iter().enumerate() {
        table.insert(i, group_hash(key, types)?);
    }
    Some(table.order())
}

/// What the planner knows of one grouping column of a table with no
/// statistics (`get_variable_numdistinct`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStat {
    /// A boolean: 2 distinct values.
    Bool,
    /// The column alone is a primary key or carries a unique index: as many
    /// distinct values as rows.
    Unique,
    /// Anything else: `DEFAULT_NUM_DISTINCT`, 200, or the rows if fewer.
    Unknown,
}

/// `get_typavgwidth`: a fixed-width type's length; for a declared maximum
/// (`varchar(n)`, `char(n)`: `typmod` is `n` + 4, at 4 bytes a character
/// in UTF-8) the whole of it up to 32 bytes, then 32 plus half the rest, at
/// most 516 -- `char(n)` always the whole; otherwise 32.
pub fn type_avg_width(pg_type: &str, typmod: i32, typlen: i64) -> i64 {
    if typlen > 0 {
        return typlen;
    }
    let base = pg_type.trim_start_matches("pg_catalog.");
    let bounded = matches!(
        base,
        "varchar" | "bpchar" | "character varying" | "character"
    );
    if bounded && typmod >= 4 {
        let maxwidth = i64::from(typmod - 4) * 4 + 4;
        if matches!(base, "bpchar" | "character") || maxwidth <= 32 {
            return maxwidth;
        }
        if maxwidth < 1000 {
            return 32 + (maxwidth - 32) / 2;
        }
        return 32 + (1000 - 32) / 2;
    }
    32
}

/// The rows the planner assumes a never-analysed table holds
/// (`table_block_relation_estimate_size`): `(8192 - 24) / (data width + 28)`
/// to a page, over the table's pages -- at least 10 while it has never been
/// vacuumed or analysed. `data_width` is the sum of the columns'
/// `get_typavgwidth`; `rows` is what the table holds now.
///
/// The page count is PostgreSQL's real heap size, which this server does
/// not keep; it is taken as the rows at the same density, which is exact for
/// fixed-width rows without NULLs and close otherwise. Only past ten pages
/// does it matter at all.
pub fn table_rows(data_width: i64, rows: u64) -> f64 {
    let density = (8192 - 24) / (data_width.max(0) + 28) as u64;
    let density = density.max(1);
    let pages = rows.div_ceil(density).max(10);
    (density * pages) as f64
}

/// `estimate_num_groups` over one table with no statistics and no WHERE.
/// A boolean key short-circuits: its 2 multiplies the result and takes no
/// part in the clamp. The other columns' distinct counts multiply, clamped
/// by the rows for one column and by a tenth of the rows (but not below the
/// largest column's count) for several; the whole is clamped to the rows,
/// at least 1.
pub fn estimate_groups(keys: &[KeyStat], tuples: f64) -> u64 {
    let clamp_row_est = |n: f64| if n <= 1.0 { 1.0 } else { n.round() };
    let mut numdistinct = 1.0f64;
    let mut product = 1.0f64;
    let mut max_single = 0.0f64;
    let mut vars = 0usize;
    for key in keys {
        let n = match key {
            KeyStat::Bool => {
                numdistinct *= 2.0;
                continue;
            }
            KeyStat::Unique => clamp_row_est(tuples),
            KeyStat::Unknown if tuples < 200.0 => clamp_row_est(tuples),
            KeyStat::Unknown => 200.0,
        };
        product *= n;
        max_single = max_single.max(n);
        vars += 1;
    }
    if vars > 0 {
        let mut clamp = tuples;
        if vars > 1 {
            clamp *= 0.1;
            if clamp < max_single {
                clamp = max_single.min(tuples);
            }
        }
        numdistinct *= clamp_row_est(product.min(clamp));
    }
    numdistinct.ceil().min(tuples).max(1.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PostgreSQL 15.19, a fresh table (k int, g int, t text) of 40 rows,
    /// g = (i * 7) % 11: `select g, count(*) from cm_a group by g` emits
    /// 9 3 5 4 0 10 6 2 7 1 8 from its 256-bucket table (200 groups
    /// estimated). The groups arrive in the order of their first rows.
    #[test]
    fn int_groups_come_out_in_postgres_order() {
        let mut arrival: Vec<i32> = Vec::new();
        for i in 1..=40 {
            let g = (i * 7) % 11;
            if !arrival.contains(&g) {
                arrival.push(g);
            }
        }
        let groups: Vec<Vec<Option<Bson>>> = arrival
            .iter()
            .map(|g| vec![Some(Bson::Int32(*g))])
            .collect();
        let order = hash_agg_order(&groups, &["int4".to_string()], 200).unwrap();
        let out: Vec<i32> = order.iter().map(|&i| arrival[i]).collect();
        assert_eq!(out, [9, 3, 5, 4, 0, 10, 6, 2, 7, 1, 8]);
    }

    /// The same table: `group by t` (t = 'x' || i % 6) emits x1 x3 x4 x5 x0
    /// x2, and `group by g, t` begins 0|x4 2|x2 10|x0 5|x1 5|x4 3|x5 -- the
    /// two-column estimate is 200 here too (a tenth of the 1,200 rows the
    /// planner assumes, raised to the largest single-column estimate).
    #[test]
    fn text_and_two_column_groups_come_out_in_postgres_order() {
        let mut texts: Vec<String> = Vec::new();
        let mut pairs: Vec<(i32, String)> = Vec::new();
        for i in 1..=40 {
            let t = format!("x{}", i % 6);
            if !texts.contains(&t) {
                texts.push(t.clone());
            }
            let pair = ((i * 7) % 11, t);
            if !pairs.contains(&pair) {
                pairs.push(pair);
            }
        }
        let groups: Vec<Vec<Option<Bson>>> = texts
            .iter()
            .map(|t| vec![Some(Bson::String(t.clone()))])
            .collect();
        let order = hash_agg_order(&groups, &["text".to_string()], 200).unwrap();
        let out: Vec<&str> = order.iter().map(|&i| texts[i].as_str()).collect();
        assert_eq!(out, ["x1", "x3", "x4", "x5", "x0", "x2"]);

        let groups: Vec<Vec<Option<Bson>>> = pairs
            .iter()
            .map(|(g, t)| vec![Some(Bson::Int32(*g)), Some(Bson::String(t.clone()))])
            .collect();
        let types = ["int4".to_string(), "text".to_string()];
        let order = hash_agg_order(&groups, &types, 200).unwrap();
        let out: Vec<String> = order[..6]
            .iter()
            .map(|&i| format!("{}|{}", pairs[i].0, pairs[i].1))
            .collect();
        assert_eq!(out, ["0|x4", "2|x2", "10|x0", "5|x1", "5|x4", "3|x5"]);
    }

    /// PostgreSQL 15.19's estimates on fresh tables (`EXPLAIN`): 2260 rows
    /// for (int, int), 2550 for (int), 200 groups for one int key.
    #[test]
    fn estimates_match_postgres() {
        assert_eq!(table_rows(8, 12), 2260.0);
        assert_eq!(table_rows(4, 20), 2550.0);
        // (int, varchar(10), char(3), numeric, date): 4 + 38 + 16 + 32 + 4,
        // which PostgreSQL plans as 660 rows.
        let width = type_avg_width("int4", -1, 4)
            + type_avg_width("varchar", 14, -1)
            + type_avg_width("bpchar", 7, -1)
            + type_avg_width("numeric", -1, -1)
            + type_avg_width("date", -1, 4);
        assert_eq!(width, 94);
        assert_eq!(table_rows(width, 0), 660.0);
        assert_eq!(estimate_groups(&[KeyStat::Unknown], 2260.0), 200);
        assert_eq!(estimate_groups(&[KeyStat::Unique], 2550.0), 2550);
        assert_eq!(
            estimate_groups(&[KeyStat::Unknown, KeyStat::Unknown], 1200.0),
            200
        );
        assert_eq!(
            estimate_groups(&[KeyStat::Unknown, KeyStat::Unknown], 2260.0),
            226
        );
        assert_eq!(estimate_groups(&[KeyStat::Bool], 2260.0), 2);
        // `group by g, b` over (int primary key, int, bool): 200 * 2.
        assert_eq!(
            estimate_groups(&[KeyStat::Unknown, KeyStat::Bool], 2200.0),
            400
        );
    }
}
