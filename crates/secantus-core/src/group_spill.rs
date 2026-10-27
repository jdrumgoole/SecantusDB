//! A `$group` over its memory budget, spilled to disk and merged back, as
//! mongod 8.2 does it (RM-211).
//!
//! When the groups held in memory pass 100 MiB and the command allows disk
//! use, mongod sorts them by key, writes each group's partial accumulator
//! states to a spill file and starts again with none in memory
//! (`GroupProcessor::spill`, SBE's `HashAggStage::spill`). At the end it
//! spills what is left, merges the sorted runs key by key -- folding each
//! key's partial states together in the order they were written -- and
//! hands the groups on in KEY ORDER, where a group that never spilled comes
//! out in the hash table's order. Measured on mongod 8.2.11 (2026-10-26):
//! the same answers either way, and the key order once spilled.
//!
//! A partial state is written as a small BSON document per accumulator and
//! read back into the same state, so a merged `$sum` adds the two
//! double-double totals and a merged `$push` concatenates in input order --
//! what mongod's `getValue(toBeMerged)` / `process(merging)` pair does.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use bson::{doc, Bson, Document};

use crate::fallback::Fallback;
use crate::group::{
    const_add, finalize, gkey, update_extreme, Acc, Capped, ConstSum, NElemKind, Num, TopNKind,
    Width,
};
use crate::summation::DoubleDoubleSum;
use crate::{decimal, memlimit, order};

type R<T> = Result<T, Fallback>;

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

/// An I/O failure writing or reading a spill file: the query fails with it,
/// never quietly loses groups.
fn io_error(what: &str, path: &std::path::Path, e: impl std::fmt::Display) -> Fallback {
    Fallback::mongo(
        1,
        format!(
            "$group could not {what} its spill file {}: {e}",
            path.display()
        ),
    )
}

/// One sorted run of spilled groups.
struct Run {
    path: PathBuf,
    groups: usize,
}

/// The runs a `$group` has spilled. The files are removed when merged, or
/// when the group fails and this is dropped.
#[derive(Default)]
pub(crate) struct Spills {
    runs: Vec<Run>,
}

impl Spills {
    pub(crate) fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
}

impl Drop for Spills {
    fn drop(&mut self) {
        for run in self.runs.drain(..) {
            // Only reached when the group failed part way (a merged run's
            // file is removed with its error checked); the failure is
            // already the query's error.
            if let Err(e) = std::fs::remove_file(&run.path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "secantus: could not remove $group spill file {}: {e}",
                        run.path.display()
                    );
                }
            }
        }
    }
}

/// Write the groups in memory to a new run, sorted by key.
/// `held`: mongod's estimate of the groups' memory, which it reports as the
/// spill's `spilledBytes` (the file's size is `spilledDataStorageSize`).
pub(crate) fn spill(
    ids: Vec<Bson>,
    states: Vec<Vec<Acc>>,
    spills: &mut Spills,
    held: usize,
) -> R<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut order: Vec<usize> = (0..ids.len()).collect();
    order.sort_by(|&a, &b| order::cmp(&ids[a], &ids[b]));
    let mut groups: Vec<Option<(Bson, Vec<Acc>)>> = ids.into_iter().zip(states).map(Some).collect();
    let dir = memlimit::spill_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(io_error("create the directory for", &dir, e));
    }
    let path = dir.join(format!(
        "secantus-group-{}-{}.bson",
        std::process::id(),
        NEXT_FILE.fetch_add(1, AtomicOrdering::Relaxed)
    ));
    let file = File::create(&path).map_err(|e| io_error("create", &path, e))?;
    // Registered before anything is written, so a failure below removes it.
    spills.runs.push(Run {
        path: path.clone(),
        groups: 0,
    });
    let mut out = BufWriter::new(file);
    let mut bytes: i64 = 0;
    let mut records: i64 = 0;
    for i in order {
        let (id, accs) = groups[i].take().expect("each group is written once");
        let mut states = Vec::with_capacity(accs.len());
        for acc in accs {
            states.push(Bson::Document(acc_to_doc(acc)?));
        }
        let record = doc! { "k": id, "s": states };
        let mut buf = Vec::new();
        record
            .to_writer(&mut buf)
            .map_err(|e| io_error("encode a group for", &path, e))?;
        out.write_all(&buf)
            .map_err(|e| io_error("write", &path, e))?;
        bytes += buf.len() as i64;
        records += 1;
    }
    out.flush().map_err(|e| io_error("write", &path, e))?;
    let file = out
        .into_inner()
        .map_err(|e| io_error("write", &path, e.error()))?;
    file.sync_all().map_err(|e| io_error("write", &path, e))?;
    let stored = file
        .metadata()
        .map_err(|e| io_error("measure", &path, e))?
        .len() as i64;
    if let Some(run) = spills.runs.last_mut() {
        run.groups = records as usize;
    }
    memlimit::record_group_spill(records, held as i64, stored.max(bytes));
    Ok(())
}

/// A run being read back: its file and the group at its head.
struct Reader {
    path: PathBuf,
    input: BufReader<File>,
    left: usize,
    head: Option<(Bson, Vec<Acc>)>,
}

impl Reader {
    fn advance(&mut self) -> R<()> {
        self.head = None;
        if self.left == 0 {
            return Ok(());
        }
        self.left -= 1;
        // Each record is one BSON document: its length, then the rest.
        let mut len = [0u8; 4];
        self.input
            .read_exact(&mut len)
            .map_err(|e| io_error("read", &self.path, e))?;
        let size = usize::try_from(i32::from_le_bytes(len))
            .ok()
            .filter(|n| *n >= 5)
            .ok_or_else(|| io_error("read", &self.path, "a record with a bad length"))?;
        let mut bytes = vec![0u8; size];
        bytes[..4].copy_from_slice(&len);
        self.input
            .read_exact(&mut bytes[4..])
            .map_err(|e| io_error("read", &self.path, e))?;
        let record =
            crate::bsondoc::from_bytes(&bytes).map_err(|e| io_error("read", &self.path, e))?;
        let id = record
            .get("k")
            .cloned()
            .ok_or_else(|| io_error("read", &self.path, "a group without its key"))?;
        let states = record
            .get_array("s")
            .map_err(|e| io_error("read", &self.path, e))?;
        let mut accs = Vec::with_capacity(states.len());
        for s in states {
            let Bson::Document(d) = s else {
                return Err(io_error(
                    "read",
                    &self.path,
                    "a state that is not a document",
                ));
            };
            accs.push(acc_from_doc(d)?);
        }
        self.head = Some((id, accs));
        Ok(())
    }
}

/// Merge the runs into the finished groups, in key order. `fields` names
/// each accumulator's output field.
pub(crate) fn merge(mut spills: Spills, fields: &[&str]) -> R<Vec<Document>> {
    let runs = std::mem::take(&mut spills.runs);
    let mut readers = Vec::with_capacity(runs.len());
    // Hand the paths back so the files are removed if opening one fails.
    spills.runs = runs
        .iter()
        .map(|r| Run {
            path: r.path.clone(),
            groups: r.groups,
        })
        .collect();
    for run in &runs {
        let file = File::open(&run.path).map_err(|e| io_error("open", &run.path, e))?;
        let mut reader = Reader {
            path: run.path.clone(),
            input: BufReader::new(file),
            left: run.groups,
            head: None,
        };
        reader.advance()?;
        readers.push(reader);
    }
    let mut out = Vec::new();
    loop {
        crate::deadline::check()?;
        // The least key at any head; equal keys merge in run order, which
        // is the order their documents arrived in.
        let mut least: Option<usize> = None;
        for (i, r) in readers.iter().enumerate() {
            let Some((id, _)) = &r.head else { continue };
            match least {
                None => least = Some(i),
                Some(l) => {
                    let (lid, _) = readers[l].head.as_ref().expect("a head");
                    if order::cmp(id, lid) == std::cmp::Ordering::Less {
                        least = Some(i);
                    }
                }
            }
        }
        let Some(first) = least else { break };
        let key = gkey(&readers[first].head.as_ref().expect("a head").0)?;
        let mut merged: Option<(Bson, Vec<Acc>)> = None;
        for r in readers.iter_mut() {
            let same = match &r.head {
                Some((id, _)) => gkey(id)? == key,
                None => false,
            };
            if !same {
                continue;
            }
            let (id, accs) = r.head.take().expect("a head");
            merged = Some(match merged {
                None => (id, accs),
                Some((kept, mut into)) => {
                    for (acc, other) in into.iter_mut().zip(accs) {
                        merge_acc(acc, other)?;
                    }
                    (kept, into)
                }
            });
            r.advance()?;
        }
        let (id, accs) = merged.expect("the least head merged");
        out.push(finalize(id, fields.iter().copied().zip(accs).collect())?);
    }
    drop(readers);
    for run in std::mem::take(&mut spills.runs) {
        std::fs::remove_file(&run.path).map_err(|e| io_error("remove", &run.path, e))?;
    }
    Ok(out)
}

fn width_code(w: Width) -> i32 {
    match w {
        Width::Int => 0,
        Width::Long => 1,
        Width::Double => 2,
        Width::Decimal => 3,
    }
}

fn width_of(code: i32) -> R<Width> {
    Ok(match code {
        0 => Width::Int,
        1 => Width::Long,
        2 => Width::Double,
        3 => Width::Decimal,
        _ => return Err(Fallback::Defer),
    })
}

fn num_to_doc(n: Num) -> R<Document> {
    Ok(match n {
        Num::Const(ConstSum::Int(i)) => doc! { "c": i },
        Num::Const(ConstSum::Long(i)) => doc! { "c": i },
        Num::Const(ConstSum::Double(d)) => doc! { "c": d },
        Num::Sum {
            width,
            total,
            decimal: dec,
        } => {
            let (s, a, x) = total.parts();
            let mut d = doc! { "w": width_code(width), "s": s, "a": a, "x": x };
            if let Some(dec) = dec {
                d.insert("d", decimal::to_bson(&dec).ok_or(Fallback::Defer)?);
            }
            d
        }
    })
}

fn num_from_doc(d: &Document) -> R<Num> {
    if let Some(c) = d.get("c") {
        return Ok(Num::Const(match c {
            Bson::Int32(i) => ConstSum::Int(*i),
            Bson::Int64(i) => ConstSum::Long(*i),
            Bson::Double(f) => ConstSum::Double(*f),
            _ => return Err(Fallback::Defer),
        }));
    }
    let f = |k: &str| d.get_f64(k).map_err(|_| Fallback::Defer);
    Ok(Num::Sum {
        width: width_of(d.get_i32("w").map_err(|_| Fallback::Defer)?)?,
        total: DoubleDoubleSum::from_parts(f("s")?, f("a")?, f("x")?),
        decimal: match d.get("d") {
            Some(b) => Some(decimal::from_bson(b).ok_or(Fallback::Defer)?),
            None => None,
        },
    })
}

fn const_bson(c: ConstSum) -> Bson {
    match c {
        ConstSum::Int(i) => Bson::Int32(i),
        ConstSum::Long(i) => Bson::Int64(i),
        ConstSum::Double(d) => Bson::Double(d),
    }
}

/// Fold partial total `other` into `into`.
fn merge_num(into: Num, other: Num) -> R<Num> {
    Ok(match (into, other) {
        (
            Num::Sum {
                width,
                mut total,
                decimal: dec,
            },
            Num::Sum {
                width: w2,
                total: t2,
                decimal: d2,
            },
        ) => {
            total.merge(&t2);
            let decimal = match (dec, d2) {
                (Some(a), Some(b)) => Some(decimal::add(&a, &b).ok_or(Fallback::Defer)?),
                (a, b) => a.or(b),
            };
            Num::Sum {
                width: width.max(w2),
                total,
                decimal,
            }
        }
        (Num::Const(a), Num::Const(b)) => {
            // Widen to the wider of the two, then add as the constant sum
            // adds a value.
            let a = match (a, b) {
                (ConstSum::Int(x), ConstSum::Long(_)) => ConstSum::Long(i64::from(x)),
                (ConstSum::Int(x), ConstSum::Double(_)) => ConstSum::Double(f64::from(x)),
                (ConstSum::Long(x), ConstSum::Double(_)) => ConstSum::Double(x as f64),
                (a, _) => a,
            };
            Num::Const(const_add(a, &const_bson(b))?)
        }
        _ => return Err(Fallback::Defer),
    })
}

fn capped_to_doc(t: &str, c: Capped) -> Document {
    doc! { "t": t, "v": c.items, "m": c.mem as i64 }
}

fn capped_from_doc(d: &Document) -> R<Capped> {
    Ok(Capped {
        items: d.get_array("v").map_err(|_| Fallback::Defer)?.clone(),
        mem: d.get_i64("m").map_err(|_| Fallback::Defer)? as usize,
    })
}

fn optional(t: &str, v: Option<Bson>) -> Document {
    let mut d = doc! { "t": t };
    if let Some(v) = v {
        d.insert("v", v);
    }
    d
}

fn doubles(values: Vec<f64>) -> Bson {
    Bson::Array(values.into_iter().map(Bson::Double).collect())
}

fn doubles_of(d: &Document, k: &str) -> R<Vec<f64>> {
    d.get_array(k)
        .map_err(|_| Fallback::Defer)?
        .iter()
        .map(|v| v.as_f64().ok_or(Fallback::Defer))
        .collect()
}

fn nelem_code(k: NElemKind) -> i32 {
    match k {
        NElemKind::First => 0,
        NElemKind::Last => 1,
        NElemKind::Max => 2,
        NElemKind::Min => 3,
    }
}

fn topn_code(k: TopNKind) -> i32 {
    match k {
        TopNKind::Top => 0,
        TopNKind::Bottom => 1,
        TopNKind::TopN => 2,
        TopNKind::BottomN => 3,
    }
}

fn opt_n(n: Option<usize>) -> Bson {
    n.map_or(Bson::Null, |n| Bson::Int64(n as i64))
}

fn n_of(d: &Document) -> Option<usize> {
    d.get_i64("n").ok().map(|n| n as usize)
}

/// One accumulator's partial state, as written to a spill file.
fn acc_to_doc(acc: Acc) -> R<Document> {
    Ok(match acc {
        Acc::Sum(n) => doc! { "t": "sum", "n": num_to_doc(n)? },
        Acc::Count(n) => doc! { "t": "count", "n": n },
        Acc::Avg(None) => doc! { "t": "avg" },
        Acc::Avg(Some((n, c))) => doc! { "t": "avg", "n": num_to_doc(n)?, "c": c },
        Acc::Min(v) => optional("min", v),
        Acc::Max(v) => optional("max", v),
        Acc::First(v) => optional("first", v),
        Acc::Last(v) => optional("last", v),
        Acc::Push(c) => capped_to_doc("push", c),
        Acc::AddToSet(c) => capped_to_doc("addToSet", c),
        Acc::ConcatArrays(c) => capped_to_doc("concatArrays", c),
        Acc::SetUnion(c) => capped_to_doc("setUnion", c),
        Acc::MergeObjects(d) => doc! { "t": "mergeObjects", "v": d },
        Acc::StdDev { values, pop } => doc! { "t": "stdDev", "v": doubles(values), "p": pop },
        Acc::Percentile {
            is_median,
            ps,
            values,
        } => {
            let mut d = doc! { "t": "percentile", "m": is_median, "v": doubles(values) };
            if let Some(ps) = ps {
                d.insert("ps", doubles(ps));
            }
            d
        }
        Acc::NElem { kind, n, vals } => {
            doc! { "t": "nElem", "k": nelem_code(kind), "n": opt_n(n), "v": vals }
        }
        Acc::TopN {
            kind,
            n,
            dirs,
            items,
        } => {
            let items: Vec<Bson> = items
                .into_iter()
                .map(|(keys, out)| Bson::Array(vec![Bson::Array(keys), out]))
                .collect();
            doc! { "t": "topN", "k": topn_code(kind), "n": opt_n(n), "d": dirs, "v": items }
        }
        // A `$group` with `$accumulator` fails on its first document.
        Acc::Script => return Err(Fallback::Defer),
    })
}

fn acc_from_doc(d: &Document) -> R<Acc> {
    let t = d.get_str("t").map_err(|_| Fallback::Defer)?;
    let n = || d.get_document("n").map_err(|_| Fallback::Defer);
    let v = || d.get("v").cloned();
    Ok(match t {
        "sum" => Acc::Sum(num_from_doc(n()?)?),
        "count" => Acc::Count(d.get_i64("n").map_err(|_| Fallback::Defer)?),
        "avg" => match d.get_document("n") {
            Ok(num) => Acc::Avg(Some((
                num_from_doc(num)?,
                d.get_i64("c").map_err(|_| Fallback::Defer)?,
            ))),
            Err(_) => Acc::Avg(None),
        },
        "min" => Acc::Min(v()),
        "max" => Acc::Max(v()),
        "first" => Acc::First(v()),
        "last" => Acc::Last(v()),
        "push" => Acc::Push(capped_from_doc(d)?),
        "addToSet" => Acc::AddToSet(capped_from_doc(d)?),
        "concatArrays" => Acc::ConcatArrays(capped_from_doc(d)?),
        "setUnion" => Acc::SetUnion(capped_from_doc(d)?),
        "mergeObjects" => {
            Acc::MergeObjects(d.get_document("v").map_err(|_| Fallback::Defer)?.clone())
        }
        "stdDev" => Acc::StdDev {
            values: doubles_of(d, "v")?,
            pop: d.get_bool("p").map_err(|_| Fallback::Defer)?,
        },
        "percentile" => Acc::Percentile {
            is_median: d.get_bool("m").map_err(|_| Fallback::Defer)?,
            ps: match d.get("ps") {
                Some(_) => Some(doubles_of(d, "ps")?),
                None => None,
            },
            values: doubles_of(d, "v")?,
        },
        "nElem" => Acc::NElem {
            kind: match d.get_i32("k").map_err(|_| Fallback::Defer)? {
                0 => NElemKind::First,
                1 => NElemKind::Last,
                2 => NElemKind::Max,
                _ => NElemKind::Min,
            },
            n: n_of(d),
            vals: d.get_array("v").map_err(|_| Fallback::Defer)?.clone(),
        },
        "topN" => Acc::TopN {
            kind: match d.get_i32("k").map_err(|_| Fallback::Defer)? {
                0 => TopNKind::Top,
                1 => TopNKind::Bottom,
                2 => TopNKind::TopN,
                _ => TopNKind::BottomN,
            },
            n: n_of(d),
            dirs: d
                .get_array("d")
                .map_err(|_| Fallback::Defer)?
                .iter()
                .map(|b| b.as_bool().ok_or(Fallback::Defer))
                .collect::<R<Vec<bool>>>()?,
            items: d
                .get_array("v")
                .map_err(|_| Fallback::Defer)?
                .iter()
                .map(|pair| match pair {
                    Bson::Array(p) if p.len() == 2 => match (&p[0], &p[1]) {
                        (Bson::Array(keys), out) => Ok((keys.clone(), out.clone())),
                        _ => Err(Fallback::Defer),
                    },
                    _ => Err(Fallback::Defer),
                })
                .collect::<R<Vec<_>>>()?,
        },
        _ => return Err(Fallback::Defer),
    })
}

/// Fold `other`, a later partial state of the same accumulator, into
/// `into` (mongod's `process(..., merging = true)`).
fn merge_acc(into: &mut Acc, other: Acc) -> R<()> {
    match (into, other) {
        (Acc::Sum(a), Acc::Sum(b)) => {
            let taken = std::mem::replace(a, Num::zero());
            *a = merge_num(taken, b)?;
        }
        (Acc::Count(a), Acc::Count(b)) => *a += b,
        (Acc::Avg(a), Acc::Avg(b)) => {
            *a = match (a.take(), b) {
                (Some((n1, c1)), Some((n2, c2))) => Some((merge_num(n1, n2)?, c1 + c2)),
                (x, y) => x.or(y),
            }
        }
        (Acc::Min(a), Acc::Min(Some(b))) => update_extreme(a, b, std::cmp::Ordering::Less)?,
        (Acc::Max(a), Acc::Max(Some(b))) => update_extreme(a, b, std::cmp::Ordering::Greater)?,
        (Acc::Min(_), Acc::Min(None)) | (Acc::Max(_), Acc::Max(None)) => {}
        (Acc::First(a), Acc::First(b)) => {
            if a.is_none() {
                *a = b;
            }
        }
        (Acc::Last(a), Acc::Last(b)) => {
            if b.is_some() {
                *a = b;
            }
        }
        (Acc::Push(a), Acc::Push(b)) => {
            for v in b.items {
                a.push(v)?;
            }
        }
        (Acc::AddToSet(a), Acc::AddToSet(b)) => {
            for v in b.items {
                a.add(v)?;
            }
        }
        (Acc::ConcatArrays(a), Acc::ConcatArrays(b)) => a.concat(b.items)?,
        (Acc::SetUnion(a), Acc::SetUnion(b)) => a.union(b.items)?,
        (Acc::MergeObjects(a), Acc::MergeObjects(b)) => {
            for (k, v) in b {
                a.insert(k, v);
            }
        }
        (Acc::StdDev { values: a, .. }, Acc::StdDev { values: b, .. }) => a.extend(b),
        (
            Acc::Percentile { ps, values, .. },
            Acc::Percentile {
                ps: ps2,
                values: v2,
                ..
            },
        ) => {
            if ps.is_none() {
                *ps = ps2;
            }
            values.extend(v2);
        }
        (
            Acc::NElem { n, vals, .. },
            Acc::NElem {
                n: n2, vals: v2, ..
            },
        ) => {
            if n.is_none() {
                *n = n2;
            }
            vals.extend(v2);
        }
        (
            Acc::TopN { n, dirs, items, .. },
            Acc::TopN {
                n: n2,
                dirs: d2,
                items: i2,
                ..
            },
        ) => {
            if n.is_none() {
                *n = n2;
            }
            if dirs.is_empty() {
                *dirs = d2;
            }
            items.extend(i2);
        }
        _ => return Err(Fallback::Defer),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use bson::{bson, doc, Bson, Document};

    use crate::memlimit::{self, Budgets};

    fn docs(n: i32) -> Vec<Document> {
        (0..n)
            .map(|i| {
                doc! {
                    "_id": i,
                    "k": (i * 7) % 23,
                    "v": f64::from(i) / 3.0,
                    "n": i64::from(i),
                    "d": Bson::Decimal128(format!("{i}.5").parse().unwrap()),
                    "s": format!("{i:04}").repeat(25),
                    "o": { format!("f{}", i % 3): i },
                }
            })
            .collect()
    }

    fn group(
        budget: usize,
        allow: bool,
        sbe: bool,
    ) -> Result<Vec<Document>, crate::fallback::Fallback> {
        let spec = bson!({
            "_id": "$k",
            "sum": {"$sum": "$v"}, "sumInt": {"$sum": "$_id"}, "one": {"$sum": 1},
            "dec": {"$sum": "$d"}, "avg": {"$avg": "$n"}, "count": {"$count": {}},
            "min": {"$min": "$s"}, "max": {"$max": "$v"},
            "first": {"$first": "$_id"}, "last": {"$last": "$_id"},
            "push": {"$push": "$_id"}, "set": {"$addToSet": "$s"},
            "merged": {"$mergeObjects": "$o"}, "sd": {"$stdDevPop": "$v"},
            "firstN": {"$firstN": {"n": 2, "input": "$_id"}},
            "top": {"$topN": {"n": 2, "sortBy": {"v": -1}, "output": "$_id"}},
            "med": {"$median": {"input": "$v", "method": "approximate"}},
            "cat": {"$concatArrays": {"$ifNull": [["$_id"], []]}},
        });
        let budgets = Budgets {
            group: budget,
            sbe_group: budget,
            ..Default::default()
        };
        let _b = memlimit::with_budgets(budgets);
        let _d = memlimit::allow_disk_use(allow);
        let _e = crate::sbe::enter(sbe);
        crate::group::group_stage(&spec, &docs(400), &Document::new())
    }

    fn by_key(mut out: Vec<Document>) -> Vec<Document> {
        out.sort_by(|a, b| crate::order::cmp(a.get("_id").unwrap(), b.get("_id").unwrap()));
        out
    }

    #[test]
    fn a_spilled_group_gives_the_same_groups_in_key_order() {
        for sbe in [false, true] {
            memlimit::reset_report();
            let whole = group(memlimit::MAX_BYTES, true, sbe).unwrap();
            assert_eq!(memlimit::report(), memlimit::SpillReport::default());
            let spilled = group(20_000, true, sbe).unwrap();
            let report = memlimit::report();
            assert!(report.used_disk && report.group_spills > 1, "{report:?}");
            // Same groups, every accumulator merged to the same answer.
            assert_eq!(by_key(whole), spilled.clone(), "sbe: {sbe}");
            // In key order, as mongod returns a spilled group.
            let keys: Vec<i32> = spilled.iter().map(|d| d.get_i32("_id").unwrap()).collect();
            assert_eq!(keys, (0..23).collect::<Vec<_>>());
        }
    }

    #[test]
    fn a_group_over_budget_without_disk_is_292_in_its_engines_words() {
        let classic = group(20_000, false, false).unwrap_err();
        assert_eq!(
            classic.as_mongo(),
            Some((
                292,
                "Exceeded memory limit for $group, but didn't allow external sort. Pass \
                 allowDiskUse:true to opt in."
            ))
        );
        let sbe = group(20_000, false, true).unwrap_err();
        assert_eq!(
            sbe.as_mongo(),
            Some((
                292,
                "Exceeded memory limit for $group, but didn't allow external spilling; pass \
                 allowDiskUse:true to opt in"
            ))
        );
    }

    #[test]
    fn a_constant_key_is_never_spilled_in_sbe() {
        let spec = bson!({"_id": null, "p": {"$push": "$s"}});
        let budgets = Budgets {
            group: 1_000,
            sbe_group: 1_000,
            ..Default::default()
        };
        let _b = memlimit::with_budgets(budgets);
        let _d = memlimit::allow_disk_use(false);
        {
            let _e = crate::sbe::enter(true);
            let out = crate::group::group_stage(&spec, &docs(50), &Document::new()).unwrap();
            assert_eq!(out[0].get_array("p").unwrap().len(), 50);
        }
        // The classic engine counts it, and refuses.
        assert!(crate::group::group_stage(&spec, &docs(50), &Document::new()).is_err());
    }

    #[test]
    fn one_array_past_its_budget_is_146_in_its_engines_words() {
        let spec = bson!({"_id": null, "p": {"$push": "$s"}});
        let budgets = Budgets {
            push: 1_000,
            ..Default::default()
        };
        let _b = memlimit::with_budgets(budgets);
        {
            // SBE: each 100-byte string is 114; the ninth would reach 1000.
            let _e = crate::sbe::enter(true);
            let e = crate::group::group_stage(&spec, &docs(50), &Document::new()).unwrap_err();
            assert_eq!(
                e.as_mongo(),
                Some((
                    146,
                    "Used too much memory for a single array. Memory limit: 1000 bytes. The \
                     array contains 8 elements and is of size 912 bytes. The element being \
                     added has size 114 bytes."
                ))
            );
        }
        let e = crate::group::group_stage(&spec, &docs(50), &Document::new()).unwrap_err();
        assert_eq!(
            e.as_mongo(),
            Some((
                146,
                "$push used too much memory and cannot spill to disk. Memory limit: 1000 bytes"
            ))
        );
    }

    #[test]
    fn a_whole_partition_window_over_budget_without_disk_is_5643011() {
        let spec = bson!({"sortBy": {"_id": 1}, "output": {"t": {"$sum": "$v"}}});
        let run = |budget: usize, allow: bool, rest: &[Bson]| {
            let budgets = Budgets {
                window_fields: budget,
                ..Default::default()
            };
            let _b = memlimit::with_budgets(budgets);
            let _d = memlimit::allow_disk_use(allow);
            let mut pipeline = vec![Bson::Document(doc! {"$setWindowFields": spec.clone()})];
            pipeline.extend_from_slice(rest);
            crate::aggregate::apply_pipeline(docs(20), &pipeline, &Document::new(), None)
        };
        let e = run(1_000, false, &[]).unwrap_err();
        assert_eq!(e.as_mongo().map(|(c, _)| c), Some(5643011));
        // With disk, mongod spills the cache: it runs.
        assert_eq!(run(1_000, true, &[]).unwrap().len(), 20);
        // Only `_id` and `v` are cached when the stages after need `_id`
        // alone: 20 x (27 + 104) bytes fits in 3000.
        let out = run(
            3_000,
            false,
            &[Bson::Document(doc! {"$project": {"_id": 1}})],
        )
        .unwrap();
        assert_eq!(out.len(), 20);
        assert_eq!(out[0], doc! {"_id": 0});
    }
}
