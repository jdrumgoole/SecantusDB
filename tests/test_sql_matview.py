"""``CREATE MATERIALIZED VIEW`` / ``REFRESH`` / ``DROP MATERIALIZED VIEW``.

A materialized view stores a snapshot of its SELECT's rows in a backing
collection (queried like a table) plus the definition text. Unlike a plain view
it does not track the base tables — ``REFRESH MATERIALIZED VIEW`` recomputes the
snapshot. Reflected as ``pg_class`` relkind ``'m'`` (not in
``information_schema.tables``, matching Postgres).
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


def test_create_tag_and_query(storage, session):
    res = run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id, n FROM t WHERE n > 8")
    assert res.command_tag == "SELECT 2"
    got = run(storage, session, "SELECT * FROM mv")
    assert [c.name for c in got.columns] == ["id", "n"]  # no storage _id column
    assert sorted(got.rows) == [(1, 10), (2, 20)]


def test_query_columns_and_aggregate(storage, session):
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t WHERE n > 8")
    assert sorted(rows(storage, session, "SELECT id FROM mv ORDER BY id")) == [(1,), (2,)]
    assert rows(storage, session, "SELECT count(*) FROM mv") == [(2,)]


def test_snapshot_is_stale_until_refresh(storage, session):
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t WHERE n > 8")
    run(storage, session, "INSERT INTO t (id, n) VALUES (4, 99)")
    # The matview does not see the new base row until refreshed.
    assert sorted(rows(storage, session, "SELECT id FROM mv ORDER BY id")) == [(1,), (2,)]
    assert run(storage, session, "REFRESH MATERIALIZED VIEW mv").command_tag == (
        "REFRESH MATERIALIZED VIEW"
    )
    assert sorted(rows(storage, session, "SELECT id FROM mv ORDER BY id")) == [(1,), (2,), (4,)]


def test_refresh_shrinks_snapshot(storage, session):
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t WHERE n > 8")
    run(storage, session, "DELETE FROM t WHERE id = 1")
    run(storage, session, "REFRESH MATERIALIZED VIEW mv")
    assert rows(storage, session, "SELECT id FROM mv") == [(2,)]


def test_pg_class_relkind_m(storage, session):
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t")
    assert rows(
        storage,
        session,
        "SELECT relname, relkind FROM pg_catalog.pg_class WHERE relkind = 'm'",
    ) == [("mv", "m")]


def test_pg_get_viewdef(storage, session):
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id, n FROM t WHERE n > 8")
    assert rows(
        storage,
        session,
        "SELECT pg_get_viewdef(oid) FROM pg_catalog.pg_class WHERE relname = 'mv'",
    ) == [("SELECT id, n FROM t WHERE n > 8",)]


def test_not_in_information_schema_tables(storage, session):
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t")
    assert rows(
        storage,
        session,
        "SELECT count(*) FROM information_schema.tables WHERE table_name = 'mv'",
    ) == [(0,)]


def test_duplicate_rejected(storage, session):
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t")
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t")
    assert ei.value.sqlstate == "42P07"


def test_drop(storage, session):
    run(storage, session, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t")
    assert (
        run(storage, session, "DROP MATERIALIZED VIEW mv").command_tag == "DROP MATERIALIZED VIEW"
    )
    assert rows(
        storage, session, "SELECT count(*) FROM pg_catalog.pg_class WHERE relkind = 'm'"
    ) == [(0,)]
    with pytest.raises(errors.SQLError):
        run(storage, session, "SELECT * FROM mv")


def test_drop_missing_errors(storage, session):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, "DROP MATERIALIZED VIEW nope")
    assert ei.value.sqlstate == "42P01"


def test_drop_if_exists(storage, session):
    assert run(storage, session, "DROP MATERIALIZED VIEW IF EXISTS nope").command_tag == (
        "DROP MATERIALIZED VIEW"
    )


def test_refresh_missing_errors(storage, session):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, "REFRESH MATERIALIZED VIEW nope")
    assert ei.value.sqlstate == "42P01"


def test_matview_over_aggregate(storage, session):
    run(
        storage,
        session,
        "CREATE MATERIALIZED VIEW mv AS SELECT count(*) AS c, sum(n) AS total FROM t",
    )
    assert rows(storage, session, "SELECT c, total FROM mv") == [(3, 35)]


def test_sqlalchemy_reflects_matview(tmp_path):
    sa = pytest.importorskip("sqlalchemy")
    from secantus.sql.pgserver import SecantusPGServer

    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
        with engine.begin() as conn:
            conn.execute(sa.text("CREATE TABLE t (id bigint primary key, n int)"))
            conn.execute(sa.text("INSERT INTO t (id, n) VALUES (1, 10), (2, 20), (3, 5)"))
            conn.execute(sa.text("CREATE MATERIALIZED VIEW mv AS SELECT id, n FROM t WHERE n > 8"))
        insp = sa.inspect(engine)
        assert "mv" in insp.get_materialized_view_names()
        assert "mv" not in insp.get_table_names()
        with engine.connect() as conn:
            got = conn.execute(sa.text("SELECT id, n FROM mv ORDER BY id")).fetchall()
        assert [tuple(r) for r in got] == [(1, 10), (2, 20)]
        engine.dispose()
    finally:
        srv.stop()
        st.close()
