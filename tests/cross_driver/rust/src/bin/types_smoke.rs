//! Cross-driver BSON type fidelity smoke — Rust (mongo-rust-driver).
//!
//! Insert one document containing every BSON type the Rust driver
//! surfaces as a distinct variant of `bson::Bson`, then find it back
//! and assert each field preserved its variant and value. Catches
//! wire-shape divergences that one driver tolerates but another
//! doesn't (e.g. int32-vs-int64 collision under permissive type
//! coercion).
use std::env;
use std::process::exit;

use bson::oid::ObjectId;
use bson::spec::BinarySubtype;
use bson::{doc, Binary, Bson, DateTime, Decimal128};
use mongodb::sync::Client;

fn fail(msg: String) -> ! {
    eprintln!("{msg}");
    exit(1);
}

fn main() {
    let uri = env::var("MONGODB_URI").unwrap_or_else(|_| {
        eprintln!("MONGODB_URI not set");
        exit(2);
    });

    let client = Client::with_uri_str(&uri).unwrap_or_else(|e| fail(format!("connect: {e}")));
    let coll = client.database("types_xd_rust").collection::<bson::Document>("c");
    let _ = coll.drop().run();

    let oid = ObjectId::new();
    let dec = "3.141592653589793238".parse::<Decimal128>().unwrap();
    let when = DateTime::from_millis(1_780_000_000_000); // 2026-05-29
    let bin_payload = b"hello".to_vec();
    let bin = Binary { subtype: BinarySubtype::Generic, bytes: bin_payload.clone() };

    let doc_in = doc! {
        "_id": oid,
        "i32": 2_147_483_647_i32,
        "i64": 9_223_372_036_854_775_807_i64,
        "f64": 2.5_f64,
        "dec": dec.clone(),
        "dt": when,
        "bin": bin.clone(),
        "b": true,
        "n": Bson::Null,
        "sub": doc! { "x": 1_i32 },
        "arr": [Bson::Int32(1), Bson::String("two".into()), Bson::Double(3.5)],
    };
    coll.insert_one(doc_in).run().unwrap_or_else(|e| fail(format!("insert: {e}")));

    let got = coll
        .find_one(doc! {"_id": oid})
        .run()
        .unwrap_or_else(|e| fail(format!("find: {e}")))
        .unwrap_or_else(|| fail("find_one returned None".into()));

    if got.get_object_id("_id").ok() != Some(oid) {
        fail(format!("_id: got {:?}", got.get("_id")));
    }
    if got.get_i32("i32").ok() != Some(2_147_483_647) {
        fail(format!("i32: got {:?}", got.get("i32")));
    }
    if got.get_i64("i64").ok() != Some(9_223_372_036_854_775_807) {
        fail(format!("i64: got {:?}", got.get("i64")));
    }
    if got.get_f64("f64").ok() != Some(2.5) {
        fail(format!("f64: got {:?}", got.get("f64")));
    }
    if got.get("dec") != Some(&Bson::Decimal128(dec)) {
        fail(format!("dec: got {:?}", got.get("dec")));
    }
    if got.get_datetime("dt").ok().map(|d| d.timestamp_millis()) != Some(1_780_000_000_000) {
        fail(format!("dt: got {:?}", got.get("dt")));
    }
    match got.get("bin") {
        Some(Bson::Binary(b)) if b.bytes == bin_payload && b.subtype == BinarySubtype::Generic => {}
        other => fail(format!("bin: got {:?}", other)),
    }
    if got.get_bool("b").ok() != Some(true) {
        fail(format!("b: got {:?}", got.get("b")));
    }
    if got.get("n") != Some(&Bson::Null) {
        fail(format!("n: got {:?}", got.get("n")));
    }
    let sub = got.get_document("sub").unwrap_or_else(|e| fail(format!("sub: {e}")));
    if sub.get_i32("x").ok() != Some(1) {
        fail(format!("sub.x: got {:?}", sub.get("x")));
    }
    let arr = got.get_array("arr").unwrap_or_else(|e| fail(format!("arr: {e}")));
    if arr.len() != 3 {
        fail(format!("arr len: got {}", arr.len()));
    }
    match (&arr[0], &arr[1], &arr[2]) {
        (Bson::Int32(1), Bson::String(s), Bson::Double(3.5)) if s == "two" => {}
        _ => fail(format!("arr: got {:?}", arr)),
    }

    println!("OK");
}
