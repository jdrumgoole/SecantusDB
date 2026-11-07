### A comment the Rust PostgreSQL server could not read is an error, never a NULL

`obj_description`, `col_description` and the `pg_description` catalog answer
from the comments `secantusd-pg` publishes to its planner once per catalog
version. Every storage read on that path -- the index comments, the table
catalog, each table's index list, domains, functions, the other comments --
answered a failure with "no comments", so a transient storage error became a
NULL comment. The publication was then recorded as current, so the wrong answer
lasted until the catalog next changed. `COMMENT ON INDEX` had the same flaw: a
failed read of the index list was reported as `relation ... does not exist`.

CI saw that symptom once (`obj_description` of a commented index answered NULL
on the `pg-oracle` job). It has not been reproduced locally, so its cause is not
known yet (RP-499). If it happens again, the error will now say what failed.

#### Fixed

- `secantus-pg`: `object_comments` returns the read's error. A publication
  whose comment read failed is not recorded as current, so the next statement
  reads the comments again. The planner holds the error where the comments
  would be, and `obj_description` / `col_description` raise it (`XX000`, with
  the storage message). The other tables are still published, because several
  callers of the publication cannot carry an error. A failed read while
  building `pg_description`'s rows is raised by the statement reading them,
  and those rows are not cached. `COMMENT ON INDEX` and the comments use a new
  fallible `try_index_relations`.
- `tests/test_rust_pg_corpora.py`: a corpus setup statement that errors now
  fails its corpus's test, with the statement and its error. Before, the
  failure surfaced only later as a wrong answer to some check. A statement that
  is expected to fail is listed in `SETUP_ERRORS` with its reason and its exact
  error. Today five setup statements fail, all in two corpora, and PostgreSQL
  14.13 and 15.19 fail each of them with the same error. In `hash_partitions`
  an `int4` product overflows. In `numeric_index` an INSERT spans four lines,
  and each line runs as a statement of its own.

#### Added

- `secantus-storage`: `set_read_fault`, a test-only hook that fails the
  document scans a test chooses. `WtError` is re-exported so the hook can
  build an engine error. Regression test:
  `crates/secantus-pg/tests/embedded.rs::a_failed_comment_read_is_an_error_not_a_null`.
  It fails the index-comment scan with a `WT_ROLLBACK` and checks three things:
  `obj_description` and `pg_description` answer `XX000` naming the cause, a
  statement that reads no comment still succeeds, and once the fault is
  cleared the same catalog version reads the comment. Without the fix it
  answers NULL.
- The Rust MongoDB server links `secantus-storage`. The fault check costs it
  one relaxed atomic load per scan, and nothing else changes for it.
