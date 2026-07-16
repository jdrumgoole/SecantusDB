"""LATERAL joins and ``DISTINCT ON`` — PostgreSQL extensions to plain joins /
DISTINCT.

- ``DISTINCT ON (exprs)`` keeps the first row per distinct value of ``exprs``, in
  the query's ORDER BY order (route through the evaluated path's
  sort-then-keep-first-per-key). This is distinct from plain ``DISTINCT`` (dedup
  the whole output row).
- A ``LATERAL`` subquery correlates against the preceding FROM items; it lowers
  to a correlated ``$lookup`` (``let`` / sub-``pipeline``) + ``$unwind`` — the
  canonical "expand related rows" and "top-N per group" shapes.
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

    def q(sql):
        run_sql(s, DB, sql, session=session)

    q("CREATE TABLE sales (id bigint primary key, region text, amount int)")
    for i, (r, a) in enumerate([("e", 10), ("e", 30), ("w", 20), ("w", 50), ("w", 5)], 1):
        q(f"INSERT INTO sales (id, region, amount) VALUES ({i}, '{r}', {a})")
    q("CREATE TABLE t (id bigint primary key, name text)")
    q("CREATE TABLE u (id bigint primary key, tid bigint, val int)")
    for i, n in [(1, "a"), (2, "b"), (3, "c")]:
        q(f"INSERT INTO t (id, name) VALUES ({i}, '{n}')")
    for i, (tid, v) in enumerate([(1, 10), (1, 40), (2, 30), (2, 5), (2, 20)], 1):
        q(f"INSERT INTO u (id, tid, val) VALUES ({i}, {tid}, {v})")
    try:
        yield s
    finally:
        s.close()


def rows(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1].rows


# -- DISTINCT ON ------------------------------------------------------------ #


def test_distinct_on_keeps_highest_per_group(storage, session):
    assert rows(
        storage,
        session,
        "SELECT DISTINCT ON (region) region, amount FROM sales ORDER BY region, amount DESC",
    ) == [("e", 30), ("w", 50)]


def test_distinct_on_keeps_lowest_per_group(storage, session):
    assert rows(
        storage,
        session,
        "SELECT DISTINCT ON (region) region, amount FROM sales ORDER BY region, amount",
    ) == [("e", 10), ("w", 5)]


def test_distinct_on_carries_other_columns(storage, session):
    assert rows(
        storage,
        session,
        "SELECT DISTINCT ON (region) region, id, amount FROM sales ORDER BY region, amount DESC",
    ) == [("e", 2, 30), ("w", 4, 50)]


def test_plain_distinct_still_dedups_whole_row(storage, session):
    assert rows(storage, session, "SELECT DISTINCT region FROM sales ORDER BY region") == [
        ("e",),
        ("w",),
    ]


def test_distinct_on_over_join(storage, session):
    # Highest-val u row per t.name, across the join.
    assert rows(
        storage,
        session,
        "SELECT DISTINCT ON (t.name) t.name, u.val "
        "FROM t JOIN u ON u.tid = t.id ORDER BY t.name, u.val DESC",
    ) == [("a", 40), ("b", 30)]


# -- LATERAL ---------------------------------------------------------------- #


def test_comma_lateral_expands(storage, session):
    assert rows(
        storage,
        session,
        "SELECT t.name, s.val FROM t, LATERAL (SELECT val FROM u WHERE u.tid = t.id) s "
        "ORDER BY t.name, s.val",
    ) == [("a", 10), ("a", 40), ("b", 5), ("b", 20), ("b", 30)]


def test_cross_join_lateral_top_1(storage, session):
    assert rows(
        storage,
        session,
        "SELECT t.name, s.val FROM t CROSS JOIN LATERAL "
        "(SELECT val FROM u WHERE u.tid = t.id ORDER BY val DESC LIMIT 1) s ORDER BY t.name",
    ) == [("a", 40), ("b", 30)]


def test_join_lateral_on_true(storage, session):
    assert rows(
        storage,
        session,
        "SELECT t.name, s.val FROM t JOIN LATERAL "
        "(SELECT val FROM u WHERE u.tid = t.id ORDER BY val DESC LIMIT 1) s ON true "
        "ORDER BY t.name",
    ) == [("a", 40), ("b", 30)]


def test_left_join_lateral_preserves_unmatched(storage, session):
    # t=c has no u rows → preserved with a NULL from the lateral.
    assert rows(
        storage,
        session,
        "SELECT t.name, s.val FROM t LEFT JOIN LATERAL "
        "(SELECT val FROM u WHERE u.tid = t.id ORDER BY val DESC LIMIT 1) s ON true "
        "ORDER BY t.name",
    ) == [("a", 40), ("b", 30), ("c", None)]


def test_lateral_top_2_per_group(storage, session):
    assert rows(
        storage,
        session,
        "SELECT t.name, s.val FROM t CROSS JOIN LATERAL "
        "(SELECT val FROM u WHERE u.tid = t.id ORDER BY val DESC LIMIT 2) s "
        "ORDER BY t.name, s.val DESC",
    ) == [("a", 40), ("a", 10), ("b", 30), ("b", 20)]


# -- Rich LATERAL: correlated aggregate / GROUP BY / DISTINCT inside (b233) --- #


def test_lateral_correlated_count_per_group(storage, session):
    # A correlated scalar-aggregate subquery yields exactly one row per outer
    # row (0 for the empty group, so t=c survives with count 0).
    assert rows(
        storage,
        session,
        "SELECT t.name, s.c FROM t CROSS JOIN LATERAL "
        "(SELECT count(*) AS c FROM u WHERE u.tid = t.id) s ORDER BY t.name",
    ) == [("a", 2), ("b", 3), ("c", 0)]


def test_lateral_correlated_sum_left(storage, session):
    # sum() over the empty group is NULL; LEFT keeps t=c with a NULL total.
    assert rows(
        storage,
        session,
        "SELECT t.name, s.total FROM t LEFT JOIN LATERAL "
        "(SELECT sum(val) AS total FROM u WHERE u.tid = t.id) s ON true ORDER BY t.name",
    ) == [("a", 50), ("b", 55), ("c", None)]


def test_lateral_distinct_inside(storage, session):
    # DISTINCT inside the correlated subquery — one row per distinct val.
    assert rows(
        storage,
        session,
        "SELECT t.name, s.val FROM t CROSS JOIN LATERAL "
        "(SELECT DISTINCT val FROM u WHERE u.tid = t.id) s ORDER BY t.name, s.val",
    ) == [("a", 10), ("a", 40), ("b", 5), ("b", 20), ("b", 30)]


def test_lateral_group_by_inside(storage, session):
    # GROUP BY inside the correlated subquery, expanded per outer row.
    assert rows(
        storage,
        session,
        "SELECT t.name, s.val, s.c FROM t CROSS JOIN LATERAL "
        "(SELECT val, count(*) AS c FROM u WHERE u.tid = t.id GROUP BY val) s "
        "ORDER BY t.name, s.val",
    ) == [("a", 10, 1), ("a", 40, 1), ("b", 5, 1), ("b", 20, 1), ("b", 30, 1)]


def test_lateral_on_condition_rejected(storage, session):
    with pytest.raises(errors.SQLError):
        rows(
            storage,
            session,
            "SELECT * FROM t JOIN LATERAL (SELECT val FROM u WHERE u.tid = t.id) s ON t.id > 0",
        )
