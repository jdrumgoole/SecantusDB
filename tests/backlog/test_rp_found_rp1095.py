"""Gap tests of the divergences found while working RP-1095 (`pg_get_viewdef`
did not relabel a user domain operand to its base type; fixed): a view
calling `num_nulls` prints as written, an untyped literal at a
`VARIADIC "any"` parameter is cast to text, an `= ANY` sublink is not
printed as `IN`, and an `ARRAY[...]` of one domain is typed by its base.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.20 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists p1095 cascade",
    "create table p1095 (k int, t text)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _viewdefs(dsn: str, query: str) -> tuple[str, str]:
    """`pg_get_viewdef` of a view over `query`, plain and pretty."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        c.execute(f"create view v1095 as {query}")
        row = c.execute(
            "select pg_get_viewdef('v1095'::regclass), pg_get_viewdef('v1095'::regclass, true)"
        ).fetchone()
        c.execute(_SETUP[0])
    assert row is not None
    return row


# `num_nulls(VARIADIC "any")` returns integer; the view prints as any other.
@pytest.mark.backlog("RP-1223")
def test_pg_get_viewdef_of_a_num_nulls_call(dsn: str) -> None:
    got = _viewdefs(dsn, "select num_nulls(k) as x from p1095")
    expected = " SELECT num_nulls(p1095.k) AS x\n   FROM p1095;"
    assert got == (expected, expected)


# `"any"` takes an `unknown` constant as it is: printed bare.
@pytest.mark.backlog("RP-1224")
def test_pg_get_viewdef_keeps_an_untyped_literal_at_an_any_parameter(dsn: str) -> None:
    got = _viewdefs(dsn, "select concat(t, 'x') as x from p1095")
    expected = " SELECT concat(p1095.t, 'x') AS x\n   FROM p1095;"
    assert got == (expected, expected)


# `get_sublink_expr` represents `= ANY` as `IN`, however it was written.
@pytest.mark.backlog("RP-1225")
def test_pg_get_viewdef_prints_an_equals_any_sublink_as_in(dsn: str) -> None:
    got = _viewdefs(dsn, "select k from p1095 where k = any (select k from p1095)")
    expected = (
        " SELECT p1095.k\n   FROM p1095\n  WHERE (p1095.k IN ( SELECT p1095_1.k\n"
        "           FROM p1095 p1095_1));"
    )
    assert got == (expected, expected)


# An ARRAY of members all of one domain is that domain's array type; `=
# ANY` over it coerces the array to the base's array type.
@pytest.mark.backlog("RP-1226")
def test_pg_get_viewdef_keeps_an_array_of_one_domain(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists u1095 cascade")
        c.execute("drop domain if exists dint1095")
        c.execute("create domain dint1095 as int")
        c.execute("create table u1095 (k int, dd dint1095)")
        c.execute("create view a1095 as select array[dd, dd] as x from u1095")
        c.execute("create view b1095 as select k from u1095 where dd = any (array[dd, dd])")
        got = [
            c.execute(f"select pg_get_viewdef('{v}'::regclass)").fetchone()
            for v in ("a1095", "b1095")
        ]
        c.execute("drop table u1095 cascade")
        c.execute("drop domain dint1095")
    assert got == [
        (" SELECT ARRAY[u1095.dd, u1095.dd] AS x\n   FROM u1095;",),
        (
            " SELECT u1095.k\n   FROM u1095\n  WHERE ((u1095.dd)::integer = ANY "
            "((ARRAY[u1095.dd, u1095.dd])::integer[]));",
        ),
    ]
