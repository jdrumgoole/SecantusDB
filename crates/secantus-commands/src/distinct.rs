//! The `distinct` command — distinct values of a field across matching docs.
//!
//! Port of `commands.py::_distinct`: fetch the matching documents, resolve the
//! dotted `key` path on each (flattening one array level, as mongod does), and
//! collect the distinct values. A command `collation` applies to both the query
//! filter (collation-aware COLLSCAN) and the value dedup (collation-equal strings
//! collapse to one).

use bson::{doc, Bson, Document};
use secantus_core::collation::Collation;
use secantus_core::get_path;

use crate::util::{coll_arg, collation_of, command_error, doc_field};
use crate::{CommandContext, CommandError, HandlerResult};

/// `distinct` — return the distinct values of `key` over docs matching `query`.
pub fn distinct(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "distinct")?;
    let key = match doc.get("key") {
        Some(Bson::String(s)) => s.clone(),
        _ => {
            return Ok(
                CommandError::new(14, "TypeMismatch", "distinct key must be a string").into_reply(),
            )
        }
    };
    let filter = doc_field(doc, "query");
    let collation = collation_of(doc);
    let storage = ctx.storage()?;
    let bytes = storage
        .find_collated(
            &ctx.db_name,
            &coll,
            &filter,
            None,
            None,
            collation.as_ref(),
            &Document::new(),
        )
        .map_err(command_error)?;

    let mut values: Vec<Bson> = Vec::new();
    for b in bytes {
        let d = Document::from_reader(&mut b.as_slice())
            .map_err(|e| CommandError::new(1, "InternalError", format!("decode: {e}")))?;
        match get_path(&d, &key) {
            // An array value contributes each of its elements (one level).
            Some(Bson::Array(arr)) => {
                for e in arr {
                    push_distinct(&mut values, e, collation.as_ref());
                }
            }
            Some(v) => push_distinct(&mut values, v, collation.as_ref()),
            None => {}
        }
    }
    Ok(doc! { "values": values, "ok": 1.0 })
}

/// Add `v` if no existing value is equal — collation-aware for strings (two
/// collation-equal strings collapse to one). Falls back to BSON equality when
/// no collation, or when the collation can't compare the pair (non-ASCII).
fn push_distinct(values: &mut Vec<Bson>, v: &Bson, collation: Option<&Collation>) {
    let dup = values.iter().any(|e| match (collation, e, v) {
        (Some(c), Bson::String(a), Bson::String(b)) => {
            secantus_core::collation::equal(a, b, c).unwrap_or(a == b)
        }
        _ => e == v,
    });
    if !dup {
        values.push(v.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;
    use crate::storage::{RawHint, Storage, StorageError, UpdateOutcome};
    use std::sync::{Arc, Mutex};

    fn matches(d: &Document, filter: &Document) -> bool {
        filter.iter().all(|(k, v)| d.get(k) == Some(v))
    }

    #[derive(Default)]
    struct FakeStorage {
        docs: Mutex<Vec<Document>>,
    }

    impl FakeStorage {
        fn with(docs: Vec<Document>) -> Arc<FakeStorage> {
            Arc::new(FakeStorage {
                docs: Mutex::new(docs),
            })
        }
    }

    impl Storage for FakeStorage {
        fn insert(
            &self,
            _: &str,
            _: &str,
            _: Vec<Vec<u8>>,
            _: bool,
        ) -> Result<(usize, Vec<Document>), StorageError> {
            Ok((0, vec![]))
        }
        fn update_matching(
            &self,
            _: &str,
            _: &str,
            _: &Document,
            _: &Document,
            _: bool,
            _: bool,
        ) -> Result<UpdateOutcome, StorageError> {
            Ok(UpdateOutcome::default())
        }
        fn delete_matching(
            &self,
            _: &str,
            _: &str,
            _: &Document,
            _: usize,
        ) -> Result<usize, StorageError> {
            Ok(0)
        }
        fn count_matching(&self, _: &str, _: &str, _: &Document) -> Result<usize, StorageError> {
            Ok(0)
        }
        fn find(
            &self,
            _: &str,
            _: &str,
            filter: &Document,
            _: Option<&Document>,
            _: Option<RawHint<'_>>,
        ) -> Result<Vec<Vec<u8>>, StorageError> {
            Ok(self
                .docs
                .lock()
                .unwrap()
                .iter()
                .filter(|d| matches(d, filter))
                .map(|d| {
                    let mut v = Vec::new();
                    d.to_writer(&mut v).unwrap();
                    v
                })
                .collect())
        }
    }

    fn ctx(storage: Arc<FakeStorage>) -> CommandContext {
        let mut c = CommandContext::new(1).with_storage(storage);
        c.db_name = "t".into();
        c
    }

    fn values(reply: &Document) -> Vec<Bson> {
        reply.get_array("values").unwrap().clone()
    }

    #[test]
    fn distinct_scalar_field() {
        let s = FakeStorage::with(vec![
            doc! {"x": 1},
            doc! {"x": 2},
            doc! {"x": 1},
            doc! {"x": 3},
        ]);
        let mut c = ctx(s);
        let reply = dispatch(&doc! {"distinct": "c", "key": "x"}, &mut c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        let mut v = values(&reply);
        v.sort_by_key(|b| b.as_i32().unwrap());
        assert_eq!(v, vec![Bson::Int32(1), Bson::Int32(2), Bson::Int32(3)]);
    }

    #[test]
    fn distinct_flattens_arrays() {
        let s = FakeStorage::with(vec![doc! {"tags": ["a", "b"]}, doc! {"tags": ["b", "c"]}]);
        let mut c = ctx(s);
        let reply = dispatch(&doc! {"distinct": "c", "key": "tags"}, &mut c);
        let mut v: Vec<String> = values(&reply)
            .iter()
            .map(|b| b.as_str().unwrap().to_string())
            .collect();
        v.sort();
        assert_eq!(v, vec!["a", "b", "c"]);
    }

    #[test]
    fn distinct_with_query_filter() {
        let s = FakeStorage::with(vec![
            doc! {"g": "a", "x": 1},
            doc! {"g": "a", "x": 2},
            doc! {"g": "b", "x": 9},
        ]);
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"distinct": "c", "key": "x", "query": {"g": "a"}},
            &mut c,
        );
        let mut v = values(&reply);
        v.sort_by_key(|b| b.as_i32().unwrap());
        assert_eq!(v, vec![Bson::Int32(1), Bson::Int32(2)]);
    }

    #[test]
    fn distinct_dotted_key() {
        let s = FakeStorage::with(vec![doc! {"a": {"b": 1}}, doc! {"a": {"b": 2}}]);
        let mut c = ctx(s);
        let reply = dispatch(&doc! {"distinct": "c", "key": "a.b"}, &mut c);
        assert_eq!(values(&reply).len(), 2);
    }

    #[test]
    fn distinct_non_string_key_is_type_mismatch() {
        let s = FakeStorage::with(vec![]);
        let mut c = ctx(s);
        let reply = dispatch(&doc! {"distinct": "c", "key": 1}, &mut c);
        assert_eq!(reply.get_i32("code").unwrap(), 14);
        assert_eq!(reply.get_str("codeName").unwrap(), "TypeMismatch");
    }
}
