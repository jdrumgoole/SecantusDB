### Rust PG server: a UNION side selecting a bare NULL from a relation takes the other side's type (RP-310)

`select 1 union all select null from (values (1)) v` answered 42804 "UNION
types integer and text cannot be matched" where PostgreSQL 15.19 returns the
two rows: an untyped NULL or string literal is `unknown`, and
`select_common_type` gives it the other side's type. Only a side without a
FROM was recognised; the set operation now reads each side's untyped targets
from the parse tree, whatever its FROM.

#### Fixed

- A set-operation side selecting an untyped NULL or string literal FROM a
  relation takes the other side's type, as on PostgreSQL 15.19.

Fixes RP-310 (tests/backlog/test_rp_top10_found.py::test_a_union_branch_selecting_null_from_a_relation_takes_the_other_type)
