//! The order of the indexes in a collection and of each spec's fields, as
//! mongod 8.2.11 keeps them (RM-734).
//!
//! mongod's durable catalog holds indexes in build order and each spec in
//! the field order it was written in: `v`, `unique`, `hidden`,
//! `prepareUnique`, `key`, `name`, then the rest at creation, and after
//! `name` for an option a `collMod` adds. Its in-memory catalog moves an
//! index a `collMod` changed to the end, which `dropIndexes: "*"` follows
//! until the process restarts.

use bson::{doc, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-ixorder-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn build(st: &Storage) {
    st.insert_one("app", "c", &bson::to_vec(&doc! {"_id": 1}).unwrap())
        .unwrap();
    st.create_index("app", "c", "a_1", &doc! {"a": 1}, &doc! {"unique": true})
        .unwrap();
    st.create_index("app", "c", "b_1", &doc! {"b": 1}, &Document::new())
        .unwrap();
    st.create_index("app", "c", "d_1", &doc! {"d": 1}, &doc! {"hidden": true})
        .unwrap();
}

/// `(index, its spec's field names)` of every `dropIndexes` entry logged.
fn dropped(st: &Storage) -> Vec<(String, Vec<String>)> {
    st.flush_oplog();
    st.read_oplog(1, 1000)
        .unwrap()
        .into_iter()
        .filter_map(|(_, blob)| {
            let e: Document = bson::from_slice(&blob).ok()?;
            let o = e.get_document("o").ok()?;
            o.get_str("dropIndexes").ok()?;
            // mongod's form carries the spec as `o2`.
            let spec = e
                .get_document("o2")
                .or_else(|_| o.get_document("spec"))
                .ok()?;
            Some((
                o.get_str("index").ok()?.to_string(),
                spec.keys().cloned().collect(),
            ))
        })
        .collect()
}

fn names(v: &[(String, Vec<String>)]) -> Vec<&str> {
    v.iter().map(|(n, _)| n.as_str()).collect()
}

fn fields(v: &[(String, Vec<String>)], name: &str) -> Vec<String> {
    v.iter().find(|(n, _)| n == name).unwrap().1.clone()
}

#[test]
fn a_collmodded_index_is_dropped_last_with_its_option_after_name() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    build(&st);
    st.set_index_options("app", "c", "a_1", &doc! {"hidden": true})
        .unwrap();
    st.drop_all_indexes("app", "c").unwrap();
    let got = dropped(&st);
    assert_eq!(names(&got), ["b_1", "d_1", "a_1"]);
    assert_eq!(
        fields(&got, "a_1"),
        ["v", "unique", "key", "name", "hidden"]
    );
    assert_eq!(fields(&got, "d_1"), ["v", "hidden", "key", "name"]);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_restart_puts_the_indexes_back_in_build_order_and_keeps_the_spec_order() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    build(&st);
    st.set_index_options("app", "c", "a_1", &doc! {"hidden": true})
        .unwrap();
    drop(st);
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    st.drop_all_indexes("app", "c").unwrap();
    let got = dropped(&st);
    assert_eq!(names(&got), ["a_1", "b_1", "d_1"]);
    assert_eq!(
        fields(&got, "a_1"),
        ["v", "unique", "key", "name", "hidden"]
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_index_made_after_a_collmod_is_dropped_after_the_moved_one() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    build(&st);
    st.set_index_options("app", "c", "a_1", &doc! {"hidden": true})
        .unwrap();
    st.create_index("app", "c", "e_1", &doc! {"e": 1}, &Document::new())
        .unwrap();
    st.drop_all_indexes("app", "c").unwrap();
    assert_eq!(names(&dropped(&st)), ["b_1", "d_1", "a_1", "e_1"]);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_option_set_to_its_default_leaves_the_spec_and_a_dropped_index_forgets_its_place() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    build(&st);
    st.set_index_options("app", "c", "a_1", &doc! {"hidden": true})
        .unwrap();
    st.set_index_options("app", "c", "a_1", &doc! {"hidden": false})
        .unwrap();
    // Dropped and made again, `a_1` is the newest.
    st.drop_index("app", "c", "a_1").unwrap();
    st.create_index("app", "c", "a_1", &doc! {"a": 1}, &Document::new())
        .unwrap();
    let before = dropped(&st).len();
    st.drop_all_indexes("app", "c").unwrap();
    let got = dropped(&st).split_off(before);
    let listed: Vec<&str> = names(&got).into_iter().filter(|n| *n != "a_1").collect();
    assert_eq!(listed, ["b_1", "d_1"]);
    assert_eq!(*names(&got).last().unwrap(), "a_1");
    assert_eq!(fields(&got, "a_1"), ["v", "key", "name"]);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_new_ttl_setting_is_written_last_in_the_spec() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    st.insert_one("app", "c", &bson::to_vec(&doc! {"_id": 1}).unwrap())
        .unwrap();
    st.create_index(
        "app",
        "c",
        "x_1",
        &doc! {"x": 1},
        &doc! {"expireAfterSeconds": 100, "sparse": true},
    )
    .unwrap();
    let order = |st: &Storage| -> Vec<String> {
        st.index_catalog_spec("app", "c", "x_1")
            .unwrap()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    };
    assert_eq!(
        order(&st),
        ["v", "key", "name", "expireAfterSeconds", "sparse"]
    );
    assert!(st.set_index_expiry("app", "c", "x_1", 7).unwrap());
    assert_eq!(
        order(&st),
        ["v", "key", "name", "sparse", "expireAfterSeconds"]
    );
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
