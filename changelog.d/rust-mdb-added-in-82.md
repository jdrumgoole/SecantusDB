### The Rust MongoDB server has the expressions mongod 8.2 added, and refuses them at FCV 8.0

`$sigmoid` and the three vector-similarity expressions mongod 8.2 added,
`$similarityCosine`, `$similarityDotProduct` and `$similarityEuclidean`, were
unknown expressions on the Rust MongoDB server. They now answer as mongod
8.2.11 does, value for value, refusals included. `setFeatureCompatibilityVersion`
also stops being only a record: at FCV 8.0 the server refuses those four
expressions with mongod's 224, as mongod does.

#### Added

- `secantus-core` `expressions.rs`: `$sigmoid`, rewritten as mongod rewrites
  it when it parses it (`1 / (1 + e^(-x))` in `$divide` / `$add` / `$exp` /
  `$multiply`, which `explain` prints and whose errors a non-numeric argument
  gets); `$similarityCosine` / `$similarityDotProduct` /
  `$similarityEuclidean` in their array and `{vectors, score}` forms, with
  mongod's ten error codes (10413200-10413209). Fixes RM-197
  (tests/backlog/test_rm_added_in_82.py::test_expressions_82_added,
  ::test_sigmoid_is_explained_as_mongod_rewrites_it).

#### Fixed

- At FCV 8.0 the expressions 8.2 added are refused with 224
  QueryFeatureNotAllowed, in mongod's words; the version is cached in the
  server and read once from `admin.system.version`. Fixes RM-025
  (tests/backlog/test_rm_001_028.py::test_fcv_80_refuses_a_feature_82_added,
  tests/backlog/test_rm_added_in_82.py::test_fcv_80_refuses_every_expression_82_added).

- A resume token in mongod's own format that decodes is looked up from its
  cluster time and answers 280 (`the resume token was not found`) at the
  first event found, as on mongod; a `_typeBits` that is not BinData is 40648
  and all-zero type bits are no bits. RM-075 part
  (tests/backlog/test_rm_057_084.py::test_well_formed_mongod_token_is_looked_up,
  ::test_type_bits_of_wrong_type_or_all_zero).

#### Known gaps

- The hybrid-search stages 8.2 added (`$rankFusion`, `$score`,
  `$scoreFusion`) are still missing (RM-198).
- Type bits that change a number's type in a mongod-format token (RM-075).
