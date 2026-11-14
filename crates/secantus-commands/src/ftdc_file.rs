//! mongod's FTDC archive: the samples written to `<dbpath>/diagnostic.data`
//! (RM-417), in mongod's own file format, so its decoders read them.
//!
//! A file is a run of BSON documents, each `{_id: <date>, type: <n>, ...}`
//! (measured on 8.2.11, 2026-11-14):
//!
//! - `type: 0`, metadata, `doc: {start, buildInfo, getCmdLineOpts,
//!   hostInfo, end}`: the first document of every file;
//! - `type: 1`, a metric chunk, `data: BinData(0)`: a little-endian `u32`
//!   of the uncompressed length, then the zlib of the chunk's first sample
//!   (the reference document), a `u32` count of the metrics extracted from
//!   it, a `u32` count of the samples after it, and the samples as deltas,
//!   metric by metric, each an unsigned LEB128 varint, a run of zero deltas
//!   written as `0` and the run's length less one (`FTDCCompressor`).
//!
//! A chunk ends when it holds `diagnosticDataCollectionSamplesPerChunk`
//! samples or a sample's shape differs from its reference document. Every
//! `diagnosticDataCollectionSamplesPerInterimUpdate` samples the chunk so far
//! is rewritten to `metrics.interim`, which the next start copies into its
//! new file. A file past `diagnosticDataCollectionFileSizeMB` is closed and
//! the next sample opens `metrics.<UTC time>-<n>`; the oldest files are
//! removed while the directory is past `diagnosticDataCollectionDirectorySizeMB`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use bson::{doc, spec::BinarySubtype, Binary, Bson, DateTime, Document};

/// The interim file's name.
pub const INTERIM: &str = "metrics.interim";

/// The writer's limits, from the server's parameters.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub samples_per_chunk: usize,
    pub samples_per_interim: usize,
    pub file_bytes: u64,
    pub directory_bytes: u64,
}

impl Limits {
    /// The limits `params` set now, mongod's defaults without them.
    pub fn from_params(params: Option<&crate::params::ServerParams>) -> Self {
        let n =
            |name, default: i64| params.map_or(default, |p| p.number(name, default)).max(1) as u64;
        Self {
            samples_per_chunk: n(SAMPLES_PER_CHUNK, 300) as usize,
            samples_per_interim: n(SAMPLES_PER_INTERIM, 10) as usize,
            file_bytes: n(FILE_SIZE_MB, 10) * 1024 * 1024,
            directory_bytes: n(DIRECTORY_SIZE_MB, 250) * 1024 * 1024,
        }
    }
}

pub const DIRECTORY_PATH: &str = "diagnosticDataCollectionDirectoryPath";
pub const DIRECTORY_SIZE_MB: &str = "diagnosticDataCollectionDirectorySizeMB";
pub const FILE_SIZE_MB: &str = "diagnosticDataCollectionFileSizeMB";
pub const SAMPLES_PER_CHUNK: &str = "diagnosticDataCollectionSamplesPerChunk";
pub const SAMPLES_PER_INTERIM: &str = "diagnosticDataCollectionSamplesPerInterimUpdate";

/// The `diagnostic.data` directory of a store at `dbpath`.
pub fn directory_of(dbpath: &str) -> String {
    Path::new(dbpath)
        .join("diagnostic.data")
        .to_string_lossy()
        .into_owned()
}

/// Append `element`'s metrics to `out` (mongod's
/// `extractMetricsFromDocument`): a number as a 64-bit integer, a bool as
/// 0 or 1, a date as its milliseconds, a timestamp as its seconds and then
/// its increment, a document or array by its own elements.
fn metrics_of(value: &Bson, out: &mut Vec<i64>) {
    match value {
        Bson::Double(d) => out.push(*d as i64),
        Bson::Int32(n) => out.push(i64::from(*n)),
        Bson::Int64(n) => out.push(*n),
        Bson::Decimal128(d) => out.push(d.to_string().parse::<f64>().unwrap_or(0.0) as i64),
        Bson::Boolean(b) => out.push(i64::from(*b)),
        Bson::DateTime(t) => out.push(t.timestamp_millis()),
        Bson::Timestamp(t) => {
            out.push(i64::from(t.time));
            out.push(i64::from(t.increment));
        }
        Bson::Document(d) => d.values().for_each(|v| metrics_of(v, out)),
        Bson::Array(a) => a.iter().for_each(|v| metrics_of(v, out)),
        _ => {}
    }
}

fn is_metric(value: &Bson) -> bool {
    matches!(
        value,
        Bson::Double(_)
            | Bson::Int32(_)
            | Bson::Int64(_)
            | Bson::Decimal128(_)
            | Bson::Boolean(_)
            | Bson::DateTime(_)
            | Bson::Timestamp(_)
    )
}

/// Whether `sample` has `reference`'s shape: the same names in the same
/// order, metrics where it has metrics, and nested documents and arrays of
/// the same shape. A sample that does not starts a new chunk.
fn same_shape(reference: &Bson, sample: &Bson) -> bool {
    match (reference, sample) {
        (Bson::Document(r), Bson::Document(s)) => {
            r.len() == s.len()
                && r.iter()
                    .zip(s.iter())
                    .all(|((rk, rv), (sk, sv))| rk == sk && same_shape(rv, sv))
        }
        (Bson::Array(r), Bson::Array(s)) => {
            r.len() == s.len() && r.iter().zip(s).all(|(a, b)| same_shape(a, b))
        }
        (Bson::Document(_) | Bson::Array(_), _) | (_, Bson::Document(_) | Bson::Array(_)) => false,
        (r, s) => {
            is_metric(r) == is_metric(s)
                && matches!(r, Bson::Timestamp(_)) == matches!(s, Bson::Timestamp(_))
        }
    }
}

fn varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// The samples of one chunk: its reference document and the metrics of
/// every sample after it.
#[derive(Default)]
pub struct Chunk {
    reference: Option<Document>,
    reference_metrics: Vec<i64>,
    samples: Vec<Vec<i64>>,
}

impl Chunk {
    /// How many samples the chunk holds, its reference included.
    pub fn len(&self) -> usize {
        self.reference
            .as_ref()
            .map_or(0, |_| 1 + self.samples.len())
    }

    pub fn is_empty(&self) -> bool {
        self.reference.is_none()
    }

    /// Add `sample` if it has the reference's shape (or the chunk is
    /// empty); `false`, adding nothing, if it does not.
    pub fn add(&mut self, sample: &Document) -> bool {
        let mut metrics = Vec::new();
        metrics_of(&Bson::Document(sample.clone()), &mut metrics);
        match &self.reference {
            None => {
                self.reference = Some(sample.clone());
                self.reference_metrics = metrics;
                true
            }
            Some(r) => {
                let fits = metrics.len() == self.reference_metrics.len()
                    && same_shape(&Bson::Document(r.clone()), &Bson::Document(sample.clone()));
                if fits {
                    self.samples.push(metrics);
                }
                fits
            }
        }
    }

    /// The chunk as mongod's `type: 1` document.
    pub fn to_document(&self) -> io::Result<Document> {
        let reference = self
            .reference
            .as_ref()
            .expect("an empty chunk is never written");
        let mut raw = secantus_core::bsondoc::to_vec(reference).map_err(io::Error::other)?;
        raw.extend((self.reference_metrics.len() as u32).to_le_bytes());
        raw.extend((self.samples.len() as u32).to_le_bytes());
        let mut zeros = 0u64;
        for (i, first) in self.reference_metrics.iter().enumerate() {
            let mut previous = *first;
            for sample in &self.samples {
                let delta = sample[i].wrapping_sub(previous) as u64;
                previous = sample[i];
                if delta == 0 {
                    zeros += 1;
                    continue;
                }
                if zeros > 0 {
                    varint(0, &mut raw);
                    varint(zeros - 1, &mut raw);
                    zeros = 0;
                }
                varint(delta, &mut raw);
            }
        }
        if zeros > 0 {
            varint(0, &mut raw);
            varint(zeros - 1, &mut raw);
        }
        let mut data = (raw.len() as u32).to_le_bytes().to_vec();
        let mut z = flate2::write::ZlibEncoder::new(&mut data, flate2::Compression::default());
        z.write_all(&raw)?;
        z.finish()?;
        let id = match reference.get("start") {
            Some(Bson::DateTime(t)) => *t,
            _ => DateTime::now(),
        };
        Ok(doc! {
            "_id": id,
            "type": 1_i32,
            "data": Binary { subtype: BinarySubtype::Generic, bytes: data },
        })
    }
}

/// The open archive file.
struct Archive {
    file: File,
    size: u64,
}

/// The writer of one `diagnostic.data` directory.
pub struct Writer {
    dir: PathBuf,
    archive: Option<Archive>,
    chunk: Chunk,
    since_interim: usize,
}

fn append(file: &mut File, document: &Document) -> io::Result<u64> {
    let bytes = secantus_core::bsondoc::to_vec(document).map_err(io::Error::other)?;
    file.write_all(&bytes)?;
    Ok(bytes.len() as u64)
}

impl Writer {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            archive: None,
            chunk: Chunk::default(),
            since_interim: 0,
        }
    }

    /// The archive file's name for a file opened at `now`: mongod's
    /// `metrics.<YYYY-MM-DDTHH-MM-SSZ>-<n>`, `n` counting past a name in use.
    fn archive_name(&self, now: DateTime) -> PathBuf {
        let stamp = now
            .try_to_rfc3339_string()
            .unwrap_or_default()
            .get(..19)
            .unwrap_or("")
            .replace(':', "-");
        (0..)
            .map(|n| self.dir.join(format!("metrics.{stamp}Z-{n:05}")))
            .find(|p| !p.exists())
            .expect("an unused name")
    }

    /// Open a new archive file, its metadata first and then the chunk a
    /// previous run left in `metrics.interim`.
    fn open(&mut self, metadata: &Document) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let now = DateTime::now();
        let path = self.archive_name(now);
        let mut file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(path)?;
        let mut size = append(
            &mut file,
            &doc! {"_id": now, "type": 0_i32, "doc": metadata.clone()},
        )?;
        let interim = self.dir.join(INTERIM);
        if self.chunk.is_empty() && interim.exists() {
            let bytes = fs::read(&interim)?;
            file.write_all(&bytes)?;
            size += bytes.len() as u64;
            fs::remove_file(&interim)?;
        }
        file.sync_data()?;
        self.archive = Some(Archive { file, size });
        Ok(())
    }

    /// Write the chunk to the archive and start the next one.
    fn flush(&mut self) -> io::Result<()> {
        if self.chunk.is_empty() {
            return Ok(());
        }
        let document = self.chunk.to_document()?;
        let archive = self.archive.as_mut().expect("opened before a flush");
        archive.size += append(&mut archive.file, &document)?;
        archive.file.sync_data()?;
        self.chunk = Chunk::default();
        self.since_interim = 0;
        let interim = self.dir.join(INTERIM);
        if interim.exists() {
            fs::remove_file(interim)?;
        }
        Ok(())
    }

    /// Write `sample`; `metadata` is asked for when a file is opened.
    pub fn write(
        &mut self,
        sample: &Document,
        limits: Limits,
        metadata: impl FnOnce() -> Document,
    ) -> io::Result<()> {
        if self.archive.is_none() {
            self.open(&metadata())?;
        }
        if !self.chunk.add(sample) {
            self.flush()?;
            self.chunk.add(sample);
        }
        if self.chunk.len() >= limits.samples_per_chunk.max(2) {
            self.flush()?;
        } else {
            self.since_interim += 1;
            if self.since_interim >= limits.samples_per_interim {
                self.since_interim = 0;
                let document = self.chunk.to_document()?;
                let tmp = self.dir.join("metrics.interim.temp");
                fs::write(
                    &tmp,
                    secantus_core::bsondoc::to_vec(&document).map_err(io::Error::other)?,
                )?;
                fs::rename(tmp, self.dir.join(INTERIM))?;
            }
        }
        if self
            .archive
            .as_ref()
            .is_some_and(|a| a.size >= limits.file_bytes)
        {
            self.archive = None;
            self.prune(limits.directory_bytes)?;
        }
        Ok(())
    }

    /// Remove the oldest archive files while the directory is past `limit`.
    fn prune(&self, limit: u64) -> io::Result<()> {
        let mut files: Vec<(String, u64)> = fs::read_dir(&self.dir)?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let archive = name.starts_with("metrics.") && !name.starts_with(INTERIM);
                archive.then(|| (name, e.metadata().map(|m| m.len()).unwrap_or(0)))
            })
            .collect();
        files.sort();
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        for (name, size) in files {
            if total <= limit {
                break;
            }
            fs::remove_file(self.dir.join(name))?;
            total -= size;
        }
        Ok(())
    }

    /// Write the chunk in progress, as mongod does when it stops.
    pub fn close(&mut self) -> io::Result<()> {
        if self.archive.is_some() {
            self.flush()?;
        }
        self.archive = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn limits(chunk: usize, interim: usize) -> Limits {
        Limits {
            samples_per_chunk: chunk,
            samples_per_interim: interim,
            file_bytes: 10 << 20,
            directory_bytes: 250 << 20,
        }
    }

    fn decode_chunk(d: &Document) -> (Document, u32, u32, Vec<u8>) {
        let bytes = &d.get_binary_generic("data").unwrap()[..];
        let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(&bytes[4..])
            .read_to_end(&mut raw)
            .unwrap();
        assert_eq!(raw.len(), length);
        let reference = secantus_core::bsondoc::from_bytes(&raw).unwrap();
        let at = secantus_core::bsondoc::to_vec(&reference).unwrap().len();
        let n = |i: usize| u32::from_le_bytes(raw[i..i + 4].try_into().unwrap());
        (reference, n(at), n(at + 4), raw[at + 8..].to_vec())
    }

    fn documents(path: &Path) -> Vec<Document> {
        let bytes = fs::read(path).unwrap();
        let mut out = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let d = secantus_core::bsondoc::from_bytes(&bytes[at..]).unwrap();
            at += secantus_core::bsondoc::to_vec(&d).unwrap().len();
            out.push(d);
        }
        out
    }

    #[test]
    fn deltas_are_varints_by_metric_with_zero_runs() {
        // mongod 8.2.11 wrote `dc07 ee07 e007 00 02 01 01 01` for a `start`
        // advancing 988, 1006 and 992 ms, three unchanged metrics, then one
        // rising by 1 each sample.
        let mut chunk = Chunk::default();
        let mut t = 1_000_000;
        for (i, step) in [0, 988, 1006, 992].into_iter().enumerate() {
            t += step;
            chunk.add(&doc! {
                "start": DateTime::from_millis(t),
                "a": 5_i32, "b": true, "c": 7_i64, "d": i as i64,
            });
        }
        let (reference, metrics, samples, deltas) = decode_chunk(&chunk.to_document().unwrap());
        assert_eq!(reference.get_i32("a").unwrap(), 5);
        assert_eq!((metrics, samples), (5, 3));
        let mut want = Vec::new();
        for v in [988, 1006, 992, 0, 8, 1, 1, 1] {
            varint(v, &mut want);
        }
        assert_eq!(deltas, want);
    }

    #[test]
    fn a_sample_of_another_shape_starts_a_chunk() {
        let mut chunk = Chunk::default();
        assert!(chunk.add(&doc! {"a": 1_i32, "s": "x"}));
        assert!(chunk.add(&doc! {"a": 2_i64, "s": "y"}));
        assert!(!chunk.add(&doc! {"b": 2_i32, "s": "y"}));
        assert!(!chunk.add(&doc! {"a": "no", "s": "y"}));
        assert!(!chunk.add(&doc! {"a": {"t": bson::Timestamp {time: 1, increment: 2}}, "s": "y"}));
    }

    #[test]
    fn a_file_starts_with_metadata_and_holds_full_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Writer::new(dir.path());
        for i in 0..5_i32 {
            w.write(&doc! {"n": i}, limits(2, 10), || doc! {"buildInfo": {}})
                .unwrap();
        }
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        let docs = documents(&dir.path().join(&names[0]));
        assert_eq!(
            docs.iter()
                .map(|d| d.get_i32("type").unwrap())
                .collect::<Vec<_>>(),
            [0, 1, 1]
        );
        w.close().unwrap();
        let docs = documents(&dir.path().join(&names[0]));
        assert_eq!(docs.len(), 4);
        assert_eq!(decode_chunk(&docs[3]).0, doc! {"n": 4});
    }

    #[test]
    fn the_interim_file_is_carried_into_the_next_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Writer::new(dir.path());
        for i in 0..3_i32 {
            w.write(&doc! {"n": i}, limits(300, 2), Document::new)
                .unwrap();
        }
        assert!(dir.path().join(INTERIM).exists());
        drop(w); // a crash: nothing flushed
        let mut next = Writer::new(dir.path());
        next.write(&doc! {"n": 9}, limits(300, 2), Document::new)
            .unwrap();
        assert!(!dir.path().join(INTERIM).exists());
        let carried: Vec<Document> = fs::read_dir(dir.path())
            .unwrap()
            .flat_map(|e| documents(&e.unwrap().path()))
            .filter(|d| d.get_i32("type") == Ok(1))
            .collect();
        assert_eq!(carried.len(), 1);
        assert_eq!(decode_chunk(&carried[0]).2, 1, "the interim's two samples");
    }

    #[test]
    fn a_full_file_rotates_and_the_directory_is_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Writer::new(dir.path());
        let small = Limits {
            samples_per_chunk: 2,
            samples_per_interim: 100,
            file_bytes: 1,
            directory_bytes: 1,
        };
        for i in 0..6_i32 {
            w.write(&doc! {"n": i}, small, Document::new).unwrap();
        }
        let left = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(left, 0, "every closed file is past the directory's limit");
    }

    #[test]
    fn archive_names_are_utc_seconds_and_count_past_a_clash() {
        let dir = tempfile::tempdir().unwrap();
        let w = Writer::new(dir.path());
        let t = DateTime::from_millis(1_794_121_279_040);
        let first = w.archive_name(t);
        assert_eq!(
            first.file_name().unwrap(),
            "metrics.2026-11-08T07-01-19Z-00000"
        );
        fs::write(&first, b"").unwrap();
        assert_eq!(
            w.archive_name(t).file_name().unwrap(),
            "metrics.2026-11-08T07-01-19Z-00001"
        );
    }
}
