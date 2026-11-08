"""Gap tests of the divergences found while working RP-522 (a whole row
compared with a record runs `record_eq`'s field checks): RP-529 (a domain
column's whole row is typed by its base type), RP-530 (a record compared
with a scalar is 0A000, not 42883), RP-531 (NULLIF, GREATEST and LEAST over
a whole row) and RP-532 (GROUP BY a whole row not in the select list).

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
    "drop table if exists fs, fd cascade",
    "drop domain if exists dint cascade",
    "create domain dint as int",
    "create table fs (a int, c text)",
    "create table fd (a dint)",
    "insert into fs values (1, 'y')",
    "insert into fd values (1)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])
        c.execute(_SETUP[1])


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
        return cur.fetchall()


# A whole row's field of a domain type is the DOMAIN to `record_eq`: its
# tuple descriptor carries the domain's oid, so a row of the base type is
# 42804 whatever the values (the type is checked before the value).
DOMAIN_FIELD = {
    "select fd = row(1) from fd": (
        "42804",
        "cannot compare dissimilar column types dint and integer at record column 1",
        None,
        None,
    ),
    "select fd = row(2) from fd": (
        "42804",
        "cannot compare dissimilar column types dint and integer at record column 1",
        None,
        None,
    ),
    "select fd = row(1::dint) from fd": [(True,)],
}


@pytest.mark.backlog("RP-529")
@pytest.mark.parametrize("query", list(DOMAIN_FIELD))
def test_domain_field_of_a_whole_row(query: str, dsn: str) -> None:
    assert answer(dsn, query) == DOMAIN_FIELD[query]


_NO_OPERATOR_HINT = (
    "No operator matches the given name and argument types. "
    "You might need to add explicit type casts."
)

# A record against a scalar has no operator: 42883 naming both types, at
# the operator.
RECORD_AGAINST_SCALAR = {
    "select fs = (1) from fs": (
        "42883",
        "operator does not exist: fs = integer",
        _NO_OPERATOR_HINT,
        "11",
    ),
    "select (1) = fs from fs": (
        "42883",
        "operator does not exist: integer = fs",
        _NO_OPERATOR_HINT,
        "12",
    ),
    "select fs::text = (1,'y') from fs": (
        "42883",
        "operator does not exist: text = record",
        _NO_OPERATOR_HINT,
        "17",
    ),
}


@pytest.mark.backlog("RP-530")
@pytest.mark.parametrize("query", list(RECORD_AGAINST_SCALAR))
def test_record_against_scalar(query: str, dsn: str) -> None:
    assert answer(dsn, query) == RECORD_AGAINST_SCALAR[query]


# NULLIF compares its arguments with `record_eq`; GREATEST and LEAST
# resolve one common type for their arguments (`select_common_type`) and
# order them with `record_cmp`.
RECORD_CONDITIONALS = {
    "select nullif(fs, (1,'y'::text)) from fs": [(None,)],
    "select nullif(fs, row(2,'y')::fs) from fs": [("(1,y)",)],
    "select greatest(fs, (2,'a'::text)) from fs": (
        "42804",
        "GREATEST types fs and record cannot be matched",
        None,
        "21",
    ),
    "select least(fs, row(0,'a')::fs) from fs": [("(0,a)",)],
}


@pytest.mark.backlog("RP-531")
@pytest.mark.parametrize("query", list(RECORD_CONDITIONALS))
def test_record_conditionals(query: str, dsn: str) -> None:
    assert answer(dsn, query) == RECORD_CONDITIONALS[query]


# A GROUP BY item need not be in the select list, a whole row included.
GROUP_BY_WHOLE_ROW = {
    "select count(*) from fs group by fs": [(1,)],
    "select fs, count(*) from fs group by fs": [("(1,y)", 1)],
}


@pytest.mark.backlog("RP-532")
@pytest.mark.parametrize("query", list(GROUP_BY_WHOLE_ROW))
def test_group_by_whole_row(query: str, dsn: str) -> None:
    assert answer(dsn, query) == GROUP_BY_WHOLE_ROW[query]
