//! Users / roles / profiling tests (Phase 4 sub-phase 5e gap-closure): the
//! `(db, name) -> bson` auth records and per-db profile settings in the Rust
//! storage engine. Against real WiredTiger.

use bson::{doc, Document};
use secantus_storage::Storage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-auth-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn with_db(body: impl FnOnce(&Storage)) {
    let home = temp_home();
    let st = Storage::open(home.to_str().unwrap()).unwrap();
    body(&st);
    drop(st);
    let _ = std::fs::remove_dir_all(&home);
}

fn enc(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

fn dec(b: &[u8]) -> Document {
    Document::from_reader(&mut std::io::Cursor::new(b)).unwrap()
}

#[test]
fn user_crud_and_replace() {
    with_db(|st| {
        let rec = enc(&doc! {"user": "alice", "db": "admin", "roles": ["read"]});
        assert!(st.add_user("admin", "alice", &rec, false).unwrap());
        // Duplicate without replace is a no-op (returns false).
        assert!(!st
            .add_user("admin", "alice", &enc(&doc! {"user": "x"}), false)
            .unwrap());
        // Stored verbatim.
        let got = dec(&st.get_user("admin", "alice").unwrap().unwrap());
        assert_eq!(got.get_str("user").unwrap(), "alice");
        assert_eq!(got.get_array("roles").unwrap().len(), 1);
        // replace=true overwrites.
        let rec2 = enc(&doc! {"user": "alice", "roles": ["read", "write"]});
        assert!(st.add_user("admin", "alice", &rec2, true).unwrap());
        assert_eq!(
            dec(&st.get_user("admin", "alice").unwrap().unwrap())
                .get_array("roles")
                .unwrap()
                .len(),
            2
        );
        // Missing user → None; drop returns true then false.
        assert!(st.get_user("admin", "nobody").unwrap().is_none());
        assert!(st.drop_user("admin", "alice").unwrap());
        assert!(!st.drop_user("admin", "alice").unwrap());
        assert!(st.get_user("admin", "alice").unwrap().is_none());
    });
}

#[test]
fn list_users_filter_and_paginate() {
    with_db(|st| {
        st.add_user("admin", "a", &enc(&doc! {"user": "a"}), false)
            .unwrap();
        st.add_user("admin", "b", &enc(&doc! {"user": "b"}), false)
            .unwrap();
        st.add_user("app", "c", &enc(&doc! {"user": "c"}), false)
            .unwrap();
        // db=None spans all dbs.
        assert_eq!(st.list_users(None, 0, 100).unwrap().len(), 3);
        // Filter by db.
        assert_eq!(st.list_users(Some("admin"), 0, 100).unwrap().len(), 2);
        assert_eq!(st.list_users(Some("app"), 0, 100).unwrap().len(), 1);
        // skip / limit.
        assert_eq!(st.list_users(Some("admin"), 1, 100).unwrap().len(), 1);
        assert_eq!(st.list_users(None, 0, 2).unwrap().len(), 2);
    });
}

#[test]
fn role_crud_independent_of_users() {
    with_db(|st| {
        st.add_user("admin", "alice", &enc(&doc! {"user": "alice"}), false)
            .unwrap();
        let role = enc(&doc! {"role": "auditor", "db": "admin", "privileges": []});
        assert!(st.add_role("admin", "auditor", &role, false).unwrap());
        assert_eq!(
            dec(&st.get_role("admin", "auditor").unwrap().unwrap())
                .get_str("role")
                .unwrap(),
            "auditor"
        );
        // Roles and users live in separate tables.
        assert_eq!(st.list_roles(None, 0, 100).unwrap().len(), 1);
        assert_eq!(st.list_users(None, 0, 100).unwrap().len(), 1);
        assert!(st.drop_role("admin", "auditor").unwrap());
        assert!(st.get_role("admin", "auditor").unwrap().is_none());
    });
}

#[test]
fn profile_defaults_and_roundtrip() {
    with_db(|st| {
        // Unset → mongod defaults.
        let p = st.get_profile("app").unwrap();
        assert_eq!(p.get_i32("level").unwrap(), 0);
        assert_eq!(p.get_i32("slowms").unwrap(), 100);
        assert_eq!(p.get_f64("sampleRate").unwrap(), 1.0);
        // Set + read back (slowms=0 / rate=0.0 must round-trip, not revert).
        st.set_profile("app", 1, 0, 0.0).unwrap();
        let p = st.get_profile("app").unwrap();
        assert_eq!(p.get_i32("level").unwrap(), 1);
        assert_eq!(p.get_i32("slowms").unwrap(), 0);
        assert_eq!(p.get_f64("sampleRate").unwrap(), 0.0);
    });
}

#[test]
fn profile_validation_rejects_bad_values() {
    with_db(|st| {
        assert!(st.set_profile("app", 3, 100, 1.0).is_err()); // level out of range
        assert!(st.set_profile("app", 1, -1, 1.0).is_err()); // negative slowms
        assert!(st.set_profile("app", 1, 100, 1.5).is_err()); // rate > 1
    });
}

#[test]
fn ensure_profile_collection_is_capped_and_idempotent() {
    with_db(|st| {
        assert!(!st.collection_exists("app", "system.profile").unwrap());
        st.ensure_profile_collection("app", 4096).unwrap();
        assert!(st.collection_exists("app", "system.profile").unwrap());
        assert!(st.collection_is_capped("app", "system.profile").unwrap());
        // Idempotent — a second call is a no-op (doesn't error or recreate).
        st.ensure_profile_collection("app", 4096).unwrap();
        assert!(st.collection_is_capped("app", "system.profile").unwrap());
    });
}
