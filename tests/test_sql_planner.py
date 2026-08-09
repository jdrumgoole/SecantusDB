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


def test_parse_handles_adjacent_parameters():
    # pg8000 / psycopg emit ``$1,$2`` without spaces; sqlglot mis-tokenizes that
    # as a dollar-quoted string unless we normalize.
    (stmt,) = planner.parse("INSERT INTO t (a,b,c) VALUES ($1,$2,$3)")
    assert len(list(stmt.find_all(sqlglot.exp.Parameter))) == 3


def test_parse_leaves_dollar_in_string_literal_untouched():
    (stmt,) = planner.parse("SELECT * FROM t WHERE name = '$1,$2'")
    assert stmt.args["where"].this.expression.this == "$1,$2"


# --------------------------------------------------------------------------- #
# Parameter substitution
# --------------------------------------------------------------------------- #


def _params_sql(n: int) -> str:
    return "SELECT coalesce(" + ",".join(f"${i + 1}" for i in range(n)) + ")"


def test_substitute_parameters_binds_every_placeholder():
    stmt = planner.parse(_params_sql(4))[0]
    out = planner.substitute_parameters(stmt, [1, None, "x", 2.5])
    assert out.sql(dialect="postgres") == "SELECT COALESCE(1, NULL, 'x', 2.5)"


def test_substitute_parameters_binds_a_bare_placeholder():
    stmt = planner.parse("SELECT $1")[0]
    assert planner.substitute_parameters(stmt, ["x"]).sql(dialect="postgres") == "SELECT 'x'"


def test_substitute_parameters_leaves_the_source_statement_alone():
    stmt = planner.parse("SELECT $1, $2")[0]
    planner.substitute_parameters(stmt, [1, 2])
    assert stmt.sql(dialect="postgres") == "SELECT $1, $2"


def test_substitute_parameters_scales_linearly():
    """Binding N placeholders under one node must not be O(N**2).

    Replacing them one at a time makes sqlglot re-parent every sibling per call.
    pgjdbc's rewritten batch INSERT binds tens of thousands of parameters in a
    single statement, which took minutes and timed the connection out.
    """
    import time

    def elapsed(n: int) -> float:
        # Best of several: timing noise is one-sided (a loaded CI runner only
        # ever makes a run slower), so the minimum is the stable estimate. A
        # single sample against a ~10ms baseline is mostly scheduler jitter.
        best = float("inf")
        for _ in range(5):
            stmt = planner.parse(_params_sql(n))[0]
            values = [None] * n
            t = time.perf_counter()
            planner.substitute_parameters(stmt, values)
            best = min(best, time.perf_counter() - t)
        return best

    small = max(elapsed(2000), 1e-4)
    large = elapsed(16000)
    # 8x the parameters: linear predicts ~8x, quadratic ~64x. 25x sits well
    # clear of both — it still fails loudly on a return to quadratic, without
    # tracking the constant overheads that dominate the small measurement.
    ratio = large / small
    assert ratio < 25, f"{small:.4f}s -> {large:.4f}s = {ratio:.1f}x for 8x the parameters"
