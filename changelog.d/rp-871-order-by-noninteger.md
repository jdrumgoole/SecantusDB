### An ORDER BY constant that is no integer is a syntax error

PostgreSQL reads a bare constant in ORDER BY as a select-list position and
refuses any other constant -- `order by 2.5`, `order by 'x'`, `order by
null` -- as 42601 `non-integer constant in ORDER BY` at the constant. The
Rust PostgreSQL server now does the same, after any error the FROM list,
select list, WHERE, HAVING or an earlier ORDER BY item raises, and ahead of
a GROUP BY constant's error, in the order `transformSelectStmt` analyses
the clauses. It used to refuse the first as an unsupported expression and
sort by the second as a value.

#### Fixed

- A non-integer ORDER BY constant is 42601 `non-integer constant in ORDER
  BY` at the constant, as `findTargetlistEntrySQL92` raises it.
  Fixes RP-871 (tests/backlog/test_rp_found_rp854.py::test_a_non_integer_order_by_constant_is_42601)
