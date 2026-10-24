### Rust PostgreSQL server: cancels in the extended protocol, reg* filters, executor calls in FILTER

Two silent wrong answers filed in batch 52 are fixed. A cancel that landed
during an extended-protocol Parse is no longer lost.

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

- Fixes RP-215
  (`tests/backlog/test_rp_batch53.py::test_a_cancel_during_a_prepared_statements_parse_is_honoured`):
  a statement prepared on its own, as psycopg's `prepare=True` sends it
  (Parse and Sync, then Bind, Execute and Sync), was not cancellable during
  the Parse. The Parse's analysis parsed the text without the cancel hook
  (0.75 s for a 1.2 MB statement). Its describe pass planned without the
  hook, and a cancel left pending when the Parse answered was cleared as
  stale at the next message. The statement then ran to its end
  (`pg_sleep(3)` included). A cancel at any point of the cycle is now
  answered within 0.21 s (PostgreSQL 15.19: 0.015 s).
- Fixes RP-216
  (`tests/backlog/test_rp_batch53.py::test_a_long_constant_in_list_matching_late_is_linear`):
  a constant `x IN (...)` cloned the whole expression, list included, for
  each item it compared, so its cost was quadratic in the list. A
  200,000-item list matching at its 780th item took 24 s; one matching at
  its last now takes 3.7 s in a debug build.
- The parse memo is bounded by the text it holds, not only by its count. A
  parse tree holds about 160 bytes per byte of text, and each distinct
  200,000-item `IN` list left 200 MB resident, never released (measured:
  1.3 GB to 2.2 GB over five statements). It now stays near 930 MB.

#### Known

- RP-214, filed: a system catalog's `pg_relation_size` is NULL by oid and
  0 by name. PostgreSQL answers its heap size. Which model to use is a
  decision.
