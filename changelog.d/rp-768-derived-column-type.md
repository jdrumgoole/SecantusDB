### Rust PG server: a column read through a subquery or a CTE keeps its type

A FROM subquery's or a WITH item's column that passes a column of its own
FROM straight through keeps that column's type and collation, as
PostgreSQL's Var does. The Rust PostgreSQL server typed every such column as
text for its ORDER BY, comparisons, `min` / `max` and window ordering, unless
a cast in the subquery named the type. So `select m::text as v from (select
m from t) q order by m` over an enum column came back in the labels'
alphabetical order instead of the enum's declaration order, and `where m >
'z'` over it returned the wrong rows. An `inet`, `citext` or range column,
and a text column with an ICU collation, read through a subquery sorted as
plain text in the same way, and a citext one compared case-sensitively. The
derived column now takes its type from the column it names, through a `*`, a
qualified `*`, a set operation's legs, a join, a nested subquery and a WITH
inside one.

#### Fixed

- An enum, `inet` / `cidr`, `citext`, range or collated text column read
  through a FROM subquery or a CTE sorts and compares as its own type.

Fixes RP-768 (tests/backlog/test_rp_found_rp766.py::test_a_derived_enum_column_sorts_by_declaration)
