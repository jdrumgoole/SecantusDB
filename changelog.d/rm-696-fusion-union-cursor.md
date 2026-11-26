### A fusion input pipeline's `$unionWith` explains its `$cursor`

`explain` of a `$rankFusion` or `$scoreFusion` now lists the `$unionWith`
each later input pipeline runs in as mongod 8.2.11 does: a `$cursor` holding
the query plan of the stages the user wrote, then the stages the fusion
desugars into.

#### Fixed

- The `$unionWith` a fusion input pipeline runs in listed every stage
  desugared with no `$cursor`.
  Fixes RM-696 (tests/backlog/test_rm_found_2026_11_15_rm426.py::test_a_fusion_input_union_opens_with_its_cursor, tests/backlog/test_rm_found_2026_11_15_rm426.py::test_a_fusion_input_unions_cursor_is_mongods)
