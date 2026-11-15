### An operator's type error no longer jumps ahead of a missing column to its left

In a SELECT with no FROM, `select nosuch + (1 + true)` answered 42883
`operator does not exist: integer + boolean`, because the operator-type
check ran over the whole statement before any column reference was looked
at. PostgreSQL analyses an operator's operands before it looks the operator
up, so the missing column to its left fails first: 42703 `column "nosuch"
does not exist` at 8. The FROM-less reference walk now resolves each
operator with no reference under it as it meets it, and runs ahead of the
operator-type check, so the first error in parse-analysis order is the one
reported, whichever side of the operator it is on.

#### Fixed

- Rust PG server: in a FROM-less SELECT, a missing column to an operator's
  left is reported before the operator's 42883.
  Fixes RP-447 (tests/backlog/test_rp_fromless_analysis.py::test_missing_column_before_operator_to_its_right)
