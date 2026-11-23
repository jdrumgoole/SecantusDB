### An empty `$project` is refused with mongod's envelope

`{$project: {}}` now answers mongod 8.2.11's 51272 "Invalid $project ::
caused by :: projection specification must have at least one field", at
the top level and inside a `$facet`, `$lookup` or `$unionWith` pipeline.
The message lacked the "Invalid $project :: caused by ::" envelope, and a
sub-pipeline answered BadValue (2, "not supported by the Rust server")
until RM-645 checked sub-pipeline stage specs as the pipeline parses.

#### Fixed

Fixes RM-650 (tests/backlog/test_rm_found_2026_11_14_rm530.py::test_an_empty_projection_is_an_invalid_project)
