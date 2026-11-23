### A `$documents` in a sub-pipeline is refused as mongod refuses it

A `$documents` past the first stage of a `$lookup` or `$unionWith` pipeline
that reads a collection now answers 40602, and an empty `{$documents: {}}`
carries mongod's "Invalid $documents :: caused by ::" envelope.

#### Fixed

- `$documents` not first under a collection answered 73 ("can only be run
  with {aggregate: 1}") instead of mongod 8.2.11's 40602, and the 51270 for
  an empty spec dropped its "Invalid $documents" envelope.

Fixes RM-651 (tests/backlog/test_rm_found_2026_11_14_rm530.py::test_documents_refusals_in_a_sub_pipeline)
