//! A plan cache entry's `estimatedSizeBytes`: the bytes mongod 8.2.11
//! estimates the entry takes in its memory
//! (`PlanCacheEntryBase::_estimateObjectSizeInBytes`, `plan_cache.h`).
//!
//! The estimate is a "deep size" over mongod's C++ objects, so it is the
//! sum of two kinds of term:
//!
//! - what the entry holds -- BSON sizes (`objsize()`), string capacities,
//!   vector capacities -- which follows from the entry this server builds;
//! - the `sizeof` of each C++ type, which follows from how mongod was
//!   built: the standard library and the CPU. Those are a [`Layout`], one
//!   per mongod build, chosen by the target this server is built for
//!   (Joe, 2026-11-08).
//!
//! The tree is mongod's, function for function:
//!
//! - the entry: `sizeof(PlanCacheEntryBase)`, its `SolutionCacheData` and
//!   its `DebugInfo`;
//! - `SolutionCacheData`: its `sizeof` and its `PlanCacheIndexTree`, one
//!   node per node of the query's match expression, each node carrying a
//!   copy of the `IndexEntry` its index tag names. mongod tags the
//!   predicates its plan assigned to an index, and then every node above
//!   them with the lowest-numbered index below it (`tagForSort`);
//! - `DebugInfo`: the BSON of the query, sort, projection, collation and
//!   distinct it was created from, and its `PlanRankingDecision` -- every
//!   candidate's `PlanStageStats` tree and the vectors of scores and of the
//!   candidates' order, by capacity.
//!
//! Measured against mongod 8.2.11 on macOS arm64 (2026-11-08,
//! `tests/backlog/test_rm_340_342_plan_cache_gaps.py`): every term below
//! reproduces mongod's figure exactly over one and two field indexes,
//! `_id_`, multikey fields, sorts, projections, skips, limits, `$in`,
//! `$ne`, ranges, index names around the short-string limit, three
//! candidates, `AND_SORTED` and `SORT_MERGE` candidates, and the entries a
//! query run in SBE caches.

use bson::{Bson, Document};

/// The `sizeof` of each type the estimate counts, for one mongod build.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    /// `PlanCacheEntryBase<SolutionCacheData, DebugInfo>`.
    pub entry: u64,
    pub solution_cache_data: u64,
    pub index_tree: u64,
    pub index_entry: u64,
    pub ranking_decision: u64,
    /// `BasePlanStageStats<CommonStats, StageType>`.
    pub stage_stats: u64,
    /// `MultikeyComponents`: one per key field of `multikeyPaths`.
    pub multikey_components: u64,
    pub index_scan_stats: u64,
    pub fetch_stats: u64,
    pub sort_stats: u64,
    pub limit_stats: u64,
    pub skip_stats: u64,
    pub projection_stats: u64,
    pub and_sorted_stats: u64,
    pub merge_sort_stats: u64,
    /// `OrStats` (measured 8.2.11: an `OR` winner adds 32 bytes).
    pub or_stats: u64,
    /// `std::string::capacity()` of a string of this length, built by copy.
    pub string_capacity: fn(usize) -> u64,
}

/// libc++ on arm64: mongod's macOS build. Each `sizeof` is the type's
/// members as `r8.2.11` declares them, laid out by the LP64 ABI with
/// libc++'s 24-byte `std::string`, `std::vector` and `std::set`; the sum
/// they make is mongod's figure (measured 8.2.11, 2026-11-08).
pub(crate) const LIBCXX_ARM64: Layout = Layout {
    entry: 88,
    solution_cache_data: 40,
    index_tree: 72,
    index_entry: 328,
    ranking_decision: 96,
    stage_stats: 160,
    multikey_components: 32,
    index_scan_stats: 320,
    fetch_stats: 24,
    sort_stats: 104,
    limit_stats: 16,
    skip_stats: 16,
    projection_stats: 24,
    and_sorted_stats: 32,
    merge_sort_stats: 40,
    or_stats: 32,
    string_capacity: libcxx_string_capacity,
};

/// libc++'s `basic_string::__recommend`: up to 22 characters fit in the
/// object itself; a longer string's buffer is its length and terminator
/// rounded up to 8, and never exactly the inline capacity plus one
/// (measured 8.2.11: names of 22, 23, 24, 31, 32 and 40 characters add
/// 0, 2, 9, 9, 17 and 25 bytes a copy).
fn libcxx_string_capacity(len: usize) -> u64 {
    const INLINE: u64 = 22;
    let len = len as u64;
    if len <= INLINE {
        return INLINE;
    }
    let guess = ((len + 1 + 7) & !7) - 1;
    if guess == INLINE + 1 {
        guess + 1
    } else {
        guess
    }
}

/// The layout of the mongod build this server stands in for: the
/// platform it is built for. `None` where no mongod build of the platform
/// has been measured (Linux x86_64's libstdc++ layout is RM-442): the
/// field is then left out rather than given another build's number.
pub(crate) const LAYOUT: Option<Layout> = if cfg!(all(target_os = "macos", target_arch = "aarch64"))
{
    Some(LIBCXX_ARM64)
} else {
    None
};

/// An index as an `IndexEntry` copy holds it.
#[derive(Clone, Debug)]
pub(crate) struct IndexCopy {
    pub name: String,
    pub key: Document,
    /// Per key field, how many path components are multikey; empty for an
    /// index without path-level multikey metadata (`_id_`).
    pub multikey: Vec<usize>,
    /// Its position among the query's relevant indexes, the catalog's
    /// order: `tagForSort` tags a node with its lowest-numbered index.
    pub number: usize,
}

/// The index tags mongod's plan put on one clause of the conjunction.
#[derive(Clone, Debug)]
pub(crate) enum ClauseTag {
    /// No index answers the clause: it is rechecked by a FETCH.
    None,
    /// The clause's predicate set the bounds of this index.
    One(IndexCopy),
    /// An `$or` whose every branch is one predicate that sets the bounds
    /// of the index given for it, in the order of the branches.
    Or(Vec<IndexCopy>),
}

/// A node of the `PlanCacheIndexTree`: the match expression's node, and the
/// index its tag names, if any.
#[derive(Clone, Debug, Default)]
pub(crate) struct TreeNode {
    pub entry: Option<IndexCopy>,
    pub children: Vec<TreeNode>,
}

/// The capacity `push_back` leaves a vector of `n` elements with: doubled
/// from 1 (libc++ and libstdc++ alike).
fn grown(n: usize) -> u64 {
    if n == 0 {
        0
    } else {
        n.next_power_of_two() as u64
    }
}

/// `BSONObj::objsize()`; an absent object is the empty `BSONObj` (5).
/// `None` for a document that does not encode: no size is given then.
fn objsize(d: Option<&Document>) -> Option<u64> {
    let Some(d) = d else { return Some(5) };
    crate::bsondoc::to_vec(d).ok().map(|b| b.len() as u64)
}

/// `IndexEntry::estimateObjectSizeInBytes`: `multikeyPaths` by element,
/// the identifier's two strings (the disambiguator is empty), the key
/// pattern; `infoObj` is shared with the catalog and left out.
fn index_entry(l: &Layout, ie: &IndexCopy) -> Option<u64> {
    let paths: u64 = ie.multikey.len() as u64 * l.multikey_components
        + ie.multikey.iter().map(|&n| n as u64).sum::<u64>();
    Some(
        paths
            + (l.string_capacity)(ie.name.len())
            + (l.string_capacity)(0)
            + objsize(Some(&ie.key))?
            + l.index_entry,
    )
}

/// `PlanCacheIndexTree::estimateObjectSizeInBytes`.
fn index_tree(l: &Layout, node: &TreeNode) -> Option<u64> {
    let mut size = grown(node.children.len()) * 8;
    for c in &node.children {
        size += index_tree(l, c)?;
    }
    if let Some(e) = &node.entry {
        size += index_entry(l, e)?;
    }
    Some(size + l.index_tree)
}

/// `PlanStageStats::estimateObjectSizeInBytes`, over a stage as
/// `creationExecStats` prints it. `None` for a stage whose stats this does
/// not size.
fn stage(l: &Layout, s: &Document) -> Option<u64> {
    let mut children: Vec<&Document> = Vec::new();
    if let Ok(input) = s.get_document("inputStage") {
        children.push(input);
    }
    if let Ok(inputs) = s.get_array("inputStages") {
        for i in inputs {
            children.push(i.as_document()?);
        }
    }
    let mut size = 0;
    for c in &children {
        size += stage(l, c)?;
    }
    size += grown(children.len()) * 8;
    size += objsize(s.get_document("filter").ok())?;
    let specific = match s.get_str("stage").ok()? {
        "IXSCAN" => {
            let paths = s.get_document("multiKeyPaths").ok();
            let components: u64 = paths
                .map(|p| {
                    p.iter()
                        .map(|(_, v)| match v {
                            Bson::Array(a) => a.len() as u64,
                            _ => 0,
                        })
                        .sum()
                })
                .unwrap_or(0);
            paths.map_or(0, |p| p.len() as u64) * l.multikey_components
                + components
                + objsize(s.get_document("keyPattern").ok())?
                + objsize(s.get_document("collation").ok())?
                + objsize(s.get_document("indexBounds").ok())?
                + (l.string_capacity)(s.get_str("indexName").ok()?.len())
                // `indexType`: empty for a btree index.
                + (l.string_capacity)(0)
                + l.index_scan_stats
        }
        "FETCH" => l.fetch_stats,
        "SORT" => objsize(s.get_document("sortPattern").ok())? + l.sort_stats,
        "SORT_MERGE" => objsize(s.get_document("sortPattern").ok())? + l.merge_sort_stats,
        "LIMIT" => l.limit_stats,
        "SKIP" => l.skip_stats,
        "PROJECTION_SIMPLE" | "PROJECTION_DEFAULT" | "PROJECTION_COVERED" => {
            objsize(s.get_document("transformBy").ok())? + l.projection_stats
        }
        // `failedAnd`, sized to the children.
        "AND_SORTED" => children.len() as u64 * 8 + l.and_sorted_stats,
        "OR" => l.or_stats,
        _ => return None,
    };
    Some(size + specific + l.stage_stats)
}

/// The entry's `estimatedSizeBytes`. `created_from` is its
/// `createdFromQuery`, `creation` its `creationExecStats`, best first, and
/// `tree` its `PlanCacheIndexTree`. `None` when a stage is one this does
/// not size.
pub(crate) fn estimate(
    l: &Layout,
    created_from: &Document,
    creation: &[Document],
    tree: &TreeNode,
) -> Option<u64> {
    let mut created = 0;
    for f in ["query", "sort", "projection", "collation", "distinct"] {
        created += objsize(created_from.get_document(f).ok())?;
    }
    let mut stats = 0;
    for c in creation {
        stats += stage(l, c.get_document("executionStages").ok()?)?;
    }
    // `candidatePlanStats`, `scores` and `candidateOrder` grow one by one;
    // no candidate failed, so `failedCandidates` holds nothing.
    let decision = stats + grown(creation.len()) * 8 * 3 + l.ranking_decision;
    Some(l.entry + l.solution_cache_data + index_tree(l, tree)? + created + decision)
}

/// The `PlanCacheIndexTree` mongod builds from the match expression
/// `canonical` (its `parsedQuery`) when its plan assigned the clauses
/// `tagged` to `index` (`QueryPlanner::cacheDataFromTaggedTree`, after
/// `tagForSort`). `None` for an expression whose tree this does not build:
/// an `$or` or `$nor` that is not tagged as [`ClauseTag::Or`] (an `OR` may
/// take an `OrPushdownTag` of its own), a `$not` of several predicates, `$jsonSchema`.
pub(crate) fn tagged_tree(
    canonical: &Document,
    clauses: &[Document],
    tags: &[ClauseTag],
) -> Option<TreeNode> {
    let mut children = Vec::with_capacity(clauses.len());
    for (i, clause) in clauses.iter().enumerate() {
        children.push(match tags.get(i).unwrap_or(&ClauseTag::None) {
            ClauseTag::None => clause_node(clause, None)?,
            ClauseTag::One(ix) => clause_node(clause, Some(ix))?,
            ClauseTag::Or(branches) => or_node(clause, branches)?,
        });
    }
    let rooted_and = canonical.len() == 1 && canonical.contains_key("$and");
    if !rooted_and {
        let [only] = <[TreeNode; 1]>::try_from(children).ok()?;
        return Some(only);
    }
    Some(sort_tagged(TreeNode {
        entry: None,
        children,
    }))
}

/// One clause of the conjunction: a predicate (no children), or a `$not`
/// over one. A tagged clause's predicate carries the index.
fn clause_node(clause: &Document, tag: Option<&IndexCopy>) -> Option<TreeNode> {
    let mut fields = clause.iter();
    let (path, value) = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    let leaf = || TreeNode {
        entry: tag.cloned(),
        children: vec![],
    };
    if path.starts_with('$') {
        return match path.as_str() {
            "$expr" | "$where" | "$alwaysTrue" | "$alwaysFalse" | "$text" | "$sampleRate" => {
                Some(leaf())
            }
            _ => None,
        };
    }
    let Bson::Document(pred) = value else {
        return Some(leaf());
    };
    let ops: Vec<&str> = pred.keys().map(String::as_str).collect();
    match ops.as_slice() {
        // An `ELEM_MATCH_VALUE` holds one node per operator; a `$not`
        // inside is a `NOT` over its predicate. Nothing below it carries a
        // tag: when the plan's index read it, mongod tags the
        // `ELEM_MATCH_VALUE` itself (`Indexability::arrayUsesIndexOnOwnField`;
        // measured 8.2.11: two copies of the index entry for `{a: {$lt: 5},
        // b: {$elemMatch: {$not: {$gt: 3}}}}` read from `b_1`, by the
        // name's length). The object form is an `ELEM_MATCH_OBJECT`
        // ([`elem_match_object`]).
        ["$elemMatch"] => {
            let inner = pred.get_document("$elemMatch").ok()?;
            if inner.keys().next().is_some_and(|k| !k.starts_with('$')) {
                return elem_match_object(inner, tag);
            }
            let mut children = Vec::with_capacity(inner.len());
            for (op, v) in inner {
                if !op.starts_with('$') {
                    return None;
                }
                children.push(clause_node(&bson::doc! { path: { op: v.clone() } }, None)?);
            }
            Some(TreeNode {
                entry: tag.cloned(),
                children,
            })
        }
        ["$not"] => {
            let one = match pred.get("$not")? {
                Bson::Document(d) => d.len() == 1 && !d.contains_key("$elemMatch"),
                Bson::RegularExpression(_) => true,
                _ => false,
            };
            if !one {
                return None;
            }
            Some(sort_tagged(TreeNode {
                entry: None,
                children: vec![leaf()],
            }))
        }
        [_] | ["$regex", "$options"] => Some(leaf()),
        _ => None,
    }
}

/// An `ELEM_MATCH_OBJECT` over `{path: {$elemMatch: inner}}`: one child,
/// the inner predicate on its own relative path, which carries the tag
/// when the plan read it from the index on `path.<field>`; `tagForSort`
/// then tags the `ELEM_MATCH_OBJECT` with it too (measured 8.2.11: 3230
/// for `{a: {$lt: 5}, d: {$elemMatch: {e: 3}}}` over `a_1`, `b_1` and
/// `d.e_1` on macOS arm64). An inner conjunction of several predicates is
/// left out: no measured figure says how its `AND` is tagged.
fn elem_match_object(inner: &Document, tag: Option<&IndexCopy>) -> Option<TreeNode> {
    if inner.len() != 1 {
        return None;
    }
    let child = clause_node(inner, tag)?;
    Some(sort_tagged(TreeNode {
        entry: None,
        children: vec![child],
    }))
}

/// An `$or` whose branches are single predicates, each tagged with the
/// index the plan read it from; the `OR` takes the lowest-numbered.
fn or_node(clause: &Document, branches: &[IndexCopy]) -> Option<TreeNode> {
    let list = clause.get_array("$or").ok()?;
    if clause.len() != 1 || list.len() != branches.len() {
        return None;
    }
    let mut children = Vec::with_capacity(list.len());
    for (b, ix) in list.iter().zip(branches) {
        // A branch of several predicates is an `AND` over them, the one on
        // the index's leading field tagged; `tagForSort` tags the `AND`.
        let preds = super::plan::clauses(b.as_document()?);
        let lead = ix.key.keys().next()?;
        let mut nodes = Vec::with_capacity(preds.len());
        for p in &preds {
            if p.len() != 1 || p.keys().any(|k| k.starts_with('$')) {
                return None;
            }
            let tag = p.contains_key(lead).then_some(ix);
            nodes.push(clause_node(p, tag)?);
        }
        if nodes.len() == 1 {
            children.extend(nodes);
        } else {
            children.push(sort_tagged(TreeNode {
                entry: None,
                children: nodes,
            }));
        }
    }
    Some(sort_tagged(TreeNode {
        entry: None,
        children,
    }))
}

/// `tagForSort` for one internal node whose children are tagged already:
/// it takes the tag of its lowest-numbered index.
fn sort_tagged(mut node: TreeNode) -> TreeNode {
    node.entry = node
        .children
        .iter()
        .filter_map(|c| c.entry.as_ref())
        .min_by_key(|e| e.number)
        .cloned();
    node
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn ixscan(name: &str, key: Document, bounds: Document) -> Document {
        let paths: Document = key
            .keys()
            .map(|k| (k.clone(), Bson::Array(vec![])))
            .collect();
        doc! {
            "stage": "IXSCAN",
            "keyPattern": key,
            "indexName": name,
            "multiKeyPaths": paths,
            "indexBounds": bounds,
        }
    }

    fn fetch(filter: Option<Document>, input: Document) -> Document {
        let mut d = doc! { "stage": "FETCH" };
        if let Some(f) = filter {
            d.insert("filter", f);
        }
        d.insert("inputStage", input);
        d
    }

    fn a_1(name: &str) -> IndexCopy {
        IndexCopy {
            name: name.into(),
            key: doc! { "a": 1 },
            multikey: vec![0],
            number: 1,
        }
    }

    /// `{a: {$lt: 5}, b: {$lt: 90}}` over `a_1` and `b_1`, two candidates.
    fn base(name: &str) -> u64 {
        let created = doc! {
            "query": { "a": { "$lt": 5 }, "b": { "$lt": 90 } },
            "sort": {},
            "projection": {},
        };
        let creation = vec![
            doc! { "executionStages": fetch(
                Some(doc! { "b": { "$lt": 90 } }),
                ixscan(name, doc! { "a": 1 }, doc! { "a": ["[-inf, 5)"] }),
            ) },
            doc! { "executionStages": fetch(
                Some(doc! { "a": { "$lt": 5 } }),
                ixscan("b_1", doc! { "b": 1 }, doc! { "b": ["[-inf, 90)"] }),
            ) },
        ];
        let canonical = doc! { "$and": [
            { "a": { "$lt": 5 } }, { "b": { "$lt": 90 } },
        ] };
        let clauses = vec![doc! { "a": { "$lt": 5 } }, doc! { "b": { "$lt": 90 } }];
        let tags = [ClauseTag::One(a_1(name)), ClauseTag::None];
        let tree = tagged_tree(&canonical, &clauses, &tags).unwrap();
        estimate(&LIBCXX_ARM64, &created, &creation, &tree).unwrap()
    }

    #[test]
    fn mongods_figures_on_macos_arm64() {
        // mongod 8.2.11, macOS arm64 (2026-11-08): 3040, and the winning
        // index renamed to 22, 23, 24, 32 and 40 characters.
        assert_eq!(base("a_1"), 3040);
        assert_eq!(base(&"x".repeat(22)), 3040);
        assert_eq!(base(&"x".repeat(23)), 3046);
        assert_eq!(base(&"x".repeat(24)), 3067);
        assert_eq!(base(&"x".repeat(32)), 3091);
        assert_eq!(base(&"x".repeat(40)), 3115);
    }

    #[test]
    fn a_single_predicate_is_the_tree_root() {
        // mongod 8.2.11: `{a: {$lt: 5}}` over `a_1` and `a_1_c_1`, 2483.
        let created = doc! { "query": { "a": { "$lt": 5 } }, "sort": {}, "projection": {} };
        let creation = vec![
            doc! { "executionStages": fetch(None, ixscan("a_1", doc! { "a": 1 }, doc! { "a": ["[-inf, 5)"] })) },
            doc! { "executionStages": fetch(None, ixscan(
                "a_1_c_1",
                doc! { "a": 1, "c": 1 },
                doc! { "a": ["[-inf, 5)"], "c": ["[MinKey, MaxKey]"] },
            )) },
        ];
        let canonical = doc! { "a": { "$lt": 5 } };
        let clauses = vec![canonical.clone()];
        let tree = tagged_tree(&canonical, &clauses, &[ClauseTag::One(a_1("a_1"))]).unwrap();
        assert_eq!(
            estimate(&LIBCXX_ARM64, &created, &creation, &tree),
            Some(2483)
        );
    }

    #[test]
    fn trees_it_does_not_build() {
        for clause in [
            doc! { "$or": [{ "a": 1 }, { "b": 2 }] },
            doc! { "a": { "$not": { "$gt": 1, "$lt": 5 } } },
            doc! { "$jsonSchema": {} },
        ] {
            assert!(tagged_tree(&clause, std::slice::from_ref(&clause), &[]).is_none());
        }
        // The object form of `$elemMatch` over several predicates is not built.
        let clause = doc! { "a": { "$elemMatch": { "b": 1, "c": 2 } } };
        assert!(tagged_tree(&clause, std::slice::from_ref(&clause), &[]).is_none());
    }

    #[test]
    fn an_object_elem_match_tags_its_predicate_and_itself() {
        // `{d: {$elemMatch: {e: 3}}}` read from `d.e_1`: the
        // `ELEM_MATCH_OBJECT` and its one predicate both carry the tag.
        let clause = doc! { "d": { "$elemMatch": { "e": 3 } } };
        let tags = [ClauseTag::One(a_1("d.e_1"))];
        let tree = tagged_tree(&clause, std::slice::from_ref(&clause), &tags).unwrap();
        assert_eq!(tree.entry.as_ref().unwrap().name, "d.e_1");
        assert_eq!(tree.children.len(), 1);
        assert_eq!(tree.children[0].entry.as_ref().unwrap().name, "d.e_1");
    }

    #[test]
    fn a_tagged_elem_match_holds_the_tag_itself() {
        // `{a: {$elemMatch: {$not: {$gt: 3}}}}` read from the index: the
        // `ELEM_MATCH_VALUE` carries the tag, the `NOT` and `$gt` none.
        let clause = doc! { "a": { "$elemMatch": { "$not": { "$gt": 3 } } } };
        let tags = [ClauseTag::One(a_1("a_1"))];
        let tree = tagged_tree(&clause, std::slice::from_ref(&clause), &tags).unwrap();
        assert_eq!(tree.entry.as_ref().unwrap().name, "a_1");
        let not = &tree.children[0];
        assert!(not.entry.is_none());
        assert!(not.children[0].entry.is_none());
    }

    #[test]
    fn elem_match_and_or_trees() {
        let clauses = [
            doc! { "a": { "$lt": 5 } },
            doc! { "b": { "$elemMatch": { "$gt": 1, "$lt": 90 } } },
        ];
        let canonical = doc! { "$and": clauses.to_vec() };
        let tree = tagged_tree(&canonical, &clauses, &[ClauseTag::One(a_1("a_1"))]).unwrap();
        assert_eq!(tree.children[1].children.len(), 2);
        assert!(tree.children[1].entry.is_none());
        let mut b_1 = a_1("b_1");
        b_1.number = 2;
        let clauses = [
            doc! { "$or": [{ "b": 5 }, { "a": 3 }] },
            doc! { "c": { "$lt": 3 } },
        ];
        let canonical = doc! { "$and": clauses.to_vec() };
        let tags = [ClauseTag::Or(vec![b_1, a_1("a_1")]), ClauseTag::None];
        let tree = tagged_tree(&canonical, &clauses, &tags).unwrap();
        // `tagForSort`: the lowest-numbered index below, at every level.
        assert_eq!(tree.entry.unwrap().name, "a_1");
        assert_eq!(tree.children[0].entry.as_ref().unwrap().name, "a_1");
    }

    #[test]
    fn an_or_branch_of_several_predicates_is_a_tagged_and() {
        // `{$or: [{a: 3, c: 2}, {b: 5}], c: {$lt: 3}}`, the first branch
        // read from `a_1`: its `AND` holds `a` tagged and `c` untagged.
        let mut b_1 = a_1("b_1");
        b_1.key = doc! { "b": 1 };
        b_1.number = 2;
        let clauses = [
            doc! { "$or": [{ "$and": [{ "a": 3 }, { "c": 2 }] }, { "b": 5 }] },
            doc! { "c": { "$lt": 3 } },
        ];
        let canonical = doc! { "$and": clauses.to_vec() };
        let tags = [ClauseTag::Or(vec![a_1("a_1"), b_1]), ClauseTag::None];
        let tree = tagged_tree(&canonical, &clauses, &tags).unwrap();
        let and = &tree.children[0].children[0];
        assert_eq!(and.entry.as_ref().unwrap().name, "a_1");
        assert_eq!(and.children[0].entry.as_ref().unwrap().name, "a_1");
        assert!(and.children[1].entry.is_none());
    }

    #[test]
    fn string_capacity() {
        let cap = LIBCXX_ARM64.string_capacity;
        assert_eq!(
            [0, 22, 23, 24, 31, 32, 39, 40].map(cap),
            [22, 22, 24, 31, 31, 39, 39, 47]
        );
    }
}
