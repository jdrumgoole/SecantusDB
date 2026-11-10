"""What was found while working RP-692 (a set operation's group is its
first input's first row).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t692j, t692e cascade",
    "create table t692j (x numeric, k int)",
    "insert into t692j values (1.0, 1)",
    "create table t692e (x numeric, k int)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _connect(dsn: str) -> psycopg.Connection:
    c = psycopg.connect(dsn, autocommit=True)
    for q in _SETUP:
        c.execute(q)
    return c


def _text(c: psycopg.Connection, query: str) -> list[tuple[str | None, ...]]:
    return [tuple(None if v is None else str(v) for v in r) for r in c.execute(query).fetchall()]


@pytest.mark.backlog("RP-841")
def test_an_intersect_over_a_join_puts_the_smaller_input_first(dsn: str) -> None:
    """`select a.x from t692j a join t692j b on a.k = b.k intersect select
    1.000` is `1.000` on PostgreSQL 15.19: the join's estimated groups
    outnumber the constant's one, so the constant is the HashSetOp's first
    input and its row is the group's. Here a join input is not estimated,
    so the left input stays first and its `1.0` is returned."""
    with _connect(dsn) as c:
        got = _text(c, "select a.x from t692j a join t692j b on a.k = b.k intersect select 1.000")
    assert got == [("1.000",)]


@pytest.mark.backlog("RP-842")
def test_a_hashed_set_operation_returns_its_groups_in_hash_order(dsn: str) -> None:
    """A HashSetOp returns its groups in its hash table's order
    (`setop_retrieve_hash_table`): `select x, y from (values (1.0, true),
    (1.00, false), (1.000, true)) v(x, y) intersect select x, y from
    (values (1, true), (1, false)) w(x, y)` is `(1, f), (1, t)` on
    PostgreSQL 15.19. Here the groups come out in the first input's order,
    `(1, t), (1, f)`."""
    with _connect(dsn) as c:
        got = _text(
            c,
            "select x, y from (values (1.0, true), (1.00, false), (1.000, true)) v(x, y)"
            " intersect select x, y from (values (1, true), (1, false)) w(x, y)",
        )
    assert got == [("1", "False"), ("1", "True")]


@pytest.mark.backlog("RP-843")
def test_a_numeric_constant_selected_from_a_from_item_is_inserted(dsn: str) -> None:
    """`insert into t692e select 1.0, g from generate_series(1, 2) g`, the
    same over `(values (3)) v(g)`, and `insert into t692e (x) select 1.5
    from t692j` insert their rows on PostgreSQL 15.19: the constant is
    `numeric`. Here each is 42804 `column "x" is of type numeric but
    expression is of type text`, though `insert into t692e select 1.0, 1`
    (no FROM) is inserted."""
    inserts = [
        "insert into t692e select 1.0, g from generate_series(1, 2) g",
        "insert into t692e select 1.0, g from (values (3)) v(g)",
        "insert into t692e (x) select 1.5 from t692j",
    ]
    errors: list[tuple[str, str | None]] = []
    with _connect(dsn) as c:
        for q in inserts:
            try:
                c.execute(q)
            except psycopg.Error as e:
                errors.append((q, e.sqlstate))
        got = _text(c, "select x, k from t692e order by k, x")
    assert errors == []
    assert got == [("1.0", "1"), ("1.0", "2"), ("1.0", "3"), ("1.5", None)]
