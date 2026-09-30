//! The `pgcrypto` extension's hashing and password functions: `digest`,
//! `hmac`, `crypt`, `gen_salt` and `gen_random_bytes`. They exist only once
//! `CREATE EXTENSION pgcrypto` has run, as on PostgreSQL -- before that a call
//! is `42883`. Messages and SQLSTATEs measured on PostgreSQL 14.
//!
//! `crypt` / `gen_salt` cover the four algorithms pgcrypto has: `bf`
//! (bcrypt, `$2a$`), `md5` (`$1$`), `xdes` (BSDi extended DES, `_`) and `des`
//! (traditional two-character salt). The PGP functions are not here.

use super::*;

/// The functions, by name.
pub const FUNCTIONS: &[&str] = &["digest", "hmac", "crypt", "gen_salt", "gen_random_bytes"];

pub fn is_function(name: &str) -> bool {
    FUNCTIONS.contains(&name) && extension_installed("pgcrypto")
}

pub fn result_type(name: &str) -> Option<&'static str> {
    if !is_function(name) {
        return None;
    }
    Some(match name {
        "crypt" | "gen_salt" => "text",
        _ => "bytea",
    })
}

fn int_of(v: &Bson) -> Result<i64> {
    match v {
        Bson::Int32(i) => Ok(i64::from(*i)),
        Bson::Int64(i) => Ok(*i),
        Bson::Double(d) if d.fract() == 0.0 => Ok(*d as i64),
        other => value_text(other).trim().parse().map_err(|_| {
            Error::Sqlstate(
                "22P02",
                format!(
                    "invalid input syntax for type integer: \"{}\"",
                    value_text(other)
                ),
            )
        }),
    }
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

fn no_algorithm(alg: &str) -> Error {
    Error::Sqlstate(
        "22023",
        format!("Cannot use \"{alg}\": No such hash algorithm"),
    )
}

fn hash(alg: &str, data: &[u8]) -> Result<Vec<u8>> {
    use sha2::Digest;
    Ok(match alg.to_ascii_lowercase().as_str() {
        "md5" => md5::Md5::digest(data).to_vec(),
        "sha1" => sha1::Sha1::digest(data).to_vec(),
        "sha224" => sha2::Sha224::digest(data).to_vec(),
        "sha256" => sha2::Sha256::digest(data).to_vec(),
        "sha384" => sha2::Sha384::digest(data).to_vec(),
        "sha512" => sha2::Sha512::digest(data).to_vec(),
        _ => return Err(no_algorithm(alg)),
    })
}

fn hmac_of(alg: &str, data: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    use hmac::{Hmac, Mac};
    macro_rules! mac {
        ($d:ty) => {{
            let mut m = <Hmac<$d> as Mac>::new_from_slice(key)
                .map_err(|e| Error::Internal(e.to_string()))?;
            m.update(data);
            m.finalize().into_bytes().to_vec()
        }};
    }
    Ok(match alg.to_ascii_lowercase().as_str() {
        "md5" => mac!(md5::Md5),
        "sha1" => mac!(sha1::Sha1),
        "sha224" => mac!(sha2::Sha224),
        "sha256" => mac!(sha2::Sha256),
        "sha384" => mac!(sha2::Sha384),
        "sha512" => mac!(sha2::Sha512),
        _ => return Err(no_algorithm(alg)),
    })
}

fn random(n: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; n];
    getrandom::getrandom(&mut out).map_err(|e| Error::Internal(e.to_string()))?;
    Ok(out)
}

/// The crypt(3) salt alphabet.
const ITOA64: &[u8] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn salt_chars(n: usize) -> Result<String> {
    Ok(random(n)?
        .into_iter()
        .map(|b| ITOA64[(b & 0x3f) as usize] as char)
        .collect())
}

fn gen_salt(kind: &str, rounds: Option<i64>) -> Result<String> {
    let bad_rounds = || Error::Sqlstate("22023", "gen_salt: Incorrect number of rounds".into());
    match kind.to_ascii_lowercase().as_str() {
        "bf" => {
            let r = rounds.unwrap_or(6);
            if !(4..=31).contains(&r) {
                return Err(bad_rounds());
            }
            // bcrypt's 16 random bytes in its own base64.
            let raw = random(16)?;
            Ok(format!("$2a${r:02}${}", bcrypt_base64(&raw)))
        }
        "md5" => {
            if rounds.is_some() {
                return Err(bad_rounds());
            }
            Ok(format!("$1${}", salt_chars(8)?))
        }
        "xdes" => {
            let r = rounds.unwrap_or(725);
            if !(1..=16_777_215).contains(&r) || r % 2 == 0 {
                return Err(bad_rounds());
            }
            let mut s = String::from("_");
            for i in 0..4 {
                s.push(ITOA64[((r >> (6 * i)) & 0x3f) as usize] as char);
            }
            s.push_str(&salt_chars(4)?);
            Ok(s)
        }
        "des" => {
            if rounds.is_some() {
                return Err(bad_rounds());
            }
            salt_chars(2)
        }
        _ => Err(Error::Sqlstate(
            "22023",
            "gen_salt: Unknown salt algorithm".into(),
        )),
    }
}

fn bcrypt_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"./ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
        }
    }
    out.truncate(22);
    out
}

// md5 / extended-DES / DES crypt are legacy, and pgcrypto offers them anyway:
// a stored hash in one of them must still verify.
#[allow(deprecated)]
fn crypt(password: &str, salt: &str) -> Result<String> {
    let bad = || Error::Sqlstate("22023", "invalid salt".into());
    let out = if salt.starts_with("$2a$") || salt.starts_with("$2b$") || salt.starts_with("$2y$") {
        // bcrypt reads only the 29-character setting.
        let setting: String = salt.chars().take(29).collect();
        pwhash::bcrypt::hash_with(
            pwhash::bcrypt::BcryptSetup {
                salt: Some(&setting[7..]),
                cost: setting[4..6].parse().ok(),
                variant: Some(pwhash::bcrypt::BcryptVariant::V2a),
            },
            password,
        )
        .map_err(|_| bad())?
    } else if salt.starts_with("$1$") {
        pwhash::md5_crypt::hash_with(salt, password).map_err(|_| bad())?
    } else if salt.starts_with('_') {
        pwhash::bsdi_crypt::hash_with(salt, password).map_err(|_| bad())?
    } else {
        pwhash::unix_crypt::hash_with(salt, password).map_err(|_| bad())?
    };
    Ok(out)
}

/// Run one of the functions, or `None` when `name` is not one of them.
pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    if !is_function(name) {
        return None;
    }
    if args.contains(&Bson::Null) {
        return Some(Ok(Bson::Null));
    }
    let text = |i: usize| args.get(i).map(value_text).unwrap_or_default();
    Some((|| -> Result<Bson> {
        match (name, args.len()) {
            ("digest", 2) => Ok(bytea(hash(&text(1), &bytes_of(&args[0]))?)),
            ("hmac", 3) => Ok(bytea(hmac_of(
                &text(2),
                &bytes_of(&args[0]),
                &bytes_of(&args[1]),
            )?)),
            ("gen_random_bytes", 1) => {
                let n = int_of(&args[0])?;
                if !(1..=1024).contains(&n) {
                    return Err(Error::Sqlstate("39000", "Length not in range".into()));
                }
                Ok(bytea(random(n as usize)?))
            }
            ("gen_salt", 1) => Ok(Bson::String(gen_salt(&text(0), None)?)),
            ("gen_salt", 2) => Ok(Bson::String(gen_salt(&text(0), Some(int_of(&args[1])?))?)),
            ("crypt", 2) => Ok(Bson::String(crypt(&text(0), &text(1))?)),
            _ => Err(Error::UndefinedFunction(format!(
                "function {name} does not exist"
            ))),
        }
    })())
}
