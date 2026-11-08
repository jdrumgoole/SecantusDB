"""Gap tests of the divergences found while working RP-554 (a scalar
subquery's array of a row type was described as `text[]`): RP-600 (an
ARRAY(subquery) of a built-in type was typed from its values, not from the
subquery's column, so `pg_typeof` of one was 42P18 and a timestamp, interval
or regclass element read back NULL), fixed.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster, the cases from `timestamp` on (no DDL) on the
shared one.
"""

from __future__ import annotations

import datetime
from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


# `transformSubLink` types an ARRAY sublink as the array of its subquery's
# column type (`get_promoted_array_type`), so the column's own type decides:
# a `smallint` column is `smallint[]` (1005), and an empty one too.
ARRAY_SUBLINK: dict[str, tuple[int, list[tuple[Any, ...]], str]] = {
    "select array(select 1::int2)": (1005, [([1],)], "smallint[]"),
    "select array(select 'a'::varchar(3))": (1015, [(["a"],)], "character varying[]"),
    "select array(select 1::int2 where false)": (1005, [([],)], "smallint[]"),
    "select array(select 1.5::float4)": (1021, [([1.5],)], "real[]"),
    "select array(select 'x'::name)": (1003, [(["x"],)], "name[]"),
    # A text column is `text[]` with no rows to type it by, not `int4[]`.
    "select array(select 'x' where false)": (1009, [([],)], "text[]"),
    # Values a cast to the array type reads back as themselves: each was
    # NULL, or the number a `pg_lsn` is carried as, before.
    "select array(select '2020-01-01 10:00'::timestamp)": (
        1115,
        [([datetime.datetime(2020, 1, 1, 10, 0)],)],
        "timestamp without time zone[]",
    ),
    "select array(select '1 day'::interval)": (
        1187,
        [([datetime.timedelta(days=1)],)],
        "interval[]",
    ),
    "select array(select 'pg_class'::regclass)": (2210, [(["pg_class"],)], "regclass[]"),
    "select array(select '0/16B3748'::pg_lsn)": (3221, [(["0/16B3748"],)], "pg_lsn[]"),
    # An array column is its own type: the result is one more dimension.
    "select array(select array[1::int2])": (1005, [([[1]],)], "smallint[]"),
}


@pytest.mark.backlog_fixed("RP-600")
@pytest.mark.parametrize("query", list(ARRAY_SUBLINK))
def test_array_sublink_element_type(query: str, dsn: str) -> None:
    oid, rows, type_name = ARRAY_SUBLINK[query]
    with psycopg.connect(dsn, autocommit=True) as c:
        cur = c.execute(query)
        assert [d.type_code for d in cur.description] == [oid]
        assert cur.fetchall() == rows
        assert c.execute(f"select pg_typeof(({query[7:]}))").fetchall() == [(type_name,)]
