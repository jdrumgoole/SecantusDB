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
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(session, tmp_path):
    s = Storage(str(tmp_path))
    run_sql(s, DB, "CREATE TABLE t (id bigint primary key, n int, grp text)", session=session)
    run_sql(
        s,
        DB,
        "INSERT INTO t (id, n, grp) VALUES (1, 10, 'a'), (2, 20, 'a'), (3, 5, 'b')",
        session=session,
    )
    try:
        yield s
    finally:
        s.close()


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


def test_cte_shadows_same_named_view(storage, session):
    # A CTE named the same as a stored view must win — the view is not expanded
    # in place of the CTE (the CTE-catalog re-dispatch must not re-run expansion).
    run_sql(storage, DB, "CREATE VIEW big AS SELECT id FROM t WHERE n < 0", session=session)
    assert rows(
        storage,
        session,
        "WITH big AS (SELECT id FROM t WHERE n >= 8) SELECT id FROM big ORDER BY id",
    ) == [(1,), (2,)]


def test_with_insert_select_from_cte_not_broken_by_views(storage, session):
    # Regression: `WITH cte AS (...) INSERT INTO dst SELECT ... FROM cte` re-enters
    # dispatch with a _CTECatalog; view expansion must not crash on it.
    run_sql(storage, DB, "CREATE TABLE dst (id bigint primary key, n int)", session=session)
    run_sql(
        storage,
        DB,
        "WITH big AS (SELECT id, n FROM t WHERE n >= 8) "
        "INSERT INTO dst (id, n) SELECT id, n FROM big",
        session=session,
    )
    assert rows(storage, session, "SELECT id FROM dst ORDER BY id") == [(1,), (2,)]


def test_views_persist_in_catalog(storage, session):
    from secantus.sql.catalog import Catalog

    run_sql(storage, DB, "CREATE VIEW v AS SELECT id FROM t", session=session)
    cat = Catalog(storage)
    assert cat.list_views(DB) == ["v"]
    assert cat.get_view(DB, "v") == "SELECT id FROM t"


def test_sqlalchemy_reflects_views(session, tmp_path):
    sa = pytest.importorskip("sqlalchemy")
    from secantus.sql.pgserver import SecantusPGServer

    st = Storage(str(tmp_path / "srv"))
    srv = SecantusPGServer(port=0, storage=st)
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
        st.close()


# -- writable views (#146) -------------------------------------------------- #


def _run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def test_insert_through_star_view(storage, session):
    _run(storage, session, "CREATE VIEW v AS SELECT * FROM t")
    _run(storage, session, "INSERT INTO v (id, n, grp) VALUES (4, 40, 'c')")
    assert rows(storage, session, "SELECT id, n, grp FROM t WHERE id = 4") == [(4, 40, "c")]


def test_update_delete_through_star_view(storage, session):
    _run(storage, session, "CREATE VIEW v AS SELECT * FROM t")
    _run(storage, session, "UPDATE v SET n = 99 WHERE id = 1")
    assert rows(storage, session, "SELECT n FROM t WHERE id = 1") == [(99,)]
    _run(storage, session, "DELETE FROM v WHERE id = 3")
    assert rows(storage, session, "SELECT id FROM t ORDER BY id") == [(1,), (2,)]


def test_filtered_view_update_restricted_by_view_where(storage, session):
    # The view's WHERE bounds which base rows the DML may touch.
    _run(storage, session, "CREATE VIEW va AS SELECT id, n FROM t WHERE grp = 'a'")
    _run(storage, session, "UPDATE va SET n = 100")  # only grp='a' rows (1,2)
    assert rows(storage, session, "SELECT id, n FROM t ORDER BY id") == [(1, 100), (2, 100), (3, 5)]


def test_filtered_view_delete_excludes_nonmatching(storage, session):
    _run(storage, session, "CREATE VIEW va AS SELECT id, n FROM t WHERE grp = 'a'")
    # id=3 is grp='b' -> excluded by the view -> not deleted.
    _run(storage, session, "DELETE FROM va WHERE id = 3")
    assert rows(storage, session, "SELECT id FROM t ORDER BY id") == [(1,), (2,), (3,)]
    # id=1 is grp='a' -> deletable through the view.
    _run(storage, session, "DELETE FROM va WHERE id = 1")
    assert rows(storage, session, "SELECT id FROM t ORDER BY id") == [(2,), (3,)]


def test_insert_through_filtered_view(storage, session):
    _run(storage, session, "CREATE VIEW va AS SELECT id, n FROM t WHERE grp = 'a'")
    _run(storage, session, "INSERT INTO va (id, n) VALUES (5, 50)")
    assert rows(storage, session, "SELECT id, n FROM t WHERE id = 5") == [(5, 50)]


def test_non_updatable_view_rejected(storage, session):
    _run(storage, session, "CREATE VIEW vagg AS SELECT grp, count(*) AS c FROM t GROUP BY grp")
    with pytest.raises(errors.SQLError) as exc:
        _run(storage, session, "INSERT INTO vagg (grp, c) VALUES ('z', 1)")
    assert exc.value.sqlstate == "0A000"


def test_join_view_not_updatable(storage, session):
    _run(storage, session, "CREATE TABLE u (id bigint primary key, label text)")
    _run(
        storage, session, "CREATE VIEW vj AS SELECT t.id, t.n, u.label FROM t JOIN u ON t.id = u.id"
    )
    with pytest.raises(errors.SQLError) as exc:
        _run(storage, session, "DELETE FROM vj WHERE id = 1")
    assert exc.value.sqlstate == "0A000"
