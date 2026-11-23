### A nested collectionless `$unionWith` is refused as mongod does

A `$unionWith` with no `coll` whose pipeline does not begin with
`$documents` is now refused wherever it stands, inside a `$facet`,
`$lookup` or `$unionWith` too, as mongod 8.2.11 does.

#### Fixed

- Nested, the stage ran its pipeline over the outer collection and unioned
  those documents in; it now answers 9 "$unionWith stage without explicit
  collection must have a pipeline with $documents as first stage".

Fixes RM-648 (tests/backlog/test_rm_found_2026_11_14_rm530.py::test_a_nested_union_with_no_collection_must_make_its_documents)
