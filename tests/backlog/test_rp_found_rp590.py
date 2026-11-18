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
# Measured on PostgreSQL 15.19: money is an int64 by value (so -1 sorts
# above 2), and a bit string's VarBit image starts with its native-endian bit
# count, so B'11111111111' (11 bits) sorts below B'1'.
RECORD_IMAGE_MONEY_AND_BITS = {
    "select (select row('1'::money)) *< (select row('2'::money))": [(True,)],
    "select (select row('-1'::money)) *> (select row('2'::money))": [(True,)],
    "select (select row(B'101')) *< (select row(B'110'))": [(True,)],
    "select (select row(B'1')) *< (select row(B'10'))": [(True,)],
    "select (select row(B'11111111111')) *< (select row(B'1'))": [(False,)],
    "select (select row(B'10'::varbit)) *> (select row(B'1'::varbit))": [(True,)],
}


@pytest.mark.backlog_fixed("RP-976")
@pytest.mark.parametrize("query", list(RECORD_IMAGE_MONEY_AND_BITS))
def test_record_image_over_money_and_bit_strings(query: str, dsn: str) -> None:
    """Money and bit-string fields were 0A000; they compare by their images."""
    assert answer(dsn, query) == RECORD_IMAGE_MONEY_AND_BITS[query]


# Measured on PostgreSQL 15.19: an array compares by its `ArrayType` image
# (ndim, dataoffset, element oid, dims, lower bounds, NULL bitmap, the
# MAXALIGNed elements), so a longer first dimension or a NULL bitmap
# decides before any element; a range by its `RangeType` image (oid, finite
# bounds, the flags byte last), so `[1.0,2]` and `[1.00,2]` differ.
RECORD_IMAGE_ARRAYS_AND_RANGES = [
    ("'{1}'::int[]", "*<", "'{1}'::int[]", False),
    ("'{1}'::int[]", "*=", "'{1}'::int[]", True),
    ("'{-1}'::int[]", "*<", "'{1}'::int[]", False),
    ("'{-1}'::int[]", "*=", "'{1}'::int[]", False),
    ("'{1,2}'::int[]", "*<", "'{2}'::int[]", False),
    ("'{1,2}'::int[]", "*=", "'{2}'::int[]", False),
    ("'{}'::int[]", "*<", "'{1}'::int[]", True),
    ("'{}'::int[]", "*=", "'{1}'::int[]", False),
    ("'{{1,2},{3,4}}'::int[]", "*<", "'{1,2,3,4}'::int[]", False),
    ("'{{1,2},{3,4}}'::int[]", "*=", "'{1,2,3,4}'::int[]", False),
    ("'[0:1]={1,2}'::int[]", "*<", "'{1,2}'::int[]", True),
    ("'[0:1]={1,2}'::int[]", "*=", "'{1,2}'::int[]", False),
    ("array[1,null]::int[]", "*<", "array[1,2]::int[]", False),
    ("array[1,null]::int[]", "*=", "array[1,2]::int[]", False),
    ("array[null,1]::int[]", "*<", "array[1]::int[]", False),
    ("array[null,1]::int[]", "*=", "array[1]::int[]", False),
    ("'{1}'::int8[]", "*<", "'{2}'::int8[]", True),
    ("'{1}'::int8[]", "*=", "'{2}'::int8[]", False),
    ("'{t}'::bool[]", "*<", "'{f,f}'::bool[]", True),
    ("'{t}'::bool[]", "*=", "'{f,f}'::bool[]", False),
    ("'{1.5}'::float8[]", "*<", "'{-1}'::float8[]", False),
    ("'{1.5}'::float8[]", "*=", "'{-1}'::float8[]", False),
    ("'{2020-01-01}'::date[]", "*<", "'{1999-01-01}'::date[]", True),
    ("'{2020-01-01}'::date[]", "*=", "'{1999-01-01}'::date[]", False),
    ("'{1,2}'::int2[]", "*<", "'{1,3}'::int2[]", True),
    ("'{1,2}'::int2[]", "*=", "'{1,3}'::int2[]", False),
    ("'[1,2)'::int4range", "*<", "'[1,2)'::int4range", False),
    ("'[1,2)'::int4range", "*=", "'[1,2)'::int4range", True),
    ("'[1,3)'::int4range", "*<", "'[2,3)'::int4range", True),
    ("'[1,3)'::int4range", "*=", "'[2,3)'::int4range", False),
    ("'empty'::int4range", "*<", "'[2,3)'::int4range", True),
    ("'empty'::int4range", "*=", "'[2,3)'::int4range", False),
    ("'(,3)'::int4range", "*<", "'[2,3)'::int4range", False),
    ("'(,3)'::int4range", "*=", "'[2,3)'::int4range", False),
    ("'[1,)'::int4range", "*<", "'[1,2)'::int4range", False),
    ("'[1,)'::int4range", "*=", "'[1,2)'::int4range", False),
    ("'[1,2)'::int8range", "*<", "'[1,5)'::int8range", True),
    ("'[1,2)'::int8range", "*=", "'[1,5)'::int8range", False),
    ("'[1.0,2]'::numrange", "*<", "'[1.00,2]'::numrange", False),
    ("'[1.0,2]'::numrange", "*=", "'[1.00,2]'::numrange", False),
    ("'[1.5,2]'::numrange", "*<", "'(1.5,2]'::numrange", False),
    ("'[1.5,2]'::numrange", "*=", "'(1.5,2]'::numrange", False),
    ("'[2020-01-01,2020-02-01)'::daterange", "*<", "'[2020-01-02,2020-02-01)'::daterange", True),
    ("'[2020-01-01,2020-02-01)'::daterange", "*=", "'[2020-01-02,2020-02-01)'::daterange", False),
    (
        "'[2020-01-01 10:00,2020-02-01)'::tsrange",
        "*<",
        "'[2020-01-01 09:00,2020-02-01)'::tsrange",
        False,
    ),
    (
        "'[2020-01-01 10:00,2020-02-01)'::tsrange",
        "*=",
        "'[2020-01-01 09:00,2020-02-01)'::tsrange",
        False,
    ),
    ("'[2020-01-01 10:00+00,)'::tstzrange", "*<", "'[2020-01-01 09:00+00,)'::tstzrange", False),
    ("'[2020-01-01 10:00+00,)'::tstzrange", "*=", "'[2020-01-01 09:00+00,)'::tstzrange", False),
]


@pytest.mark.backlog_fixed("RP-976")
@pytest.mark.parametrize(("left", "op", "right", "want"), RECORD_IMAGE_ARRAYS_AND_RANGES)
def test_record_image_over_arrays_and_ranges(
    left: str, op: str, right: str, want: bool, dsn: str
) -> None:
    """Arrays of fixed-length elements and the built-in ranges were 0A000."""
    query = f"select (select row({left})) {op} (select row({right}))"
    assert answer(dsn, query) == [(want,)]


RECORD_IMAGE_VARLENA_ARRAYS = [
    (
        "'{b}'::text[]",
        "*<",
        "'{" + "a" * 63 + "}'::text[]",
        False,
    ),
    ("'{a}'::text[]", "*=", "'{a}'::text[]", True),
    ("'{a,b}'::text[]", "*<", "'{ab}'::text[]", False),
    ("'{x,NULL}'::text[]", "*<", "'{x,y}'::text[]", False),
    ("'{x,NULL,z}'::text[]", "*>", "'{x,NULL,y}'::text[]", True),
    ("'{a}'::varchar[]", "*<", "'{b}'::varchar[]", True),
    ("'{ab}'::varchar[]", "*<", "'{b}'::varchar[]", False),
    ("'{a}'::char(3)[]", "*=", "'{a}'::char(3)[]", True),
    ("'{a}'::char(3)[]", "*<", "'{a}'::char(2)[]", False),
    ("'{1.5}'::numeric[]", "*=", "'{1.50}'::numeric[]", False),
    ("'{1.5}'::numeric[]", "*<", "'{1.50}'::numeric[]", False),
    ("'{-1}'::numeric[]", "*<", "'{1}'::numeric[]", False),
    ("""'{"\\\\x01"}'::bytea[]""", "*<", """'{"\\\\x0102"}'::bytea[]""", True),
    ("'[2:3]={a,b}'::text[]", "*>", "'{a,b}'::text[]", True),
    ("'{{a,b},{c,d}}'::text[]", "*<", "'{a,b,c,d}'::text[]", False),
]


@pytest.mark.backlog_fixed("RP-976")
@pytest.mark.parametrize(("left", "op", "right", "want"), RECORD_IMAGE_VARLENA_ARRAYS)
def test_record_image_over_arrays_of_varlena_elements(
    left: str, op: str, right: str, want: bool, dsn: str
) -> None:
    """Arrays of text, varchar, bpchar, numeric and bytea were 0A000: an
    element keeps the 4-byte header array_in gives it (PostgreSQL 15.19)."""
    query = f"select (select row({left})) {op} (select row({right}))"
    assert answer(dsn, query) == [(want,)]


# Measured on PostgreSQL 15.19: (left, right, left *< right, left *= right).
RECORD_IMAGE_JSONB = [
    ("null", "true", False, False),
    ("1", "null", False, False),
    ("1", "true", False, False),
    ("1", "1.0", True, False),
    ("1", '"a"', False, False),
    ("1", '"ab"', False, False),
    ("1", "[]", False, False),
    ("1", "{}", False, False),
    ("1", '["a",1]', True, False),
    ("1", '[1,"a"]', True, False),
    ("1", '{"a":1}', False, False),
    ("1", '{"a": 1.0}', False, False),
    ("1", '{"b":1,"a":2}', True, False),
    ("1", '{"aa":1,"b":2}', True, False),
    ("1.0", "null", False, False),
    ("1.0", "true", False, False),
    ("1.0", '"a"', False, False),
    ("1.0", '"ab"', False, False),
    ("1.0", "[]", False, False),
    ("1.0", "{}", False, False),
    ("1.0", '["a",1]', True, False),
    ("1.0", '[1,"a"]', True, False),
    ("1.0", '{"a":1}', False, False),
    ("1.0", '{"a": 1.0}', False, False),
    ("1.0", '{"b":1,"a":2}', True, False),
    ("1.0", '{"aa":1,"b":2}', True, False),
    ('"a"', "null", False, False),
    ('"a"', "true", False, False),
    ('"a"', '"ab"', True, False),
    ('"a"', "[]", False, False),
    ('"a"', "{}", False, False),
    ('"a"', '["a",1]', True, False),
    ('"a"', '[1,"a"]', True, False),
    ('"a"', '{"a":1}', False, False),
    ('"a"', '{"a": 1.0}', False, False),
    ('"a"', '{"b":1,"a":2}', True, False),
    ('"a"', '{"aa":1,"b":2}', True, False),
    ('"ab"', "null", False, False),
    ('"ab"', "true", False, False),
    ('"ab"', "[]", False, False),
    ('"ab"', "{}", False, False),
    ('"ab"', '["a",1]', True, False),
    ('"ab"', '[1,"a"]', True, False),
    ('"ab"', '{"a":1}', False, False),
    ('"ab"', '{"a": 1.0}', False, False),
    ('"ab"', '{"b":1,"a":2}', True, False),
    ('"ab"', '{"aa":1,"b":2}', True, False),
    ("[]", "null", True, False),
    ("[]", "true", True, False),
    ("[]", "{}", False, False),
    ("{}", "null", True, False),
    ("{}", "true", True, False),
    ('["a",1]', "null", False, False),
    ('["a",1]', "true", False, False),
    ('["a",1]', "[]", False, False),
    ('["a",1]', "{}", False, False),
    ('["a",1]', '[1,"a"]', True, False),
    ('["a",1]', '{"a":1}', False, False),
    ('["a",1]', '{"a": 1.0}', False, False),
    ('["a",1]', '{"b":1,"a":2}', False, False),
    ('["a",1]', '{"aa":1,"b":2}', False, False),
    ('[1,"a"]', "null", False, False),
    ('[1,"a"]', "true", False, False),
    ('[1,"a"]', "[]", False, False),
    ('[1,"a"]', "{}", False, False),
    ('[1,"a"]', '{"a":1}', False, False),
    ('[1,"a"]', '{"a": 1.0}', False, False),
    ('[1,"a"]', '{"b":1,"a":2}', False, False),
    ('[1,"a"]', '{"aa":1,"b":2}', False, False),
    ('{"a":1}', "null", True, False),
    ('{"a":1}', "true", True, False),
    ('{"a":1}', "[]", False, False),
    ('{"a":1}', "{}", False, False),
    ('{"a":1}', '{"a": 1.0}', True, False),
    ('{"a":1}', '{"b":1,"a":2}', True, False),
    ('{"a":1}', '{"aa":1,"b":2}', True, False),
    ('{"a": 1.0}', "null", True, False),
    ('{"a": 1.0}', "true", True, False),
    ('{"a": 1.0}', "[]", False, False),
    ('{"a": 1.0}', "{}", False, False),
    ('{"a": 1.0}', '{"b":1,"a":2}', True, False),
    ('{"a": 1.0}', '{"aa":1,"b":2}', True, False),
    ('{"b":1,"a":2}', "null", False, False),
    ('{"b":1,"a":2}', "true", False, False),
    ('{"b":1,"a":2}', "[]", False, False),
    ('{"b":1,"a":2}', "{}", False, False),
    ('{"b":1,"a":2}', '{"aa":1,"b":2}', True, False),
    ('{"aa":1,"b":2}', "null", False, False),
    ('{"aa":1,"b":2}', "true", False, False),
    ('{"aa":1,"b":2}', "[]", False, False),
    ('{"aa":1,"b":2}', "{}", False, False),
    ("[" + ",".join(str(i) for i in range(40)) + "]", '"' + "x" * 70 + '"', False, False),
]


@pytest.mark.backlog_fixed("RP-976")
@pytest.mark.parametrize(("left", "right", "lt", "eq"), RECORD_IMAGE_JSONB)
def test_record_image_over_jsonb(left: str, right: str, lt: bool, eq: bool, dsn: str) -> None:
    """jsonb was 0A000: it compares by memcmp of its JsonbContainer."""
    a, b = (f"(select row('{v}'::jsonb))" for v in (left, right))
    assert answer(dsn, f"select {a} *< {b}, {a} *= {b}") == [(lt, eq)]


RECORD_IMAGE_MORE_ARRAYS = [
    ("array['1'::jsonb]", "*<", "array['1.0'::jsonb]", True),
    ("""array['{"a":1}'::jsonb, null]""", "*<", "array['1'::jsonb]", False),
    ("array['1.1.1.1'::inet]", "*<", "array['1.1.1.1/24'::inet]", False),
    ("array['::1'::inet, null]", "*=", "array['::1'::inet, null]", True),
    ("array['10.0.0.0/8'::cidr]", "*<", "array['10.0.0.0/16'::cidr]", True),
]


@pytest.mark.backlog_fixed("RP-976")
@pytest.mark.parametrize(("left", "op", "right", "want"), RECORD_IMAGE_MORE_ARRAYS)
def test_record_image_over_arrays_of_jsonb_and_inet(
    left: str, op: str, right: str, want: bool, dsn: str
) -> None:
    """Arrays of jsonb, inet and cidr were 0A000 (PostgreSQL 15.19)."""
    query = f"select (select row({left})) {op} (select row({right}))"
    assert answer(dsn, query) == [(want,)]


RECORD_IMAGE_STILL_REFUSED = {
    "select (select row(row(1, 'a')::ca)) *= (select row(row(1, 'a')::ca))": [(True,)],
}


# Measured on PostgreSQL 15.19.
RECORD_IMAGE_FIXED_BYREF_ARRAYS = [
    ("array['1 day'::interval]", "*=", "array['1 day'::interval]", True),
    ("array['1 day'::interval]", "*<", "array['24 hours'::interval]", True),
    ("array['1 day'::interval, null]", "*<", "array['1 day'::interval]", False),
    ("array[null, '1 mon'::interval]", "*>", "array[null, '30 days'::interval]", False),
    (
        "array['00000000-0000-0000-0000-000000000002'::uuid]",
        "*<",
        "array['00000000-0000-0000-0000-000000000001'::uuid,"
        " '00000000-0000-0000-0000-000000000000'::uuid]",
        True,
    ),
    ("array['b'::name]", "*<", "array['ab'::name]", False),
    ("array['a'::name, null]", "*=", "array['a'::name, null]", True),
    ("array['01:00+01'::timetz]", "*<", "array['00:00+00'::timetz]", False),
    ("array['08:00:2b:01:02:03'::macaddr]", "*<", "array['08:00:2b:01:02:04'::macaddr]", True),
    (
        "array['08:00:2b:01:02:03:04:05'::macaddr8, null]",
        "*=",
        "array['08:00:2b:01:02:03:04:05'::macaddr8, null]",
        True,
    ),
    ("array['(1,2)'::tid]", "*<", "array['(0,3)'::tid]", False),
    ("array['(0,1)'::tid, '(0,2)'::tid]", "*<", "array['(0,1)'::tid, '(0,3)'::tid]", True),
]


@pytest.mark.backlog_fixed("RP-976")
@pytest.mark.parametrize(("left", "op", "right", "want"), RECORD_IMAGE_FIXED_BYREF_ARRAYS)
def test_record_image_over_arrays_of_fixed_length_by_reference_elements(
    left: str, op: str, right: str, want: bool, dsn: str
) -> None:
    """Arrays of interval, uuid, name, timetz, macaddr, macaddr8 and tid were
    0A000: each element is its whole image at its type's alignment."""
    query = f"select (select row({left})) {op} (select row({right}))"
    assert answer(dsn, query) == [(want,)]


# Each `(left, right, type)` answers `(left *< right, left *= right,
# right *< left)`, measured on PostgreSQL 15.19.
RECORD_IMAGE_MULTIRANGES = [
    ("{[1,2)}", "{[1,2),[5,6)}", "int4multirange", (True, False, False)),
    ("{[1,2),[5,6)}", "{[1,2),[4,6)}", "int4multirange", (False, False, True)),
    ("{}", "{[1,2)}", "int4multirange", (True, False, False)),
    ("{}", "{}", "int4multirange", (False, True, False)),
    ("{[1,2)}", "{(,2)}", "int8multirange", (True, False, False)),
    ("{[-1,3)}", "{[1,2)}", "int8multirange", (False, False, True)),
    ("{[1.5,2)}", "{[1.50,2)}", "nummultirange", (False, False, True)),
    ("{[1.5,2),[3,4)}", "{[1.5,2),[3.0,4)}", "nummultirange", (True, False, False)),
    ("{[1,1000000),[3000000,4000000)}", "{[1,10000),[3,4)}", "nummultirange", (False, False, True)),
    (
        "{[1,2),[3,4),[5,6),[7,8),[9,10),[11,12)}",
        "{[1,2),[3,4),[5,6),[7,8),[9,11),[12,13)}",
        "int8multirange",
        (True, False, False),
    ),
    (
        "{[1,2),[3,4),[5,6),[7,8),[9,10)}",
        "{[1,2),[3,4),[5,6),[7,8)}",
        "int4multirange",
        (False, False, True),
    ),
    (
        "{[2020-01-01,2020-02-01)}",
        "{[2020-01-02,2020-02-01)}",
        "datemultirange",
        (True, False, False),
    ),
    (
        "{[2020-01-01 00:00,2020-02-01)}",
        "{[2020-01-01 00:00:01,2020-02-01), [2021-01-01,)}",
        "tsmultirange",
        (True, False, False),
    ),
]


@pytest.mark.backlog_fixed("RP-976")
@pytest.mark.parametrize(("left", "right", "ty", "want"), RECORD_IMAGE_MULTIRANGES)
def test_record_image_over_multiranges(
    left: str, right: str, ty: str, want: tuple[bool, bool, bool], dsn: str
) -> None:
    """A built-in multirange field was 0A000: its image is the
    `MultirangeType` `make_multirange` lays out (count, items, flags, then
    each range's bounds at the element's alignment)."""
    a, b = f"(select row('{left}'::{ty}))", f"(select row('{right}'::{ty}))"
    assert answer(dsn, f"select {a} *< {b}, {a} *= {b}, {b} *< {a}") == [want]


@pytest.mark.backlog("RP-976")
@pytest.mark.parametrize("query", list(RECORD_IMAGE_STILL_REFUSED))
def test_record_image_over_still_refused_field_types(query: str, dsn: str) -> None:
    assert answer(dsn, query) == RECORD_IMAGE_STILL_REFUSED[query]
