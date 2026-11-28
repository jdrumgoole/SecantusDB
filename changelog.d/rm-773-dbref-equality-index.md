### An equality to a DBRef reads the index

`{a: DBRef("c", 1)}` is an equality to the document `{$ref: "c", $id: 1}`,
and the Rust MongoDB server now plans it as mongod 8.2 does: a FETCH over an
IXSCAN of an index on `a`, with point bounds on the DBRef.

#### Fixed

- The query parser read a DBRef's `$ref` as an operator and planned a
  collection scan; an object whose first field is `$ref`, `$id` or `$db` is
  now a value, as in mongod's `isExpression` test.

Fixes RM-773 (tests/backlog/test_rm_found_rm732.py::test_an_equality_to_a_dbref_reads_the_index)
