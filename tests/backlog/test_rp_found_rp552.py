"""Gap tests of the divergences found while working RP-552 (a field's
domain error in a record literal is at the literal): RP-945 (an untyped
literal written to a composite or domain-array column, or coerced as an
`ARRAY[]` element, reports a field's domain error with no position), RP-946
(a NULL element of a NOT NULL domain's array literal is accepted), RP-947 (a
composite column compared with an untyped literal reads the literal as the
column's type) and RP-948 (under `PREPARE`, a recorded error location is
counted in the inner statement).

An answer is the rows, the command tag for a statement that returns none,
or `(sqlstate, message, position)` for an error. The expected answers were
measured on PostgreSQL 15.19 with `_SETUP`, which each statement runs
after.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "deallocate all",
    "drop table if exists tct cascade",
    "drop type if exists ctp cascade",
    "drop domain if exists pint, nnint cascade",
    "create domain pint as int check (value > 0)",
    "create domain nnint as int not null",
    "create type ctp as (a pint, b text)",
    "create table tct (c ctp, d pint[])",
    "insert into tct(c) values ('(1,x)')",
]

_PINT = 'value for domain pint violates check constraint "pint_check"'


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:4]:
            c.execute(q)


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        return cur.fetchall() if cur.description else cur.statusmessage


# `coerce_type` reads an untyped literal with the target's input function
# under the literal's parser error position, for an assignment and an
# `ARRAY[]` element as for a cast.
ASSIGNED = {
    "insert into tct(c) values ('(0,x)')": ("23514", _PINT, "28"),
    "insert into tct(d) values ('{0}')": ("23514", _PINT, "28"),
    "update tct set c = '(0,x)'": ("23514", _PINT, "20"),
    "select array['(0,x)']::ctp[]": ("23514", _PINT, "14"),
}


@pytest.mark.backlog("RP-945")
@pytest.mark.parametrize("query", list(ASSIGNED))
def test_assigned_literal_domain_error_position(query: str, dsn: str) -> None:
    assert answer(dsn, query) == ASSIGNED[query]


# `array_in` reads each element with `domain_in`, which refuses a NULL.
@pytest.mark.backlog("RP-946")
def test_not_null_domain_array_element(dsn: str) -> None:
    assert answer(dsn, "select '{NULL}'::nnint[]") == (
        "23502",
        "domain nnint does not allow null values",
        "8",
    )


# No `ctp = ctp` operator: `=` is `record = record`, and the literal an
# anonymous record, which has no input.
@pytest.mark.backlog("RP-947")
def test_composite_column_equals_untyped_literal(dsn: str) -> None:
    assert answer(dsn, "select * from tct where c = '(0,x)'") == (
        "0A000",
        "input of anonymous composite types is not implemented",
        "29",
    )


PREPARED = {
    "prepare p5 as select 1::inet": ("42846", "cannot cast type integer to inet", "23"),
    "prepare p6 as select '(0,x)'::ctp": ("23514", _PINT, "22"),
}


@pytest.mark.backlog("RP-948")
@pytest.mark.parametrize("query", list(PREPARED))
def test_prepare_error_location(query: str, dsn: str) -> None:
    assert answer(dsn, query) == PREPARED[query]
