//! The plan tree `explain` reports, and how each stage is printed.

use super::bounds::{render_bounds, render_key_pattern, Interval};
use bson::{doc, Bson, Document};
use secantus_core::collation::Collation;
use std::rc::Rc;

/// A count as mongod sends it: an int32 while it fits.
pub(crate) fn num(n: i64) -> Bson {
    match i32::try_from(n) {
        Ok(small) => Bson::Int32(small),
        Err(_) => Bson::Int64(n),
    }
}

/// What the planner knows about one index.
#[derive(Debug)]
pub(crate) struct IndexInfo {
    pub name: String,
    pub key: Document,
    pub unique: bool,
    pub sparse: bool,
    pub partial: Option<Document>,
    /// Per key field, the path components that hold arrays.
    pub multikey_paths: Vec<Vec<String>>,
    pub version: i32,
    pub collation: Option<Collation>,
    /// Every key value is `1` or `-1` (no text, geo, hashed or wildcard key).
    pub plain: bool,
}

impl IndexInfo {
    pub fn is_multikey(&self) -> bool {
        self.multikey_paths.iter().any(|p| !p.is_empty())
    }

    pub fn fields(&self) -> Vec<(&str, i64)> {
        self.key
            .iter()
            .map(|(k, v)| (k.as_str(), crate::util::as_i64(v).unwrap_or(1)))
            .collect()
    }

    pub fn position(&self, field: &str) -> Option<usize> {
        self.key.keys().position(|k| k == field)
    }

    /// The flags every index-scanning stage carries, in mongod's order.
    fn describe(&self, out: &mut Document) {
        out.insert("keyPattern", self.key.clone());
        out.insert("indexName", self.name.clone());
        if let Some(c) = &self.collation {
            out.insert("collation", c.spec().clone());
        }
        out.insert("isMultiKey", self.is_multikey());
        // The `_id` index cannot be multikey, and mongod prints no paths for it.
        if self.name != "_id_" {
            let mut paths = Document::new();
            for (field, parts) in self.key.keys().zip(&self.multikey_paths) {
                paths.insert(
                    field.clone(),
                    parts.iter().cloned().map(Bson::String).collect::<Vec<_>>(),
                );
            }
            out.insert("multiKeyPaths", paths);
        }
        out.insert("isUnique", self.unique);
        out.insert("isSparse", self.sparse);
        out.insert("isPartial", self.partial.is_some());
        out.insert("indexVersion", self.version);
    }
}

/// One index scan: the index, the bounds per key field, the direction.
#[derive(Clone, Debug)]
pub(crate) struct IxScan {
    pub index: Rc<IndexInfo>,
    /// Ascending intervals per key field, whatever the field's direction.
    pub bounds: Vec<Vec<Interval>>,
    pub backward: bool,
    /// A predicate rechecked against the key.
    pub filter: Option<Document>,
    /// `min` / `max` were given: mongod prints no bounds.
    pub no_bounds: bool,
}

impl IxScan {
    pub fn full(index: Rc<IndexInfo>) -> Self {
        let n = index.key.len();
        IxScan {
            index,
            bounds: vec![vec![Interval::all()]; n],
            backward: false,
            filter: None,
            no_bounds: false,
        }
    }

    fn bounds_doc(&self) -> Document {
        let mut out = Document::new();
        if self.no_bounds {
            return out;
        }
        for ((field, dir), list) in self.index.fields().into_iter().zip(&self.bounds) {
            out.insert(field, render_bounds(list, (dir < 0) != self.backward));
        }
        out
    }

    fn direction(&self) -> &'static str {
        if self.backward {
            "backward"
        } else {
            "forward"
        }
    }

    /// The first and last key of a scan with one interval per field, for
    /// `COUNT_SCAN`. `None` when some field has several intervals.
    pub fn single_range(&self) -> Option<(Document, bool, Document, bool)> {
        let (mut start, mut end) = (Document::new(), Document::new());
        let (mut start_incl, mut end_incl) = (true, true);
        for ((field, dir), list) in self.index.fields().into_iter().zip(&self.bounds) {
            let [one] = list.as_slice() else { return None };
            let (lo, lo_incl, hi, hi_incl) = if dir < 0 {
                (&one.hi, one.hi_incl, &one.lo, one.lo_incl)
            } else {
                (&one.lo, one.lo_incl, &one.hi, one.hi_incl)
            };
            start.insert(field, lo.clone());
            end.insert(field, hi.clone());
            start_incl &= lo_incl;
            end_incl &= hi_incl;
        }
        Some((start, start_incl, end, end_incl))
    }
}

/// A stage of the plan.
#[derive(Clone, Debug)]
pub(crate) enum Node {
    CollScan {
        filter: Option<Document>,
        backward: bool,
    },
    IxScan(IxScan),
    Fetch {
        filter: Option<Document>,
        input: Box<Node>,
    },
    Or(Vec<Node>),
    SortMerge {
        pattern: Document,
        inputs: Vec<Node>,
    },
    Sort {
        pattern: Document,
        limit: Option<i64>,
        /// Sorting index keys below the fetch: mongod calls the type `default`.
        on_keys: bool,
        input: Box<Node>,
    },
    Skip(i64, Box<Node>),
    Limit(i64, Box<Node>),
    Projection {
        name: &'static str,
        spec: Document,
        input: Box<Node>,
    },
    ReturnKey(Box<Node>),
    Subplan(Box<Node>),
    Eof(&'static str),
    /// A point lookup through a unique index: `path == value`.
    Express {
        index: Rc<IndexInfo>,
        value: Bson,
    },
    IdHack(Bson),
    /// An update or delete of one document by `_id`.
    ExpressWrite {
        index: Rc<IndexInfo>,
        value: Bson,
        delete: bool,
    },
    Count(Box<Node>),
    CountScan(IxScan),
    FastCount,
    /// The scan, and how many leading key fields make a distinct value.
    DistinctScan(IxScan, usize),
    Update(Box<Node>),
    Delete {
        batched: bool,
        input: Box<Node>,
    },
}

/// What running a stage counted. Mirrors the `Node` tree.
#[derive(Clone, Debug, Default)]
pub(crate) struct Stats {
    pub n_returned: i64,
    pub keys: i64,
    pub docs: i64,
    /// Stage-specific counters, appended after the plan's own fields.
    pub extra: Document,
    pub children: Vec<Stats>,
}

impl Stats {
    /// Keys and documents examined by this stage and everything under it.
    pub fn totals(&self) -> (i64, i64) {
        let (mut k, mut d) = (self.keys, self.docs);
        for c in &self.children {
            let (ck, cd) = c.totals();
            k += ck;
            d += cd;
        }
        (k, d)
    }
}

fn child(stats: Option<&Stats>, i: usize) -> Option<&Stats> {
    stats.and_then(|s| s.children.get(i))
}

impl Node {
    pub fn fetch(filter: Option<Document>, input: Node) -> Node {
        Node::Fetch {
            filter,
            input: Box::new(input),
        }
    }

    /// The stage as `explain` prints it; with `stats`, as `executionStages`.
    pub fn to_doc(&self, stats: Option<&Stats>) -> Document {
        let mut out = Document::new();
        let counted = |out: &mut Document| {
            if let Some(s) = stats {
                out.insert("nReturned", num(s.n_returned));
            }
        };
        let extra = |out: &mut Document| {
            if let Some(s) = stats {
                for (k, v) in &s.extra {
                    out.insert(k.clone(), v.clone());
                }
            }
        };
        let input_of = |out: &mut Document, input: &Node| {
            out.insert("inputStage", input.to_doc(child(stats, 0)));
        };
        let inputs_of = |out: &mut Document, inputs: &[Node]| {
            let list: Vec<Bson> = inputs
                .iter()
                .enumerate()
                .map(|(i, n)| Bson::Document(n.to_doc(child(stats, i))))
                .collect();
            out.insert("inputStages", list);
        };
        match self {
            Node::CollScan { filter, backward } => {
                out.insert("stage", "COLLSCAN");
                if let Some(f) = filter {
                    out.insert("filter", f.clone());
                }
                counted(&mut out);
                out.insert("direction", if *backward { "backward" } else { "forward" });
                if let Some(s) = stats {
                    out.insert("docsExamined", num(s.docs));
                }
            }
            Node::IxScan(ix) => {
                out.insert("stage", "IXSCAN");
                if let Some(f) = &ix.filter {
                    out.insert("filter", f.clone());
                }
                counted(&mut out);
                ix.index.describe(&mut out);
                out.insert("direction", ix.direction());
                out.insert("indexBounds", ix.bounds_doc());
                if let Some(s) = stats {
                    out.insert("keysExamined", num(s.keys));
                }
            }
            Node::Fetch { filter, input } => {
                out.insert("stage", "FETCH");
                if let Some(f) = filter {
                    out.insert("filter", f.clone());
                }
                counted(&mut out);
                if let Some(s) = stats {
                    out.insert("docsExamined", num(s.docs));
                }
                input_of(&mut out, input);
            }
            Node::Or(inputs) => {
                out.insert("stage", "OR");
                counted(&mut out);
                extra(&mut out);
                inputs_of(&mut out, inputs);
            }
            Node::SortMerge { pattern, inputs } => {
                out.insert("stage", "SORT_MERGE");
                counted(&mut out);
                out.insert("sortPattern", pattern.clone());
                extra(&mut out);
                inputs_of(&mut out, inputs);
            }
            Node::Sort {
                pattern,
                limit,
                on_keys,
                input,
            } => {
                out.insert("stage", "SORT");
                counted(&mut out);
                out.insert("sortPattern", pattern.clone());
                out.insert("memLimit", 104_857_600_i32);
                if let Some(n) = limit {
                    out.insert("limitAmount", num(*n));
                }
                out.insert("type", if *on_keys { "default" } else { "simple" });
                extra(&mut out);
                input_of(&mut out, input);
            }
            Node::Skip(n, input) => {
                out.insert("stage", "SKIP");
                counted(&mut out);
                out.insert("skipAmount", num(*n));
                input_of(&mut out, input);
            }
            Node::Limit(n, input) => {
                out.insert("stage", "LIMIT");
                counted(&mut out);
                out.insert("limitAmount", num(*n));
                input_of(&mut out, input);
            }
            Node::Projection { name, spec, input } => {
                out.insert("stage", *name);
                counted(&mut out);
                out.insert("transformBy", spec.clone());
                input_of(&mut out, input);
            }
            Node::ReturnKey(input) => {
                out.insert("stage", "RETURN_KEY");
                counted(&mut out);
                input_of(&mut out, input);
            }
            Node::Subplan(input) => {
                out.insert("stage", "SUBPLAN");
                counted(&mut out);
                input_of(&mut out, input);
            }
            Node::Eof(kind) => {
                out.insert("stage", "EOF");
                counted(&mut out);
                out.insert("type", *kind);
            }
            Node::Express { index, .. } => {
                out.insert("stage", "EXPRESS_IXSCAN");
                out.insert("keyPattern", render_key_pattern(&index.key));
                out.insert("indexName", index.name.clone());
                if let Some(s) = stats {
                    out.insert("keysExamined", num(s.keys));
                    out.insert("docsExamined", num(s.docs));
                    out.insert("nReturned", num(s.n_returned));
                }
            }
            Node::IdHack(_) => {
                out.insert("stage", "IDHACK");
                if let Some(s) = stats {
                    out.insert("nReturned", num(s.n_returned));
                    out.insert("keysExamined", num(s.keys));
                    out.insert("docsExamined", num(s.docs));
                }
            }
            Node::ExpressWrite { index, delete, .. } => {
                out.insert(
                    "stage",
                    if *delete { "EXPRESS_DELETE" } else { "EXPRESS_UPDATE" },
                );
                out.insert("keyPattern", render_key_pattern(&index.key));
                out.insert("indexName", index.name.clone());
                if let Some(s) = stats {
                    out.insert("keysExamined", num(s.keys));
                    out.insert("docsExamined", num(s.docs));
                    out.insert("nReturned", num(s.n_returned));
                }
                extra(&mut out);
            }
            Node::Count(input) => {
                out.insert("stage", "COUNT");
                counted(&mut out);
                extra(&mut out);
                input_of(&mut out, input);
            }
            Node::CountScan(ix) => {
                out.insert("stage", "COUNT_SCAN");
                counted(&mut out);
                if let Some(s) = stats {
                    out.insert("keysExamined", num(s.keys));
                }
                ix.index.describe(&mut out);
                let (start, start_incl, end, end_incl) =
                    ix.single_range().unwrap_or_default();
                out.insert(
                    "indexBounds",
                    doc! {
                        "startKey": start,
                        "startKeyInclusive": start_incl,
                        "endKey": end,
                        "endKeyInclusive": end_incl,
                    },
                );
            }
            Node::FastCount => {
                out.insert("stage", "RECORD_STORE_FAST_COUNT");
                counted(&mut out);
                extra(&mut out);
            }
            Node::DistinctScan(ix, _) => {
                out.insert("stage", "DISTINCT_SCAN");
                counted(&mut out);
                ix.index.describe(&mut out);
                out.insert("isShardFiltering", false);
                out.insert("isFetching", false);
                out.insert("direction", ix.direction());
                out.insert("indexBounds", ix.bounds_doc());
                if let Some(s) = stats {
                    out.insert("keysExamined", num(s.keys));
                }
                extra(&mut out);
            }
            Node::Update(input) => {
                out.insert("stage", "UPDATE");
                counted(&mut out);
                extra(&mut out);
                input_of(&mut out, input);
            }
            Node::Delete { batched, input } => {
                out.insert(
                    "stage",
                    if *batched { "BATCHED_DELETE" } else { "DELETE" },
                );
                counted(&mut out);
                extra(&mut out);
                input_of(&mut out, input);
            }
        }
        out
    }
}
