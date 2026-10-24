### Rust PostgreSQL server: ON CONFLICT over a computed partial unique index

`INSERT ... ON CONFLICT` can name a unique index whose predicate computes
(`WHERE a % 3 = 0`), and a `DO UPDATE` that would update one row twice
refuses as PostgreSQL 15.19 does.

#### Fixed

- `ON CONFLICT (b) WHERE a % 3 = 0` arbitrates on a unique index whose
  predicate is kept as SQL, where it refused with 0A000 (RP-236).
  - **The arbiter** is the stored row the predicate admits that holds the
    proposed row's key. A row the predicate does not admit, or one with a
    NULL key, never conflicts.
  - **DO NOTHING** skips the row and **DO UPDATE** updates the one it
    found. An update that would make a duplicate is 23505.
  - **Under another arbiter**, a row the clause skips is no longer checked
    against such an index, and a row it inserts still is (23505).

  Fixes RP-236 (tests/backlog/test_rp_batch61.py::test_scenario[ON CONFLICT over a computed unique index],
  tests/backlog/test_rp_batch62.py::test_scenario[ON CONFLICT DO UPDATE over a computed unique index],
  tests/backlog/test_rp_batch62.py::test_scenario[a computed unique index another arbiter does not skip]).
- An `ON CONFLICT DO UPDATE` whose proposed rows meet one row twice is
  21000, `ON CONFLICT DO UPDATE command cannot affect row a second time`,
  with PostgreSQL's hint. That covers two proposed rows with one key, and a
  row followed by one conflicting with it. Before, the row was updated
  twice. As on PostgreSQL, the check is the row's xmin and cmin being the
  running command's, made before the clause's WHERE.

  Fixes RP-237 (tests/backlog/test_rp_batch62.py::test_scenario[DO UPDATE cannot affect a row a second time]).
