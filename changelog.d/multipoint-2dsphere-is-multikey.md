### A 2dsphere index a MultiPoint gives several keys is multikey

The catalog entry of a `2dsphere` index now records the field as multikey when a
document holds more than one cell for it (a `MultiPoint`, a polygon), with the
field's path, as `mongod` does; each ring scan of a `$near` over such an index
reports `isMultiKey: true` and `multiKeyPaths: {loc: ["loc"]}`. It came with the
`2dsphere` keys of store format 11.

#### Fixed

- `secantus-storage`: the multikey flag and paths of a `2dsphere` index follow
  its keys.

Fixes RM-712 (tests/backlog/test_rm_found_rm707.py::test_a_multipoint_2dsphere_near_ring_scan_is_multikey)
