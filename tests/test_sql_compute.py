"""Computed scalar expressions in the SELECT list (and ORDER BY / join output).

Arithmetic (`+`/`-`/`*`/`/`/`%`), `||` concatenation, and the common scalar
functions evaluate per row through the evaluated-select path. A computed item
routes a SELECT to that path automatically; GROUP BY *keys* must still be bare
columns (a separate slice).
"""

from __future__ import annotations

import bson
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
    s.insert(
        DB,
        "items",
        [
            {"_id": bson.Int64(1), "name": "Apple", "price": bson.Int64(10), "qty": bson.Int64(3)},
            {"_id": bson.Int64(2), "name": "pear", "price": bson.Int64(7), "qty": bson.Int64(0)},
        ],
    )
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def rows(storage, session, sql):
    return run(storage, session, sql).rows


def test_arithmetic_product(storage, session):
    assert rows(storage, session, "SELECT name, price * qty AS total FROM items ORDER BY _id") == [
        ("Apple", 30),
        ("pear", 0),
    ]


def test_arithmetic_add_sub(storage, session):
    assert rows(
        storage, session, "SELECT price + qty AS a, price - qty AS b FROM items WHERE _id = 1"
    ) == [(13, 7)]


def test_integer_division_truncates(storage, session):
    # Postgres integer division truncates toward zero: 10 / 3 = 3.
    assert rows(storage, session, "SELECT price / 3 AS d FROM items WHERE _id = 1") == [(3,)]


def test_string_functions(storage, session):
    assert rows(
        storage, session, "SELECT upper(name) AS u, length(name) AS l FROM items ORDER BY _id"
    ) == [("APPLE", 5), ("PEAR", 4)]


def test_concat_operator(storage, session):
    assert rows(storage, session, "SELECT name || '!' AS x FROM items WHERE _id = 1") == [
        ("Apple!",)
    ]


def test_round(storage, session):
    # numeric, not float: real Postgres answers 3.33 with pg_typeof numeric.
    got = rows(storage, session, "SELECT round(price / 3.0, 2) AS r FROM items WHERE _id = 1")
    assert [(str(got[0][0]),)] == [("3.33",)]


def test_coalesce_and_nullif(storage, session):
    assert rows(
        storage, session, "SELECT coalesce(missing, price) AS c FROM items WHERE _id = 1"
    ) == [(10,)]
    assert rows(storage, session, "SELECT nullif(qty, 0) AS n FROM items ORDER BY _id") == [
        (3,),
        (None,),
    ]


def test_greatest_least_substring(storage, session):
    assert rows(
        storage,
        session,
        "SELECT greatest(price, qty) AS g, least(price, qty) AS s, substring(name, 1, 2) AS sub "
        "FROM items WHERE _id = 1",
    ) == [(10, 3, "Ap")]


def test_computed_in_order_by(storage, session):
    assert rows(storage, session, "SELECT name FROM items ORDER BY price * qty DESC") == [
        ("Apple",),
        ("pear",),
    ]


def test_computed_type_tags(storage, session):
    res = run(
        storage,
        session,
        "SELECT price * qty AS total, upper(name) AS u, length(name) AS l FROM items WHERE _id = 1",
    )
    tags = {c.name: c.type_tag for c in res.columns}
    # price/qty are int8 columns; bigint * bigint stays bigint in Postgres.
    assert tags == {"total": "int8", "u": "text", "l": "int4"}


def test_computed_over_join(storage, session):
    # A computed expression spanning two joined tables.
    storage.insert(
        DB,
        "items2",
        [
            {"_id": bson.Int64(1), "name": "Apple", "price": bson.Int64(10), "cat": bson.Int64(7)},
            {"_id": bson.Int64(2), "name": "pear", "price": bson.Int64(5), "cat": bson.Int64(8)},
        ],
    )
    storage.insert(
        DB,
        "cats",
        [
            {"_id": bson.Int64(7), "rate": bson.Int64(2)},
            {"_id": bson.Int64(8), "rate": bson.Int64(3)},
        ],
    )
    res = rows(
        storage,
        session,
        "SELECT i.name, i.price * c.rate AS taxed FROM items2 i "
        "JOIN cats c ON i.cat = c._id ORDER BY i.name",
    )
    assert res == [("Apple", 20), ("pear", 15)]
