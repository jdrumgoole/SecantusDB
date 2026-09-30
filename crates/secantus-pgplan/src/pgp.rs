//! pgcrypto's PGP functions over a password: `pgp_sym_encrypt[_bytea]`,
//! `pgp_sym_decrypt[_bytea]`, `pgp_key_id`, `armor` and `dearmor`.
//!
//! The message format is OpenPGP (RFC 4880) as pgcrypto writes it: a
//! Symmetric-Key Encrypted Session Key packet (tag 3) with an S2K
//! specifier, then a Symmetrically Encrypted Integrity Protected Data packet
//! (tag 18, with its SHA-1 modification detection code) -- or, with
//! `disable-mdc=1`, the older tag 9 with OpenPGP's resynchronising CFB.
//! Inside is a Literal Data packet (tag 11), optionally in a Compressed Data
//! packet (tag 8, ZIP or ZLIB).
//!
//! Ciphertext is random, so correctness is measured by CROSS-decryption: a
//! message this module writes must decrypt on PostgreSQL, and one PostgreSQL
//! writes must decrypt here. Error texts are pgcrypto's `px_strerror`, all
//! SQLSTATE 39000.

use super::*;
use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use sha1::Digest;

pub(crate) fn px(message: &str) -> Error {
    Error::Sqlstate("39000", message.to_string())
}

pub(crate) fn corrupt() -> Error {
    px("Wrong key or corrupt data")
}

/// A cipher by its OpenPGP algorithm id.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Algo {
    TripleDes,
    Cast5,
    Blowfish,
    Aes128,
    Aes192,
    Aes256,
}

impl Algo {
    pub(crate) fn from_id(id: u8) -> Result<Self> {
        Ok(match id {
            2 => Algo::TripleDes,
            3 => Algo::Cast5,
            4 => Algo::Blowfish,
            7 => Algo::Aes128,
            8 => Algo::Aes192,
            9 => Algo::Aes256,
            _ => return Err(px("Unsupported cipher algorithm")),
        })
    }

    pub(crate) fn from_name(name: &str) -> Result<Self> {
        Ok(match name {
            "3des" => Algo::TripleDes,
            "cast5" => Algo::Cast5,
            "bf" => Algo::Blowfish,
            "aes" | "aes128" => Algo::Aes128,
            "aes192" => Algo::Aes192,
            "aes256" => Algo::Aes256,
            _ => return Err(px("Unsupported cipher algorithm")),
        })
    }

    pub(crate) fn id(self) -> u8 {
        match self {
            Algo::TripleDes => 2,
            Algo::Cast5 => 3,
            Algo::Blowfish => 4,
            Algo::Aes128 => 7,
            Algo::Aes192 => 8,
            Algo::Aes256 => 9,
        }
    }

    pub(crate) fn key_len(self) -> usize {
        match self {
            Algo::Aes128 | Algo::Cast5 | Algo::Blowfish => 16,
            Algo::Aes192 | Algo::TripleDes => 24,
            Algo::Aes256 => 32,
        }
    }

    pub(crate) fn block_size(self) -> usize {
        match self {
            Algo::TripleDes | Algo::Cast5 | Algo::Blowfish => 8,
            _ => 16,
        }
    }
}

/// A keyed block cipher. CFB needs only the encrypting direction; the raw
/// `encrypt()` / `decrypt()` functions (`pgcrypto_raw`) use both.
pub(crate) enum Cipher {
    Aes128(Box<aes::Aes128>),
    Aes192(Box<aes::Aes192>),
    Aes256(Box<aes::Aes256>),
    TripleDes(Box<des::TdesEde3>),
    Cast5(Box<cast5::Cast5>),
    Blowfish(Box<blowfish::Blowfish>),
    /// Single DES: only the raw `encrypt()` offers it, never a PGP message.
    Des(Box<des::Des>),
}

impl Cipher {
    pub(crate) fn new(algo: Algo, key: &[u8]) -> Result<Self> {
        let bad = |_| corrupt();
        Ok(match algo {
            Algo::Aes128 => {
                Cipher::Aes128(Box::new(aes::Aes128::new_from_slice(key).map_err(bad)?))
            }
            Algo::Aes192 => {
                Cipher::Aes192(Box::new(aes::Aes192::new_from_slice(key).map_err(bad)?))
            }
            Algo::Aes256 => {
                Cipher::Aes256(Box::new(aes::Aes256::new_from_slice(key).map_err(bad)?))
            }
            Algo::TripleDes => {
                Cipher::TripleDes(Box::new(des::TdesEde3::new_from_slice(key).map_err(bad)?))
            }
            Algo::Cast5 => Cipher::Cast5(Box::new(cast5::Cast5::new_from_slice(key).map_err(bad)?)),
            Algo::Blowfish => Cipher::Blowfish(Box::new(
                blowfish::Blowfish::new_from_slice(key).map_err(bad)?,
            )),
        })
    }

    pub(crate) fn decrypt_block(&self, block: &mut [u8]) {
        match self {
            Cipher::Aes128(c) => c.decrypt_block(GenericArray::from_mut_slice(block)),
            Cipher::Aes192(c) => c.decrypt_block(GenericArray::from_mut_slice(block)),
            Cipher::Aes256(c) => c.decrypt_block(GenericArray::from_mut_slice(block)),
            Cipher::TripleDes(c) => c.decrypt_block(GenericArray::from_mut_slice(block)),
            Cipher::Cast5(c) => c.decrypt_block(GenericArray::from_mut_slice(block)),
            Cipher::Blowfish(c) => c.decrypt_block(GenericArray::from_mut_slice(block)),
            Cipher::Des(c) => c.decrypt_block(GenericArray::from_mut_slice(block)),
        }
    }

    pub(crate) fn encrypt_block(&self, block: &mut [u8]) {
        match self {
            Cipher::Aes128(c) => c.encrypt_block(GenericArray::from_mut_slice(block)),
            Cipher::Aes192(c) => c.encrypt_block(GenericArray::from_mut_slice(block)),
            Cipher::Aes256(c) => c.encrypt_block(GenericArray::from_mut_slice(block)),
            Cipher::TripleDes(c) => c.encrypt_block(GenericArray::from_mut_slice(block)),
            Cipher::Cast5(c) => c.encrypt_block(GenericArray::from_mut_slice(block)),
            Cipher::Blowfish(c) => c.encrypt_block(GenericArray::from_mut_slice(block)),
            Cipher::Des(c) => c.encrypt_block(GenericArray::from_mut_slice(block)),
        }
    }
}

/// Standard CFB from `iv`, in place.
pub(crate) fn cfb(cipher: &Cipher, bs: usize, iv: &[u8], data: &mut [u8], decrypt: bool) {
    let mut fr = iv.to_vec();
    for chunk in data.chunks_mut(bs) {
        let mut pad = fr.clone();
        cipher.encrypt_block(&mut pad);
        let mut next = vec![0u8; bs];
        for (i, b) in chunk.iter_mut().enumerate() {
            let c = if decrypt { *b } else { *b ^ pad[i] };
            *b ^= pad[i];
            next[i] = c;
        }
        fr = next;
    }
}

/// OpenPGP's CFB for the tag 9 packet: the `bs + 2`-byte prefix, then a
/// resynchronisation on the ciphertext's bytes `2..bs + 2`.
pub(crate) fn openpgp_cfb(
    cipher: &Cipher,
    bs: usize,
    data: &mut [u8],
    decrypt: bool,
) -> Result<()> {
    if data.len() < bs + 2 {
        return Err(corrupt());
    }
    let (head, tail) = data.split_at_mut(bs + 2);
    let zero = vec![0u8; bs];
    let resync_from_cipher = |head: &[u8]| head[2..bs + 2].to_vec();
    if decrypt {
        let iv = resync_from_cipher(head);
        cfb(cipher, bs, &zero, head, true);
        cfb(cipher, bs, &iv, tail, true);
    } else {
        cfb(cipher, bs, &zero, head, false);
        let iv = resync_from_cipher(head);
        cfb(cipher, bs, &iv, tail, false);
    }
    Ok(())
}

/// The S2K hash: MD5 (1) or SHA-1 (2), as pgcrypto offers.
pub(crate) enum S2kHash {
    Md5(md5::Md5),
    Sha1(sha1::Sha1),
}

impl S2kHash {
    pub(crate) fn new(id: u8) -> Result<Self> {
        Ok(match id {
            1 => S2kHash::Md5(md5::Md5::new()),
            2 => S2kHash::Sha1(sha1::Sha1::new()),
            _ => return Err(px("Unsupported digest algorithm")),
        })
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        match self {
            S2kHash::Md5(h) => h.update(data),
            S2kHash::Sha1(h) => h.update(data),
        }
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        match self {
            S2kHash::Md5(h) => h.finalize().to_vec(),
            S2kHash::Sha1(h) => h.finalize().to_vec(),
        }
    }
}

pub(crate) fn s2k_hash_id(name: &str) -> Result<u8> {
    match name {
        "md5" => Ok(1),
        "sha1" => Ok(2),
        _ => Err(px("Illegal argument to function")),
    }
}

/// An S2K specifier: mode 0 (simple), 1 (salted) or 3 (iterated + salted).
pub(crate) struct S2k {
    mode: u8,
    hash: u8,
    salt: [u8; 8],
    count: u8,
}

pub(crate) fn decode_count(c: u8) -> usize {
    (16 + usize::from(c & 15)) << (usize::from(c >> 4) + 6)
}

/// The smallest count byte whose decoded count is at least `count`.
pub(crate) fn encode_count(count: usize) -> u8 {
    (0..=255u8)
        .find(|c| decode_count(*c) >= count)
        .unwrap_or(255)
}

impl S2k {
    pub(crate) fn derive(&self, pass: &[u8], key_len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut preload = 0usize;
        while out.len() < key_len {
            let mut h = S2kHash::new(self.hash)?;
            h.update(&vec![0u8; preload]);
            match self.mode {
                0 => h.update(pass),
                1 => {
                    h.update(&self.salt);
                    h.update(pass);
                }
                _ => {
                    let mut sp = self.salt.to_vec();
                    sp.extend_from_slice(pass);
                    let mut left = decode_count(self.count).max(sp.len());
                    while left > 0 {
                        let n = left.min(sp.len());
                        h.update(&sp[..n]);
                        left -= n;
                    }
                }
            }
            out.extend(h.finish());
            preload += 1;
        }
        out.truncate(key_len);
        Ok(out)
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        out.push(self.mode);
        out.push(self.hash);
        if self.mode != 0 {
            out.extend_from_slice(&self.salt);
        }
        if self.mode == 3 {
            out.push(self.count);
        }
    }

    pub(crate) fn read(body: &[u8]) -> Result<(Self, usize)> {
        let mode = *body.first().ok_or_else(corrupt)?;
        let hash = *body.get(1).ok_or_else(corrupt)?;
        let mut salt = [0u8; 8];
        let mut used = 2;
        if mode == 1 || mode == 3 {
            salt.copy_from_slice(body.get(2..10).ok_or_else(corrupt)?);
            used = 10;
        } else if mode != 0 {
            return Err(corrupt());
        }
        let mut count = 0;
        if mode == 3 {
            count = *body.get(10).ok_or_else(corrupt)?;
            used = 11;
        }
        Ok((
            S2k {
                mode,
                hash,
                salt,
                count,
            },
            used,
        ))
    }
}

/// The options string: `cipher-algo=aes256, compress-algo=1`.
pub(crate) struct Options {
    pub(crate) cipher: Algo,
    pub(crate) s2k_cipher: Option<Algo>,
    pub(crate) compress_algo: u8,
    pub(crate) compress_level: u32,
    pub(crate) convert_crlf: bool,
    pub(crate) disable_mdc: bool,
    pub(crate) sess_key: bool,
    pub(crate) s2k_mode: u8,
    pub(crate) s2k_count: Option<usize>,
    pub(crate) s2k_digest: u8,
    pub(crate) unicode_mode: bool,
}

impl Options {
    pub(crate) fn parse(text: &str) -> Result<Self> {
        let mut o = Options {
            cipher: Algo::Aes128,
            s2k_cipher: None,
            compress_algo: 0,
            compress_level: 6,
            convert_crlf: false,
            disable_mdc: false,
            sess_key: false,
            s2k_mode: 3,
            s2k_count: None,
            s2k_digest: 2,
            unicode_mode: false,
        };
        let illegal = || px("Illegal argument to function");
        for item in text.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let (key, value) = item.split_once('=').ok_or_else(illegal)?;
            let (key, value) = (key.trim(), value.trim());
            let flag = |v: &str| match v {
                "0" => Ok(false),
                "1" => Ok(true),
                _ => Err(illegal()),
            };
            let int = |v: &str| v.parse::<i64>().map_err(|_| illegal());
            match key {
                "cipher-algo" => o.cipher = Algo::from_name(value)?,
                "s2k-cipher-algo" => o.s2k_cipher = Some(Algo::from_name(value)?),
                "compress-algo" => {
                    o.compress_algo = match int(value)? {
                        0 => 0,
                        1 => 1,
                        2 => 2,
                        3 => return Err(px("Unsupported compression algorithm")),
                        _ => return Err(illegal()),
                    }
                }
                "compress-level" => {
                    let l = int(value)?;
                    if !(0..=9).contains(&l) {
                        return Err(illegal());
                    }
                    o.compress_level = l as u32;
                }
                "convert-crlf" => o.convert_crlf = flag(value)?,
                "disable-mdc" => o.disable_mdc = flag(value)?,
                "sess-key" => o.sess_key = flag(value)?,
                "unicode-mode" => o.unicode_mode = flag(value)?,
                "s2k-mode" => {
                    o.s2k_mode = match int(value)? {
                        0 => 0,
                        1 => 1,
                        3 => 3,
                        _ => return Err(illegal()),
                    }
                }
                "s2k-count" => {
                    let c = int(value)?;
                    if !(1024..=65_011_712).contains(&c) {
                        return Err(illegal());
                    }
                    o.s2k_count = Some(c as usize);
                }
                "s2k-digest-algo" => o.s2k_digest = s2k_hash_id(value)?,
                // Decrypt-side options pgcrypto accepts and checks only in
                // its debug paths.
                "debug"
                | "expect-cipher-algo"
                | "expect-disable-mdc"
                | "expect-sess-key"
                | "expect-s2k-mode"
                | "expect-s2k-count"
                | "expect-s2k-digest-algo"
                | "expect-s2k-cipher-algo"
                | "expect-compress-algo"
                | "expect-unicode-mode" => {}
                _ => return Err(illegal()),
            }
        }
        Ok(o)
    }
}

pub(crate) fn random(n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).map_err(|_| px("Random generator error"))?;
    Ok(buf)
}

/// One packet in the new format with a definite length.
pub(crate) fn packet(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0xC0 | tag];
    let n = body.len();
    if n < 192 {
        out.push(n as u8);
    } else if n < 8384 {
        let m = n - 192;
        out.push(((m >> 8) + 192) as u8);
        out.push((m & 0xFF) as u8);
    } else {
        out.push(0xFF);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    }
    out.extend_from_slice(body);
    out
}

/// Read one packet at `pos`: its tag, its (reassembled) body and the
/// position after it. Old and new formats, partial lengths included.
pub(crate) fn read_packet(buf: &[u8], pos: usize) -> Result<(u8, Vec<u8>, usize)> {
    let first = *buf.get(pos).ok_or_else(corrupt)?;
    if first & 0x80 == 0 {
        return Err(corrupt());
    }
    let mut p = pos + 1;
    let byte = |i: usize| buf.get(i).copied().ok_or_else(corrupt);
    let take = |from: usize, n: usize| buf.get(from..from + n).ok_or_else(corrupt);
    if first & 0x40 == 0 {
        let tag = (first >> 2) & 0x0F;
        let len = match first & 3 {
            0 => {
                p += 1;
                usize::from(byte(p - 1)?)
            }
            1 => {
                p += 2;
                usize::from(byte(p - 2)?) << 8 | usize::from(byte(p - 1)?)
            }
            2 => {
                p += 4;
                u32::from_be_bytes(take(p - 4, 4)?.try_into().map_err(|_| corrupt())?) as usize
            }
            _ => buf.len() - p,
        };
        return Ok((tag, take(p, len)?.to_vec(), p + len));
    }
    let tag = first & 0x3F;
    let mut body = Vec::new();
    loop {
        let l1 = byte(p)?;
        let (len, last) = if l1 < 192 {
            p += 1;
            (usize::from(l1), true)
        } else if l1 < 224 {
            p += 2;
            (
                ((usize::from(l1) - 192) << 8) + usize::from(byte(p - 1)?) + 192,
                true,
            )
        } else if l1 == 255 {
            p += 5;
            (
                u32::from_be_bytes(take(p - 4, 4)?.try_into().map_err(|_| corrupt())?) as usize,
                true,
            )
        } else {
            p += 1;
            (1usize << (l1 & 0x1F), false)
        };
        body.extend_from_slice(take(p, len)?);
        p += len;
        if last {
            return Ok((tag, body, p));
        }
    }
}

pub(crate) fn compress(algo: u8, level: u32, data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    let level = flate2::Compression::new(level);
    let err = |_| px("Compression error");
    Ok(match algo {
        1 => {
            let mut e = flate2::write::DeflateEncoder::new(Vec::new(), level);
            e.write_all(data).map_err(err)?;
            e.finish().map_err(err)?
        }
        _ => {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), level);
            e.write_all(data).map_err(err)?;
            e.finish().map_err(err)?
        }
    })
}

pub(crate) fn decompress(algo: u8, data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    match algo {
        0 => out.extend_from_slice(data),
        1 => {
            flate2::read::DeflateDecoder::new(data)
                .read_to_end(&mut out)
                .map_err(|_| corrupt())?;
        }
        2 => {
            flate2::read::ZlibDecoder::new(data)
                .read_to_end(&mut out)
                .map_err(|_| corrupt())?;
        }
        _ => return Err(px("Unsupported compression algorithm")),
    }
    Ok(out)
}

/// `\n` -> `\r\n`, for `convert-crlf` on a text message.
pub(crate) fn to_crlf(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for &b in data {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

pub(crate) fn from_crlf(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'\r' && data.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(data[i]);
        i += 1;
    }
    out
}

pub(crate) fn encrypt(data: &[u8], pass: &[u8], opts: &Options, text: bool) -> Result<Vec<u8>> {
    let count = match opts.s2k_count {
        Some(c) => c,
        // pgcrypto picks a random iteration count in this range.
        None => {
            let r = random(4)?;
            65_536 + (u32::from_be_bytes([r[0], r[1], r[2], r[3]]) as usize % (253_952 - 65_536))
        }
    };
    let salt: [u8; 8] = random(8)?.try_into().map_err(|_| corrupt())?;
    let s2k = S2k {
        mode: opts.s2k_mode,
        hash: opts.s2k_digest,
        salt,
        count: encode_count(count),
    };
    let mut out = Vec::new();
    // Tag 3: the session key is the S2K key itself, or (`sess-key=1`) a
    // random key encrypted under it.
    let mut skesk = vec![4u8];
    let key = if opts.sess_key {
        let wrap = opts.s2k_cipher.unwrap_or(opts.cipher);
        skesk.push(wrap.id());
        s2k.write(&mut skesk);
        let s2k_key = s2k.derive(pass, wrap.key_len())?;
        let session = random(opts.cipher.key_len())?;
        let mut esk = vec![opts.cipher.id()];
        esk.extend_from_slice(&session);
        cfb(
            &Cipher::new(wrap, &s2k_key)?,
            wrap.block_size(),
            &vec![0u8; wrap.block_size()],
            &mut esk,
            false,
        );
        skesk.extend_from_slice(&esk);
        session
    } else {
        skesk.push(opts.cipher.id());
        s2k.write(&mut skesk);
        s2k.derive(pass, opts.cipher.key_len())?
    };
    out.extend(packet(3, &skesk));
    seal(&mut out, opts.cipher, &key, data, opts, text)?;
    Ok(out)
}

/// The encrypted body every message ends with, under the session `key`:
/// the literal data (perhaps compressed) in a tag 18 packet, or tag 9.
pub(crate) fn seal(
    out: &mut Vec<u8>,
    algo: Algo,
    key: &[u8],
    data: &[u8],
    opts: &Options,
    text: bool,
) -> Result<()> {
    // Tag 11: the literal data.
    let kind = if !text {
        b'b'
    } else if opts.unicode_mode {
        b'u'
    } else {
        b't'
    };
    let body = if text && opts.convert_crlf {
        to_crlf(data)
    } else {
        data.to_vec()
    };
    let mut literal = vec![kind, 0];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32);
    literal.extend_from_slice(&now.to_be_bytes());
    literal.extend_from_slice(&body);
    let mut inner = packet(11, &literal);
    if opts.compress_algo != 0 {
        let mut c = vec![opts.compress_algo];
        c.extend(compress(opts.compress_algo, opts.compress_level, &inner)?);
        inner = packet(8, &c);
    }
    // The encrypted packet: a random block with its last two bytes repeated.
    let bs = algo.block_size();
    let cipher = Cipher::new(algo, key)?;
    let mut plain = random(bs)?;
    let repeat = [plain[bs - 2], plain[bs - 1]];
    plain.extend_from_slice(&repeat);
    plain.extend_from_slice(&inner);
    if opts.disable_mdc {
        openpgp_cfb(&cipher, bs, &mut plain, false)?;
        out.extend(packet(9, &plain));
    } else {
        plain.extend_from_slice(&[0xD3, 0x14]);
        let mdc = sha1::Sha1::digest(&plain);
        plain.extend_from_slice(&mdc);
        cfb(&cipher, bs, &vec![0u8; bs], &mut plain, false);
        let mut body = vec![1u8];
        body.extend_from_slice(&plain);
        out.extend(packet(18, &body));
    }
    Ok(())
}

/// Where a message's session key comes from: a password (tag 3 packets)
/// or a secret key, which opens a tag 1 packet's encrypted session key.
/// Opens a tag 1 packet's session key: the cipher and the key.
pub(crate) type OpenSessionKey<'a> = &'a dyn Fn(&[u8]) -> Result<(Algo, Vec<u8>)>;

pub(crate) enum Source<'a> {
    Password(&'a [u8]),
    Key(OpenSessionKey<'a>),
}

/// The literal packet's type byte and data, after decryption.
pub(crate) fn decrypt(message: &[u8], source: Source<'_>) -> Result<(u8, Vec<u8>)> {
    let mut pos = 0;
    let mut key: Option<(Algo, Vec<u8>)> = None;
    while pos < message.len() {
        let (tag, body, next) = read_packet(message, pos)?;
        pos = next;
        match tag {
            3 => {
                let Source::Password(pass) = source else {
                    return Err(corrupt());
                };
                if body.first() != Some(&4) {
                    return Err(corrupt());
                }
                let algo = Algo::from_id(*body.get(1).ok_or_else(corrupt)?)?;
                let (s2k, used) = S2k::read(&body[2..])?;
                let s2k_key = s2k.derive(pass, algo.key_len())?;
                let esk = &body[2 + used..];
                key = Some(if esk.is_empty() {
                    (algo, s2k_key)
                } else {
                    let mut esk = esk.to_vec();
                    cfb(
                        &Cipher::new(algo, &s2k_key)?,
                        algo.block_size(),
                        &vec![0u8; algo.block_size()],
                        &mut esk,
                        true,
                    );
                    let inner = Algo::from_id(esk[0])?;
                    if esk.len() != 1 + inner.key_len() {
                        return Err(corrupt());
                    }
                    (inner, esk[1..].to_vec())
                });
            }
            1 => match &source {
                Source::Key(open) => {
                    if key.is_some() {
                        return Err(corrupt());
                    }
                    key = Some(open(&body)?);
                }
                // pgcrypto's own text for a public-key message given to the
                // password functions.
                Source::Password(_) => return Err(px("pgcrypto bug")),
            },
            9 | 18 => {
                let (algo, k) = key.take().ok_or_else(corrupt)?;
                let bs = algo.block_size();
                let cipher = Cipher::new(algo, &k)?;
                let mut data = if tag == 18 {
                    if body.first() != Some(&1) {
                        return Err(corrupt());
                    }
                    let mut d = body[1..].to_vec();
                    if d.len() < bs + 2 + 22 {
                        return Err(corrupt());
                    }
                    cfb(&cipher, bs, &vec![0u8; bs], &mut d, true);
                    d
                } else {
                    let mut d = body.clone();
                    openpgp_cfb(&cipher, bs, &mut d, true)?;
                    d
                };
                if data[bs - 2..bs] != data[bs..bs + 2] {
                    return Err(corrupt());
                }
                if tag == 18 {
                    let split = data.len() - 20;
                    if data[split - 2..split] != [0xD3, 0x14]
                        || sha1::Sha1::digest(&data[..split]).as_slice() != &data[split..]
                    {
                        return Err(corrupt());
                    }
                    data.truncate(split - 2);
                }
                return literal(&data[bs + 2..]);
            }
            _ => return Err(corrupt()),
        }
    }
    Err(corrupt())
}

/// The decrypted packets: a Literal Data packet, perhaps compressed.
pub(crate) fn literal(packets: &[u8]) -> Result<(u8, Vec<u8>)> {
    let (tag, body, _) = read_packet(packets, 0)?;
    match tag {
        8 => {
            let algo = *body.first().ok_or_else(corrupt)?;
            literal(&decompress(algo, &body[1..])?)
        }
        11 => {
            let kind = *body.first().ok_or_else(corrupt)?;
            let name_len = usize::from(*body.get(1).ok_or_else(corrupt)?);
            let start = 2 + name_len + 4;
            let data = body.get(start..).ok_or_else(corrupt)?.to_vec();
            Ok((kind, data))
        }
        _ => Err(corrupt()),
    }
}

// ---- ASCII armor ------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn base64(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub(crate) fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        let v = B64.iter().position(|b| *b == c)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub(crate) fn crc24(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xB704CE;
    for &b in data {
        crc ^= u32::from(b) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x100_0000 != 0 {
                crc ^= 0x186_4CFB;
            }
        }
    }
    crc & 0xFF_FFFF
}

pub(crate) fn armor(data: &[u8], headers: &[(String, String)]) -> String {
    let mut out = String::from("-----BEGIN PGP MESSAGE-----\n");
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\n"));
    }
    out.push('\n');
    let encoded = base64(data);
    for line in encoded.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(line).unwrap_or_default());
        out.push('\n');
    }
    let crc = crc24(data);
    out.push('=');
    out.push_str(&base64(&[(crc >> 16) as u8, (crc >> 8) as u8, crc as u8]));
    out.push_str("\n-----END PGP MESSAGE-----\n");
    out
}

pub(crate) fn dearmor(text: &str) -> Result<Vec<u8>> {
    let bad = || px("Corrupt ascii-armor");
    let start = text.find("-----BEGIN PGP ").ok_or_else(bad)?;
    let rest = &text[start..];
    let rest = &rest[rest.find('\n').ok_or_else(bad)? + 1..];
    // Headers run to the first empty line.
    let mut body_start = None;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim().is_empty() {
            body_start = Some(offset + line.len());
            break;
        }
        if !line.contains(':') {
            return Err(bad());
        }
        offset += line.len();
    }
    let body = &rest[body_start.ok_or_else(bad)?..];
    let end = body.find("-----END PGP ").ok_or_else(bad)?;
    let body = &body[..end];
    let (data, crc) = match body.rfind("\n=") {
        Some(i) => (&body[..i], Some(body[i + 2..].trim())),
        None => (body, None),
    };
    let bytes = unbase64(data).ok_or_else(bad)?;
    if let Some(crc) = crc {
        let c = unbase64(crc).ok_or_else(bad)?;
        if c.len() != 3 {
            return Err(bad());
        }
        let want = u32::from(c[0]) << 16 | u32::from(c[1]) << 8 | u32::from(c[2]);
        if want != crc24(&bytes) {
            return Err(bad());
        }
    }
    Ok(bytes)
}

/// `pgp_key_id`: `SYMKEY` for a password-encrypted message.
pub(crate) fn key_id(message: &[u8]) -> Result<String> {
    let (tag, body, _) = read_packet(message, 0)?;
    match tag {
        5 | 6 | 7 | 14 => crate::pgp_pub::key_data_id(message),
        3 => Ok("SYMKEY".into()),
        1 => {
            let id = body.get(1..9).ok_or_else(corrupt)?;
            if id.iter().all(|b| *b == 0) {
                return Ok("ANYKEY".into());
            }
            Ok(id.iter().map(|b| format!("{b:02X}")).collect())
        }
        _ => Err(px("Wrong key or corrupt data")),
    }
}

// ---- the SQL functions -------------------------------------------------

pub const FUNCTIONS: &[&str] = &[
    "pgp_sym_encrypt",
    "pgp_sym_encrypt_bytea",
    "pgp_sym_decrypt",
    "pgp_sym_decrypt_bytea",
    "pgp_key_id",
    "armor",
    "dearmor",
];

pub fn result_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "pgp_sym_decrypt" | "pgp_key_id" | "armor" => "text",
        "pgp_sym_encrypt" | "pgp_sym_encrypt_bytea" | "pgp_sym_decrypt_bytea" | "dearmor" => {
            "bytea"
        }
        _ => return None,
    })
}

pub(crate) fn bytes_of(v: &Bson) -> Vec<u8> {
    match v {
        Bson::Binary(b) => b.bytes.clone(),
        other => value_text(other).into_bytes(),
    }
}

pub(crate) fn bytea(bytes: Vec<u8>) -> Bson {
    Bson::Binary(bson::Binary {
        subtype: bson::spec::BinarySubtype::Generic,
        bytes,
    })
}

pub(crate) fn text_array(v: &Bson) -> Vec<String> {
    match v {
        Bson::Array(items) => items.iter().map(value_text).collect(),
        _ => Vec::new(),
    }
}

pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    FUNCTIONS.contains(&name).then(|| {
        if args.contains(&Bson::Null) {
            return Ok(Bson::Null);
        }
        let opts = || Options::parse(&args.get(2).map(value_text).unwrap_or_default());
        match (name, args.len()) {
            ("pgp_sym_encrypt", 2 | 3) => Ok(bytea(encrypt(
                value_text(&args[0]).as_bytes(),
                &bytes_of(&args[1]),
                &opts()?,
                true,
            )?)),
            ("pgp_sym_encrypt_bytea", 2 | 3) => Ok(bytea(encrypt(
                &bytes_of(&args[0]),
                &bytes_of(&args[1]),
                &opts()?,
                false,
            )?)),
            ("pgp_sym_decrypt", 2 | 3) => {
                let o = opts()?;
                let (kind, data) =
                    decrypt(&bytes_of(&args[0]), Source::Password(&bytes_of(&args[1])))?;
                if kind == b'b' {
                    return Err(px("Not text data"));
                }
                let data = if o.convert_crlf {
                    from_crlf(&data)
                } else {
                    data
                };
                String::from_utf8(data).map(Bson::String).map_err(|_| {
                    Error::Sqlstate(
                        "22021",
                        "invalid byte sequence for encoding \"UTF8\"".into(),
                    )
                })
            }
            ("pgp_sym_decrypt_bytea", 2 | 3) => {
                let o = opts()?;
                let (kind, data) =
                    decrypt(&bytes_of(&args[0]), Source::Password(&bytes_of(&args[1])))?;
                Ok(bytea(if o.convert_crlf && kind != b'b' {
                    from_crlf(&data)
                } else {
                    data
                }))
            }
            ("pgp_key_id", 1) => key_id(&bytes_of(&args[0])).map(Bson::String),
            ("armor", 1) => Ok(Bson::String(armor(&bytes_of(&args[0]), &[]))),
            ("armor", 3) => {
                let (keys, values) = (text_array(&args[1]), text_array(&args[2]));
                if keys.len() != values.len() {
                    return Err(Error::Sqlstate(
                        "2202E",
                        "mismatched array dimensions".into(),
                    ));
                }
                let headers: Vec<(String, String)> = keys.into_iter().zip(values).collect();
                Ok(Bson::String(armor(&bytes_of(&args[0]), &headers)))
            }
            ("dearmor", 1) => Ok(bytea(dearmor(&value_text(&args[0]))?)),
            _ => Err(Error::UndefinedFunction(format!(
                "function {name} does not exist"
            ))),
        }
    })
}
