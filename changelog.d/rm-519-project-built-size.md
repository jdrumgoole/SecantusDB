### The Rust MongoDB server sizes the documents an inclusion `$project` builds as mongod does

Behind a stage that changed its documents, such as an `$unwind`, mongod's
inclusion `$project` does not copy each document's BSON. It builds a new
document with room reserved for every field the projection names, counting
`_id` and any field the document lacks, then copies the fields it keeps and
sets the computed ones. The `$sort` or `$setWindowFields` after it counts
that storage. The Rust MongoDB server counted the BSON each projected
document would encode to, so `explain` reported 188700 bytes for a sort
where mongod 8.2 reports 351300, and the memory budget that sort spills
against counted too little.

#### Fixed

- `docstore::Inclusion` models mongod 8.2's `InclusionNode`. The output
  document is a `MutableDocument(n)` reserved for every field the spec
  names. A field it copies shares the input's value, which copying caches
  in the input: a value an earlier `$addFields` built counts as built, and
  a computed field reading inside a copied sub-document (`{e: 1, w:
  "$e.y"}`) grows the storage both share, as RM-515 models it. A dotted or nested
  sub-projection builds a document of its own, and an array is rebuilt
  element by element (nested arrays too), where an element it leaves out
  counts nothing. Computed fields are set last, in the spec's order, and a
  scalar under a computed sub-field becomes a document of the computed
  fields. A projection with nothing to compute takes a document no stage
  wrote to straight from its BSON into new BSON
  (`FastPathEligibleInclusionNode`).
- The pipeline carries each document's storage through an inclusion
  `$project` to the sorts and windows behind it, instead of dropping it.

Fixes RM-519 (tests/backlog/test_rm_found_rm408.py::test_a_document_a_project_built_is_sized_as_built).
The new `tests/mongod_live/test_project_storage.py` compares 28 projection
shapes and a window with mongod 8.2. Files RM-637 (an exclusion `$project`),
RM-638 (a `$project` behind a `$group` pushed into SBE) and RM-639 (a
`$sort` behind a window on its key that mongod drops).
