"""Join DML: ``DELETE ... USING`` (#162) and ``UPDATE ... SET ... FROM`` (#163).

Both join the target table with one or more source tables and act on the target
rows that match — a semi-join for DELETE, and an update whose SET right-hand sides
may read the source for UPDATE. Driven through ``run_sql`` over the real
WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path, session):
    s = Storage(str(tmp_path))
    run_sql(s, DB, "CREATE TABLE a (id int PRIMARY KEY, n int)", session=session)
    run_sql(s, DB, "INSERT INTO a VALUES (1, 1), (2, 2), (3, 3)", session=session)
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def rows(storage, session, sql):
    return run(storage, session, sql).rows


# -- DELETE ... USING -------------------------------------------------------- #


def test_delete_using_semijoin(storage, session):
    run(storage, session, "CREATE TABLE b (id int)")
    run(storage, session, "INSERT INTO b VALUES (1), (3)")
    res = run(storage, session, "DELETE FROM a USING b WHERE a.id = b.id")
    assert res.command_tag == "DELETE 2"
    assert rows(storage, session, "SELECT id FROM a ORDER BY id") == [(2,)]


def test_delete_using_no_match_deletes_nothing(storage, session):
    run(storage, session, "CREATE TABLE b (id int)")  # empty
    res = run(storage, session, "DELETE FROM a USING b WHERE a.id = b.id")
    assert res.command_tag == "DELETE 0"
    assert rows(storage, session, "SELECT id FROM a ORDER BY id") == [(1,), (2,), (3,)]


def test_delete_using_two_sources(storage, session):
    run(storage, session, "CREATE TABLE b (id int, k int)")
    run(storage, session, "CREATE TABLE c (k int)")
    run(storage, session, "INSERT INTO b VALUES (1, 10), (2, 20), (3, 30)")
    run(storage, session, "INSERT INTO c VALUES (10), (30)")  # matches id 1 and 3
    res = run(storage, session, "DELETE FROM a USING b, c WHERE a.id = b.id AND b.k = c.k")
    assert res.command_tag == "DELETE 2"
    assert rows(storage, session, "SELECT id FROM a ORDER BY id") == [(2,)]


def test_delete_using_returning(storage, session):
    run(storage, session, "CREATE TABLE b (id int)")
    run(storage, session, "INSERT INTO b VALUES (1), (3)")
    res = run(storage, session, "DELETE FROM a USING b WHERE a.id = b.id RETURNING a.id")
    assert sorted(res.rows) == [(1,), (3,)]


def test_delete_using_target_matched_by_many_sources_deletes_once(storage, session):
    run(storage, session, "CREATE TABLE b (id int)")
    run(storage, session, "INSERT INTO b VALUES (1), (1)")  # id=1 twice
    res = run(storage, session, "DELETE FROM a USING b WHERE a.id = b.id")
    assert res.command_tag == "DELETE 1"  # deleted once, not twice
    assert rows(storage, session, "SELECT id FROM a ORDER BY id") == [(2,), (3,)]


# -- UPDATE ... SET ... FROM ------------------------------------------------- #


def test_update_from_copies_source(storage, session):
    run(storage, session, "CREATE TABLE c (id int, bonus int)")
    run(storage, session, "INSERT INTO c VALUES (1, 100), (3, 300)")
    res = run(storage, session, "UPDATE a SET n = c.bonus FROM c WHERE a.id = c.id")
    assert res.command_tag == "UPDATE 2"
    assert rows(storage, session, "SELECT id, n FROM a ORDER BY id") == [(1, 100), (2, 2), (3, 300)]


def test_update_from_expression_over_both(storage, session):
    run(storage, session, "CREATE TABLE c (id int, bonus int)")
    run(storage, session, "INSERT INTO c VALUES (1, 100), (3, 300)")
    run(storage, session, "UPDATE a SET n = n + c.bonus FROM c WHERE a.id = c.id")
    assert rows(storage, session, "SELECT id, n FROM a ORDER BY id") == [(1, 101), (2, 2), (3, 303)]


def test_update_from_subquery_source(storage, session):
    run(
        storage,
        session,
        "UPDATE a SET n = s.v FROM (SELECT 1 AS id, 99 AS v) s WHERE a.id = s.id",
    )
    assert rows(storage, session, "SELECT id, n FROM a ORDER BY id") == [(1, 99), (2, 2), (3, 3)]


def test_update_from_no_match(storage, session):
    run(storage, session, "CREATE TABLE c (id int, bonus int)")  # empty
    res = run(storage, session, "UPDATE a SET n = c.bonus FROM c WHERE a.id = c.id")
    assert res.command_tag == "UPDATE 0"
    assert rows(storage, session, "SELECT id, n FROM a ORDER BY id") == [(1, 1), (2, 2), (3, 3)]


def test_update_from_returning(storage, session):
    run(storage, session, "CREATE TABLE c (id int, bonus int)")
    run(storage, session, "INSERT INTO c VALUES (2, 222)")
    res = run(
        storage, session, "UPDATE a SET n = c.bonus FROM c WHERE a.id = c.id RETURNING a.id, a.n"
    )
    assert res.rows == [(2, 222)]


# -- plain forms are unaffected ---------------------------------------------- #


def test_plain_delete_and_update_still_work(storage, session):
    run(storage, session, "DELETE FROM a WHERE id = 2")
    run(storage, session, "UPDATE a SET n = 7 WHERE id = 1")
    assert rows(storage, session, "SELECT id, n FROM a ORDER BY id") == [(1, 7), (3, 3)]
