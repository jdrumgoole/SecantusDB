### A subquery can test an outer whole row with nothing else to type it

On the Rust PostgreSQL server, `select 1 from fs where exists (select 1
where fs is not null)` and `select (select fs is null) from fs` failed with
`could not determine data type of parameter $1`, and so did the same EXISTS
in an UPDATE's or DELETE's WHERE. PostgreSQL answers each: the outer row
reaches the subquery carrying its row type. The per-row call now passes an
outer whole row as a parameter of that type (a table's row type, or a record
for a FROM-subquery), so row `IS [NOT] NULL` and the rest are typed as they
are on PostgreSQL.

#### Fixed

- `secantus-pgplan` (`correlated.rs`): an outer whole-row reference in a
  correlated subquery is declared with its row type when the subquery runs
  per row, instead of as an untyped parameter.

Fixes RP-521 (tests/backlog/test_rp_subquery_scope.py::test_untyped_outer_whole_row)
