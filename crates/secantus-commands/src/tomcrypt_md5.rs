//! MD5 as mongod 8.2 runs it: libtomcrypt's `md5_process` / `md5_done` over
//! its `hash_state` union, kept as the raw bytes mongod hands a client.
//!
//! `filemd5` with `partialOk` answers `md5state`, which is `sizeof(st)` bytes
//! of that union copied out of memory (416 on a 64-bit build), and takes it
//! back the same way. So the state is modelled as those bytes, each field at
//! the offset libtomcrypt's `struct md5_state` puts it: `length` (a u64 count
//! of the bits compressed so far), `state[4]` and `curlen` (u32s) and the
//! 64-byte `buf`. Bytes past the struct are carried through untouched, as
//! mongod's `memcpy` carries them. Measured against mongod 8.2.11
//! (2026-11-10), including a restored state's stale buffer bytes.

/// `sizeof(hash_state)` in mongod 8.2's libtomcrypt build.
pub(crate) const STATE_SIZE: usize = 416;

const LENGTH: usize = 0;
const STATE: usize = 8;
const CURLEN: usize = 24;
const BUF: usize = 28;
const BLOCK: usize = 64;

/// A libtomcrypt status other than `CRYPT_OK`: mongod reports it as 12220700
/// "MD5 operation failed".
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Md5Failed;

#[derive(Clone)]
pub(crate) struct Md5State(pub(crate) [u8; STATE_SIZE]);

impl Md5State {
    /// `md5_init` on a zeroed `hash_state`.
    pub(crate) fn new() -> Self {
        let mut s = Md5State([0; STATE_SIZE]);
        for (i, w) in [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476]
            .iter()
            .enumerate()
        {
            s.set_word(i, *w);
        }
        s
    }

    /// The state mongod copied out, when it is `sizeof(hash_state)` bytes.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        bytes.try_into().ok().map(Md5State)
    }

    fn length(&self) -> u64 {
        u64::from_le_bytes(self.0[LENGTH..LENGTH + 8].try_into().unwrap())
    }

    fn set_length(&mut self, v: u64) {
        self.0[LENGTH..LENGTH + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn word(&self, i: usize) -> u32 {
        let at = STATE + 4 * i;
        u32::from_le_bytes(self.0[at..at + 4].try_into().unwrap())
    }

    fn set_word(&mut self, i: usize, v: u32) {
        let at = STATE + 4 * i;
        self.0[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn curlen(&self) -> u32 {
        u32::from_le_bytes(self.0[CURLEN..CURLEN + 4].try_into().unwrap())
    }

    fn set_curlen(&mut self, v: u32) {
        self.0[CURLEN..CURLEN + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn compress(&mut self, block: &[u8]) {
        let mut abcd = [self.word(0), self.word(1), self.word(2), self.word(3)];
        compress(&mut abcd, block);
        for (i, w) in abcd.iter().enumerate() {
            self.set_word(i, *w);
        }
    }

    fn compress_buf(&mut self) {
        let block: [u8; BLOCK] = self.0[BUF..BUF + BLOCK].try_into().unwrap();
        self.compress(&block);
    }

    /// mongod's `md5_append`: a chunk with no bytes is not passed on;
    /// otherwise libtomcrypt's `md5_process`.
    pub(crate) fn append(&mut self, mut data: &[u8]) -> Result<(), Md5Failed> {
        if data.is_empty() {
            return Ok(());
        }
        if self.curlen() as usize > BLOCK {
            return Err(Md5Failed);
        }
        let length = self.length();
        if length.wrapping_add((data.len() as u64).wrapping_mul(8)) < length {
            return Err(Md5Failed);
        }
        while !data.is_empty() {
            let curlen = self.curlen() as usize;
            if curlen == 0 && data.len() >= BLOCK {
                self.compress(&data[..BLOCK]);
                self.set_length(self.length().wrapping_add(8 * BLOCK as u64));
                data = &data[BLOCK..];
            } else {
                let n = data.len().min(BLOCK - curlen);
                self.0[BUF + curlen..BUF + curlen + n].copy_from_slice(&data[..n]);
                self.set_curlen((curlen + n) as u32);
                data = &data[n..];
                if curlen + n == BLOCK {
                    self.compress_buf();
                    self.set_length(self.length().wrapping_add(8 * BLOCK as u64));
                    self.set_curlen(0);
                }
            }
        }
        Ok(())
    }

    /// libtomcrypt's `md5_done`, on a copy: mongod answers `md5state`
    /// before it finishes.
    pub(crate) fn finish(&self) -> Result<[u8; 16], Md5Failed> {
        let mut s = self.clone();
        let mut curlen = s.curlen() as usize;
        if curlen >= BLOCK {
            return Err(Md5Failed);
        }
        s.set_length(s.length().wrapping_add(8 * curlen as u64));
        s.0[BUF + curlen] = 0x80;
        curlen += 1;
        if curlen > 56 {
            s.0[BUF + curlen..BUF + BLOCK].fill(0);
            s.compress_buf();
            curlen = 0;
        }
        s.0[BUF + curlen..BUF + 56].fill(0);
        let length = s.length().to_le_bytes();
        s.0[BUF + 56..BUF + BLOCK].copy_from_slice(&length);
        s.compress_buf();
        let mut out = [0u8; 16];
        for i in 0..4 {
            out[4 * i..4 * i + 4].copy_from_slice(&s.word(i).to_le_bytes());
        }
        Ok(out)
    }
}

/// RFC 1321's transform of one 64-byte block.
fn compress(abcd: &mut [u32; 4], block: &[u8]) {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let m: [u32; 16] =
        std::array::from_fn(|i| u32::from_le_bytes(block[4 * i..4 * i + 4].try_into().unwrap()));
    let [mut a, mut b, mut c, mut d] = *abcd;
    for i in 0..64 {
        let (f, g) = match i / 16 {
            0 => ((b & c) | (!b & d), i),
            1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
            2 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let t = d;
        d = c;
        c = b;
        b = b.wrapping_add(
            a.wrapping_add(f)
                .wrapping_add(K[i])
                .wrapping_add(m[g])
                .rotate_left(S[i]),
        );
        a = t;
    }
    for (w, v) in abcd.iter_mut().zip([a, b, c, d]) {
        *w = w.wrapping_add(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use md5::{Digest, Md5};

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn digests_match_md5_for_every_split() {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 7 + 3) as u8).collect();
        for len in [0usize, 1, 55, 56, 57, 63, 64, 65, 127, 128, 129, 300] {
            let want = hex(&Md5::digest(&data[..len]));
            for split in [0, 1, 3, 64, len / 2, len] {
                let split = split.min(len);
                let mut s = Md5State::new();
                s.append(&data[..split]).unwrap();
                s.append(&data[split..len]).unwrap();
                assert_eq!(hex(&s.finish().unwrap()), want, "len {len} split {split}");
            }
        }
    }

    /// mongod 8.2.11's `md5state` after the chunks "abc" then 70 "d"s, and
    /// after restoring it and appending "zz" (measured 2026-11-10): the
    /// buffer keeps the bytes a block left behind.
    #[test]
    fn state_bytes_are_mongods() {
        let mut s = Md5State::new();
        s.append(b"abc").unwrap();
        s.append(&[b'd'; 70]).unwrap();
        let head = "0002000000000000773668c18b335bf8c2926202da5e0b5609000000";
        assert!(hex(&s.0).starts_with(&format!("{head}{}", "64".repeat(64))));
        assert!(s.0[92..].iter().all(|b| *b == 0));
        assert_eq!(
            hex(&s.finish().unwrap()),
            "3638bd81f0663b031de72212fa505baa"
        );
        s.append(b"zz").unwrap();
        assert_eq!(&s.0[24..28], &[11, 0, 0, 0]);
        assert_eq!(&s.0[28..28 + 13], b"dddddddddzzdd");
        assert_eq!(
            hex(&s.finish().unwrap()),
            "9a5547516e30b249f4ddeca392d3031a"
        );
    }

    #[test]
    fn a_bad_state_fails_as_libtomcrypt_does() {
        let mut s = Md5State::new();
        s.set_curlen(64);
        assert_eq!(s.finish(), Err(Md5Failed));
        // 64 is still a full buffer to process: it is compressed first.
        s.append(b"zz").unwrap();
        assert_eq!(s.curlen(), 2);
        s.set_curlen(65);
        assert_eq!(s.append(b"x"), Err(Md5Failed));
        // An empty chunk never reaches md5_process.
        assert_eq!(s.append(b""), Ok(()));
        let mut s = Md5State::new();
        s.set_length(u64::MAX - 7);
        assert_eq!(s.append(b"abc"), Err(Md5Failed));
    }
}
