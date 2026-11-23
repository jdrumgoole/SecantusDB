### A malformed stage inside a sub-pipeline is refused as the pipeline parses

The Rust MongoDB server now checks the stages of a `$facet`'s, a `$lookup`'s
and a `$unionWith`'s pipelines while it checks the outer pipeline, as mongod
8.2 does, so a bad spec is refused even over an empty collection.

#### Fixed

- `{$facet: {a: [{$match: 5}]}}` answers 15959 instead of a generic BadValue,
  and a `$lookup` with a bad `let` in a `$facet`, or a lone `localField`
  inside a `$lookup` pipeline, is refused (14, 9) instead of running.

Fixes RM-645 (tests/backlog/test_rm_found_2026_11_14_rm528.py::test_stage_specs_in_a_sub_pipeline_are_checked_as_it_parses)
