//! The large-object functions called from SQL (`SELECT lo_unlink(oid)`,
//! `lo_get`, `lo_put`, `lo_from_bytea`): each runs the Fastpath call it
//! equals, so the two paths cannot drift.

use super::*;

/// The SQL-callable large-object functions this module answers.
pub(crate) fn is_sql_function(name: &str) -> bool {
    LO_PROCS.iter().any(|(n, _, _, _)| *n == name)
        || matches!(name, "lo_get" | "lo_put" | "lo_from_bytea")
}

fn bson_int(v: &Bson) -> PgWireResult<i64> {
    match v {
        Bson::Int32(i) => Ok(i64::from(*i)),
        Bson::Int64(i) => Ok(*i),
        Bson::Double(d) => Ok(*d as i64),
        Bson::String(s) => s.trim().parse().map_err(|_| {
            err(
                "22P02",
                format!("invalid input syntax for type bigint: \"{s}\""),
            )
        }),
        other => Err(err(
            "42804",
            format!("an integer argument was expected, not {other}"),
        )),
    }
}

fn wire_bytes(v: Vec<u8>) -> Option<Bytes> {
    Some(Bytes::from(v))
}

impl PgHandler {
    /// A large-object function called from SQL (`SELECT lo_unlink(oid)`).
    pub(crate) fn lo_sql_call(&self, name: &str, args: &[Bson]) -> PgWireResult<Bson> {
        if args.contains(&Bson::Null) {
            return Ok(Bson::Null);
        }
        let int = |i: usize| -> PgWireResult<i64> {
            args.get(i).map(bson_int).unwrap_or_else(|| {
                Err(err(
                    "42883",
                    format!("function {name} needs more arguments"),
                ))
            })
        };
        let bytes = |i: usize| -> PgWireResult<Vec<u8>> {
            secantus_pgplan::bytea::parse(args.get(i).unwrap_or(&Bson::Null))
                .map_err(|e| Self::err(&e))
        };
        let i32b = |v: i64| wire_bytes((v as i32).to_be_bytes().to_vec());
        let i64b = |v: i64| wire_bytes(v.to_be_bytes().to_vec());
        let oid_of = |n: &str| LO_PROCS.iter().find(|p| p.0 == n).map_or(0, |p| p.1) as u32;
        let as_int = |out: Option<Vec<u8>>| -> Bson {
            match out.as_deref() {
                Some(b) if b.len() == 4 => {
                    Bson::Int32(i32::from_be_bytes(b.try_into().expect("4 bytes")))
                }
                Some(b) if b.len() == 8 => {
                    Bson::Int64(i64::from_be_bytes(b.try_into().expect("8 bytes")))
                }
                _ => Bson::Null,
            }
        };
        let as_oid = |b: Bson| match b {
            Bson::Int32(i) => Bson::Int64(i64::from(i as u32)),
            other => other,
        };
        match name {
            "lo_get" => {
                let oid = int(0)?;
                let (off, len) = if args.len() >= 3 {
                    (int(1)?, int(2)?)
                } else {
                    (0, i64::MAX)
                };
                self.in_open_transaction(|| {
                    let size = self.lo_size(oid)?.ok_or_else(|| {
                        err("42704", format!("large object {oid} does not exist"))
                    })?;
                    if off < 0 || len < 0 {
                        return Err(err("22023", "invalid large object read request".into()));
                    }
                    let len = len.min(size.saturating_sub(off).max(0));
                    Ok(secantus_pgplan::bytea::to_binary(
                        self.lo_read(oid, off, len)?,
                    ))
                })
            }
            "lo_put" => {
                let (oid, off, data) = (int(0)?, int(1)?, bytes(2)?);
                self.in_open_transaction(|| {
                    if self.lo_size(oid)?.is_none() {
                        return Err(err("42704", format!("large object {oid} does not exist")));
                    }
                    self.lo_write(oid, off, &data)?;
                    // `void`, which a client reads as the empty string.
                    Ok(Bson::String(String::new()))
                })
            }
            "lo_from_bytea" => {
                let (oid, data) = (int(0)?, bytes(1)?);
                let out = self.lo_call(oid_of("lo_create"), &[i32b(oid)])?;
                let new = as_oid(as_int(out));
                let n = bson_int(&new)?;
                self.in_open_transaction(|| self.lo_write(n, 0, &data))?;
                Ok(new)
            }
            "lo_creat" | "lo_create" | "lo_unlink" | "lo_close" | "lo_tell" | "lo_tell64" => {
                let out = self.lo_call(oid_of(name), &[i32b(int(0)?)])?;
                Ok(match name {
                    "lo_creat" | "lo_create" => as_oid(as_int(out)),
                    _ => as_int(out),
                })
            }
            "lo_open" | "lo_truncate" => Ok(as_int(
                self.lo_call(oid_of(name), &[i32b(int(0)?), i32b(int(1)?)])?,
            )),
            "lo_truncate64" => Ok(as_int(
                self.lo_call(oid_of(name), &[i32b(int(0)?), i64b(int(1)?)])?,
            )),
            "loread" => {
                let out = self.lo_call(oid_of(name), &[i32b(int(0)?), i32b(int(1)?)])?;
                Ok(secantus_pgplan::bytea::to_binary(out.unwrap_or_default()))
            }
            "lowrite" => {
                let data = bytes(1)?;
                Ok(as_int(self.lo_call(
                    oid_of(name),
                    &[i32b(int(0)?), wire_bytes(data)],
                )?))
            }
            "lo_lseek" => Ok(as_int(
                self.lo_call(oid_of(name), &[i32b(int(0)?), i32b(int(1)?), i32b(int(2)?)])?,
            )),
            "lo_lseek64" => Ok(as_int(
                self.lo_call(oid_of(name), &[i32b(int(0)?), i64b(int(1)?), i32b(int(2)?)])?,
            )),
            _ => Err(err("42883", format!("function {name} is not supported"))),
        }
    }
}
