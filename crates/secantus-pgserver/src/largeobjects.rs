//! Large objects over the Fastpath sub-protocol (`FunctionCall` 'F'), which is
//! how pgjdbc's `LargeObjectManager` (its `Blob` / `Clob`) reaches them: it
//! resolves the `lo_*` oids from `pg_proc` once, then calls by oid.
//!
//! Stored as the Python server stores them (`src/secantus/sql/
//! largeobjects.py`), so either server reads the other's objects: one
//! `__sql_largeobjects__` document per object, `{_id: oid, size: n}`, and
//! 256 KB chunks in `__sql_largeobject_chunks__`, `{_id: {o: oid, i: n},
//! data: <binary>}`; a hole reads as zeros. Writes join the session's open
//! transaction, so a ROLLBACK discards them as PostgreSQL's do.

use super::*;
use pgwire::messages::fastpath::{FunctionCall, FunctionCallResponse};

mod sql;
pub(crate) use sql::is_sql_function;

const LO_COLLECTION: &str = "__sql_largeobjects__";
const LO_CHUNKS: &str = "__sql_largeobject_chunks__";
const CHUNK: i64 = 256 * 1024;
const FIRST_LO_OID: i64 = 16384;
const MAX_LO_SIZE: i64 = 4 * 1024 * 1024 * 1024 * 1024;
const INV_WRITE: i32 = 0x0002_0000;

/// PostgreSQL's own oids (`pg_proc.dat`): name, oid, argument types, result.
pub(crate) const LO_PROCS: &[(&str, i64, &[i64], i64)] = &[
    ("lo_open", 952, &[26, 23], 23),
    ("lo_close", 953, &[23], 23),
    ("loread", 954, &[23, 23], 17),
    ("lowrite", 955, &[23, 17], 23),
    ("lo_lseek", 956, &[23, 23, 23], 23),
    ("lo_creat", 957, &[23], 26),
    ("lo_create", 715, &[26], 26),
    ("lo_tell", 958, &[23], 23),
    ("lo_unlink", 964, &[26], 23),
    ("lo_truncate", 1004, &[23, 23], 23),
    ("lo_lseek64", 3170, &[23, 20, 23], 20),
    ("lo_tell64", 3171, &[23], 20),
    ("lo_truncate64", 3172, &[23, 20], 23),
];

/// An open descriptor: the object, the position, the open mode.
#[derive(Clone, Copy)]
pub(crate) struct LoDesc {
    oid: i64,
    pos: i64,
    mode: i32,
}

#[derive(Default)]
pub(crate) struct LoDescriptors {
    open: HashMap<i32, LoDesc>,
}

fn err(code: &str, msg: String) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new("ERROR".into(), code.into(), msg)))
}

fn int_bson(v: i64) -> Bson {
    i32::try_from(v).map_or(Bson::Int64(v), Bson::Int32)
}

fn arg_i32(args: &[Option<Bytes>], i: usize) -> PgWireResult<i32> {
    let b = args
        .get(i)
        .and_then(|a| a.as_ref())
        .ok_or_else(|| err("22023", "fastpath argument missing".into()))?;
    let a: [u8; 4] = b.as_ref().try_into().map_err(|_| {
        err(
            "22023",
            format!("fastpath argument is {} bytes, expected 4", b.len()),
        )
    })?;
    Ok(i32::from_be_bytes(a))
}

fn arg_i64(args: &[Option<Bytes>], i: usize) -> PgWireResult<i64> {
    let b = args
        .get(i)
        .and_then(|a| a.as_ref())
        .ok_or_else(|| err("22023", "fastpath argument missing".into()))?;
    let a: [u8; 8] = b.as_ref().try_into().map_err(|_| {
        err(
            "22023",
            format!("fastpath argument is {} bytes, expected 8", b.len()),
        )
    })?;
    Ok(i64::from_be_bytes(a))
}

/// An oid argument: unsigned on the wire.
fn arg_oid(args: &[Option<Bytes>], i: usize) -> PgWireResult<i64> {
    Ok(i64::from(arg_i32(args, i)? as u32))
}

impl PgHandler {
    fn lo_find(&self, coll: &str, filter: Document) -> PgWireResult<Vec<Document>> {
        if !self
            .storage
            .collection_exists(self.db(), coll)
            .map_err(|e| Self::storage_err("could not read the large object", e))?
        {
            return Ok(Vec::new());
        }
        Ok(self
            .storage
            .find_matching(self.db(), coll, &filter)
            .map_err(|e| Self::storage_err("could not read the large object", e))?
            .iter()
            .filter_map(|b| decode_doc(b).ok())
            .collect())
    }

    fn lo_delete(&self, coll: &str, filter: Document) -> PgWireResult<u64> {
        if !self
            .storage
            .collection_exists(self.db(), coll)
            .map_err(|e| Self::storage_err("could not write the large object", e))?
        {
            return Ok(0);
        }
        let n = self
            .storage
            .delete_matching(self.db(), coll, &filter, 0, &Document::new(), None)
            .map_err(|e| Self::storage_err("could not write the large object", e))?;
        Ok(n as u64)
    }

    fn lo_insert(&self, coll: &str, doc: Document) -> PgWireResult<()> {
        let bytes = encode_doc(&doc)
            .map_err(|e| Self::storage_err("could not encode the large object", e))?;
        self.insert_checked(coll, vec![bytes], "could not write the large object")?;
        Ok(())
    }

    /// The object's size, or `None` when there is no such object.
    fn lo_size(&self, oid: i64) -> PgWireResult<Option<i64>> {
        Ok(self
            .lo_find(LO_COLLECTION, bson::doc! {"_id": int_bson(oid)})?
            .first()
            .map(|d| match d.get("size") {
                Some(Bson::Int32(n)) => i64::from(*n),
                Some(Bson::Int64(n)) => *n,
                Some(Bson::Double(n)) => *n as i64,
                _ => 0,
            }))
    }

    fn lo_set_size(&self, oid: i64, size: i64) -> PgWireResult<()> {
        self.lo_delete(LO_COLLECTION, bson::doc! {"_id": int_bson(oid)})?;
        self.lo_insert(
            LO_COLLECTION,
            bson::doc! {"_id": int_bson(oid), "size": int_bson(size)},
        )
    }

    fn chunk_id(oid: i64, idx: i64) -> Document {
        bson::doc! {"o": int_bson(oid), "i": int_bson(idx)}
    }

    fn lo_chunk(&self, oid: i64, idx: i64) -> PgWireResult<Vec<u8>> {
        Ok(self
            .lo_find(LO_CHUNKS, bson::doc! {"_id": Self::chunk_id(oid, idx)})?
            .first()
            .and_then(|d| match d.get("data") {
                Some(Bson::Binary(b)) => Some(b.bytes.clone()),
                _ => None,
            })
            .unwrap_or_default())
    }

    fn lo_put_chunk(&self, oid: i64, idx: i64, data: Vec<u8>) -> PgWireResult<()> {
        self.lo_delete(LO_CHUNKS, bson::doc! {"_id": Self::chunk_id(oid, idx)})?;
        if data.is_empty() {
            return Ok(());
        }
        self.lo_insert(
            LO_CHUNKS,
            bson::doc! {
                "_id": Self::chunk_id(oid, idx),
                "data": Bson::Binary(bson::Binary {
                    subtype: bson::spec::BinarySubtype::Generic,
                    bytes: data,
                }),
            },
        )
    }

    fn lo_read(&self, oid: i64, pos: i64, len: i64) -> PgWireResult<Vec<u8>> {
        let size = self.lo_size(oid)?.unwrap_or(0);
        if pos >= size || len <= 0 {
            return Ok(Vec::new());
        }
        let end = (pos + len).min(size);
        let mut out = Vec::with_capacity((end - pos) as usize);
        for idx in pos / CHUNK..=(end - 1) / CHUNK {
            let mut chunk = self.lo_chunk(oid, idx)?;
            let full = CHUNK.min(size - idx * CHUNK) as usize;
            if chunk.len() < full {
                chunk.resize(full, 0);
            }
            let lo = (pos - idx * CHUNK).max(0) as usize;
            let hi = (end - idx * CHUNK).min(CHUNK) as usize;
            out.extend_from_slice(&chunk[lo..hi]);
        }
        Ok(out)
    }

    fn lo_write(&self, oid: i64, pos: i64, data: &[u8]) -> PgWireResult<()> {
        let end = pos + data.len() as i64;
        if !data.is_empty() {
            for idx in pos / CHUNK..=(end - 1) / CHUNK {
                let mut chunk = self.lo_chunk(oid, idx)?;
                let lo = (pos - idx * CHUNK).max(0) as usize;
                let hi = (end - idx * CHUNK).min(CHUNK) as usize;
                if chunk.len() < hi {
                    chunk.resize(hi, 0);
                }
                let src = (idx * CHUNK + lo as i64 - pos) as usize;
                chunk[lo..hi].copy_from_slice(&data[src..src + (hi - lo)]);
                self.lo_put_chunk(oid, idx, chunk)?;
            }
        }
        if end > self.lo_size(oid)?.unwrap_or(0) {
            self.lo_set_size(oid, end)?;
        }
        Ok(())
    }

    fn lo_truncate_to(&self, oid: i64, len: i64) -> PgWireResult<()> {
        let size = self.lo_size(oid)?.unwrap_or(0);
        if len < size {
            let last = if len > 0 { (len - 1) / CHUNK } else { -1 };
            for idx in last + 1..=(size - 1) / CHUNK {
                self.lo_delete(LO_CHUNKS, bson::doc! {"_id": Self::chunk_id(oid, idx)})?;
            }
            if len > 0 && len % CHUNK != 0 {
                let mut keep = self.lo_chunk(oid, last)?;
                keep.truncate((len % CHUNK) as usize);
                self.lo_put_chunk(oid, last, keep)?;
            }
        }
        self.lo_set_size(oid, len)
    }

    fn lo_next_oid(&self) -> PgWireResult<i64> {
        let top = self
            .lo_find(LO_COLLECTION, Document::new())?
            .iter()
            .filter_map(|d| match d.get("_id") {
                Some(Bson::Int32(n)) => Some(i64::from(*n)),
                Some(Bson::Int64(n)) => Some(*n),
                _ => None,
            })
            .max()
            .unwrap_or(FIRST_LO_OID - 1);
        Ok((top + 1).max(FIRST_LO_OID))
    }

    fn lo_desc(&self, fd: i32) -> PgWireResult<LoDesc> {
        self.lo_descriptors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .open
            .get(&fd)
            .copied()
            .ok_or_else(|| err("42704", format!("invalid large-object descriptor: {fd}")))
    }

    fn lo_set_pos(&self, fd: i32, pos: i64) {
        if let Some(d) = self
            .lo_descriptors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .open
            .get_mut(&fd)
        {
            d.pos = pos;
        }
    }

    /// One Fastpath call: the binary result value.
    fn lo_call(&self, oid: u32, args: &[Option<Bytes>]) -> PgWireResult<Option<Vec<u8>>> {
        let name = LO_PROCS
            .iter()
            .find(|(_, o, _, _)| *o == i64::from(oid))
            .map(|(n, _, _, _)| *n)
            .ok_or_else(|| err("42883", format!("function with OID {oid} does not exist")))?;
        let writes = matches!(
            name,
            "lo_creat" | "lo_create" | "lowrite" | "lo_truncate" | "lo_truncate64" | "lo_unlink"
        );
        if writes
            && self
                .in_transaction
                .load(std::sync::atomic::Ordering::Relaxed)
            && self
                .settings
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get("transaction_read_only")
                .is_some_and(|v| v == "on")
        {
            return Err(err(
                "25006",
                format!("cannot execute {name}() in a read-only transaction"),
            ));
        }
        let i32_out = |v: i64| Some((v as i32).to_be_bytes().to_vec());
        let i64_out = |v: i64| Some(v.to_be_bytes().to_vec());
        self.in_open_transaction(|| match name {
            "lo_creat" | "lo_create" => {
                let mut oid = if name == "lo_create" { arg_oid(args, 0)? } else { 0 };
                if oid == 0 {
                    oid = self.lo_next_oid()?;
                } else if self.lo_size(oid)?.is_some() {
                    return Err(err(
                        "23505",
                        format!("duplicate key value violates unique constraint \"pg_largeobject_metadata_oid_index\"\nDetail: Key (oid)=({oid}) already exists."),
                    ));
                }
                self.lo_insert(LO_COLLECTION, bson::doc! {"_id": int_bson(oid), "size": 0})?;
                Ok(i32_out(oid))
            }
            "lo_open" => {
                let (oid, mode) = (arg_oid(args, 0)?, arg_i32(args, 1)?);
                if self.lo_size(oid)?.is_none() {
                    return Err(err("42704", format!("large object {oid} does not exist")));
                }
                let mut descs = self.lo_descriptors.lock().unwrap_or_else(|e| e.into_inner());
                // The lowest free slot, as PostgreSQL's `newLOfd` picks: a
                // transaction's first descriptor is 0 again.
                let fd = (0..)
                    .find(|i| !descs.open.contains_key(i))
                    .expect("a free descriptor");
                descs.open.insert(fd, LoDesc { oid, pos: 0, mode });
                Ok(i32_out(i64::from(fd)))
            }
            "lo_close" => {
                let fd = arg_i32(args, 0)?;
                self.lo_descriptors
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .open
                    .remove(&fd)
                    .ok_or_else(|| err("42704", format!("invalid large-object descriptor: {fd}")))?;
                Ok(i32_out(0))
            }
            "loread" => {
                let fd = arg_i32(args, 0)?;
                let d = self.lo_desc(fd)?;
                let data = self.lo_read(d.oid, d.pos, i64::from(arg_i32(args, 1)?.max(0)))?;
                self.lo_set_pos(fd, d.pos + data.len() as i64);
                Ok(Some(data))
            }
            "lowrite" => {
                let fd = arg_i32(args, 0)?;
                let d = self.lo_desc(fd)?;
                if d.mode & INV_WRITE == 0 {
                    return Err(err(
                        "55000",
                        format!("large object descriptor {fd} was not opened for writing"),
                    ));
                }
                let data = args.get(1).and_then(|a| a.clone()).unwrap_or_default();
                self.lo_write(d.oid, d.pos, &data)?;
                self.lo_set_pos(fd, d.pos + data.len() as i64);
                Ok(i32_out(data.len() as i64))
            }
            "lo_lseek" | "lo_lseek64" => {
                let fd = arg_i32(args, 0)?;
                let d = self.lo_desc(fd)?;
                let wide = name.ends_with("64");
                let offset = if wide { arg_i64(args, 1)? } else { i64::from(arg_i32(args, 1)?) };
                let base = match arg_i32(args, 2)? {
                    0 => 0,
                    1 => d.pos,
                    2 => self.lo_size(d.oid)?.unwrap_or(0),
                    w => return Err(err("22023", format!("invalid whence setting: {w}"))),
                };
                let new = base + offset;
                if !(0..=MAX_LO_SIZE).contains(&new) || (!wide && new > i64::from(i32::MAX)) {
                    return Err(err("22023", format!("invalid seek offset: {new}")));
                }
                self.lo_set_pos(fd, new);
                Ok(if wide { i64_out(new) } else { i32_out(new) })
            }
            "lo_tell" | "lo_tell64" => {
                let d = self.lo_desc(arg_i32(args, 0)?)?;
                Ok(if name.ends_with("64") { i64_out(d.pos) } else { i32_out(d.pos) })
            }
            "lo_truncate" | "lo_truncate64" => {
                let fd = arg_i32(args, 0)?;
                let d = self.lo_desc(fd)?;
                if d.mode & INV_WRITE == 0 {
                    return Err(err(
                        "55000",
                        format!("large object descriptor {fd} was not opened for writing"),
                    ));
                }
                let len = if name.ends_with("64") {
                    arg_i64(args, 1)?
                } else {
                    i64::from(arg_i32(args, 1)?)
                };
                if !(0..=MAX_LO_SIZE).contains(&len) {
                    return Err(err("22023", format!("invalid large object truncation target: {len}")));
                }
                self.lo_truncate_to(d.oid, len)?;
                Ok(i32_out(0))
            }
            "lo_unlink" => {
                let oid = arg_oid(args, 0)?;
                if self.lo_delete(LO_COLLECTION, bson::doc! {"_id": int_bson(oid)})? == 0 {
                    return Err(err("42704", format!("large object {oid} does not exist")));
                }
                self.lo_delete(LO_CHUNKS, bson::doc! {"_id.o": int_bson(oid)})?;
                self.lo_descriptors
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .open
                    .retain(|_, d| d.oid != oid);
                Ok(i32_out(1))
            }
            _ => Err(err("42883", format!("fastpath function {name} is not implemented"))),
        })
    }

    /// The Fastpath message: run the call, answer `V` then `ReadyForQuery`.
    pub(crate) async fn fastpath_call<C>(
        &self,
        client: &mut C,
        call: FunctionCall,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let in_block = self
            .in_transaction
            .load(std::sync::atomic::Ordering::Relaxed);
        if in_block && self.txn_failed.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Self::in_failed_transaction());
        }
        let out = match self.lo_call(call.function_oid, &call.arguments) {
            Ok(v) => v,
            Err(e) => {
                if in_block {
                    self.txn_failed
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
                return Err(e);
            }
        };
        if !in_block {
            // Outside a block the call was its own transaction, and a
            // descriptor lasts only as long as its transaction.
            self.lo_close_all();
        }
        client
            .feed(PgWireBackendMessage::FunctionCallResponse(
                FunctionCallResponse::new(out.map(Bytes::from)),
            ))
            .await?;
        let status = client.transaction_status();
        client
            .send(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
                status,
            )))
            .await?;
        Ok(())
    }

    /// A transaction ended: every descriptor it opened is closed.
    pub(crate) fn lo_close_all(&self) {
        self.lo_descriptors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .open
            .clear();
    }

    /// `pg_largeobject_metadata`: one row per large object.
    pub(crate) fn lo_metadata_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        self.lo_find(LO_COLLECTION, Document::new())
            .unwrap_or_default()
            .iter()
            .filter_map(|d| {
                let oid = match d.get("_id") {
                    Some(Bson::Int32(n)) => i64::from(*n),
                    Some(Bson::Int64(n)) => *n,
                    _ => return None,
                };
                let mut row = Document::new();
                row.insert(f("oid"), Bson::Int64(oid));
                row.insert(f("lomowner"), Bson::Int64(10));
                row.insert(f("lomacl"), Bson::Null);
                Some(row)
            })
            .collect()
    }

    /// The `lo_*` functions' rows in `pg_proc`, so a client finds their oids.
    pub(crate) fn lo_pg_proc_rows(def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        LO_PROCS
            .iter()
            .map(|(name, oid, args, ret)| {
                let mut row = Document::new();
                row.insert(f("oid"), Bson::Int64(*oid));
                row.insert(f("proname"), *name);
                row.insert(f("pronamespace"), Bson::Int64(11));
                row.insert(f("proowner"), Bson::Int64(10));
                row.insert(f("prolang"), Bson::Int64(12));
                row.insert(f("prokind"), "f");
                row.insert(f("prosecdef"), false);
                row.insert(f("proisstrict"), true);
                row.insert(f("proretset"), false);
                row.insert(f("provolatile"), "v");
                row.insert(f("pronargs"), Bson::Int32(args.len() as i32));
                row.insert(f("prorettype"), Bson::Int64(*ret));
                row.insert(
                    f("proargtypes"),
                    Bson::Array(args.iter().map(|a| Bson::Int64(*a)).collect()),
                );
                row.insert(f("proargnames"), Bson::Null);
                row.insert(f("prosrc"), *name);
                row
            })
            .collect()
    }
}
