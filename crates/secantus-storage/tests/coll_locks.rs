//! Per-collection write locks are let go once nobody holds them (RM-194):
//! a server that has created and dropped many collections must not keep a
//! mutex for every one.

use bson::doc;
use secantus_storage::Storage;

#[test]
fn per_collection_locks_do_not_grow_without_bound() {
    let dir = tempfile::tempdir().unwrap();
    let st = Storage::open(dir.path().to_str().unwrap()).unwrap();
    for i in 0..5_000 {
        let coll = format!("c{i}");
        st.insert(
            "app",
            &coll,
            vec![bson::to_vec(&doc! {"_id": i}).unwrap()],
            true,
        )
        .unwrap();
        st.drop_collection("app", &coll).unwrap();
    }
    // Pruned whenever a new namespace finds the map at its threshold
    // (1,024, doubling past what is still held), so it stays near it.
    assert!(
        st.coll_lock_count() <= 2_048,
        "{} per-collection locks kept",
        st.coll_lock_count()
    );
    // A namespace still works after its lock was pruned.
    st.insert(
        "app",
        "c0",
        vec![bson::to_vec(&doc! {"_id": 1}).unwrap()],
        true,
    )
    .unwrap();
    assert_eq!(st.count_matching("app", "c0", &doc! {}, None).unwrap(), 1);
}
