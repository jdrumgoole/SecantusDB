"""RP-590's WHERE regression, and the gap test of what was found while
working it: RP-650 (the `record_image` operators `*=` .. `*>=` were
refused; fixed).

Two different composite types compare through `record = record`, which
`record_eq` / `record_cmp` run field by field (`test_rp_found_rp530.py`
holds the select-list answers). In a WHERE, a composite column compared
with a record of other field types or names was lowered to a filter that
compared the stored record whole, so it matched no row where PostgreSQL
compares the fields by position or raises `record_eq`'s 42804.

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
    "drop table if exists tca",
    "drop type if exists ca, cb, cc, cd cascade",
    "create type ca as (x int, y text)",
    "create type cb as (x int, y int)",
    "create type cc as (x int, y text, z int)",
    "create type cd as (p int, q text)",
    "create table tca (k int, v ca)",
    "insert into tca values (1, row(1, 'a')), (2, row(2, 'b')), (3, null)",
]

_DISSIMILAR = (
    "42804",
    "cannot compare dissimilar column types text and integer at record column 2",
    None,
    None,
)
_WIDTHS = ("42804", "cannot compare record types with different numbers of columns", None, None)


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


# `record_eq` / `record_cmp` compare by position (`cd`'s names are not
# `ca`'s), check each field pair they reach, and check the widths once the
# pairs both have compared equal (`(2, 'a', 0)::cc` is decided unequal
# before that).
WHERE_COMPOSITES = {
    "select k from tca where v = row(1, 'a')::cd order by k": [(1,)],
    "select k from tca where v < row(2, 'a')::cd order by k": [(1,)],
    "select k from tca where v = (select row(2, 'b')::cd) order by k": [(2,)],
    "select k from tca where v = row(1, 2)::cb order by k": _DISSIMILAR,
    "select k from tca where v = row(2, 2)::cb order by k": _DISSIMILAR,
    "select k from tca where v = row(1, 'a', 0)::cc order by k": _WIDTHS,
    "select k from tca where v = row(2, 'a', 0)::cc order by k": [],
    "select k from tca where v = (1, 'a') order by k": (
        "42804",
        "cannot compare dissimilar column types text and unknown at record column 2",
        None,
        None,
    ),
    "select k from tca where v is not distinct from row(1, 2)::cb order by k": _DISSIMILAR,
    "select k from tca where v in (row(1, 'a', 0)::cc, row(2, 'c', 0)::cc) order by k": _WIDTHS,
    "select k from tca where v not in (row(1, 'a')::cd) order by k": [(2,)],
    "select k from tca where v = any(array[row(1, 2)::cb]) order by k": _DISSIMILAR,
    "select k from tca where v between row(1, 2)::cb and row(1, 3)::cb order by k": _DISSIMILAR,
    "select k from tca where not (v = row(1, 2)::cb) order by k": _DISSIMILAR,
    "select k from tca where v = row(1, 'a')::ca order by k": [(1,)],
    "select k, v = row(2, 'b')::cd from tca order by k": [(1, False), (2, True), (3, None)],
}


@pytest.mark.backlog_fixed("RP-590")
@pytest.mark.parametrize("query", list(WHERE_COMPOSITES))
def test_where_compares_a_composite_column_as_a_record(query: str, dsn: str) -> None:
    """A composite column against another record type in a WHERE is
    `record_eq` / `record_cmp`'s answer. Each was an empty result, or 42883
    `operator does not exist: ca = cb` before RP-590."""
    assert answer(dsn, query) == WHERE_COMPOSITES[query]


# `record_image_eq` and its siblings compare the fields' binary images:
# `1.0` and `1.00` are equal numerics with different images. Between two
# ROW constructors the operator is applied field by field
# (`make_row_comparison_op`), and `integer *= integer` does not exist.
RECORD_IMAGE = {
    "select (select row(1.0)) *= (select row(1.00)), (select row(1.0)) = (select row(1.00))": [
        (False, True)
    ],
    "select (select row(1, 'a'::text)) *< (select row(1, 'b'::text))": [(True,)],
    "select (select row(2)) *<> (select row(2)), (select row(1)) *>= (select row(1))": [
        (False, True)
    ],
    "select row(1, 'a')::ca *= row(1, 'a')::cd": [(True,)],
    "select k, v *= row(2, 'b')::ca from tca order by k": [(1, False), (2, True), (3, None)],
    "select row(1) *= row(1)": (
        "42883",
        "operator does not exist: integer *= integer",
        "No operator matches the given name and argument types. "
        "You might need to add explicit type casts.",
        "15",
    ),
    # A by-value datum compares as the unsigned word it is stored as.
    "select (select row(-1)) *< (select row(1)), (select row(-1)) *> (select row(1))": [
        (False, True)
    ],
    "select (select row(null::int)) *> (select row(1))": [(True,)],
    "select (select row(10000.0)) *< (select row(9999.99))": [(False,)],
    "select (select row(1, 'a')) *< (select row(1, 'ab'))": (
        "XX000",
        "unexpected attlen: -2",
        None,
        None,
    ),
}


@pytest.mark.backlog_fixed("RP-650")
@pytest.mark.parametrize("query", list(RECORD_IMAGE))
def test_record_image_operators(query: str, dsn: str) -> None:
    assert answer(dsn, query) == RECORD_IMAGE[query]


# `record_image_cmp` compares every by-value or varlena datum, whatever its
# type: a date is its int32 day number, a uuid its 16 bytes. Fixed for
# date, timestamp, timestamptz and uuid; other types stay RP-976.
RECORD_IMAGE_OTHER_TYPES = {
    "select (select row('2024-01-02'::date)) *< (select row('2024-01-03'::date))": [(True,)],
    "select (select row('00000000-0000-0000-0000-000000000001'::uuid))"
    " *= (select row('00000000-0000-0000-0000-000000000001'::uuid))": [(True,)],
    # A negative day count is a large unsigned word: before 2000 sorts after.
    "select (select row('1999-12-31'::date)) *< (select row('2000-01-01'::date))": [(False,)],
    "select (select row('2024-01-01 00:00'::timestamp))"
    " *< (select row('2024-01-01 00:00:01'::timestamp))": [(True,)],
    "select (select row('1990-01-01 00:00+00'::timestamptz))"
    " *> (select row('2024-01-01 00:00+00'::timestamptz))": [(True,)],
    "select (select row('ff000000-0000-0000-0000-000000000000'::uuid))"
    " *> (select row('01000000-0000-0000-0000-000000000000'::uuid))": [(True,)],
}


@pytest.mark.backlog_fixed("RP-976")
@pytest.mark.parametrize("query", list(RECORD_IMAGE_OTHER_TYPES))
def test_record_image_over_other_field_types(query: str, dsn: str) -> None:
    assert answer(dsn, query) == RECORD_IMAGE_OTHER_TYPES[query]


# More field types' datum images: an interval is a 16-byte fixed-length
# datum (so `1 day` and `24 hours` differ), a time its int64 microseconds, a
# name its 64 NAMEDATALEN bytes; the fixed-length ones compare by `memcmp`
# of their little-endian memory (PostgreSQL 15.19).
RECORD_IMAGE_REMAINING_TYPES = {
    "select (select row('1 day'::interval)) *= (select row('24 hours'::interval))": [(False,)],
    "select (select row('1 mon'::interval)) *< (select row('31 days'::interval))": [(True,)],
    "select (select row('-1 hour'::interval)) *> (select row('1 hour'::interval))": [(False,)],
    "select (select row('10:00'::time)) *< (select row('11:00'::time))": [(True,)],
    "select (select row('10:00+00'::timetz)) *= (select row('11:00+01'::timetz))": [(False,)],
    "select (select row('10:00+01'::timetz)) *< (select row('10:00+02'::timetz))": [(False,)],
    "select (select row('a'::name)) *= (select row('a'::name))": [(True,)],
    "select (select row('b'::name)) *> (select row('ab'::name))": [(True,)],
    """select (select row('a'::"char")) *< (select row('b'::"char"))""": [(True,)],
    "select (select row('08:00:2b:01:02:03'::macaddr))"
    " *< (select row('08:00:2b:01:02:04'::macaddr))": [(True,)],
    "select (select row('10.0.0.1'::inet)) *< (select row('::1'::inet))": [(True,)],
    "select (select row('10.0.0.1/8'::inet)) *> (select row('10.0.0.1'::inet))": [(False,)],
    "select (select row('(0,2)'::tid)) *> (select row('(1,1)'::tid))": [(False,)],
    "select (select row('0/10'::pg_lsn)) *< (select row('1/0'::pg_lsn))": [(True,)],
}


@pytest.mark.backlog_fixed("RP-976")
@pytest.mark.parametrize("query", list(RECORD_IMAGE_REMAINING_TYPES))
def test_record_image_over_remaining_field_types(query: str, dsn: str) -> None:
    """Interval, time, timetz, name, "char", macaddr, inet, tid and pg_lsn
    fields were refused (0A000); their datum images are modelled now."""
    assert answer(dsn, query) == RECORD_IMAGE_REMAINING_TYPES[query]


# The field types whose datum image is still not modelled: money (an int64
# by value), an array, a jsonb, a bit string and a range (varlenas with
# their own layouts), measured on PostgreSQL 15.19.
RECORD_IMAGE_STILL_REFUSED = {
    "select (select row('1'::money)) *< (select row('2'::money))": [(True,)],
    "select (select row('{1}'::int[])) *= (select row('{1}'::int[]))": [(True,)],
    """select (select row('{"a":1}'::jsonb)) *= (select row('{"a": 1}'::jsonb))""": [(True,)],
    "select (select row(B'101')) *< (select row(B'110'))": [(True,)],
    "select (select row('[1,2)'::int4range)) *= (select row('[1,2)'::int4range))": [(True,)],
}


@pytest.mark.backlog("RP-976")
@pytest.mark.parametrize("query", list(RECORD_IMAGE_STILL_REFUSED))
def test_record_image_over_still_refused_field_types(query: str, dsn: str) -> None:
    assert answer(dsn, query) == RECORD_IMAGE_STILL_REFUSED[query]
