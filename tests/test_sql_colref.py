"""Column-to-column (and column-to-expression) WHERE predicates.

A comparison where neither side is a constant — ``qty > shipped``,
``price < cost * 1.5`` — lowers to a Mongo ``$expr`` (the field/literal fast
path, which the storage index planner can use, is unchanged). Arithmetic
(``+``/``-``/``*``/``/``) over columns and literals is supported inside the
comparison; arbitrary function calls in such a predicate are not (yet).
"""

from __future__ import annotations

import bson
import pytest
import sqlglot

from secantus.sql import SQLError, run_sql
from secantus.sql.catalog import Column, TableDef
from secantus.sql.planner import plan_select
from secantus.sql.session import Session
from sqlfake import FakeStorage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage():
    s = FakeStorage()
    s.insert(
        DB,
        "orders",
        [
            {
                "_id": bson.Int64(1),
                "qty": bson.Int64(5),
                "shipped": bson.Int64(5),
                "cost": bson.Int64(10),
                "price": bson.Int64(20),
            },
            {
                "_id": bson.Int64(2),
                "qty": bson.Int64(8),
                "shipped": bson.Int64(3),
                "cost": bson.Int64(10),
                "price": bson.Int64(12),
            },
            {
                "_id": bson.Int64(3),
                "qty": bson.Int64(4),
                "shipped": bson.Int64(4),
                "cost": bson.Int64(30),
                "price": bson.Int64(40),
            },
        ],
    )
    return s


def q(storage, session, sql):
    return [r[0] for r in run_sql(storage, DB, sql, session=session)[0].rows]


# -- behaviour --------------------------------------------------------------- #


def test_col_eq_col(storage, session):
    assert q(storage, session, "SELECT _id FROM orders WHERE qty = shipped ORDER BY _id") == [1, 3]


def test_col_gt_col(storage, session):
    assert q(storage, session, "SELECT _id FROM orders WHERE qty > shipped ORDER BY _id") == [2]


def test_col_ne_col(storage, session):
    assert q(storage, session, "SELECT _id FROM orders WHERE qty <> shipped ORDER BY _id") == [2]


def test_col_lt_arithmetic(storage, session):
    # price < cost * 1.5 → 20<15 no, 12<15 yes, 40<45 yes
    assert q(storage, session, "SELECT _id FROM orders WHERE price < cost * 1.5 ORDER BY _id") == [
        2,
        3,
    ]


def test_arithmetic_vs_literal(storage, session):
    # price - cost > 15 → 10, 2, 10 — none exceed 15
    assert q(storage, session, "SELECT _id FROM orders WHERE price - cost > 15 ORDER BY _id") == []


def test_col_col_with_literal_predicate(storage, session):
    assert q(
        storage,
        session,
        "SELECT _id FROM orders WHERE qty > shipped AND price > 10 ORDER BY _id",
    ) == [2]


def test_literal_on_left_still_indexable(storage, session):
    # ``5 = qty`` keeps the field/const fast path (just flipped).
    assert q(storage, session, "SELECT _id FROM orders WHERE 5 = qty ORDER BY _id") == [1]


def test_col_col_through_group_by(storage, session):
    storage.insert(
        DB,
        "sales",
        [
            {"_id": bson.Int64(1), "region": "e", "amt": bson.Int64(10), "target": bson.Int64(5)},
            {"_id": bson.Int64(2), "region": "e", "amt": bson.Int64(3), "target": bson.Int64(5)},
            {"_id": bson.Int64(3), "region": "w", "amt": bson.Int64(30), "target": bson.Int64(5)},
        ],
    )
    rows = run_sql(
        storage,
        DB,
        "SELECT region, SUM(amt) FROM sales WHERE amt > target GROUP BY region ORDER BY region",
        session=session,
    )[0].rows
    assert rows == [("e", 10), ("w", 30)]


def test_function_in_colref_predicate_unsupported(storage, session):
    # A function call inside a column-to-column predicate isn't lowered yet.
    with pytest.raises(SQLError) as ei:
        q(storage, session, "SELECT _id FROM orders WHERE qty = abs(shipped)")
    assert ei.value.sqlstate == "0A000"


# -- filter shape (semantics pinned) ----------------------------------------- #


def _filter_for(where_sql):
    table = TableDef(
        "orders",
        "orders",
        [
            Column("_id", "int8", "_id", pk=True, nullable=False),
            Column("qty", "int8", "qty", pk=False, nullable=True),
            Column("shipped", "int8", "shipped", pk=False, nullable=True),
            Column("cost", "int8", "cost", pk=False, nullable=True),
            Column("price", "int8", "price", pk=False, nullable=True),
        ],
    )
    stmt = sqlglot.parse_one(f"SELECT _id FROM orders WHERE {where_sql}", read="postgres")
    return plan_select(stmt, table).filter


def test_colref_lowers_to_expr():
    assert _filter_for("qty > shipped") == {"$expr": {"$gt": ["$qty", "$shipped"]}}


def test_arithmetic_lowers_to_expr():
    assert _filter_for("price < cost * 1.5") == {
        "$expr": {"$lt": ["$price", {"$multiply": ["$cost", 1.5]}]}
    }


def test_field_literal_keeps_fast_path():
    # Unchanged: a field/const comparison stays the indexable shorthand.
    assert _filter_for("qty > 3") == {"qty": {"$gt": 3}}
    assert _filter_for("qty = 3") == {"qty": 3}
