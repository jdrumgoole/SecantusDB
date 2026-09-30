//! pgcrypto's PGP functions over a KEY: `pgp_pub_encrypt[_bytea]`,
//! `pgp_pub_decrypt[_bytea]`, and `pgp_key_id` over key data.
//!
//! Keys are OpenPGP v4 key data, as `gpg --export` / `--export-secret-keys`
//! write it. pgcrypto encrypts to exactly one SUBKEY: the primary key is
//! skipped whatever its algorithm, a second primary is "Several keys given",
//! a second encryption subkey "Several subkeys not supported". RSA (1, 2)
//! and Elgamal (16) encrypt; a secret key may be protected (S2K usage 254,
//! SHA-1 check, or 255, a 16-bit checksum). The session key rides a tag 1
//! packet in PKCS#1 v1.5 encoding; the body is the same as the password
//! functions' (`pgp::seal`). Every rule and message here was measured on
//! PostgreSQL 15 with keys PostgreSQL accepted.

use super::*;
use crate::pgp::{corrupt, packet, px, random, read_packet, Algo, Cipher, Options, S2k, Source};
use num_bigint::BigUint;
use sha1::Digest;

enum Material {
    Rsa {
        n: BigUint,
        e: BigUint,
        d: Option<BigUint>,
    },
    Elgamal {
        p: BigUint,
        g: BigUint,
        y: BigUint,
        x: Option<BigUint>,
    },
}

struct Key {
    id: [u8; 8],
    algo: u8,
    material: Material,
}

fn read_mpi(buf: &[u8], pos: &mut usize) -> Result<BigUint> {
    let bits = buf.get(*pos..*pos + 2).ok_or_else(corrupt)?;
    let bits = usize::from(bits[0]) << 8 | usize::from(bits[1]);
    let len = bits.div_ceil(8);
    let bytes = buf.get(*pos + 2..*pos + 2 + len).ok_or_else(corrupt)?;
    *pos += 2 + len;
    Ok(BigUint::from_bytes_be(bytes))
}

fn mpi(n: &BigUint) -> Vec<u8> {
    let bits = n.bits() as u16;
    let mut out = bits.to_be_bytes().to_vec();
    if bits > 0 {
        out.extend(n.to_bytes_be());
    }
    out
}

fn can_encrypt(algo: u8) -> bool {
    matches!(algo, 1 | 2 | 16)
}

/// A key packet's public part: its algorithm, MPIs, and the length of the
/// public part (which a secret packet continues past).
fn public_part(body: &[u8]) -> Result<(u8, Vec<BigUint>, usize)> {
    if body.first() != Some(&4) {
        return Err(px("Unsupported public key version"));
    }
    let algo = *body.get(5).ok_or_else(corrupt)?;
    let count = match algo {
        1..=3 => 2,
        16 | 20 => 3,
        17 => 4,
        _ => return Err(px("Unknown public-key encryption algorithm")),
    };
    let mut pos = 6;
    let mut mpis = Vec::with_capacity(count);
    for _ in 0..count {
        mpis.push(read_mpi(body, &mut pos)?);
    }
    Ok((algo, mpis, pos))
}

/// The v4 key id: the low 64 bits of the SHA-1 fingerprint.
fn key_id_of(public: &[u8]) -> [u8; 8] {
    let mut h = sha1::Sha1::new();
    h.update([0x99]);
    h.update((public.len() as u16).to_be_bytes());
    h.update(public);
    let fp = h.finalize();
    let mut id = [0u8; 8];
    id.copy_from_slice(&fp[12..20]);
    id
}

/// A secret subkey's secret MPIs, decrypted with `psw` when protected.
fn secret_mpis(body: &[u8], from: usize, algo: u8, psw: Option<&[u8]>) -> Result<Vec<BigUint>> {
    let usage = *body.get(from).ok_or_else(corrupt)?;
    let plain = match usage {
        0 => body[from + 1..].to_vec(),
        254 | 255 => {
            let cipher = Algo::from_id(*body.get(from + 1).ok_or_else(corrupt)?)?;
            let (s2k, used) = S2k::read(body.get(from + 2..).ok_or_else(corrupt)?)?;
            let iv_at = from + 2 + used;
            let bs = cipher.block_size();
            let iv = body.get(iv_at..iv_at + bs).ok_or_else(corrupt)?;
            let psw = psw.ok_or_else(|| px("Need password for secret key"))?;
            let key = s2k.derive(psw, cipher.key_len())?;
            let mut data = body[iv_at + bs..].to_vec();
            crate::pgp::cfb(&Cipher::new(cipher, &key)?, bs, iv, &mut data, true);
            if usage == 254 {
                if data.len() < 20 {
                    return Err(corrupt());
                }
                let split = data.len() - 20;
                if sha1::Sha1::digest(&data[..split]).as_slice() != &data[split..] {
                    return Err(corrupt());
                }
                data.truncate(split);
            }
            data
        }
        _ => return Err(px("Unsupported cipher algorithm")),
    };
    let count = match algo {
        1..=3 => 4,
        16 | 20 => 1,
        17 => 1,
        _ => return Err(corrupt()),
    };
    let mut pos = 0;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(read_mpi(&plain, &mut pos)?);
    }
    // The clear form and usage 255 end in a 16-bit sum of the MPI bytes.
    if usage != 254 {
        let sum = plain[..pos]
            .iter()
            .fold(0u16, |a, b| a.wrapping_add(u16::from(*b)));
        let stored = plain.get(pos..pos + 2).ok_or_else(corrupt)?;
        if sum != u16::from_be_bytes([stored[0], stored[1]]) {
            return Err(corrupt());
        }
    }
    Ok(out)
}

/// `pgp_set_pubkey`: the one encryption subkey in `data`.
fn load_key(data: &[u8], want_secret: bool, psw: Option<&[u8]>) -> Result<Key> {
    let mut pos = 0;
    let mut got_main = false;
    let mut found: Option<Key> = None;
    while pos < data.len() {
        let (tag, body, next) = read_packet(data, pos)?;
        pos = next;
        match tag {
            5 | 6 => {
                if got_main {
                    return Err(px("Several keys given - pgcrypto does not handle keyring"));
                }
                got_main = true;
            }
            14 | 7 => {
                if tag == 14 && want_secret {
                    return Err(px("Cannot decrypt with public key"));
                }
                if tag == 7 && !want_secret {
                    return Err(px("Refusing to encrypt with secret key"));
                }
                let (algo, mpis, len) = public_part(&body)?;
                if !can_encrypt(algo) {
                    continue;
                }
                if found.is_some() {
                    return Err(px("Several subkeys not supported"));
                }
                let secret = if tag == 7 {
                    Some(secret_mpis(&body, len, algo, psw)?)
                } else {
                    None
                };
                let material = match algo {
                    1 | 2 => Material::Rsa {
                        n: mpis[0].clone(),
                        e: mpis[1].clone(),
                        d: secret.map(|s| s[0].clone()),
                    },
                    _ => Material::Elgamal {
                        p: mpis[0].clone(),
                        g: mpis[1].clone(),
                        y: mpis[2].clone(),
                        x: secret.map(|s| s[0].clone()),
                    },
                };
                found = Some(Key {
                    id: key_id_of(&body[..len]),
                    algo,
                    material,
                });
            }
            // Signatures, markers, trust, user ids and attributes.
            2 | 10 | 12 | 13 | 17 | 61 => {}
            _ => return Err(px("Unexpected packet in key data")),
        }
    }
    found.ok_or_else(|| px("No encryption key found"))
}

fn random_nonzero(n: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        out.extend(random(n)?.into_iter().filter(|b| *b != 0));
    }
    out.truncate(n);
    Ok(out)
}

/// A random integer in `1..limit`.
fn random_below(limit: &BigUint) -> Result<BigUint> {
    let bytes = (limit.bits() as usize).div_ceil(8) + 8;
    let r = BigUint::from_bytes_be(&random(bytes)?);
    Ok(r % (limit - 1u32) + 1u32)
}

fn encrypt(data: &[u8], key_data: &[u8], opts: &Options, text: bool) -> Result<Vec<u8>> {
    let key = load_key(key_data, false, None)?;
    let algo = opts.cipher;
    let session = random(algo.key_len())?;
    let mut message = vec![algo.id()];
    message.extend_from_slice(&session);
    let sum = session
        .iter()
        .fold(0u16, |a, b| a.wrapping_add(u16::from(*b)));
    message.extend(sum.to_be_bytes());
    let modulus = match &key.material {
        Material::Rsa { n, .. } => n,
        Material::Elgamal { p, .. } => p,
    };
    // EME-PKCS1-v1_5, as an integer (the leading zero byte is implicit).
    let k = (modulus.bits() as usize).div_ceil(8);
    let ps = k
        .checked_sub(3 + message.len())
        .filter(|n| *n >= 8)
        .ok_or_else(corrupt)?;
    let mut padded = vec![2u8];
    padded.extend(random_nonzero(ps)?);
    padded.push(0);
    padded.extend(&message);
    let m = BigUint::from_bytes_be(&padded);
    let mut body = vec![3u8];
    body.extend_from_slice(&key.id);
    body.push(key.algo);
    match &key.material {
        Material::Rsa { n, e, .. } => body.extend(mpi(&m.modpow(e, n))),
        Material::Elgamal { p, g, y, .. } => {
            let kk = random_below(&(p - 1u32))?;
            body.extend(mpi(&g.modpow(&kk, p)));
            body.extend(mpi(&(y.modpow(&kk, p) * &m % p)));
        }
    }
    let mut out = packet(1, &body);
    crate::pgp::seal(&mut out, algo, &session, data, opts, text)?;
    Ok(out)
}

fn decrypt(message: &[u8], key_data: &[u8], psw: Option<&[u8]>) -> Result<(u8, Vec<u8>)> {
    let key = load_key(key_data, true, psw)?;
    let open = |body: &[u8]| -> Result<(Algo, Vec<u8>)> {
        if body.first() != Some(&3) {
            return Err(corrupt());
        }
        let id = body.get(1..9).ok_or_else(corrupt)?;
        if id != key.id && id.iter().any(|b| *b != 0) {
            return Err(px("Wrong key"));
        }
        let mut pos = 10;
        let m = match &key.material {
            Material::Rsa { n, d: Some(d), .. } => read_mpi(body, &mut pos)?.modpow(d, n),
            Material::Elgamal { p, x: Some(x), .. } => {
                let a = read_mpi(body, &mut pos)?;
                let b = read_mpi(body, &mut pos)?;
                let exp = p - 1u32 - x;
                b * a.modpow(&exp, p) % p
            }
            _ => return Err(corrupt()),
        };
        let bytes = m.to_bytes_be();
        if bytes.first() != Some(&2) {
            return Err(corrupt());
        }
        let sep = bytes[1..]
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(corrupt)?
            + 1;
        let rest = &bytes[sep + 1..];
        let algo = Algo::from_id(*rest.first().ok_or_else(corrupt)?)?;
        if rest.len() != 1 + algo.key_len() + 2 {
            return Err(corrupt());
        }
        let session = &rest[1..1 + algo.key_len()];
        let sum = session
            .iter()
            .fold(0u16, |a, b| a.wrapping_add(u16::from(*b)));
        if sum.to_be_bytes() != rest[1 + algo.key_len()..] {
            return Err(corrupt());
        }
        Ok((algo, session.to_vec()))
    };
    crate::pgp::decrypt(message, Source::Key(&open))
}

/// `pgp_key_id` over key data: the encryption subkey's id.
pub(crate) fn key_data_id(data: &[u8]) -> Result<String> {
    let several = || px("Several keys given - pgcrypto does not handle keyring");
    let (mut main, mut public, mut secret) = (false, false, false);
    let mut id: Option<[u8; 8]> = None;
    let mut pos = 0;
    while pos < data.len() {
        let (tag, body, next) = read_packet(data, pos)?;
        pos = next;
        match tag {
            5 | 6 => {
                if main {
                    return Err(several());
                }
                main = true;
            }
            14 | 7 => {
                let seen = if tag == 14 { &mut public } else { &mut secret };
                if *seen {
                    return Err(several());
                }
                *seen = true;
                let (algo, _, len) = public_part(&body)?;
                if can_encrypt(algo) {
                    id = Some(key_id_of(&body[..len]));
                }
            }
            2 | 10 | 12 | 13 | 17 | 61 => {}
            _ => return Err(px("Unexpected packet in key data")),
        }
    }
    if public && secret {
        return Err(several());
    }
    id.map(|id| id.iter().map(|b| format!("{b:02X}")).collect())
        .ok_or_else(|| px("No encryption key found"))
}

pub const FUNCTIONS: &[&str] = &[
    "pgp_pub_encrypt",
    "pgp_pub_encrypt_bytea",
    "pgp_pub_decrypt",
    "pgp_pub_decrypt_bytea",
];

pub fn result_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "pgp_pub_decrypt" => "text",
        "pgp_pub_encrypt" | "pgp_pub_encrypt_bytea" | "pgp_pub_decrypt_bytea" => "bytea",
        _ => return None,
    })
}

fn bytes_of(v: &Bson) -> Vec<u8> {
    match v {
        Bson::Binary(b) => b.bytes.clone(),
        other => value_text(other).into_bytes(),
    }
}

fn bytea(bytes: Vec<u8>) -> Bson {
    Bson::Binary(bson::Binary {
        subtype: bson::spec::BinarySubtype::Generic,
        bytes,
    })
}

pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    FUNCTIONS.contains(&name).then(|| {
        if args.contains(&Bson::Null) {
            return Ok(Bson::Null);
        }
        let opts_at = |i: usize| Options::parse(&args.get(i).map(value_text).unwrap_or_default());
        let psw = args.get(2).map(bytes_of);
        match (name, args.len()) {
            ("pgp_pub_encrypt", 2 | 3) => Ok(bytea(encrypt(
                value_text(&args[0]).as_bytes(),
                &bytes_of(&args[1]),
                &opts_at(2)?,
                true,
            )?)),
            ("pgp_pub_encrypt_bytea", 2 | 3) => Ok(bytea(encrypt(
                &bytes_of(&args[0]),
                &bytes_of(&args[1]),
                &opts_at(2)?,
                false,
            )?)),
            ("pgp_pub_decrypt" | "pgp_pub_decrypt_bytea", 2..=4) => {
                let o = opts_at(3)?;
                let (kind, data) =
                    decrypt(&bytes_of(&args[0]), &bytes_of(&args[1]), psw.as_deref())?;
                if name == "pgp_pub_decrypt_bytea" {
                    return Ok(bytea(if o.convert_crlf && kind != b'b' {
                        crate::pgp::from_crlf(&data)
                    } else {
                        data
                    }));
                }
                if kind == b'b' {
                    return Err(px("Not text data"));
                }
                let data = if o.convert_crlf {
                    crate::pgp::from_crlf(&data)
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
            _ => Err(Error::UndefinedFunction(format!(
                "function {name} does not exist"
            ))),
        }
    })
}
