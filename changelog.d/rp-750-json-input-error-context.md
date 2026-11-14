### Rust PG server: a JSON input error says which token failed, and where

When PostgreSQL rejects `json` or `jsonb` input, it says why in a DETAIL
line (`Token "t" is invalid.`, `Expected ":", but found "1".`). It also
shows where, in a `JSON data, line N:` CONTEXT line that quotes the failing
line up to the token, with `...` where it cuts the line. The Rust
PostgreSQL server sent only the bare `invalid input syntax for type json`,
so a client had nothing to locate the error by. In PL/pgSQL, the JSON line
was also missing from the top of the error's stack.

#### Fixed

- A `json` / `jsonb` input error (a cast, a function or operator argument,
  a PL/pgSQL variable) carries PostgreSQL 15's DETAIL and its `JSON data`
  CONTEXT line. Both come from a port of `jsonapi.c`'s parser and lexer
  and of `report_json_context`, which runs once the server's own parser
  has rejected the text. That covers every parse error `jsonapi.c` can
  report: an invalid token, a bad escape, an unescaped control character,
  a lone surrogate, input that ended early, and trailing garbage. The
  line number is counted, the excerpt is cut to its last 49 bytes, and
  `\u0000` into `jsonb` is still 22P05 `unsupported Unicode escape
  sequence`, now with `\u0000 cannot be converted to text.` beside it.
  Fixes RP-750 (tests/backlog/test_rp_found_rp712.py::test_jsonb_input_error_carries_its_json_data_context,
  tests/backlog/test_rp_found_rp712.py::test_json_input_error_names_its_token_and_line).
- Filed: RP-952 (a JSON input error on a constant has no statement
  position) and RP-953 (`jsonb_in` / `json_in` / `int4in` called by name
  are refused).
