//! pgcrypto's raw cipher functions: `encrypt(data, key, type)`,
//! `decrypt(...)`, `encrypt_iv(data, key, iv, type)` and `decrypt_iv(...)`.
//!
//! `type` is `algo[-mode][/pad:padding]`: `aes` (alias `rijndael`), `bf`
//! (`blowfish`), `des`, `3des` and `cast5`; mode `cbc` (the default) or
//! `ecb`; padding `pkcs` (the default) or `none`. The key is cut to the
//! cipher's largest size and zero-padded to the size it selects (AES picks
//! 128, 192 or 256 bits by the key's length); the IV is cut or zero-padded
//! to one block, and is all zeros when not given -- as `px.c` does it.
//! Deterministic, so it is compared byte for byte with PostgreSQL.

use super::*;
use crate::pgp::Cipher;
use aes::cipher::KeyInit;

fn no_cipher(spec: &str) -> Error {
    Error::Sqlstate(
        "22023",
        format!("Cannot use \"{spec}\": No such cipher algorithm"),
    )
}

struct Spec {
    cipher: Cipher,
    block: usize,
    cbc: bool,
    pad: bool,
}

fn padded(key: &[u8], size: usize) -> Vec<u8> {
    let mut k = key[..key.len().min(size)].to_vec();
    k.resize(size, 0);
    k
}

fn parse(spec: &str, key: &[u8]) -> Result<Spec> {
    let lower = spec.to_ascii_lowercase();
    let mut parts = lower.split('/');
    let head = parts.next().unwrap_or_default();
    let (algo, mode) = match head.split_once('-') {
        Some((a, m)) => (a, m),
        None => (head, "cbc"),
    };
    let cbc = match mode {
        "cbc" => true,
        "ecb" => false,
        _ => return Err(no_cipher(spec)),
    };
    let mut pad = true;
    for opt in parts {
        match opt.split_once(':') {
            Some(("pad", "pkcs")) => pad = true,
            Some(("pad", "none")) => pad = false,
            Some(("pad", _)) => return Err(no_cipher(spec)),
            _ => {
                return Err(Error::Sqlstate(
                    "22023",
                    format!("Cannot use \"{spec}\": Unknown option"),
                ))
            }
        }
    }
    let init = || {
        Error::Sqlstate(
            "39000",
            "encrypt error: Cipher cannot be initialized".into(),
        )
    };
    let (cipher, block) = match algo {
        "aes" | "rijndael" => {
            let k = &key[..key.len().min(32)];
            let size = if k.len() <= 16 {
                16
            } else if k.len() <= 24 {
                24
            } else {
                32
            };
            let k = padded(k, size);
            let c = match size {
                16 => Cipher::Aes128(Box::new(
                    aes::Aes128::new_from_slice(&k).map_err(|_| init())?,
                )),
                24 => Cipher::Aes192(Box::new(
                    aes::Aes192::new_from_slice(&k).map_err(|_| init())?,
                )),
                _ => Cipher::Aes256(Box::new(
                    aes::Aes256::new_from_slice(&k).map_err(|_| init())?,
                )),
            };
            (c, 16)
        }
        "bf" | "blowfish" => {
            let k = &key[..key.len().min(56)];
            (
                Cipher::Blowfish(Box::new(
                    blowfish::Blowfish::new_from_slice(k).map_err(|_| init())?,
                )),
                8,
            )
        }
        "cast5" => {
            // CAST5 takes 40 to 128 bits; a shorter key is zero-padded.
            let mut k = key[..key.len().min(16)].to_vec();
            if k.len() < 5 {
                k.resize(5, 0);
            }
            (
                Cipher::Cast5(Box::new(
                    cast5::Cast5::new_from_slice(&k).map_err(|_| init())?,
                )),
                8,
            )
        }
        "des" => (
            Cipher::Des(Box::new(
                des::Des::new_from_slice(&padded(key, 8)).map_err(|_| init())?,
            )),
            8,
        ),
        "3des" => (
            Cipher::TripleDes(Box::new(
                des::TdesEde3::new_from_slice(&padded(key, 24)).map_err(|_| init())?,
            )),
            8,
        ),
        _ => return Err(no_cipher(spec)),
    };
    Ok(Spec {
        cipher,
        block,
        cbc,
        pad,
    })
}

fn encrypt(data: &[u8], key: &[u8], iv: &[u8], spec: &str) -> Result<Vec<u8>> {
    let s = parse(spec, key)?;
    let bs = s.block;
    let mut buf = data.to_vec();
    if s.pad {
        let n = bs - buf.len() % bs;
        buf.extend(std::iter::repeat_n(n as u8, n));
    } else if !buf.len().is_multiple_of(bs) {
        return Err(Error::Sqlstate(
            "39000",
            "encrypt error: Encryption failed".into(),
        ));
    }
    let mut prev = padded(iv, bs);
    for block in buf.chunks_mut(bs) {
        if s.cbc {
            for (b, p) in block.iter_mut().zip(&prev) {
                *b ^= p;
            }
        }
        s.cipher.encrypt_block(block);
        prev = block.to_vec();
    }
    Ok(buf)
}

fn decrypt(data: &[u8], key: &[u8], iv: &[u8], spec: &str) -> Result<Vec<u8>> {
    let s = parse(spec, key)?;
    let bs = s.block;
    let failed = || Error::Sqlstate("39000", "decrypt error: Decryption failed".into());
    if !data.len().is_multiple_of(bs) {
        return Err(failed());
    }
    let mut buf = data.to_vec();
    let mut prev = padded(iv, bs);
    for block in buf.chunks_mut(bs) {
        let cipher_block = block.to_vec();
        s.cipher.decrypt_block(block);
        if s.cbc {
            for (b, p) in block.iter_mut().zip(&prev) {
                *b ^= p;
            }
        }
        prev = cipher_block;
    }
    if s.pad {
        let n = usize::from(*buf.last().ok_or_else(failed)?);
        if n == 0
            || n > bs
            || n > buf.len()
            || buf[buf.len() - n..].iter().any(|b| usize::from(*b) != n)
        {
            return Err(failed());
        }
        buf.truncate(buf.len() - n);
    }
    Ok(buf)
}

pub const FUNCTIONS: &[&str] = &["encrypt", "decrypt", "encrypt_iv", "decrypt_iv"];

fn bytes_of(v: &Bson) -> Vec<u8> {
    match v {
        Bson::Binary(b) => b.bytes.clone(),
        other => value_text(other).into_bytes(),
    }
}

pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    FUNCTIONS.contains(&name).then(|| {
        if args.contains(&Bson::Null) {
            return Ok(Bson::Null);
        }
        let out = match (name, args.len()) {
            ("encrypt", 3) => encrypt(
                &bytes_of(&args[0]),
                &bytes_of(&args[1]),
                &[],
                &value_text(&args[2]),
            )?,
            ("decrypt", 3) => decrypt(
                &bytes_of(&args[0]),
                &bytes_of(&args[1]),
                &[],
                &value_text(&args[2]),
            )?,
            ("encrypt_iv", 4) => encrypt(
                &bytes_of(&args[0]),
                &bytes_of(&args[1]),
                &bytes_of(&args[2]),
                &value_text(&args[3]),
            )?,
            ("decrypt_iv", 4) => decrypt(
                &bytes_of(&args[0]),
                &bytes_of(&args[1]),
                &bytes_of(&args[2]),
                &value_text(&args[3]),
            )?,
            _ => {
                return Err(Error::UndefinedFunction(format!(
                    "function {name} does not exist"
                )))
            }
        };
        Ok(Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::Generic,
            bytes: out,
        }))
    })
}
