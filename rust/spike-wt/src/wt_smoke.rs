//! Spike 2 — minimal WiredTiger FFI smoke test.
//!
//! Exercises the handful of WT calls the production storage layer leans on,
//! proving the FFI path end to end: open a connection on a temp dir, open a
//! session, create a table, insert/search a key via a cursor, and range-scan.
//! If this runs green, the "FFI into the vendored C library" plan (option A in
//! tasks/rust-rewrite-plan.md §4.1) is viable.

#![allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code
)]

mod wt {
    include!(concat!(env!("OUT_DIR"), "/wt_bindings.rs"));
}

use std::ffi::{c_char, c_int, CStr, CString};
use std::ptr;

unsafe fn check(rc: c_int, what: &str) {
    if rc != 0 {
        let msg = wt::wiredtiger_strerror(rc);
        let s = if msg.is_null() {
            "<null>".to_string()
        } else {
            CStr::from_ptr(msg).to_string_lossy().into_owned()
        };
        panic!("{what} failed: rc={rc} ({s})");
    }
}

fn main() {
    // Unique temp home so reruns don't collide.
    let home = std::env::temp_dir().join(format!("wt-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let home_c = CString::new(home.to_string_lossy().as_bytes()).unwrap();

    unsafe {
        let mut conn: *mut wt::WT_CONNECTION = ptr::null_mut();
        let cfg = CString::new("create,cache_size=64M").unwrap();
        check(
            wt::wiredtiger_open(home_c.as_ptr(), ptr::null_mut(), cfg.as_ptr(), &mut conn),
            "wiredtiger_open",
        );

        let open_session = (*conn).open_session.expect("open_session");
        let mut session: *mut wt::WT_SESSION = ptr::null_mut();
        check(
            open_session(conn, ptr::null_mut(), ptr::null(), &mut session),
            "open_session",
        );

        let create = (*session).create.expect("create");
        let uri = CString::new("table:smoke").unwrap();
        let schema = CString::new("key_format=S,value_format=S").unwrap();
        check(
            create(session, uri.as_ptr(), schema.as_ptr()),
            "create table",
        );

        // Insert a few rows.
        let open_cursor = (*session).open_cursor.expect("open_cursor");
        let mut cursor: *mut wt::WT_CURSOR = ptr::null_mut();
        check(
            open_cursor(
                session,
                uri.as_ptr(),
                ptr::null_mut(),
                ptr::null(),
                &mut cursor,
            ),
            "open_cursor",
        );
        let set_key = (*cursor).set_key.expect("set_key");
        let set_value = (*cursor).set_value.expect("set_value");
        let insert = (*cursor).insert.expect("insert");

        let rows = [("alpha", "1"), ("bravo", "2"), ("charlie", "3")];
        for (k, v) in rows {
            let kc = CString::new(k).unwrap();
            let vc = CString::new(v).unwrap();
            set_key(cursor, kc.as_ptr());
            set_value(cursor, vc.as_ptr());
            check(insert(cursor), "insert");
        }
        let reset = (*cursor).reset.expect("reset");
        check(reset(cursor), "reset");

        // Point search.
        let search = (*cursor).search.expect("search");
        let get_value = (*cursor).get_value.expect("get_value");
        let kc = CString::new("bravo").unwrap();
        set_key(cursor, kc.as_ptr());
        check(search(cursor), "search bravo");
        let mut val: *const c_char = ptr::null();
        check(get_value(cursor, &mut val), "get_value");
        let got = CStr::from_ptr(val).to_string_lossy();
        assert_eq!(got, "2", "search returned wrong value");
        println!("  [ok] point search: bravo -> {got}");

        check(reset(cursor), "reset");

        // Ordered range scan — confirms the B-tree gives us sorted iteration,
        // the property the whole index design rests on.
        let next = (*cursor).next.expect("next");
        let get_key = (*cursor).get_key.expect("get_key");
        let mut scanned = Vec::new();
        loop {
            let rc = next(cursor);
            if rc == wt::WT_NOTFOUND {
                break;
            }
            check(rc, "next");
            let mut k: *const c_char = ptr::null();
            let mut v: *const c_char = ptr::null();
            check(get_key(cursor, &mut k), "get_key");
            check(get_value(cursor, &mut v), "get_value");
            scanned.push((
                CStr::from_ptr(k).to_string_lossy().into_owned(),
                CStr::from_ptr(v).to_string_lossy().into_owned(),
            ));
        }
        assert_eq!(
            scanned,
            vec![
                ("alpha".into(), "1".into()),
                ("bravo".into(), "2".into()),
                ("charlie".into(), "3".into()),
            ],
            "range scan order/content wrong"
        );
        println!("  [ok] ordered scan: {scanned:?}");

        let close = (*conn).close.expect("close");
        check(close(conn, ptr::null()), "close");
    }

    // Tidy up the temp home.
    let _ = std::fs::remove_dir_all(&home);
    println!("\nRESULT: PASS — WiredTiger FFI (open/session/create/insert/search/scan) works");
}
