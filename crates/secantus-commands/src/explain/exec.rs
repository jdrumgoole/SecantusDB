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
use super::node::{num, IndexInfo, IxScan, Node, Stats};
use bson::{Bson, Document};
use secantus_core::collation::Collation;
use secantus_core::heap::Heap;
use std::collections::{BTreeSet, VecDeque};

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
    /// Every filter a plan runs passes exactly the documents the query
    /// matches, among those its scan returns.
    pub known: Option<&'a [bool]>,
}

impl Env<'_> {
    pub(crate) fn matches(&self, row: usize, filter: &Option<Document>) -> bool {
        if let (Some(known), Some(_)) = (self.known, filter) {
            return known[row];
        }
        match filter {
            None => true,
            Some(f) => {
                secantus_core::query::matches(&self.docs[row], f, &Document::new(), self.collation)
                    .unwrap_or(false)
            }
        }
    }
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
fn directed(v: &Bson, reversed: bool, collation: Option<&Collation>) -> Vec<u8> {
    let bytes = match (v, collation) {
        (Bson::String(_), Some(_)) => {
            secantus_core::sortkey::encode_sort_value(v, collation).unwrap_or_default()
        }
        _ => sort_key(v),
    };
    if !reversed {
        return bytes;
    }
    let mut out = Vec::with_capacity(bytes.len() + 2);
    for b in bytes {
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
    for (row, doc) in env.docs.iter().enumerate() {
        if partial.is_some() && !env.matches(row, &partial) {
            continue;
        }
        let per_field: Vec<Vec<Bson>> = fields
            .iter()
            .map(|(field, _)| values_at(doc, field).0)
            .collect();
        if index.sparse && per_field.iter().all(Vec::is_empty) {
            continue;
        }
        let reversed = |dir: i64| (dir < 0) != backward;
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

/// One field's intervals in scan order: `(low, inclusive, high, inclusive)`.
type Ranges = Vec<(Vec<u8>, bool, Vec<u8>, bool)>;

fn ranges(list: &[Interval], reversed: bool, collation: Option<&Collation>) -> Ranges {
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
enum Step {
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
/// (`secantus_core::heap`, libc++'s).
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
    multikey: bool,
    filter: Option<Document>,
    pos: usize,
    started: bool,
    done: bool,
    seen: BTreeSet<usize>,
    keys: i64,
    returned: i64,
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
        // Points, then one interval, then everything: a single run of keys.
        let mut contiguous = true;
        let mut open = false;
        for list in &ix.bounds {
            let [one] = list.as_slice() else {
                contiguous = false;
                break;
            };
            if open && !one.is_all() {
                contiguous = false;
                break;
            }
            if !one.is_point() {
                open = true;
            }
        }
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
            multikey: ix.index.is_multikey(),
            filter: ix.filter.clone(),
            pos: 0,
            started: false,
            done: false,
            seen: BTreeSet::new(),
            keys: 0,
            returned: 0,
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

/// The first field whose key is outside `ranges`, and whether a later
/// interval of that field could still be reached.
fn outside_of(ranges: &[Ranges], key: &[Vec<u8>]) -> Option<(usize, bool)> {
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
        let start: Vec<&[u8]> = self
            .ranges
            .iter()
            .map(|list| list.first().map_or(&[][..], |r| r.0.as_slice()))
            .collect();
        while self.pos < self.entries.len() {
            let key = &self.entries[self.pos].0;
            let before = key
                .iter()
                .map(Vec::as_slice)
                .zip(start.iter().copied())
                .find_map(|(k, s)| (k != s).then_some(k < s))
                .unwrap_or(false);
            if !before {
                break;
            }
            self.pos += 1;
        }
    }

    /// One unit of mongod's work: an entry inside the bounds, a key that
    /// sends the scan seeking, or the end. The key that ends a scan of
    /// several intervals is examined in the work that ends it; a single run
    /// of keys ends without examining one, as does the end of the index.
    fn work_entry(&mut self) -> Step {
        if !self.started {
            self.started = true;
            self.seek_start();
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
            if !ahead && field == 0 {
                self.done = true;
                return Step::End;
            }
            // Seek: past every key that fails the same way.
            let prefix = key[..field].to_vec();
            self.pos += 1;
            while self.pos < self.entries.len() {
                let other = &self.entries[self.pos].0;
                if other[..field] != prefix[..] {
                    break;
                }
                if ahead && !matches!(self.outside(other), Some((f, true)) if f == field) {
                    break;
                }
                self.pos += 1;
            }
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
                if (self.multikey && !self.seen.insert(row)) || !env.matches(row, &self.filter) {
                    Work::NeedTime
                } else {
                    self.returned += 1;
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
            if !env.matches(row, &self.filter) {
                continue;
            }
            self.returned += 1;
            return Some(row);
        }
    }

    /// The keys the scan has examined.
    pub(crate) fn keys(&self) -> i64 {
        self.keys
    }

    /// A DISTINCT_SCAN's next document: the first entry of a new value of
    /// the leading `width` fields (the scan seeks past the rest).
    pub(crate) fn next_distinct(
        &mut self,
        width: usize,
        prefixes: &mut BTreeSet<Vec<Vec<u8>>>,
    ) -> Option<usize> {
        loop {
            let row = self.next_entry()?;
            let key = &self.entries[self.pos - 1].0;
            if prefixes.insert(key[..width.min(key.len())].to_vec()) {
                return Some(row);
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
            | Node::Subplan(input)
            | Node::Count(input)
            | Node::Update(input) => vec![input],
            Node::Or(inputs) | Node::AndSorted(inputs) | Node::SortMerge { inputs, .. } => {
                inputs.iter().collect()
            }
            _ => vec![],
        };
        let scan = match node {
            Node::IxScan(ix) | Node::CountScan(ix) | Node::DistinctScan(ix, _) => {
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
        Run {
            node,
            inputs: inputs.into_iter().map(|n| Run::new(n, env)).collect(),
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
                if row.is_none() && scan.key_beyond() {
                    self.keys += 1;
                }
                row
            }
            Node::DistinctScan(_, width) => {
                // One entry per distinct value of the leading `width` fields.
                let scan = self.scan.as_mut()?;
                loop {
                    let Some(row) = scan.next_entry() else {
                        self.keys = self.prefixes.len() as i64 + i64::from(scan.key_beyond());
                        return None;
                    };
                    let key = &scan.entries[scan.pos - 1].0;
                    if self
                        .prefixes
                        .insert(key[..(*width).min(key.len())].to_vec())
                    {
                        self.keys = self.prefixes.len() as i64;
                        return Some(row);
                    }
                }
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
            Node::Sort { limit, .. } => {
                if self.buffer.is_none() {
                    let mut all = Vec::new();
                    while let Some(row) = self.inputs[0].pull(env) {
                        all.push(row);
                    }
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
            Node::Projection { .. } | Node::ReturnKey(_) | Node::Subplan(_) => {
                self.inputs[0].pull(env)
            }
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
        Stats {
            n_returned: self.returned,
            keys: self.keys,
            docs: self.docs,
            extra,
            children: self.inputs.iter().map(Run::stats).collect(),
        }
    }
}

/// The counters a stage reports whatever it read.
pub(crate) fn fixed_extra(node: &Node, extra: &mut Document) {
    match node {
        Node::Sort { .. } => {
            extra.insert("usedDisk", false);
            extra.insert("spills", 0_i32);
            extra.insert("spilledRecords", 0_i32);
            extra.insert("spilledBytes", 0_i32);
            extra.insert("spilledDataStorageSize", 0_i32);
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
    root.stats()
}
