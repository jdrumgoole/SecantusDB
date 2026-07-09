"""``CREATE VIEW … WITH [LOCAL|CASCADED] CHECK OPTION`` (#164).

An auto-updatable view with a check option rejects any INSERT / UPDATE through
the view that would produce a row not visible through it (the view's WHERE not
TRUE) with SQLSTATE ``44000``. A DELETE is unaffected (it removes rows). Driven
through ``run_sql`` over the real WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path, session):
    s = Storage(str(tmp_path))
    run_sql(s, DB, "CREATE TABLE t (id int PRIMARY KEY, n int)", session=session)
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def rows(storage, session, sql):
    return run(storage, session, sql).rows


def mkview(storage, session, mode=""):
    run(storage, session, f"CREATE VIEW v AS SELECT * FROM t WHERE n > 0 {mode}")


# -- INSERT ------------------------------------------------------------------ #


def test_insert_violating_predicate_raises_44000(storage, session):
    mkview(storage, session, "WITH CHECK OPTION")
    with pytest.raises(SQLError) as ei:
        run(storage, session, "INSERT INTO v VALUES (1, -5)")
    assert ei.value.sqlstate == "44000"
    assert rows(storage, session, "SELECT id FROM t") == []  # nothing written


def test_insert_satisfying_predicate_ok(storage, session):
    mkview(storage, session, "WITH CHECK OPTION")
    run(storage, session, "INSERT INTO v VALUES (1, 5)")
    assert rows(storage, session, "SELECT id, n FROM t") == [(1, 5)]


def test_insert_null_predicate_value_violates(storage, session):
    # n IS NULL → (n > 0) is NULL → not visible → CHECK OPTION violation.
    mkview(storage, session, "WITH CHECK OPTION")
    with pytest.raises(SQLError) as ei:
        run(storage, session, "INSERT INTO v (id) VALUES (1)")
    assert ei.value.sqlstate == "44000"


# -- UPDATE ------------------------------------------------------------------ #


def test_update_to_violate_raises(storage, session):
    mkview(storage, session, "WITH CHECK OPTION")
    run(storage, session, "INSERT INTO v VALUES (1, 5)")
    with pytest.raises(SQLError) as ei:
        run(storage, session, "UPDATE v SET n = -1 WHERE id = 1")
    assert ei.value.sqlstate == "44000"
    assert rows(storage, session, "SELECT n FROM t") == [(5,)]  # unchanged


def test_update_computed_to_violate_raises(storage, session):
    # The computed-SET path (materialized) also enforces the check option.
    mkview(storage, session, "WITH CHECK OPTION")
    run(storage, session, "INSERT INTO v VALUES (1, 5)")
    with pytest.raises(SQLError) as ei:
        run(storage, session, "UPDATE v SET n = n - 10 WHERE id = 1")
    assert ei.value.sqlstate == "44000"


def test_update_staying_valid_ok(storage, session):
    mkview(storage, session, "WITH CHECK OPTION")
    run(storage, session, "INSERT INTO v VALUES (1, 5)")
    run(storage, session, "UPDATE v SET n = 9 WHERE id = 1")
    assert rows(storage, session, "SELECT n FROM t") == [(9,)]


# -- variants / negatives ---------------------------------------------------- #


def test_local_check_option(storage, session):
    mkview(storage, session, "WITH LOCAL CHECK OPTION")
    with pytest.raises(SQLError) as ei:
        run(storage, session, "INSERT INTO v VALUES (1, -1)")
    assert ei.value.sqlstate == "44000"


def test_no_check_option_allows_invisible_row(storage, session):
    # Without the option, a write that lands outside the view's predicate is
    # allowed (the row is just invisible through the view) — Postgres behaviour.
    mkview(storage, session)  # no WITH CHECK OPTION
    run(storage, session, "INSERT INTO v VALUES (1, -5)")
    assert rows(storage, session, "SELECT n FROM t") == [(-5,)]


def test_delete_through_view_unaffected(storage, session):
    mkview(storage, session, "WITH CHECK OPTION")
    run(storage, session, "INSERT INTO v VALUES (1, 5)")
    run(storage, session, "DELETE FROM v WHERE id = 1")
    assert rows(storage, session, "SELECT id FROM t") == []


def test_check_option_reflected_in_information_schema(storage, session):
    mkview(storage, session, "WITH CHECK OPTION")
    assert rows(
        storage,
        session,
        "SELECT check_option FROM information_schema.views WHERE table_name = 'v'",
    ) == [("CASCADED",)]
    run(storage, session, "CREATE VIEW v2 AS SELECT * FROM t WHERE n > 0 WITH LOCAL CHECK OPTION")
    assert rows(
        storage,
        session,
        "SELECT check_option FROM information_schema.views WHERE table_name = 'v2'",
    ) == [("LOCAL",)]


def test_plain_view_reports_none_check_option(storage, session):
    mkview(storage, session)
    assert rows(
        storage,
        session,
        "SELECT check_option FROM information_schema.views WHERE table_name = 'v'",
    ) == [("NONE",)]
