"""``WITH … <write>`` — a CTE prefix on INSERT / UPDATE / DELETE.

The CTEs materialize the same way as for a SELECT, then the write body runs
against the CTE-aware backend + catalog overlay: an ``INSERT … SELECT FROM cte``
reads the CTE as its source, and an ``UPDATE`` / ``DELETE`` whose WHERE has a
subquery over a CTE resolves it. Writes forward to real storage.
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
def storage(tmp_path):
    s = Storage(str(tmp_path))
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE src (id bigint primary key, region text, amount int)")
    s.q("CREATE TABLE dst (id bigint primary key, region text, amount int)")
    s.q("CREATE TABLE t (id bigint primary key, n int)")
    for i, r, a in [(1, "e", 10), (2, "e", 20), (3, "w", 30)]:
        s.q(f"INSERT INTO src (id, region, amount) VALUES ({i}, '{r}', {a})")
    for i, n in [(1, 5), (2, 15), (3, 25)]:
        s.q(f"INSERT INTO t (id, n) VALUES ({i}, {n})")
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def test_with_insert_select(storage, session):
    res = q(
        storage,
        session,
        "WITH big AS (SELECT id, region, amount FROM src WHERE amount >= 20) "
        "INSERT INTO dst (id, region, amount) SELECT id, region, amount FROM big",
    )
    assert res.command_tag == "INSERT 0 2"
    assert q(storage, session, "SELECT id, region FROM dst ORDER BY id").rows == [
        (2, "e"),
        (3, "w"),
    ]


def test_with_update_where_subquery(storage, session):
    res = q(
        storage,
        session,
        "WITH hi AS (SELECT id FROM t WHERE n > 10) "
        "UPDATE t SET n = 0 WHERE id IN (SELECT id FROM hi)",
    )
    assert res.command_tag == "UPDATE 2"
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [(1, 5), (2, 0), (3, 0)]


def test_with_delete_where_subquery(storage, session):
    res = q(
        storage,
        session,
        "WITH gone AS (SELECT id FROM src WHERE region = 'e') "
        "DELETE FROM src WHERE id IN (SELECT id FROM gone)",
    )
    assert res.command_tag == "DELETE 2"
    assert q(storage, session, "SELECT id FROM src ORDER BY id").rows == [(3,)]


def test_with_insert_returning_computed(storage, session):
    res = q(
        storage,
        session,
        "WITH one AS (SELECT 42 AS v) "
        "INSERT INTO t (id, n) SELECT 9, v FROM one RETURNING id, n * 2 AS dbl",
    )
    assert res.rows == [(9, 84)]


def test_with_multiple_ctes_on_insert(storage, session):
    res = q(
        storage,
        session,
        "WITH a AS (SELECT id, amount FROM src WHERE region = 'e'), "
        "b AS (SELECT id, amount FROM a WHERE amount > 15) "
        "INSERT INTO dst (id, region, amount) SELECT id, 'x', amount FROM b",
    )
    assert res.command_tag == "INSERT 0 1"
    assert q(storage, session, "SELECT id, region, amount FROM dst ORDER BY id").rows == [
        (2, "x", 20)
    ]


# --- Data-modifying CTEs (#147) -------------------------------------------
# A CTE body that is itself INSERT/UPDATE/DELETE (optionally … RETURNING). The
# write executes for its side effects; its RETURNING rows feed the rest of the
# statement — the classic "move rows between tables in one statement" idiom.


def test_datamod_cte_delete_returning_feeds_insert(storage, session):
    res = q(
        storage,
        session,
        "WITH moved AS (DELETE FROM src WHERE amount >= 20 RETURNING id, region, amount) "
        "INSERT INTO dst (id, region, amount) SELECT id, region, amount FROM moved",
    )
    assert res.command_tag == "INSERT 0 2"
    # The rows really left src and landed in dst.
    assert q(storage, session, "SELECT id FROM src ORDER BY id").rows == [(1,)]
    assert q(storage, session, "SELECT id, amount FROM dst ORDER BY id").rows == [
        (2, 20),
        (3, 30),
    ]


def test_datamod_cte_insert_returning_feeds_select(storage, session):
    res = q(
        storage,
        session,
        "WITH ins AS "
        "(INSERT INTO dst (id, region, amount) VALUES (7, 'z', 70) RETURNING id, amount) "
        "SELECT id, amount FROM ins",
    )
    assert res.rows == [(7, 70)]
    assert q(storage, session, "SELECT id FROM dst").rows == [(7,)]


def test_datamod_cte_update_returning_feeds_aggregate(storage, session):
    res = q(
        storage,
        session,
        "WITH upd AS (UPDATE src SET amount = 0 WHERE region = 'e' RETURNING id, amount) "
        "SELECT count(*) AS c, sum(amount) AS s FROM upd",
    )
    assert res.rows == [(2, 0)]
    assert q(storage, session, "SELECT id, amount FROM src ORDER BY id").rows == [
        (1, 0),
        (2, 0),
        (3, 30),
    ]


def test_datamod_cte_without_returning_still_runs(storage, session):
    # No RETURNING: the DELETE still executes; the outer query just doesn't read it.
    res = q(
        storage,
        session,
        "WITH d AS (DELETE FROM src WHERE region = 'e') SELECT count(*) AS remaining FROM src",
    )
    assert res.rows == [(1,)]


def test_recursive_before_write_body(storage, session):
    # WITH RECURSIVE feeding an INSERT body (the recursive CTE materializes first).
    res = q(
        storage,
        session,
        "WITH RECURSIVE nums(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM nums WHERE x < 4) "
        "INSERT INTO dst (id, region, amount) SELECT x, 'n', x * 10 FROM nums",
    )
    assert res.command_tag == "INSERT 0 4"
    assert q(storage, session, "SELECT id, amount FROM dst ORDER BY id").rows == [
        (1, 10),
        (2, 20),
        (3, 30),
        (4, 40),
    ]


def test_recursive_and_datamod_cte_combined(storage, session):
    res = q(
        storage,
        session,
        "WITH RECURSIVE nums(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM nums WHERE x < 3), "
        "ins AS (INSERT INTO dst (id, region, amount) SELECT x, 'r', x FROM nums RETURNING id) "
        "SELECT count(*) AS c FROM ins",
    )
    assert res.rows == [(3,)]
    assert q(storage, session, "SELECT id FROM dst ORDER BY id").rows == [(1,), (2,), (3,)]


def test_with_merge(storage, session):
    # WITH before a MERGE (#169): the CTE materialises, then the MERGE runs against
    # the CTE-aware backend. big = src rows with amount >= 20 → ids 2, 3.
    q(storage, session, "INSERT INTO dst (id, region, amount) VALUES (2, 'x', 0)")
    res = q(
        storage,
        session,
        "WITH big AS (SELECT id, region, amount FROM src WHERE amount >= 20) "
        "MERGE INTO dst d USING big b ON d.id = b.id "
        "WHEN MATCHED THEN UPDATE SET amount = b.amount "
        "WHEN NOT MATCHED THEN INSERT (id, region, amount) VALUES (b.id, b.region, b.amount)",
    )
    assert res.command_tag == "MERGE 2"
    # id 2 matched → amount updated to 20; id 3 not matched → inserted.
    assert q(storage, session, "SELECT id, amount FROM dst ORDER BY id").rows == [
        (2, 20),
        (3, 30),
    ]
