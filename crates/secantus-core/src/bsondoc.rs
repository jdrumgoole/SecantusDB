//! Decoding stored and received BSON into a [`Document`], literally.
//!
//! `Document::from_reader` goes through serde, and the `bson` crate's serde
//! visitor reads any sub-document that looks like extended JSON as the value
//! it would stand for there: `{a: {$numberLong: "1"}}` comes back as
//! `{a: Int64(1)}`, and `{$oid: "..."}`, `{$date: ...}`, `{$numberInt: ...}`
//! and the rest likewise. To a database those are documents a client stored.
//! Decoding one that way and writing it back replaced the sub-document with a
//! number, so an update of any OTHER field of such a document changed it
//! (measured against mongod 8.2.11, 2026-10-12, which keeps the sub-document).
//!
//! The raw-document conversion used here maps each element to the `Bson` of
//! its own type and nothing else.

use bson::{Document, RawDocument};

/// The document at the start of `bytes`. Trailing bytes are ignored, as they
/// are by a reader.
pub fn from_bytes(bytes: &[u8]) -> Result<Document, bson::raw::Error> {
    let declared = bytes
        .get(..4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n <= bytes.len());
    let exact = match declared {
        Some(n) => &bytes[..n],
        None => bytes,
    };
    Document::try_from(RawDocument::from_bytes(exact)?)
}

#[cfg(test)]
mod tests {
    use super::from_bytes;
    use bson::{doc, Bson, Document};

    fn bytes(d: &Document) -> Vec<u8> {
        let mut out = Vec::new();
        d.to_writer(&mut out).unwrap();
        out
    }

    /// Every extended-JSON wrapper, as a sub-document and inside an array.
    #[test]
    fn a_sub_document_that_looks_like_extended_json_stays_a_document() {
        let wrappers = [
            doc! { "$numberLong": "1" },
            doc! { "$numberInt": "1" },
            doc! { "$numberDouble": "1.5" },
            doc! { "$numberDecimal": "1.5" },
            doc! { "$oid": "0123456789abcdef01234567" },
            doc! { "$date": 5_i64 },
            doc! { "$date": { "$numberLong": "5" } },
            doc! { "$symbol": "s" },
            doc! { "$code": "f()" },
            doc! { "$timestamp": { "t": 1, "i": 2 } },
            doc! { "$regularExpression": { "pattern": "a", "options": "i" } },
            doc! { "$binary": { "base64": "AA==", "subType": "00" } },
            doc! { "$uuid": "00000000-0000-0000-0000-000000000000" },
            doc! { "$minKey": 1 },
            doc! { "$maxKey": 1 },
            doc! { "$undefined": true },
            doc! { "$dbPointer": { "$ref": "c", "$id": { "$oid": "0123456789abcdef01234567" } } },
        ];
        for wrapper in wrappers {
            let stored = doc! { "_id": 1, "a": wrapper.clone(), "l": [wrapper.clone()] };
            let back = from_bytes(&bytes(&stored)).unwrap();
            assert_eq!(
                back.get("a"),
                Some(&Bson::Document(wrapper.clone())),
                "{wrapper}"
            );
            assert_eq!(bytes(&back), bytes(&stored), "{wrapper}");
        }
    }

    #[test]
    fn typed_values_round_trip() {
        let stored = doc! {
            "_id": bson::oid::ObjectId::new(),
            "i": 1_i32,
            "l": 1_i64,
            "d": 1.5,
            "s": "x",
            "n": Bson::Null,
            "t": bson::Timestamp { time: 1, increment: 2 },
            "when": bson::DateTime::from_millis(5),
            "o": { "x": [1, { "y": true }] },
        };
        assert_eq!(from_bytes(&bytes(&stored)).unwrap(), stored);
    }

    #[test]
    fn trailing_bytes_are_ignored_and_a_short_buffer_is_refused() {
        let mut b = bytes(&doc! { "a": 1 });
        b.extend_from_slice(b"junk");
        assert_eq!(from_bytes(&b).unwrap(), doc! { "a": 1 });
        assert!(from_bytes(&b[..5]).is_err());
        assert!(from_bytes(&[]).is_err());
    }
}
