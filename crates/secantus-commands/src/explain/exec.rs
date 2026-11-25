//! Running a plan to count what it examines.
//!
//! `executionStats` reports, per stage, how many index keys and documents the
//! plan looked at. The counts here come from walking the reported plan over
//! the collection's documents with mongod's stage rules: a `LIMIT` stops its
//! input early, a multikey scan drops repeated documents, and a scan over
//! several intervals examines the key that tells it an interval has ended.
//! The documents are read from a collection scan, so this is a count of what
//! the reported plan would touch and never a measurement of this server.

use super::bounds::{sort_key, Interval};
use super::node::{num, IndexInfo, IxScan, Node, Stats, WorkCounts};
use crate::storage::RecordId;
use bson::{Bson, Document};
use secantus_core::collation::Collation;
use secantus_core::geoorder::near::{Candidate, IntervalCounts, NearSearch, Source};
use secantus_core::geoorder::FieldBounds;
use secantus_core::heap::Heap;
use std::collections::{BTreeSet, HashMap, VecDeque};

/// What the stages read: the documents, in storage order.
pub(crate) struct Env<'a> {
    pub docs: &'a [Document],
    pub collation: Option<&'a Collation>,
    /// `count`'s `skip` and `limit`; `limit` 0 is none.
    pub skip: i64,
    pub limit: i64,
    /// The write being explained, for `UPDATE` and `DELETE`.
    pub multi: bool,
    pub upsert: bool,
    /// `findAndModify`: the write hands its document back.
    pub returns: bool,
    /// Per document, whether it matches the QUERY, when the caller already
    /// knows: a stage's filter is then answered from this and not run, so
    /// the documents need only hold the fields the indexes are keyed on.
    /// Every FETCH or COLLSCAN filter a plan runs passes exactly the
    /// documents the query matches, among those its scan returns; an index
    /// scan's key filter is only part of the query and is always run.
    pub known: Option<&'a [bool]>,
    /// The command's filter, which a positional projection below a sort
    /// reads; `None` where no sort over one reports what it sorted.
    pub query: Option<&'a Document>,
    /// The collection is clustered: a row's RecordId is the KeyString of its
    /// `_id`, whose length a sort that spills its members reports.
    pub clustered: bool,
}

impl Env<'_> {
    pub(crate) fn matches(&self, row: usize, filter: &Option<Document>) -> bool {
        if let (Some(known), Some(_)) = (self.known, filter) {
            return known[row];
        }
        self.evaluate(row, filter)
    }

    /// An index scan's filter over the keys: only part of the query (the
    /// rest is its FETCH's), so `known` does not answer it. Its fields are
    /// the index's, which the documents hold (RM-440).
    pub(crate) fn key_matches(&self, row: usize, filter: &Option<Document>) -> bool {
        self.evaluate(row, filter)
    }

    fn evaluate(&self, row: usize, filter: &Option<Document>) -> bool {
        match filter {
            None => true,
            Some(f) => secantus_core::query::matches(
                &self.docs[row],
                f,
                &secantus_core::vars::Vars::new(),
                self.collation,
            )
            .unwrap_or(false),
        }
    }
}

/// The key a hashed index holds for `doc` at `field`: the hash of the
/// value, null's for a missing field.
pub(crate) fn hashed_key(index: &IndexInfo, field: &str, doc: &Document) -> Bson {
    secantus_core::hashed::index_key(
        secantus_core::get_path(doc, field),
        index.collation.as_ref(),
    )
}

/// The values a path holds in a document, one per array element on the way,
/// and the path components that were arrays. No value: the field is missing.
pub(crate) fn values_at(doc: &Document, path: &str) -> (Vec<Bson>, Vec<String>) {
    fn walk(v: &Bson, parts: &[&str], at: usize, out: &mut Vec<Bson>, arrays: &mut Vec<String>) {
        let prefix = || parts[..at].join(".");
        if let Bson::Array(items) = v {
            if at > 0 && !arrays.contains(&prefix()) {
                arrays.push(prefix());
            }
            if at == parts.len() {
                if items.is_empty() {
                    out.push(Bson::Undefined);
                }
                out.extend(items.iter().cloned());
                return;
            }
            for item in items {
                // An element that does not have the rest of the path is
                // indexed as null: a document without the field, and an
                // element that is not a document.
                let before = out.len();
                if matches!(item, Bson::Document(_)) {
                    walk_doc(item, parts, at, out, arrays);
                }
                if out.len() == before {
                    out.push(Bson::Null);
                }
            }
            return;
        }
        walk_doc(v, parts, at, out, arrays);
    }
    fn walk_doc(
        v: &Bson,
        parts: &[&str],
        at: usize,
        out: &mut Vec<Bson>,
        arrays: &mut Vec<String>,
    ) {
        if at == parts.len() {
            out.push(v.clone());
            return;
        }
        if let Bson::Document(d) = v {
            if let Some(next) = d.get(parts[at]) {
                walk(next, parts, at + 1, out, arrays);
            }
        }
    }
    let parts: Vec<&str> = path.split('.').collect();
    let (mut out, mut arrays) = (Vec::new(), Vec::new());
    // The first component is read from the document as it stands: no copy.
    if let Some(next) = doc.get(parts[0]) {
        walk(next, &parts, 1, &mut out, &mut arrays);
    }
    (out, arrays)
}

/// Which components of each key field hold an array in some document.
pub(crate) fn multikey_paths(docs: &[Document], key: &Document) -> Vec<Vec<String>> {
    key.keys()
        .map(|field| {
            let mut found: Vec<String> = Vec::new();
            for doc in docs {
                for part in values_at(doc, field).1 {
                    if !found.contains(&part) {
                        found.push(part);
                    }
                }
            }
            found.sort();
            found
        })
        .collect()
}

/// One index entry: the key per field in SCAN order, and the document.
pub(crate) type Entry = (Vec<Vec<u8>>, usize);

/// One value's key in an index field, in SCAN order: under the index's
/// collation, and for a field read against its direction a key that sorts
/// the other way.
///
/// Inverting the bytes alone does not reverse two keys when one is the start
/// of the other (`"a"` and `"ab"`, `""` and anything), so a reversed key is
/// first made one no other key starts with: its zero bytes escaped and a
/// terminator added, as the entries table joins the parts of a compound key.
pub(crate) fn directed(v: &Bson, reversed: bool, collation: Option<&Collation>) -> Vec<u8> {
    // Under a collation every string takes it, one inside a document or an
    // array too, as the index's entries hold them.
    let bytes = match (v, collation) {
        (Bson::String(_) | Bson::Document(_) | Bson::Array(_), Some(_)) => {
            secantus_core::sortkey::encode_sort_value(v, collation).unwrap_or_default()
        }
        _ => sort_key(v),
    };
    if !reversed {
        return bytes;
    }
    reversed_key(&bytes)
}

/// A key whose byte order is the reverse of `bytes`': inverted, with each
/// zero escaped and a terminator, so no reversed key starts another.
pub(crate) fn reversed_key(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 2);
    for &b in bytes {
        out.push(!b);
        if b == 0 {
            out.push(0x00);
        }
    }
    out.extend([0xFF, 0xFF]);
    out
}

/// The index's entries, unordered.
pub(crate) fn entries(index: &IndexInfo, backward: bool, env: &Env) -> Vec<Entry> {
    let fields = index.fields();
    let collation = index.collation.as_ref();
    let partial = index.partial.clone();
    let mut out: Vec<Entry> = Vec::with_capacity(env.docs.len());
    let reversed = |dir: i64| (dir < 0) != backward;
    for (row, doc) in env.docs.iter().enumerate() {
        if partial.is_some() && !env.matches(row, &partial) {
            continue;
        }
        // A wildcard index's entries (an entry of it for one path reads them all): `$_path`, then the value
        // under it, one key for each leaf the document holds.
        if let Some(wild) = index.wildcard.as_ref().filter(|w| w.path.is_some()) {
            let mut keys: Vec<Vec<Vec<u8>>> =
                secantus_core::wildcard::leaves(doc, &wild.projection)
                    .keys
                    .into_iter()
                    .map(|(p, v)| {
                        vec![
                            directed(&Bson::String(p), false, None),
                            directed(&v, reversed(fields[1].1), collation),
                        ]
                    })
                    .collect();
            keys.sort();
            keys.dedup();
            out.extend(keys.into_iter().map(|k| (k, row)));
            continue;
        }
        // A geo index's entries: the cells (or geohashes) of its geo
        // field, and the other fields' keys.
        if let Some(geo) = &index.geo {
            use secantus_core::geoorder::KeyPart;
            let mut keys: Vec<Vec<Vec<u8>>> = geo
                .keys(doc)
                .into_iter()
                .map(|key| {
                    key.into_iter()
                        .zip(&fields)
                        .map(|(part, (_, dir))| match part {
                            KeyPart::Cell(c) => sort_key(&Bson::Int64(c)),
                            KeyPart::Hash(h) => sort_key(&Bson::Binary(bson::Binary {
                                subtype: bson::spec::BinarySubtype::UserDefined(128),
                                bytes: h.to_be_bytes().to_vec(),
                            })),
                            KeyPart::Value(bytes) if reversed(*dir) => reversed_key(&bytes),
                            KeyPart::Value(bytes) => bytes,
                        })
                        .collect()
                })
                .collect();
            keys.sort();
            keys.dedup();
            out.extend(keys.into_iter().map(|k| (k, row)));
            continue;
        }
        let per_field: Vec<Vec<Bson>> = fields
            .iter()
            .map(|(field, _)| values_at(doc, field).0)
            .collect();
        if index.sparse && per_field.iter().all(Vec::is_empty) {
            continue;
        }
        // A hashed field's key is the hash of its value.
        let per_field: Vec<Vec<Bson>> = per_field
            .into_iter()
            .enumerate()
            .map(|(pos, values)| {
                if index.is_hashed(pos) {
                    vec![hashed_key(index, fields[pos].0, doc)]
                } else {
                    values
                }
            })
            .collect();
        // One value a field: one key, built directly.
        if per_field.iter().all(|values| values.len() <= 1) {
            let key = per_field
                .iter()
                .zip(&fields)
                .map(|(values, (_, dir))| {
                    directed(
                        values.first().unwrap_or(&Bson::Null),
                        reversed(*dir),
                        collation,
                    )
                })
                .collect();
            out.push((key, row));
            continue;
        }
        let mut keys: Vec<Vec<Vec<u8>>> = vec![vec![]];
        for (values, (_, dir)) in per_field.iter().zip(&fields) {
            let mut encoded: Vec<Vec<u8>> = if values.is_empty() {
                vec![directed(&Bson::Null, reversed(*dir), collation)]
            } else {
                values
                    .iter()
                    .map(|v| directed(v, reversed(*dir), collation))
                    .collect()
            };
            encoded.sort();
            encoded.dedup();
            keys = keys
                .into_iter()
                .flat_map(|k| {
                    encoded.iter().map(move |e| {
                        let mut next = k.clone();
                        next.push(e.clone());
                        next
                    })
                })
                .collect();
        }
        out.extend(keys.into_iter().map(|k| (k, row)));
    }
    out
}

/// Entries in the order the scan meets them: by key, and entries with one
/// key in RecordId order, last first for a backward scan.
fn sort_entries(out: &mut [Entry], backward: bool) {
    if backward {
        out.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    } else {
        out.sort_unstable();
    }
}

/// The documents an index scan returns, in the order it returns them.
pub(crate) fn scan_rows(ix: &IxScan, env: &Env) -> Vec<usize> {
    let mut scan = Scan::new(ix, env);
    let mut out = Vec::new();
    while let Some(row) = scan.next(env) {
        out.push(row);
    }
    out
}

/// What a DISTINCT_SCAN over the leading `width` fields of `ix` returns, in
/// order: for each new value of those fields, the first entry's document and
/// the value its key holds in the last of them (null for a document without
/// the field, undefined for an empty array, an element of an array).
pub(crate) fn distinct_entries(ix: &IxScan, width: usize, env: &Env) -> Vec<(usize, Bson)> {
    let fields = ix.index.fields();
    let Some(&(field, dir)) = width.checked_sub(1).and_then(|at| fields.get(at)) else {
        return Vec::new();
    };
    let reversed = (dir < 0) != ix.backward;
    let collation = ix.index.collation.as_ref();
    let mut scan = Scan::new(ix, env);
    let mut prefixes = BTreeSet::new();
    let mut out = Vec::new();
    while let Some(row) = scan.next_distinct(width, &mut prefixes) {
        let key = &scan.entries[scan.pos - 1].0[width - 1];
        let value = values_at(&env.docs[row], field)
            .0
            .into_iter()
            .find(|v| directed(v, reversed, collation) == *key)
            .unwrap_or(Bson::Null);
        out.push((row, value));
    }
    out
}

/// One field's intervals in scan order: `(low, inclusive, high, inclusive)`.
pub(crate) type Ranges = Vec<(Vec<u8>, bool, Vec<u8>, bool)>;

pub(crate) fn ranges(list: &[Interval], reversed: bool, collation: Option<&Collation>) -> Ranges {
    let mut out: Ranges = list
        .iter()
        .map(|i| {
            if reversed {
                (
                    directed(&i.hi, true, collation),
                    i.hi_incl,
                    directed(&i.lo, true, collation),
                    i.lo_incl,
                )
            } else {
                (
                    directed(&i.lo, false, collation),
                    i.lo_incl,
                    directed(&i.hi, false, collation),
                    i.hi_incl,
                )
            }
        })
        .collect();
    out.sort();
    out
}

fn inside(key: &[u8], r: &(Vec<u8>, bool, Vec<u8>, bool)) -> bool {
    let above = key > r.0.as_slice() || (r.1 && key == r.0.as_slice());
    let below = key < r.2.as_slice() || (r.3 && key == r.2.as_slice());
    above && below
}

/// What one call into an index scan did.
pub(crate) enum Step {
    Entry(usize),
    Seek,
    End,
}

/// What one unit of work of a stage produced, as mongod's `PlanStage::work`
/// reports it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Work {
    Advanced(usize),
    NeedTime,
    Eof,
}

/// The order a SORT_MERGE hands back its children's heads in.
///
/// mongod keeps one head per child in a `std::priority_queue` whose
/// comparator is "sorts after", so the top is the least. Heads that compare
/// equal come out in the order the heap's sift steps leave them
/// (`secantus_core::heap`, libstdc++'s, as on the Linux build).
pub(crate) struct Head {
    key: Vec<Vec<u8>>,
    pub row: usize,
    pub child: usize,
}

impl secantus_core::heap::HeapOrder for Head {
    fn heap_less(&self, other: &Self) -> bool {
        self.key > other.key
    }
}

/// A document's key under a merge's sort pattern: each field as the index
/// holds it (a missing field is null), read in the pattern's direction.
pub(crate) fn merge_head(pattern: &Document, env: &Env, row: usize, child: usize) -> Head {
    let key = pattern
        .iter()
        .map(|(field, dir)| {
            let value = values_at(&env.docs[row], field)
                .0
                .into_iter()
                .next()
                .unwrap_or(Bson::Null);
            let descending = crate::util::as_i64(dir).is_some_and(|d| d < 0);
            directed(&value, descending, env.collation)
        })
        .collect();
    Head { key, row, child }
}

/// A scan in progress.
pub(crate) struct Scan {
    entries: Vec<Entry>,
    /// For a single run of keys, which keeps only its own entries: whether
    /// the index holds a key after it.
    after_run: Option<bool>,
    ranges: Vec<Ranges>,
    /// The bounds are one contiguous run of keys: nothing outside is examined.
    contiguous: bool,
    /// A DISTINCT_SCAN has counted the key past a contiguous run.
    ended_beyond: bool,
    multikey: bool,
    filter: Option<Document>,
    pos: usize,
    started: bool,
    done: bool,
    seen: BTreeSet<usize>,
    keys: i64,
    returned: i64,
    /// mongod's `seeks`: the first, and one per jump to a later interval,
    /// counted in the work that makes it (the one after the key that asks
    /// for it).
    seeks: i64,
    seek_pending: bool,
    /// A multikey scan's checks for a document it already returned.
    dups_tested: i64,
    dups_dropped: i64,
    /// Under a SORT of index keys: the entry each returned document was
    /// found at, the key its working-set member carries (RM-476).
    found: Option<HashMap<usize, usize>>,
}

impl Scan {
    pub(crate) fn new(ix: &IxScan, env: &Env) -> Scan {
        Scan::over(ix, &entries(&ix.index, ix.backward, env))
    }

    /// A scan of `ix` over the index's entries, `all` (unordered), which
    /// several scans of one index may share.
    pub(crate) fn over(ix: &IxScan, all: &[Entry]) -> Scan {
        let fields = ix.index.fields();
        let ranges: Vec<Ranges> = if ix.no_bounds {
            vec![]
        } else {
            ix.bounds
                .iter()
                .zip(&fields)
                .map(|(list, (_, dir))| {
                    ranges(list, (*dir < 0) != ix.backward, ix.index.collation.as_ref())
                })
                .collect()
        };
        let contiguous = contiguous(&ix.bounds);
        // A single run of keys examines none outside it, so only the run's
        // entries are kept (and sorted); whether a key follows the run is
        // all that is asked of the rest.
        let mut after_run = None;
        let mut all: Vec<Entry> = if contiguous && !ix.no_bounds {
            let mut after = false;
            let kept = all
                .iter()
                .filter(|(key, _)| match outside_of(&ranges, key) {
                    None => true,
                    Some((_, ahead)) => {
                        after |= !ahead;
                        false
                    }
                })
                .cloned()
                .collect();
            after_run = Some(after);
            kept
        } else {
            all.to_vec()
        };
        sort_entries(&mut all, ix.backward);
        Scan {
            entries: all,
            after_run,
            ranges,
            contiguous,
            ended_beyond: false,
            multikey: ix.index.is_multikey(),
            filter: ix.filter.clone(),
            pos: 0,
            started: false,
            done: false,
            seen: BTreeSet::new(),
            keys: 0,
            returned: 0,
            seeks: 0,
            seek_pending: false,
            dups_tested: 0,
            dups_dropped: 0,
            found: None,
        }
    }

    /// Keep the key each returned document was found at (see
    /// [`Scan::found_at`]).
    pub(crate) fn keep_found(&mut self) {
        self.found = Some(HashMap::new());
    }

    /// The key the scan found `row` at when it returned it, once
    /// [`Scan::keep_found`] asked for it.
    pub(crate) fn found_at(&self, row: usize) -> Option<&[Vec<u8>]> {
        let at = *self.found.as_ref()?.get(&row)?;
        Some(&self.entries[at].0)
    }

    /// `row` was returned, found at the entry before `pos`.
    fn returning(&mut self, row: usize) {
        self.returned += 1;
        if let Some(found) = self.found.as_mut() {
            found.insert(row, self.pos - 1);
        }
    }

    /// The first field whose key is outside its bounds, and whether a later
    /// interval of that field could still be reached.
    fn outside(&self, key: &[Vec<u8>]) -> Option<(usize, bool)> {
        outside_of(&self.ranges, key)
    }

    /// Whether the index holds a key past the end of the bounds.
    pub(crate) fn key_beyond(&self) -> bool {
        if let Some(after) = self.after_run {
            return after;
        }
        self.entries[self.pos.min(self.entries.len())..]
            .iter()
            .any(|(key, _)| self.outside(key).is_some_and(|(_, ahead)| !ahead))
    }
}

/// Whether `bounds` are one contiguous run of keys -- points, then one
/// interval, then everything -- so that a scan examines no key outside it.
pub(crate) fn contiguous(bounds: &[Vec<Interval>]) -> bool {
    let mut open = false;
    for list in bounds {
        let [one] = list.as_slice() else {
            return false;
        };
        if open && !one.is_all() {
            return false;
        }
        if !one.is_point() {
            open = true;
        }
    }
    true
}

/// The first field whose key is outside `ranges`, and whether a later
/// interval of that field could still be reached.
pub(crate) fn outside_of(ranges: &[Ranges], key: &[Vec<u8>]) -> Option<(usize, bool)> {
    for (f, (part, list)) in key.iter().zip(ranges).enumerate() {
        if list.iter().any(|r| inside(part, r)) {
            continue;
        }
        let ahead = list
            .iter()
            .any(|r| part.as_slice() < r.0.as_slice() || (!r.1 && part == &r.0));
        return Some((f, ahead));
    }
    None
}

impl Scan {
    /// The first seek goes straight to the low end of the first intervals:
    /// the keys before it are never examined.
    fn seek_start(&mut self) {
        // `IndexBoundsChecker::getStartSeekPoint`: each field's first
        // interval's start. Where that start is exclusive, a key equal to it
        // there is still before the seek point.
        let start: Vec<(Vec<u8>, bool)> = self
            .ranges
            .iter()
            .map(|list| list.first().map_or((vec![], true), |r| (r.0.clone(), r.1)))
            .collect();
        self.skip_before(&start);
    }

    /// Move past every entry before the seek point `start` (each field's
    /// value, and whether a key equal to it there is at the point or
    /// before it).
    fn skip_before(&mut self, start: &[(Vec<u8>, bool)]) {
        while self.pos < self.entries.len() {
            let key = &self.entries[self.pos].0;
            let before = key
                .iter()
                .zip(start)
                .find_map(|(k, (s, inclusive))| {
                    if k != s {
                        Some(k < s)
                    } else if !inclusive {
                        Some(true)
                    } else {
                        None
                    }
                })
                .unwrap_or(false);
            if !before {
                break;
            }
            self.pos += 1;
        }
    }

    /// Whether every field before `field` holds its last interval, and that
    /// interval is a point: no later key can fall inside the bounds again.
    fn on_last_points(&self, key: &[Vec<u8>], field: usize) -> bool {
        key[..field].iter().zip(&self.ranges).all(|(part, list)| {
            list.last()
                .is_some_and(|r| r.0 == r.2 && r.1 && r.3 && inside(part, r))
        })
    }

    /// One unit of mongod's work: an entry inside the bounds, a key that
    /// sends the scan seeking, or the end. The key that ends a scan of
    /// several intervals is examined in the work that ends it; a single run
    /// of keys ends without examining one, as does the end of the index.
    fn work_entry(&mut self) -> Step {
        if !self.started {
            self.started = true;
            self.seeks += 1;
            self.seek_start();
        }
        if std::mem::take(&mut self.seek_pending) {
            self.seeks += 1;
        }
        while !self.done && self.pos < self.entries.len() {
            let Some((field, ahead)) = self.outside(&self.entries[self.pos].0) else {
                self.keys += 1;
                self.pos += 1;
                return Step::Entry(self.entries[self.pos - 1].1);
            };
            let key = self.entries[self.pos].0.clone();
            if self.contiguous {
                // Before the run: not examined. After it: the scan is over.
                if ahead {
                    self.pos += 1;
                    continue;
                }
                self.done = true;
                return Step::End;
            }
            self.keys += 1;
            // `IndexBoundsChecker::checkKey`: past the last interval of a
            // field, the scan is over when no earlier field can move on to
            // another key -- the first field, or earlier fields each on the
            // point that is their last interval.
            if !ahead && self.on_last_points(&key, field) {
                self.done = true;
                return Step::End;
            }
            self.pos += 1;
            if ahead {
                // Seek to the key's prefix, the field's next interval's
                // start, and the first interval's start of each later field.
                let part = &key[field];
                let next = self.ranges[field]
                    .iter()
                    .find(|r| part.as_slice() < r.0.as_slice() || (!r.1 && part == &r.0))
                    .map(|r| (r.0.clone(), r.1));
                let mut start: Vec<(Vec<u8>, bool)> =
                    key[..field].iter().map(|k| (k.clone(), true)).collect();
                start.extend(next);
                start.extend(
                    self.ranges[field + 1..]
                        .iter()
                        .map(|list| list.first().map_or((vec![], true), |r| (r.0.clone(), r.1))),
                );
                self.skip_before(&start);
            } else {
                // Seek past every key with the same prefix.
                let prefix = key[..field].to_vec();
                while self.pos < self.entries.len()
                    && self.entries[self.pos].0[..field] == prefix[..]
                {
                    self.pos += 1;
                }
            }
            self.seek_pending = true;
            return Step::Seek;
        }
        self.done = true;
        Step::End
    }

    /// The next entry inside the bounds, in scan order.
    fn next_entry(&mut self) -> Option<usize> {
        loop {
            match self.work_entry() {
                Step::Entry(row) => return Some(row),
                Step::Seek => continue,
                Step::End => return None,
            }
        }
    }

    /// One work of the scan as a stage: a document it returns, one it does
    /// not (a repeat, a key its filter rejects, a seek), or the end.
    pub(crate) fn work(&mut self, env: &Env) -> Work {
        match self.work_entry() {
            Step::End => Work::Eof,
            Step::Seek => Work::NeedTime,
            Step::Entry(row) => {
                if self.multikey {
                    self.dups_tested += 1;
                }
                let repeat = self.multikey && !self.seen.insert(row);
                if repeat {
                    self.dups_dropped += 1;
                }
                if repeat || !env.key_matches(row, &self.filter) {
                    Work::NeedTime
                } else {
                    self.returning(row);
                    Work::Advanced(row)
                }
            }
        }
    }

    fn next(&mut self, env: &Env) -> Option<usize> {
        loop {
            let row = self.next_entry()?;
            if self.multikey && !self.seen.insert(row) {
                continue;
            }
            if !env.key_matches(row, &self.filter) {
                continue;
            }
            self.returning(row);
            return Some(row);
        }
    }

    /// The keys the scan has examined.
    pub(crate) fn keys(&self) -> i64 {
        self.keys
    }

    /// `seeks`, `dupsTested` and `dupsDropped`.
    pub(crate) fn seek_counts(&self) -> (i64, i64, i64) {
        (self.seeks, self.dups_tested, self.dups_dropped)
    }

    /// One work of a DISTINCT_SCAN (mongod's `DistinctScan::doWork`): a key
    /// it returns, after which it seeks past every key holding the same
    /// leading `width` values; a key the bounds reject, which it examines
    /// and seeks from (a key on the trailing fields' bounds' wrong side
    /// included, RM-447); or the end, where the key past the bounds is
    /// examined too.
    pub(crate) fn work_distinct(
        &mut self,
        width: usize,
        prefixes: &mut BTreeSet<Vec<Vec<u8>>>,
    ) -> Step {
        match self.work_entry() {
            Step::Entry(row) => {
                let key = &self.entries[self.pos - 1].0;
                let prefix = key[..width.min(key.len())].to_vec();
                while self.pos < self.entries.len() && self.entries[self.pos].0.starts_with(&prefix)
                {
                    self.pos += 1;
                }
                self.seek_pending = true;
                if prefixes.insert(prefix) {
                    Step::Entry(row)
                } else {
                    Step::Seek
                }
            }
            Step::End => {
                if self.contiguous && !std::mem::replace(&mut self.ended_beyond, true) {
                    self.keys += i64::from(self.key_beyond());
                }
                Step::End
            }
            Step::Seek => Step::Seek,
        }
    }

    /// A DISTINCT_SCAN's next document: the first entry of a new value of
    /// the leading `width` fields (the scan seeks past the rest).
    pub(crate) fn next_distinct(
        &mut self,
        width: usize,
        prefixes: &mut BTreeSet<Vec<Vec<u8>>>,
    ) -> Option<usize> {
        loop {
            match self.work_distinct(width, prefixes) {
                Step::Entry(row) => return Some(row),
                Step::Seek => continue,
                Step::End => return None,
            }
        }
    }
}

/// A stage being run: its own state and its inputs.
struct Run<'a> {
    node: &'a Node,
    inputs: Vec<Run<'a>>,
    scan: Option<Scan>,
    pos: usize,
    done: bool,
    seen: BTreeSet<usize>,
    prefixes: BTreeSet<Vec<Vec<u8>>>,
    buffer: Option<Vec<usize>>,
    /// A SORT_MERGE: the children without a head, and the heads.
    queue: Option<VecDeque<usize>>,
    heads: Heap<Head>,
    returned: i64,
    keys: i64,
    docs: i64,
    extra: Document,
    /// A GEO_NEAR stage's ring search, over the documents it reads.
    near: Option<Box<(NearSearch<()>, Docs)>>,
}

/// The collection as a `$near` search reads it, unchanging.
struct Docs(Vec<Candidate<()>>);

impl Source<()> for Docs {
    type Error = std::convert::Infallible;
    fn candidates(&mut self) -> Result<&[Candidate<()>], Self::Error> {
        Ok(&self.0)
    }
}

/// The ring search mongod's GEO_NEAR stage runs for `clause` over `index`,
/// or `None` when the search is not one this replays.
fn near_search(index: &IndexInfo, clause: &Document, env: &Env) -> Option<(NearSearch<()>, Docs)> {
    let geo = index.geo.as_ref()?;
    let found = crate::near::find_near(clause).ok()??;
    // Multikey as the documents make it (the catalog's record is not here).
    let key: Document = geo
        .fields
        .iter()
        .map(|f| (f.clone(), Bson::Int32(1)))
        .collect();
    let multikey: Vec<bool> = multikey_paths(env.docs, &key)
        .iter()
        .map(|p| !p.is_empty())
        .collect();
    let base = super::geo::trailing_bounds(geo, env.query.unwrap_or(clause), &multikey)?;
    let measure = secantus_core::geoorder::near::Measure::new(geo, &found.near)?;
    let docs = env
        .docs
        .iter()
        .enumerate()
        .map(|(i, d)| measure.candidate(i as i64, d, true, ()))
        .collect();
    Some((NearSearch::new(measure, base), Docs(docs)))
}

impl<'a> Run<'a> {
    fn new(node: &'a Node, env: &Env) -> Run<'a> {
        let inputs: Vec<&Node> = match node {
            Node::Fetch { input, .. }
            | Node::Sort { input, .. }
            | Node::Projection { input, .. }
            | Node::Delete { input, .. } => vec![input],
            Node::Skip(_, input) | Node::Limit(_, input) => vec![input],
            Node::ReturnKey(input)
            | Node::SortKeyGenerator(input)
            | Node::Subplan(input)
            | Node::Count(input)
            | Node::Update(input) => vec![input],
            Node::Or(inputs) | Node::AndSorted(inputs) | Node::SortMerge { inputs, .. } => {
                inputs.iter().collect()
            }
            _ => vec![],
        };
        let scan = match node {
            Node::IxScan(ix) | Node::CountScan(ix) | Node::DistinctScan(ix, _, _) => {
                Some(Scan::new(ix, env))
            }
            _ => None,
        };
        let mut extra = Document::new();
        if let Node::AndSorted(inputs) = node {
            for i in 0..inputs.len() {
                extra.insert(format!("failedAnd_{i}"), 0_i32);
            }
        }
        let near = match node {
            Node::GeoNear { index, clause, .. } => near_search(index, clause, env).map(Box::new),
            _ => None,
        };
        let mut inputs: Vec<Run> = inputs.into_iter().map(|n| Run::new(n, env)).collect();
        if let Node::Sort { on_keys: true, .. } = node {
            inputs[0].keep_found();
        }
        Run {
            node,
            inputs,
            scan,
            pos: 0,
            done: false,
            seen: BTreeSet::new(),
            prefixes: BTreeSet::new(),
            buffer: None,
            queue: None,
            heads: Heap::new(),
            returned: 0,
            keys: 0,
            docs: 0,
            extra,
            near,
        }
    }

    /// Below a SORT of index keys: each scan keeps the key it found each
    /// document at.
    fn keep_found(&mut self) {
        match self.node {
            Node::IxScan(_) => {
                if let Some(scan) = self.scan.as_mut() {
                    scan.keep_found();
                }
            }
            Node::Projection { .. } | Node::Or(_) => {
                self.inputs.iter_mut().for_each(Run::keep_found);
            }
            _ => {}
        }
    }

    /// The scan that returned `row` and the key it found it at: an `$or`'s
    /// first child to return it, the one whose copy it kept.
    fn found(&self, row: usize) -> Option<(&IxScan, &[Vec<u8>])> {
        match self.node {
            Node::IxScan(ix) => Some((ix, self.scan.as_ref()?.found_at(row)?)),
            Node::Projection { .. } | Node::Or(_) => self.inputs.iter().find_map(|i| i.found(row)),
            _ => None,
        }
    }

    fn pull(&mut self, env: &Env) -> Option<usize> {
        if self.done {
            return None;
        }
        let row = self.step(env);
        match row {
            Some(_) => self.returned += 1,
            None => self.done = true,
        }
        row
    }

    fn step(&mut self, env: &Env) -> Option<usize> {
        match self.node {
            Node::CollScan { filter, backward } => {
                while self.pos < env.docs.len() {
                    let row = if *backward {
                        env.docs.len() - 1 - self.pos
                    } else {
                        self.pos
                    };
                    self.pos += 1;
                    self.docs += 1;
                    if env.matches(row, filter) {
                        return Some(row);
                    }
                }
                None
            }
            // mongod's ring search, ring by ring (RM-707).
            Node::GeoNear { .. } if self.near.is_some() => {
                let near = self.near.as_mut()?;
                let (search, docs) = &mut **near;
                let row = search.next(docs).unwrap_or_else(|e| match e {});
                self.keys = search.keys_examined;
                self.docs = search.docs_examined;
                row.map(|(rid, ())| rid as usize)
            }
            // A search this does not replay: the documents in storage order.
            Node::GeoNear { clause, .. } => {
                let clause = Some(clause.clone());
                while self.pos < env.docs.len() {
                    let row = self.pos;
                    self.pos += 1;
                    self.docs += 1;
                    if env.key_matches(row, &clause) {
                        return Some(row);
                    }
                }
                None
            }
            Node::IxScan(_) => {
                let scan = self.scan.as_mut()?;
                let row = scan.next(env);
                self.keys = scan.keys;
                row
            }
            Node::CountScan(_) => {
                let scan = self.scan.as_mut()?;
                let row = scan.next_entry();
                self.keys = scan.keys;
                // mongod's COUNT_SCAN counts the call that finds its end as
                // a key examined, a key past the bounds there or not
                // (`CountScan::doWork` counts before it looks; measured
                // 8.2.11: `{x: {$gt: 3}}` over keys 0 to 11 examines 9, and
                // `{x: 99}` over them 1). RM-356.
                if row.is_none() {
                    self.keys += 1;
                }
                row
            }
            Node::DistinctScan(_, width, _) => {
                // One entry per distinct value of the leading `width` fields.
                let scan = self.scan.as_mut()?;
                let row = scan.next_distinct(*width, &mut self.prefixes);
                self.keys = scan.keys;
                row
            }
            Node::Fetch { filter, .. } => loop {
                let row = self.inputs[0].pull(env)?;
                self.docs += 1;
                if env.matches(row, filter) {
                    return Some(row);
                }
            },
            Node::SortMerge { pattern, .. } => {
                // Every child with no head is worked to its next document
                // (a repeat is dropped), in turn; then the least head goes.
                let mut queue = self
                    .queue
                    .take()
                    .unwrap_or_else(|| (0..self.inputs.len()).collect());
                while let Some(&child) = queue.front() {
                    match self.inputs[child].pull(env) {
                        Some(row) => {
                            if self.seen.insert(row) {
                                queue.pop_front();
                                self.heads.push(merge_head(pattern, env, row, child));
                            }
                        }
                        None => {
                            queue.pop_front();
                        }
                    }
                }
                let top = self.heads.pop();
                if let Some(top) = &top {
                    queue.push_back(top.child);
                }
                self.queue = Some(queue);
                top.map(|t| t.row)
            }
            // `AndSortedStage`: the first child names a target RecordId, and
            // each other child is worked until it reaches it; one that passes
            // it names a new target, a failure of the old target's child.
            Node::AndSorted(_) => {
                let (mut node, mut target) = (0usize, self.inputs[0].pull(env)?);
                let mut queue: VecDeque<usize> = (1..self.inputs.len()).collect();
                while let Some(&child) = queue.front() {
                    let row = self.inputs[child].pull(env)?;
                    if row == target {
                        queue.pop_front();
                    } else if row > target {
                        self.failed_and(node);
                        (node, target) = (child, row);
                        queue = (0..self.inputs.len()).filter(|&c| c != child).collect();
                    }
                }
                Some(target)
            }
            Node::Or(_) => {
                while self.pos < self.inputs.len() {
                    match self.inputs[self.pos].pull(env) {
                        Some(row) => {
                            if self.seen.insert(row) {
                                return Some(row);
                            }
                        }
                        None => self.pos += 1,
                    }
                }
                None
            }
            Node::Sort {
                limit,
                pattern,
                input,
                keeps_metadata,
                ..
            } => {
                if self.buffer.is_none() {
                    let mut all = Vec::new();
                    while let Some(row) = self.inputs[0].pull(env) {
                        all.push(row);
                    }
                    let keys = key_members(pattern, &all, env, |r| self.inputs[0].found(r));
                    sort_stats(
                        &mut self.extra,
                        pattern,
                        *limit,
                        input,
                        &all,
                        keys,
                        *keeps_metadata,
                        env,
                    );
                    if let Some(n) = limit {
                        all.truncate(*n as usize);
                    }
                    self.buffer = Some(all);
                }
                let row = self.buffer.as_ref()?.get(self.pos).copied();
                self.pos += 1;
                row
            }
            Node::Skip(n, _) => {
                while (self.pos as i64) < *n {
                    self.pos += 1;
                    self.inputs[0].pull(env)?;
                }
                self.inputs[0].pull(env)
            }
            Node::Limit(n, _) => {
                if self.returned >= *n {
                    return None;
                }
                self.inputs[0].pull(env)
            }
            Node::Projection { .. }
            | Node::ReturnKey(_)
            | Node::SortKeyGenerator(_)
            | Node::Subplan(_) => self.inputs[0].pull(env),
            Node::Eof(_) | Node::FastCount => None,
            Node::Express { index, value } => {
                if self.pos > 0 {
                    return None;
                }
                self.pos = 1;
                let field = index.key.keys().next()?;
                let want = sort_key(value);
                let row = env
                    .docs
                    .iter()
                    .position(|d| values_at(d, field).0.iter().any(|v| sort_key(v) == want))?;
                self.keys = 1;
                self.docs = 1;
                Some(row)
            }
            Node::IdHack(value) => {
                if self.pos > 0 {
                    return None;
                }
                self.pos = 1;
                let want = sort_key(value);
                let row = env
                    .docs
                    .iter()
                    .position(|d| d.get("_id").is_some_and(|v| sort_key(v) == want))?;
                self.keys = 1;
                self.docs = 1;
                Some(row)
            }
            Node::Count(input) => {
                if self.pos > 0 {
                    return None;
                }
                self.pos = 1;
                let (mut counted, mut skipped) = (0i64, 0i64);
                if matches!(**input, Node::FastCount) {
                    (counted, skipped) = fast_count(env);
                } else {
                    while env.limit == 0 || counted < env.limit {
                        if self.inputs[0].pull(env).is_none() {
                            break;
                        }
                        if skipped < env.skip {
                            skipped += 1;
                        } else {
                            counted += 1;
                        }
                    }
                }
                self.extra.insert("nCounted", num(counted));
                self.extra.insert("nSkipped", num(skipped));
                None
            }
            Node::Update(_) => {
                if self.pos > 0 {
                    return None;
                }
                self.pos = 1;
                let (mut matched, mut modified) = (0i64, 0i64);
                let mut first = None;
                while let Some(row) = self.inputs[0].pull(env) {
                    matched += 1;
                    first.get_or_insert(row);
                    // mongod's explain does not apply the update: every
                    // matched document counts as one it would modify.
                    modified += 1;
                    if !env.multi {
                        break;
                    }
                }
                self.extra.insert("nMatched", num(matched));
                self.extra.insert("nWouldModify", num(modified));
                self.extra
                    .insert("nWouldUpsert", num(i64::from(env.upsert && matched == 0)));
                first.filter(|_| env.returns)
            }
            Node::Delete { .. } => {
                if self.pos > 0 {
                    return None;
                }
                self.pos = 1;
                let mut deleted = 0i64;
                let mut first = None;
                while let Some(row) = self.inputs[0].pull(env) {
                    deleted += 1;
                    first.get_or_insert(row);
                    if !env.multi {
                        break;
                    }
                }
                self.extra.insert("nWouldDelete", num(deleted));
                first.filter(|_| env.returns)
            }
            Node::ExpressWrite { value, delete, .. } => {
                if self.pos > 0 {
                    return None;
                }
                self.pos = 1;
                let want = sort_key(value);
                let row = env
                    .docs
                    .iter()
                    .position(|d| d.get("_id").is_some_and(|v| sort_key(v) == want));
                let found = i64::from(row.is_some());
                self.keys = found;
                self.docs = found;
                let modify = row.is_some() && !*delete;
                self.extra.insert("nWouldModify", num(i64::from(modify)));
                self.extra.insert("nWouldUpsert", 0_i32);
                self.extra
                    .insert("nWouldDelete", num(if *delete { found } else { 0 }));
                row.filter(|_| env.returns)
            }
        }
    }

    /// One more document the target's child had that another passed over.
    fn failed_and(&mut self, child: usize) {
        let key = format!("failedAnd_{child}");
        let n = self
            .extra
            .get(&key)
            .and_then(crate::util::as_i64)
            .unwrap_or(0);
        self.extra.insert(key, num(n + 1));
    }

    fn stats(&self) -> Stats {
        let mut extra = self.extra.clone();
        fixed_extra(self.node, &mut extra);
        let docs = fetched(self.node, self.returned, &mut extra).unwrap_or(self.docs);
        let mut work = None;
        if let (Node::GeoNear { index, sphere, .. }, Some(near)) = (self.node, &self.near) {
            let search = &near.0;
            let c = &search.counts;
            work = Some(WorkCounts {
                works: c.works,
                advanced: c.advanced,
                need_time: c.need_time,
                eof: c.eof,
                ..WorkCounts::default()
            });
            near_extra(index, *sphere, search, &mut extra);
        }
        let children: Vec<Stats> = self.inputs.iter().map(Run::stats).collect();
        // mongod's LimitStage passes each work of its input through and
        // answers one more, its EOF, once it has returned `n` (RM-714).
        if let (Node::Limit(n, _), Some(Stats { work: Some(cw), .. })) =
            (self.node, children.first())
        {
            let stopped = self.returned >= *n && !cw.eof;
            work = Some(WorkCounts {
                works: cw.works + i64::from(stopped),
                advanced: self.returned,
                need_time: cw.need_time,
                eof: stopped || cw.eof,
                saved: cw.saved,
                restored: cw.restored,
                ..WorkCounts::default()
            });
        }
        Stats {
            n_returned: self.returned,
            keys: self.keys,
            docs,
            extra,
            children,
            work,
        }
    }
}

/// A GEO_NEAR stage's `searchIntervals` (the rings it finished) and
/// `inputStages` (each ring's FETCH over its IXSCAN), as mongod 8.2.11
/// reports them.
fn near_extra(index: &IndexInfo, sphere: bool, search: &NearSearch<()>, extra: &mut Document) {
    let rings = &search.counts.intervals;
    // A ring is listed from when it is buffered, the one being handed out
    // with what it has returned so far.
    let intervals: Vec<Bson> = rings
        .iter()
        .map(|i| {
            Bson::Document(bson::doc! {
                "minDistance": i.min,
                "maxDistance": i.max,
                "maxInclusive": i.inclusive_max,
                "nBuffered": num(i.buffered),
                "nReturned": num(i.returned),
            })
        })
        .collect();
    let stages: Vec<Bson> = rings
        .iter()
        .map(|i| Bson::Document(ring_stages(index, sphere, search.counts.multikey, i)))
        .collect();
    extra.insert("searchIntervals", intervals);
    extra.insert("inputStages", stages);
}

fn common(out: &mut Document, n: i64, works: i64, advanced: i64, need_time: i64) {
    out.insert("nReturned", num(n));
    out.insert("executionTimeMillisEstimate", 0_i32);
    out.insert("works", num(works));
    out.insert("advanced", num(advanced));
    out.insert("needTime", num(need_time));
    out.insert("needYield", 0_i32);
    out.insert("saveState", 0_i32);
    out.insert("restoreState", 0_i32);
    out.insert("isEOF", 1_i32);
}

/// One ring's FETCH over its IXSCAN.
fn ring_stages(index: &IndexInfo, sphere: bool, multikey: bool, ring: &IntervalCounts) -> Document {
    let s = &ring.scan;
    let mut scan = bson::doc! { "stage": "IXSCAN" };
    common(&mut scan, s.advanced, s.works, s.advanced, s.need_time);
    scan.insert("keyPattern", index.key.clone());
    scan.insert("indexName", index.name.clone());
    // A `2d` index is multikey when a document has several entries, not
    // for the array a point is (`[x, y]`).
    scan.insert(
        "isMultiKey",
        if sphere {
            index.is_multikey()
        } else {
            multikey
        },
    );
    if sphere {
        let mut paths = Document::new();
        for (at, field) in index.key.keys().enumerate() {
            let list: Vec<Bson> = index
                .multikey_paths
                .get(at)
                .map(|p| p.iter().cloned().map(Bson::String).collect())
                .unwrap_or_default();
            paths.insert(field.clone(), list);
        }
        scan.insert("multiKeyPaths", paths);
    }
    scan.insert("isUnique", index.unique);
    scan.insert("isSparse", index.sparse);
    scan.insert("isPartial", index.partial.is_some());
    scan.insert("indexVersion", index.version);
    scan.insert("direction", "forward");
    let geo_at = index.geo.as_ref().map_or(0, |g| g.geo_at);
    let mut bounds = Document::new();
    for (at, field) in index.key.keys().enumerate() {
        let list: Vec<Bson> = if at == geo_at {
            geo_bounds(&ring.bounds)
        } else {
            vec![Bson::String("[MinKey, MaxKey]".into())]
        };
        bounds.insert(field.clone(), list);
    }
    scan.insert("indexBounds", bounds);
    scan.insert("keysExamined", num(s.keys_examined));
    scan.insert("seeks", num(s.seeks));
    scan.insert("dupsTested", 0_i32);
    scan.insert("dupsDropped", 0_i32);
    let mut fetch = bson::doc! { "stage": "FETCH" };
    if !sphere {
        fetch.insert("filter", bson::doc! { "$TwoDPtInAnnulusExpression": true });
    }
    // The FETCH works as often as its scan; the last work finds the end.
    let fetch_need = s.works - ring.fetch_advanced - 1;
    common(
        &mut fetch,
        ring.fetch_advanced,
        s.works,
        ring.fetch_advanced,
        fetch_need,
    );
    fetch.insert("docsExamined", num(ring.docs));
    fetch.insert("alreadyHasObj", 0_i32);
    fetch.insert("inputStage", scan);
    fetch
}

/// A geo field's intervals as mongod prints them: a `2d` geohash as
/// `BinData(128, ...)`, a `2dsphere` cell as its integer.
fn geo_bounds(bounds: &FieldBounds) -> Vec<Bson> {
    match bounds {
        FieldBounds::Hashes(list) => list
            .iter()
            .map(|(lo, hi)| {
                Bson::String(format!(
                    "[BinData(128, {lo:016X}), BinData(128, {hi:016X})]"
                ))
            })
            .collect(),
        FieldBounds::Cells(list) => list
            .iter()
            .map(|(lo, hi)| Bson::String(format!("[{lo}, {hi}]")))
            .collect(),
        _ => vec![Bson::String("[MinKey, MaxKey]".into())],
    }
}

/// A fetching DISTINCT_SCAN reads the document of each key it returns:
/// that count, also put in its `docsExamined`. `None`: not such a stage.
pub(crate) fn fetched(node: &Node, returned: i64, extra: &mut Document) -> Option<i64> {
    match node {
        Node::DistinctScan(_, _, true) => {
            extra.insert("docsExamined", num(returned));
            Some(returned)
        }
        _ => None,
    }
}

/// What a SORT of index keys (`type: "default"`, below the FETCH) holds
/// of each document it sorts, as mongod's working-set member does: its
/// sort key's values, under `pattern`, and the values of the index key it
/// was found at. `found`: the scan below that returned a row, and that
/// key. `None`: some row has no such key, a sort of documents.
/// Whether `db.coll` is clustered: its rows' RecordIds are `_id` KeyStrings.
pub(crate) fn is_clustered(storage: &dyn crate::storage::Storage, db: &str, coll: &str) -> bool {
    storage
        .get_collection_options(db, coll)
        .is_ok_and(|o| o.contains_key("clusteredIndex"))
}

/// The RecordId row `row` of `env` stands for: its place, or for a clustered
/// collection the KeyString of its `_id` as mongod's `keyForObj` makes it.
fn record_id_of(env: &Env, row: usize) -> RecordId {
    use secantus_core::keystring::{Builder, Ordering, Version};
    if env.clustered {
        let id = env.docs[row].get("_id").cloned().unwrap_or(Bson::Null);
        let mut b = Builder::new(Version::V1, Ordering::all_ascending());
        let appended = match env.collation {
            Some(c) => b.append_bson_element_with(
                &id,
                Some(&|s: &str| secantus_core::collation::sort_key(s, c)),
            ),
            None => b.append_bson_element(&id),
        };
        if appended.is_ok() {
            return RecordId::Bytes(b.buffer().to_vec());
        }
    }
    RecordId::Long(row as i64 + 1)
}

pub(crate) fn key_members<'s>(
    pattern: &Document,
    rows: &[usize],
    env: &Env,
    found: impl Fn(usize) -> Option<(&'s IxScan, &'s [Vec<u8>])>,
) -> Option<Vec<KeyMember>> {
    rows.iter()
        .map(|&row| {
            let (ix, key) = found(row)?;
            let fields = ix.index.fields();
            let collation = ix.index.collation.as_ref();
            // The value each key part was built from: of an array's
            // elements, the one the entry holds.
            let values: Vec<Bson> = fields
                .iter()
                .zip(key)
                .enumerate()
                .map(|(pos, ((field, dir), part))| {
                    let (values, _) = if ix.index.is_hashed(pos) {
                        (vec![hashed_key(&ix.index, field, &env.docs[row])], vec![])
                    } else {
                        values_at(&env.docs[row], field)
                    };
                    let reversed = (*dir < 0) != ix.backward;
                    values
                        .iter()
                        .find(|v| directed(v, reversed, collation) == *part)
                        .or(values.first())
                        .cloned()
                        .unwrap_or(Bson::Null)
                })
                .collect();
            let sort_key: Vec<Bson> = pattern
                .iter()
                .map(|(field, _)| {
                    fields
                        .iter()
                        .position(|(f, _)| f == field)
                        .map_or(Bson::Null, |at| values[at].clone())
                })
                .collect();
            Some(KeyMember {
                sort_key,
                index_key: values,
                key_pattern: ix.index.key.clone(),
                index: ix.index.name.clone(),
                // The rows are the collection's documents in the order
                // they are stored: each one's place stands for its
                // RecordId, which moves only what a spilled run
                // compresses to (RM-556). A clustered collection's is the
                // KeyString of `_id`, whose length is in the member's size.
                rid: record_id_of(env, row),
            })
        })
        .collect()
}

/// One member of a SORT of index keys, as its sorter holds it: its sort
/// key's values, and the index entry it was found at.
pub(crate) struct KeyMember {
    sort_key: Vec<Bson>,
    index_key: Vec<Bson>,
    key_pattern: Document,
    index: String,
    rid: RecordId,
}

/// A `SORT`'s memory and spill counters (RM-315): the rows it sorts, run
/// through the sorter its budget holds, as mongod's executes it. `keys`:
/// a SORT of index keys' members, counted as its sorter counts them
/// (RM-476), not the documents behind them. `keeps_metadata`: a `default`
/// sort of documents, which holds each as the working-set member it was
/// read in (RM-577).
#[allow(clippy::too_many_arguments)]
pub(crate) fn sort_stats(
    extra: &mut Document,
    pattern: &Document,
    limit: Option<i64>,
    input: &Node,
    rows: &[usize],
    keys: Option<Vec<KeyMember>>,
    keeps_metadata: bool,
    env: &Env,
) {
    if !crate::explain::order::plain_sort(pattern) {
        return;
    }
    let top = limit.and_then(|n| usize::try_from(n).ok());
    if let (
        Some(members),
        Node::Projection {
            name: "PROJECTION_COVERED",
            spec,
            ..
        },
    ) = (&keys, input)
    {
        // A covered projection below the sort: it sorts the documents the
        // projection built of the keys, OWNED_OBJ members (RM-473).
        let members: Vec<_> = members
            .iter()
            .map(|m| secantus_core::sorter::OwnedObjMember {
                sort_key: m.sort_key.clone(),
                doc: secantus_core::projection::covered_document(
                    &m.key_pattern,
                    &m.index_key,
                    spec,
                ),
            })
            .collect();
        let (ran, report) = secantus_core::memlimit::captured(|| {
            let _disk = secantus_core::memlimit::allow_disk_use(true);
            secantus_core::sorter::sort_owned_objs(
                members,
                &descending(pattern),
                secantus_core::memlimit::budgets().sort,
                top.filter(|&k| k > 0),
            )
        });
        if ran.is_ok() {
            sort_extra(extra, &report);
        }
        return;
    }
    if let Some(members) = keys {
        // In the order they came, each index's id its place among those
        // registered, as execution's sort takes them (RM-475).
        let mut registered: Vec<&str> = Vec::new();
        let members: Vec<_> = members
            .iter()
            .map(|m| {
                let index_id = registered
                    .iter()
                    .position(|n| *n == m.index)
                    .unwrap_or_else(|| {
                        registered.push(&m.index);
                        registered.len() - 1
                    });
                secantus_core::sorter::IndexKeyMember {
                    sort_key: m.sort_key.clone(),
                    key_pattern: &m.key_pattern,
                    index_key: m.index_key.clone(),
                    index_id: index_id as u32,
                    rid: m.rid.clone(),
                }
            })
            .collect();
        let descending = descending(pattern);
        let budget = secantus_core::memlimit::budgets().sort;
        let (ran, report) = secantus_core::memlimit::captured(|| {
            let _disk = secantus_core::memlimit::allow_disk_use(true);
            secantus_core::sorter::sort_index_keys(
                members,
                &descending,
                budget,
                top.filter(|&k| k > 0),
            )
        });
        if ran.is_ok() {
            sort_extra(extra, &report);
        }
        return;
    }
    if keeps_metadata && !matches!(input, Node::Projection { .. }) {
        // A `default` sort of the documents read: RID_AND_OBJ members, each
        // row's place standing for its RecordId, as `key_members`' do.
        let docs = rows
            .iter()
            .map(|&r| (env.docs[r].clone(), record_id_of(env, r)))
            .collect();
        let (ran, report) = secantus_core::memlimit::captured(|| {
            let _disk = secantus_core::memlimit::allow_disk_use(true);
            secantus_core::sorter::sort_rid_objs(
                docs,
                pattern,
                secantus_core::memlimit::budgets().sort,
                top.filter(|&k| k > 0),
            )
        });
        if ran.is_ok() {
            sort_extra(extra, &report);
        }
        return;
    }
    // A projection below the sort: it sorts the projected documents.
    let docs: Result<Vec<Document>, _> = rows
        .iter()
        .map(|&r| match input {
            Node::Projection { spec, .. } => {
                secantus_core::projection::apply_projection(&env.docs[r], spec, env.query)
            }
            _ => Ok(env.docs[r].clone()),
        })
        .collect();
    let Ok(docs) = docs else {
        return;
    };
    let sizing = secantus_core::sorter::Sizing {
        beside: None,
        sizes: None,
        source: secantus_core::sorter::SortSource::QueryLayer,
        fields: None,
        budget: secantus_core::memlimit::budgets().sort,
        added: None,
    };
    let (ran, report) = secantus_core::memlimit::captured(|| {
        let _disk = secantus_core::memlimit::allow_disk_use(true);
        secantus_core::sorter::sort_documents(docs, &Bson::Document(pattern.clone()), &sizing, top)
    });
    if ran.is_err() {
        return;
    }
    sort_extra(extra, &report);
}

/// Each sort field's direction: descending or not.
fn descending(pattern: &Document) -> Vec<bool> {
    pattern
        .values()
        .map(|dir| crate::util::as_i64(dir).unwrap_or(1) < 0)
        .collect()
}

/// A `SORT`'s memory and spill counters from what its sorter reported.
pub(crate) fn sort_extra(extra: &mut Document, report: &secantus_core::memlimit::SpillReport) {
    // `appendNumber`: an int32 while the count fits (measured 8.2.11, the
    // trial's SORT in `allPlansExecution`).
    extra.insert("totalDataSizeSorted", num(report.sort_total_data_size));
    extra.insert("usedDisk", report.sort_spills > 0);
    extra.insert("spills", num(report.sort_spills));
    extra.insert("spilledRecords", num(report.sort_spilled_records));
    extra.insert("spilledBytes", num(report.sort_spilled_bytes));
    extra.insert(
        "spilledDataStorageSize",
        num(report.sort_spilled_data_storage_size),
    );
}

/// The counters a stage reports whatever it read.
pub(crate) fn fixed_extra(node: &Node, extra: &mut Document) {
    match node {
        Node::Sort { .. } => {
            if !extra.contains_key("usedDisk") {
                extra.insert("usedDisk", false);
                extra.insert("spills", 0_i32);
                extra.insert("spilledRecords", 0_i32);
                extra.insert("spilledBytes", 0_i32);
                extra.insert("spilledDataStorageSize", 0_i32);
            }
        }
        // `MergeSortStats`: zero when no child is multikey.
        Node::SortMerge { .. } => {
            for key in ["dupsTested", "dupsDropped"] {
                if !extra.contains_key(key) {
                    extra.insert(key, 0_i32);
                }
            }
        }
        Node::DistinctScan(..) => {
            extra.insert("docsExamined", 0_i32);
            extra.insert("chunkSkips", 0_i32);
            extra.insert("orphanChunkSkips", 0_i32);
        }
        _ => {}
    }
}

/// What a count that reads no documents reports: `(counted, skipped)`.
fn fast_count(env: &Env) -> (i64, i64) {
    let total = env.docs.len() as i64;
    let skipped = env.skip.min(total);
    let mut counted = total - skipped;
    if env.limit > 0 {
        counted = counted.min(env.limit);
    }
    (counted, skipped)
}

/// The documents a plan returns, in the order it returns them.
pub(crate) fn rows(plan: &Node, env: &Env) -> Vec<usize> {
    let mut root = Run::new(plan, env);
    std::iter::from_fn(|| root.pull(env)).collect()
}

/// Run the plan to completion and report what each stage counted.
pub(crate) fn run(plan: &Node, env: &Env) -> Stats {
    let mut root = Run::new(plan, env);
    if matches!(plan, Node::FastCount) {
        let (counted, skipped) = fast_count(env);
        root.extra.insert("nCounted", num(counted));
        root.extra.insert("nSkipped", num(skipped));
    }
    while root.pull(env).is_some() {}
    let mut stats = root.stats();
    if let Some(worked) = super::trial::worked_through(plan, env) {
        graft_work(&mut stats, &worked);
    }
    stats
}

/// [`run`], with what the plan had examined, `(keys, docs)`, as it
/// returned each of its documents: where a batch that ends at that
/// document stops counting, as mongod's executor stops working when the
/// batch is full (RM-708).
pub(crate) fn run_marks(plan: &Node, env: &Env) -> (Vec<(i64, i64)>, Stats) {
    let mut root = Run::new(plan, env);
    let mut marks = Vec::new();
    while root.pull(env).is_some() {
        marks.push(root.stats().totals());
    }
    (marks, root.stats())
}

/// Each stage's work accounting from `worked`, a tree of the same plan.
fn graft_work(stats: &mut Stats, worked: &Stats) {
    if worked.work.is_some() || stats.work.is_none() {
        stats.work = worked.work;
    }
    if stats.children.len() == worked.children.len() {
        for (s, w) in stats.children.iter_mut().zip(&worked.children) {
            graft_work(s, w);
        }
    }
}
