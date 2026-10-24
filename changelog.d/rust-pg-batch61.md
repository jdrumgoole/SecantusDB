### Rust PostgreSQL server: partial indexes over computed predicates

A partial index's predicate may compute (`WHERE a % 2 = 0`), and a UNIQUE
one is enforced among the rows it admits, as on PostgreSQL 15.19.

#### Fixed

- `CREATE [UNIQUE] INDEX ... WHERE <expression>` with a predicate no
  storage filter expresses no longer answers 0A000 `AExpr is not
  supported yet`.
  - **How it is kept.** The predicate is kept as its SQL (`pg_indexes`,
    `pg_index.indpred`) and evaluated per row. The B-tree model holds only
    the rows it admits, so `pg_relation_size` matches PostgreSQL.
  - **UNIQUE.** The executor enforces it among those rows. CREATE refuses
    existing duplicates (23505, `is duplicated`), and an INSERT or UPDATE
    that makes a duplicate refuses with 23505 too.
  - **ON CONFLICT** naming such an index refuses with 0A000 (RP-236, open).

  Fixes part of RP-236 (tests/backlog/test_rp_batch60.py::test_scenario[a partial index over an expression's value],
  tests/backlog/test_rp_batch61.py::test_scenario[a computed predicate's index holds only its rows],
  tests/backlog/test_rp_batch61.py::test_scenario[a unique partial index over a computed predicate],
  tests/backlog/test_rp_batch61.py::test_scenario[an UPDATE into a computed unique index's rows]).
