//! The storage router reads an index for the shapes mongod's plan does
//! (`tasks/backlog-rust-mongodb.md` RM-069): `$ne`, `$nin`, `$not`, `$exists`,
//! `$type`, `$mod`, `$all`, both forms of `$elemMatch`, an equality to a
//! document, a regex, a range on `_id` and an explicit `$and` -- and never
//! loses a document doing it. Over real WiredTiger.
//!
//! Every answer here is checked two ways: against the same documents in a
//! collection with no index (an index must never change the answer), and,
//! for the traps, against the `_id`s mongod 8.2.11 returned for the same
//! documents and filter (measured 2026-10-15).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use bson::{doc, Bson, Document, Regex};
use secantus_storage::{RouterAccess, Storage};

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct Store {
    wt: Option<Storage>,
    dir: PathBuf,
}

impl Store {
    fn wt(&self) -> &Storage {
        self.wt.as_ref().unwrap()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // The connection closes before its directory goes.
        drop(self.wt.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn store() -> Store {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-router-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Store {
        wt: Some(Storage::open(dir.to_str().unwrap()).unwrap()),
        dir,
    }
}

fn load(s: &Store, coll: &str, docs: &[Document]) {
    let blobs: Vec<Vec<u8>> = docs.iter().map(|d| bson::to_vec(d).unwrap()).collect();
    let (n, errors) = s.wt().insert("t", coll, blobs, true).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(n, docs.len());
}

fn index(s: &Store, coll: &str, name: &str, key: Document, options: Document) {
    s.wt()
        .create_index("t", coll, name, &key, &options)
        .unwrap();
}

/// The `_id`s `filter` matches in `coll`, in the order the store gives.
fn ids(s: &Store, coll: &str, filter: &Document) -> Vec<Bson> {
    let rows = s
        .wt()
        .matching_rows("t", coll, filter, None, None, &Document::new())
        .unwrap()
        .expect("a plain collection gives its matched rows");
    rows.rows
        .iter()
        .map(|(_, blob)| {
            secantus_commands::bsondoc::from_bytes(blob)
                .unwrap()
                .get("_id")
                .cloned()
                .unwrap()
        })
        .collect()
}

fn sorted(mut v: Vec<Bson>) -> Vec<i32> {
    v.sort_by_key(|b| b.as_i32().unwrap());
    v.into_iter().map(|b| b.as_i32().unwrap()).collect()
}

/// `docs` in `plain` (no index) and `ix` (with one): the same answer for
/// every filter, and that answer.
fn twin(s: &Store, docs: &[Document], key: Document, options: Document) {
    load(s, "plain", docs);
    load(s, "ix", docs);
    index(s, "ix", "the_index", key, options);
}

fn same(s: &Store, filter: Document) -> Vec<i32> {
    let with = sorted(ids(s, "ix", &filter));
    let without = sorted(ids(s, "plain", &filter));
    assert_eq!(with, without, "the index changed the answer to {filter}");
    with
}

fn access(s: &Store, coll: &str, filter: Document) -> RouterAccess {
    s.wt().router_access("t", coll, &filter).unwrap()
}

fn ordered(name: &str) -> RouterAccess {
    RouterAccess::Index {
        name: name.into(),
        in_order: true,
    }
}

fn unordered(name: &str) -> RouterAccess {
    RouterAccess::Index {
        name: name.into(),
        in_order: false,
    }
}

fn scalar_docs() -> Vec<Document> {
    vec![
        doc! {"_id": 0, "a": 5},
        doc! {"_id": 1, "a": 6},
        doc! {"_id": 2, "a": 2},
        doc! {"_id": 3, "a": Bson::Null},
        doc! {"_id": 4},
        doc! {"_id": 5, "a": f64::NAN},
        doc! {"_id": 6, "a": "s"},
        doc! {"_id": 7, "a": "sa"},
        doc! {"_id": 8, "a": {"k": 1}},
        doc! {"_id": 12, "a": 5.0},
        doc! {"_id": 13, "a": true},
    ]
}

/// `scalar_docs` and three numbers outside the encoder's exponent range.
fn collapsed_docs() -> Vec<Document> {
    let mut docs = scalar_docs();
    docs.push(doc! {"_id": 9, "a": 1e-300});
    docs.push(doc! {"_id": 10, "a": -1e-300});
    docs.push(doc! {"_id": 11, "a": 3e250});
    docs
}

#[test]
fn a_negation_reads_the_whole_index_in_key_order() {
    let s = store();
    twin(&s, &scalar_docs(), doc! {"a": 1}, doc! {});
    // mongod 8.2.11: everything but the two fives, missing and null included.
    assert_eq!(
        same(&s, doc! {"a": {"$ne": 5}}),
        [1, 2, 3, 4, 5, 6, 7, 8, 13]
    );
    assert_eq!(
        access(&s, "ix", doc! {"a": {"$ne": 5}}),
        ordered("the_index")
    );
    assert_eq!(
        same(&s, doc! {"a": {"$nin": [5, "s"]}}),
        [1, 2, 3, 4, 5, 7, 8, 13]
    );
    assert_eq!(
        same(&s, doc! {"a": {"$not": {"$eq": 6}}}),
        [0, 2, 3, 4, 5, 6, 7, 8, 12, 13]
    );
    // In the index's order, which is mongod's: null and missing, NaN, the
    // numbers (equal ones by insertion), the strings, the document, true.
    let order: Vec<i32> = ids(&s, "ix", &doc! {"a": {"$ne": 6}})
        .iter()
        .map(|b| b.as_i32().unwrap())
        .collect();
    assert_eq!(order, [3, 4, 5, 2, 0, 12, 6, 7, 8, 13]);
    assert_eq!(same(&s, doc! {"a": {"$mod": [2, 0]}}), [1, 2]);
    assert_eq!(
        access(&s, "ix", doc! {"a": {"$mod": [2, 0]}}),
        ordered("the_index")
    );
}

#[test]
fn not_of_a_range_keeps_the_bound_nan_null_and_missing() {
    let s = store();
    twin(&s, &collapsed_docs(), doc! {"a": 1}, doc! {});
    // mongod: 2, null, missing, NaN, the tiny ones, and every non-number.
    assert_eq!(
        same(&s, doc! {"a": {"$not": {"$gt": 2}}}),
        [2, 3, 4, 5, 6, 7, 8, 9, 10, 13]
    );
    assert_eq!(
        same(&s, doc! {"a": {"$not": {"$lt": "sa"}}}),
        [0, 1, 2, 3, 4, 5, 7, 8, 9, 10, 11, 12, 13]
    );
    assert_eq!(
        access(&s, "ix", doc! {"a": {"$not": {"$gt": 2}}}),
        unordered("the_index"),
        "a collapsed number was read, out of its value's place"
    );
}

#[test]
fn a_range_reads_the_numbers_the_encoder_collapses() {
    // Numbers whose exponent is outside -128..=127 share one key per sign,
    // which sorts above every positive (below every negative). A range scan
    // used to skip them: with an index, `{a: {$lt: 1}}` lost 1e-300 and
    // `{a: {$gt: 1e200}}` lost 3e250 (mongod returns both).
    let s = store();
    twin(&s, &collapsed_docs(), doc! {"a": 1}, doc! {});
    assert_eq!(same(&s, doc! {"a": {"$lt": 1}}), [9, 10]);
    assert_eq!(same(&s, doc! {"a": {"$gt": -1, "$lt": 0}}), [10]);
    assert_eq!(same(&s, doc! {"a": {"$gt": 1e200}}), [11]);
    assert_eq!(
        same(&s, doc! {"a": {"$type": "number"}}),
        [0, 1, 2, 5, 9, 10, 11, 12]
    );
}

#[test]
fn a_descending_index_reads_a_string_range_whole() {
    // Inverted bytes do not reverse a prefix: over `{b: -1}`, `{b: {$gt: ""}}`
    // returned NOTHING and `{b: {$gt: "s"}}` lost "sa" and "sab".
    let s = store();
    let docs: Vec<Document> = ["s", "sa", "sab", "t", "r", "", "a"]
        .iter()
        .enumerate()
        .map(|(i, v)| doc! {"_id": i as i32, "b": *v})
        .collect();
    twin(&s, &docs, doc! {"b": -1}, doc! {});
    assert_eq!(same(&s, doc! {"b": {"$gt": ""}}), [0, 1, 2, 3, 4, 6]);
    assert_eq!(same(&s, doc! {"b": {"$gt": "s"}}), [1, 2, 3]);
    assert_eq!(same(&s, doc! {"b": {"$gt": "s", "$lt": "t"}}), [1, 2]);
    assert_eq!(same(&s, doc! {"b": {"$lte": "sa"}}), [0, 1, 4, 5, 6]);
    assert_eq!(same(&s, doc! {"b": {"$regex": "^s"}}), [0, 1, 2]);
    // Their order is not the inverted bytes': not claimed.
    assert_eq!(
        access(&s, "ix", doc! {"b": {"$gt": "s"}}),
        unordered("the_index")
    );
}

#[test]
fn a_regex_is_a_pattern_not_a_key() {
    // `{a: /^s/}` over an index on `a` read only the entries holding that
    // regex, and found nothing.
    let s = store();
    let mut docs = scalar_docs();
    docs.push(doc! {"_id": 14, "a": Regex { pattern: "^s".into(), options: String::new() }});
    docs.push(doc! {"_id": 15, "a": "x s"});
    twin(&s, &docs, doc! {"a": 1, "c": 1}, doc! {});
    let re = |p: &str| {
        Bson::RegularExpression(Regex {
            pattern: p.into(),
            options: String::new(),
        })
    };
    assert_eq!(same(&s, doc! {"a": re("^s")}), [6, 7, 14]);
    assert_eq!(same(&s, doc! {"a": {"$regex": "^s"}}), [6, 7, 14]);
    assert_eq!(same(&s, doc! {"a": {"$regex": "s$"}}), [6, 15]);
    // Alternation: no prefix bounds it.
    assert_eq!(same(&s, doc! {"a": {"$regex": "^q|x"}}), [15]);
    assert_eq!(same(&s, doc! {"a": {"$in": [re("^sa"), 6]}}), [1, 7]);
    assert_eq!(
        same(&s, doc! {"a": re("^s"), "c": {"$exists": false}}),
        [6, 7, 14]
    );
    assert_eq!(
        access(&s, "ix", doc! {"a": {"$regex": "^s"}}),
        ordered("the_index")
    );
}

fn array_docs() -> Vec<Document> {
    vec![
        doc! {"_id": 0, "a": [5, 6]},
        doc! {"_id": 1, "a": [6, 7]},
        doc! {"_id": 2, "a": []},
        doc! {"_id": 3, "a": [[5]]},
        doc! {"_id": 4, "a": 5},
        doc! {"_id": 5},
        doc! {"_id": 6, "a": [Bson::Null]},
        doc! {"_id": 7, "a": [{"k": 2}, {"k": 3}]},
        doc! {"_id": 8, "a": [3, 5]},
    ]
}

#[test]
fn a_multikey_index_keeps_the_fetch_filter() {
    let s = store();
    twin(&s, &array_docs(), doc! {"a": 1}, doc! {});
    // `$ne: 5` over [5, 6] must not match: mongod reads it through 6 and
    // its filter drops it. An empty array, missing and [[5]] do match.
    assert_eq!(same(&s, doc! {"a": {"$ne": 5}}), [1, 2, 3, 5, 6, 7]);
    assert_eq!(same(&s, doc! {"a": {"$all": [5]}}), [0, 4, 8]);
    assert_eq!(same(&s, doc! {"a": {"$all": [5, 6]}}), [0]);
    assert_eq!(
        same(&s, doc! {"a": {"$elemMatch": {"$gt": 4, "$lt": 6}}}),
        [0, 8]
    );
    assert_eq!(same(&s, doc! {"a": {"$elemMatch": {"$eq": 5}}}), [0, 8]);
    assert_eq!(
        same(&s, doc! {"a": {"$exists": true}}),
        [0, 1, 2, 3, 4, 6, 7, 8]
    );
    assert_eq!(same(&s, doc! {"a": {"$exists": false}}), [5]);
    assert_eq!(
        same(&s, doc! {"a": {"$type": "array"}}),
        [0, 1, 2, 3, 6, 7, 8]
    );
    assert_eq!(
        same(&s, doc! {"a": {"$not": {"$gt": 5}}}),
        [2, 3, 4, 5, 6, 7, 8]
    );
    assert_eq!(same(&s, doc! {"a": {"k": 2}}), [7]);
    // A nested `$elemMatch` tests an element that is an array: [[5]] is
    // keyed by [5], not by 5.
    assert_eq!(
        same(&s, doc! {"a": {"$elemMatch": {"$elemMatch": {"$gt": 4}}}}),
        [3]
    );
    // Points come out at a document's first key, as mongod's scan does;
    // anything wider is not claimed.
    assert_eq!(
        access(&s, "ix", doc! {"a": {"$all": [5]}}),
        ordered("the_index")
    );
    assert_eq!(
        access(&s, "ix", doc! {"a": {"$ne": 5}}),
        unordered("the_index")
    );
}

#[test]
fn a_sparse_or_unimplied_partial_index_is_not_read_for_a_missing_value() {
    let s = store();
    twin(&s, &scalar_docs(), doc! {"a": 1}, doc! {"sparse": true});
    for filter in [
        doc! {"a": {"$ne": 5}},
        doc! {"a": {"$nin": [5]}},
        doc! {"a": {"$not": {"$gt": 2}}},
        doc! {"a": {"$exists": false}},
    ] {
        assert_eq!(access(&s, "ix", filter.clone()), RouterAccess::Collection);
        assert!(same(&s, filter).contains(&4));
    }
    // `$all: [null]` is an equality to null, which a missing field meets
    // (mongod returns `_id` 4). The matcher does not yet (backlog); the
    // sparse index is kept out of it all the same.
    let all_null = doc! {"a": {"$all": [Bson::Null]}};
    assert_eq!(access(&s, "ix", all_null.clone()), RouterAccess::Collection);
    same(&s, all_null);
    // A predicate that needs the field present may read it.
    assert_eq!(
        same(&s, doc! {"a": {"$exists": true}}),
        [0, 1, 2, 3, 5, 6, 7, 8, 12, 13]
    );
    assert_eq!(
        access(&s, "ix", doc! {"a": {"$type": "string"}}),
        ordered("the_index")
    );

    let p = store();
    let docs: Vec<Document> = scalar_docs()
        .into_iter()
        .enumerate()
        .map(|(i, mut d)| {
            d.insert("c", (i % 3) as i32);
            d
        })
        .collect();
    twin(
        &p,
        &docs,
        doc! {"a": 1},
        doc! {"partialFilterExpression": {"c": {"$gte": 1}}},
    );
    assert_eq!(
        access(&p, "ix", doc! {"a": {"$exists": true}}),
        RouterAccess::Collection
    );
    assert_eq!(same(&p, doc! {"a": {"$exists": true}}).len(), 10);
    same(&p, doc! {"a": {"$ne": 5}, "c": {"$gte": 1}});
    same(&p, doc! {"a": {"$type": "number"}, "c": 2});
}

#[test]
fn elem_match_on_documents_reads_the_dotted_index() {
    let s = store();
    let docs = vec![
        doc! {"_id": 0, "n": [{"k": 1}, {"j": 1}]},
        doc! {"_id": 1, "n": [{"j": 1}]},
        doc! {"_id": 2, "n": [{"k": 2}, {"k": 3}]},
        doc! {"_id": 3, "n": {"k": 2}},
        doc! {"_id": 4, "n": [{"k": Bson::Null}]},
        doc! {"_id": 5, "c": 1},
    ];
    twin(&s, &docs, doc! {"n.k": 1}, doc! {});
    assert_eq!(same(&s, doc! {"n": {"$elemMatch": {"k": 2}}}), [2]);
    assert_eq!(same(&s, doc! {"n": {"$elemMatch": {"k": {"$gt": 1}}}}), [2]);
    assert_eq!(
        access(&s, "ix", doc! {"n": {"$elemMatch": {"k": 2}}}),
        ordered("the_index")
    );
    // An element WITHOUT `k` meets `{k: null}`: the index holds no entry
    // for it, so it is not read.
    assert_eq!(
        same(&s, doc! {"n": {"$elemMatch": {"k": Bson::Null}}}),
        [0, 1, 4]
    );
    assert_eq!(
        access(&s, "ix", doc! {"n": {"$elemMatch": {"k": Bson::Null}}}),
        RouterAccess::Collection
    );
    same(&s, doc! {"n": {"$elemMatch": {"k": {"$exists": false}}}});
}

#[test]
fn an_id_range_reads_the_id_index() {
    let s = store();
    let mut docs: Vec<Document> = (0..20).map(|i| doc! {"_id": i, "x": i % 3}).collect();
    docs.push(doc! {"_id": "s", "x": 1});
    docs.push(doc! {"_id": {"k": 1}, "x": 1});
    load(&s, "ix", &docs);
    let got = |f: Document| -> Vec<Bson> { ids(&s, "ix", &f) };
    let ints = |v: Vec<Bson>| -> Vec<i32> { v.iter().filter_map(|b| b.as_i32()).collect() };
    assert_eq!(ints(got(doc! {"_id": {"$gt": 15}})), [16, 17, 18, 19]);
    assert_eq!(ints(got(doc! {"_id": {"$gte": 3, "$lt": 6}})), [3, 4, 5]);
    assert_eq!(access(&s, "ix", doc! {"_id": {"$gt": 15}}), ordered("_id_"));
    // A document `_id` is keyed by its raw BSON: its bracket is read whole,
    // and not claimed to be in order.
    assert_eq!(
        got(doc! {"_id": {"$gte": {"k": 0}}}),
        [Bson::Document(doc! {"k": 1})]
    );
    assert_eq!(
        access(&s, "ix", doc! {"_id": {"$gte": {"k": 0}}}),
        unordered("_id_")
    );
    assert_eq!(
        got(doc! {"_id": {"$type": "string"}}),
        [Bson::String("s".into())]
    );
    assert_eq!(got(doc! {"_id": {"$ne": 4}}).len(), docs.len() - 1);
    // 1e-300 is collapsed: read, and the order not claimed.
    load(&s, "ix", &[doc! {"_id": 1e-300, "x": 1}]);
    let lt = got(doc! {"_id": {"$lt": 2}});
    assert!(lt.contains(&Bson::Double(1e-300)));
    assert_eq!(lt.len(), 3);
    assert_eq!(
        access(&s, "ix", doc! {"_id": {"$lt": 2}}),
        unordered("_id_")
    );
}

#[test]
fn an_explicit_and_routes_like_the_filter_it_spells() {
    let s = store();
    let docs: Vec<Document> = (0..30)
        .map(|i| doc! {"_id": i, "a": i % 10, "c": i % 3})
        .collect();
    twin(&s, &docs, doc! {"a": 1}, doc! {});
    assert_eq!(
        same(&s, doc! {"$and": [{"a": {"$gt": 6}}, {"a": {"$lt": 8}}]}),
        [7, 17, 27]
    );
    assert_eq!(
        access(
            &s,
            "ix",
            doc! {"$and": [{"a": {"$gt": 6}}, {"a": {"$lt": 8}}]}
        ),
        ordered("the_index")
    );
    assert_eq!(same(&s, doc! {"$and": [{"a": 4}, {"c": 1}]}), [4]);
    assert_eq!(
        access(&s, "ix", doc! {"$and": [{"a": 4}, {"c": 1}]}),
        ordered("the_index")
    );
    assert_eq!(
        same(&s, doc! {"$and": [{"a": 4}, {"a": 5}]}),
        Vec::<i32>::new()
    );
    assert_eq!(
        same(&s, doc! {"c": 2, "$and": [{"a": {"$ne": 5}}]}).len(),
        9
    );
}

#[test]
fn a_narrow_field_is_read_before_a_wide_one() {
    let s = store();
    let docs: Vec<Document> = (0..30)
        .map(|i| doc! {"_id": i, "a": i % 10, "b": i})
        .collect();
    load(&s, "ix", &docs);
    index(&s, "ix", "a_1", doc! {"a": 1}, doc! {});
    index(&s, "ix", "b_1", doc! {"b": 1}, doc! {});
    // The equality on `b`, not the whole of `a_1`. (A field of several is
    // read without claiming the order.)
    assert_eq!(
        access(&s, "ix", doc! {"a": {"$ne": 5}, "b": 7}),
        unordered("b_1")
    );
    // An `$or` takes no wide branch: the collection once is cheaper.
    assert_eq!(
        access(&s, "ix", doc! {"$or": [{"a": {"$ne": 5}}, {"b": 3}]}),
        RouterAccess::Collection
    );
}
