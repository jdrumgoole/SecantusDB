### A PL/pgSQL notice says where it was raised

A `RAISE NOTICE`, `WARNING` or `INFO` now carries the same CONTEXT
PostgreSQL gives it: the statement that raised it, then each calling
statement out to the top, such as `SQL statement "SELECT f()"` and
`PL/pgSQL function inline_code_block line 1 at PERFORM`. Before, only an
error said where it came from. Each function contributes only the
statement it is running, as PostgreSQL's error callback does.

#### Fixed

- Rust PG server: a RAISE below ERROR carries its CONTEXT, with its
  callers' frames.
  Fixes RP-833 (tests/backlog/test_rp_found_rp751.py::test_a_notice_carries_its_context,
  tests/backlog/test_rp_found_rp751.py::test_a_notice_carries_its_callers_frames)
