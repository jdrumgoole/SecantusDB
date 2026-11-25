"""Gap tests of the divergences found while working RP-552 (a field's
domain error in a record literal is at the literal): RP-945 (an untyped
literal written to a composite or domain-array column, or coerced as an
`ARRAY[]` element or an `INSERT ... SELECT` output column, reported a
field's domain error with no position; fixed), RP-1044 (a `text` SELECT
column written to a composite column was accepted; fixed), RP-1048 (a
`text` SELECT column written to a domain-array column was accepted; fixed), RP-1049
(a `record` SELECT column written to a composite column was accepted;
fixed), RP-1086 (one written to a non-composite column is 0A000), RP-1050
(a UNION of a composite and a record has no error position), RP-946
(a NULL element of a NOT NULL domain's array literal was accepted; fixed),
RP-947 (a
composite column compared with an untyped literal read the literal as the
column's type; fixed) and RP-948 (under `PREPARE`, a recorded error location is
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
    "drop table if exists tdc cascade",
    "drop domain if exists dctp cascade",
    "drop type if exists ctp cascade",
    "drop domain if exists pint, nnint cascade",
    "create domain pint as int check (value > 0)",
    "create domain nnint as int not null",
    "create type ctp as (a pint, b text)",
    "create domain dctp as ctp",
    "create table tdc (e dctp)",
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
        for q in _SETUP[:7]:
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
# the INSERT's analysis, under the literal's position, even when no row is
# selected.
SELECTED = {
    "insert into tct(c) select '(0,x)'": ("23514", _PINT, "27"),
    "insert into tct(c) select '(0,x)' where false": ("23514", _PINT, "27"),
    "insert into tct(d) select '{0}'": ("23514", _PINT, "27"),
    "insert into tct(c, d) select '(1,x)', '{0}'": ("23514", _PINT, "39"),
    "insert into tct(c) select '(0,x)' from tct": ("23514", _PINT, "27"),
    "insert into tct(c) (select '(0,x)')": ("23514", _PINT, "28"),
    "with w as (select 1) insert into tct(c) select '(0,x)'": ("23514", _PINT, "48"),
    "insert into tct select '(0,x)' as z": ("23514", _PINT, "24"),
    "insert into tnn select '{NULL}'": ("23502", _NNINT, "24"),
}


@pytest.mark.backlog_fixed("RP-945")
@pytest.mark.parametrize("query", list(SELECTED))
def test_insert_select_literal_domain_error_position(query: str, dsn: str) -> None:
    """Each had no position (and `where false` was accepted)."""
    assert answer(dsn, query) == SELECTED[query]


# A set operation, ORDER BY or DISTINCT resolves the literal to `text`
# first, and `text` has no assignment cast to a composite.
TEXT_SELECTED = {
    "insert into tct(c) select '(0,x)' union all select '(1,x)'": "27",
    "insert into tct(c) select '(1,x)' group by 1": "27",
    "insert into tct(c) select '(1,x)' where false union select '(1,x)'": "27",
    "insert into tct(c) select null::text": "27",
    "insert into tct(c) select * from (select '(0,x)') s": "27",
    "insert into tct(c) select '(0,x)'::text": "27",
    "insert into tct(c) select '(0,x)' order by 1": "27",
    "insert into tct(c) select distinct '(0,x)'": "36",
}


@pytest.mark.backlog_fixed("RP-1044")
@pytest.mark.parametrize("query", list(TEXT_SELECTED))
def test_insert_select_text_into_composite(query: str, dsn: str) -> None:
    """42804 at the column; the text was read as the composite."""
    assert answer(dsn, query) == (
        "42804",
        'column "c" is of type ctp but expression is of type text',
        TEXT_SELECTED[query],
    )


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
COMPOSITE_EQUALS_LITERAL = {
    "select * from tct where c = '(0,x)'": "29",
    "select * from tct where '(0,x)' = c": "25",
    "select * from tct where c in ('(0,x)')": "31",
    "select * from tct where c is distinct from '(0,x)'": "44",
}


@pytest.mark.backlog_fixed("RP-947")
@pytest.mark.parametrize("query", list(COMPOSITE_EQUALS_LITERAL))
def test_composite_column_equals_untyped_literal(query: str, dsn: str) -> None:
    """The literal was read as `ctp`, so a field's domain error came first."""
    assert answer(dsn, query) == (
        "0A000",
        "input of anonymous composite types is not implemented",
        COMPOSITE_EQUALS_LITERAL[query],
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


_PINT_ARRAY = 'column "d" is of type pint[] but expression is of type {}'

# `find_coercion_pathway` recurses into two arrays' elements, and `int` is
# `pint`'s base: an array of a type with an assignment cast to `int` is
# coerced, anything else (`text`, `text[]`, a `NULL` typed `text`) is 42804.
DOMAIN_ARRAY = {
    "insert into tct(d) select '{1}' union all select '{1}'": (
        "42804",
        _PINT_ARRAY.format("text"),
        "27",
    ),
    "insert into tct(d) select '{1}'::text": ("42804", _PINT_ARRAY.format("text"), "27"),
    "insert into tct(d) select array['1'::text]": (
        "42804",
        _PINT_ARRAY.format("text[]"),
        "27",
    ),
    "insert into tct(d) select null::text": ("42804", _PINT_ARRAY.format("text"), "27"),
    "insert into tct(d) select array[1]": "INSERT 0 1",
    "insert into tct(d) select array[1::int8]": "INSERT 0 1",
    "insert into tct(d) select array[1.5]": "INSERT 0 1",
}


@pytest.mark.backlog_fixed("RP-1048")
@pytest.mark.parametrize("query", list(DOMAIN_ARRAY))
def test_insert_select_text_into_domain_array(query: str, dsn: str) -> None:
    """`text` has no assignment cast to `pint[]`; it was read as the array,
    where the target's domain element is now looked through to `int`."""
    assert answer(dsn, query) == DOMAIN_ARRAY[query]


@pytest.mark.backlog_fixed("RP-1049")
def test_insert_select_record_into_composite(dsn: str) -> None:
    """`record` to `ctp` is not an assignment coercion; it was stored."""
    assert answer(dsn, "insert into tct(c) select row(1,'x')") == (
        "42846",
        "cannot cast type record to ctp",
        "27",
    )


@pytest.mark.backlog_fixed("RP-1049")
def test_insert_select_record_into_domain_over_composite(dsn: str) -> None:
    """The refusal names the domain, the column's own type, not its base."""
    assert answer(dsn, "insert into tdc(e) select row(1,'x')") == (
        "42846",
        "cannot cast type record to dctp",
        "27",
    )


@pytest.mark.backlog_fixed("RP-1049")
def test_values_record_into_composite_is_stored(dsn: str) -> None:
    """A `RowExpr` written directly, as in `VALUES`, is coerced and stored."""
    assert answer(dsn, "insert into tct(c) values (row(1,'x')), (row(2,'y'))") == "INSERT 0 2"


@pytest.mark.backlog("RP-1086")
def test_insert_select_record_into_non_composite(dsn: str) -> None:
    """`record` to `pint[]` has no assignment cast; here it is 0A000."""
    assert answer(dsn, "insert into tct(d) select row(1,'x')") == (
        "42804",
        'column "d" is of type pint[] but expression is of type record',
        "27",
    )


@pytest.mark.backlog("RP-1050")
def test_union_composite_record_position(dsn: str) -> None:
    """The UNION's type mismatch is placed at the right arm's column."""
    assert answer(dsn, "insert into tct(c) select (1,'x')::ctp union select row(2,'y')") == (
        "42804",
        "UNION types ctp and record cannot be matched",
        "53",
    )
