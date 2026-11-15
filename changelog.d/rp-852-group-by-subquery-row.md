### Grouping by a FROM subquery's row no longer groups its columns

A query that groups by a FROM subquery's whole row, as in `select v.a from (values (1), (2)) v(a) group by v`, answered the rows as though `v.a` were grouped. PostgreSQL derives no functional dependency of a column on a subquery's row, so it rejects such a column outside an aggregate with 42803 at the reference, a select-list `v.*` included. The Rust PostgreSQL server now does the same, while a column grouped in its own right alongside the row is still accepted.

#### Fixed

- `GROUP BY v` over a FROM subquery: a column of `v` outside an aggregate and not itself grouped is 42803 `column "v.a" must appear in the GROUP BY clause or be used in an aggregate function`, positioned at the reference.

Fixes RP-852 (tests/backlog/test_rp_found_rp699.py::test_group_by_a_from_subquery_row_leaves_its_columns_ungrouped)
