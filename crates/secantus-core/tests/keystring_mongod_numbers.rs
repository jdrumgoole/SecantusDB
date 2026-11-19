//! KeyString against mongod 8.2.11 for numbers: Decimal128 values with long
//! continuations, at the exponent extremes and equal to a double or not;
//! doubles at the subnormal, integer and 2**53 boundaries; and ints and longs
//! at their edges. Recorded by `tools/probes/mongod_keystring_corpus.py`
//! (`tests/fixtures/storage_model/mongod-8.2.11-keystring-numbers.json`):
//! one document per value, `_id` = its position, so its RecordId is the
//! position + 1, under an ascending and a descending `{v}` index.

use bson::Bson;
use secantus_core::keystring::{Builder, Ordering, Version};
use std::collections::HashMap;

fn fixture() -> serde_json::Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/storage_model/mongod-8.2.11-keystring-numbers.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json")
}

/// The values, in insertion order, from their canonical extended JSON.
fn values(fx: &serde_json::Value) -> Vec<Bson> {
    fx["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let doc: serde_json::Value = serde_json::from_str(s.as_str().unwrap()).unwrap();
            let doc = Bson::try_from(doc).expect("canonical extended JSON");
            doc.as_document().unwrap().get("v").unwrap().clone()
        })
        .collect()
}

/// mongod's rows, `key hex -> TypeBits hex`.
fn rows(fx: &serde_json::Value, which: &str) -> HashMap<String, String> {
    fx[which]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let r = r.as_array().unwrap();
            (
                r[0].as_str().unwrap().to_string(),
                r[1].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn check(which: &str, direction: i32) {
    let fx = fixture();
    let values = values(&fx);
    let mongod = rows(&fx, which);
    assert_eq!(mongod.len(), values.len(), "one row per value");
    let mut wrong = Vec::new();
    for (i, v) in values.iter().enumerate() {
        let mut b = Builder::new(Version::V1, Ordering::from_directions(&[direction]));
        b.append_bson_element(v).unwrap();
        b.append_record_id_long(i as i64 + 1).unwrap();
        let (key, tb) = b.finish().unwrap();
        let (key, tb) = (hex(&key), hex(&tb.serialize()));
        match mongod.get(&key) {
            Some(want) if *want == tb => {}
            Some(want) => wrong.push(format!("{v:?}: TypeBits {tb}, mongod {want}")),
            None => wrong.push(format!("{v:?}: key {key} is not one mongod wrote")),
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} differ:\n{}",
        wrong.len(),
        values.len(),
        wrong.join("\n")
    );
}

#[test]
fn numbers_key_as_mongod_keys_them_ascending() {
    check("ascending", 1);
}

#[test]
fn numbers_key_as_mongod_keys_them_descending() {
    check("descending", -1);
}
