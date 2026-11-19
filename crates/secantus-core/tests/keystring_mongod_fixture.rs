//! KeyString against a real mongod 8.2.11.
//!
//! `tests/fixtures/storage_model/mongod-8.2.11-layout.json` is the store a
//! mongod 8.2.11 wrote (`tools/probes/mongod_store_layout.py`). Each index
//! table there is a list of (key, value) rows as the storage engine holds them:
//! for an ordinary index the key is the KeyString of the indexed values plus
//! the RecordId and the value is the TypeBits; for `_id_` the key is the
//! KeyString of `_id` alone and the value is the RecordId then the TypeBits.
//! Every row below is rebuilt from the documents and compared byte for byte,
//! keys AND TypeBits, so a single changed byte fails the test.

use bson::{
    doc, oid::ObjectId, spec::BinarySubtype, Binary, Bson, DateTime, Decimal128, Regex, Timestamp,
};
use secantus_core::keystring::{self, Builder, Ordering, TypeBits, Version};
use std::collections::BTreeMap;

fn fixture() -> serde_json::Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/storage_model/mongod-8.2.11-layout.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json")
}

/// `index_tables[name]` as hex key -> hex value.
fn table(fx: &serde_json::Value, name: &str) -> BTreeMap<String, String> {
    fx["index_tables"][name]
        .as_array()
        .unwrap_or_else(|| panic!("no index table {name}"))
        .iter()
        .map(|r| {
            (
                r["key"].as_str().unwrap().to_string(),
                r["value"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// `KEYSTRING_VALUES` of `tools/probes/mongod_store_layout.py`, in order.
fn keystring_values() -> Vec<(&'static str, Bson)> {
    let date = |s: &str| Bson::DateTime(DateTime::parse_rfc3339_str(s).unwrap());
    vec![
        ("minkey", Bson::MinKey),
        ("null", Bson::Null),
        ("int_neg", Bson::Int32(-5)),
        ("int_zero", Bson::Int32(0)),
        ("int_one", Bson::Int32(1)),
        ("int_big", Bson::Int32(i32::MAX)),
        ("long_one", Bson::Int64(1)),
        ("long_big", Bson::Int64((1i64 << 53) + 1)),
        ("double_half", Bson::Double(0.5)),
        ("double_one", Bson::Double(1.0)),
        ("double_neg_zero", Bson::Double(-0.0)),
        ("double_nan", Bson::Double(f64::NAN)),
        ("double_inf", Bson::Double(f64::INFINITY)),
        ("double_ninf", Bson::Double(f64::NEG_INFINITY)),
        ("double_big", Bson::Double(1e300)),
        (
            "decimal_one",
            Bson::Decimal128("1".parse::<Decimal128>().unwrap()),
        ),
        (
            "decimal_onepointzero",
            Bson::Decimal128("1.0".parse::<Decimal128>().unwrap()),
        ),
        (
            "decimal_frac",
            Bson::Decimal128("0.1".parse::<Decimal128>().unwrap()),
        ),
        (
            "decimal_nan",
            Bson::Decimal128("NaN".parse::<Decimal128>().unwrap()),
        ),
        ("string_empty", Bson::String(String::new())),
        ("string_a", Bson::String("a".into())),
        ("string_nul", Bson::String("a\0b".into())),
        ("string_utf8", Bson::String("é漢".into())),
        ("object_empty", Bson::Document(doc! {})),
        ("object", Bson::Document(doc! {"x": 1, "y": "z"})),
        ("array_empty_in_obj", Bson::Document(doc! {"a": []})),
        (
            "bindata",
            Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes: vec![1, 2],
            }),
        ),
        (
            "bindata_uuid",
            Bson::Binary(Binary {
                subtype: BinarySubtype::Uuid,
                bytes: b"0123456789abcdef".to_vec(),
            }),
        ),
        (
            "objectid",
            Bson::ObjectId(ObjectId::parse_str("650000000000000000000001").unwrap()),
        ),
        ("bool_false", Bson::Boolean(false)),
        ("bool_true", Bson::Boolean(true)),
        ("date", date("2020-01-02T03:04:05Z")),
        ("date_neg", date("1960-01-01T00:00:00Z")),
        (
            "timestamp",
            Bson::Timestamp(Timestamp {
                time: 5,
                increment: 6,
            }),
        ),
        (
            "regex",
            Bson::RegularExpression(Regex {
                pattern: "ab+c".into(),
                options: "i".into(),
            }),
        ),
        ("code", Bson::JavaScriptCode("function(){}".into())),
        ("maxkey", Bson::MaxKey),
    ]
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn record_id(rid: i64) -> Vec<u8> {
    let mut b = Builder::new(Version::V1, Ordering::all_ascending());
    b.append_record_id_long(rid).unwrap();
    b.finish().unwrap().0
}

/// One index row of an ordinary index: the key (with RecordId) and the value.
fn index_row(ordering: Ordering, values: &[&Bson], rid: i64) -> (String, String) {
    let mut b = Builder::new(Version::V1, ordering);
    for v in values {
        b.append_bson_element(v).unwrap();
    }
    b.append_record_id_long(rid).unwrap();
    let (key, tb) = b.finish().unwrap();
    (hex(&key), hex(&tb.serialize()))
}

fn assert_table(name: &str, expected: BTreeMap<String, String>, fx: &serde_json::Value) {
    let actual = table(fx, name);
    assert_eq!(actual.len(), expected.len(), "{name}: row count");
    for (k, v) in &expected {
        match actual.get(k) {
            None => panic!(
                "{name}: key {k} (TypeBits {v}) is not in mongod's table; mongod has {actual:#?}"
            ),
            Some(av) => assert_eq!(av, v, "{name}: TypeBits of key {k}"),
        }
    }
    assert_eq!(actual, expected, "{name}");
}

fn ks_expected(ordering: Ordering) -> BTreeMap<String, String> {
    let values = keystring_values();
    let mut rows = BTreeMap::new();
    for (i, (label, v)) in values.iter().enumerate() {
        let (k, tb) = index_row(ordering, &[v], i as i64 + 1);
        assert!(rows.insert(k, tb).is_none(), "{label}: duplicate key");
    }
    // `{_id: 1000, label: "missing"}` has no `v`: an index key of null.
    let (k, tb) = index_row(ordering, &[&Bson::Null], values.len() as i64 + 1);
    rows.insert(k, tb);
    rows
}

#[test]
fn ks_ascending_index_matches_mongod_byte_for_byte() {
    let fx = fixture();
    assert_table("app.ks/v_1", ks_expected(Ordering::all_ascending()), &fx);
}

#[test]
fn ks_descending_index_matches_mongod_byte_for_byte() {
    let fx = fixture();
    assert_table(
        "app.ks/v_-1",
        ks_expected(Ordering::from_directions(&[-1])),
        &fx,
    );
}

#[test]
fn id_index_matches_mongod_byte_for_byte() {
    let fx = fixture();
    // `_id` = list position, RecordId = insert order from 1; the key is the
    // KeyString of `_id` alone (with the end byte), the value is RecordId then
    // TypeBits (empty for an int).
    let n = keystring_values().len() as i64;
    let mut expected = BTreeMap::new();
    for (id, rid) in (0..n).map(|i| (i, i + 1)).chain([(1000, n + 1)]) {
        let mut b = Builder::new(Version::V1, Ordering::all_ascending());
        b.append_bson_element(&Bson::Int32(id as i32)).unwrap();
        let (key, tb) = b.finish().unwrap();
        let mut value = record_id(rid);
        value.extend_from_slice(&tb.serialize());
        expected.insert(hex(&key), hex(&value));
    }
    assert_table("app.ks/_id_", expected, &fx);
}

#[test]
fn every_ks_key_decodes_back_to_its_value() {
    let fx = fixture();
    for (name, ordering) in [
        ("app.ks/v_1", Ordering::all_ascending()),
        ("app.ks/v_-1", Ordering::from_directions(&[-1])),
    ] {
        let values = keystring_values();
        for (key_hex, tb_hex) in table(&fx, name) {
            let key = unhex(&key_hex);
            let (rest, rid) = keystring::without_record_id_long_at_end(&key).unwrap();
            let tb = TypeBits::from_buffer(Version::V1, &unhex(&tb_hex)).unwrap();
            let got = keystring::to_bson(rest, ordering, &tb).unwrap();
            assert_eq!(got.len(), 1, "{name} {key_hex}");
            let want = if rid as usize > values.len() {
                Bson::Null
            } else {
                values[rid as usize - 1].1.clone()
            };
            assert!(
                same_bson(&got[0], &want),
                "{name} {key_hex}: {:?} vs {:?}",
                got[0],
                want
            );
            // The whole-key size agrees with the key without its RecordId.
            assert_eq!(keystring::get_key_size(&key, ordering).unwrap(), rest.len());
        }
    }
}

/// Equality that treats NaN as equal to itself and keeps the BSON type.
fn same_bson(a: &Bson, b: &Bson) -> bool {
    match (a, b) {
        (Bson::Double(x), Bson::Double(y)) => {
            (x.is_nan() && y.is_nan()) || x.to_bits() == y.to_bits()
        }
        (Bson::Decimal128(x), Bson::Decimal128(y)) => {
            // Same value; the exponent is carried by the TypeBits, so it is
            // compared too (bytes).
            x.bytes() == y.bytes() || (x.to_string() == "NaN" && y.to_string() == "NaN")
        }
        _ => a == b,
    }
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// The six documents behind `app.c`: `{_id: i, a: i % 3, b: "s{i}", t: [i, i+1]}`,
/// plus `{_id: 99, a: 9}` (RecordId 7, inserted in a transaction).
fn c_docs() -> Vec<(i64, bson::Document)> {
    let mut v: Vec<_> = (0..6)
        .map(|i| (i + 1, doc! {"_id": i as i32, "a": (i % 3) as i32, "b": format!("s{i}"), "t": [i as i32, i as i32 + 1]}))
        .collect();
    v.push((7, doc! {"_id": 99, "a": 9}));
    v
}

fn field<'a>(d: &'a bson::Document, k: &str) -> &'a Bson {
    d.get(k).unwrap_or(&Bson::Null)
}

#[test]
fn compound_unique_multikey_partial_and_ttl_indexes_match() {
    let fx = fixture();
    let docs = c_docs();

    // a_1_b_-1: ascending then descending.
    let ord = Ordering::from_directions(&[1, -1]);
    let exp: BTreeMap<_, _> = docs
        .iter()
        .map(|(rid, d)| index_row(ord, &[field(d, "a"), field(d, "b")], *rid))
        .collect();
    assert_table("app.c/a_1_b_-1", exp, &fx);

    // b_unique: the same key shape, value = TypeBits only (a unique index with
    // no duplicates keeps its RecordId in the key here, as the fixture shows).
    let exp: BTreeMap<_, _> = docs
        .iter()
        .map(|(rid, d)| index_row(Ordering::all_ascending(), &[field(d, "b")], *rid))
        .collect();
    assert_table("app.c/b_unique", exp, &fx);

    // t_multikey: one key per array element; a missing `t` is a null key.
    let mut exp = BTreeMap::new();
    for (rid, d) in &docs {
        match d.get("t") {
            Some(Bson::Array(a)) => {
                for e in a {
                    let (k, v) = index_row(Ordering::all_ascending(), &[e], *rid);
                    exp.insert(k, v);
                }
            }
            _ => {
                let (k, v) = index_row(Ordering::all_ascending(), &[&Bson::Null], *rid);
                exp.insert(k, v);
            }
        }
    }
    assert_table("app.c/t_multikey", exp, &fx);

    // a_partial: only documents with a > 0.
    let exp: BTreeMap<_, _> = docs
        .iter()
        .filter(|(_, d)| d.get_i32("a").unwrap() > 0)
        .map(|(rid, d)| index_row(Ordering::all_ascending(), &[field(d, "a")], *rid))
        .collect();
    assert_table("app.c/a_partial", exp, &fx);

    // ttl_idx on `created`, which no document has: a null key each.
    let exp: BTreeMap<_, _> = docs
        .iter()
        .map(|(rid, _)| index_row(Ordering::all_ascending(), &[&Bson::Null], *rid))
        .collect();
    assert_table("app.c/ttl_idx", exp, &fx);

    // _id_ of app.c.
    let exp: BTreeMap<_, _> = docs
        .iter()
        .map(|(rid, d)| {
            let mut b = Builder::new(Version::V1, Ordering::all_ascending());
            b.append_bson_element(field(d, "_id")).unwrap();
            let (key, tb) = b.finish().unwrap();
            let mut value = record_id(*rid);
            value.extend_from_slice(&tb.serialize());
            (hex(&key), hex(&value))
        })
        .collect();
    assert_table("app.c/_id_", exp, &fx);
}

#[test]
fn clustered_collection_indexes_use_string_record_ids() {
    let fx = fixture();
    // app.clustered: _id 1 (int), "s" (string), ObjectId(65..02) -- the
    // RecordId is the KeyString of `_id` without its end byte.
    let rid_of = |id: &Bson| {
        let mut b = Builder::new(Version::V1, Ordering::all_ascending());
        b.append_bson_element(id).unwrap();
        b.buffer().to_vec()
    };
    let ids = [
        Bson::Int32(1),
        Bson::String("s".into()),
        Bson::ObjectId(ObjectId::parse_str("650000000000000000000002").unwrap()),
    ];
    // x_1 keys: x = _id's `x`: the fixture shows 1->2, "s"->? Rebuild from the
    // fixture's own rows: decode each, then re-encode.
    let rows = table(&fx, "app.clustered/x_1");
    assert_eq!(rows.len(), 3);
    let mut rebuilt = BTreeMap::new();
    for (key_hex, tb_hex) in &rows {
        let key = unhex(key_hex);
        let (rest, rid) = keystring::without_record_id_str_at_end(&key).unwrap();
        let tb = TypeBits::from_buffer(Version::V1, &unhex(tb_hex)).unwrap();
        let x = keystring::to_bson(rest, Ordering::all_ascending(), &tb).unwrap();
        assert_eq!(x.len(), 1);
        assert!(
            ids.iter().any(|id| rid_of(id) == rid),
            "unknown clustered RecordId {rid:?}"
        );
        let mut b = Builder::new(Version::V1, Ordering::all_ascending());
        b.append_bson_element(&x[0]).unwrap();
        b.append_record_id_bytes(rid).unwrap();
        let (k, t) = b.finish().unwrap();
        rebuilt.insert(hex(&k), hex(&t.serialize()));
    }
    assert_eq!(rebuilt, rows);
}

#[test]
fn long_record_ids_round_trip_in_every_length() {
    for rid in [
        0i64,
        1,
        31,
        32,
        1023,
        1024,
        1 << 20,
        1 << 40,
        1 << 55,
        i64::MAX,
    ] {
        let mut b = Builder::new(Version::V1, Ordering::all_ascending());
        b.append_bson_element(&Bson::Int32(1)).unwrap();
        b.append_record_id_long(rid).unwrap();
        let (key, _) = b.finish().unwrap();
        let (_, got) = keystring::without_record_id_long_at_end(&key).unwrap();
        assert_eq!(got, rid);
    }
}
