"""Column-to-column (and column-to-expression) WHERE predicates.

A comparison where neither side is a constant — ``qty > shipped``,
``price < cost * 1.5`` — lowers to a Mongo ``$expr`` (the field/literal fast
path, which the storage index planner can use, is unchanged). Arithmetic
(``+``/``-``/``*``/``/``) over columns and literals is supported inside the
comparison, as are the common scalar functions (``abs``/``lower``/``upper``/…,
lowered by the same ``_func_to_agg_expr`` the computed GROUP BY keys use); a
function the aggregation engine can't lower (e.g. ``substr``) is still ``0A000``.
"""

from __future__ import annotations

from decimal import Decimal

import bson
import pytest
import sqlglot

from secantus.sql import run_sql
from secantus.sql.catalog import Column, TableDef
from secantus.sql.planner import plan_select
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
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
    try:
        yield s
    finally:
        s.close()


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


def test_function_in_colref_predicate(storage, session):
    # A common function inside a column-to-column predicate now lowers to $expr
    # (the same _func_to_agg_expr the computed GROUP BY keys use). abs(5)=5 ⇒ _id 1,
    # abs(4)=4 ⇒ _id 3; _id 2 (qty 8 ≠ abs(3)) is excluded.
    ids = q(storage, session, "SELECT _id FROM orders WHERE qty = abs(shipped) ORDER BY _id")
    assert ids == [1, 3]


def test_function_in_colref_predicate_through_group_by(storage, session):
    # The same $expr function lowering rides the GROUP BY pipeline's leading $match.
    storage.insert(
        DB,
        "sales",
        [
            {"_id": bson.Int64(1), "region": "e", "amt": bson.Int64(10), "target": bson.Int64(-10)},
            {"_id": bson.Int64(2), "region": "e", "amt": bson.Int64(30), "target": bson.Int64(30)},
            {"_id": bson.Int64(3), "region": "w", "amt": bson.Int64(5), "target": bson.Int64(5)},
            {"_id": bson.Int64(4), "region": "w", "amt": bson.Int64(40), "target": bson.Int64(-40)},
        ],
    )
    rows = run_sql(
        storage,
        DB,
        "SELECT region, SUM(amt) FROM sales WHERE amt = abs(target) "
        "GROUP BY region ORDER BY region",
        session=session,
    )[0].rows
    assert rows == [("e", 40), ("w", 45)]


def test_function_in_colref_predicate_through_join(storage, session):
    # A function-call comparison also pushes down onto a JOIN pipeline's $match.
    storage.insert(
        DB,
        "ord2",
        [
            {"_id": bson.Int64(1), "region": "e", "qty": bson.Int64(10), "cost": bson.Int64(-10)},
            {"_id": bson.Int64(2), "region": "w", "qty": bson.Int64(40), "cost": bson.Int64(-40)},
        ],
    )
    storage.insert(DB, "reg", [{"_id": "e", "region": "e"}, {"_id": "w", "region": "w"}])
    rows = run_sql(
        storage,
        DB,
        "SELECT o.region, SUM(o.qty) FROM ord2 o JOIN reg r ON o.region = r.region "
        "WHERE o.qty = abs(o.cost) GROUP BY o.region ORDER BY o.region",
        session=session,
    )[0].rows
    assert rows == [("e", 10), ("w", 40)]


def test_function_in_colref_predicate_evaluates_per_row(storage, session):
    # A predicate the pushdown can't lower (a scalar function against a column)
    # now routes to per-row evaluation instead of 0A000. qty is an int and
    # substr() yields text, so real Postgres would error 42883 (no int = text
    # operator) — but ``orders`` here is a REFLECTED table (seeded through
    # storage.insert, no CREATE TABLE), and the plan-time 42883 analysis
    # (secantus.sql.typecheck) deliberately exempts reflected tables: their
    # column types come from sampling 50 documents, so a heterogeneous BSON
    # field can be declared text while holding ints. The silent no-match is
    # the right answer here; a declared table gets the 42883
    # (tests/test_sql_typecheck.py).
    assert q(storage, session, "SELECT _id FROM orders WHERE qty = substr(shipped, 1, 1)") == []


def test_unlowerable_predicate_through_group_by_evaluates_per_row(storage, session):
    # An unlowerable predicate on the pipeline path evaluates per-row before
    # the $group (was 0A000): substr('e', 1, 1) = 'e' matches the row.
    storage.insert(DB, "sales2", [{"_id": bson.Int64(1), "region": "e", "amt": bson.Int64(1)}])
    res = run_sql(
        storage,
        DB,
        "SELECT region, SUM(amt) FROM sales2 WHERE region = substr(region, 1, 1) GROUP BY region",
        session=session,
    )[-1]
    assert res.rows == [("e", 1)]


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
    # Both sides are null-guarded: BSON total order is two-valued (NULL sorts
    # below numbers), but SQL's unknown never satisfies a WHERE.
    assert _filter_for("qty > shipped") == {
        "$expr": {
            "$and": [
                {"$ne": ["$qty", None]},
                {"$ne": ["$shipped", None]},
                {"$gt": ["$qty", "$shipped"]},
            ]
        }
    }


def test_arithmetic_lowers_to_expr():
    # 1.5 lowers as Decimal128, not a float: a decimal literal is ``numeric``
    # in Postgres, and carrying it exactly is what makes ``0.1 + 0.2 = 0.3``
    # true. This pins the lowered shape, so the type is part of the contract.
    product = {"$multiply": ["$cost", bson.Decimal128(Decimal("1.5"))]}
    assert _filter_for("price < cost * 1.5") == {
        "$expr": {
            "$and": [
                {"$ne": ["$price", None]},
                {"$ne": [product, None]},
                {"$lt": ["$price", product]},
            ]
        }
    }


def test_field_literal_keeps_fast_path():
    # Unchanged: a field/const comparison stays the indexable shorthand.
    assert _filter_for("qty > 3") == {"qty": {"$gt": 3}}
    assert _filter_for("qty = 3") == {"qty": 3}


# -- function on the field side vs a constant (#169) ------------------------- #


def _text_table(storage, session):
    run_sql(storage, DB, "CREATE TABLE p (id int primary key, name text, x int)", session=session)
    for i, (n, x) in enumerate([("Alice", 1), ("BOB", -3), ("bob", 5)]):
        run_sql(storage, DB, f"INSERT INTO p VALUES ({i}, '{n}', {x})", session=session)


def test_upper_eq_const(storage, session):
    _text_table(storage, session)
    # upper(name) = 'BOB' matches BOB and bob.
    assert q(storage, session, "SELECT id FROM p WHERE upper(name) = 'BOB' ORDER BY id") == [1, 2]


def test_abs_eq_const(storage, session):
    _text_table(storage, session)
    assert q(storage, session, "SELECT id FROM p WHERE abs(x) = 3 ORDER BY id") == [1]


def test_length_gt_const(storage, session):
    _text_table(storage, session)
    assert q(storage, session, "SELECT id FROM p WHERE length(name) > 3 ORDER BY id") == [0]
