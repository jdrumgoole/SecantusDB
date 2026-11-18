### A back reference under a case-insensitive match folds case

A pattern with a back reference matched case-insensitively, such as
`'ABab' ~* '(ab)\1'`, is now true, as PostgreSQL's regular expression
engine compares the referenced text under the pattern's case folding.
The backtracking engine that runs such patterns, `fancy-regex`, moves
from 0.14, which compared a back reference byte for byte even under
`(?i)`, to 0.20, in the planner and in `secantus-core`, so a table's
WHERE, the scalar operators and the `regexp_*` functions all agree.

#### Fixed

- `fancy-regex` 0.20 in `secantus-pgplan` and `secantus-core` (and every
  committed `Cargo.lock`); `regex-automata` moves to 0.4.18, which it
  needs. `secantus-core` is linked by the MongoDB server too: a `$regex`
  with a back reference under the `i` option now folds case there as
  well, as PCRE does on `mongod`.
- Fixes RP-1022 (tests/backlog/test_rp_found_rp423.py::test_a_case_insensitive_back_reference)
