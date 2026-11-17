### A nested pipeline is parsed with the command

The Rust MongoDB server now reads the pipeline inside a `$facet`, a
`$lookup` and a `$unionWith` as mongod 8.2 does, as the command parses.

#### Fixed

- An unknown stage inside a nested pipeline is refused with 40324,
  "Unrecognized pipeline stage name", where it answered BadValue (2,
  "not supported by the Rust server"); a `$unionWith` whose `pipeline`
  is not an array is refused with 14, "A pipeline must be an array of
  objects", where it ran the union and answered ok.

Fixes RM-603 (tests/backlog/test_rm_found_rm386.py::test_a_nested_pipeline_is_parsed_with_the_command)
