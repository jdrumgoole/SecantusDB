### Rust PG server: `int2vector` and `oidvector` input read as PostgreSQL's

The vector input functions are ported from PostgreSQL 15.19's
`int2vectorin` and `oidvectorin`, in place of a split on whitespace that
parsed each word whole.

#### Fixed

- An `int2vector` input error quotes the input from the failing number to
  its end, as `int2vectorin` does: `'70000 1'::int2vector` and
  `'1 70000 1'::int2vector` are 22003 `value "70000 1" is out of range for
  type smallint`, and `'1 x 2'::int2vector` is 22P02 for `"x 2"`. A number
  must be followed by a space or the end (`'1x 2'`, or a tab after a
  number, is 22P02), and a number past 64 bits is out of range rather than
  a syntax error.
- `oidvector` input takes `strtoul`'s answer, as `oidin_subr` does:
  `'-1'::oidvector` is `4294967295` (it was -1), `'1-2'::oidvector` is
  `1 4294967294` (it was refused), `'1x'::oidvector` is 22P02 for `"x"`,
  and a value past 32 bits is 22003 `out of range for type oid`.
- A vector literal's 22P02 is placed at the literal when the message
  quotes the rest of the input, inside an array literal too
  (`'{"1 x 2"}'::int2vector[]`).
- Fixes RP-605 (tests/backlog/test_rp_found_rp575.py::test_int2vector_range_error_quotes_the_rest),
  pinned with tests/backlog/test_rp_found_rp575.py::test_vector_input_reads_as_postgresql
  and the `vectors` unit tests in `secantus-pgplan`.
- Filed while working it: RP-866 (a literal's 22003 has no position) and
  RP-867 (an escape-string literal's input error has no position when an
  escape is in its value).
