"""Unit tests pinning the SQL -> Mongo translation (the semantics oracle).

These assert the exact filter / update / sort structures the planner lowers to,
independent of any storage. They are the precise contract the executor and the
future wire layer rely on.
"""

from __future__ import annotations

import bson
import sqlglot

from secantus.sql import planner
from secantus.sql.catalog import Column, TableDef

USERS = TableDef(
    name="users",
    collection="users",
    columns=[
        Column("id", "int8", "_id", pk=True, nullable=False),
        Column("name", "text", "name", pk=False, nullable=True),
        Column("age", "int4", "age", pk=False, nullable=True),
    ],
)


def filter_of(where_sql):
    stmt = sqlglot.parse_one(f"SELECT * FROM users WHERE {where_sql}", read="postgres")
    return planner.plan_select(stmt, USERS).filter


def test_equality_maps_pk_to_id():
    assert filter_of("id = 5") == {"_id": bson.Int64(5)}


def test_simple_and_merges_to_one_dict():
    assert filter_of("age >= 18 AND name = 'a'") == {"age": {"$gte": 18}, "name": "a"}


def test_or_uses_dollar_or():
    assert filter_of("age < 1 OR age > 9") == {"$or": [{"age": {"$lt": 1}}, {"age": {"$gt": 9}}]}


def test_in_between_like_isnull():
    assert filter_of("age IN (1, 2)") == {"age": {"$in": [1, 2]}}
    assert filter_of("age BETWEEN 1 AND 9") == {"age": {"$gte": 1, "$lte": 9}}
    assert filter_of("name LIKE 'a%'") == {"name": {"$regex": "^a.*$"}}
    assert filter_of("name IS NULL") == {"name": None}
    assert filter_of("name IS NOT NULL") == {"name": {"$ne": None}}


def test_column_on_right_flips_operator():
    assert filter_of("18 < age") == {"age": {"$gt": 18}}


def test_update_lowers_to_set_and_skips_pk_field():
    stmt = sqlglot.parse_one("UPDATE users SET age = 7 WHERE id = 1", read="postgres")
    plan = planner.plan_update(stmt, USERS)
    assert plan.update == {"$set": {"age": 7}}
    assert plan.filter == {"_id": bson.Int64(1)}


def test_insert_builds_field_keyed_docs():
    stmt = sqlglot.parse_one("INSERT INTO users (id, name) VALUES (1, 'x')", read="postgres")
    plan = planner.plan_insert(stmt, USERS)
    assert plan.docs == [{"_id": bson.Int64(1), "name": "x"}]
