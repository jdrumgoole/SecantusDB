"""``WITH RECURSIVE`` — recursive common table expressions.

A recursive CTE is a ``UNION [ALL]`` of an anchor (seed) term and a recursive
term that references the CTE. The engine evaluates it by semi-naive iteration:
run the anchor, then repeatedly run the recursive term against just the rows the
previous step produced (registered under the CTE name) until it yields nothing
new. ``UNION`` dedups against all rows seen (so a cyclic graph terminates);
``UNION ALL`` keeps every row (guarded against runaway recursion).
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, engine, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def test_counter_union_all(storage, session):
    res = q(
        storage,
        session,
        "WITH RECURSIVE nums(n) AS ("
        "  SELECT 1 UNION ALL SELECT n + 1 FROM nums WHERE n < 5"
        ") SELECT n FROM nums ORDER BY n",
    )
    assert res.rows == [(1,), (2,), (3,), (4,), (5,)]


def test_recursive_result_feeds_aggregate(storage, session):
    res = q(
        storage,
        session,
        "WITH RECURSIVE nums(n) AS ("
        "  SELECT 1 UNION ALL SELECT n + 1 FROM nums WHERE n < 5"
        ") SELECT SUM(n) AS s FROM nums",
    )
    assert res.rows == [(15,)]


def test_hierarchy_walk_with_join(storage, session):
    storage.q = lambda sql: run_sql(storage, DB, sql, session=session)[0]
    storage.q("CREATE TABLE emp (id bigint primary key, name text, mgr int)")
    for i, n, m in [(1, "ceo", 0), (2, "vp1", 1), (3, "vp2", 1), (4, "eng", 2), (5, "eng2", 4)]:
        storage.q(f"INSERT INTO emp (id, name, mgr) VALUES ({i}, '{n}', {m})")
    res = q(
        storage,
        session,
        "WITH RECURSIVE chain(id, name, lvl) AS ("
        "  SELECT id, name, 0 FROM emp WHERE id = 1"
        "  UNION ALL"
        "  SELECT e.id, e.name, c.lvl + 1 FROM emp e JOIN chain c ON e.mgr = c.id"
        ") SELECT id, name, lvl FROM chain ORDER BY id",
    )
    assert res.rows == [(1, "ceo", 0), (2, "vp1", 1), (3, "vp2", 1), (4, "eng", 2), (5, "eng2", 3)]


def test_union_distinct_terminates_on_cycle(storage, session):
    storage.q = lambda sql: run_sql(storage, DB, sql, session=session)[0]
    storage.q("CREATE TABLE edge (id bigint primary key, a int, b int)")
    for i, a, b in [(1, 1, 2), (2, 2, 3), (3, 3, 1)]:  # cycle 1->2->3->1
        storage.q(f"INSERT INTO edge (id, a, b) VALUES ({i}, {a}, {b})")
    res = q(
        storage,
        session,
        "WITH RECURSIVE reach(node) AS ("
        "  SELECT 1"
        "  UNION"
        "  SELECT e.b FROM edge e JOIN reach r ON e.a = r.node"
        ") SELECT node FROM reach ORDER BY node",
    )
    # UNION dedup stops the 1->2->3->1 cycle once every node is seen.
    assert res.rows == [(1,), (2,), (3,)]


def test_empty_anchor_yields_no_rows(storage, session):
    storage.q = lambda sql: run_sql(storage, DB, sql, session=session)[0]
    storage.q("CREATE TABLE seed (id bigint primary key, n int)")  # empty
    res = q(
        storage,
        session,
        "WITH RECURSIVE r(n) AS ("
        "  SELECT n FROM seed UNION ALL SELECT n + 1 FROM r WHERE n < 3"
        ") SELECT n FROM r",
    )
    assert res.rows == []


def test_non_recursive_cte_in_recursive_with(storage, session):
    # A WITH RECURSIVE may hold a CTE that never references itself; it runs as a
    # plain (non-recursive) CTE.
    res = q(
        storage,
        session,
        "WITH RECURSIVE base AS (SELECT 7 AS x) SELECT x FROM base",
    )
    assert res.rows == [(7,)]


def test_column_alias_count_mismatch_rejected(storage, session):
    with pytest.raises(SQLError) as ei:
        q(
            storage,
            session,
            "WITH RECURSIVE r(a, b) AS ("
            "  SELECT 1 UNION ALL SELECT a + 1 FROM r WHERE a < 2"
            ") SELECT a FROM r",
        )
    assert ei.value.sqlstate == "42601"


def test_runaway_recursion_guarded(storage, session, monkeypatch):
    # A cyclic graph under UNION ALL recurses forever; the row cap stops it with a
    # clear error rather than hanging. Patch the cap low so the test is fast.
    monkeypatch.setattr(engine, "_MAX_RECURSION_ROWS", 50)
    storage.q = lambda sql: run_sql(storage, DB, sql, session=session)[0]
    storage.q("CREATE TABLE edge (id bigint primary key, a int, b int)")
    for i, a, b in [(1, 1, 2), (2, 2, 1)]:  # cycle 1<->2
        storage.q(f"INSERT INTO edge (id, a, b) VALUES ({i}, {a}, {b})")
    with pytest.raises(SQLError) as ei:
        q(
            storage,
            session,
            "WITH RECURSIVE reach(node) AS ("
            "  SELECT 1 UNION ALL SELECT e.b FROM edge e JOIN reach r ON e.a = r.node"
            ") SELECT node FROM reach",
        )
    assert ei.value.sqlstate == "54001"
