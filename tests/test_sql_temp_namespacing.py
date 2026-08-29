"""Per-session temp-table namespacing.

Postgres gives every backend its own ``pg_temp_<n>`` schema, so two
concurrently-open sessions can each ``CREATE TEMPORARY TABLE bar`` without
colliding, and each session's ``bar`` shadows a permanent ``bar``. We shared
one namespace: the second concurrent create failed 42P07 (the confirmed gap
in tasks/backlog.md's pgx survey). Temp tables now live under a per-session
``pg_temp_<n>.`` catalog prefix, allocated lazily and resolved implicitly
ahead of ``public`` — matching real PG's search order.
"""

from __future__ import annotations

import psycopg
import pytest

from secantus.sql import engine
from secantus.sql.errors import SQLError
from secantus.sql.pgserver import SecantusPGServer
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return engine.run_sql(storage, session.database, sql, session=session)


def rows(storage, session, sql):
    return run(storage, session, sql)[0].rows


class TestConcurrentSessions:
    def test_same_name_temp_tables_do_not_collide(self, storage):
        s1, s2 = Session(database="db"), Session(database="db")
        run(storage, s1, "CREATE TEMPORARY TABLE bar (a int4)")
        run(storage, s2, "CREATE TEMPORARY TABLE bar (a int4)")
        assert s1.temp_schema != s2.temp_schema

    def test_each_session_sees_only_its_own_rows(self, storage):
        s1, s2 = Session(database="db"), Session(database="db")
        run(storage, s1, "CREATE TEMP TABLE bar (a int4)")
        run(storage, s2, "CREATE TEMP TABLE bar (a int4)")
        run(storage, s1, "INSERT INTO bar VALUES (1)")
        run(storage, s2, "INSERT INTO bar VALUES (2), (3)")
        assert rows(storage, s1, "SELECT count(*) FROM bar") == [(1,)]
        assert rows(storage, s2, "SELECT count(*) FROM bar") == [(2,)]

    def test_teardown_drops_only_that_sessions_tables(self, storage):
        s1, s2 = Session(database="db"), Session(database="db")
        run(storage, s1, "CREATE TEMP TABLE bar (a int4)")
        run(storage, s2, "CREATE TEMP TABLE bar (a int4)")
        run(storage, s2, "INSERT INTO bar VALUES (2)")
        engine.drop_session_temp_tables(storage, s1)
        assert rows(storage, s2, "SELECT a FROM bar") == [(2,)]
        with pytest.raises(SQLError) as exc:
            run(storage, s1, "SELECT * FROM bar")
        assert exc.value.sqlstate == "42P01"


class TestResolution:
    def test_temp_shadows_permanent(self, storage):
        s1, s2 = Session(database="db"), Session(database="db")
        run(storage, s1, "CREATE TABLE shadow (a int4)")
        run(storage, s1, "INSERT INTO shadow VALUES (10)")
        run(storage, s1, "CREATE TEMP TABLE shadow (a int4)")
        run(storage, s1, "INSERT INTO shadow VALUES (99)")
        assert rows(storage, s1, "SELECT a FROM shadow") == [(99,)]
        # A session without its own temp shadow sees the permanent table.
        assert rows(storage, s2, "SELECT a FROM shadow") == [(10,)]

    def test_drop_resolves_temp_first_and_unshadows(self, storage):
        s = Session(database="db")
        run(storage, s, "CREATE TABLE shadow (a int4)")
        run(storage, s, "INSERT INTO shadow VALUES (10)")
        run(storage, s, "CREATE TEMP TABLE shadow (a int4)")
        run(storage, s, "DROP TABLE shadow")
        assert rows(storage, s, "SELECT a FROM shadow") == [(10,)]

    def test_explicit_pg_temp_qualifier(self, storage):
        s = Session(database="db")
        run(storage, s, "CREATE TEMP TABLE bar (a int4)")
        run(storage, s, "INSERT INTO pg_temp.bar VALUES (7)")
        assert rows(storage, s, "SELECT a FROM pg_temp.bar") == [(7,)]

    def test_create_table_pg_temp_dot_name_is_temp(self, storage):
        s = Session(database="db")
        run(storage, s, "CREATE TABLE pg_temp.px (a int4)")
        assert ("db", f"{s.temp_schema}.px") in s.temp_tables
        engine.drop_session_temp_tables(storage, s)
        with pytest.raises(SQLError) as exc:
            run(storage, s, "SELECT * FROM px")
        assert exc.value.sqlstate == "42P01"

    def test_in_session_duplicate_is_42P07_with_bare_name(self, storage):
        s = Session(database="db")
        run(storage, s, "CREATE TEMP TABLE bar (a int4)")
        with pytest.raises(SQLError) as exc:
            run(storage, s, "CREATE TEMP TABLE bar (a int4)")
        assert exc.value.sqlstate == "42P07"
        assert 'relation "bar" already exists' in str(exc.value)

    def test_temp_into_named_schema_is_rejected(self, storage):
        s = Session(database="db")
        run(storage, s, "CREATE SCHEMA myschema")
        with pytest.raises(SQLError) as exc:
            run(storage, s, "CREATE TEMP TABLE myschema.t (a int4)")
        assert exc.value.sqlstate == "42P16"

    def test_serial_sequences_are_per_session(self, storage):
        s1, s2 = Session(database="db"), Session(database="db")
        run(storage, s1, "CREATE TEMP TABLE ser (id serial PRIMARY KEY, v int4)")
        run(storage, s2, "CREATE TEMP TABLE ser (id serial PRIMARY KEY, v int4)")
        run(storage, s1, "INSERT INTO ser (v) VALUES (7)")
        run(storage, s1, "INSERT INTO ser (v) VALUES (8)")
        run(storage, s2, "INSERT INTO ser (v) VALUES (9)")
        assert rows(storage, s1, "SELECT id, v FROM ser ORDER BY id") == [(1, 7), (2, 8)]
        assert rows(storage, s2, "SELECT id, v FROM ser") == [(1, 9)]


class TestCatalogSurfaces:
    def test_pg_class_relname_is_bare(self, storage):
        s = Session(database="db")
        run(storage, s, "CREATE TEMP TABLE tmp_t (a int4)")
        got = rows(
            storage,
            s,
            "SELECT relname FROM pg_class WHERE relkind = 'r' AND relpersistence = 't'",
        )
        assert ("tmp_t",) in got

    def test_information_schema_reports_session_namespace(self, storage):
        s = Session(database="db")
        run(storage, s, "CREATE TEMP TABLE tmp_t (a int4)")
        got = rows(
            storage,
            s,
            "SELECT table_schema, table_type FROM information_schema.tables "
            "WHERE table_name = 'tmp_t'",
        )
        assert got == [(s.temp_schema, "LOCAL TEMPORARY")]


@pytest.fixture()
def server(tmp_path):
    srv = SecantusPGServer(storage_path=str(tmp_path), port=0)
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture()
def dsn(server):
    host, port = server.address
    return f"host={host} port={port} dbname=test user=test password=test"


class TestWireConcurrency:
    def test_two_open_connections_create_same_temp_table(self, dsn):
        # The confirmed gap from tasks/backlog.md: the second concurrent
        # CREATE TEMPORARY TABLE failed 42P07; Postgres allows both.
        with (
            psycopg.connect(dsn, autocommit=True) as c1,
            psycopg.connect(dsn, autocommit=True) as c2,
        ):
            c1.execute("CREATE TEMPORARY TABLE bar (a int4)")
            c2.execute("CREATE TEMPORARY TABLE bar (a int4)")
            c1.execute("INSERT INTO bar VALUES (1)")
            c2.execute("INSERT INTO bar VALUES (2), (3)")
            assert c1.execute("SELECT count(*) FROM bar").fetchone() == (1,)
            assert c2.execute("SELECT count(*) FROM bar").fetchone() == (2,)

    def test_temp_table_invisible_to_other_connection(self, dsn):
        with (
            psycopg.connect(dsn, autocommit=True) as c1,
            psycopg.connect(dsn, autocommit=True) as c2,
        ):
            c1.execute("CREATE TEMP TABLE mine (a int4)")
            with pytest.raises(psycopg.errors.UndefinedTable):
                c2.execute("SELECT * FROM mine")

    def test_disconnect_drops_and_name_is_reusable(self, dsn):
        with psycopg.connect(dsn, autocommit=True) as c1:
            c1.execute("CREATE TEMP TABLE gone (a int4)")
        with psycopg.connect(dsn, autocommit=True) as c2:
            c2.execute("CREATE TEMP TABLE gone (a int4)")
            with pytest.raises(psycopg.errors.UndefinedTable):
                c2.execute("SELECT * FROM public.gone")

    def test_copy_from_stdin_into_temp_table(self, dsn):
        # pgx's TestConnCopyFrom shape: COPY resolves the session's temp
        # table (COPY rides the wire server's own path, not _run_statement).
        with psycopg.connect(dsn, autocommit=True) as c:
            c.execute("CREATE TEMP TABLE ct (a int4, b text)")
            with c.cursor() as cur, cur.copy("COPY ct FROM STDIN") as copy:
                copy.write("1\tx\n2\ty\n")
            assert c.execute("SELECT a, b FROM ct ORDER BY a").fetchall() == [
                (1, "x"),
                (2, "y"),
            ]

    def test_copy_to_stdout_from_temp_table(self, dsn):
        with psycopg.connect(dsn, autocommit=True) as c:
            c.execute("CREATE TEMP TABLE cout (a int4)")
            c.execute("INSERT INTO cout VALUES (5)")
            with c.cursor() as cur, cur.copy("COPY cout TO STDOUT") as copy:
                data = b"".join(bytes(chunk) for chunk in copy)
            assert data == b"5\n"

    def test_extended_protocol_describe_on_temp_table(self, dsn):
        # Parameterised (extended-protocol) statements Describe before
        # Execute; the describe path must resolve the temp namespace too.
        with psycopg.connect(dsn, autocommit=True) as c:
            c.execute("CREATE TEMP TABLE ext (a int4)")
            c.execute("INSERT INTO ext VALUES (%s)", (42,))
            assert c.execute("SELECT a FROM ext WHERE a = %s", (42,)).fetchall() == [(42,)]


class TestNamespacingFallout:
    # Two regressions the psycopg gauge caught after the namespacing landed
    # (its test_diag_attr_values / test_diag_from_commit): error diagnostics
    # leaked the pg_temp_<n>. catalog prefix into the TABLE NAME field, and a
    # SELF-referencing FK inside CREATE TEMP TABLE captured its target by the
    # pre-rewrite bare name, so enforcement never found the relation.
    def test_diag_table_name_is_bare(self, dsn):
        with psycopg.connect(dsn, autocommit=True) as c:
            c.execute("create temp table texc (data int constraint chk1 check (data = 1))")
            with pytest.raises(psycopg.errors.CheckViolation) as ei:
                c.execute("insert into texc values (2)")
            assert ei.value.diag.table_name == "texc"
            assert ei.value.diag.schema_name.startswith("pg_temp")
            assert ei.value.diag.constraint_name == "chk1"

    def test_self_referencing_deferred_fk_fires_at_commit(self, dsn):
        with psycopg.connect(dsn) as c:
            cur = c.cursor()
            cur.execute(
                "create temp table tdef (data int primary key, "
                "ref int references tdef (data) deferrable initially deferred)"
            )
            cur.execute("insert into tdef values (1, 2)")
            with pytest.raises(psycopg.errors.ForeignKeyViolation):
                c.commit()
