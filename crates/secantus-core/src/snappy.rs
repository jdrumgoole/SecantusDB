//! snappy's compressor as mongod 8.2 builds it (RM-325): snappy 1.1.10
//! (`src/third_party/snappy/dist`), whose hash table is indexed by a CRC32-C
//! on every platform mongod ships for -- its `config.h` for Linux x86-64
//! sets `SNAPPY_HAVE_X86_CRC32`, for Linux and macOS arm64
//! `SNAPPY_HAVE_NEON_CRC32`, and both instructions compute the same
//! function.
//!
//! The format is snappy's, so any decoder reads it (the `snap` crate reads
//! it back here), but which matches a compressor finds -- and so how many
//! bytes it writes -- is the compressor's own: the hash, the table's size
//! for each 64 KiB fragment, the sixteen probes it makes before skipping
//! ahead. mongod's sorter reports the compressed size of every chunk it
//! spills, so this is a port of `CompressFragment` line for line, not a
//! use of another snappy implementation (the `snap` crate ports an older
//! one, and spilled a sort 11 bytes off mongod's).

/// `kBlockSize`: input is compressed in fragments of this size.
const BLOCK: usize = 1 << 16;
/// `kMinHashTableBits`, `kMaxHashTableBits`.
const MIN_TABLE: usize = 1 << 8;
const MAX_TABLE: usize = 1 << 14;
/// `kInputMarginBytes`.
const MARGIN: usize = 15;

/// The CRC32-C table, reflected (polynomial 0x82F63B78).
const CRC32C: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0x82F6_3B78
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// `_mm_crc32_u32(crc, v)` / `__crc32cw(crc, v)`: the CRC32-C of `v`'s
/// four bytes from `crc`, with no inversion either side.
fn crc32c_u32(crc: u32, v: u32) -> u32 {
    let mut c = crc ^ v;
    for _ in 0..4 {
        c = CRC32C[(c & 0xff) as usize] ^ (c >> 8);
    }
    c
}

/// `TableEntry`: the slot for these four bytes. `mask` is twice the
/// table's last index, as the C++ masks a byte offset into a `uint16_t`
/// table.
fn entry(bytes: u32, mask: u32) -> usize {
    ((crc32c_u32(bytes, mask) & mask) >> 1) as usize
}

/// `CalculateTableSize`.
fn table_size(n: usize) -> usize {
    if n > MAX_TABLE {
        MAX_TABLE
    } else if n < MIN_TABLE {
        MIN_TABLE
    } else {
        2 << (usize::BITS - 1 - (n - 1).leading_zeros())
    }
}

/// `snappy::Compress` of `input`.
pub(crate) fn compress(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + input.len() + input.len() / 6);
    let mut n = u32::try_from(input.len()).expect("a spill chunk is under 4 GiB");
    // `Varint::Encode32`.
    while n >= 0x80 {
        out.push((n as u8) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
    let mut table = vec![0u16; MAX_TABLE];
    for fragment in input.chunks(BLOCK) {
        let size = table_size(fragment.len());
        table[..size].fill(0);
        compress_fragment(fragment, &mut table[..size], &mut out);
    }
    out
}

fn load32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn load64(b: &[u8], i: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[i..i + 8]);
    u64::from_le_bytes(a)
}

/// `EmitLiteral`.
fn emit_literal(out: &mut Vec<u8>, literal: &[u8]) {
    let n = literal.len() - 1;
    if n < 60 {
        out.push((n << 2) as u8);
    } else {
        let count = ((usize::BITS - 1 - n.leading_zeros()) >> 3) as usize + 1;
        out.push(((59 + count) << 2) as u8);
        out.extend_from_slice(&(n as u32).to_le_bytes()[..count]);
    }
    out.extend_from_slice(literal);
}

/// `EmitCopyAtMost64`.
fn emit_copy_at_most_64(out: &mut Vec<u8>, offset: usize, len: usize) {
    debug_assert!((4..=64).contains(&len) && offset < 65536);
    if len < 12 && offset < 2048 {
        out.push((1 | ((len - 4) << 2) | ((offset >> 8) << 5)) as u8);
        out.push(offset as u8);
    } else {
        out.push((2 | ((len - 1) << 2)) as u8);
        out.extend_from_slice(&(offset as u16).to_le_bytes());
    }
}

/// `EmitCopy`.
fn emit_copy(out: &mut Vec<u8>, offset: usize, mut len: usize) {
    if len < 12 {
        return emit_copy_at_most_64(out, offset, len);
    }
    while len >= 68 {
        emit_copy_at_most_64(out, offset, 64);
        len -= 64;
    }
    if len > 64 {
        emit_copy_at_most_64(out, offset, 60);
        len -= 60;
    }
    emit_copy_at_most_64(out, offset, len);
}

/// `FindMatchLength`: how many bytes from `s1` match those from `s2`,
/// `s2` reading no further than the end.
fn match_length(b: &[u8], s1: usize, s2: usize) -> usize {
    b[s2..]
        .iter()
        .zip(&b[s1..])
        .take_while(|(x, y)| x == y)
        .count()
}

/// `internal::CompressFragment`. Where the C++ carries the next bytes in
/// `data` it reads them again here: it only ever holds the input at `ip`.
fn compress_fragment(input: &[u8], table: &mut [u16], out: &mut Vec<u8>) {
    let end = input.len();
    let mask = 2 * (table.len() as u32 - 1);
    let mut ip = 0usize;
    if end >= MARGIN {
        let limit = end - MARGIN;
        let mut preload = load32(input, ip + 1);
        'emit: loop {
            let next_emit = ip;
            ip += 1;
            let mut data = load64(input, ip);
            let mut skip: u32 = 32;
            let mut candidate: usize = 0;
            let mut found = false;
            if limit >= ip && limit - ip >= 16 {
                let delta = ip;
                'probe: for j in 0..4 {
                    for k in 0..4 {
                        let i = 4 * j + k;
                        let dword = if i == 0 { preload } else { data as u32 };
                        let e = entry(dword, mask);
                        candidate = table[e] as usize;
                        table[e] = (delta + i) as u16;
                        if load32(input, candidate) == dword {
                            // A literal of the i + 1 bytes before the match.
                            out.push((i << 2) as u8);
                            out.extend_from_slice(&input[next_emit..next_emit + i + 1]);
                            ip += i;
                            found = true;
                            break 'probe;
                        }
                        data >>= 8;
                    }
                    data = load64(input, ip + 4 * j + 4);
                }
                if !found {
                    ip += 16;
                    skip += 16;
                }
            }
            if !found {
                loop {
                    let e = entry(data as u32, mask);
                    let between = (skip >> 5) as usize;
                    skip += skip >> 5;
                    let next_ip = ip + between;
                    if next_ip > limit {
                        ip = next_emit;
                        break 'emit;
                    }
                    candidate = table[e] as usize;
                    table[e] = ip as u16;
                    if data as u32 == load32(input, candidate) {
                        break;
                    }
                    data = load32(input, next_ip) as u64;
                    ip = next_ip;
                }
                emit_literal(out, &input[next_emit..ip]);
            }
            // emit_match:
            loop {
                let base = ip;
                let matched = 4 + match_length(input, candidate + 4, ip + 4);
                ip += matched;
                emit_copy(out, base - candidate, matched);
                if ip >= limit {
                    break 'emit;
                }
                let e = entry(load32(input, ip - 1), mask);
                table[e] = (ip - 1) as u16;
                let here = load32(input, ip);
                let e = entry(here, mask);
                candidate = table[e] as usize;
                table[e] = ip as u16;
                if here != load32(input, candidate) {
                    break;
                }
            }
            preload = load32(input, ip + 1);
        }
    }
    if ip < end {
        emit_literal(out, &input[ip..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(input: &[u8]) -> Vec<u8> {
        let c = compress(input);
        let back = snap::raw::Decoder::new().decompress_vec(&c).unwrap();
        assert_eq!(back, input);
        c
    }

    #[test]
    fn crc32c_is_the_instructions() {
        // `_mm_crc32_u32(0, 0)` is 0, and the standard CRC32-C check value
        // of "123456789" (inverted either side) is 0xE3069283.
        assert_eq!(crc32c_u32(0, 0), 0);
        let mut c = !0u32;
        for b in b"123456789" {
            c = CRC32C[((c ^ *b as u32) & 0xff) as usize] ^ (c >> 8);
        }
        assert_eq!(!c, 0xE306_9283);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn crc32c_is_the_arm_instruction() {
        let mut x: u32 = 0x1234_5678;
        for _ in 0..10_000 {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let m = 2 * ((1u32 << (x % 7 + 8)) - 1);
            // SAFETY: every arm64 Mac has the CRC extension.
            let hw = unsafe { std::arch::aarch64::__crc32cw(x, m) };
            assert_eq!(crc32c_u32(x, m), hw);
        }
    }

    #[test]
    fn table_sizes_are_snappys() {
        assert_eq!(table_size(1), 256);
        assert_eq!(table_size(256), 256);
        assert_eq!(table_size(257), 512);
        assert_eq!(table_size(1000), 1024);
        assert_eq!(table_size(1 << 14), 1 << 14);
        assert_eq!(table_size(1 << 16), 1 << 14);
    }

    #[test]
    fn short_and_incompressible_inputs_are_one_literal() {
        assert_eq!(round_trip(b""), [0]);
        assert_eq!(round_trip(b"abc"), [3, 2 << 2, b'a', b'b', b'c']);
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let noise: Vec<u8> = (0..70_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        round_trip(&noise);
    }

    #[test]
    fn runs_become_copies() {
        // A literal of one byte, then copies of the run at offset 1: 64,
        // 64, ..., as `EmitCopy` splits them.
        let c = round_trip(&[b'x'; 200]);
        assert_eq!(&c[..3], &[200, 1, 0][..]);
        assert_eq!(c[3], b'x');
        round_trip(&b"abcdabcdabcdefghefghefgh".repeat(5000));
    }
}
