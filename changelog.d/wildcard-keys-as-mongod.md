### A wildcard index holds the keys mongod 8.2 holds

A wildcard index (`$**` or `<path>.$**`, with or without a `wildcardProjection`,
alone or after other fields) now has an entry for each leaf path of a document
as `mongod`'s `WildcardKeyGenerator` makes it: the fields before `$**`, then
`$_path` (the dotted path, always ascending and never collated), then the
value under it in the index's direction. Scalars, an empty object, `undefined`
for an empty array and an array nested in an array are the leaves; arrays are
walked through without an index in the path. An array also writes a multikey
metadata key (`MinKey` for the other fields, `1`, the path) under the RecordId
mongod reserves for them, which no write removes, and makes the index multikey
in the catalog, with no paths. Until now it held one null entry per document.
mongod 8.2.11 opens a store this server wrote and `validate`s the index.

#### Fixed

- `secantus-storage`: a wildcard index wrote a null entry per document and was
  never multikey; store format 11.

Fixes RM-743 (tests/backlog/test_rm_storage_model.py::test_mongod_validates_a_wildcard_index_secantus_wrote)
