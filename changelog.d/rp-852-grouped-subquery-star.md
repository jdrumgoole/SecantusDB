### A star over a FROM subquery expands in a grouped query

`select v.* from (values (1), (1)) v(a) group by a` and `(v).*` beside an aggregate now answer the subquery's columns, as PostgreSQL does, instead of failing with a grouping error that named a column `""`. A grouping error on an expanded column now points at the star that produced it.

#### Fixed

- Rust PG server: `v.*`, `*` and `(v).*` over a FROM subquery in a grouped query expand to the subquery's columns (alias column list, target names, or `columnN` for VALUES).

Fixes RP-852 (tests/backlog/test_rp_found_rp699.py::test_a_star_over_a_from_subquery_in_a_grouped_query); the `GROUP BY v` remainder stays open under RP-852.
