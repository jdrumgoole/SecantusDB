//! PostgreSQL's pglz compressor (`src/common/pg_lzcompress.c`), as far as
//! the length of what it writes: a catalog row too long for its page is
//! compressed before it is stored (`heap_toast_insert_or_update`), and
//! where the row goes depends on the length it then has (RP-291). The
//! output itself is never kept.

/// `PGLZ_MAX_HISTORY_LISTS`.
const MAX_HISTORY_LISTS: usize = 8192;
/// `PGLZ_HISTORY_SIZE`.
const HISTORY_SIZE: usize = 4096;
/// `PGLZ_MAX_MATCH`.
const MAX_MATCH: usize = 273;

/// `PGLZ_strategy_default`.
const MIN_INPUT_SIZE: usize = 32;
const MIN_COMP_RATE: usize = 25;
const FIRST_SUCCESS_BY: usize = 1024;
const MATCH_SIZE_GOOD: usize = 128;
const MATCH_SIZE_DROP: usize = 10;

/// `PGLZ_HistEntry`, its links as positions in the entry table (0 is
/// `INVALID_ENTRY`, which `pglz_hist_add` may scribble on).
#[derive(Clone, Copy, Default)]
struct Entry {
    next: usize,
    prev: Option<usize>,
    hindex: usize,
    pos: usize,
}

/// `pglz_hist_idx`: a hash of the next four bytes (fewer at the end),
/// each read as a C `char`, which is signed on the platforms PostgreSQL
/// is measured on.
fn hist_idx(s: &[u8], at: usize, mask: usize) -> usize {
    let c = |i: usize| i32::from(s[at + i] as i8);
    let h = if s.len() - at < 4 {
        c(0)
    } else {
        (c(0) << 6) ^ (c(1) << 4) ^ (c(2) << 2) ^ c(3)
    };
    (h as usize) & mask
}

struct History {
    start: Vec<usize>,
    entries: Vec<Entry>,
    next: usize,
    recycle: bool,
    mask: usize,
}

impl History {
    /// `pglz_hist_add`.
    fn add(&mut self, s: &[u8], at: usize) {
        let hindex = hist_idx(s, at, self.mask);
        let me = self.next;
        if self.recycle {
            let old = self.entries[me];
            match old.prev {
                None => self.start[old.hindex] = old.next,
                Some(p) => self.entries[p].next = old.next,
            }
            self.entries[old.next].prev = old.prev;
        }
        let head = self.start[hindex];
        self.entries[me] = Entry {
            next: head,
            prev: None,
            hindex,
            pos: at,
        };
        self.entries[head].prev = Some(me);
        self.start[hindex] = me;
        self.next += 1;
        if self.next > HISTORY_SIZE {
            self.next = 1;
            self.recycle = true;
        }
    }

    /// `pglz_find_match`: the longest match for the input at `at`, its
    /// length and offset, when longer than two bytes.
    fn find_match(&self, s: &[u8], at: usize) -> Option<(usize, usize)> {
        let mut good_match = MATCH_SIZE_GOOD;
        let (mut len, mut off) = (0usize, 0usize);
        let mut hent = self.start[hist_idx(s, at, self.mask)];
        while hent != 0 {
            let hp = self.entries[hent].pos;
            let thisoff = at - hp;
            if thisoff >= 0x0fff {
                break;
            }
            let mut thislen = 0;
            let extend = |from: usize| {
                let mut n = from;
                while at + n < s.len() && s[at + n] == s[hp + n] && n < MAX_MATCH {
                    n += 1;
                }
                n
            };
            if len >= 16 {
                if s[at..at + len] == s[hp..hp + len] {
                    thislen = extend(len);
                }
            } else {
                thislen = extend(0);
            }
            if thislen > len {
                len = thislen;
                off = thisoff;
            }
            hent = self.entries[hent].next;
            if hent != 0 {
                if len >= good_match {
                    break;
                }
                good_match -= (good_match * MATCH_SIZE_DROP) / 100;
            }
        }
        (len > 2).then_some((len, off))
    }
}

/// `pglz_compress` with the default strategy: the length of the
/// compressed form of `src`, or `None` where PostgreSQL gives up (too
/// short, or not compressing well enough).
pub(crate) fn compressed_len(src: &[u8]) -> Option<usize> {
    let slen = src.len();
    if slen < MIN_INPUT_SIZE {
        return None;
    }
    let result_max = (slen * (100 - MIN_COMP_RATE)) / 100;
    let hashsz = match slen {
        0..128 => 512,
        128..256 => 1024,
        256..512 => 2048,
        512..1024 => 4096,
        _ => MAX_HISTORY_LISTS,
    };
    let mut h = History {
        start: vec![0; hashsz],
        entries: vec![Entry::default(); HISTORY_SIZE + 1],
        next: 1,
        recycle: false,
        mask: hashsz - 1,
    };
    // The bytes written: each control byte (one per eight items, the
    // first reserved as the first item is written), each literal's one,
    // each tag's two or three.
    let mut out = 0usize;
    let mut items = 0usize;
    let mut found_match = false;
    let mut dp = 0;
    while dp < slen {
        if out >= result_max {
            return None;
        }
        if !found_match && out >= FIRST_SUCCESS_BY {
            return None;
        }
        if items.is_multiple_of(8) {
            out += 1;
        }
        items += 1;
        match h.find_match(src, dp) {
            Some((len, _)) => {
                out += if len > 17 { 3 } else { 2 };
                for _ in 0..len {
                    h.add(src, dp);
                    dp += 1;
                }
                found_match = true;
            }
            None => {
                out += 1;
                h.add(src, dp);
                dp += 1;
            }
        }
    }
    (out < result_max).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_input_is_not_compressed() {
        assert_eq!(compressed_len(b"abcabcabcabcabcabcabcabcabcabc"), None);
    }

    fn md5_hex(s: &str) -> String {
        use md5::{Digest, Md5};
        Md5::digest(s.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn the_lengths_are_postgresqls() {
        // Each value stored in a text column of a table on PostgreSQL
        // 15.20, its `pg_column_size` less the 8 bytes of
        // `VARHDRSZ_COMPRESSED`.
        assert_eq!(compressed_len(&[b'a'; 3000]), Some(36));
        assert_eq!(
            compressed_len("abcdefghij".repeat(300).as_bytes()),
            Some(46)
        );
        let digests = |n: u32, f: &dyn Fn(u32) -> u32, sep: &str| {
            (1..=n)
                .map(|g| md5_hex(&f(g).to_string()))
                .collect::<Vec<_>>()
                .join(sep)
        };
        // `string_agg(md5((g % 7)::text), ' ')` over 1..200.
        assert_eq!(
            compressed_len(digests(200, &|g| g % 7, " ").as_bytes()),
            Some(333)
        );
        // `repeat('ab', 2000) || string_agg(md5(g::text), '')` over 1..40.
        let mixed = "ab".repeat(2000) + &digests(40, &|g| g, "");
        assert_eq!(compressed_len(mixed.as_bytes()), Some(1468));
        // 120 digests: stored uncompressed (out of line).
        assert_eq!(compressed_len(digests(120, &|g| g, "").as_bytes()), None);
        // A view's `_RETURN` rule's `ev_action` (the module's `rpc_v`):
        // 1142 bytes in `pg_rewrite`.
        let action = include_str!("pglz_ev_action.txt");
        assert_eq!(action.len(), 2578);
        assert_eq!(compressed_len(action.as_bytes()), Some(1134));
    }
}
