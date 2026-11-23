### A nested `$lookup` spec is checked as it is at the top level

A `$lookup` inside a `$facet` or `$unionWith` pipeline whose `let` is not a
document, or that names `localField` without `foreignField`, is now refused
as mongod 8.2.11 refuses it (14 and 9), as the top level already was.

#### Fixed

- A nested `$lookup` with a non-document `let` ran; one with a lone
  `localField` answered 2 "$lookup requires localField+foreignField or
  pipeline".

Fixes RM-649 (tests/backlog/test_rm_found_2026_11_14_rm530.py::test_a_nested_lookup_spec_is_checked_as_it_is_parsed)
