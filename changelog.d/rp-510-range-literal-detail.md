### Rust PG server: a malformed range literal says what is wrong with it

A range literal is now read by a port of PostgreSQL's `range_parse`, so its
22P02 carries the same DETAIL, and a bound keeps its whitespace for the
element's input function to judge.

#### Fixed

- `select '[1,2'::int4range` and the other malformed range literals were
  22P02 `malformed range literal` with no DETAIL. Each now has
  PostgreSQL 15.19's: `Unexpected end of input.`, `Missing left
  parenthesis or bracket.`, `Missing comma after lower bound.`, `Too many
  commas.`, `Junk after right parenthesis or bracket.` and `Junk after
  "empty" key word.`. The message names the literal as written, leading
  whitespace included.
- Whitespace outside quotes was dropped from a bound, so `'[1 2,3)'::int4range`
  was `[12,3)` and `'[ ,2)'` an unbounded range. Both are now int4's
  `invalid input syntax`, as on PostgreSQL; `'[ 1 , 2 )'` is still `[1,2)`.
  A vertical tab counts as whitespace, as C `isspace` reads it.
- Fixes RP-510 (tests/backlog/test_rp_found_rp500.py::test_range_literal_detail).
  Regression tests: `range::tests` in `crates/secantus-pgplan/src/range.rs`.
- Filed RP-878: the same gap for a multirange literal, whose bad member is
  also reported as a range literal (tests/backlog/test_rp_found_rp510.py).
