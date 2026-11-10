### Rust PG server: a GROUP BY over a subquery groups by its value

`select (select v.a) from v group by 1`, its `(select v.*)` form, `group
by <output name>` and `group by (select v.a)` answer as PostgreSQL 15.19
does: the grouped item is the subquery, computed once per input row, and the
outer column it reads is grouped through it.

#### Fixed

- A grouped query with a subquery in its select list, HAVING or ORDER BY is
  split so the subqueries run over the grouped rows. The split resolved no
  GROUP BY position or output name against the select list -- `group by 1`
  named the inner query's own first column -- so the queries above were
  42803 `subquery uses ungrouped column "v.a" from outer query`, 42P01 or
  42803 `must appear in the GROUP BY clause`, and `select a, (select 1) ...
  group by 1` was 42703. Keys are now resolved as `findTargetlistEntrySQL92`
  reads them, and a key holding a subquery is computed by a LATERAL
  subquery the grouped query joins, since the aggregate planner keys on no
  subquery: `select count(*) ... group by (select v.a)` was 0A000 `SubLink
  is not supported yet`. A scalar subquery whose one target is a star is
  named after its first column (`a`, not `v`), and a VALUES item's columns
  are known to the split.
  Fixes RP-700 (tests/backlog/test_rp_found_rp517.py::test_group_by_a_correlated_subquery_target).
- A LATERAL subquery whose select list holds a scalar subquery over the
  left side's row failed to prepare: its Describe planned the body over
  sample values with the correlated subquery run, and a Describe has no
  executor (0A000 `a correlated subquery evaluated outside a statement's
  execution`). That planning now reads the subquery as a NULL of its type,
  as the name-binding probe beside it already did.
  Fixes RP-856 (tests/backlog/test_rp_found_rp700.py::test_a_prepared_lateral_subquery_reads_a_correlated_subquery).
- Regression test: `crates/secantus-pg/tests/group_by_subquery.rs`.

#### Filed

- RP-853: an ungrouped column beside a subquery in a grouped select list is
  42703, not 42803.
- RP-854: a GROUP BY constant that is no integer is not 42601.
