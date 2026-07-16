"""``ALTER TABLE … ADD CONSTRAINT CHECK/UNIQUE`` and ``DROP CONSTRAINT``.

Adds CHECK / UNIQUE constraints to an existing table (and drops any declared
constraint by name), persisted on the ``TableDef`` and reflected exactly like a
``CREATE TABLE`` constraint. Neither is enforced — schema-shape record only.
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
    run_sql(
        s,
        DB,
        "CREATE TABLE t (id bigint primary key, email text, age int, status text)",
        session=session,
    )
    try:
        yield s
    finally:
        s.close()


def rows(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1].rows


def contypes(storage, session):
    return rows(
        storage,
        session,
        "SELECT conname, contype FROM pg_catalog.pg_constraint "
        "WHERE contype IN ('u', 'c') ORDER BY contype, conname",
    )


def test_add_named_check(storage, session):
    run_sql(storage, DB, "ALTER TABLE t ADD CONSTRAINT ck_age CHECK (age >= 0)", session=session)
    assert contypes(storage, session) == [("ck_age", "c")]
    assert rows(
        storage,
        session,
        "SELECT pg_get_constraintdef(oid) FROM pg_catalog.pg_constraint WHERE conname = 'ck_age'",
    ) == [("CHECK ((age >= 0))",)]


def test_add_named_unique(storage, session):
    run_sql(
        storage, DB, "ALTER TABLE t ADD CONSTRAINT uq_es UNIQUE (email, status)", session=session
    )
    assert contypes(storage, session) == [("uq_es", "u")]
    assert rows(
        storage,
        session,
        "SELECT column_name FROM information_schema.key_column_usage "
        "WHERE constraint_name = 'uq_es' ORDER BY ordinal_position",
    ) == [("email",), ("status",)]


def test_add_unnamed_unique_gets_default_name(storage, session):
    run_sql(storage, DB, "ALTER TABLE t ADD UNIQUE (status)", session=session)
    assert contypes(storage, session) == [("t_status_key", "u")]


def test_added_constraints_persist_in_catalog(storage, session):
    from secantus.sql.catalog import Catalog

    run_sql(storage, DB, "ALTER TABLE t ADD CONSTRAINT ck CHECK (age < 200)", session=session)
    run_sql(storage, DB, "ALTER TABLE t ADD CONSTRAINT uq UNIQUE (email)", session=session)
    tbl = Catalog(storage).get(DB, "t")
    assert tbl is not None
    assert {c.name: c.expression for c in tbl.check_constraints} == {"ck": "age < 200"}
    assert {u.name: u.columns for u in tbl.unique_constraints} == {"uq": ("email",)}


def test_drop_constraint(storage, session):
    run_sql(storage, DB, "ALTER TABLE t ADD CONSTRAINT ck_age CHECK (age >= 0)", session=session)
    run_sql(storage, DB, "ALTER TABLE t ADD CONSTRAINT uq_email UNIQUE (email)", session=session)
    run_sql(storage, DB, "ALTER TABLE t DROP CONSTRAINT ck_age", session=session)
    assert contypes(storage, session) == [("uq_email", "u")]


def test_drop_constraint_missing_errors(storage, session):
    with pytest.raises(errors.SQLError):
        run_sql(storage, DB, "ALTER TABLE t DROP CONSTRAINT nope", session=session)


def test_drop_constraint_if_exists(storage, session):
    # No error when the constraint is absent and IF EXISTS is given.
    run_sql(storage, DB, "ALTER TABLE t DROP CONSTRAINT IF EXISTS nope", session=session)
    assert contypes(storage, session) == []


def test_drop_foreign_key_constraint(storage, session):
    run_sql(storage, DB, "CREATE TABLE u (id bigint primary key)", session=session)
    run_sql(
        storage,
        DB,
        "ALTER TABLE t ADD CONSTRAINT fk_u FOREIGN KEY (id) REFERENCES u(id)",
        session=session,
    )
    assert rows(
        storage,
        session,
        "SELECT conname FROM pg_catalog.pg_constraint WHERE contype = 'f'",
    ) == [("fk_u",)]
    run_sql(storage, DB, "ALTER TABLE t DROP CONSTRAINT fk_u", session=session)
    assert (
        rows(storage, session, "SELECT conname FROM pg_catalog.pg_constraint WHERE contype = 'f'")
        == []
    )


def test_sqlalchemy_reflects_altered_constraints(storage, session, tmp_path):
    sa = pytest.importorskip("sqlalchemy")
    from secantus.sql.pgserver import SecantusPGServer

    st = Storage(str(tmp_path / "srv"))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
        with engine.begin() as conn:
            conn.execute(sa.text("CREATE TABLE t (id bigint primary key, email text, age int)"))
            conn.execute(sa.text("ALTER TABLE t ADD CONSTRAINT ck_age CHECK (age >= 0)"))
            conn.execute(sa.text("ALTER TABLE t ADD CONSTRAINT uq_email UNIQUE (email)"))
        insp = sa.inspect(engine)
        assert [
            (u["name"], tuple(u["column_names"])) for u in insp.get_unique_constraints("t")
        ] == [("uq_email", ("email",))]
        assert [(c["name"], c["sqltext"]) for c in insp.get_check_constraints("t")] == [
            ("ck_age", "age >= 0")
        ]
        engine.dispose()
    finally:
        srv.stop()
        st.close()
