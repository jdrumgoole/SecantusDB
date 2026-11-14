### A field's domain error in a record or array literal points at the literal

When a string literal is cast to a composite or array type, PostgreSQL reads
it with `record_in` or `array_in`, which check each field or element of a
domain type as they read it. An error from that check is reported at the
literal, so `select '(0,x)'::ctp`, with `ctp` a composite holding a
`CHECK (value > 0)` domain field, fails with 23514 at position 8. The Rust
PostgreSQL server reported the same error with no position. It now reports
it at the literal for every form of the cast. A cast straight to the domain
(`'0'::pint`) still has no position, because PostgreSQL checks that one after
reading the value.

#### Fixed

- Rust PostgreSQL server: a domain error (23514, 23502) from a field or
  element of an untyped string literal cast to a non-domain type is at the
  literal: `'(0,x)'::ctp`, `cast('(0,x)' as ctp)`, `'{1,0}'::pint[]`,
  `'{"(0,x)"}'::ctp[]`, and the same casts nested or in a subquery
  (PostgreSQL 15.19). Fixes RP-552
  (tests/backlog/test_rp_found_rp529.py::test_record_literal_domain_error_position;
  more forms in `test_literal_input_domain_error_position`).
- Filed while working it: RP-945 (an assigned literal and an `ARRAY[]`
  element still have no position), RP-946 (`'{NULL}'::nnint[]` is
  accepted), RP-947 (`c = '(0,x)'` over a composite column reads the
  literal as the column's type, where PostgreSQL refuses an anonymous
  record) and RP-948 (under `PREPARE` a recorded error location is
  counted in the inner statement). Gap tests in
  `tests/backlog/test_rp_found_rp552.py`.
