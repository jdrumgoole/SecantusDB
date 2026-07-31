"""``COMMENT ON TABLE`` / ``COMMENT ON COLUMN`` — stored in the catalog, reflected
via ``pg_description`` (SQLAlchemy's ``get_table_comment`` / ``get_columns``).

Comments are metadata only. A table comment lands in ``pg_description`` with
``objsubid = 0``; a column comment's ``objsubid`` is the column's attnum.
``COMMENT ON … IS NULL`` removes the comment.
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
    try:
        yield s
    finally:
        s.close()


def rows(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1].rows


def test_table_comment_in_pg_description(storage, session):
    run_sql(storage, DB, "COMMENT ON TABLE t IS 'the t table'", session=session)
    assert rows(
        storage,
        session,
        "SELECT objsubid, description FROM pg_catalog.pg_description",
    ) == [(0, "the t table")]


def test_column_comment_in_pg_description(storage, session):
    run_sql(storage, DB, "COMMENT ON COLUMN t.n IS 'a number'", session=session)
    # n is the 2nd column → attnum 2.
    assert rows(
        storage,
        session,
        "SELECT objsubid, description FROM pg_catalog.pg_description",
    ) == [(2, "a number")]


def test_comment_removal_with_is_null(storage, session):
    run_sql(storage, DB, "COMMENT ON COLUMN t.n IS 'x'", session=session)
    run_sql(storage, DB, "COMMENT ON COLUMN t.n IS NULL", session=session)
    assert rows(storage, session, "SELECT count(*) FROM pg_catalog.pg_description") == [(0,)]


def test_where_is_null_still_works(storage, session):
    # Regression guard: the COMMENT IS NULL rewrite must not touch a query's
    # WHERE ... IS NULL.
    run_sql(storage, DB, "INSERT INTO t (id, n) VALUES (1, NULL), (2, 5)", session=session)
    assert rows(storage, session, "SELECT id FROM t WHERE n IS NULL") == [(1,)]


def test_comment_on_missing_table_errors(storage, session):
    with pytest.raises(errors.SQLError):
        run_sql(storage, DB, "COMMENT ON TABLE nope IS 'x'", session=session)


def test_comment_on_missing_column_errors(storage, session):
    with pytest.raises(errors.SQLError):
        run_sql(storage, DB, "COMMENT ON COLUMN t.nope IS 'x'", session=session)


def test_comments_persist_in_catalog(storage, session):
    from secantus.sql.catalog import Catalog

    run_sql(storage, DB, "COMMENT ON TABLE t IS 'tc'", session=session)
    run_sql(storage, DB, "COMMENT ON COLUMN t.n IS 'nc'", session=session)
    tbl = Catalog(storage).get(DB, "t")
    assert tbl is not None
    assert tbl.comment == "tc"
    assert tbl.column("n").comment == "nc"


def test_sqlalchemy_reflects_comments(storage, session, tmp_path):
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
            conn.execute(sa.text("COMMENT ON TABLE t IS 'my table'"))
            conn.execute(sa.text("COMMENT ON COLUMN t.n IS 'the n col'"))
        insp = sa.inspect(engine)
        assert insp.get_table_comment("t") == {"text": "my table"}
        cols = {c["name"]: c.get("comment") for c in insp.get_columns("t")}
        assert cols["n"] == "the n col"
        engine.dispose()
    finally:
        srv.stop()
        st.close()


class TestConstraintComments:
    """``COMMENT ON CONSTRAINT c ON t`` — the two-name form sqlglot can't
    parse; stored on the catalog's check / unique constraint records."""

    def test_check_constraint_comment(self, storage, session):
        run_sql(
            storage,
            DB,
            "CREATE TABLE ct (id bigint primary key, n int CONSTRAINT n_pos CHECK (n > 0))",
            session=session,
        )
        run_sql(
            storage, DB, "COMMENT ON CONSTRAINT n_pos ON ct IS 'keep n positive'", session=session
        )
        from secantus.sql.catalog import Catalog

        table = Catalog(storage).get(DB, "ct")
        assert table.check_constraints[0].comment == "keep n positive"

    def test_unique_constraint_comment_and_null_removal(self, storage, session):
        run_sql(
            storage,
            DB,
            "CREATE TABLE ut (id bigint primary key, n int, CONSTRAINT uq_n UNIQUE (n))",
            session=session,
        )
        run_sql(storage, DB, "COMMENT ON CONSTRAINT uq_n ON ut IS 'one n each'", session=session)
        run_sql(storage, DB, "COMMENT ON CONSTRAINT uq_n ON ut IS NULL", session=session)
        from secantus.sql.catalog import Catalog

        table = Catalog(storage).get(DB, "ut")
        assert table.unique_constraints[0].comment is None

    def test_unknown_constraint_errors(self, storage, session):
        with pytest.raises(errors.SQLError) as exc:
            run_sql(storage, DB, "COMMENT ON CONSTRAINT nope ON t IS 'x'", session=session)
        assert exc.value.sqlstate == "42704"

    def test_escaped_quote_in_comment(self, storage, session):
        run_sql(
            storage,
            DB,
            "CREATE TABLE qt (id bigint primary key, n int CONSTRAINT n_pos CHECK (n > 0))",
            session=session,
        )
        run_sql(storage, DB, "COMMENT ON CONSTRAINT n_pos ON qt IS 'it''s fine'", session=session)
        from secantus.sql.catalog import Catalog

        table = Catalog(storage).get(DB, "qt")
        assert table.check_constraints[0].comment == "it's fine"
