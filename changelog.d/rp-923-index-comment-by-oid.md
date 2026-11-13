### A renamed index keeps its comment on the Rust PostgreSQL server

PostgreSQL keeps a comment in `pg_description`, keyed by the commented
object's oid, and a rename keeps the oid, so `COMMENT ON INDEX ix IS 'c'`
followed by `ALTER INDEX ix RENAME TO ix2` still answers `c` for `ix2`.
`secantusd-pg` kept index comments under the index's name, which the rename
left behind: the renamed index answered NULL, for an index made by `CREATE
INDEX` and for a primary key's alike. Index comments are now keyed by the
index's oid, as PostgreSQL's are.

That only holds if a rename keeps the oid, and one did not: renaming the index
of a UNIQUE or EXCLUDE constraint, by `ALTER INDEX ... RENAME` or `ALTER TABLE
... RENAME CONSTRAINT`, drew it a new one. It now keeps its oid, as on
PostgreSQL. A comment recorded under an index's name by an earlier build is
still read, and a rename or a new `COMMENT ON INDEX` moves it to the oid key.

#### Fixed

- `secantus-pg`: `COMMENT ON INDEX` records the comment under the index's oid,
  and `pg_description` / `obj_description` read it from there, falling back to
  the name key an earlier build wrote. `ALTER INDEX ... RENAME` moves a
  name-keyed comment to the oid key before the name goes.
- `secantus-pg`: `assign_index_oids` treats a constraint that carries the oid
  of one of the table's own constraints as the same constraint renamed, and
  keeps its oid instead of drawing a new one.
- `tests/test_rust_pgserver_slice.py::test_the_index_relations_follow_every_index_change`
  now asserts the comment follows the rename (it carved RP-923 out before).

Fixes RP-923 (tests/backlog/test_rp_found_rp911.py::test_an_index_comment_survives_a_rename).
Regression tests:
`tests/backlog/test_rp_found_rp911.py::test_a_constraint_index_keeps_its_oid_and_comment_through_a_rename`,
`crates/secantus-pg/tests/embedded.rs::an_index_comment_recorded_under_its_name_survives_a_rename`.

#### Found

- RP-933: an index of a table outside `public` cannot be renamed, qualified or
  through the search path (`tests/backlog/test_rp_found_rp923.py::test_an_index_outside_public_can_be_renamed`).
