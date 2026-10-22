### Rust MongoDB server: query operators check and match as mongod does (RM-259 to RM-267)

A line-coverage run found about 480 lines of the query matcher that no test
executed. A probe written against them (`tools/probes/query_shapes.py`)
compared 626 operator cases with mongod 8.2.11. 66 matched different
documents, 37 failed with a different code and 32 with a different message.
Now none match differently. Five `$where` cases still give a different
code: they are refused as a `--noscripting` mongod refuses them, as before.
The probe's 287 `$jsonSchema` cases are filed as RM-268 and RM-269.

#### Fixed

- A bit-position list (`{$bitsAllSet: [0, 5]}`) is int32 positions,
  refused as mongod refuses them. A position of 2^31 or more used to size a
  byte buffer to it, so a query could hang the server or exhaust its
  memory. A whole Decimal128 is accepted as a mask or a position. Fixes
  RM-259 (tests/backlog/test_rm_query_shapes.py::test_bit_positions).
- `$mod` checks its arguments in mongod's order, with its messages, and
  truncates the divisor and the remainder. A NaN or infinite argument is
  refused rather than matching nothing, and `$mod: [-1, 0]` matches -2^63.
  Fixes RM-260 (tests/backlog/test_rm_query_shapes.py::test_mod_arguments).
- `$type` accepts a whole Decimal128 code and shows a bad numeric one as
  mongod does. `$size` refuses a length outside int32. Fixes RM-261
  (tests/backlog/test_rm_query_shapes.py::test_type_and_size_arguments).
- Regex options are checked as mongod checks them: the flags `imsxu` only,
  `$options` a string and only beside a `$regex`, never both a flagged
  Regex and `$options`, and no NUL in a pattern. A Symbol is matched as a
  string, and `$eq` of a Symbol no longer fails the query. Fixes RM-262
  (tests/backlog/test_rm_query_shapes.py::test_regex_options).
- In a path, only `0` or digits without a leading zero index an array
  (`a.00` is a field name). An empty component re-reads an array only at
  the end of the path (`a.`), and is the field `""` in the middle (`a..b`).
  Fixes RM-263 (tests/backlog/test_rm_query_shapes.py::test_path_components).
- An expression's field path through an array reads the rest of the path
  from the elements that are documents, as mongod does: `"$a.b"` over
  `[{b: 1}, 5]` is `[1]`, and a numeric component is a field name. A
  missing value is never `$in` a list. Fixes RM-264
  (tests/backlog/test_rm_query_shapes.py::test_expr_field_paths).
- A document is a set of operators only when its first key is one.
  `$elemMatch` matches an element that is an array as the document
  `{"0": ..., "1": ...}`, and refuses `$expr`, `$where` and `$text`.
  `$all` is decided by its first element. Fixes RM-265
  (tests/backlog/test_rm_query_shapes.py::test_elem_match_all_and_operator_documents).
- `$sampleRate` is supported. `$comment` and `$maxDistance` as field
  operators, a non-string `$where`, and `undefined` as an operand are
  refused with mongod's errors. `$alwaysTrue` takes a Decimal128 1. Fixes
  RM-266 (tests/backlog/test_rm_query_shapes.py::test_operator_refusals).
- A parse error in a `$match` filter is sent bare, as `find` sends it,
  not under "Executor error during aggregate command". `$text` in a
  `$match` that is not the first stage is refused (17313). Fixes RM-267
  (tests/backlog/test_rm_query_shapes.py::test_match_stage_parse_errors).
