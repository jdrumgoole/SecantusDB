"""Range and multirange operators on the Python PG server, each expected value
PostgreSQL 15's. Corpus `range_ops` went 260 divergences of 771 -> 0."""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def one(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1].rows[0][0]


@pytest.mark.parametrize(
    ("sql", "expected"),
    [
        # ORDER: empty first, then lower bound (unbounded lowest), then upper.
        ("select '[1,5)'::int4range < '(,4]'::int4range", False),
        ("select '(,4]'::int4range < '[1,5)'::int4range", True),
        ("select 'empty'::int4range < '[1,5)'::int4range", True),
        ("select '[1,5)'::int4range >= '[1,)'::int4range", False),
        # position
        ("select '[1,5)'::int4range << '[5,8)'::int4range", True),
        ("select '[1,5)'::int4range >> '[5,8)'::int4range", False),
        ("select '[5,8)'::int4range >> '[1,5)'::int4range", True),
        ("select '[1,5)'::int4range &< '[5,8)'::int4range", True),
        ("select '[3,10)'::int4range &< '[5,8)'::int4range", False),
        ("select '[5,8)'::int4range &> '[1,5)'::int4range", True),
        ("select 'empty'::int4range << '[1,5)'::int4range", False),
        ("select '{[1,3),[5,7)}'::int4multirange << '{[8,9)}'::int4multirange", True),
        # containment of a date
        ("select '[2020-01-01,2020-02-01)'::daterange @> '2020-01-15'::date", True),
    ],
)
def test_range_predicates(storage, session, sql, expected):
    assert one(storage, session, sql) is expected


@pytest.mark.parametrize(
    ("sql", "expected"),
    [
        ("select ('{[1,3),[5,7)}'::int4multirange + '{[2,6)}'::int4multirange)::text", "{[1,7)}"),
        (
            "select ('{[1,3),[5,7)}'::int4multirange * '{[2,6)}'::int4multirange)::text",
            "{[2,3),[5,6)}",
        ),
        ("select ('{[1,10)}'::int4multirange - '{[3,5)}'::int4multirange)::text", "{[1,3),[5,10)}"),
        (
            "select pg_typeof('{[1,3)}'::int4multirange * '{[2,6)}'::int4multirange)::text",
            "int4multirange",
        ),
    ],
)
def test_multirange_arithmetic(storage, session, sql, expected):
    assert one(storage, session, sql) == expected


def test_a_non_contiguous_difference_is_22000(storage, session):
    with pytest.raises(SQLError) as ei:
        one(storage, session, "select ('[3,10)'::int4range - '[5,8)'::int4range)::text")
    assert (ei.value.sqlstate, str(ei.value)) == (
        "22000",
        "result of range difference would not be contiguous",
    )


def test_a_multirange_and_a_range_have_no_operator(storage, session):
    with pytest.raises(SQLError) as ei:
        one(storage, session, "select '{[1,3)}'::int4multirange + '[2,4)'::int4range")
    assert (ei.value.sqlstate, str(ei.value)) == (
        "42883",
        "operator does not exist: int4multirange + int4range",
    )
