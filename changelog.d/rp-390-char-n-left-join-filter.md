### EXPLAIN ANALYZE counts a LEFT join filter over a char(n) column

A LEFT Nested Loop whose Join Filter reads a `char(n)` column now prints
its children's actual rows and loops, as PostgreSQL 15.19 does. The
per-pair evaluation of the filter used to refuse any `char(n)` column, so
the scans below the join printed bare; it now reads the column
blank-padded to its declared width, as PostgreSQL stores it.

#### Fixed

- `explain (analyze) select * from cc left join s on cc.b < s.c` over a
  `char(4)` column annotates `Seq Scan on cc` and the Materialize below
  the join.

Fixes RP-390, part (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a left nested loop over a char(n) join filter]; crates/secantus-pgplan/src/tests.rs::a_pair_expression_pads_a_char_n_column)
