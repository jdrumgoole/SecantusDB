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
use bson::Bson;
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

// ---- ordering and the network functions --------------------------------

fn bits_of(addr: IpAddr) -> (u128, u8) {
    match addr {
        IpAddr::V4(v4) => (u128::from(u32::from(v4)), 32),
        IpAddr::V6(v6) => (u128::from(v6), 128),
    }
}

fn addr_of(n: u128, width: u8) -> IpAddr {
    if width == 32 {
        IpAddr::V4(Ipv4Addr::from(n as u32))
    } else {
        IpAddr::V6(Ipv6Addr::from(n))
    }
}

/// The mask of the leading `bits` of a `width`-bit address.
fn mask(bits: u8, width: u8) -> u128 {
    let all = if width == 32 {
        u128::from(u32::MAX)
    } else {
        u128::MAX
    };
    if bits == 0 {
        return 0;
    }
    if bits >= width {
        return all;
    }
    all & !(all >> bits)
}

/// A string whose ORDER is PostgreSQL's `network_cmp`: IPv4 before IPv6;
/// then the network bits both values share, a shorter prefix first; then
/// the full address. One bit per character (`1` / `2`) with a `0`
/// terminator makes "a prefix sorts first" plain string order, and the
/// canonical value rides after `|` so an extreme can be read back.
pub fn sort_key(stored: &str) -> Option<String> {
    let (addr, bits) = parse_addr_bits(stored, false)?;
    let (n, width) = bits_of(addr);
    let mut key = String::with_capacity(usize::from(bits) + 40);
    key.push(if width == 32 { '4' } else { '6' });
    for i in 0..bits {
        let bit = (n >> (width - 1 - i)) & 1;
        key.push(if bit == 1 { '2' } else { '1' });
    }
    key.push('0');
    key.push_str(&format!("{:0w$x}", n, w = usize::from(width / 4)));
    key.push('|');
    key.push_str(stored);
    Some(key)
}

/// The network functions, by name: the result type from the arguments'
/// static types (`inet` / `cidr`), or `None` when `name` is not one.
pub fn result_type(name: &str, types: &[String]) -> Option<String> {
    let first = types.first().map(String::as_str).unwrap_or("");
    if !matches!(first, "inet" | "cidr") {
        return None;
    }
    Some(
        match name {
            "host" | "abbrev" | "text" => "text",
            "masklen" | "family" => "int4",
            "network" | "inet_merge" => "cidr",
            "broadcast" | "netmask" | "hostmask" => "inet",
            "set_masklen" => first,
            "inet_same_family" => "bool",
            _ => return None,
        }
        .to_string(),
    )
}

fn arg(v: &Bson) -> Result<(IpAddr, u8)> {
    match v {
        Bson::String(s) => parse_addr_bits(s, false).ok_or_else(|| {
            Error::InvalidText(format!("invalid input syntax for type inet: \"{s}\""))
        }),
        _ => Err(Error::Internal("a network value that is not text".into())),
    }
}

fn shown(addr: IpAddr, bits: u8) -> Bson {
    Bson::String(format!("{addr}/{bits}"))
}

/// Run one of the network functions (`result_type` names them).
pub fn call(name: &str, args: &[Bson], types: &[String]) -> Result<Bson> {
    if args.contains(&Bson::Null) {
        return Ok(Bson::Null);
    }
    let is_cidr = types.first().is_some_and(|t| t == "cidr");
    let (addr, bits) = arg(&args[0])?;
    let (n, width) = bits_of(addr);
    let full = |x: u128| {
        let a = addr_of(x, width);
        shown(a, width)
    };
    Ok(match name {
        "host" => Bson::String(addr.to_string()),
        "text" => Bson::String(format!("{addr}/{bits}")),
        "masklen" => Bson::Int32(i32::from(bits)),
        "family" => Bson::Int32(if width == 32 { 4 } else { 6 }),
        "network" => shown(addr_of(n & mask(bits, width), width), bits),
        "broadcast" => shown(
            addr_of(n | !mask(bits, width) & mask(width, width), width),
            bits,
        ),
        "netmask" => full(mask(bits, width)),
        "hostmask" => full(!mask(bits, width) & mask(width, width)),
        "abbrev" => {
            if is_cidr && width == 32 {
                // `cidr` abbreviates to the octets its mask covers.
                let octets = addr.to_string();
                let parts: Vec<&str> = octets.split('.').collect();
                let keep = usize::from(bits.div_ceil(8)).max(1);
                Bson::String(format!("{}/{bits}", parts[..keep.min(4)].join(".")))
            } else if !is_cidr && bits == width {
                Bson::String(addr.to_string())
            } else {
                Bson::String(format!("{addr}/{bits}"))
            }
        }
        "set_masklen" => {
            let requested = match &args[1] {
                Bson::Int32(i) => i64::from(*i),
                Bson::Int64(i) => *i,
                _ => return Err(Error::Internal("a masklen that is not an integer".into())),
            };
            let new = if requested == -1 {
                i64::from(width)
            } else {
                requested
            };
            if !(0..=i64::from(width)).contains(&new) {
                return Err(Error::InvalidParameter(format!(
                    "invalid mask length: {requested}"
                )));
            }
            let new = new as u8;
            if is_cidr {
                shown(addr_of(n & mask(new, width), width), new)
            } else {
                shown(addr, new)
            }
        }
        "inet_same_family" => {
            let (b, _) = arg(&args[1])?;
            Bson::Boolean(addr.is_ipv4() == b.is_ipv4())
        }
        "inet_merge" => {
            let (b, b_bits) = arg(&args[1])?;
            if addr.is_ipv4() != b.is_ipv4() {
                return Err(Error::InvalidParameter(
                    "cannot merge addresses from different families".into(),
                ));
            }
            let (m, _) = bits_of(b);
            let mut common = bits.min(b_bits);
            while common > 0 && (n & mask(common, width)) != (m & mask(common, width)) {
                common -= 1;
            }
            shown(addr_of(n & mask(common, width), width), common)
        }
        _ => return Err(Error::Internal(format!("{name} is not a network function"))),
    })
}

/// The containment operators: `<<` (is contained by), `<<=`, `>>`, `>>=`
/// and `&&` (overlaps). `None` for any other operator.
pub fn containment(op: &str, l: &str, r: &str) -> Option<Result<bool>> {
    if !matches!(op, "<<" | "<<=" | ">>" | ">>=" | "&&") {
        return None;
    }
    Some((|| {
        let (a, ab) = parse_addr_bits(l, false).ok_or_else(|| Error::Internal("inet".into()))?;
        let (b, bb) = parse_addr_bits(r, false).ok_or_else(|| Error::Internal("inet".into()))?;
        if a.is_ipv4() != b.is_ipv4() {
            return Ok(false);
        }
        let ((x, width), (y, _)) = (bits_of(a), bits_of(b));
        // Does network (p, pb) contain (q, qb)?
        let within = |p: u128, pb: u8, q: u128, qb: u8, strict: bool| {
            (if strict { qb > pb } else { qb >= pb })
                && (p & mask(pb, width)) == (q & mask(pb, width))
        };
        Ok(match op {
            "<<" => within(y, bb, x, ab, true),
            "<<=" => within(y, bb, x, ab, false),
            ">>" => within(x, ab, y, bb, true),
            ">>=" => within(x, ab, y, bb, false),
            _ => {
                let m = ab.min(bb);
                (x & mask(m, width)) == (y & mask(m, width))
            }
        })
    })())
}

/// The integer argument of `inet + n` / `inet - n`.
fn int_arg(v: &Bson) -> Result<i128> {
    match v {
        Bson::Int32(i) => Ok(i128::from(*i)),
        Bson::Int64(i) => Ok(i128::from(*i)),
        _ => Err(Error::Internal(
            "an inet offset that is not an integer".into(),
        )),
    }
}

fn out_of_range() -> Error {
    Error::NumericOutOfRange("result is out of range".into())
}

/// `inet + n`, `n + inet`, `inet - n`, `~inet`, `inet & inet`, `inet | inet`:
/// an `inet` (a `cidr` operand's result is an inet too). `l` is `None` for
/// the prefix `~`.
pub fn arith(op: &str, l: Option<&Bson>, r: &Bson) -> Result<Bson> {
    if l == Some(&Bson::Null) || *r == Bson::Null {
        return Ok(Bson::Null);
    }
    let is_net = |v: &Bson| matches!(v, Bson::String(_));
    match (op, l) {
        ("~", None) => {
            let (addr, bits) = arg(r)?;
            let (n, width) = bits_of(addr);
            Ok(shown(addr_of(!n & mask(width, width), width), bits))
        }
        ("+" | "-", Some(l)) => {
            let (net, offset, sign) = if is_net(l) {
                (l, r, if op == "-" { -1 } else { 1 })
            } else {
                (r, l, 1)
            };
            let (addr, bits) = arg(net)?;
            let (n, width) = bits_of(addr);
            let moved = i128::try_from(n).map_err(|_| out_of_range())? + sign * int_arg(offset)?;
            let max = i128::try_from(mask(width, width)).unwrap_or(i128::MAX);
            if moved < 0 || moved > max {
                return Err(out_of_range());
            }
            Ok(shown(addr_of(moved as u128, width), bits))
        }
        ("&" | "|", Some(l)) => {
            let ((a, ab), (b, bb)) = (arg(l)?, arg(r)?);
            if a.is_ipv4() != b.is_ipv4() {
                let what = if op == "&" { "AND" } else { "OR" };
                return Err(Error::InvalidParameter(format!(
                    "cannot {what} inet values of different sizes"
                )));
            }
            let ((x, width), (y, _)) = (bits_of(a), bits_of(b));
            let v = if op == "&" { x & y } else { x | y };
            Ok(shown(addr_of(v, width), ab.max(bb)))
        }
        _ => Err(Error::Internal(format!("inet operator {op}"))),
    }
}

/// `inet - inet`: the difference of the addresses, a `bigint`.
pub fn diff(l: &Bson, r: &Bson) -> Result<Bson> {
    if *l == Bson::Null || *r == Bson::Null {
        return Ok(Bson::Null);
    }
    let ((a, _), (b, _)) = (arg(l)?, arg(r)?);
    if a.is_ipv4() != b.is_ipv4() {
        return Err(Error::InvalidParameter(
            "cannot subtract inet values of different sizes".into(),
        ));
    }
    let ((x, _), (y, _)) = (bits_of(a), bits_of(b));
    let d = i128::try_from(x).map_err(|_| out_of_range())?
        - i128::try_from(y).map_err(|_| out_of_range())?;
    i64::try_from(d)
        .map(Bson::Int64)
        .map_err(|_| out_of_range())
}
