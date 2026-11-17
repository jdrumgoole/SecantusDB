### A RETURNING subquery reads the table as the statement found it

PostgreSQL computes RETURNING under the statement's snapshot, so a subquery
there that reads the written table sees it as it was before the statement's
own writes: `delete from t returning (select count(*) from t z where z.a =
t.a)` counts each deleted row, and an UPDATE's correlated read of the
target returns the old value. The Rust PostgreSQL server ran such a
subquery after the write and saw the write's result. A write whose
RETURNING runs a subquery now records the rows it changes as they were,
the way a volatile function's writes are hidden from the statement calling
it, and RETURNING reads through them.

#### Fixed

- Fixes RP-943 (tests/backlog/test_rp_found_rp664.py::test_a_returning_subquery_reads_the_statement_snapshot)
