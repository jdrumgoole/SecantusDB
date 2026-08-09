### A 2dsphere index reports its format version

MongoDB stamps every `2dsphere` index with the index format version it was
built at, and drivers read it back through `listIndexes` — the PHP library
exposes it as `IndexInfo::is2dSphere()` and `$index['2dsphereIndexVersion']`.
The Rust server left the field off entirely, so a client asking which 2dsphere
format an index used got no answer. It now reports version 3, matching both
the Python server and MongoDB 3.2 onwards. A `2d` index carries no such field
and still doesn't.

#### Fixed

- `listIndexes` reports `2dsphereIndexVersion` for a `2dsphere` index on the
  Rust server.
