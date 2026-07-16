"""Materialized-view polish: ``WITH [NO] DATA``, ``REFRESH … CONCURRENTLY``,
``ALTER MATERIALIZED VIEW … RENAME TO``.

``WITH NO DATA`` registers the matview unpopulated (not scannable — querying it
errors ``55000``) until the first ``REFRESH``. ``CONCURRENTLY`` recomputes the
snapshot but, like Postgres, requires the matview to be populated and to carry a
unique index (else ``0A000``). ``RENAME TO`` moves the matview, its catalog shape,
and its backing collection.
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
def storage(session, tmp_path):
    s = Storage(str(tmp_path))
    run_sql(s, DB, "CREATE TABLE t (id bigint primary key, n int)", session=session)
    run_sql(s, DB, "INSERT INTO t (id, n) VALUES (1, 10), (2, 20), (3, 5)", session=session)
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def rows(storage, session, sql):
    return run(storage, session, sql).rows


def test_with_no_data_is_unpopulated(storage, session):
    res = run(
        storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t WHERE n > 8 WITH NO DATA"
    )
    assert res.command_tag == "CREATE MATERIALIZED VIEW"
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, "SELECT * FROM mv")
    assert ei.value.sqlstate == "55000"


def test_with_no_data_still_reflects_as_matview(storage, session):
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t WITH NO DATA")
    assert rows(
        storage, session, "SELECT relkind FROM pg_catalog.pg_class WHERE relname = 'mv'"
    ) == [("m",)]


def test_refresh_populates_a_no_data_matview(storage, session):
    run(
        storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t WHERE n > 8 WITH NO DATA"
    )
    run(storage, session, "REFRESH MATERIALIZED VIEW mv")
    assert sorted(rows(storage, session, "SELECT id FROM mv ORDER BY id")) == [(1,), (2,)]


def test_with_data_explicit_is_populated(storage, session):
    res = run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t WITH DATA")
    assert res.command_tag == "SELECT 3"
    assert sorted(rows(storage, session, "SELECT id FROM mv ORDER BY id")) == [(1,), (2,), (3,)]


def test_refresh_concurrently(storage, session):
    # Postgres requires a unique index for a CONCURRENTLY refresh.
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t WHERE n > 8")
    run(storage, session, "CREATE UNIQUE INDEX mv_id ON mv (id)")
    run(storage, session, "INSERT INTO t (id, n) VALUES (4, 99)")
    assert run(storage, session, "REFRESH MATERIALIZED VIEW CONCURRENTLY mv").command_tag == (
        "REFRESH MATERIALIZED VIEW"
    )
    assert sorted(rows(storage, session, "SELECT id FROM mv ORDER BY id")) == [(1,), (2,), (4,)]


def test_refresh_concurrently_without_unique_index_rejected(storage, session):
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t WHERE n > 8")
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, "REFRESH MATERIALIZED VIEW CONCURRENTLY mv")
    assert ei.value.sqlstate == "0A000"
    assert "concurrently" in str(ei.value).lower()


def test_refresh_concurrently_before_populated_rejected(storage, session):
    run(
        storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t WHERE n > 8 WITH NO DATA"
    )
    run(storage, session, "CREATE UNIQUE INDEX mv_id ON mv (id)")
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, "REFRESH MATERIALIZED VIEW CONCURRENTLY mv")
    assert ei.value.sqlstate == "0A000"


def test_alter_rename(storage, session):
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t WHERE n > 8")
    assert run(storage, session, "ALTER MATERIALIZED VIEW mv RENAME TO mv2").command_tag == (
        "ALTER MATERIALIZED VIEW"
    )
    assert sorted(rows(storage, session, "SELECT id FROM mv2 ORDER BY id")) == [(1,), (2,)]
    with pytest.raises(errors.SQLError):
        run(storage, session, "SELECT * FROM mv")
    assert rows(
        storage, session, "SELECT relname FROM pg_catalog.pg_class WHERE relkind = 'm'"
    ) == [("mv2",)]


def test_alter_rename_preserves_unpopulated_state(storage, session):
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t WITH NO DATA")
    run(storage, session, "ALTER MATERIALIZED VIEW mv RENAME TO mv2")
    # Still unpopulated after the rename.
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, "SELECT * FROM mv2")
    assert ei.value.sqlstate == "55000"


def test_alter_rename_missing_errors(storage, session):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, "ALTER MATERIALIZED VIEW nope RENAME TO x")
    assert ei.value.sqlstate == "42P01"


def test_alter_rename_to_existing_errors(storage, session):
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t")
    run(storage, session, "CREATE TABLE taken (id bigint primary key)")
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, "ALTER MATERIALIZED VIEW mv RENAME TO taken")
    assert ei.value.sqlstate == "42P07"
