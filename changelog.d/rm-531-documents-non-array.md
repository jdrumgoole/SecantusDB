### Rust MongoDB server: `$documents` of a non-array is mongod's executor error

A `$documents` whose argument is not an array -- a number, a string, `null`,
a document, `$$ROOT`, `$$NOW` -- now fails at run time as mongod 8.2.11 does,
with 5858203 "an array is expected" under the executor prefix of the command
running it: `<db>.$cmd.aggregate` for a collectionless aggregate, the outer
collection inside a `$lookup` or `$unionWith`.

#### Fixed

- `$documents: 5` answered 2 "$documents argument must evaluate to an
  array"; it is now 5858203 "Executor error during aggregate command on
  namespace: <ns> :: caused by :: an array is expected".
  Fixes RM-531 (tests/backlog/test_rm_found_2026_11_10_lookup.py::test_documents_of_a_non_array_is_an_executor_error)
- Filed while working it: RM-653 (a non-document element answers 2, not
  40228), RM-654 (a failing `$documents` expression answers "expression not
  supported"), RM-655 (`$$NOW` unbound inside a `$unionWith` pipeline) and
  RM-656 (a run-time error in a sub-pipeline or after `$documents` names the
  wrong namespace).
