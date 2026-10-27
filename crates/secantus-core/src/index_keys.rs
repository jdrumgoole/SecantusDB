//! The values a document gives an index field: mongod's keys, and the
//! whole-array keys this store also writes (entry format 6).
//!
//! mongod's key generator, measured on 8.2.11 (2026-10-27, `returnKey` over
//! a hinted scan of every shape below):
//!
//! - a leaf value is its own key; a leaf ARRAY gives one key per element (an
//!   element that is itself an array is kept whole), and an EMPTY leaf array
//!   gives `undefined`, which sorts after MinKey and before null;
//! - a dotted path walks THROUGH an array of documents, one level: each
//!   element document continues the path. An element that does not reach the
//!   leaf -- a document without the field, or an element that is not a
//!   document (a nested array included) -- gives null, except in a SPARSE
//!   index, where it gives nothing;
//! - a document the path reaches nothing in is null (a sparse index leaves
//!   it out when no field of the index reaches anything).
//!
//! This store also writes the whole leaf array as a key of its own, which a
//! query for the array as a value probes; it is not one of mongod's keys,
//! and a cursor walking the index passes over it.

use bson::{Bson, Document};

use crate::get_path_values;

/// What one field of an index holds for a document.
pub struct FieldKeys {
    /// mongod's keys, in no particular order (repeats possible). Never empty:
    /// a field the document lacks is null.
    pub mongod: Vec<Bson>,
    /// The whole-array keys this store writes besides: each leaf array that
    /// holds at least one element.
    pub whole_arrays: Vec<Bson>,
    /// The field makes the index multikey: the path met an array.
    pub multikey: bool,
}

/// The keys `doc` gives an index field on `path`; `sparse` for a sparse
/// index (its elements that lack the leaf give no null).
pub fn field_keys(doc: &Document, path: &str, sparse: bool) -> FieldKeys {
    let (values, descended) = get_path_values(doc, path);
    let mut mongod: Vec<Bson> = Vec::new();
    let mut whole_arrays: Vec<Bson> = Vec::new();
    let mut multikey = descended;
    for v in &values {
        match v {
            Bson::Array(items) if items.is_empty() => {
                multikey = true;
                mongod.push(Bson::Undefined);
                whole_arrays.push((*v).clone());
            }
            Bson::Array(items) => {
                multikey = true;
                mongod.extend(items.iter().cloned());
                whole_arrays.push((*v).clone());
            }
            other => mongod.push((*other).clone()),
        }
    }
    if mongod.is_empty() {
        mongod.push(Bson::Null);
    } else if !sparse {
        let parts: Vec<&str> = path.split('.').collect();
        if element_lacks_leaf(&Bson::Document(doc.clone()), &parts, false) {
            mongod.push(Bson::Null);
        }
    }
    FieldKeys {
        mongod,
        whole_arrays,
        multikey,
    }
}

/// Whether the path `parts` passes through an array one of whose elements
/// does not reach the leaf: not a document, or a document without the field.
pub fn element_lacks_leaf(value: &Bson, parts: &[&str], in_array: bool) -> bool {
    let Some((first, rest)) = parts.split_first() else {
        return false;
    };
    match value {
        Bson::Document(d) => match d.get(*first) {
            Some(next) => element_lacks_leaf(next, rest, in_array),
            None => in_array,
        },
        Bson::Array(items) => items.iter().any(|item| match item {
            Bson::Document(_) => element_lacks_leaf(item, parts, true),
            _ => true,
        }),
        _ => in_array,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn keys(d: Document, path: &str, sparse: bool) -> Vec<Bson> {
        field_keys(&d, path, sparse).mongod
    }

    /// The shapes measured on mongod 8.2.11 (module notes).
    #[test]
    fn mongod_keys_for_awkward_shapes() {
        assert_eq!(keys(doc! {"a": []}, "a", false), vec![Bson::Undefined]);
        assert_eq!(keys(doc! {"a": []}, "a.b", false), vec![Bson::Null]);
        assert_eq!(keys(doc! {"a": [1]}, "a.b", false), vec![Bson::Null]);
        assert_eq!(
            keys(doc! {"a": [[{"b": 1}]]}, "a.b", false),
            vec![Bson::Null]
        );
        assert_eq!(
            keys(doc! {"a": [{"b": []}]}, "a.b", false),
            vec![Bson::Undefined]
        );
        assert_eq!(
            keys(doc! {"a": [{"c": 1}, {"b": 3}]}, "a.b", false),
            vec![Bson::Int32(3), Bson::Null]
        );
        assert_eq!(
            keys(doc! {"a": [{"c": 1}, {"b": 3}]}, "a.b", true),
            vec![Bson::Int32(3)]
        );
        assert_eq!(
            keys(doc! {"a": [{"b": [[]]}]}, "a.b", false),
            vec![Bson::Array(vec![])]
        );
        assert_eq!(keys(doc! {}, "a", false), vec![Bson::Null]);
        let t = field_keys(&doc! {"t": [1, [2]]}, "t", false);
        assert_eq!(
            t.mongod,
            vec![Bson::Int32(1), Bson::Array(vec![Bson::Int32(2)])]
        );
        assert_eq!(t.whole_arrays.len(), 1);
        assert!(t.multikey);
    }
}
