### A row beside a scalar reports a missing column first

A row constructor compared with a value that is not a row now reports a
missing column in either operand before it reports that no operator takes the
two, as PostgreSQL does: `(1, nosuch) = (1)` and `(1, nosuch) in (1, 2)` are
42703 `column "nosuch" does not exist`, not 42883 `operator does not exist:
record = integer`.

#### Fixed

- Rust PG server: `transformAExprOp` / `transformAExprIn` order for a row
  beside a non-row operand; every `IN` item is transformed before any
  operator is looked up.

Fixes RP-875 (tests/backlog/test_rp_found_rp849.py::test_a_row_beside_a_non_row_transforms_its_operands_first)
