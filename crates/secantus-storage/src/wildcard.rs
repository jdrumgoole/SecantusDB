//! A wildcard index's entries as mongod 8.2.11 writes them
//! (`WildcardKeyGenerator`, measured 2026-11-23 by reading the `index-<UUID>`
//! tables of a store mongod wrote).
//!
//! A leaf path of a document under the index becomes one key of the fields
//! before the wildcard one, then `$_path` (the dotted path as a string, never
//! collated, always ascending), then the value under it (in the index's
//! direction). Leaves are scalars, empty objects (`{}` is the value),
//! an empty array (`undefined`) and an array nested in an array (the array
//! whole); objects are walked into and an array's elements are walked with
//! the array's path (no index in it). An array also puts a *multikey
//! metadata* key in the table, for the path it is at: the same fields, but
//! `MinKey` for those before the wildcard field, `1` for `$_path` and the
//! path as the value, under a RecordId of its own that no document has.
//! mongod never takes one away.
//!
//! The projection mongod applies is the index's `wildcardProjection`; with
//! none, a `$**` index leaves out `_id`, and `<path>.$**` is the projection
//! `{<path>: 1, _id: 0}`. An exclusion projection that does not name `_id`
//! leaves it out as well.

use std::collections::{BTreeMap, BTreeSet};

use bson::{Bson, Document};

/// The RecordId every multikey metadata key of a wildcard index ends in
/// (`RecordId::reservedIdFor(kWildcardMultikeyMetadataId)`), as
/// `Builder::append_record_id_long` writes it: measured on mongod 8.2.11.
pub(crate) const METADATA_RID_BYTES: [u8; 9] =
    [0xe3, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xff];

/// The RecordId value of [`METADATA_RID_BYTES`].
pub(crate) fn metadata_rid() -> i64 {
    static RID: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *RID.get_or_init(|| {
        secantus_core::keystring::decode_record_id_long(&METADATA_RID_BYTES)
            .map(|(rid, _)| rid)
            .unwrap_or(i64::MAX)
    })
}

/// Is `key_spec` a wildcard index's: a field `$**` or `<path>.$**`?
pub(crate) fn is_wildcard_spec(key_spec: &Document) -> bool {
    key_spec.keys().any(|f| f == "$**" || f.ends_with(".$**"))
}

#[derive(Clone, Debug)]
enum Tree {
    /// The whole subtree (`1` in an inclusion, `0` in an exclusion).
    Leaf,
    Branch(BTreeMap<String, Tree>),
}

fn insert(map: &mut BTreeMap<String, Tree>, path: &[&str]) {
    match path {
        [] => {}
        [last] => {
            map.insert((*last).to_string(), Tree::Leaf);
        }
        [first, rest @ ..] => {
            let entry = map
                .entry((*first).to_string())
                .or_insert_with(|| Tree::Branch(BTreeMap::new()));
            if let Tree::Branch(inner) = entry {
                insert(inner, rest);
            }
        }
    }
}

/// What the projection says of the subtree at a path.
#[derive(Clone, Copy)]
enum Scope<'a> {
    /// Everything below.
    All,
    /// Only the listed children (an inclusion).
    Only(&'a BTreeMap<String, Tree>),
    /// Everything but the listed children (an exclusion).
    Except(&'a BTreeMap<String, Tree>),
}

impl<'a> Scope<'a> {
    fn child(self, name: &str) -> Option<Scope<'a>> {
        match self {
            Scope::All => Some(Scope::All),
            Scope::Only(map) => match map.get(name)? {
                Tree::Leaf => Some(Scope::All),
                Tree::Branch(inner) => Some(Scope::Only(inner)),
            },
            Scope::Except(map) => match map.get(name) {
                None => Some(Scope::All),
                Some(Tree::Leaf) => None,
                Some(Tree::Branch(inner)) => Some(Scope::Except(inner)),
            },
        }
    }

    /// May a scalar (or an empty object) be keyed here?
    fn keeps_value(self) -> bool {
        !matches!(self, Scope::Only(_))
    }
}

/// A wildcard index's projection.
#[derive(Clone, Debug)]
pub(crate) struct Projection {
    inclusion: bool,
    tree: BTreeMap<String, Tree>,
}

impl Projection {
    /// The projection of a wildcard index on `field` (`$**` or `<path>.$**`)
    /// with the index's `wildcardProjection`, if it has one.
    pub fn new(field: &str, spec: Option<&Document>) -> Projection {
        let mut tree = BTreeMap::new();
        if let Some(root) = field.strip_suffix(".$**") {
            let parts: Vec<&str> = root.split('.').collect();
            insert(&mut tree, &parts);
            return Projection {
                inclusion: true,
                tree,
            };
        }
        let spec = spec.filter(|d| !d.is_empty());
        let on = |v: &Bson| match v {
            Bson::Boolean(b) => *b,
            Bson::Int32(n) => *n != 0,
            Bson::Int64(n) => *n != 0,
            Bson::Double(n) => *n != 0.0,
            _ => true,
        };
        let Some(spec) = spec else {
            insert(&mut tree, &["_id"]);
            return Projection {
                inclusion: false,
                tree,
            };
        };
        // `_id` may be named on either side of an inclusion.
        let inclusion = spec.iter().any(|(k, v)| k != "_id" && on(v))
            || (spec.len() == 1 && spec.get("_id").is_some_and(on));
        for (path, v) in spec {
            let parts: Vec<&str> = path.split('.').collect();
            if inclusion == on(v) {
                insert(&mut tree, &parts);
            }
        }
        if !inclusion && !spec.contains_key("_id") {
            insert(&mut tree, &["_id"]);
        }
        Projection { inclusion, tree }
    }

    fn scope(&self) -> Scope<'_> {
        if self.inclusion {
            Scope::Only(&self.tree)
        } else {
            Scope::Except(&self.tree)
        }
    }
}

/// What a document holds under the index.
#[derive(Default, Debug)]
pub(crate) struct Leaves {
    /// `(path, value)` for each key, in the order met.
    pub keys: Vec<(String, Bson)>,
    /// The paths of the arrays met.
    pub multikey: BTreeSet<String>,
}

/// The keys `doc` holds under a wildcard index with `projection`.
pub(crate) fn leaves(doc: &Document, projection: &Projection) -> Leaves {
    let mut out = Leaves::default();
    let mut path: Vec<String> = Vec::new();
    walk_document(doc, projection.scope(), &mut path, &mut out);
    out
}

fn walk_document(d: &Document, scope: Scope<'_>, path: &mut Vec<String>, out: &mut Leaves) {
    for (name, v) in d {
        let Some(inner) = scope.child(name) else {
            continue;
        };
        path.push(name.clone());
        walk_value(v, inner, path, out);
        path.pop();
    }
}

fn walk_value(v: &Bson, scope: Scope<'_>, path: &mut Vec<String>, out: &mut Leaves) {
    match v {
        Bson::Document(d) if d.is_empty() => {
            if scope.keeps_value() {
                out.keys.push((path.join("."), v.clone()));
            }
        }
        Bson::Document(d) => walk_document(d, scope, path, out),
        Bson::Array(items) => {
            out.multikey.insert(path.join("."));
            if items.is_empty() {
                if scope.keeps_value() {
                    out.keys.push((path.join("."), Bson::Undefined));
                }
                return;
            }
            for item in items {
                match item {
                    Bson::Document(d) if d.is_empty() => {
                        if scope.keeps_value() {
                            out.keys.push((path.join("."), item.clone()));
                        }
                    }
                    Bson::Document(d) => walk_document(d, scope, path, out),
                    other => {
                        if scope.keeps_value() {
                            out.keys.push((path.join("."), other.clone()));
                        }
                    }
                }
            }
        }
        other => {
            if scope.keeps_value() {
                out.keys.push((path.join("."), other.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;
    use secantus_core::keystring::{Builder, Ordering, Version};

    #[test]
    fn the_metadata_rid_is_the_one_mongod_wrote() {
        let mut b = Builder::new(Version::V1, Ordering::all_ascending());
        b.append_record_id_long(metadata_rid()).unwrap();
        let (key, _) = b.finish().unwrap();
        assert_eq!(key, METADATA_RID_BYTES);
    }

    fn found(
        field: &str,
        spec: Option<Document>,
        d: Document,
    ) -> (Vec<(String, Bson)>, Vec<String>) {
        let l = leaves(&d, &Projection::new(field, spec.as_ref()));
        (l.keys, l.multikey.into_iter().collect())
    }

    #[test]
    fn leaves_are_what_mongod_keys() {
        // Measured on mongod 8.2.11 (`$**`, 2026-11-23).
        let (keys, multikey) = found(
            "$**",
            None,
            doc! {"_id": 1, "a": [1, [2], {"b": 3}, {}], "c": {"d": {}}, "e": [], "f": null},
        );
        assert_eq!(
            keys,
            vec![
                ("a".to_string(), Bson::Int32(1)),
                ("a".to_string(), Bson::Array(vec![Bson::Int32(2)])),
                ("a.b".to_string(), Bson::Int32(3)),
                ("a".to_string(), Bson::Document(doc! {})),
                ("c.d".to_string(), Bson::Document(doc! {})),
                ("e".to_string(), Bson::Undefined),
                ("f".to_string(), Bson::Null),
            ]
        );
        assert_eq!(multikey, vec!["a".to_string(), "e".to_string()]);
    }

    #[test]
    fn a_projection_keeps_or_drops_paths() {
        let d = doc! {"_id": 1, "a": 1, "b": {"c": 2, "d": 3}};
        let (keys, _) = found("$**", Some(doc! {"b.c": 1}), d.clone());
        assert_eq!(keys, vec![("b.c".to_string(), Bson::Int32(2))]);
        let (keys, _) = found("$**", Some(doc! {"b": 0}), d.clone());
        assert_eq!(keys, vec![("a".to_string(), Bson::Int32(1))]);
        let (keys, _) = found("$**", Some(doc! {"_id": 1, "a": 1}), d.clone());
        assert_eq!(
            keys,
            vec![
                ("_id".to_string(), Bson::Int32(1)),
                ("a".to_string(), Bson::Int32(1))
            ]
        );
        let (keys, _) = found("b.$**", None, d);
        assert_eq!(
            keys,
            vec![
                ("b.c".to_string(), Bson::Int32(2)),
                ("b.d".to_string(), Bson::Int32(3))
            ]
        );
    }
}
