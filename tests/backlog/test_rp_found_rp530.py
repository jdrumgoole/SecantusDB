"""Gap tests of the divergences found while working RP-530 (a row beside a
typed operand that is not a row has no operator): RP-590 (two different
composite types were refused a comparison, fixed), RP-591 (an untyped literal
beside a row is not read as `record`), RP-592 (a row beside an `ARRAY[...]`
names the row `text`) and RP-593 (an error a PREPARE's query raises carries
the query's own offset).

An answer is the rows, or `(sqlstate, message, hint, position)` for an
error. The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists fs cascade",
    "drop type if exists ct, ct2 cascade",
    "create table fs (a int, c text)",
    "insert into fs values (1, 'y')",
    "create type ct as (x int)",
    "create type ct2 as (x int)",
]

_NO_OPERATOR_HINT = (
    "No operator matches the given name and argument types. "
    "You might need to add explicit type casts."
)


def _no_operator(types: str, position: str) -> tuple:
    return ("42883", f"operator does not exist: {types}", _NO_OPERATOR_HINT, position)


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:2]:
            c.execute(q)


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_hint,
                e.diag.statement_position,
            )
        return cur.fetchall() if cur.description else cur.statusmessage


# Both sides coerce to `record`, and `record_eq` / `record_cmp` compare the
# fields.
TWO_COMPOSITES = {
    "select row(1)::ct = row(1)::ct2": [(True,)],
    "select row(1)::ct < row(2)::ct2": [(True,)],
}


@pytest.mark.backlog_fixed("RP-590")
@pytest.mark.parametrize("query", list(TWO_COMPOSITES))
def test_two_composite_types_compare(query: str, dsn: str) -> None:
    """Two different composite types compare as records. Each was 42883
    `operator does not exist: ct = ct2`; in a WHERE: test_rp_found_rp590.py."""
    assert answer(dsn, query) == TWO_COMPOSITES[query]


# The operator resolves to `record = record`, and `record_in` refuses the
# literal, at the literal.
_ANONYMOUS = "input of anonymous composite types is not implemented"
UNTYPED_BESIDE_ROW = {
    "select fs = 'x' from fs": ("0A000", _ANONYMOUS, None, "13"),
    "select 'x' = fs from fs": ("0A000", _ANONYMOUS, None, "8"),
    "select (1,2) = 'x'": ("0A000", _ANONYMOUS, None, "16"),
}


@pytest.mark.backlog("RP-591")
@pytest.mark.parametrize("query", list(UNTYPED_BESIDE_ROW))
def test_untyped_literal_beside_a_row(query: str, dsn: str) -> None:
    assert answer(dsn, query) == UNTYPED_BESIDE_ROW[query]


ROW_BESIDE_ARRAY = {
    "select array[1] = (1,2)": _no_operator("integer[] = record", "17"),
    "select fs = array[1] from fs": _no_operator("fs = integer[]", "11"),
    "select array[1] = fs from fs": _no_operator("integer[] = fs", "17"),
}


@pytest.mark.backlog("RP-592")
@pytest.mark.parametrize("query", list(ROW_BESIDE_ARRAY))
def test_row_beside_an_array_constructor(query: str, dsn: str) -> None:
    assert answer(dsn, query) == ROW_BESIDE_ARRAY[query]


@pytest.mark.backlog("RP-593")
def test_prepare_error_position(dsn: str) -> None:
    """The position is in the PREPARE's own text, at the operator."""
    query = "prepare p1(int) as select 12 = true"
    assert answer(dsn, query) == _no_operator("integer = boolean", "30")
