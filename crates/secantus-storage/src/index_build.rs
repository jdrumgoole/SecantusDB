//! Hybrid index builds (RM-187): mongod's two-phase build, in which the
//! collection takes writes while its documents are scanned.
//!
//! A build (`IndexBuildsCoordinator` on mongod 8.2) runs in phases:
//!
//! 1. **Set up**, under the global and collection locks (and, in the command
//!    layer, the collection's `X` lock): the indexes are checked against the
//!    catalog, the build is recorded ([`BUILDS_TABLE`], which is what an open
//!    after a crash restarts), `startIndexBuild` is logged, and the build is
//!    REGISTERED -- from then on every write to the collection also records,
//!    in its own transaction, what it does to the new indexes' keys in a side
//!    table ([`SIDE_TABLE`], mongod's `IndexBuildInterceptor`).
//! 2. **Scan and bulk load**, holding no lock: the documents are read in
//!    batches, each index's keys go to an external sorter that keeps at most
//!    its share of `maxIndexBuildMemoryUsageMegabytes` in memory and spills
//!    sorted runs to `<dbpath>/_tmp` past it, and the merged runs are written
//!    to the entries table in bounded transactions.
//! 3. **Drain**: the side table is applied to the entries, in the order the
//!    writes were made, until it is nearly empty.
//! 4. **Commit**, under the locks again: the last side writes are applied,
//!    the documents whose keys could not be made when they were written are
//!    keyed again (mongod's `SkippedRecordTracker`), every key two documents
//!    were seen to share is checked again (its duplicate-key tracker), and
//!    the catalog rows, `commitIndexBuild` and the removal of the build's
//!    record commit in one transaction.
//!
//! Applying a side write is idempotent (an entry insert that is there
//! already, a removal of one that is not), and the writes to one document
//! are recorded in the order they committed, because the second waits for
//! the first. So a document the scan read before or after a write ends with
//! the same entries, which is what lets the scan read without a snapshot of
//! its own. A build that is aborted -- `dropIndexes`, a drop of its
//! collection or database, `killOp`, an error -- removes its entries and
//! side rows and logs `abortIndexBuild` with the cause.

use super::*;
use std::io::{BufReader, BufWriter, Read, Write};

/// Each in-progress build's record: its namespace and indexes, kept from
/// set-up until the build commits or aborts.
pub(crate) const BUILDS_TABLE: &str = "table:secantus_index_builds";
/// Writes made to a collection while an index is built over it, keyed by
/// build UUID then the order they were made in.
pub(crate) const SIDE_TABLE: &str = "table:secantus_index_build_side";

/// The collection mongod records a build in (`config.system.indexBuilds`).
const CONFIG_DB: &str = "config";
const BUILDS_COLL: &str = "system.indexBuilds";

/// Documents read per scan batch.
const SCAN_BATCH: usize = 1000;
/// Side writes applied per drain transaction.
const DRAIN_BATCH: usize = 1000;

/// mongod's default `maxIndexBuildMemoryUsageMegabytes` (200), in bytes.
pub const DEFAULT_BUILD_MEMORY_BYTES: usize = 200 * 1024 * 1024;

/// What a sorter counts against its budget for one key beyond its bytes:
/// the vector that holds it.
const SORTER_ENTRY_OVERHEAD: usize = 32;

/// The error a build returns when it stopped because an abort was asked
/// for; the cause is [`Storage::index_build_abort_cause`].
pub const INDEX_BUILD_ABORTED: &str = "index build aborted";

/// An index a build is asked to create: its name, key pattern and options
/// (the spec without `key` / `name` / `v`).
#[derive(Clone, Debug)]
pub struct IndexBuildSpec {
    pub name: String,
    pub key: Document,
    pub options: Document,
}

/// Why a build was aborted: mongod's error, as `abortIndexBuild` records it
/// in `cause` (`extra` carries a duplicate key's `keyPattern` / `keyValue`).
#[derive(Clone, Debug, PartialEq)]
pub struct IndexBuildAbort {
    pub code: i32,
    pub code_name: String,
    pub errmsg: String,
    pub extra: Document,
}

impl IndexBuildAbort {
    pub fn new(code: i32, code_name: &str, errmsg: impl Into<String>) -> Self {
        IndexBuildAbort {
            code,
            code_name: code_name.to_string(),
            errmsg: errmsg.into(),
            extra: Document::new(),
        }
    }

    /// The `cause` document of `abortIndexBuild`.
    fn cause(&self) -> Document {
        let mut c = Document::new();
        c.insert("ok", false);
        c.insert("code", self.code);
        c.insert("codeName", self.code_name.clone());
        c.insert("errmsg", self.errmsg.clone());
        for (k, v) in &self.extra {
            c.insert(k.clone(), v.clone());
        }
        c
    }
}

/// A build in progress, as `listIndexes` / `currentOp` describe it.
#[derive(Clone, Debug)]
pub struct IndexBuildInfo {
    pub uuid: [u8; 16],
    pub db: String,
    pub coll: String,
    /// Each index's spec, `{v, key, name, <options>}`.
    pub indexes: Vec<Document>,
    /// mongod's phase number and name (`indexBuildInfo.phase` /
    /// `phaseStr`).
    pub phase: i32,
    pub phase_str: &'static str,
    /// An abort has been asked for.
    pub aborting: bool,
}

/// One index of a build.
struct BuildIndex {
    name: String,
    key: Document,
    /// The catalog `options` it will be recorded with.
    stored: Document,
    desc: IndexDesc,
    /// Some document written or scanned holds an array on its key path.
    multikey: AtomicBool,
    /// Per key field, the array components seen on its path: the catalog's
    /// `multikeyPaths` (`crate::array_components`).
    paths: Mutex<Vec<Vec<String>>>,
    /// The index's own table (`index-<UUID>`), made when the scan starts and
    /// listed in the placement only when the build commits.
    ident: Mutex<Option<String>>,
}

impl BuildIndex {
    /// `d` makes the index multikey: flag it, and add the array components
    /// on its key paths to what the catalog will record.
    fn note_multikey(&self, d: &Document) {
        let (makes, doc_paths) = self.desc.multikey_paths_of(d);
        if !makes {
            return;
        }
        self.multikey.store(true, Ordering::Relaxed);
        let mut paths = self.paths.lock().unwrap_or_else(|e| e.into_inner());
        for (parts, add) in paths.iter_mut().zip(&doc_paths) {
            for path in add {
                if !parts.contains(path) {
                    parts.push(path.clone());
                }
            }
        }
    }

    /// The catalog's `multikeyPaths` option: per key field, its array
    /// components, sorted.
    fn recorded_paths(&self) -> Bson {
        let paths = self.paths.lock().unwrap_or_else(|e| e.into_inner());
        let mut recorded = Document::new();
        for (field, parts) in self.key.keys().zip(paths.iter()) {
            let mut parts = parts.clone();
            parts.sort();
            recorded.insert(
                field.clone(),
                Bson::Array(parts.into_iter().map(Bson::String).collect()),
            );
        }
        Bson::Document(recorded)
    }

    fn new(name: String, key: Document, stored: Document) -> Self {
        let desc = index_desc_from(name.clone(), key.clone(), &stored);
        let fields = key.len();
        BuildIndex {
            name,
            key,
            stored,
            desc,
            multikey: AtomicBool::new(false),
            paths: Mutex::new(vec![Vec::new(); fields]),
            ident: Mutex::new(None),
        }
    }

    /// The table the build writes this index's entries to.
    fn table(&self) -> Result<String> {
        self.ident
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_deref()
            .map(mdb_catalog::table_uri)
            .ok_or_else(|| {
                StorageError::Internal(format!("index build: index {} has no table yet", self.name))
            })
    }

    fn unique(&self) -> bool {
        self.desc.unique
    }

    /// The spec, `{v, key, name, <user options>}`.
    fn spec(&self) -> Document {
        let mut payload = Document::new();
        payload.insert("key", Bson::Document(self.key.clone()));
        payload.insert("options", Bson::Document(self.stored.clone()));
        let event = index_event_spec(&payload, &self.name);
        let mut spec = Document::new();
        spec.insert("v", 2i32);
        spec.insert("key", Bson::Document(self.key.clone()));
        spec.insert("name", self.name.clone());
        for (k, v) in &event {
            if !matches!(k.as_str(), "v" | "key" | "name") {
                spec.insert(k.clone(), v.clone());
            }
        }
        spec
    }
}

/// A registered build.
pub(crate) struct ActiveBuild {
    uuid: [u8; 16],
    db: String,
    coll: String,
    indexes: Vec<BuildIndex>,
    abort: Mutex<Option<IndexBuildAbort>>,
    /// mongod's phase: 1 collection scan, 2 bulk load, 3 drain writes.
    phase: std::sync::atomic::AtomicI32,
    /// Escaped keys two documents were seen to share, per index: checked
    /// again at commit.
    dup_candidates: Mutex<Vec<BTreeSet<Vec<u8>>>>,
    /// `(index, RecordId)` of documents whose keys could not be made when
    /// they were written: keyed again at commit.
    skipped: Mutex<BTreeSet<(usize, RecordId)>>,
    /// The member (`host:port`) that started it, which a restarted build
    /// records as ready to commit, as mongod's restarted builds do.
    member: String,
}

impl ActiveBuild {
    /// Shape every index's entries for a clustered collection's RecordIds.
    fn with_rid_bytes(mut self, rid_bytes: bool) -> Self {
        for ix in &mut self.indexes {
            ix.desc.rid_bytes = rid_bytes;
        }
        self
    }

    fn new(uuid: [u8; 16], db: &str, coll: &str, indexes: Vec<BuildIndex>) -> Self {
        let n = indexes.len();
        ActiveBuild {
            member: String::new(),
            uuid,
            db: db.to_string(),
            coll: coll.to_string(),
            indexes,
            abort: Mutex::new(None),
            phase: std::sync::atomic::AtomicI32::new(1),
            dup_candidates: Mutex::new(vec![BTreeSet::new(); n]),
            skipped: Mutex::new(BTreeSet::new()),
        }
    }

    fn abort_requested(&self) -> Option<IndexBuildAbort> {
        self.abort.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn info(&self) -> IndexBuildInfo {
        let phase = self.phase.load(Ordering::Acquire);
        IndexBuildInfo {
            uuid: self.uuid,
            db: self.db.clone(),
            coll: self.coll.clone(),
            indexes: self.indexes.iter().map(BuildIndex::spec).collect(),
            phase,
            phase_str: match phase {
                1 => "collection scan",
                2 => "bulk load",
                _ => "drain writes",
            },
            aborting: self.abort_requested().is_some(),
        }
    }

    /// The `indexes` array of the build's oplog entries.
    fn oplog_specs(&self) -> Vec<Bson> {
        self.indexes
            .iter()
            .map(|ix| Bson::Document(oplog_index_spec(&ix.spec())))
            .collect()
    }
}

/// The builds this process is running.
#[derive(Default)]
pub(crate) struct Registry {
    builds: std::sync::RwLock<Vec<Arc<ActiveBuild>>>,
    /// How many are registered: the write path's one-load fast path.
    active: AtomicUsize,
    /// The order side writes are made in.
    seq: AtomicU64,
    /// Builds unregistered whose last writes (the `config.system.indexBuilds`
    /// row's delete) are still to come: a waiter waits for those too.
    finishing: Mutex<Vec<[u8; 16]>>,
    /// Signalled when a build has finished.
    gone_lock: Mutex<()>,
    gone: Condvar,
}

/// A build unregistered but not yet finished: dropping it finishes it, on
/// every path, so a waiter is never left waiting.
pub(crate) struct Finishing<'a> {
    registry: &'a Registry,
    uuid: [u8; 16],
}

impl Drop for Finishing<'_> {
    fn drop(&mut self) {
        self.registry
            .finishing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|u| u != &self.uuid);
        let _g = self
            .registry
            .gone_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.registry.gone.notify_all();
    }
}

impl Registry {
    fn insert(&self, b: Arc<ActiveBuild>) {
        let mut builds = self.builds.write().unwrap_or_else(|e| e.into_inner());
        builds.push(b);
        self.active.store(builds.len(), Ordering::Release);
    }

    /// Unregister a build: no write records itself for it from here. It
    /// counts as gone for [`Storage::wait_index_builds_gone`] only when the
    /// answer is dropped, after the build's last writes.
    #[must_use]
    fn remove(&self, uuid: &[u8; 16]) -> Finishing<'_> {
        self.finishing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(*uuid);
        let mut builds = self.builds.write().unwrap_or_else(|e| e.into_inner());
        builds.retain(|b| &b.uuid != uuid);
        self.active.store(builds.len(), Ordering::Release);
        Finishing {
            registry: self,
            uuid: *uuid,
        }
    }

    /// Whether `uuid` is registered or still finishing.
    fn pending(&self, uuid: &[u8; 16]) -> bool {
        self.get(uuid).is_some()
            || self
                .finishing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(uuid)
    }

    fn get(&self, uuid: &[u8; 16]) -> Option<Arc<ActiveBuild>> {
        self.builds
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|b| &b.uuid == uuid)
            .cloned()
    }

    fn matching(&self, db: Option<&str>, coll: Option<&str>) -> Vec<Arc<ActiveBuild>> {
        self.builds
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|b| db.is_none_or(|d| d == b.db) && coll.is_none_or(|c| c == b.coll))
            .cloned()
            .collect()
    }
}

/// A side write: what one write did to one index's keys.
enum SideOp<'a> {
    Insert(&'a EntryKey),
    Remove(&'a EntryKey),
    /// The document's keys could not be made; key it again at commit.
    Skipped(&'a RecordId),
}

/// An entry as a side write or a sorter holds it: its key, its TypeBits,
/// and the TypeBits' length (so the two can be told apart).
fn encode_entry(e: &EntryKey) -> Vec<u8> {
    let mut out = Vec::with_capacity(e.key.len() + e.type_bits.len() + 4);
    out.extend_from_slice(&e.key);
    out.extend_from_slice(&e.type_bits);
    out.extend_from_slice(&(e.type_bits.len() as u32).to_be_bytes());
    out
}

fn decode_entry(bytes: &[u8]) -> Result<EntryKey> {
    let bad = || StorageError::Internal("index build: an entry too short to read".into());
    let split = bytes.len().checked_sub(4).ok_or_else(bad)?;
    let tb_len = u32::from_be_bytes(bytes[split..].try_into().map_err(|_| bad())?) as usize;
    let key_len = split.checked_sub(tb_len).ok_or_else(bad)?;
    Ok(EntryKey {
        key: bytes[..key_len].to_vec(),
        type_bits: bytes[key_len..split].to_vec(),
    })
}

fn encode_side(index: usize, op: &SideOp<'_>) -> Vec<u8> {
    let (tag, payload): (u8, Vec<u8>) = match op {
        SideOp::Insert(p) => (b'i', encode_entry(p)),
        SideOp::Remove(p) => (b'd', encode_entry(p)),
        // A counter's eight bytes, or a clustered collection's key bytes: the
        // build knows which its collection has.
        SideOp::Skipped(rid) => (
            b's',
            match rid {
                RecordId::Long(n) => n.to_be_bytes().to_vec(),
                RecordId::Bytes(k) => k.clone(),
            },
        ),
    };
    let mut out = Vec::with_capacity(payload.len() + 3);
    out.push(tag);
    out.extend_from_slice(&(index as u16).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

fn decode_side(value: &[u8]) -> Result<(u8, usize, &[u8])> {
    if value.len() < 3 {
        return Err(StorageError::Internal(
            "index build: a side write too short to read".into(),
        ));
    }
    let index = u16::from_be_bytes([value[1], value[2]]) as usize;
    Ok((value[0], index, &value[3..]))
}

fn side_key(uuid: &[u8; 16], seq: u64) -> Vec<u8> {
    let mut k = uuid.to_vec();
    k.extend_from_slice(&seq.to_be_bytes());
    k
}

fn uuid_text(uuid: &[u8; 16]) -> String {
    let h: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// An external sorter (mongod's `Sorter`): keys are held in memory up to
/// `limit` bytes, then sorted and written to a run file; the runs are
/// merged at the end.
struct ExtSorter {
    buf: Vec<Vec<u8>>,
    bytes: usize,
    limit: usize,
    dir: std::path::PathBuf,
    prefix: String,
    runs: Vec<std::path::PathBuf>,
}

impl ExtSorter {
    fn new(limit: usize, dir: std::path::PathBuf, prefix: String) -> Self {
        ExtSorter {
            buf: Vec::new(),
            bytes: 0,
            limit,
            dir,
            prefix,
            runs: Vec::new(),
        }
    }

    fn push(&mut self, key: Vec<u8>) -> Result<()> {
        self.bytes += key.len() + SORTER_ENTRY_OVERHEAD;
        self.buf.push(key);
        if self.bytes > self.limit {
            self.spill()?;
        }
        Ok(())
    }

    fn spill(&mut self) -> Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        self.buf.sort_unstable();
        std::fs::create_dir_all(&self.dir).map_err(io_error)?;
        let path = self
            .dir
            .join(format!("extsort-index.{}-{}", self.prefix, self.runs.len()));
        let file = std::fs::File::create(&path).map_err(io_error)?;
        let mut w = BufWriter::new(file);
        for k in self.buf.drain(..) {
            w.write_all(&(k.len() as u32).to_be_bytes())
                .map_err(io_error)?;
            w.write_all(&k).map_err(io_error)?;
        }
        w.flush().map_err(io_error)?;
        self.runs.push(path);
        self.bytes = 0;
        Ok(())
    }

    /// How many runs went to disk.
    fn spilled(&self) -> usize {
        self.runs.len()
    }

    /// Every key, in order: the merged runs, or the buffer sorted in
    /// memory when nothing spilled.
    fn finish(mut self) -> Result<SortedKeys> {
        if self.runs.is_empty() {
            self.buf.sort_unstable();
            let keys = std::mem::take(&mut self.buf);
            return Ok(SortedKeys::Memory(keys.into_iter()));
        }
        self.spill()?;
        let mut readers = Vec::with_capacity(self.runs.len());
        let mut heap = std::collections::BinaryHeap::new();
        for (i, path) in self.runs.iter().enumerate() {
            let mut r = BufReader::new(std::fs::File::open(path).map_err(io_error)?);
            if let Some(k) = read_run_key(&mut r)? {
                heap.push(std::cmp::Reverse((k, i)));
            }
            readers.push(r);
        }
        Ok(SortedKeys::Merge {
            readers,
            heap,
            paths: std::mem::take(&mut self.runs),
        })
    }
}

impl Drop for ExtSorter {
    fn drop(&mut self) {
        for path in &self.runs {
            // A run file left by an error is only space: the next open's
            // `_tmp` sweep removes it too.
            if let Err(e) = std::fs::remove_file(path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "secantus-storage: index build: could not remove {}: {e}",
                        path.display()
                    );
                }
            }
        }
    }
}

fn read_run_key(r: &mut BufReader<std::fs::File>) -> Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(io_error(e)),
    }
    let mut k = vec![0u8; u32::from_be_bytes(len) as usize];
    r.read_exact(&mut k).map_err(io_error)?;
    Ok(Some(k))
}

fn io_error(e: std::io::Error) -> StorageError {
    StorageError::Internal(format!("index build sorter: {e}"))
}

enum SortedKeys {
    Memory(std::vec::IntoIter<Vec<u8>>),
    Merge {
        readers: Vec<BufReader<std::fs::File>>,
        heap: std::collections::BinaryHeap<std::cmp::Reverse<(Vec<u8>, usize)>>,
        paths: Vec<std::path::PathBuf>,
    },
}

impl SortedKeys {
    fn next_key(&mut self) -> Result<Option<Vec<u8>>> {
        match self {
            SortedKeys::Memory(it) => Ok(it.next()),
            SortedKeys::Merge { readers, heap, .. } => {
                let Some(std::cmp::Reverse((k, i))) = heap.pop() else {
                    return Ok(None);
                };
                if let Some(next) = read_run_key(&mut readers[i])? {
                    heap.push(std::cmp::Reverse((next, i)));
                }
                Ok(Some(k))
            }
        }
    }
}

impl Drop for SortedKeys {
    fn drop(&mut self) {
        if let SortedKeys::Merge { paths, .. } = self {
            for path in paths.iter() {
                if let Err(e) = std::fs::remove_file(path) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        eprintln!(
                            "secantus-storage: index build: could not remove {}: {e}",
                            path.display()
                        );
                    }
                }
            }
        }
    }
}

/// The record a build keeps in [`BUILDS_TABLE`].
fn build_record(b: &ActiveBuild) -> Result<Vec<u8>> {
    let indexes: Vec<Bson> = b
        .indexes
        .iter()
        .map(|ix| {
            let mut d = Document::new();
            d.insert("name", ix.name.clone());
            d.insert("key", Bson::Document(ix.key.clone()));
            d.insert("stored", Bson::Document(ix.stored.clone()));
            Bson::Document(d)
        })
        .collect();
    let mut rec = Document::new();
    rec.insert("db", b.db.clone());
    rec.insert("coll", b.coll.clone());
    rec.insert("indexes", Bson::Array(indexes));
    rec.insert("member", b.member.clone());
    encode_doc(&rec)
}

fn build_from_record(uuid: [u8; 16], rec: &Document) -> Option<ActiveBuild> {
    let db = rec.get_str("db").ok()?;
    let coll = rec.get_str("coll").ok()?;
    let mut indexes = Vec::new();
    for ix in rec.get_array("indexes").ok()? {
        let ix = ix.as_document()?;
        indexes.push(BuildIndex::new(
            ix.get_str("name").ok()?.to_string(),
            ix.get_document("key").ok()?.clone(),
            ix.get_document("stored").ok()?.clone(),
        ));
    }
    let mut build = ActiveBuild::new(uuid, db, coll, indexes);
    build.member = rec.get_str("member").unwrap_or_default().to_string();
    Some(build)
}

impl Storage {
    /// Record what a write to `(db, coll)` did to the keys of every index
    /// being built over it: `old` is the document before (none for an
    /// insert), `new` after (none for a delete). Runs in the write's own
    /// transaction, so a write that rolls back records nothing.
    pub(crate) fn index_build_side_write(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        old: Option<&Document>,
        new: Option<&Document>,
        recordid: &RecordId,
    ) -> Result<()> {
        if self.index_builds.active.load(Ordering::Acquire) == 0 {
            return Ok(());
        }
        let builds = self.index_builds.matching(Some(db), Some(coll));
        if builds.is_empty() {
            return Ok(());
        }
        let cur = session.open_cursor(SIDE_TABLE, None)?;
        let put = |uuid: &[u8; 16], index: usize, op: SideOp<'_>| -> Result<()> {
            let seq = self.index_builds.seq.fetch_add(1, Ordering::Relaxed);
            cur.reset()?;
            cur.set_key_u(&side_key(uuid, seq));
            cur.set_value_u(&encode_side(index, &op));
            cur.insert()?;
            Ok(())
        };
        for b in &builds {
            for (i, ix) in b.indexes.iter().enumerate() {
                let keys = |d: Option<&Document>| -> Result<Option<Vec<EntryKey>>> {
                    let Some(d) = d else {
                        return Ok(Some(Vec::new()));
                    };
                    if geo_key_problem(d, std::slice::from_ref(&ix.desc)).is_some() {
                        return Ok(None);
                    }
                    match self.packed_entry_keys(d, &ix.desc, recordid) {
                        Ok(k) => Ok(Some(k)),
                        // A key mongod cannot make either (too many compound
                        // keys, a value the encoder refuses): the write goes
                        // on and the build looks at the document again.
                        Err(StorageError::QueryError { .. })
                        | Err(StorageError::UnsupportedValue) => Ok(None),
                        Err(e) => Err(e),
                    }
                };
                let (old_keys, new_keys) = (keys(old)?, keys(new)?);
                if let (Some(o), Some(n)) = (&old_keys, &new_keys) {
                    let o: HashSet<&EntryKey> = o.iter().collect();
                    let n: HashSet<&EntryKey> = n.iter().collect();
                    for k in o.difference(&n).filter(|k| !is_metadata_entry(k)) {
                        put(&b.uuid, i, SideOp::Remove(k))?;
                    }
                    for k in n.difference(&o) {
                        put(&b.uuid, i, SideOp::Insert(k))?;
                    }
                } else {
                    if let Some(o) = &old_keys {
                        for k in o.iter().filter(|k| !is_metadata_entry(k)) {
                            put(&b.uuid, i, SideOp::Remove(k))?;
                        }
                    }
                    put(&b.uuid, i, SideOp::Skipped(recordid))?;
                }
                if let Some(d) = new {
                    ix.note_multikey(d);
                }
            }
        }
        Ok(())
    }

    /// Set up an index build over `(db, coll)` under `uuid`: the indexes
    /// are checked against the catalog, the build is recorded and logged
    /// (`config.system.indexBuilds`, `startIndexBuild`), and from here on
    /// every write to the collection is recorded for it. `member` is the
    /// replica-set member running it (`host:port`), which it records as
    /// ready to commit -- again after a restart.
    pub fn begin_index_build(
        &self,
        db: &str,
        coll: &str,
        uuid: [u8; 16],
        specs: &[IndexBuildSpec],
        member: &str,
    ) -> Result<()> {
        // One operation: what it writes after its entry shares the entry's
        // timestamp, and what it writes before it is untimestamped, over
        // rows earlier writes timestamped (RM-746).
        let _op = self.ddl_scope();
        // A type this engine does not build is refused before anything is
        // written, as the one-step build refuses it. The build over documents
        // (#2191) skipped the check, and a text index came back created.
        for s in specs {
            if let Some(refused) = crate::unsupported_index_type(&s.key, &s.options) {
                return Err(refused);
            }
        }
        // The collection exists, with its UUID, before anything is logged.
        self.create_collection(db, coll)?;
        let coll_uuid = self.collection_uuid(db, coll)?;
        if self.enable_oplog {
            let names: Vec<Bson> = specs.iter().map(|s| Bson::String(s.name.clone())).collect();
            let row = bson::doc! {
                "_id": uuid_binary(&uuid),
                "collectionUUID": uuid_binary(&coll_uuid),
                "commitQuorum": "votingMembers",
                "indexNames": names,
            };
            let (_, errors) = self.insert(CONFIG_DB, BUILDS_COLL, vec![encode_doc(&row)?], true)?;
            if let Some(e) = errors.first() {
                return Err(StorageError::Internal(format!(
                    "index build {}: recording it in {CONFIG_DB}.{BUILDS_COLL} failed: {e:?}",
                    uuid_text(&uuid)
                )));
            }
        }
        let set_up = self.set_up_index_build(db, coll, uuid, &coll_uuid, specs, member);
        if set_up.is_err() && self.enable_oplog {
            self.delete_by_id(CONFIG_DB, BUILDS_COLL, &uuid_binary(&uuid))?;
        }
        set_up
    }

    fn set_up_index_build(
        &self,
        db: &str,
        coll: &str,
        uuid: [u8; 16],
        coll_uuid: &[u8],
        specs: &[IndexBuildSpec],
        member: &str,
    ) -> Result<()> {
        // Planner writes made here are released once they are visible.
        let _planner = PlannerScope::new();
        note_planner_write();
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let ns_lock = self.coll_lock(db, coll);
        let _c = ns_lock.lock().unwrap_or_else(|e| e.into_inner());
        for other in self.index_builds.matching(Some(db), Some(coll)) {
            if let Some(ix) = other
                .indexes
                .iter()
                .find(|ix| specs.iter().any(|s| s.name == ix.name))
            {
                return Err(StorageError::IndexOptionsConflict(format!(
                    "Index build already in progress for index {}",
                    ix.name
                )));
            }
        }
        let build = self.retry_write_conflicts("begin_index_build", || {
            let session = self.op_session()?;
            self.with_statement_txn(&session, || {
                let mut indexes = Vec::with_capacity(specs.len());
                for (i, s) in specs.iter().enumerate() {
                    let Some(stored) = self.index_stored_options(
                        &session, db, coll, &s.name, &s.key, &s.options, i as i64,
                    )?
                    else {
                        return Err(StorageError::IndexOptionsConflict(format!(
                            "Index with name '{}' already exists",
                            s.name
                        )));
                    };
                    let stored = with_geo_options(stored, &s.key, &s.options);
                    indexes.push(BuildIndex::new(s.name.clone(), s.key.clone(), stored));
                }
                let mut build = ActiveBuild::new(uuid, db, coll, indexes)
                    .with_rid_bytes(is_clustered(&session, db, coll)?);
                build.member = member.to_string();
                let rec = session.open_cursor(BUILDS_TABLE, None)?;
                rec.set_key_u(&uuid);
                rec.set_value_u(&build_record(&build)?);
                rec.insert()?;
                if self.enable_oplog {
                    let mut o = Document::new();
                    o.insert("startIndexBuild", coll);
                    o.insert("indexBuildUUID", uuid_binary(&uuid));
                    o.insert("indexes", Bson::Array(build.oplog_specs()));
                    self.emit_oplog(&session, vec![build_entry(db, coll_uuid, o)], vec![None])?;
                }
                Ok(build)
            })
        })?;
        // Registered before the collection lock is let go: every write
        // after this one records itself for the build.
        self.index_builds.insert(Arc::new(build));
        Ok(())
    }

    /// Scan the collection into the build's indexes and apply the writes
    /// made meanwhile. Holds no lock: the collection takes writes
    /// throughout. `memory_limit` is the sorters' budget in bytes, shared
    /// between the build's indexes.
    pub fn run_index_build(&self, uuid: &[u8; 16], memory_limit: usize) -> Result<()> {
        let b = self.registered_build(uuid)?;
        b.phase.store(1, Ordering::Release);
        self.scan_and_load(&b, memory_limit)?;
        b.phase.store(3, Ordering::Release);
        loop {
            self.check_build(&b)?;
            if self.drain_side_writes(&b)? < DRAIN_BATCH {
                break;
            }
        }
        Ok(())
    }

    /// Record that the build is ready to commit
    /// (`config.system.indexBuilds`'s `commitReadyMembers`, as mongod's
    /// primary records itself).
    pub fn index_build_ready(&self, uuid: &[u8; 16], member: &str) -> Result<()> {
        if !self.enable_oplog {
            return Ok(());
        }
        let filter = bson::doc! {"_id": uuid_binary(uuid)};
        let update = bson::doc! {"$set": {"commitReadyMembers": [member]}};
        self.update_matching(
            CONFIG_DB,
            BUILDS_COLL,
            &filter,
            &update,
            false,
            false,
            &[],
            &Vars::new(),
            None,
            None,
            false,
        )?;
        Ok(())
    }

    /// Commit the build: the writes still recorded are applied, the
    /// documents that could not be keyed and the keys seen twice are
    /// checked again, and the indexes become part of the catalog with
    /// `commitIndexBuild`. An error leaves the build registered, for
    /// [`Self::abort_index_build`].
    pub fn commit_index_build(&self, uuid: &[u8; 16]) -> Result<()> {
        // One operation: what it writes after its entry shares the entry's
        // timestamp, and what it writes before it is untimestamped, over
        // rows earlier writes timestamped (RM-746).
        let _op = self.ddl_scope();
        let b = self.registered_build(uuid)?;
        let _finishing;
        {
            // Planner writes made here are released once they are visible.
            let _planner = PlannerScope::new();
            note_planner_write();
            let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let ns_lock = self.coll_lock(&b.db, &b.coll);
            let _c = ns_lock.lock().unwrap_or_else(|e| e.into_inner());
            if b.abort_requested().is_some() {
                return Err(StorageError::Internal(INDEX_BUILD_ABORTED.into()));
            }
            // No write runs now: what is recorded is everything.
            while self.drain_side_writes(&b)? > 0 {}
            self.rekey_skipped(&b)?;
            self.check_duplicates(&b)?;
            let _gen = self.ddl_generation_scope();
            self.retry_write_conflicts("commit_index_build", || {
                let session = self.op_session()?;
                self.with_statement_txn(&session, || {
                    for ix in &b.indexes {
                        let mut stored = ix.stored.clone();
                        if ix.multikey.load(Ordering::Relaxed) {
                            stored.insert("multikey", Bson::Boolean(true));
                            stored.insert(crate::MULTIKEY_PATHS, ix.recorded_paths());
                        }
                        let mut payload = Document::new();
                        payload.insert("key", Bson::Document(ix.key.clone()));
                        payload.insert("options", Bson::Document(stored));
                        let c = session.open_cursor(IDX_TABLE, None)?;
                        c.set_key_sss(&b.db, &b.coll, &ix.name);
                        c.set_value_u(&encode_doc(&payload)?);
                        c.insert()?;
                        // Its table is listed with the registry row, in the
                        // transaction that commits the index; a retry of
                        // this one keeps the table (the build still owns it).
                        let ident = ix
                            .ident
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone()
                            .ok_or_else(|| {
                                StorageError::Internal(format!(
                                    "index build: index {} has no table to commit",
                                    ix.name
                                ))
                            })?;
                        record_index_ident(
                            self, &session, &b.db, &b.coll, &ix.name, &ident, false,
                        )?;
                    }
                    // The catalog mongod reads says what the registry says.
                    sync_catalog_entry(&session, &b.db, &b.coll)?;
                    let rec = session.open_cursor(BUILDS_TABLE, None)?;
                    rec.set_key_u(&b.uuid);
                    rec.remove()?;
                    if self.enable_oplog {
                        let ui = collection_uuid(&session, &b.db, &b.coll)?;
                        let mut o = Document::new();
                        o.insert("commitIndexBuild", b.coll.clone());
                        o.insert("indexBuildUUID", uuid_binary(&b.uuid));
                        o.insert("indexes", Bson::Array(b.oplog_specs()));
                        self.emit_oplog(&session, vec![build_entry(&b.db, &ui, o)], vec![None])?;
                    }
                    Ok(())
                })
            })?;
            // The tables belong to the catalog now, not to the build.
            for ix in &b.indexes {
                ix.ident.lock().unwrap_or_else(|e| e.into_inner()).take();
            }
            // Unregistered under the collection lock: the next write keys
            // the new indexes through the catalog.
            _finishing = self.index_builds.remove(&b.uuid);
        }
        if self.enable_oplog {
            self.delete_by_id(CONFIG_DB, BUILDS_COLL, &uuid_binary(uuid))?;
        }
        Ok(())
    }

    /// Abort the build for `cause`: its entries and recorded writes are
    /// removed, `abortIndexBuild` is logged, and it is unregistered. A
    /// build no longer registered is a no-op.
    pub fn abort_index_build(&self, uuid: &[u8; 16], cause: &IndexBuildAbort) -> Result<()> {
        // One operation: what it writes after its entry shares the entry's
        // timestamp, and what it writes before it is untimestamped, over
        // rows earlier writes timestamped (RM-746).
        let _op = self.ddl_scope();
        let Some(b) = self.index_builds.get(uuid) else {
            return Ok(());
        };
        let _finishing;
        {
            // Planner writes made here are released once they are visible.
            let _planner = PlannerScope::new();
            note_planner_write();
            let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let ns_lock = self.coll_lock(&b.db, &b.coll);
            let _c = ns_lock.lock().unwrap_or_else(|e| e.into_inner());
            // No new write records itself for the build from here; a drop
            // waiting on it goes on only once its row's delete is logged,
            // as mongod's does.
            _finishing = self.index_builds.remove(&b.uuid);
            self.discard_build_rows(&b)?;
            self.retry_write_conflicts("abort_index_build", || {
                let session = self.op_session()?;
                self.with_statement_txn(&session, || {
                    let rec = session.open_cursor(BUILDS_TABLE, None)?;
                    rec.set_key_u(&b.uuid);
                    match rec.remove() {
                        Ok(()) => {}
                        Err(e) if e.is_not_found() => {}
                        Err(e) => return Err(e.into()),
                    }
                    if self.enable_oplog {
                        // A collection dropped since has no UUID to give.
                        let ui = coll_options(&session, &b.db, &b.coll)?.and_then(|o| {
                            match o.get("uuid") {
                                Some(Bson::Binary(u)) if u.bytes.len() == 16 => {
                                    Some(u.bytes.clone())
                                }
                                _ => None,
                            }
                        });
                        if let Some(ui) = ui {
                            let mut o = Document::new();
                            o.insert("abortIndexBuild", b.coll.clone());
                            o.insert("indexBuildUUID", uuid_binary(&b.uuid));
                            o.insert("indexes", Bson::Array(b.oplog_specs()));
                            o.insert("cause", Bson::Document(cause.cause()));
                            self.emit_oplog(
                                &session,
                                vec![build_entry(&b.db, &ui, o)],
                                vec![None],
                            )?;
                        }
                    }
                    Ok(())
                })
            })?;
        }
        if self.enable_oplog {
            self.delete_by_id(CONFIG_DB, BUILDS_COLL, &uuid_binary(uuid))?;
        }
        Ok(())
    }

    /// Ask every build over `db` (one collection of it, or one build) to
    /// abort for `cause`; the builds stop at their next check and abort
    /// themselves. Answers the builds asked, for
    /// [`Self::wait_index_builds_gone`].
    pub fn request_index_build_abort(
        &self,
        db: &str,
        coll: Option<&str>,
        uuid: Option<&[u8; 16]>,
        cause: &IndexBuildAbort,
    ) -> Vec<[u8; 16]> {
        let mut asked = Vec::new();
        for b in self.index_builds.matching(Some(db), coll) {
            if uuid.is_some_and(|u| u != &b.uuid) {
                continue;
            }
            let mut slot = b.abort.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_none() {
                *slot = Some(cause.clone());
            }
            asked.push(b.uuid);
        }
        asked
    }

    /// Wait until none of `uuids` is registered any more.
    pub fn wait_index_builds_gone(&self, uuids: &[[u8; 16]]) {
        let mut g = self
            .index_builds
            .gone_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        while uuids.iter().any(|u| self.index_builds.pending(u)) {
            g = self
                .index_builds
                .gone
                .wait_timeout(g, Duration::from_millis(50))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// The cause an abort was asked for with, while the build is
    /// registered.
    pub fn index_build_abort_cause(&self, uuid: &[u8; 16]) -> Option<IndexBuildAbort> {
        self.index_builds.get(uuid)?.abort_requested()
    }

    /// The builds in progress over `db` (one collection of it), or over
    /// every database.
    pub fn index_builds(&self, db: Option<&str>, coll: Option<&str>) -> Vec<IndexBuildInfo> {
        self.index_builds
            .matching(db, coll)
            .iter()
            .map(|b| b.info())
            .collect()
    }

    /// Abort every build over `(db, coll)` -- or every collection of `db`
    /// when `coll` is `None` -- for `cause`, and wait for them to finish
    /// aborting. What a drop does first: a build must not go on writing
    /// entries for a collection that is gone.
    pub(crate) fn abort_index_builds_and_wait(
        &self,
        db: &str,
        coll: Option<&str>,
        cause: &IndexBuildAbort,
    ) {
        if self.index_builds.active.load(Ordering::Acquire) == 0 {
            return;
        }
        let asked = self.request_index_build_abort(db, coll, None, cause);
        if !asked.is_empty() {
            self.wait_index_builds_gone(&asked);
        }
    }

    fn registered_build(&self, uuid: &[u8; 16]) -> Result<Arc<ActiveBuild>> {
        self.index_builds.get(uuid).ok_or_else(|| {
            StorageError::Internal(format!("no index build {} is running", uuid_text(uuid)))
        })
    }

    /// Stop when an abort was asked for or the operation's budget is spent.
    fn check_build(&self, b: &ActiveBuild) -> Result<()> {
        if b.abort_requested().is_some() {
            return Err(StorageError::Internal(INDEX_BUILD_ABORTED.into()));
        }
        if deadline::check_now().is_err() {
            return Err(StorageError::MaxTimeExpired);
        }
        Ok(())
    }

    /// Where the sorters spill: `<dbpath>/_tmp`, as mongod's do.
    fn build_tmp_dir(&self) -> std::path::PathBuf {
        if self.in_memory || self.home.is_empty() {
            std::env::temp_dir().join(format!("secantus-index-build-{}", std::process::id()))
        } else {
            std::path::Path::new(&self.home).join("_tmp")
        }
    }

    /// Phase 1 and 2: read every document in batches, key it into each
    /// index's sorter, then write the sorted keys to the entries table.
    fn scan_and_load(&self, b: &ActiveBuild, memory_limit: usize) -> Result<()> {
        let n = b.indexes.len().max(1);
        let per_index = (memory_limit / n).max(1);
        let dir = self.build_tmp_dir();
        let prefix = uuid_text(&b.uuid);
        // Each index's own table, which nothing lists until the commit.
        {
            let session = self.conn.open_session()?;
            for ix in &b.indexes {
                let mut ident = ix.ident.lock().unwrap_or_else(|e| e.into_inner());
                if ident.is_none() {
                    *ident = Some(make_index_table(self, &session, ix.unique())?);
                }
            }
        }
        let mut sorters: Vec<ExtSorter> = (0..b.indexes.len())
            .map(|i| ExtSorter::new(per_index, dir.clone(), format!("{prefix}-{i}")))
            .collect();
        let mut after: Option<RecordId> = None;
        let mut scanned = 0usize;
        loop {
            self.check_build(b)?;
            let batch = {
                let session = self.conn.open_session()?;
                self.scan_docs_batch(&session, &b.db, &b.coll, after.clone(), SCAN_BATCH)?
            };
            let Some(last) = batch.last() else {
                break;
            };
            after = Some(last.0.clone());
            for (rid, blob) in &batch {
                if deadline::check().is_err() {
                    return Err(StorageError::MaxTimeExpired);
                }
                let d = decode_doc(blob)?;
                for (i, ix) in b.indexes.iter().enumerate() {
                    if let Some(geo) = &ix.desc.geo_2d {
                        if let Some((code, why, names)) = secantus_core::geoshape::flat_key_problem(
                            get_path(&d, &geo.field),
                            geo.lo,
                            geo.hi,
                        ) {
                            return Err(scan_stopped(scanned, code, &named(why, names, &d)));
                        }
                    }
                    if let Some(gs) = &ix.desc.geo_sphere {
                        if let Some(why) = secantus_core::geoshape::sphere_key_problem(
                            &gs.field,
                            get_path(&d, &gs.field),
                        ) {
                            return Err(scan_stopped(scanned, 16755, &no_geo_keys(&d, &why)));
                        }
                    }
                    if self.doc_in_partial(&d, &ix.desc)? {
                        ix.note_multikey(&d);
                    }
                    for entry in self.packed_entry_keys(&d, &ix.desc, rid)? {
                        sorters[i].push(encode_entry(&entry))?;
                    }
                }
                scanned += 1;
            }
        }
        b.phase.store(2, Ordering::Release);
        for (i, sorter) in sorters.into_iter().enumerate() {
            let ix = &b.indexes[i];
            let spilled = sorter.spilled();
            if spilled > 0 {
                eprintln!(
                    "secantus-storage: index build {prefix}: index {} sorted externally in {} \
                     runs",
                    ix.name,
                    spilled + 1
                );
            }
            let mut keys = sorter.finish()?;
            let mut chunk: Vec<EntryKey> = Vec::new();
            let mut bytes = 0usize;
            let mut prev: Option<EntryKey> = None;
            let mut dups = BTreeSet::new();
            while let Some(item) = keys.next_key()? {
                let entry = decode_entry(&item)?;
                if prev.as_ref() == Some(&entry) {
                    continue;
                }
                if ix.unique() {
                    if let Some(p) = &prev {
                        if let (Some(pk), Some(k)) = (
                            ixkeys::entry_prefix(&p.key, ix.desc.rid_bytes),
                            ixkeys::entry_prefix(&entry.key, ix.desc.rid_bytes),
                        ) {
                            if pk == k {
                                dups.insert(k.to_vec());
                            }
                        }
                    }
                }
                bytes += entry.key.len() + entry.type_bits.len();
                chunk.push(entry.clone());
                prev = Some(entry);
                if chunk.len() >= INDEX_BUILD_CHUNK_MAX_ENTRIES
                    || bytes >= INDEX_BUILD_CHUNK_MAX_BYTES
                {
                    self.load_entries(b, ix, &chunk)?;
                    chunk.clear();
                    bytes = 0;
                    self.check_build(b)?;
                }
            }
            if !chunk.is_empty() {
                self.load_entries(b, ix, &chunk)?;
            }
            b.dup_candidates.lock().unwrap_or_else(|e| e.into_inner())[i].extend(dups);
        }
        Ok(())
    }

    /// One bounded transaction of sorted entries.
    fn load_entries(&self, b: &ActiveBuild, ix: &BuildIndex, chunk: &[EntryKey]) -> Result<()> {
        // A build writes its keys untimestamped, as mongod's bulk load
        // does: the index is not ready, and a build cut short by a crash
        // starts again from an empty table (RM-746).
        let _untimestamped = untimestamped();
        let table = ix.table()?;
        self.retry_write_conflicts("index_build_load", || {
            let session = self.op_session()?;
            self.with_statement_txn(&session, || {
                let cur = session.open_cursor(&table, Some("overwrite=false"))?;
                for entry in chunk {
                    self.put_index_entry(&session, &cur, &b.db, &b.coll, &ix.name, entry)?;
                }
                Ok(())
            })
        })
    }

    /// Apply up to [`DRAIN_BATCH`] recorded writes, oldest first, in one
    /// transaction that also removes them. Answers how many.
    fn drain_side_writes(&self, b: &ActiveBuild) -> Result<usize> {
        // Untimestamped, as the build's keys are (RM-746).
        let _untimestamped = untimestamped();
        self.retry_write_conflicts("index_build_drain", || {
            let session = self.op_session()?;
            self.with_statement_txn(&session, || {
                let scan = session.open_cursor(SIDE_TABLE, None)?;
                scan.set_key_u(&b.uuid);
                let mut more = match scan.search_near() {
                    Ok(cmp) => cmp >= 0 || scan.next()?,
                    Err(e) if e.is_not_found() => false,
                    Err(e) => return Err(e.into()),
                };
                let mut rows: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
                while more && rows.len() < DRAIN_BATCH {
                    let k = scan.get_key_u()?;
                    if !k.starts_with(&b.uuid) {
                        break;
                    }
                    rows.push((k, scan.get_value_u()?));
                    more = scan.next()?;
                }
                let del = session.open_cursor(SIDE_TABLE, None)?;
                let mut tables: HashMap<usize, Cursor> = HashMap::new();
                for (k, v) in &rows {
                    let (op, i, payload) = decode_side(v)?;
                    let Some(ix) = b.indexes.get(i) else {
                        return Err(StorageError::Internal(format!(
                            "index build {}: a side write names index {i} of {}",
                            uuid_text(&b.uuid),
                            b.indexes.len()
                        )));
                    };
                    if matches!(op, b'i' | b'd') && !tables.contains_key(&i) {
                        tables.insert(
                            i,
                            session.open_cursor(&ix.table()?, Some("overwrite=false"))?,
                        );
                    }
                    match op {
                        b'i' => {
                            let entry = decode_entry(payload)?;
                            self.put_index_entry(
                                &session,
                                &tables[&i],
                                &b.db,
                                &b.coll,
                                &ix.name,
                                &entry,
                            )?;
                            if ix.unique() {
                                let rid = ixkeys::entry_rid(&entry.key, ix.desc.rid_bytes)
                                    .unwrap_or(RecordId::Long(i64::MIN));
                                if let Some(prefix) =
                                    ixkeys::entry_prefix(&entry.key, ix.desc.rid_bytes)
                                {
                                    if self.key_held_by_another(&session, ix, prefix, &rid)? {
                                        b.dup_candidates.lock().unwrap_or_else(|e| e.into_inner())
                                            [i]
                                            .insert(prefix.to_vec());
                                    }
                                }
                            }
                        }
                        b'd' => {
                            let entry = decode_entry(payload)?;
                            self.drop_index_entry(
                                &session,
                                &tables[&i],
                                &b.db,
                                &b.coll,
                                &ix.name,
                                &entry.key,
                            )?;
                        }
                        b's' => {
                            let rid = if ix.desc.rid_bytes {
                                RecordId::Bytes(payload.to_vec())
                            } else {
                                let long = <[u8; 8]>::try_from(payload).map_err(|_| {
                                    StorageError::Internal(
                                        "index build: a skipped record without its RecordId".into(),
                                    )
                                })?;
                                RecordId::Long(i64::from_be_bytes(long))
                            };
                            b.skipped
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert((i, rid));
                        }
                        other => {
                            return Err(StorageError::Internal(format!(
                                "index build: unknown side write {other}"
                            )))
                        }
                    }
                    del.reset()?;
                    del.set_key_u(k);
                    del.remove()?;
                }
                Ok(rows.len())
            })
        })
    }

    /// Whether an entry of `ix` other than RecordId `rid`'s holds the key
    /// whose KeyString (with its end byte, no RecordId) is `prefix`.
    fn key_held_by_another(
        &self,
        session: &Session,
        ix: &BuildIndex,
        prefix: &[u8],
        rid: &RecordId,
    ) -> Result<bool> {
        Ok(self
            .rids_holding(session, ix, prefix, 2)?
            .iter()
            .any(|r| r != rid))
    }

    /// Up to `max` RecordIds whose entries of `ix` hold the key whose
    /// KeyString is `prefix`.
    fn rids_holding(
        &self,
        session: &Session,
        ix: &BuildIndex,
        prefix: &[u8],
        max: usize,
    ) -> Result<Vec<RecordId>> {
        let cur = session.open_cursor(&ix.table()?, None)?;
        let mut more = seek_ge(&cur, prefix)?;
        let mut out = Vec::new();
        while more && out.len() < max {
            let (starts, rid) = cur.with_key_u(|key| {
                (
                    key.starts_with(prefix),
                    ixkeys::entry_rid(key, ix.desc.rid_bytes),
                )
            })?;
            if !starts {
                break;
            }
            if let Some(r) = rid {
                out.push(r);
            }
            more = cur.next()?;
        }
        Ok(out)
    }

    /// The document at RecordId `rid`, if it is still there.
    fn build_doc_at(
        &self,
        session: &Session,
        b: &ActiveBuild,
        rid: &RecordId,
    ) -> Result<Option<Document>> {
        let cur = match open_doc_cursor(session, &b.db, &b.coll, None) {
            Ok(c) => c,
            Err(e) if e.is_missing_table() => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        cur.set_key_ssq(&b.db, &b.coll, rid);
        match cur.search() {
            Ok(()) => {
                let value = cur.get_value_u()?;
                let blob = value.as_slice();
                Ok(Some(decode_doc(blob)?))
            }
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Key again the documents that could not be keyed when they were
    /// written: a document still there that still cannot be fails the
    /// build, as mongod's skipped-record check does.
    fn rekey_skipped(&self, b: &ActiveBuild) -> Result<()> {
        let skipped: Vec<(usize, RecordId)> = b
            .skipped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect();
        if skipped.is_empty() {
            return Ok(());
        }
        self.retry_write_conflicts("index_build_skipped", || {
            let session = self.op_session()?;
            self.with_statement_txn(&session, || {
                let mut tables: HashMap<usize, Cursor> = HashMap::new();
                for (i, rid) in skipped.iter().map(|(i, r)| (*i, r.clone())) {
                    let ix = &b.indexes[i];
                    if let std::collections::hash_map::Entry::Vacant(slot) = tables.entry(i) {
                        slot.insert(session.open_cursor(&ix.table()?, Some("overwrite=false"))?);
                    }
                    let entries = &tables[&i];
                    let Some(d) = self.build_doc_at(&session, b, &rid)? else {
                        continue;
                    };
                    if let Some((code, errmsg)) =
                        geo_key_problem(&d, std::slice::from_ref(&ix.desc))
                    {
                        return Err(StorageError::QueryError {
                            code,
                            errmsg,
                            exec: false,
                        });
                    }
                    for entry in self.packed_entry_keys(&d, &ix.desc, &rid)? {
                        self.put_index_entry(&session, entries, &b.db, &b.coll, &ix.name, &entry)?;
                        if ix.unique() {
                            if let Some(prefix) =
                                ixkeys::entry_prefix(&entry.key, ix.desc.rid_bytes)
                            {
                                if self.key_held_by_another(&session, ix, prefix, &rid)? {
                                    b.dup_candidates.lock().unwrap_or_else(|e| e.into_inner())[i]
                                        .insert(prefix.to_vec());
                                }
                            }
                        }
                    }
                }
                Ok(())
            })
        })
    }

    /// Check again each key two documents were seen to share: one still
    /// held by two fails the build with mongod's duplicate-key error, for
    /// the least such key.
    fn check_duplicates(&self, b: &ActiveBuild) -> Result<()> {
        let candidates = b
            .dup_candidates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let session = self.conn.open_session()?;
        for (i, keys) in candidates.iter().enumerate() {
            let ix = &b.indexes[i];
            let shape = ix.desc.shape();
            for prefix in keys {
                let rids = self.rids_holding(&session, ix, prefix, 2)?;
                if rids.len() < 2 {
                    continue;
                }
                let Some(d) = self.build_doc_at(&session, b, &rids[0])? else {
                    continue;
                };
                // The sort key of the document's own key that this entry is.
                let Some(kb) = ix
                    .desc
                    .key_values(&d, ix.desc.sparse)?
                    .into_iter()
                    .find(|(_, values)| shape.exact(values).is_ok_and(|e| e == *prefix))
                    .map(|(kb, _)| kb)
                else {
                    continue;
                };
                return Err(StorageError::DuplicateKey(Box::new(with_bindata_order(
                    ix.desc.key_order,
                    || {
                        UniqueConflict::new(
                            format!("{}.{}", b.db, b.coll),
                            &ix.name,
                            &ix.key,
                            &d,
                            &kb,
                            ix.desc.collation.as_ref(),
                        )
                    },
                ))));
            }
        }
        Ok(())
    }

    /// Remove an aborted build's entries and recorded writes, in bounded
    /// transactions.
    fn discard_build_rows(&self, b: &ActiveBuild) -> Result<()> {
        // Each index's entries are in a table of its own, which nothing lists.
        for ix in &b.indexes {
            if let Some(ident) = ix.ident.lock().unwrap_or_else(|e| e.into_inner()).take() {
                self.drop_ident_table(&ident);
            }
        }
        self.discard_side_rows(&b.uuid)
    }

    /// Remove every recorded write of build `uuid`.
    fn discard_side_rows(&self, uuid: &[u8; 16]) -> Result<()> {
        // Untimestamped, as the build's keys are (RM-746).
        let _untimestamped = untimestamped();
        loop {
            let removed = self.retry_write_conflicts("index_build_side_purge", || {
                let session = self.op_session()?;
                self.with_statement_txn(&session, || {
                    let scan = session.open_cursor(SIDE_TABLE, None)?;
                    scan.set_key_u(uuid);
                    let mut more = match scan.search_near() {
                        Ok(cmp) => cmp >= 0 || scan.next()?,
                        Err(e) if e.is_not_found() => false,
                        Err(e) => return Err(e.into()),
                    };
                    let mut keys = Vec::new();
                    while more && keys.len() < PURGE_CHUNK_MAX_ROWS {
                        let k = scan.get_key_u()?;
                        if !k.starts_with(uuid) {
                            break;
                        }
                        keys.push(k);
                        more = scan.next()?;
                    }
                    let del = session.open_cursor(SIDE_TABLE, None)?;
                    for k in &keys {
                        del.reset()?;
                        del.set_key_u(k);
                        match del.remove() {
                            Ok(()) => {}
                            Err(e) if e.is_not_found() => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                    Ok(keys.len())
                })
            })?;
            if removed < PURGE_CHUNK_MAX_ROWS {
                return Ok(());
            }
        }
    }

    /// At open: restart every build a crash or shutdown interrupted, as
    /// mongod does, from the start -- its entries and recorded writes are
    /// discarded and the collection scanned again. Recorded writes of no
    /// build (a transaction that committed after its build ended) and the
    /// sorters' spilled runs are removed.
    pub(crate) fn restart_index_builds(&self) -> Result<()> {
        let dir = self.build_tmp_dir();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("extsort-index.")
                {
                    if let Err(e) = std::fs::remove_file(entry.path()) {
                        eprintln!(
                            "secantus-storage: could not remove {}: {e}",
                            entry.path().display()
                        );
                    }
                }
            }
        }
        let records: Vec<([u8; 16], Document)> = {
            let session = self.conn.open_session()?;
            let cur = session.open_cursor(BUILDS_TABLE, None)?;
            let mut out = Vec::new();
            let mut more = cur.next()?;
            while more {
                let k = cur.get_key_u()?;
                let v = cur.get_value_u()?;
                if let Ok(uuid) = <[u8; 16]>::try_from(k.as_slice()) {
                    out.push((uuid, decode_doc(&v)?));
                }
                more = cur.next()?;
            }
            out
        };
        // Side rows of builds that have no record.
        let orphans: BTreeSet<[u8; 16]> = {
            let session = self.conn.open_session()?;
            let cur = session.open_cursor(SIDE_TABLE, None)?;
            let mut out = BTreeSet::new();
            let mut more = cur.next()?;
            while more {
                let k = cur.get_key_u()?;
                if let Ok(uuid) = <[u8; 16]>::try_from(&k[..16.min(k.len())]) {
                    if !records.iter().any(|(u, _)| *u == uuid) {
                        out.insert(uuid);
                    }
                }
                more = cur.next()?;
            }
            out
        };
        for uuid in &orphans {
            self.discard_side_rows(uuid)?;
        }
        for (uuid, rec) in records {
            let Some(build) = build_from_record(uuid, &rec) else {
                eprintln!(
                    "secantus-storage: index build {} has an unreadable record; dropped",
                    uuid_text(&uuid)
                );
                self.forget_build_record(&uuid)?;
                continue;
            };
            let (exists, clustered) = {
                let session = self.conn.open_session()?;
                (
                    collection_registered(&session, &build.db, &build.coll)?,
                    is_clustered(&session, &build.db, &build.coll)?,
                )
            };
            let b = Arc::new(build.with_rid_bytes(clustered));
            self.index_builds.insert(Arc::clone(&b));
            if !exists {
                // Its collection is gone: nothing to build, nothing to log.
                let _finishing = self.index_builds.remove(&uuid);
                self.discard_build_rows(&b)?;
                self.forget_build_record(&uuid)?;
                continue;
            }
            eprintln!(
                "secantus-storage: restarting index build {} on {}.{}",
                uuid_text(&uuid),
                b.db,
                b.coll
            );
            self.discard_build_rows(&b)?;
            let built = self
                .run_index_build(&uuid, DEFAULT_BUILD_MEMORY_BYTES)
                .and_then(|()| {
                    if b.member.is_empty() {
                        Ok(())
                    } else {
                        self.index_build_ready(&uuid, &b.member)
                    }
                })
                .and_then(|()| self.commit_index_build(&uuid));
            if let Err(e) = built {
                let cause = match &e {
                    StorageError::DuplicateKey(c) => {
                        let mut a = IndexBuildAbort::new(11000, "DuplicateKey", c.errmsg());
                        a.extra
                            .insert("keyPattern", Bson::Document(c.key_pattern.clone()));
                        a.extra
                            .insert("keyValue", Bson::Document(c.key_value.clone()));
                        a
                    }
                    StorageError::QueryError { code, errmsg, .. } => {
                        IndexBuildAbort::new(*code, "Location", errmsg.clone())
                    }
                    other => IndexBuildAbort::new(1, "InternalError", format!("{other:?}")),
                };
                eprintln!(
                    "secantus-storage: restarted index build {} failed: {}",
                    uuid_text(&uuid),
                    cause.errmsg
                );
                self.abort_index_build(&uuid, &cause)?;
            }
        }
        Ok(())
    }

    /// Remove a build's record (and its `config.system.indexBuilds` row).
    fn forget_build_record(&self, uuid: &[u8; 16]) -> Result<()> {
        self.retry_write_conflicts("forget_index_build", || {
            let session = self.op_session()?;
            self.with_statement_txn(&session, || {
                let rec = session.open_cursor(BUILDS_TABLE, None)?;
                rec.set_key_u(uuid);
                match rec.remove() {
                    Ok(()) => Ok(()),
                    Err(e) if e.is_not_found() => Ok(()),
                    Err(e) => Err(e.into()),
                }
            })
        })?;
        self.delete_by_id(CONFIG_DB, BUILDS_COLL, &uuid_binary(uuid))?;
        Ok(())
    }

    /// Replay of `startIndexBuild` (crash recovery, a restore): record the
    /// build, so the open restarts it unless its commit or abort follows.
    pub(crate) fn replay_start_index_build(
        &self,
        db: &str,
        coll: &str,
        uuid: &[u8; 16],
        indexes: &[Document],
    ) -> Result<()> {
        self.retry_write_conflicts("replay_start_index_build", || {
            let session = self.op_session()?;
            self.with_statement_txn(&session, || {
                // The record a crash left (the table is logged) is the
                // build's own, with its member: it stays.
                let rec = session.open_cursor(BUILDS_TABLE, None)?;
                rec.set_key_u(uuid);
                match rec.search() {
                    Ok(()) => return Ok(()),
                    Err(e) if e.is_not_found() => {}
                    Err(e) => return Err(e.into()),
                }
                // The catalog options, worked out as the build worked them
                // out: the entry formats are not in the logged spec.
                let mut built = Vec::new();
                for (i, spec) in indexes.iter().enumerate() {
                    let (Ok(name), Ok(key)) = (spec.get_str("name"), spec.get_document("key"))
                    else {
                        continue;
                    };
                    let options: Document = spec
                        .iter()
                        .filter(|(k, _)| !matches!(k.as_str(), "v"))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    let Some(stored) = self
                        .index_stored_options(&session, db, coll, name, key, &options, i as i64)?
                    else {
                        continue;
                    };
                    let stored = with_geo_options(stored, key, &options);
                    built.push(BuildIndex::new(name.to_string(), key.clone(), stored));
                }
                let build = ActiveBuild::new(*uuid, db, coll, built)
                    .with_rid_bytes(is_clustered(&session, db, coll)?);
                rec.reset()?;
                rec.set_key_u(uuid);
                rec.set_value_u(&build_record(&build)?);
                rec.insert()?;
                Ok(())
            })
        })
    }

    /// Replay of `commitIndexBuild` / `abortIndexBuild`: the build's record
    /// goes (the commit's indexes are created by the caller).
    pub(crate) fn replay_end_index_build(&self, uuid: &[u8; 16]) -> Result<()> {
        self.retry_write_conflicts("replay_end_index_build", || {
            let session = self.op_session()?;
            self.with_statement_txn(&session, || {
                let rec = session.open_cursor(BUILDS_TABLE, None)?;
                rec.set_key_u(uuid);
                match rec.remove() {
                    Ok(()) => Ok(()),
                    Err(e) if e.is_not_found() => Ok(()),
                    Err(e) => Err(e.into()),
                }
            })
        })
    }
}

/// A geo index's catalog options as the one-step build records them: always
/// multikey, and a `2dsphere` index's format version.
fn with_geo_options(mut stored: Document, key: &Document, options: &Document) -> Document {
    if parse_geo_2d(key, options).is_some() {
        stored.insert("multikey", Bson::Boolean(true));
    }
    if parse_geo_sphere(key, options).is_some() {
        stored.insert("multikey", Bson::Boolean(true));
        stored
            .entry("2dsphereIndexVersion".to_string())
            .or_insert(Bson::Int32(3));
    }
    stored
}

/// A build's `c` oplog entry on `db`.
fn build_entry(db: &str, coll_uuid: &[u8], o: Document) -> Document {
    let mut entry = Document::new();
    entry.insert("op", "c");
    entry.insert("ns", format!("{db}.$cmd"));
    entry.insert("ui", uuid_binary(coll_uuid));
    entry.insert("o", Bson::Document(o));
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sorter_merges_its_spilled_runs_in_order() {
        let dir = std::env::temp_dir().join(format!("secantus-sorter-{}", std::process::id()));
        let mut s = ExtSorter::new(200, dir.clone(), "t".into());
        let mut want: Vec<Vec<u8>> = (0..500u32)
            .map(|i| {
                (i.wrapping_mul(2_654_435_761) % 1000)
                    .to_be_bytes()
                    .to_vec()
            })
            .collect();
        for k in &want {
            s.push(k.clone()).unwrap();
        }
        assert!(s.spilled() > 10, "{} runs", s.spilled());
        let mut keys = s.finish().unwrap();
        let mut got = Vec::new();
        while let Some(k) = keys.next_key().unwrap() {
            got.push(k);
        }
        want.sort();
        assert_eq!(got, want);
        drop(keys);
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert!(left.is_empty(), "run files left behind: {left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_side_write_reads_back() {
        let entry = KeyShape::new(&bson::doc! {"k": 1}, None, BinDataOrder::Mongod)
            .entry(&[Bson::String("key".into())], &RecordId::Long(7))
            .unwrap();
        let v = encode_side(3, &SideOp::Insert(&entry));
        let (op, i, p) = decode_side(&v).unwrap();
        assert_eq!((op, i), (b'i', 3));
        assert_eq!(decode_entry(p).unwrap(), entry);
        let v = encode_side(1, &SideOp::Skipped(&RecordId::Long(42)));
        let (op, i, p) = decode_side(&v).unwrap();
        assert_eq!(
            (op, i, i64::from_be_bytes(p.try_into().unwrap())),
            (b's', 1, 42)
        );
    }
}
