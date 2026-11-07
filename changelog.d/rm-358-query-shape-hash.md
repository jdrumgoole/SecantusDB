### Reads name their query shape, as mongod's do

The Rust MongoDB server now prints `queryShapeHash` on the `Slow query`
line, the profile entry and the `explain` of a `find`, a `distinct` and an
`aggregate`, with the value mongod 8.2 prints for the same command. The
hash is the SHA-256 of the query's representative shape: the command as
mongod parses it, with every literal replaced by a stand-in of its type.
Grouping slow queries by shape now works the same way against both
servers.

The hash comes from a port of mongod's own shape serializers. The filter
is shaped as mongod parses it, before any optimization, and each literal
becomes mongod's representative value. Projection, sort, options,
collation and `let` are included, and pipeline stages are shaped as the
stages mongod parses them into: `$count`, for example, becomes a `$group`
followed by a `$project`. `find` and `distinct` hash a buffer laid out as
mongod lays it out. `aggregate` hashes its shape's BSON. A query that uses
an operator or stage the port does not cover yet prints no hash, rather
than a wrong one.

#### Fixed

- `secantus-commands` `query_shape.rs`: mongod 8.2's `queryShapeHash` for
  `find`, `distinct` and `aggregate`. The `Slow query` line and profile
  entry of those commands, and of an `explain` of them, carry it, and so
  does `explain`'s reply (after `queryPlanner`, `executionStats` or
  `stages`, as on mongod). A query that parsed but then failed carries it
  too. A read of `admin`, `local`, `config` or a `system.` collection
  carries none, as on mongod.
- Fixes RM-358 in part
  (tests/backlog/test_rm_355_364.py::test_a_read_names_its_query_shape,
  tests/mongod_live/test_query_shape_hash.py); geo and text predicates,
  the aggregation operators and stages with serializers of their own, and
  `aggregate: 1` stay open on
  tests/backlog/test_rm_355_364.py::test_a_query_of_every_kind_names_its_query_shape.
