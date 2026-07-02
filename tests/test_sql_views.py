"""``CREATE VIEW`` / ``DROP VIEW`` — a stored SELECT that reads like the table it
stands for.

A view is persisted as its SELECT text in ``__sql_views__``; querying one expands
it inline as a subquery (``engine._expand_views``) so single-table reads, joins,
aggregates, and nested views all work. Reflection surfaces the view in
``pg_class`` (relkind='v'), ``pg_get_viewdef``, ``information_schema.views``, and
``information_schema.tables`` (table_type='VIEW') so SQLAlchemy's inspector can
list and render it.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from sqlfake import FakeStorage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(session):
    s = FakeStorage()
    run_sql(s, DB, "CREATE TABLE t (id bigint primary key, n int, grp text)", session=session)
    run_sql(
        s,
        DB,
        "INSERT INTO t (id, n, grp) VALUES (1, 10, 'a'), (2, 20, 'a'), (3, 5, 'b')",
        session=session,
    )
    return s


def rows(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1].rows


def tag(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1].command_tag


def test_create_view_tag(storage, session):
    assert (
        tag(storage, session, "CREATE VIEW v AS SELECT id, n FROM t WHERE n > 8") == "CREATE VIEW"
    )


def test_select_from_view(storage, session):
    run_sql(storage, DB, "CREATE VIEW v AS SELECT id, n FROM t WHERE n > 8", session=session)
    assert rows(storage, session, "SELECT id, n FROM v ORDER BY id") == [(1, 10), (2, 20)]


def test_view_with_alias(storage, session):
    run_sql(storage, DB, "CREATE VIEW v AS SELECT id, n FROM t", session=session)
    assert rows(storage, session, "SELECT x.id FROM v AS x WHERE x.n = 20") == [(2,)]


def test_aggregate_over_view(storage, session):
    run_sql(storage, DB, "CREATE VIEW v AS SELECT id, n FROM t WHERE n > 8", session=session)
    assert rows(storage, session, "SELECT count(*) FROM v") == [(2,)]


def test_join_on_view(storage, session):
    run_sql(storage, DB, "CREATE VIEW v AS SELECT id, n FROM t WHERE n > 8", session=session)
    assert rows(
        storage,
        session,
        "SELECT v.id, t.grp FROM v JOIN t ON v.id = t.id ORDER BY v.id",
    ) == [(1, "a"), (2, "a")]


def test_nested_view(storage, session):
    run_sql(storage, DB, "CREATE VIEW v AS SELECT id, n FROM t WHERE n > 8", session=session)
    run_sql(storage, DB, "CREATE VIEW v2 AS SELECT id FROM v WHERE n > 15", session=session)
    assert rows(storage, session, "SELECT id FROM v2 ORDER BY id") == [(2,)]


def test_create_or_replace_view(storage, session):
    run_sql(storage, DB, "CREATE VIEW v AS SELECT id, n FROM t WHERE n > 8", session=session)
    run_sql(
        storage,
        DB,
        "CREATE OR REPLACE VIEW v AS SELECT id, n FROM t WHERE n > 100",
        session=session,
    )
    assert rows(storage, session, "SELECT id FROM v") == []


def test_create_view_duplicate_errors(storage, session):
    run_sql(storage, DB, "CREATE VIEW v AS SELECT id FROM t", session=session)
    with pytest.raises(errors.SQLError):
        run_sql(storage, DB, "CREATE VIEW v AS SELECT id FROM t", session=session)


def test_create_view_over_existing_table_errors(storage, session):
    with pytest.raises(errors.SQLError):
        run_sql(storage, DB, "CREATE VIEW t AS SELECT id FROM t", session=session)


def test_drop_view(storage, session):
    run_sql(storage, DB, "CREATE VIEW v AS SELECT id FROM t", session=session)
    assert tag(storage, session, "DROP VIEW v") == "DROP VIEW"
    with pytest.raises(errors.SQLError):
        run_sql(storage, DB, "SELECT id FROM v", session=session)


def test_drop_view_missing_errors(storage, session):
    with pytest.raises(errors.SQLError):
        run_sql(storage, DB, "DROP VIEW nope", session=session)


def test_drop_view_if_exists(storage, session):
    assert tag(storage, session, "DROP VIEW IF EXISTS nope") == "DROP VIEW"


def test_view_in_pg_class(storage, session):
    run_sql(storage, DB, "CREATE VIEW v AS SELECT id, n FROM t WHERE n > 8", session=session)
    assert rows(
        storage,
        session,
        "SELECT relname, relkind FROM pg_catalog.pg_class WHERE relkind = 'v'",
    ) == [("v", "v")]


def test_pg_get_viewdef(storage, session):
    run_sql(storage, DB, "CREATE VIEW v AS SELECT id, n FROM t WHERE n > 8", session=session)
    assert rows(
        storage,
        session,
        "SELECT pg_catalog.pg_get_viewdef(oid) FROM pg_catalog.pg_class WHERE relname = 'v'",
    ) == [("SELECT id, n FROM t WHERE n > 8",)]


def test_information_schema_views(storage, session):
    run_sql(storage, DB, "CREATE VIEW v AS SELECT id FROM t", session=session)
    assert rows(
        storage,
        session,
        "SELECT table_name, view_definition FROM information_schema.views",
    ) == [("v", "SELECT id FROM t")]


def test_information_schema_tables_lists_view(storage, session):
    run_sql(storage, DB, "CREATE VIEW v AS SELECT id FROM t", session=session)
    assert rows(
        storage,
        session,
        "SELECT table_name FROM information_schema.tables "
        "WHERE table_type = 'VIEW' ORDER BY table_name",
    ) == [("v",)]


def test_views_persist_in_catalog(storage, session):
    from secantus.sql.catalog import Catalog

    run_sql(storage, DB, "CREATE VIEW v AS SELECT id FROM t", session=session)
    cat = Catalog(storage)
    assert cat.list_views(DB) == ["v"]
    assert cat.get_view(DB, "v") == "SELECT id FROM t"


def test_sqlalchemy_reflects_views(storage, session):
    sa = pytest.importorskip("sqlalchemy")
    from secantus.sql.pgserver import SecantusPGServer

    srv = SecantusPGServer(port=0, storage=FakeStorage())
    srv.start()
    try:
        host, port = srv.address
        engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
        with engine.begin() as conn:
            conn.execute(sa.text("CREATE TABLE t (id bigint primary key, n int)"))
            conn.execute(sa.text("INSERT INTO t (id, n) VALUES (1, 10), (2, 20), (3, 5)"))
            conn.execute(sa.text("CREATE VIEW v AS SELECT id, n FROM t WHERE n > 8"))
        insp = sa.inspect(engine)
        assert insp.get_view_names() == ["v"]
        assert insp.get_view_definition("v") == "SELECT id, n FROM t WHERE n > 8"
        with engine.connect() as conn:
            got = conn.execute(sa.text("SELECT id, n FROM v ORDER BY id")).fetchall()
        assert [tuple(r) for r in got] == [(1, 10), (2, 20)]
        engine.dispose()
    finally:
        srv.stop()
