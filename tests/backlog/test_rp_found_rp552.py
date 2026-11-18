"""Gap tests of the divergences found while working RP-552 (a field's
domain error in a record literal is at the literal): RP-945 (an untyped
literal written to a composite or domain-array column, or coerced as an
`ARRAY[]` element, reported a field's domain error with no position; still
open for `INSERT ... SELECT`), RP-946
(a NULL element of a NOT NULL domain's array literal was accepted; fixed),
RP-947 (a
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
    "drop table if exists tnn cascade",
    "drop type if exists ctp cascade",
    "drop domain if exists pint, nnint cascade",
    "create domain pint as int check (value > 0)",
    "create domain nnint as int not null",
    "create type ctp as (a pint, b text)",
    "create table tct (c ctp, d pint[])",
    "create table tnn (x nnint[])",
    "insert into tct(c) values ('(1,x)')",
]

_PINT = 'value for domain pint violates check constraint "pint_check"'
_NNINT = "domain nnint does not allow null values"


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:5]:
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
    "insert into tnn values ('{NULL}')": ("23502", _NNINT, "25"),
}


@pytest.mark.backlog_fixed("RP-945")
@pytest.mark.parametrize("query", list(ASSIGNED))
def test_assigned_literal_domain_error_position(query: str, dsn: str) -> None:
    """Each had no position; the literal is read where it is analysed."""
    assert answer(dsn, query) == ASSIGNED[query]


# The SELECT's untyped output column is coerced to the target column in
# the INSERT's analysis, under the literal's position.
@pytest.mark.backlog("RP-945")
def test_insert_select_literal_domain_error_position(dsn: str) -> None:
    assert answer(dsn, "insert into tct(c) select '(0,x)'") == ("23514", _PINT, "27")


# `array_in` reads each element with `domain_in`, which refuses a NULL.
NOT_NULL_ELEMENTS = {
    "select '{NULL}'::nnint[]": ("23502", _NNINT, "8"),
    "select '{{1},{NULL}}'::nnint[]": ("23502", _NNINT, "8"),
    # A quoted NULL is the string, and a domain with no NOT NULL takes one.
    "select '{1,\"2\"}'::nnint[]": [("{1,2}",)],
    "select '{NULL}'::pint[]": [("{NULL}",)],
}


@pytest.mark.backlog_fixed("RP-946")
@pytest.mark.parametrize("query", list(NOT_NULL_ELEMENTS))
def test_not_null_domain_array_element(query: str, dsn: str) -> None:
    """A NULL element of a NOT NULL domain's array literal was accepted."""
    assert answer(dsn, query) == NOT_NULL_ELEMENTS[query]


@pytest.mark.backlog_fixed("RP-946")
def test_not_null_domain_array_element_written(dsn: str) -> None:
    assert answer(dsn, "insert into tnn values ('{NULL}')")[:2] == ("23502", _NNINT)


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
    # Analysis reads the literal whether or not a row reaches it.
    "prepare p7 as select '(0,x)'::ctp from pg_class where false": ("23514", _PINT, "22"),
    "prepare p8 as select '{0}'::pint[]": ("23514", _PINT, "22"),
    # A cast straight to a domain checks it when the expression runs.
    "prepare p9 as select '-5'::pint": "PREPARE",
}


@pytest.mark.backlog_fixed("RP-948")
@pytest.mark.parametrize("query", list(PREPARED))
def test_prepare_error_location(query: str, dsn: str) -> None:
    """A composite or array literal's field domain error was not raised at
    PREPARE; analysis now raises it at the literal."""
    assert answer(dsn, query) == PREPARED[query]
