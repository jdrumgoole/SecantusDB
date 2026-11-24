### A `2d` index holds the keys mongod 8.2 holds

A `2d` index is now keyed as `mongod` keys it: the point's GeoHash at the
index's `bits`, x then y interleaved in the high bits, as BinData of subtype 128
(`GeoHash::appendHashMin`), then the index's other fields. A field with no value
is a null, one with several (an array, or the elements along a path through
arrays) is the array of its distinct values in BSON order, as `get2DKeys` makes
it. A document with several locations is a key for each and makes the index
multikey (with no paths, as in `mongod`). mongod 8.2.11 opens a store this
server wrote and `validate`s the index. The store format is now 11: a store in
format 10 is refused by name, with the dump-and-restore advice, as the earlier
ones are.

#### Fixed

- `secantus-storage`: a `2d` index wrote one cell of its own (y first, BinData
  subtype 0) and ignored its trailing fields. The `$geoWithin` scan reads the
  same keys, through the geohash range between the query box's corners.
  `2dsphere` and wildcard indexes changed in the same release (the next two
  entries); store format 11.

Fixes RM-741 (tests/backlog/test_rm_storage_model.py::test_mongod_validates_a_2d_index_secantus_wrote)
