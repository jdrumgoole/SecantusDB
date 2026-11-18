### A JSON input error on a constant points at the literal

`select 't'::jsonb` now fails at the literal, as PostgreSQL places it: the
input function reads a constant while the statement is parsed, under the
literal's error position. The JSON input error quotes none of its input,
so the server used to find nothing to point at and sent no position.

#### Fixed

- A `json` / `jsonb` input error on an untyped literal cast or passed to a
  function (`'[1,'::json`, `json_typeof('x')`) carries the literal's
  position. A value cast through `text` first still has none, as on
  PostgreSQL.
- Fixes RP-952 (tests/backlog/test_rp_found_rp750.py::test_json_input_error_has_a_position)
