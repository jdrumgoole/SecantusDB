"""Statement shapes the pgbench + psql smoke (G7) forced into existence:
multi-name DROP TABLE, VACUUM, ALTER TABLE ADD PRIMARY KEY (row re-keying),
unknown-text numeric coercion, OPERATOR(pg_catalog.~), schema-qualified
array_to_string, comma-join scalar subqueries, IN lists in JOIN ON.
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


def test_multi_name_drop_table(storage, session):
    for n in ("a", "b", "c"):
        run(storage, session, f"CREATE TABLE {n} (id int primary key)")
    res = run_sql(storage, DB, "DROP TABLE IF EXISTS a, b, c, nope", session=session)
    # ONE statement in PG: one result, one tag (pgtest errors:9 reads the
    # simple-query reply byte-for-byte).
    assert [r.command_tag for r in res] == ["DROP TABLE"]
    with pytest.raises(SQLError):
        run(storage, session, "SELECT 1 FROM b")


def test_multi_name_drop_is_atomic_without_if_exists(storage, session):
    # Without IF EXISTS every name must resolve BEFORE anything drops.
    run(storage, session, "CREATE TABLE md_c (id int primary key)")
    with pytest.raises(SQLError) as exc:
        run(storage, session, "DROP TABLE md_c, md_missing")
    assert exc.value.sqlstate == "42P01"
    assert list(run(storage, session, "SELECT count(*) FROM md_c").rows[0]) == [0]


def test_vacuum_accepted(storage, session):
    run(storage, session, "CREATE TABLE t (id int primary key)")
    assert run(storage, session, "VACUUM ANALYZE t").command_tag == "VACUUM"
    assert run(storage, session, "VACUUM").command_tag == "VACUUM"


class TestAddPrimaryKey:
    def test_rekey_and_enforce(self, storage, session):
        run(storage, session, "CREATE TABLE b (bid int, bal int)")
        run(storage, session, "INSERT INTO b VALUES (1, 10), (2, 20)")
        run(storage, session, "ALTER TABLE b ADD PRIMARY KEY (bid)")
        assert rows(storage, session, "SELECT bal FROM b WHERE bid = 2") == [(20,)]
        run(storage, session, "UPDATE b SET bal = 99 WHERE bid = 1")
        assert rows(storage, session, "SELECT bal FROM b ORDER BY bid") == [(99,), (20,)]
        with pytest.raises(SQLError) as exc:
            run(storage, session, "INSERT INTO b VALUES (1, 0)")
        assert exc.value.sqlstate == "23505"

    def test_null_value_rejected(self, storage, session):
        run(storage, session, "CREATE TABLE n (x int)")
        run(storage, session, "INSERT INTO n VALUES (NULL)")
        with pytest.raises(SQLError) as exc:
            run(storage, session, "ALTER TABLE n ADD PRIMARY KEY (x)")
        assert exc.value.sqlstate == "23502"

    def test_duplicate_rejected(self, storage, session):
        run(storage, session, "CREATE TABLE d (x int)")
        run(storage, session, "INSERT INTO d VALUES (1), (1)")
        with pytest.raises(SQLError) as exc:
            run(storage, session, "ALTER TABLE d ADD PRIMARY KEY (x)")
        assert exc.value.sqlstate == "23505"

    def test_existing_pk_rejected(self, storage, session):
        run(storage, session, "CREATE TABLE p (id int primary key, x int)")
        with pytest.raises(SQLError) as exc:
            run(storage, session, "ALTER TABLE p ADD PRIMARY KEY (x)")
        assert exc.value.sqlstate == "42P16"


def test_unknown_text_numeric_coercion(storage, session):
    run(storage, session, "CREATE TABLE acc (aid int primary key, abal int)")
    run(storage, session, "INSERT INTO acc VALUES (1, 10)")
    run(storage, session, "UPDATE acc SET abal = abal + '5' WHERE aid = 1")
    assert rows(storage, session, "SELECT abal FROM acc") == [(15,)]
    with pytest.raises(SQLError) as exc:
        run(storage, session, "UPDATE acc SET abal = abal + 'zap' WHERE aid = 1")
    assert exc.value.sqlstate == "22P02"


def test_explicit_regex_operator(storage, session):
    run(storage, session, "CREATE TABLE r (id int primary key, n text)")
    run(storage, session, "INSERT INTO r VALUES (1, 'abc'), (2, 'xyz')")
    assert rows(
        storage,
        session,
        "SELECT n FROM r WHERE n OPERATOR(pg_catalog.~) '^a' COLLATE \"default\"",
    ) == [("abc",)]


def test_qualified_array_to_string(storage, session):
    assert rows(storage, session, "SELECT pg_catalog.array_to_string(ARRAY['a','b'], ',')") == [
        ("a,b",)
    ]


def test_comma_join_scalar_subquery(storage, session):
    run(storage, session, "CREATE TABLE t1 (id int primary key, v int)")
    run(storage, session, "CREATE TABLE t2 (id int primary key, w int)")
    run(storage, session, "INSERT INTO t1 VALUES (1, 10)")
    run(storage, session, "INSERT INTO t2 VALUES (1, 32)")
    assert rows(
        storage,
        session,
        "SELECT (SELECT a.v + b.w FROM t1 a, t2 b WHERE a.id = 1 AND b.id = 1) AS s",
    ) == [(42,)]


def test_psql_index_listing_join(storage, session):
    run(storage, session, "CREATE TABLE it (id int primary key, x int)")
    run(storage, session, "CREATE INDEX it_x ON it (x)")
    got = rows(
        storage,
        session,
        "SELECT c2.relname FROM pg_index i JOIN pg_class c ON i.indrelid = c.oid "
        "JOIN pg_class c2 ON i.indexrelid = c2.oid "
        "LEFT JOIN pg_constraint con ON con.conrelid = i.indrelid "
        "AND con.conindid = i.indexrelid AND con.contype IN ('p', 'u', 'x') "
        "WHERE c.relname = 'it' ORDER BY c2.relname",
    )
    assert ("it_x",) in got and ("it_pkey",) in got
