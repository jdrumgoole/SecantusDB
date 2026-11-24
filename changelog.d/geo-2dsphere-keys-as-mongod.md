### A `2dsphere` index holds the keys mongod 8.2 holds

A `2dsphere` index (version 3) is now keyed as `mongod` keys it: a point is one
S2 cell id at the finest level as a NumberLong, any other geometry its covering
between the index's `coarsestIndexedLevel` and `finestIndexedLevel` (at most
`maxCellsInCovering` cells), without the ancestors of each cell this server
used to add. The covering is `secantus_core::s2legacy`'s, `mongod`'s own, and
the index's other fields come beside the cell as any index keys them. The
index is multikey where a document holds more than one cell for the field (a
polygon, a `MultiPoint`) or an array in another field, and records that path in
the catalog, as `mongod` does: it was always flagged. A `$geoWithin` is served
by the intervals of cells `mongod`'s query covering gives. A `2dsphere` index of
a version other than 3 is refused at creation by name: its keys are strings of
another kind, which this server does not write. mongod 8.2.11 opens a store
this server wrote and `validate`s the index.

#### Fixed

- `secantus-storage`: a `2dsphere` index wrote a covering of its own (levels 4
  to 16, 64 cells, every ancestor of each cell, as BinData) and was always
  multikey; store format 11.

Fixes RM-742 (tests/backlog/test_rm_storage_model.py::test_mongod_validates_a_2dsphere_index_secantus_wrote)
