"""SERIAL columns and sequences — ``CREATE SEQUENCE``, ``nextval`` / ``currval`` /
``setval`` / ``lastval``, and SERIAL/BIGSERIAL/SMALLSERIAL auto-increment.

A SERIAL column is an integer column, implicitly NOT NULL, backed by an owned
sequence whose next value fills the column when an INSERT omits it. Sequences are
persisted monotonic counters; currval/lastval are per-session.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def sqlstate(storage, session, sql):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, sql)
    return ei.value.sqlstate


# -- SERIAL columns ------------------------------------------------------------ #


def test_serial_autoincrements(storage, session):
    run(storage, session, "CREATE TABLE t (id serial PRIMARY KEY, name text)")
    run(storage, session, "INSERT INTO t (name) VALUES ('a')")
    run(storage, session, "INSERT INTO t (name) VALUES ('b')")
    assert run(storage, session, "SELECT id, name FROM t ORDER BY id").rows == [(1, "a"), (2, "b")]


def test_bigserial_and_smallserial(storage, session):
    run(storage, session, "CREATE TABLE b (id bigserial PRIMARY KEY, v int)")
    run(storage, session, "CREATE TABLE s (id smallserial PRIMARY KEY, v int)")
    assert run(storage, session, "INSERT INTO b (v) VALUES (7) RETURNING id").rows == [(1,)]
    assert run(storage, session, "INSERT INTO s (v) VALUES (9) RETURNING id").rows == [(1,)]


def test_serial_returning_reports_assigned_value(storage, session):
    run(storage, session, "CREATE TABLE t (id serial PRIMARY KEY, name text)")
    res = run(storage, session, "INSERT INTO t (name) VALUES ('x') RETURNING id, name")
    assert res.rows == [(1, "x")]


def test_explicit_value_does_not_advance_sequence(storage, session):
    """Supplying the SERIAL column explicitly leaves the sequence untouched
    (Postgres behaviour — the next default draw is unaffected)."""
    run(storage, session, "CREATE TABLE t (id serial PRIMARY KEY, name text)")
    run(storage, session, "INSERT INTO t (id, name) VALUES (50, 'z')")
    run(storage, session, "INSERT INTO t (name) VALUES ('a')")  # still draws 1
    assert run(storage, session, "SELECT id FROM t ORDER BY id").rows == [(1,), (50,)]


def test_serial_currval_tracks_last_insert(storage, session):
    run(storage, session, "CREATE TABLE t (id serial PRIMARY KEY, name text)")
    run(storage, session, "INSERT INTO t (name) VALUES ('a')")
    run(storage, session, "INSERT INTO t (name) VALUES ('b')")
    assert run(storage, session, "SELECT currval('t_id_seq')").rows == [(2,)]


def test_serial_is_not_null(storage, session):
    run(storage, session, "CREATE TABLE t (id serial PRIMARY KEY, name text)")
    # An explicit NULL for the SERIAL column violates NOT NULL.
    assert sqlstate(storage, session, "INSERT INTO t (id, name) VALUES (NULL, 'a')") == "23502"


# -- CREATE / DROP SEQUENCE ---------------------------------------------------- #


def test_create_sequence_and_nextval(storage, session):
    run(storage, session, "CREATE SEQUENCE s START WITH 100 INCREMENT BY 5")
    assert run(storage, session, "SELECT nextval('s')").rows == [(100,)]
    assert run(storage, session, "SELECT nextval('s')").rows == [(105,)]
    assert run(storage, session, "SELECT currval('s')").rows == [(105,)]


def test_default_nextval_on_column(storage, session):
    run(storage, session, "CREATE SEQUENCE s START WITH 10")
    run(storage, session, "CREATE TABLE t (id bigint DEFAULT nextval('s') PRIMARY KEY, v int)")
    run(storage, session, "INSERT INTO t (v) VALUES (1)")
    run(storage, session, "INSERT INTO t (v) VALUES (2)")
    assert run(storage, session, "SELECT id, v FROM t ORDER BY id").rows == [(10, 1), (11, 2)]


def test_setval_and_lastval(storage, session):
    run(storage, session, "CREATE SEQUENCE s")
    run(storage, session, "SELECT setval('s', 42)")
    assert run(storage, session, "SELECT nextval('s')").rows == [(43,)]
    assert run(storage, session, "SELECT lastval()").rows == [(43,)]


def test_setval_not_called(storage, session):
    """``setval(s, v, false)`` makes the *next* nextval return ``v`` itself."""
    run(storage, session, "CREATE SEQUENCE s")
    run(storage, session, "SELECT setval('s', 42, false)")
    assert run(storage, session, "SELECT nextval('s')").rows == [(42,)]


def test_create_sequence_if_not_exists(storage, session):
    run(storage, session, "CREATE SEQUENCE s")
    assert sqlstate(storage, session, "CREATE SEQUENCE s") == "42P07"
    assert run(storage, session, "CREATE SEQUENCE IF NOT EXISTS s").command_tag == "CREATE SEQUENCE"


def test_drop_sequence(storage, session):
    run(storage, session, "CREATE SEQUENCE s")
    assert run(storage, session, "DROP SEQUENCE s").command_tag == "DROP SEQUENCE"
    assert sqlstate(storage, session, "DROP SEQUENCE s") == "42P01"
    assert run(storage, session, "DROP SEQUENCE IF EXISTS s").command_tag == "DROP SEQUENCE"


def test_drop_table_drops_owned_sequence(storage, session):
    run(storage, session, "CREATE TABLE t (id serial PRIMARY KEY, name text)")
    run(storage, session, "DROP TABLE t")
    # The owned sequence is gone.
    assert sqlstate(storage, session, "SELECT nextval('t_id_seq')") == "42P01"


# -- currval / lastval error states -------------------------------------------- #


def test_currval_before_nextval_errors(storage, session):
    run(storage, session, "CREATE SEQUENCE s")
    assert sqlstate(storage, session, "SELECT currval('s')") == "55000"


def test_lastval_before_any_nextval_errors(storage, session):
    assert sqlstate(storage, session, "SELECT lastval()") == "55000"


def test_nextval_unknown_sequence_errors(storage, session):
    assert sqlstate(storage, session, "SELECT nextval('nope')") == "42P01"


# -- overflow / cycle ---------------------------------------------------------- #


def test_maxvalue_without_cycle_overflows(storage, session):
    run(storage, session, "CREATE SEQUENCE s START WITH 1 INCREMENT BY 1 MAXVALUE 2")
    assert run(storage, session, "SELECT nextval('s')").rows == [(1,)]
    assert run(storage, session, "SELECT nextval('s')").rows == [(2,)]
    assert sqlstate(storage, session, "SELECT nextval('s')") == "2200H"


def test_cycle_wraps_to_minvalue(storage, session):
    run(
        storage,
        session,
        "CREATE SEQUENCE s START WITH 1 INCREMENT BY 1 MINVALUE 1 MAXVALUE 2 CYCLE",
    )
    run(storage, session, "SELECT nextval('s')")  # 1
    run(storage, session, "SELECT nextval('s')")  # 2
    assert run(storage, session, "SELECT nextval('s')").rows == [(1,)]  # wraps


# -- reflection ---------------------------------------------------------------- #


def test_pg_class_lists_sequence_relkind(storage, session):
    run(storage, session, "CREATE TABLE t (id serial PRIMARY KEY)")
    run(storage, session, "CREATE SEQUENCE s")
    rows = run(
        storage,
        session,
        "SELECT relname, relkind FROM pg_catalog.pg_class WHERE relkind = 'S' ORDER BY relname",
    ).rows
    assert rows == [("s",), ("t_id_seq",)] or rows == [("s", "S"), ("t_id_seq", "S")]


def test_information_schema_sequences(storage, session):
    run(storage, session, "CREATE SEQUENCE s START WITH 100 INCREMENT BY 5 CYCLE")
    rows = run(
        storage,
        session,
        "SELECT sequence_name, start_value, increment, cycle_option "
        "FROM information_schema.sequences",
    ).rows
    assert rows == [("s", "100", "5", "YES")]


def test_pg_sequence_row(storage, session):
    run(storage, session, "CREATE SEQUENCE s START WITH 7 INCREMENT BY 3 MAXVALUE 50")
    rows = run(
        storage,
        session,
        "SELECT seqstart, seqincrement, seqmax, seqcycle FROM pg_catalog.pg_sequence",
    ).rows
    assert rows == [(7, 3, 50, False)]


# -- batched allocation (SEQUENCE_ALLOC_BATCH) ------------------------------ #
# ``nextval`` pre-allocates a batch with ONE persisted write (PG's CACHE
# mechanism applied server-side). Values stay gapless while the server runs;
# the persisted doc carries the batch's high-water mark, so a reopen resumes
# past the batch — the same gap PG's CACHE/crash semantics produce.


def test_batched_nextvals_are_gapless(storage, session):
    run(storage, session, "CREATE SEQUENCE s")
    got = [run(storage, session, "SELECT nextval('s')").rows[0][0] for _ in range(300)]
    assert got == list(range(1, 301))  # crosses two batch boundaries


def test_batch_persists_high_water_mark_and_reopen_skips(tmp_path, session):
    from secantus.sql.catalog import SEQUENCE_ALLOC_BATCH, Catalog

    s = Storage(str(tmp_path))
    try:
        run(s, session, "CREATE SEQUENCE s")
        assert run(s, session, "SELECT nextval('s')").rows[0][0] == 1
        # The stored doc carries the whole batch's high-water mark.
        assert Catalog(s).get_sequence(DB, "s")["last_value"] == SEQUENCE_ALLOC_BATCH
    finally:
        s.close()
    s = Storage(str(tmp_path))
    try:
        # A reopen loses the in-memory run: the next value resumes past the
        # persisted mark (PG CACHE/crash gap), never repeating a handed value.
        assert run(s, session, "SELECT nextval('s')").rows[0][0] == SEQUENCE_ALLOC_BATCH + 1
    finally:
        s.close()


def test_setval_discards_prefetched_run(storage, session):
    run(storage, session, "CREATE SEQUENCE s")
    assert run(storage, session, "SELECT nextval('s')").rows[0][0] == 1
    run(storage, session, "SELECT setval('s', 1000)")
    assert run(storage, session, "SELECT nextval('s')").rows[0][0] == 1001


def test_alter_restart_discards_prefetched_run(storage, session):
    run(storage, session, "CREATE SEQUENCE s")
    assert run(storage, session, "SELECT nextval('s')").rows[0][0] == 1
    run(storage, session, "ALTER SEQUENCE s RESTART WITH 500")
    assert run(storage, session, "SELECT nextval('s')").rows[0][0] == 500


def test_bounded_sequence_exhausts_exactly_across_batches(storage, session):
    run(storage, session, "CREATE SEQUENCE s MAXVALUE 5")
    got = [run(storage, session, "SELECT nextval('s')").rows[0][0] for _ in range(5)]
    assert got == [1, 2, 3, 4, 5]
    assert sqlstate(storage, session, "SELECT nextval('s')") == "2200H"


def test_recreate_discards_prefetched_run(storage, session):
    run(storage, session, "CREATE SEQUENCE s")
    assert run(storage, session, "SELECT nextval('s')").rows[0][0] == 1
    run(storage, session, "DROP SEQUENCE s")
    run(storage, session, "CREATE SEQUENCE s START WITH 40")
    assert run(storage, session, "SELECT nextval('s')").rows[0][0] == 40
