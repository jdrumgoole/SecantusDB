//! PostgreSQL network address types: `inet` (oid 869) and `cidr` (oid 650).
//!
//! Both are carried as the canonical `addr/masklen` text the Python server
//! stores (`src/secantus/sql/net.py`), so the two share one store. `inet` keeps
//! its host bits and always carries a masklen (a bare host is `/32` or `/128`);
//! `cidr` is strict — host bits below the netmask must be zero, or the value is
//! rejected. The `::text` cast (`network_show`) always shows the masklen, which
//! is exactly the stored form, so rendering is the identity on the stored string.
//!
//! The wire BINARY format (what psycopg uses for these oids) is
//! `[family(1)][bits(1)][is_cidr(1)][nb(1)][addr nb bytes]`, family 2 for IPv4
//! and 3 for IPv6, nb = 4 or 16. Error surface measured against PostgreSQL 14:
//! a malformed `inet` is `22P02 invalid input syntax for type inet: "…"`, and a
//! `cidr` with host bits set (or otherwise malformed) is `22P02 invalid cidr
//! value: "…"`.

use crate::{Error, Result};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// PostgreSQL's on-the-wire address family for IPv4 (`PGSQL_AF_INET`).
const PGSQL_AF_INET: u8 = 2;
/// …and IPv6 (`PGSQL_AF_INET6`).
const PGSQL_AF_INET6: u8 = 3;

/// A parsed network value: the address, its masklen, and whether it is `cidr`.
struct NetVal {
    addr: IpAddr,
    bits: u8,
    is_cidr: bool,
}

fn parse_addr_bits(s: &str, is_cidr: bool) -> Option<(IpAddr, u8)> {
    let s = s.trim();
    let (addr_part, mask_part) = match s.split_once('/') {
        Some((a, m)) => (a, Some(m)),
        None => (s, None),
    };
    let addr: IpAddr = addr_part.parse().ok()?;
    let max = if addr.is_ipv4() { 32u8 } else { 128u8 };
    let bits = match mask_part {
        Some(m) => {
            let b: u8 = m.parse().ok()?;
            if b > max {
                return None;
            }
            b
        }
        None => max,
    };
    // cidr rejects any host bit set below the netmask.
    if is_cidr && !host_bits_clear(addr, bits) {
        return None;
    }
    Some((addr, bits))
}

/// Are all bits below the `bits`-length prefix zero?
fn host_bits_clear(addr: IpAddr, bits: u8) -> bool {
    match addr {
        IpAddr::V4(v4) => {
            let n = u32::from(v4);
            if bits == 0 {
                return n == 0;
            }
            if bits >= 32 {
                return true;
            }
            n & (u32::MAX >> bits) == 0
        }
        IpAddr::V6(v6) => {
            let n = u128::from(v6);
            if bits == 0 {
                return n == 0;
            }
            if bits >= 128 {
                return true;
            }
            n & (u128::MAX >> bits) == 0
        }
    }
}

/// Normalise an `inet` literal to canonical `addr/masklen` text.
pub fn normalize_inet(s: &str) -> Result<String> {
    let (addr, bits) = parse_addr_bits(s, false).ok_or_else(|| {
        Error::InvalidText(format!("invalid input syntax for type inet: \"{s}\""))
    })?;
    Ok(format!("{addr}/{bits}"))
}

/// Normalise a `cidr` literal to canonical `network/masklen` text (strict).
pub fn normalize_cidr(s: &str) -> Result<String> {
    let (addr, bits) = parse_addr_bits(s, true)
        .ok_or_else(|| Error::InvalidText(format!("invalid cidr value: \"{s}\"")))?;
    Ok(format!("{addr}/{bits}"))
}

/// Re-parse a stored canonical value for wire encoding.
fn split_stored(s: &str, is_cidr: bool) -> Option<NetVal> {
    let (addr, bits) = parse_addr_bits(s, false)?;
    let _ = is_cidr;
    Some(NetVal {
        addr,
        bits,
        is_cidr,
    })
}

/// PostgreSQL's TEXT output for an inet/cidr COLUMN (`inet_out` / `cidr_out`):
/// `inet` drops the masklen when it is a full host (`/32` or `/128`), `cidr`
/// always keeps it. (This is NOT the `::text` cast, which is `network_show` and
/// always keeps the mask — that path renders the stored string verbatim.)
pub fn text_out(stored: &str, is_cidr: bool) -> String {
    if is_cidr {
        return stored.to_string();
    }
    match parse_addr_bits(stored, false) {
        Some((addr, bits)) => {
            let max = if addr.is_ipv4() { 32 } else { 128 };
            if bits == max {
                addr.to_string()
            } else {
                format!("{addr}/{bits}")
            }
        }
        None => stored.to_string(),
    }
}

/// Encode a stored `addr/masklen` value into PostgreSQL's binary wire layout.
pub fn to_wire(stored: &str, is_cidr: bool) -> Option<Vec<u8>> {
    let v = split_stored(stored, is_cidr)?;
    let mut out = Vec::with_capacity(8);
    match v.addr {
        IpAddr::V4(a) => {
            out.push(PGSQL_AF_INET);
            out.push(v.bits);
            out.push(u8::from(v.is_cidr));
            out.push(4);
            out.extend_from_slice(&a.octets());
        }
        IpAddr::V6(a) => {
            out.push(PGSQL_AF_INET6);
            out.push(v.bits);
            out.push(u8::from(v.is_cidr));
            out.push(16);
            out.extend_from_slice(&a.octets());
        }
    }
    Some(out)
}

/// Decode PostgreSQL's binary wire layout back to canonical `addr/masklen` text.
pub fn from_wire(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 4 {
        return None;
    }
    let family = bytes[0];
    let bits = bytes[1];
    let nb = bytes[3] as usize;
    let addr_bytes = bytes.get(4..4 + nb)?;
    let addr = match (family, nb) {
        (PGSQL_AF_INET, 4) => {
            let o: [u8; 4] = addr_bytes.try_into().ok()?;
            IpAddr::V4(Ipv4Addr::from(o))
        }
        (PGSQL_AF_INET6, 16) => {
            let o: [u8; 16] = addr_bytes.try_into().ok()?;
            IpAddr::V6(Ipv6Addr::from(o))
        }
        _ => return None,
    };
    Some(format!("{addr}/{bits}"))
}
