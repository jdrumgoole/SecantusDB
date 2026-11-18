//! An index scan as mongod's `IndexScan` works it, over a geo index's
//! entries: one key read per `work()`, the bounds checked by
//! `IndexBoundsChecker`, which answers a key outside them with a seek to
//! where the bounds go on. What it counts is what mongod's IXSCAN reports
//! (`keysExamined`, `seeks`, the work accounting). A near stage's scans do
//! not drop a document's second entry, multikey or not (measured, 8.2.11):
//! the near stage drops a document it has buffered.

use super::{FieldBounds, Key, KeyPart};
use std::cmp::Ordering;

/// One end of an interval.
#[derive(Clone, Debug)]
enum Bound {
    Min,
    Part(KeyPart),
    Max,
}

fn cmp_bound(p: &KeyPart, b: &Bound) -> Ordering {
    match b {
        Bound::Min => Ordering::Greater,
        Bound::Max => Ordering::Less,
        Bound::Part(q) => p.cmp(q),
    }
}

#[derive(Clone, Debug)]
struct Iv {
    lo: Bound,
    lo_in: bool,
    hi: Bound,
    hi_in: bool,
}

impl Iv {
    /// `Less`: the value is before the interval; `Greater`: after it.
    fn locate(&self, p: &KeyPart) -> Ordering {
        match cmp_bound(p, &self.lo) {
            Ordering::Less => return Ordering::Less,
            Ordering::Equal if !self.lo_in => return Ordering::Less,
            _ => {}
        }
        match cmp_bound(p, &self.hi) {
            Ordering::Greater => Ordering::Greater,
            Ordering::Equal if !self.hi_in => Ordering::Greater,
            _ => Ordering::Equal,
        }
    }
}

fn intervals(b: &FieldBounds) -> Vec<Iv> {
    let iv = |lo: KeyPart, lo_in, hi: KeyPart, hi_in| Iv {
        lo: Bound::Part(lo),
        lo_in,
        hi: Bound::Part(hi),
        hi_in,
    };
    match b {
        FieldBounds::Cells(l) => l
            .iter()
            .map(|&(a, z)| iv(KeyPart::Cell(a), true, KeyPart::Cell(z), true))
            .collect(),
        FieldBounds::Hashes(l) => l
            .iter()
            .map(|&(a, z)| iv(KeyPart::Hash(a), true, KeyPart::Hash(z), true))
            .collect(),
        FieldBounds::Point(v) => vec![iv(
            KeyPart::Value(v.clone()),
            true,
            KeyPart::Value(v.clone()),
            true,
        )],
        FieldBounds::Ranges(l) => l
            .iter()
            .map(|(a, ai, z, zi)| {
                iv(
                    KeyPart::Value(a.clone()),
                    *ai,
                    KeyPart::Value(z.clone()),
                    *zi,
                )
            })
            .collect(),
        FieldBounds::Full => vec![Iv {
            lo: Bound::Min,
            lo_in: true,
            hi: Bound::Max,
            hi_in: true,
        }],
    }
}

/// Where the cursor goes next: the key's first `prefix` fields, then
/// (unless `exclusive`, past every key with that prefix) the starts of the
/// current intervals of the rest.
struct Seek {
    prefix: Vec<KeyPart>,
    exclusive: bool,
    suffix: Vec<(Bound, bool)>,
}

impl Seek {
    fn admits(&self, key: &Key) -> bool {
        for (f, p) in self.prefix.iter().enumerate() {
            match key[f].cmp(p) {
                Ordering::Equal => {}
                c => return c == Ordering::Greater,
            }
        }
        if self.exclusive {
            return false;
        }
        for (at, (b, incl)) in self.suffix.iter().enumerate() {
            match cmp_bound(&key[self.prefix.len() + at], b) {
                Ordering::Greater => return true,
                Ordering::Less => return false,
                Ordering::Equal if !incl => return false,
                Ordering::Equal => {}
            }
        }
        true
    }
}

enum Check {
    Valid,
    Done,
    Advance(Seek),
}

/// `IndexBoundsChecker`, forward.
struct Checker {
    fields: Vec<Vec<Iv>>,
    cur: Vec<usize>,
}

impl Checker {
    fn starts(&self, from: usize) -> Vec<(Bound, bool)> {
        (from..self.fields.len())
            .map(|f| {
                let iv = &self.fields[f][self.cur[f]];
                (iv.lo.clone(), iv.lo_in)
            })
            .collect()
    }

    fn seek(&self, key: &Key, i: usize) -> Seek {
        Seek {
            prefix: key[..i].to_vec(),
            exclusive: false,
            suffix: self.starts(i),
        }
    }

    fn reset_after(&mut self, i: usize) {
        for c in &mut self.cur[i + 1..] {
            *c = 0;
        }
    }

    fn check(&mut self, key: &Key) -> Check {
        let mut i = 0;
        while i < self.fields.len() {
            match self.fields[i][self.cur[i]].locate(&key[i]) {
                Ordering::Equal => {
                    i += 1;
                    continue;
                }
                Ordering::Less => return Check::Advance(self.seek(key, i)),
                Ordering::Greater => {}
            }
            let found = self.fields[i]
                .iter()
                .enumerate()
                .skip(self.cur[i])
                .find(|(_, iv)| iv.locate(&key[i]) != Ordering::Greater)
                .map(|(j, iv)| (j, iv.locate(&key[i])));
            match found {
                Some((j, Ordering::Equal)) => {
                    self.cur[i] = j;
                    self.reset_after(i);
                    i += 1;
                }
                Some((j, _)) => {
                    self.cur[i] = j;
                    self.reset_after(i);
                    return Check::Advance(self.seek(key, i));
                }
                None if i == 0 => return Check::Done,
                None => {
                    for c in &mut self.cur[i..] {
                        *c = 0;
                    }
                    return Check::Advance(Seek {
                        prefix: key[..i].to_vec(),
                        exclusive: true,
                        suffix: Vec::new(),
                    });
                }
            }
        }
        Check::Valid
    }
}

/// What one `work()` of the scan returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// The entry at this position of the sorted entries.
    Advanced(usize),
    NeedTime,
    Eof,
}

/// An IXSCAN's `IndexScanStats` and work accounting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanCounts {
    pub works: i64,
    pub advanced: i64,
    pub need_time: i64,
    pub keys_examined: i64,
    pub seeks: i64,
}

/// An index scan over `entries` (every entry of the index, sorted, with the
/// owner of each), bounded per field by `bounds`.
pub(crate) struct IndexScan<'a> {
    entries: &'a [(Key, usize)],
    checker: Checker,
    empty: bool,
    pos: Option<usize>,
    pending: Option<Seek>,
    started: bool,
    done: bool,
    /// A key filter, per field (`Full`: none), as mongod puts a `2d`
    /// index's other fields' predicates on its ring scans.
    filter: Option<Vec<FieldBounds>>,
    pub counts: ScanCounts,
}

impl<'a> IndexScan<'a> {
    pub(crate) fn new(entries: &'a [(Key, usize)], bounds: &[FieldBounds]) -> IndexScan<'a> {
        let fields: Vec<Vec<Iv>> = bounds.iter().map(intervals).collect();
        let empty = fields.iter().any(Vec::is_empty);
        let cur = vec![0; fields.len()];
        IndexScan {
            entries,
            checker: Checker { fields, cur },
            empty,
            pos: None,
            pending: None,
            started: false,
            done: false,
            filter: None,
            counts: ScanCounts::default(),
        }
    }

    pub(crate) fn with_filter(mut self, filter: Vec<FieldBounds>) -> Self {
        self.filter = Some(filter);
        self
    }

    fn seek_to(&self, seek: &Seek) -> Option<usize> {
        let from = self.pos.map_or(0, |p| p + 1);
        (from..self.entries.len()).find(|&e| seek.admits(&self.entries[e].0))
    }

    /// One `work()`.
    pub(crate) fn work(&mut self) -> Step {
        self.counts.works += 1;
        if self.done {
            return Step::Eof;
        }
        let at = if !self.started {
            self.started = true;
            self.counts.seeks += 1;
            if self.empty {
                None
            } else {
                let start = Seek {
                    prefix: Vec::new(),
                    exclusive: false,
                    suffix: self.checker.starts(0),
                };
                self.seek_to(&start)
            }
        } else if let Some(seek) = self.pending.take() {
            self.counts.seeks += 1;
            self.seek_to(&seek)
        } else {
            let next = self.pos.map_or(0, |p| p + 1);
            (next < self.entries.len()).then_some(next)
        };
        let Some(at) = at else {
            self.done = true;
            return Step::Eof;
        };
        self.pos = Some(at);
        self.counts.keys_examined += 1;
        match self.checker.check(&self.entries[at].0) {
            Check::Done => {
                self.done = true;
                Step::Eof
            }
            Check::Advance(seek) => {
                self.pending = Some(seek);
                self.counts.need_time += 1;
                Step::NeedTime
            }
            Check::Valid => {
                if let Some(filter) = &self.filter {
                    let key = &self.entries[at].0;
                    if !filter.iter().zip(key).all(|(b, p)| b.admits(p)) {
                        self.counts.need_time += 1;
                        return Step::NeedTime;
                    }
                }
                self.counts.advanced += 1;
                Step::Advanced(at)
            }
        }
    }
}
