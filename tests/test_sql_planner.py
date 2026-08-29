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

    Counted rather than timed: a ratio of two wall-clock measurements is
    dominated by fixed overhead at the small end and by scheduler noise on a
    shared runner, which made it flaky in both directions. The defect is
    structural, so measure the structure — every re-parented child is one unit
    of the work that went quadratic.
    """
    from sqlglot import exp

    assert hasattr(exp.Expression, "_set_parent"), "sqlglot changed: re-point this probe"
    original = exp.Expression._set_parent
    reparented = 0

    def counting(self, arg_key, value, index=None):
        nonlocal reparented
        reparented += len(value) if isinstance(value, list) else 1
        return original(self, arg_key, value, index)

    def work(n: int) -> int:
        nonlocal reparented
        stmt = planner.parse(_params_sql(n))[0]
        reparented = 0
        exp.Expression._set_parent = counting
        try:
            planner.substitute_parameters(stmt, [None] * n)
        finally:
            exp.Expression._set_parent = original
        return reparented

    n = 2000
    units = work(n)
    # Linear does one pass over the argument list (~n). Quadratic re-parents all
    # n siblings once per placeholder (~n**2 = 4,000,000 here). 10n is far above
    # the former and far below the latter, and the count is deterministic.
    assert units < 10 * n, f"{units:,} re-parented children for {n:,} parameters (expected ~{n:,})"
