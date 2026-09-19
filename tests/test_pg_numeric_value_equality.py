"""Numerics that are equal in value are the same value, whatever their scale.

Postgres compares ``numeric`` by value: ``1.5 = 1.50``, and ``2 = 2.0`` joins,
dedups and partitions as one value. The Python server split them in two ways:

- a join over a numeric key lowered to ``$lookup``, whose hash join keyed on
  the ``Decimal128`` representation, so ``t.v = u.v`` returned NO rows for
  ``1.5`` against ``1.500``, or for a numeric ``2.0`` against an int ``2``;
- UNION / INTERSECT / EXCEPT, recursive-CTE dedup, evaluated DISTINCT,
  DISTINCT ON and window PARTITION BY keyed rows on ``repr()``, so ``1.5`` and
  ``1.50`` were different rows (INTERSECT of the pair was empty).

Expected rows are what Postgres returns.
"""

from __future__ import annotations

from decimal import Decimal

import pytest

from secantus.sql import engine
from secantus.storage import Storage


@pytest.fixture
def q(tmp_path):
    st = Storage(str(tmp_path))

    def run(sql: str):
        return engine.run_sql(st, "postgres", sql)[-1].rows

    run("create table t (id int, v numeric, f float8)")
    run("create table u (v numeric, name text)")
    run("create table n (v int, name text)")
    run("insert into t values (4, 1.5, 0.0), (5, 1.50, -0.0), (6, 2, 1)")
    run("insert into u values (1.500, 'narrow'), (2.0, 'two')")
    run("insert into n values (2, 'int two')")
    try:
        yield run
    finally:
        st.close()


def _num(rows):
    return [tuple(Decimal(str(v)) if isinstance(v, Decimal) else v for v in r) for r in rows]


@pytest.mark.parametrize(
    "sql",
    [
        "select t.id, u.name from t join u on t.v = u.v order by t.id",
        "select t.id, u.name from t, u where t.v = u.v order by t.id",
    ],
)
def test_join_matches_across_scale(q, sql: str) -> None:
    assert q(sql) == [(4, "narrow"), (5, "narrow"), (6, "two")]


def test_join_numeric_to_int(q) -> None:
    assert q("select u.name, n.name from u join n on u.v = n.v") == [("two", "int two")]


def test_union_dedups_by_value(q) -> None:
    assert _num(q("select v from t union select v from u order by 1")) == [
        (Decimal("1.5"),),
        (Decimal("2"),),
    ]


def test_intersect_and_except(q) -> None:
    assert len(q("select v from t intersect select v from u")) == 2
    assert q("select v from t except select v from u") == []
    assert len(q("select v from t intersect all select v from u")) == 2


def test_signed_zero_is_one_value(q) -> None:
    assert q("select f from t union select f from t order by 1") == [(0.0,), (1.0,)]


def test_distinct_on_and_partition_by(q) -> None:
    assert [r[0] for r in q("select distinct on (v) id, v from t order by v, id")] == [4, 6]
    assert q("select id, count(*) over (partition by v) from t order by id") == [
        (4, 2),
        (5, 2),
        (6, 1),
    ]


def test_nan_is_one_value(q) -> None:
    assert len(q("select 'NaN'::numeric union select 'NaN'::numeric")) == 1
