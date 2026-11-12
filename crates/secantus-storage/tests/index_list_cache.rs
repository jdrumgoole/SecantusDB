//! The index-list cache (`Storage::list_indexes_shared`) keeps a collection's
//! list until a change of THAT collection, as PostgreSQL's relation cache
//! keeps a relation's index list until an invalidation names the relation
//! (RP-911). Before, any DDL anywhere dropped every collection's list, and
//! the PostgreSQL server's first statement after a `CREATE TABLE` re-read
//! the list of every table.
//!
//! One test in its own binary: the cache's change numbering is
//! process-wide, so another test's DDL in the same process would drop the
//! entries this one watches.

use bson::{doc, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::Arc;

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn names(list: &[Document]) -> Vec<&str> {
    list.iter().map(|d| d.get_str("name").unwrap()).collect()
}

fn home() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("secantus-idx-list-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn an_index_list_is_read_again_only_after_a_change_of_its_collection() {
    let home = home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    st.insert_one("app", "y", &enc(&doc! {"_id": 1, "a": 1}))
        .unwrap();
    st.create_index("app", "y", "a_1", &doc! {"a": 1}, &doc! {})
        .unwrap();
    let first = st.list_indexes_shared("app", "y").unwrap();
    assert!(Arc::ptr_eq(
        &first,
        &st.list_indexes_shared("app", "y").unwrap()
    ));

    // Another collection's create, index, index drop and drop.
    st.create_collection("app", "x").unwrap();
    st.create_index("app", "x", "b_1", &doc! {"b": 1}, &doc! {})
        .unwrap();
    assert_eq!(
        names(&st.list_indexes_shared("app", "x").unwrap()),
        ["_id_", "b_1"]
    );
    st.drop_index("app", "x", "b_1").unwrap();
    assert_eq!(
        names(&st.list_indexes_shared("app", "x").unwrap()),
        ["_id_"]
    );
    st.drop_collection("app", "x").unwrap();
    assert!(st.list_indexes_shared("app", "x").unwrap().is_empty());
    let kept = st.list_indexes_shared("app", "y").unwrap();
    assert!(
        Arc::ptr_eq(&first, &kept),
        "y's index list was read again after DDL on x alone"
    );

    // Its own changes are seen.
    st.create_index("app", "y", "c_1", &doc! {"c": 1}, &doc! {})
        .unwrap();
    assert_eq!(
        names(&st.list_indexes_shared("app", "y").unwrap()),
        ["_id_", "a_1", "c_1"]
    );
    st.drop_index("app", "y", "a_1").unwrap();
    let own = st.list_indexes_shared("app", "y").unwrap();
    assert_eq!(names(&own), ["_id_", "c_1"]);

    // A rename moves index rows of two collections and names neither to
    // the cache: every list is read again.
    st.insert_one("app", "x", &enc(&doc! {"_id": 1})).unwrap();
    st.create_index("app", "x", "d_1", &doc! {"d": 1}, &doc! {})
        .unwrap();
    st.rename_collection("app", "x", "app", "z", false).unwrap();
    let after = st.list_indexes_shared("app", "y").unwrap();
    assert!(!Arc::ptr_eq(&own, &after));
    assert_eq!(names(&after), ["_id_", "c_1"]);
    assert_eq!(
        names(&st.list_indexes_shared("app", "z").unwrap()),
        ["_id_", "d_1"]
    );
    assert!(st.list_indexes_shared("app", "x").unwrap().is_empty());

    st.drop_collection("app", "y").unwrap();
    assert!(st.list_indexes_shared("app", "y").unwrap().is_empty());
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
