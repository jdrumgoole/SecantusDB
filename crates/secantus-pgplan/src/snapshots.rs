//! The snapshot functions over `pg_snapshot` / `txid_snapshot`:
//! `pg_snapshot_xmin` / `_xmax` / `_xip`, `pg_visible_in_snapshot`,
//! `pg_current_snapshot`, and their `txid_` twins (which answer `int8` where
//! the `pg_` ones answer `xid8`). Measured on PostgreSQL 15.
//!
//! A snapshot is held in its canonical text (`xmin:xmax:xip,...`, see
//! [`crate::systypes::parse_snapshot`]).
//!
//! This server exposes no transaction ids, so the CURRENT snapshot is the
//! empty one at the first normal xid (`3:3:`): nothing in progress, every
//! xid below 3 -- the bootstrap and frozen ones -- visible.

use bson::Bson;

use crate::{Error, Result};

/// Scalar snapshot functions (`pg_snapshot_xip` / `txid_snapshot_xip` are
/// set-returning, and live with the FROM functions).
pub const FUNCTIONS: &[&str] = &[
    "pg_snapshot_xmin",
    "pg_snapshot_xmax",
    "txid_snapshot_xmin",
    "txid_snapshot_xmax",
    "pg_visible_in_snapshot",
    "txid_visible_in_snapshot",
    "pg_current_snapshot",
    "txid_current_snapshot",
];

/// The set-returning ones.
pub const SET_FUNCTIONS: &[&str] = &["pg_snapshot_xip", "txid_snapshot_xip"];

const CURRENT: &str = "3:3:";

pub fn result_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "pg_snapshot_xmin" | "pg_snapshot_xmax" | "pg_snapshot_xip" => "xid8",
        "txid_snapshot_xmin" | "txid_snapshot_xmax" | "txid_snapshot_xip" => "int8",
        "pg_visible_in_snapshot" | "txid_visible_in_snapshot" => "bool",
        "pg_current_snapshot" => "pg_snapshot",
        "txid_current_snapshot" => "txid_snapshot",
        _ => return None,
    })
}

/// `(xmin, xmax, xip)` of a snapshot value.
pub fn parts(value: &Bson) -> Result<(u64, u64, Vec<u64>)> {
    let canonical = crate::systypes::parse_snapshot(&crate::value_text(value), "pg_snapshot")?;
    let mut it = canonical.splitn(3, ':');
    let num = |s: Option<&str>| s.and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let xmin = num(it.next());
    let xmax = num(it.next());
    let xip = it
        .next()
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u64>().ok())
        .collect();
    Ok((xmin, xmax, xip))
}

/// One xid as the function's result type.
pub fn xid_value(name: &str, xid: u64) -> Bson {
    if name.starts_with("txid_") {
        Bson::Int64(i64::try_from(xid).unwrap_or(i64::MAX))
    } else {
        crate::numeric::numeric_bson(&xid.to_string())
    }
}

pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    if !FUNCTIONS.contains(&name) {
        return None;
    }
    Some(eval(name, args))
}

fn eval(name: &str, args: &[Bson]) -> Result<Bson> {
    let arity = |n: usize| -> Result<()> {
        if args.len() == n {
            Ok(())
        } else {
            Err(Error::UndefinedFunction(format!(
                "function {name} does not exist"
            )))
        }
    };
    match name {
        "pg_current_snapshot" => {
            arity(0)?;
            Ok(Bson::String(CURRENT.into()))
        }
        "txid_current_snapshot" => {
            arity(0)?;
            Ok(Bson::String(CURRENT.into()))
        }
        "pg_visible_in_snapshot" | "txid_visible_in_snapshot" => {
            arity(2)?;
            if args.contains(&Bson::Null) {
                return Ok(Bson::Null);
            }
            let xid = crate::value_text(&args[0])
                .trim()
                .parse::<u64>()
                .map_err(|_| Error::Unsupported(format!("{name}() over this value")))?;
            let (xmin, xmax, xip) = parts(&args[1])?;
            let visible = if xid < xmin {
                true
            } else if xid >= xmax {
                false
            } else {
                !xip.contains(&xid)
            };
            Ok(Bson::Boolean(visible))
        }
        _ => {
            arity(1)?;
            if args[0] == Bson::Null {
                return Ok(Bson::Null);
            }
            let (xmin, xmax, _) = parts(&args[0])?;
            Ok(xid_value(
                name,
                if name.ends_with("xmin") { xmin } else { xmax },
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_follows_xmin_xmax_and_xip() {
        let s = Bson::String("10:20:10,14,15".into());
        let vis = |x: i64| call("txid_visible_in_snapshot", &[Bson::Int64(x), s.clone()]);
        assert_eq!(vis(9).unwrap().unwrap(), Bson::Boolean(true));
        assert_eq!(vis(14).unwrap().unwrap(), Bson::Boolean(false));
        assert_eq!(vis(12).unwrap().unwrap(), Bson::Boolean(true));
        assert_eq!(vis(25).unwrap().unwrap(), Bson::Boolean(false));
        assert_eq!(
            call("txid_snapshot_xmax", &[s]).unwrap().unwrap(),
            Bson::Int64(20)
        );
    }
}
