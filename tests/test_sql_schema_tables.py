"""Schema-qualified tables, views, and sequences: relations in a user schema
are stored under dotted catalog keys (``test_schema.users``, like user types),
coexist with same-named public relations, reflect under their own
``pg_namespace`` row, are invisible to unqualified (search-path) lookups, and
die with ``DROP SCHEMA … CASCADE``. Also pins the ``NOT LIKE`` fix — sqlglot
parses it as ``Like(negate=True)``, which both engines previously ignored
(``NOT LIKE`` behaved as ``LIKE``).
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def rows(storage, session, sql):
    return run(storage, session, sql).rows


@pytest.fixture
def schema_env(storage, session):
    run(storage, session, "CREATE SCHEMA s1")
    run(storage, session, "CREATE TABLE users (id int primary key, name text)")
    run(storage, session, "CREATE TABLE s1.users (id int primary key, data text)")
    run(storage, session, "INSERT INTO users VALUES (9, 'pub')")
    run(storage, session, "INSERT INTO s1.users VALUES (1, 'x')")
    return storage


class TestSchemaQualifiedTables:
    def test_coexist_and_resolve(self, schema_env, session):
        assert rows(schema_env, session, "SELECT id, data FROM s1.users") == [(1, "x")]
        assert rows(schema_env, session, "SELECT id FROM users") == [(9,)]
        assert rows(schema_env, session, "SELECT id FROM public.users") == [(9,)]

    def test_dml(self, schema_env, session):
        run(schema_env, session, "UPDATE s1.users SET data = 'y' WHERE id = 1")
        assert rows(schema_env, session, "SELECT data FROM s1.users") == [("y",)]
        run(schema_env, session, "DELETE FROM s1.users WHERE id = 1")
        assert rows(schema_env, session, "SELECT COUNT(*) FROM s1.users") == [(0,)]
        assert rows(schema_env, session, "SELECT COUNT(*) FROM users") == [(1,)]

    def test_pg_class_namespaces(self, schema_env, session):
        got = rows(
            schema_env,
            session,
            "SELECT n.nspname FROM pg_class c JOIN pg_namespace n "
            "ON c.relnamespace = n.oid WHERE c.relname = 'users' AND c.relkind = 'r' "
            "ORDER BY n.nspname",
        )
        assert got == [("public",), ("s1",)]

    def test_unqualified_lookup_does_not_see_user_schema(self, storage, session):
        run(storage, session, "CREATE SCHEMA s2")
        run(storage, session, "CREATE TABLE s2.only_here (id int primary key)")
        with pytest.raises(SQLError):
            run(storage, session, "SELECT 1 FROM only_here")

    def test_create_into_unknown_schema_3f000(self, storage, session):
        with pytest.raises(SQLError) as exc:
            run(storage, session, "CREATE TABLE nope.t (id int primary key)")
        assert exc.value.sqlstate == "3F000"

    def test_drop_schema_cascade_drops_tables(self, schema_env, session):
        run(schema_env, session, "DROP SCHEMA s1 CASCADE")
        with pytest.raises(SQLError):
            run(schema_env, session, "SELECT 1 FROM s1.users")
        assert rows(schema_env, session, "SELECT COUNT(*) FROM users") == [(1,)]

    def test_drop_schema_without_cascade_2bp01(self, schema_env, session):
        with pytest.raises(SQLError) as exc:
            run(schema_env, session, "DROP SCHEMA s1")
        assert exc.value.sqlstate == "2BP01"

    def test_fk_default_name_uses_bare_table(self, schema_env, session):
        run(
            schema_env,
            session,
            "CREATE TABLE s1.orders (id int primary key, user_id int REFERENCES s1.users (id))",
        )
        got = rows(
            schema_env,
            session,
            "SELECT conname FROM pg_constraint WHERE contype = 'f'",
        )
        assert got == [("orders_user_id_fkey",)]


class TestSchemaQualifiedViewsAndSequences:
    def test_views(self, schema_env, session):
        run(schema_env, session, "CREATE VIEW users_v AS SELECT * FROM users")
        run(schema_env, session, "CREATE VIEW s1.users_v AS SELECT * FROM s1.users")
        assert rows(schema_env, session, "SELECT data FROM s1.users_v") == [("x",)]
        assert rows(schema_env, session, "SELECT name FROM users_v") == [("pub",)]

    def test_sequences(self, storage, session):
        run(storage, session, "CREATE SCHEMA sq")
        run(storage, session, "CREATE SEQUENCE sq.counter START 5")
        run(storage, session, "CREATE SEQUENCE counter START 100")
        assert rows(storage, session, "SELECT nextval('sq.counter')") == [(5,)]
        assert rows(storage, session, "SELECT nextval('counter')") == [(100,)]
        assert rows(storage, session, "SELECT nextval('public.counter')") == [(101,)]
        run(storage, session, "DROP SEQUENCE sq.counter")
        with pytest.raises(SQLError):
            run(storage, session, "SELECT nextval('sq.counter')")

    def test_comment_on_schema_qualified_column(self, schema_env, session):
        run(schema_env, session, "COMMENT ON COLUMN s1.users.data IS 'in schema'")
        got = rows(
            schema_env,
            session,
            "SELECT d.description FROM pg_description d "
            "JOIN pg_class c ON d.objoid = c.oid "
            "JOIN pg_namespace n ON c.relnamespace = n.oid "
            "WHERE n.nspname = 's1' AND d.objsubid > 0",
        )
        assert got == [("in schema",)]


class TestNotLike:
    def test_not_like_pushdown(self, storage, session):
        run(storage, session, "CREATE TABLE t (id int primary key, n text)")
        run(storage, session, "INSERT INTO t VALUES (1,'pg_catalog'),(2,'public')")
        assert rows(storage, session, "SELECT n FROM t WHERE n NOT LIKE 'pg_%'") == [("public",)]
        assert rows(storage, session, "SELECT n FROM t WHERE n LIKE 'pg_%'") == [("pg_catalog",)]

    def test_not_like_on_catalog(self, storage, session):
        got = rows(
            storage,
            session,
            "SELECT nspname FROM pg_namespace WHERE nspname NOT LIKE 'pg_%' ORDER BY nspname",
        )
        assert got == [("information_schema",), ("public",)]
