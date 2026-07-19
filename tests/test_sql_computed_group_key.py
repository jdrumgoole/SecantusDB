"""Computed GROUP BY keys (#168):

``GROUP BY`` on an *expression* rather than a bare column —
``GROUP BY lower(name)``, ``GROUP BY x + 1``, ``GROUP BY x % 2``,
``GROUP BY coalesce(city, '?')``, ``GROUP BY a || b`` — with the same expression
free to appear in the SELECT list, HAVING, and ORDER BY.

Each computed key is materialised into a synthetic column by a pre-``$group``
``$addFields`` and the ordinary bare-column group machinery takes it from there.
Driven through ``run_sql`` over the real WiredTiger-backed ``Storage``.
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
    return Session(database=DB)


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


def _t(storage, session):
    run(storage, session, "CREATE TABLE t (id int primary key, name text, city text, x int)")
    data = [
        ("Alice", "NY", 1),
        ("alice", None, 3),
        ("BOB", "NY", 10),
        ("bob", "LA", 20),
    ]
    for i, (n, c, x) in enumerate(data):
        cv = "NULL" if c is None else f"'{c}'"
        run(storage, session, f"INSERT INTO t VALUES ({i}, '{n}', {cv}, {x})")


# -- function key ------------------------------------------------------------ #


def test_group_by_lower(storage, session):
    _t(storage, session)
    assert rows(
        storage,
        session,
        "SELECT lower(name) AS g, sum(x) AS s FROM t GROUP BY lower(name) ORDER BY g",
    ) == [("alice", 4), ("bob", 30)]


def test_group_by_lower_unaliased_and_order_by_key(storage, session):
    _t(storage, session)
    # The key is projected under its own synthetic name (no alias) and ORDER BY
    # names the same expression.
    assert rows(
        storage,
        session,
        "SELECT lower(name), sum(x) FROM t GROUP BY lower(name) ORDER BY lower(name)",
    ) == [("alice", 4), ("bob", 30)]


def test_group_by_lower_order_desc_on_alias(storage, session):
    _t(storage, session)
    assert rows(
        storage,
        session,
        "SELECT lower(name) AS g, count(*) FROM t GROUP BY lower(name) ORDER BY g DESC",
    ) == [("bob", 2), ("alice", 2)]


def test_group_by_length(storage, session):
    _t(storage, session)
    assert rows(
        storage,
        session,
        "SELECT length(name) AS l, count(*) FROM t GROUP BY length(name) ORDER BY l",
    ) == [(3, 2), (5, 2)]


# -- arithmetic key ---------------------------------------------------------- #


def test_group_by_arithmetic(storage, session):
    _t(storage, session)
    assert rows(
        storage,
        session,
        "SELECT x + 1 AS k, count(*) FROM t GROUP BY x + 1 ORDER BY k",
    ) == [(2, 1), (4, 1), (11, 1), (21, 1)]


def test_group_by_modulo(storage, session):
    _t(storage, session)
    assert rows(
        storage,
        session,
        "SELECT x % 2 AS m, sum(x) AS s FROM t GROUP BY x % 2 ORDER BY m",
    ) == [(0, 30), (1, 4)]


# -- coalesce / concat ------------------------------------------------------- #


def test_group_by_coalesce(storage, session):
    _t(storage, session)
    assert rows(
        storage,
        session,
        "SELECT coalesce(city, '?') AS c, count(*) FROM t GROUP BY coalesce(city, '?') ORDER BY c",
    ) == [("?", 1), ("LA", 1), ("NY", 2)]


def test_group_by_concat(storage, session):
    _t(storage, session)
    assert rows(
        storage,
        session,
        "SELECT lower(name) || '!' AS g, count(*) FROM t GROUP BY lower(name) || '!' ORDER BY g",
    ) == [("alice!", 2), ("bob!", 2)]


# -- HAVING / mixed keys / whole-table -------------------------------------- #


def test_group_by_computed_with_having(storage, session):
    _t(storage, session)
    assert rows(
        storage,
        session,
        "SELECT lower(name) AS g, sum(x) AS s FROM t "
        "GROUP BY lower(name) HAVING sum(x) > 5 ORDER BY g",
    ) == [("bob", 30)]


def test_group_by_bare_column_and_computed(storage, session):
    _t(storage, session)
    assert rows(
        storage,
        session,
        "SELECT city, lower(name) AS g, sum(x) AS s FROM t "
        "GROUP BY city, lower(name) ORDER BY city NULLS LAST, g",
    ) == [("LA", "bob", 20), ("NY", "alice", 1), ("NY", "bob", 10), (None, "alice", 3)]


def test_computed_key_with_expression_over_aggregate(storage, session):
    _t(storage, session)
    # exercises the group-window (evaluated) path: computed key + sum(x)+1.
    assert rows(
        storage,
        session,
        "SELECT lower(name) AS g, sum(x) + 1 AS s FROM t GROUP BY lower(name) ORDER BY g",
    ) == [("alice", 5), ("bob", 31)]


# -- unsupported function ---------------------------------------------------- #


def test_group_by_key_beyond_pipeline_evaluates_in_python(storage, session):
    # A key the aggregation engine can't lower (substr's Mongo lowering isn't
    # mapped) falls back to per-doc Python evaluation before the pipeline.
    _t(storage, session)
    res = run(
        storage,
        session,
        "SELECT substr(name, 1, 1) AS g, count(*) FROM t GROUP BY substr(name, 1, 1)",
    )
    assert sorted(res.rows) == [("A", 1), ("B", 1), ("a", 1), ("b", 1)]


def test_group_by_comparison_key_and_ordinal(storage, session):
    # ``GROUP BY 1`` resolves the ordinal to the select expression, and a
    # comparison-shaped key groups (typed bool) via the Python pre-eval path.
    _t(storage, session)
    res = run(
        storage,
        session,
        "SELECT name = 'alice' AS is_alice, count(*) FROM t GROUP BY 1 ORDER BY 1",
    )
    assert res.rows == [(False, 3), (True, 1)]
    assert res.columns[0].type_tag == "bool"


# -- computed keys over a JOIN (#169) ---------------------------------------- #


def _join(storage, session):
    run(storage, session, "CREATE TABLE cust (name text primary key, region text)")
    run(storage, session, "CREATE TABLE ord (id int primary key, cust text, amt int)")
    run(storage, session, "INSERT INTO cust VALUES ('a', 'East'), ('b', 'EAST'), ('c', 'west')")
    for i, (c, a) in enumerate([("a", 4), ("b", 8), ("c", 30)]):
        run(storage, session, f"INSERT INTO ord VALUES ({i}, '{c}', {a})")


def test_join_group_by_lower(storage, session):
    _join(storage, session)
    # East + EAST fold to 'east' (4 + 8); west stays 30.
    assert rows(
        storage,
        session,
        "SELECT lower(c.region) AS r, sum(o.amt) AS s FROM ord o "
        "JOIN cust c ON o.cust = c.name GROUP BY lower(c.region) ORDER BY r",
    ) == [("east", 12), ("west", 30)]


def test_join_group_by_arithmetic(storage, session):
    _join(storage, session)
    assert rows(
        storage,
        session,
        "SELECT o.amt + 1 AS k, count(*) FROM ord o "
        "JOIN cust c ON o.cust = c.name GROUP BY o.amt + 1 ORDER BY k",
    ) == [(5, 1), (9, 1), (31, 1)]


def test_join_group_by_computed_with_having(storage, session):
    _join(storage, session)
    assert rows(
        storage,
        session,
        "SELECT lower(c.region) AS r, sum(o.amt) AS s FROM ord o "
        "JOIN cust c ON o.cust = c.name GROUP BY lower(c.region) HAVING sum(o.amt) > 5 ORDER BY r",
    ) == [("east", 12), ("west", 30)]


def test_join_group_by_unsupported_function_rejected(storage, session):
    _join(storage, session)
    with pytest.raises(SQLError) as ei:
        run(
            storage,
            session,
            "SELECT substr(c.region, 1, 1) AS r, count(*) FROM ord o "
            "JOIN cust c ON o.cust = c.name GROUP BY substr(c.region, 1, 1)",
        )
    assert ei.value.sqlstate == "0A000"


# -- computed keys over GROUPING SETS / ROLLUP / CUBE (#170) ------------------ #


def _sales(storage, session):
    run(
        storage, session, "CREATE TABLE sales (id int primary key, region text, dept text, amt int)"
    )
    for i, (r, dp, a) in enumerate([("E", "x", 10), ("e", "y", 20), ("W", "x", 30)]):
        run(storage, session, f"INSERT INTO sales VALUES ({i}, '{r}', '{dp}', {a})")


def test_rollup_computed_key(storage, session):
    _sales(storage, session)
    # E + e fold to 'e' (10 + 20); W → 'w' (30); grand-total row (NULL) = 60.
    assert rows(
        storage,
        session,
        "SELECT lower(region) AS r, sum(amt) AS s FROM sales "
        "GROUP BY ROLLUP(lower(region)) ORDER BY r NULLS LAST",
    ) == [("e", 30), ("w", 30), (None, 60)]


def test_cube_computed_key(storage, session):
    _sales(storage, session)
    assert rows(
        storage,
        session,
        "SELECT lower(region) AS r, dept, sum(amt) AS s FROM sales "
        "GROUP BY CUBE(lower(region), dept) ORDER BY r NULLS LAST, dept NULLS LAST",
    ) == [
        ("e", "x", 10),
        ("e", "y", 20),
        ("e", None, 30),
        ("w", "x", 30),
        ("w", None, 30),
        (None, "x", 40),
        (None, "y", 20),
        (None, None, 60),
    ]


def test_grouping_sets_computed_key(storage, session):
    _sales(storage, session)
    assert rows(
        storage,
        session,
        "SELECT lower(region) AS r, sum(amt) AS s FROM sales "
        "GROUP BY GROUPING SETS ((lower(region)), ()) ORDER BY r NULLS LAST",
    ) == [("e", 30), ("w", 30), (None, 60)]


def test_grouping_bitmask_on_computed_key(storage, session):
    _sales(storage, session)
    # GROUPING(lower(region)) is 0 on a real group, 1 on the rolled-up total.
    assert rows(
        storage,
        session,
        "SELECT lower(region) AS r, GROUPING(lower(region)) AS g, sum(amt) AS s "
        "FROM sales GROUP BY ROLLUP(lower(region)) ORDER BY g, r",
    ) == [("e", 0, 30), ("w", 0, 30), (None, 1, 60)]
