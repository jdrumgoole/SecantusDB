### An aggregate without a pipeline is refused, as mongod refuses it

`pipeline` is a required field of `aggregate`. The Rust MongoDB server used
to ask for a cursor when it was missing, and, given a cursor, ran an empty
pipeline and answered ok. It now answers 40414 "BSON field
'aggregate.pipeline' is missing but a required field", as mongod 8.2.11
does. A malformed aggregate now also gets the same error mongod finds first:
the server checks the command's fields in the order they appear, then the
missing pipeline, then the collection name, and only then the missing
cursor. It used to report the missing cursor before almost everything else.

#### Fixed

- `aggregate` without `pipeline` is IDLFailedToParse (40414), with or
  without `cursor`, and for `aggregate: 1` too. It used to be
  FailedToParse (9), or, given a cursor, an ok over an empty pipeline.
- `aggregate()` checks its own fields in one pass, in document order, so the
  first bad field wins: `{let: 5, bogus: 1}` is 14 and `{bogus: 1, let: 5}`
  is 40415, as on mongod. A non-object `pipeline` element is now caught in
  that pass. After the pass come, in this order, the missing `pipeline`
  (40414), the namespace, and the missing `cursor` (9). The cursor check
  used to run first, ahead of every type check.
- Fixes RM-431 (tests/backlog/test_rm_431_432.py::test_an_aggregate_without_a_pipeline_is_refused,
  tests/backlog/test_rm_431_432.py::test_aggregate_checks_its_fields_in_mongods_order).
- Filed while working it: RM-463 (`writeConcern` and
  `bypassDocumentValidation` are not type-checked), RM-464 (a non-string,
  non-number namespace is 2, not 73) and RM-465 (the type lists in
  wrong-type errors are in a different order from mongod's).
