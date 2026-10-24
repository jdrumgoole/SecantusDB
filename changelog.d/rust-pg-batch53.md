### Rust PostgreSQL server: reg* columns in WHERE, executor calls in FILTER

Two silent wrong answers filed in batch 52 are fixed.

#### Fixed

- Fixes RP-213
  (`tests/backlog/test_rp_batch52.py::test_scenario[a reg* column against a constant in WHERE]`):
  a `reg*` column against a constant in WHERE matched nothing. Examples:
  `pg_operator.oprcode = 'int4eq'::regproc`, `pg_proc.prosupport = 0`, and
  `aggfnoid = 'sum(int4)'::regprocedure`. The filter compared the stored
  value with a constant of another kind, or with a plain number. They are
  now compared by oid, as PostgreSQL compares them.
- Fixes RP-212
  (`tests/backlog/test_rp_batch52.py::test_scenario[an executor function in an aggregate's FILTER]`):
  an aggregate's FILTER over a function the executor answers
  (`count(*) filter (where pg_type_is_visible(oid))`) was refused, its
  argument typed as text. Such a FILTER is now evaluated per row, as one
  holding a subquery is.

#### Known

- RP-214, filed: a system catalog's `pg_relation_size` is NULL by oid and
  0 by name. PostgreSQL answers its heap size. Which model to use is a
  decision.
