### Rust PG server: a two-table join's WHERE reads each constant as its column's type

A two-table join's WHERE compared each constant as the string it was
written. A quoted literal against a column stored as something other than
its text matched nothing, and the query returned no rows without an error.
`select x.k from x join u on x.k = u.k where x.k = '1'` was empty for an
`int` column, and so were `x.a = '{ab}'` against a `text[]` or `char(n)[]`
column and `s.c = 'ab '::char(3)` against a `char(3)` column. PostgreSQL
15.19 answers each with the matching row. Comma joins with the join
condition in the WHERE, joins with USING, and one-table filters were
already right.

The cause was the narrow join path, the planner's first try for a join of
two items with one ON equality. It took each `alias.col <op> const`
conjunct of the WHERE as written. Each constant now goes through the
reading a one-table filter gives it, shared as `comparison_operand`. That
reading drops the blanks against a `char(n)` column (`bpchareq`), takes a
regtype or regclass by its OID, and reads an untyped literal as the
column's type. A bad literal (`x.k = 'abc'`) now raises 22P02, as in
PostgreSQL. A date compared with a timestamp, or a scalar column with an
array, goes to the general join planner, which already handles both.

#### Fixed

- Fixes RP-723 (tests/backlog/test_rp_found_rp719.py::test_a_join_filter_reads_an_untyped_literal_as_the_array_column)
- Fixes RP-724 (tests/backlog/test_rp_found_rp719.py::test_a_join_filter_compares_a_char_n_column_without_its_blanks)
