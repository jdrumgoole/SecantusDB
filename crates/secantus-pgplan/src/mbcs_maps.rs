//! The maps `mbcs` converts with, as `tools/pg_mbcs_tables.py` measured
//! them from PostgreSQL 15.19 (`mbcs_data/`, zlib-compressed LEB128 varints;
//! the script describes the layout), and GB18030's four-byte rule.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::OnceLock;

pub(crate) const DATA: [&[u8]; 12] = [
    include_bytes!("mbcs_data/euc_jp.bin"),
    include_bytes!("mbcs_data/euc_cn.bin"),
    include_bytes!("mbcs_data/euc_kr.bin"),
    include_bytes!("mbcs_data/euc_tw.bin"),
    include_bytes!("mbcs_data/sjis.bin"),
    include_bytes!("mbcs_data/big5.bin"),
    include_bytes!("mbcs_data/gbk.bin"),
    include_bytes!("mbcs_data/uhc.bin"),
    include_bytes!("mbcs_data/gb18030.bin"),
    include_bytes!("mbcs_data/johab.bin"),
    include_bytes!("mbcs_data/shift_jis_2004.bin"),
    include_bytes!("mbcs_data/euc_jis_2004.bin"),
];

/// The pairs `pg_conversion` converts between directly, their tables of
/// where the answer differs from the one through Unicode, and the error the
/// conversion function gives a character it cannot convert:
/// `euc_jis_20042shift_jis_2004` calls it invalid (22021), the others say
/// the target has no equivalent (22P05). Measured on 15.19.
pub(crate) const DIRECT: [(&str, &str, &str, &[u8]); 6] = [
    (
        "EUC_JP",
        "SJIS",
        "22P05",
        include_bytes!("mbcs_data/euc_jp-sjis.bin"),
    ),
    (
        "SJIS",
        "EUC_JP",
        "22P05",
        include_bytes!("mbcs_data/sjis-euc_jp.bin"),
    ),
    (
        "EUC_TW",
        "BIG5",
        "22P05",
        include_bytes!("mbcs_data/euc_tw-big5.bin"),
    ),
    (
        "BIG5",
        "EUC_TW",
        "22P05",
        include_bytes!("mbcs_data/big5-euc_tw.bin"),
    ),
    (
        "EUC_JIS_2004",
        "SHIFT_JIS_2004",
        "22021",
        include_bytes!("mbcs_data/euc_jis_2004-shift_jis_2004.bin"),
    ),
    (
        "SHIFT_JIS_2004",
        "EUC_JIS_2004",
        "22P05",
        include_bytes!("mbcs_data/shift_jis_2004-euc_jis_2004.bin"),
    ),
];

/// The index of a direct pair in `DIRECT`, and its table of the source codes
/// whose answer is not the one through Unicode (0: refused).
pub(crate) fn direct_diffs(from: &str, to: &str) -> (usize, &'static HashMap<u32, u32>) {
    static DIFFS: [OnceLock<HashMap<u32, u32>>; 6] = [const { OnceLock::new() }; 6];
    let i = DIRECT
        .iter()
        .position(|(f, t, _, _)| *f == from && *t == to)
        .expect("a direct pair");
    let diffs = DIFFS[i].get_or_init(|| {
        let raw = inflate(DIRECT[i].3);
        let mut r = Reader(&raw, 0);
        let mut code = 0;
        (0..r.next())
            .map(|_| {
                code += r.next();
                (code, r.next())
            })
            .collect()
    });
    (i, diffs)
}

pub(crate) struct Table {
    pub(crate) decode: HashMap<u32, u32>,
    pub(crate) pairs: HashMap<u32, (u32, u32)>,
    pub(crate) pair_encode: HashMap<(u32, u32), u32>,
    pub(crate) pair_first: HashSet<u32>,
    /// The exceptions: a code point whose code is not the lowest that
    /// decodes to it, or (0) has none.
    pub(crate) encode: HashMap<u32, u32>,
    /// The lowest code that decodes to each code point.
    pub(crate) default_encode: HashMap<u32, u32>,
}

impl Table {
    pub(crate) fn load(compressed: &[u8]) -> Table {
        let raw = inflate(compressed);
        let mut r = Reader(&raw, 0);
        let mut decode = HashMap::new();
        let mut default_encode = HashMap::new();
        let (mut code, mut cp) = (0u32, 0i64);
        for _ in 0..r.next() {
            code += r.next();
            let z = i64::from(r.next());
            cp += if z & 1 == 0 { z >> 1 } else { -((z + 1) >> 1) };
            let cp = cp as u32;
            decode.insert(code, cp);
            default_encode.entry(cp).or_insert(code);
        }
        let mut pairs = HashMap::new();
        for _ in 0..r.next() {
            let (code, a, b) = (r.next(), r.next(), r.next());
            pairs.insert(code, (a, b));
        }
        let mut encode = HashMap::new();
        let mut cp = 0;
        for _ in 0..r.next() {
            cp += r.next();
            encode.insert(cp, r.next());
        }
        Table {
            pair_encode: pairs.iter().map(|(code, ab)| (*ab, *code)).collect(),
            pair_first: pairs.values().map(|(a, _)| *a).collect(),
            decode,
            pairs,
            encode,
            default_encode,
        }
    }
}

fn inflate(compressed: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(compressed)
        .read_to_end(&mut raw)
        .expect("a measured encoding table");
    raw
}

/// LEB128 varints, as `pg_mbcs_tables.py` writes them.
struct Reader<'a>(&'a [u8], usize);

impl Reader<'_> {
    fn next(&mut self) -> u32 {
        let mut n = 0u32;
        let mut shift = 0;
        loop {
            let b = self.0[self.1];
            self.1 += 1;
            n |= u32::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                return n;
            }
            shift += 7;
        }
    }
}

/// A character's bytes read big-endian as one number, as the maps key them.
pub(crate) fn code_of(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0, |n, b| (n << 8) | u32::from(*b))
}

pub(crate) fn push_code(out: &mut Vec<u8>, code: u32) {
    let bytes = code.to_be_bytes();
    let skip = bytes.iter().take_while(|b| **b == 0).count().min(3);
    out.extend_from_slice(&bytes[skip..]);
}

/// `conv_18030_to_utf8`'s ranges: first code point, first and last code.
const GB18030_RANGES: [(u32, u32, u32); 12] = [
    (0x0452, 0x8130_D330, 0x8136_A531),
    (0x2643, 0x8137_A839, 0x8138_FD38),
    (0x361B, 0x8230_A633, 0x8230_F237),
    (0x3CE1, 0x8231_D438, 0x8232_AF32),
    (0x4160, 0x8232_C937, 0x8232_F837),
    (0x44D7, 0x8233_A339, 0x8233_C931),
    (0x478E, 0x8233_E838, 0x8234_9638),
    (0x49B8, 0x8234_A131, 0x8234_E733),
    (0x9FA6, 0x8235_8F33, 0x8336_C738),
    (0xE865, 0x8336_D030, 0x8430_8534),
    (0xFA2A, 0x8430_9C38, 0x8431_8730),
    (0x10000, 0x9030_8130, 0xE332_9A35),
];

/// `gb_linear`: a four-byte code's position counted from 0x81308130.
fn gb_linear(code: u32) -> u32 {
    let [b0, b1, b2, b3] = code.to_be_bytes().map(u32::from);
    (b0 - 0x81) * 12600 + (b1 - 0x30) * 1260 + (b2 - 0x81) * 10 + (b3 - 0x30)
}

pub(crate) fn gb18030_decode(code: u32) -> Option<u32> {
    GB18030_RANGES
        .iter()
        .find(|(_, lo, hi)| (*lo..=*hi).contains(&code))
        .map(|(cp, lo, _)| gb_linear(code) - gb_linear(*lo) + cp)
}

/// `conv_utf8_to_18030`, through `gb_unlinear`.
pub(crate) fn gb18030_encode(cp: u32) -> Option<u32> {
    let (first, lo, _) = GB18030_RANGES
        .iter()
        .find(|(first, lo, hi)| (*first..=first + gb_linear(*hi) - gb_linear(*lo)).contains(&cp))?;
    let lin = gb_linear(*lo) + cp - first;
    Some(
        ((0x81 + lin / 12600) << 24)
            | ((0x30 + lin / 1260 % 10) << 16)
            | ((0x81 + lin / 10 % 126) << 8)
            | (0x30 + lin % 10),
    )
}
