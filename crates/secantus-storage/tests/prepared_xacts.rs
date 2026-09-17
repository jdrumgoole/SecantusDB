//! Two-phase commit at the storage layer: `prepare_user_transaction` parks a
//! user transaction under a gid, `commit_prepared` / `rollback_prepared`
//! resolve it -- from the same process while the handle is live, or after a
//! reopen, when the write set recorded at PREPARE is replayed. Against real
//! WiredTiger.

use bson::{doc, Document};
use secantus_storage::{Storage, StorageError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-2pc-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn rows(st: &Storage, coll: &str) -> Vec<Document> {
    let mut out: Vec<Document> = st
        .find_matching("app", coll, &Document::new())
        .unwrap()
        .iter()
        .map(|b| bson::from_slice(b).unwrap())
        .collect();
    out.sort_by_key(|d| d.get_i32("_id").unwrap());
    out
}

fn gids(st: &Storage) -> Vec<String> {
    st.list_prepared_xacts()
        .unwrap()
        .into_iter()
        .map(|x| x.gid)
        .collect()
}

/// Insert / update / delete / create-collection inside one transaction,
/// PREPARE it, and resolve it from the same open store.
fn prepare_mixed(st: &Storage, gid: &str) {
    let txn = st.begin_user_transaction().unwrap();
    let mut txn = txn;
    st.with_user_transaction(&mut txn, || {
        st.insert_one("app", "t", &enc(&doc! {"_id": 2, "b": "new"}))?;
        st.update_matching(
            "app",
            "t",
            &doc! {"_id": 1},
            &doc! {"$set": {"b": "upd"}},
            false,
            false,
            &[],
            &Document::new(),
            None,
            None,
            false,
        )?;
        st.delete_matching("app", "t", &doc! {"_id": 3}, 0, &Document::new(), None)?;
        st.create_collection("app", "made")?;
        st.insert_one("app", "made", &enc(&doc! {"_id": 9}))?;
        Ok::<(), StorageError>(())
    })
    .unwrap()
    .unwrap();
    let x = st
        .prepare_user_transaction(txn, gid, "postgres", "postgres")
        .unwrap();
    assert_eq!(x.gid, gid);
    assert_eq!(x.owner, "postgres");
    assert_eq!(x.database, "postgres");
}

fn seed(st: &Storage) {
    st.insert_one("app", "t", &enc(&doc! {"_id": 1, "b": "base"}))
        .unwrap();
    st.insert_one("app", "t", &enc(&doc! {"_id": 3, "b": "going"}))
        .unwrap();
}

fn assert_untouched(st: &Storage) {
    assert_eq!(
        rows(st, "t"),
        vec![doc! {"_id": 1, "b": "base"}, doc! {"_id": 3, "b": "going"}]
    );
    assert!(!st
        .list_collections("app")
        .unwrap()
        .iter()
        .any(|c| c == "made"));
}

fn assert_applied(st: &Storage) {
    assert_eq!(
        rows(st, "t"),
        vec![doc! {"_id": 1, "b": "upd"}, doc! {"_id": 2, "b": "new"}]
    );
    assert_eq!(rows(st, "made"), vec![doc! {"_id": 9}]);
}

#[test]
fn prepared_work_is_invisible_until_commit_prepared() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    seed(&st);
    prepare_mixed(&st, "g1");
    assert_eq!(gids(&st), vec!["g1".to_string()]);
    assert_untouched(&st);
    st.commit_prepared("g1").unwrap();
    assert_applied(&st);
    assert!(gids(&st).is_empty());
    assert!(matches!(
        st.commit_prepared("g1"),
        Err(StorageError::PreparedTransactionNotFound(_))
    ));
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn rollback_prepared_discards_the_work() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    seed(&st);
    prepare_mixed(&st, "g2");
    st.rollback_prepared("g2").unwrap();
    assert_untouched(&st);
    assert!(gids(&st).is_empty());
    assert!(matches!(
        st.rollback_prepared("g2"),
        Err(StorageError::PreparedTransactionNotFound(_))
    ));
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn duplicate_gid_is_refused_and_the_second_transaction_rolled_back() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    seed(&st);
    prepare_mixed(&st, "dup");
    let mut txn = st.begin_user_transaction().unwrap();
    st.with_user_transaction(&mut txn, || {
        st.insert_one("app", "t", &enc(&doc! {"_id": 7}))
    })
    .unwrap()
    .unwrap();
    assert!(matches!(
        st.prepare_user_transaction(txn, "dup", "postgres", "postgres"),
        Err(StorageError::PreparedTransactionExists(_))
    ));
    st.rollback_prepared("dup").unwrap();
    // Neither transaction's rows are there: the duplicate was rolled back.
    assert_untouched(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn prepared_transaction_survives_reopen_and_commits() {
    let home = temp_home();
    {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        seed(&st);
        prepare_mixed(&st, "g3");
        prepare_mixed_second(&st, "g4");
    }
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    assert_eq!(gids(&st), vec!["g3".to_string(), "g4".to_string()]);
    assert_untouched(&st);
    st.commit_prepared("g3").unwrap();
    assert_applied(&st);
    st.rollback_prepared("g4").unwrap();
    assert!(gids(&st).is_empty());
    // g4's row (id 5) never landed.
    assert!(rows(&st, "t")
        .iter()
        .all(|d| d.get_i32("_id").unwrap() != 5));
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

fn prepare_mixed_second(st: &Storage, gid: &str) {
    let mut txn = st.begin_user_transaction().unwrap();
    st.with_user_transaction(&mut txn, || {
        st.insert_one("app", "t", &enc(&doc! {"_id": 5, "b": "other"}))
    })
    .unwrap()
    .unwrap();
    st.prepare_user_transaction(txn, gid, "postgres", "postgres")
        .unwrap();
}

#[test]
fn empty_transaction_can_be_prepared_and_committed() {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    let txn = st.begin_user_transaction().unwrap();
    st.prepare_user_transaction(txn, "", "postgres", "postgres")
        .unwrap();
    assert_eq!(gids(&st), vec![String::new()]);
    st.commit_prepared("").unwrap();
    assert!(gids(&st).is_empty());
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// A block whose FIRST write is DDL (the pgserver's `CREATE TABLE` shape).
#[test]
fn ddl_first_prepared_transaction_replays_after_reopen() {
    let home = temp_home();
    {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.create_collection("app", "made")?;
            st.insert_one("app", "made", &enc(&doc! {"_id": 9}))
        })
        .unwrap()
        .unwrap();
        st.prepare_user_transaction(txn, "ddl", "postgres", "postgres")
            .unwrap();
    }
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    assert_eq!(gids(&st), vec!["ddl".to_string()]);
    assert!(!st
        .list_collections("app")
        .unwrap()
        .iter()
        .any(|c| c == "made"));
    st.commit_prepared("ddl").unwrap();
    assert_eq!(rows(&st, "made"), vec![doc! {"_id": 9}]);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// Server bookkeeping runs `outside_user_transaction` in the middle of a
/// block (an oid counter between `CREATE TABLE`'s catalog writes). The
/// autocommit statement it runs must not walk off with the seq ranges the
/// block's earlier writes parked: those are the write set `PREPARE
/// TRANSACTION` records, and a replay from a record missing the table's
/// creation lands rows in a table that does not exist.
#[test]
fn bookkeeping_outside_the_block_keeps_the_prepared_write_set_whole() {
    let home = temp_home();
    {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        st.create_collection("app", "counter").unwrap();
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.create_collection("app", "made")?;
            st.insert_one("app", "made", &enc(&doc! {"_id": 1}))?;
            st.outside_user_transaction(|| {
                st.insert_one("app", "counter", &enc(&doc! {"_id": "next", "n": 1}))
            })?;
            st.insert_one("app", "made", &enc(&doc! {"_id": 2}))
        })
        .unwrap()
        .unwrap();
        st.prepare_user_transaction(txn, "mid", "postgres", "postgres")
            .unwrap();
        // The counter row committed on its own, outside the block.
        assert_eq!(rows(&st, "counter"), vec![doc! {"_id": "next", "n": 1}]);
    }
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    assert!(!st
        .list_collections("app")
        .unwrap()
        .iter()
        .any(|c| c == "made"));
    st.commit_prepared("mid").unwrap();
    assert_eq!(rows(&st, "made"), vec![doc! {"_id": 1}, doc! {"_id": 2}]);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

/// A recovered prepared transaction holds no row locks (it is a record, not
/// an open WiredTiger transaction), so a write can slip in under it. The
/// replay must not paper over that: an insert whose unique key now exists
/// fails the COMMIT PREPARED, nothing of the replay lands, and the record
/// stays for a ROLLBACK PREPARED.
#[test]
fn replay_refuses_a_key_taken_since_the_prepare() {
    let home = temp_home();
    {
        let st = Storage::open(home.to_str().unwrap()).unwrap();
        st.create_collection("app", "t").unwrap();
        st.create_index("app", "t", "a_1", &doc! {"a": 1}, &doc! {"unique": true})
            .unwrap();
        let mut txn = st.begin_user_transaction().unwrap();
        st.with_user_transaction(&mut txn, || {
            st.insert_one("app", "t", &enc(&doc! {"_id": 1, "a": 1}))?;
            st.insert_one("app", "t", &enc(&doc! {"_id": 2, "a": 2}))
        })
        .unwrap()
        .unwrap();
        st.prepare_user_transaction(txn, "taken", "postgres", "postgres")
            .unwrap();
    }
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    st.insert_one("app", "t", &enc(&doc! {"_id": 9, "a": 2}))
        .unwrap();
    let err = st.commit_prepared("taken").unwrap_err();
    assert!(
        matches!(err, StorageError::DuplicateKey(_)),
        "expected a duplicate-key refusal, got {err:?}"
    );
    assert_eq!(rows(&st, "t"), vec![doc! {"_id": 9, "a": 2}]);
    assert_eq!(gids(&st), vec!["taken".to_string()]);
    st.rollback_prepared("taken").unwrap();
    assert!(gids(&st).is_empty());
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}
