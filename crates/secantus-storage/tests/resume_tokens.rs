//! Resume tokens against the ones a real mongod 8.2.11 made.
//!
//! `tests/fixtures/storage_model/mongod-8.2.11-resume-tokens.json` is the
//! `_id` of change events of every kind (`tools/probes/resume_tokens_fixture.py`),
//! with what each token must say, taken from the event: its time, `txnOpIndex`,
//! the collection UUID and the event identifier. Each token is decoded here
//! and compared with that, and built again from it, byte for byte: `_data` and
//! `_typeBits` both.

use bson::{spec::BinarySubtype, Binary, Bson, Document, Timestamp};
use secantus_storage::changestreams::{
    make_high_water_mark_token, make_resume_token, parse_resume_token, position_ts, ts_position,
    ResumeTokenData, TOKEN_TYPE_EVENT, TOKEN_TYPE_HIGH_WATER_MARK, TOKEN_VERSION,
};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn cases() -> Vec<serde_json::Value> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/storage_model/mongod-8.2.11-resume-tokens.json"
    );
    let fx: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json");
    assert_eq!(fx["mongod_version"], "db version v8.2.11");
    fx["cases"].as_array().unwrap().clone()
}

fn token_of(case: &serde_json::Value) -> Document {
    let mut t = Document::new();
    t.insert("_data", case["data"].as_str().unwrap());
    if let Some(tb) = case["typeBits"].as_str() {
        t.insert(
            "_typeBits",
            Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes: hex(tb),
            }),
        );
    }
    t
}

/// What the case says the token holds, as the inputs to build one.
fn expected(case: &serde_json::Value) -> ResumeTokenData {
    let ct = case["clusterTime"].as_array().unwrap();
    let ident = match &case["eventIdentifier"] {
        serde_json::Value::Null => None,
        v => match Bson::try_from(v.clone()).unwrap() {
            Bson::Document(d) => Some(d),
            other => panic!("identifier {other:?}"),
        },
    };
    ResumeTokenData {
        cluster_time: Timestamp {
            time: ct[0].as_u64().unwrap() as u32,
            increment: ct[1].as_u64().unwrap() as u32,
        },
        version: TOKEN_VERSION,
        token_type: case["tokenType"].as_i64().unwrap() as i32,
        op_index: case["txnOpIndex"].as_i64().unwrap(),
        from_invalidate: case["fromInvalidate"].as_bool().unwrap(),
        uuid: case["uuid"].as_str().map(hex),
        event_identifier: ident,
        fragment: case["fragmentNum"].as_i64().map(|n| n as i32),
    }
}

fn bytes(d: &Document) -> Vec<u8> {
    bson::to_vec(d).unwrap()
}

#[test]
fn mongods_tokens_decode_to_what_the_event_said() {
    let cases = cases();
    assert!(cases.len() >= 35, "{} cases", cases.len());
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let got = parse_resume_token(&token_of(case)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let want = expected(case);
        assert_eq!(got.cluster_time, want.cluster_time, "{name}");
        assert_eq!(got.token_type, want.token_type, "{name}");
        assert_eq!(got.op_index, want.op_index, "{name}");
        assert_eq!(got.from_invalidate, want.from_invalidate, "{name}");
        assert_eq!(got.uuid, want.uuid, "{name}");
        assert_eq!(got.fragment, want.fragment, "{name}");
        // The identifier, with its key order and the types of its numbers.
        assert_eq!(
            got.event_identifier.as_ref().map(bytes),
            want.event_identifier.as_ref().map(bytes),
            "{name}: {:?}",
            got.event_identifier
        );
    }
}

#[test]
fn our_encoder_makes_mongods_bytes_from_the_same_inputs() {
    for case in cases() {
        let name = case["name"].as_str().unwrap();
        let made = make_resume_token(&expected(&case)).unwrap();
        let theirs = token_of(&case);
        assert_eq!(made.get_str("_data"), theirs.get_str("_data"), "{name}");
        assert_eq!(made.get("_typeBits"), theirs.get("_typeBits"), "{name}");
    }
}

#[test]
fn the_cases_cover_every_kind_of_token() {
    let cases = cases();
    let any = |f: &dyn Fn(&ResumeTokenData) -> bool| cases.iter().any(|c| f(&expected(c)));
    assert!(any(&|t| t.token_type == TOKEN_TYPE_HIGH_WATER_MARK));
    assert!(any(&|t| t.token_type == TOKEN_TYPE_EVENT && t.op_index == 2));
    assert!(any(&|t| t.from_invalidate && t.uuid.is_some()));
    assert!(any(&|t| t.from_invalidate && t.uuid.is_none()));
    assert!(any(&|t| t.fragment == Some(1)));
    assert!(any(
        &|t| t.uuid.is_none() && t.token_type == TOKEN_TYPE_EVENT
    ));
    // Type bits: int32, int64 and double `_id`s.
    let typebits: Vec<_> = cases
        .iter()
        .filter_map(|c| c["typeBits"].as_str())
        .collect();
    assert!(typebits.contains(&"820001") && typebits.contains(&"828000"));
}

#[test]
fn a_high_water_mark_is_a_time_and_nothing_else() {
    let t = make_high_water_mark_token((1795200273i64 << 32) | 5).unwrap();
    assert_eq!(
        t.get_str("_data").unwrap(),
        "826B009511".to_string() + "00000005" + "2B0429296E1404"
    );
    assert!(!t.contains_key("_typeBits"));
    let p = parse_resume_token(&t).unwrap();
    assert!(!p.is_event());
    assert_eq!(p.position(), (1795200273i64 << 32) | 5);
}

#[test]
fn a_position_is_a_timestamp() {
    let ts = Timestamp {
        time: 1795200273,
        increment: 9,
    };
    assert_eq!(position_ts(ts_position(ts)), ts);
}

#[test]
fn things_that_are_not_version_2_tokens_are_refused() {
    for data in ["", "00", "826B00951100000005"] {
        let mut t = Document::new();
        t.insert("_data", data);
        assert!(parse_resume_token(&t).is_err(), "{data:?}");
    }
    // The old encoding: the hex of a BSON document.
    let mut old = Document::new();
    old.insert("_data", "1E000000127300010000000000000000");
    assert!(parse_resume_token(&old).is_err());
}
