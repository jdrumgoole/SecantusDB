//! Cursors that read on as `getMore` asks (RM-080): each test opens a cursor,
//! takes its first batch, writes, and reads the rest. Every expected answer
//! is mongod 8.2.11's to the same steps (measured 2026-10-27), which
//! `tests/mongod_live/test_streaming_cursors.py` also compares live.

use mongodb::bson::{doc, Bson, Document};
use mongodb::options::{Collation, Hint, IndexOptions};
use mongodb::sync::{Client, Collection, Cursor};
use mongodb::IndexModel;
use secantus_mdb::Server;

fn client(server: &Server) -> Client {
    let uri = format!("{}&serverSelectionTimeoutMS=10000", server.uri());
    Client::with_uri_str(uri).expect("client")
}

fn coll(server: &Server, name: &str) -> Collection<Document> {
    client(server).database("rm080").collection::<Document>(name)
}

fn index(c: &Collection<Document>, keys: Document, options: Option<IndexOptions>) {
    c.create_index(IndexModel::builder().keys(keys).options(options).build())
        .run()
        .expect("createIndexes");
}

/// The first `n` documents, which the cursor's first batch holds.
fn take(cursor: &mut Cursor<Document>, n: usize) -> Vec<Document> {
    (0..n)
        .map(|_| cursor.next().expect("a document").expect("no error"))
        .collect()
}

fn rest(cursor: Cursor<Document>) -> Vec<Document> {
    cursor.map(|d| d.expect("no error")).collect()
}

fn ids(docs: &[Document]) -> Vec<Bson> {
    docs.iter().map(|d| d.get("_id").cloned().unwrap()).collect()
}

fn ints(v: &[i32]) -> Vec<Bson> {
    v.iter().map(|i| Bson::Int32(*i)).collect()
}

fn strings(docs: &[Document]) -> Vec<String> {
    docs.iter()
        .map(|d| d.get_str("s").unwrap().to_string())
        .collect()
}

/// A forward walk of `{s: -1}` meets a string before its prefixes, and
/// sees the writes made ahead of it (entry format 6).
#[test]
fn a_descending_index_of_strings_reads_on() {
    let server = Server::start().expect("start");
    let c = coll(&server, "desc");
    let vals = ["a", "ab", "abc", "b", "ba", "c"];
    c.insert_many(vals.iter().enumerate().map(|(i, s)| doc! {"_id": i as i32, "s": *s}))
        .run()
        .unwrap();
    index(&c, doc! {"s": -1}, None);
    let mut cur = c.find(doc! {"s": {"$gte": ""}}).batch_size(2).run().unwrap();
    let mut got = take(&mut cur, 2);
    c.insert_one(doc! {"_id": 10, "s": "bb"}).run().unwrap();
    c.insert_one(doc! {"_id": 11, "s": "aba"}).run().unwrap();
    c.delete_one(doc! {"s": "ab"}).run().unwrap();
    c.update_one(doc! {"s": "a"}, doc! {"$set": {"s": "bz"}})
        .run()
        .unwrap();
    c.update_one(doc! {"s": "c"}, doc! {"$set": {"s": "a0"}})
        .run()
        .unwrap();
    got.extend(rest(cur));
    assert_eq!(strings(&got), ["c", "ba", "b", "abc", "aba", "a0"]);
}

/// A sort read backward along `{s: -1}`.
#[test]
fn a_descending_index_read_backward_reads_on() {
    let server = Server::start().expect("start");
    let c = coll(&server, "descback");
    let vals = ["a", "ab", "abc", "b", "ba", "c", ""];
    c.insert_many(vals.iter().enumerate().map(|(i, s)| doc! {"_id": i as i32, "s": *s}))
        .run()
        .unwrap();
    index(&c, doc! {"s": -1}, None);
    let mut cur = c.find(doc! {}).sort(doc! {"s": 1}).batch_size(2).run().unwrap();
    let mut got = take(&mut cur, 2);
    c.insert_one(doc! {"_id": 11, "s": "aa"}).run().unwrap();
    c.insert_one(doc! {"_id": 12, "s": ""}).run().unwrap();
    got.extend(rest(cur));
    assert_eq!(strings(&got), ["", "a", "aa", "ab", "abc", "b", "ba", "c"]);
}

/// A multikey scan returns each document once across batches, passes over
/// the whole-array keys this store writes, and keys an empty array by
/// `undefined` (before null) as mongod does.
#[test]
fn a_multikey_index_reads_on() {
    let server = Server::start().expect("start");
    let c = coll(&server, "multikey");
    c.insert_many([
        doc! {"_id": 1, "t": ["a", "z"]},
        doc! {"_id": 2, "t": ["b"]},
        doc! {"_id": 3, "t": ["c", "d"]},
        doc! {"_id": 4, "t": "e"},
        doc! {"_id": 5, "t": ["f", "a"]},
    ])
    .run()
    .unwrap();
    index(&c, doc! {"t": 1}, None);
    let mut cur = c.find(doc! {"t": {"$gte": "a"}}).batch_size(2).run().unwrap();
    let mut got = take(&mut cur, 2);
    c.update_one(doc! {"_id": 2}, doc! {"$push": {"t": "y"}})
        .run()
        .unwrap();
    c.insert_one(doc! {"_id": 6, "t": ["c", "q"]}).run().unwrap();
    c.update_one(doc! {"_id": 4}, doc! {"$set": {"t": ["0"]}})
        .run()
        .unwrap();
    got.extend(rest(cur));
    assert_eq!(ids(&got), ints(&[1, 5, 2, 3, 6]));

    let c = coll(&server, "multikey_all");
    c.insert_many([
        doc! {"_id": 1, "t": []},
        doc! {"_id": 2, "t": [[1], 2]},
        doc! {"_id": 3, "t": [3, [0]]},
        doc! {"_id": 4},
        doc! {"_id": 5, "t": Bson::Null},
        doc! {"_id": 6, "t": [[]]},
    ])
    .run()
    .unwrap();
    index(&c, doc! {"t": 1}, None);
    let mut cur = c.find(doc! {}).hint(Hint::Keys(doc! {"t": 1})).batch_size(1).run().unwrap();
    let mut got = take(&mut cur, 1);
    c.insert_one(doc! {"_id": 7, "t": []}).run().unwrap();
    c.insert_one(doc! {"_id": 8, "t": [[2]]}).run().unwrap();
    got.extend(rest(cur));
    assert_eq!(ids(&got), ints(&[1, 7, 4, 5, 2, 3, 6, 8]));
}

/// An element on a dotted path that lacks the leaf is keyed null.
#[test]
fn a_missing_leaf_is_read_at_null() {
    let server = Server::start().expect("start");
    let c = coll(&server, "leaf");
    c.insert_many([
        doc! {"_id": 1, "a": [{"b": 5}, {"c": 1}]},
        doc! {"_id": 2, "a": [{"b": 1}]},
        doc! {"_id": 3, "a": {"b": 3}},
        doc! {"_id": 4, "a": [{"b": 2}, {"b": 7}]},
        doc! {"_id": 5},
    ])
    .run()
    .unwrap();
    index(&c, doc! {"a.b": 1}, None);
    let mut cur = c
        .find(doc! {})
        .sort(doc! {"a.b": 1})
        .hint(Hint::Keys(doc! {"a.b": 1}))
        .batch_size(1)
        .run()
        .unwrap();
    let mut got = take(&mut cur, 1);
    c.insert_one(doc! {"_id": 6, "a": [{"c": 1}]}).run().unwrap();
    c.insert_one(doc! {"_id": 7, "a": [{"b": 6}]}).run().unwrap();
    got.extend(rest(cur));
    assert_eq!(ids(&got), ints(&[1, 5, 6, 2, 4, 3, 7]));
}

/// A collated index of strings is walked in the collation's order.
#[test]
fn a_collated_index_of_strings_reads_on() {
    let server = Server::start().expect("start");
    let c = coll(&server, "collated");
    let collation = Collation::builder()
        .locale("en")
        .strength(mongodb::options::CollationStrength::Secondary)
        .build();
    let vals = ["b", "A", "a", "B", "é", "e", "ab", "Ab", "c"];
    c.insert_many(vals.iter().enumerate().map(|(i, s)| doc! {"_id": i as i32, "s": *s}))
        .run()
        .unwrap();
    index(
        &c,
        doc! {"s": 1},
        Some(IndexOptions::builder().collation(collation.clone()).build()),
    );
    let mut cur = c
        .find(doc! {"s": {"$gte": "a"}})
        .collation(collation)
        .batch_size(3)
        .run()
        .unwrap();
    let mut got = take(&mut cur, 3);
    c.insert_one(doc! {"_id": 20, "s": "AB"}).run().unwrap();
    c.insert_one(doc! {"_id": 21, "s": "a"}).run().unwrap();
    c.insert_one(doc! {"_id": 22, "s": "d"}).run().unwrap();
    got.extend(rest(cur));
    assert_eq!(ids(&got), ints(&[1, 2, 6, 7, 20, 0, 3, 8, 22, 5, 4]));
}

/// An `$or` reads its branches in turn, each document once, and a
/// `SORT_MERGE` holds one head per branch across batches.
#[test]
fn or_and_sort_merge_plans_read_on() {
    let server = Server::start().expect("start");
    let c = coll(&server, "or");
    c.insert_many((0..10).map(|i| doc! {"_id": i, "a": i, "b": 10 - i}))
        .run()
        .unwrap();
    index(&c, doc! {"a": 1}, None);
    index(&c, doc! {"b": 1}, None);
    let mut cur = c
        .find(doc! {"$or": [{"a": {"$lt": 4}}, {"b": {"$lt": 3}}]})
        .batch_size(2)
        .run()
        .unwrap();
    let mut got = take(&mut cur, 2);
    c.insert_one(doc! {"_id": 20, "a": 3.5, "b": 100}).run().unwrap();
    c.insert_one(doc! {"_id": 21, "a": -1, "b": 100}).run().unwrap();
    c.update_one(doc! {"_id": 0}, doc! {"$set": {"b": 0}})
        .run()
        .unwrap();
    c.insert_one(doc! {"_id": 22, "a": 100, "b": 1}).run().unwrap();
    got.extend(rest(cur));
    assert_eq!(ids(&got), ints(&[0, 1, 2, 3, 20, 9, 22, 8]));

    let c = coll(&server, "merge");
    c.insert_many((0..12).map(|i| doc! {"_id": i, "a": i % 3, "c": i}))
        .run()
        .unwrap();
    index(&c, doc! {"a": 1, "c": 1}, None);
    let mut cur = c
        .find(doc! {"a": {"$in": [0, 1]}})
        .sort(doc! {"c": 1})
        .batch_size(2)
        .run()
        .unwrap();
    let mut got = take(&mut cur, 2);
    c.insert_one(doc! {"_id": 20, "a": 0, "c": 2.5}).run().unwrap();
    c.insert_one(doc! {"_id": 21, "a": 1, "c": 0.5}).run().unwrap();
    c.delete_one(doc! {"_id": 4}).run().unwrap();
    got.extend(rest(cur));
    assert_eq!(ids(&got), ints(&[0, 1, 3, 6, 7, 9, 10]));

    let c = coll(&server, "merge_or");
    c.insert_many((0..12).map(|i| doc! {"_id": i, "a": i % 3, "b": i % 2, "c": i}))
        .run()
        .unwrap();
    index(&c, doc! {"a": 1, "c": 1}, None);
    index(&c, doc! {"b": 1, "c": 1}, None);
    let mut cur = c
        .find(doc! {"$or": [{"a": 0}, {"b": 1}]})
        .sort(doc! {"c": 1})
        .batch_size(2)
        .run()
        .unwrap();
    let mut got = take(&mut cur, 2);
    c.insert_one(doc! {"_id": 20, "a": 0, "b": 1, "c": 4.5})
        .run()
        .unwrap();
    c.update_one(doc! {"_id": 1}, doc! {"$set": {"c": 100}})
        .run()
        .unwrap();
    got.extend(rest(cur));
    assert_eq!(ids(&got), ints(&[0, 1, 3, 20, 5, 6, 7, 9, 11]));
}

/// `min` / `max` bound the hinted scan, which reads on between them.
#[test]
fn min_and_max_read_on() {
    let server = Server::start().expect("start");
    let c = coll(&server, "minmax");
    c.insert_many((0..10).map(|i| doc! {"_id": i, "x": i}))
        .run()
        .unwrap();
    index(&c, doc! {"x": 1}, None);
    let mut cur = c
        .find(doc! {})
        .hint(Hint::Keys(doc! {"x": 1}))
        .min(doc! {"x": 2})
        .max(doc! {"x": 8})
        .batch_size(2)
        .run()
        .unwrap();
    let mut got = take(&mut cur, 2);
    c.insert_one(doc! {"_id": 20, "x": 5.5}).run().unwrap();
    c.insert_one(doc! {"_id": 21, "x": 8}).run().unwrap();
    c.delete_one(doc! {"_id": 6}).run().unwrap();
    got.extend(rest(cur));
    assert_eq!(ids(&got), ints(&[2, 3, 4, 5, 20, 7]));
}

/// `returnKey` answers each document's key in the index, multikey included;
/// `showRecordId` adds each RecordId.
#[test]
fn return_key_and_show_record_id_read_on() {
    let server = Server::start().expect("start");
    let c = coll(&server, "rk");
    c.insert_many((0..4).map(|i| doc! {"_id": i, "t": [i, i + 10]}))
        .run()
        .unwrap();
    index(&c, doc! {"t": 1}, None);
    let mut cur = c
        .find(doc! {"t": {"$gte": 1}})
        .return_key(true)
        .batch_size(2)
        .run()
        .unwrap();
    let mut got = take(&mut cur, 2);
    c.insert_one(doc! {"_id": 20, "t": [2.5]}).run().unwrap();
    got.extend(rest(cur));
    let want: Vec<Document> = vec![
        doc! {"t": 1},
        doc! {"t": 2},
        doc! {"t": 2.5},
        doc! {"t": 3},
        doc! {"t": 10},
    ];
    assert_eq!(got, want);

    let c = coll(&server, "rid");
    c.insert_many((0..6).map(|i| doc! {"_id": i, "x": i}))
        .run()
        .unwrap();
    index(&c, doc! {"x": 1}, None);
    let mut cur = c
        .find(doc! {"x": {"$gte": 1}})
        .show_record_id(true)
        .batch_size(2)
        .run()
        .unwrap();
    let mut got = take(&mut cur, 2);
    c.insert_one(doc! {"_id": 20, "x": 4.5}).run().unwrap();
    got.extend(rest(cur));
    assert_eq!(ids(&got), ints(&[1, 2, 3, 4, 20, 5]));
    assert!(got.iter().all(|d| matches!(d.get("$recordId"), Some(Bson::Int64(_)))));
}

/// A sort of index keys below the FETCH reads every key for the first
/// batch and fetches each document as a batch reaches it: one deleted since
/// is gone, one whose key moved is dropped, one changed otherwise is read as
/// it is now.
#[test]
fn a_sort_of_keys_fetches_as_it_goes() {
    let server = Server::start().expect("start");
    let c = coll(&server, "sortkeys");
    c.insert_many((0..12).map(|i| doc! {"_id": i, "a": i % 3, "c": 20 - i, "z": 0}))
        .run()
        .unwrap();
    index(&c, doc! {"a": 1, "c": 1}, None);
    let mut cur = c
        .find(doc! {"a": {"$gt": 0}})
        .sort(doc! {"c": 1})
        .batch_size(2)
        .run()
        .unwrap();
    let mut got = take(&mut cur, 2);
    c.delete_one(doc! {"_id": 5}).run().unwrap();
    c.update_one(doc! {"_id": 4}, doc! {"$set": {"z": 1}})
        .run()
        .unwrap();
    c.update_one(doc! {"_id": 7}, doc! {"$set": {"c": 100, "z": 2}})
        .run()
        .unwrap();
    c.insert_one(doc! {"_id": 50, "a": 1, "c": 0, "z": 3})
        .run()
        .unwrap();
    got.extend(rest(cur));
    let seen: Vec<(i32, i32)> = got
        .iter()
        .map(|d| (d.get_i32("_id").unwrap(), d.get_i32("z").unwrap()))
        .collect();
    assert_eq!(seen, [(11, 0), (10, 0), (8, 0), (4, 1), (2, 0), (1, 0)]);
}

/// The `_id` index walks document `_id`s by value (`_id` key format 2).
#[test]
fn the_id_index_of_documents_reads_on() {
    let server = Server::start().expect("start");
    let c = coll(&server, "ids");
    c.insert_many((0..5).map(|i| doc! {"_id": {"a": i}}))
        .run()
        .unwrap();
    let mut cur = c.find(doc! {}).sort(doc! {"_id": 1}).batch_size(2).run().unwrap();
    let mut got = take(&mut cur, 2);
    c.insert_one(doc! {"_id": {"a": 3.5}}).run().unwrap();
    c.insert_one(doc! {"_id": {"a": 0.5}}).run().unwrap();
    c.delete_one(doc! {"_id": {"a": 4}}).run().unwrap();
    got.extend(rest(cur));
    let want: Vec<Bson> = vec![
        Bson::Document(doc! {"a": 0}),
        Bson::Document(doc! {"a": 1}),
        Bson::Document(doc! {"a": 2}),
        Bson::Document(doc! {"a": 3}),
        Bson::Document(doc! {"a": 3.5}),
    ];
    assert_eq!(ids(&got), want);
}
