"""Multirange containment / overlap operators — ``@>`` / ``<@`` / ``&&`` where
either operand is a multirange (#166). A multirange's members are disjoint and
non-adjacent, so a range is contained iff a single member contains it. Driven
through ``run_sql`` over the real WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def val(storage, expr):
    return run_sql(storage, DB, f"SELECT {expr} AS r", session=Session(database=DB))[-1].rows[0][0]


@pytest.mark.parametrize(
    "expr,want",
    [
        ("int4multirange(int4range(1,10)) @> int4range(2,5)", True),
        ("int4multirange(int4range(1,5), int4range(10,20)) @> int4range(2,5)", True),
        # a range spanning the gap between two members is NOT contained
        ("int4multirange(int4range(1,5), int4range(10,20)) @> int4range(4,12)", False),
        # multirange @> element
        ("int4multirange(int4range(1,5), int4range(10,20)) @> 12", True),
        ("int4multirange(int4range(1,5), int4range(10,20)) @> 7", False),
        # multirange @> multirange
        (
            "int4multirange(int4range(1,20)) @> int4multirange(int4range(2,5), int4range(10,15))",
            True,
        ),
        (
            "int4multirange(int4range(1,8)) @> int4multirange(int4range(2,5), int4range(10,15))",
            False,
        ),
        # range @> multirange
        ("int4range(1,20) @> int4multirange(int4range(2,5), int4range(10,15))", True),
        # multirange && range / multirange
        ("int4multirange(int4range(1,5), int4range(10,20)) && int4range(4,12)", True),
        ("int4multirange(int4range(1,5), int4range(10,20)) && int4range(6,9)", False),
        ("int4multirange(int4range(1,5)) && int4multirange(int4range(4,9))", True),
        ("int4multirange(int4range(1,5)) && int4multirange(int4range(6,9))", False),
        # <@ : left contained by right
        ("int4range(2,5) <@ int4multirange(int4range(1,10))", True),
        ("int4range(2,12) <@ int4multirange(int4range(1,5), int4range(10,20))", False),
        ("int4multirange(int4range(2,5)) <@ int4multirange(int4range(1,10))", True),
        # plain single-range operators still work (regression)
        ("int4range(1,10) @> int4range(2,5)", True),
        ("int4range(1,10) @> 5", True),
        ("int4range(1,10) && int4range(5,15)", True),
    ],
)
def test_multirange_operator(storage, expr, want):
    assert val(storage, expr) is want


def test_numrange_multirange(storage):
    assert val(storage, "nummultirange(numrange(1.0, 2.5)) @> 1.5") is True
    assert val(storage, "nummultirange(numrange(1.0, 2.5)) @> 3.0") is False


def test_multirange_in_where(storage):
    run = lambda sql: run_sql(storage, DB, sql, session=Session(database=DB))[-1]  # noqa: E731
    run("CREATE TABLE t (id int primary key, mr int4multirange)")
    run("INSERT INTO t VALUES (1, int4multirange(int4range(1,5), int4range(10,20)))")
    run("INSERT INTO t VALUES (2, int4multirange(int4range(100,200)))")
    r = run("SELECT id FROM t WHERE mr @> 12 ORDER BY id")
    assert r.rows == [(1,)]
