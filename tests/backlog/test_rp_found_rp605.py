"""Gap tests of the divergences found while working RP-605 (an `int2vector`
input's out-of-range error quoted only its number): RP-866 (a literal's
22003 has no position) and RP-867 (an escape-string literal whose value
differs from its spelling has none for its input error).

The expected answers were measured on PostgreSQL 15.19; none needs DDL.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _error(dsn: str, sql: str) -> tuple[str | None, str, str | None]:
    with psycopg.connect(dsn, autocommit=True) as c, pytest.raises(psycopg.Error) as info:
        c.execute(sql)
    e = info.value
    return (e.sqlstate, str(e).splitlines()[0], e.diag.statement_position)


# Each literal's out-of-range error was unplaced; it is at the literal
# (position 8).
OUT_OF_RANGE = {
    "select '70000'::int2": 'value "70000" is out of range for type smallint',
    "select '99999999999'::int4": 'value "99999999999" is out of range for type integer',
    "select '70000 1'::int2vector": 'value "70000 1" is out of range for type smallint',
    "select '4294967296'::oidvector": 'value "4294967296" is out of range for type oid',
    "select '{\"1 70000\"}'::int2vector[]": 'value "70000" is out of range for type smallint',
}


@pytest.mark.backlog_fixed("RP-866")
@pytest.mark.parametrize("sql", list(OUT_OF_RANGE))
def test_out_of_range_literal_has_a_position(sql: str, dsn: str) -> None:
    assert _error(dsn, sql) == ("22003", OUT_OF_RANGE[sql], "8")


# An escape-string literal whose value holds a decoded escape: at the
# literal (position 8) too.
ESCAPED = {
    "select E'a\\tb'::int4": 'invalid input syntax for type integer: "a\tb"',
    "select E'1\\t2'::int2vector": 'invalid input syntax for type smallint: "1\t2"',
}


@pytest.mark.backlog("RP-867")
@pytest.mark.parametrize("sql", list(ESCAPED))
def test_escaped_literal_input_error_has_a_position(sql: str, dsn: str) -> None:
    assert _error(dsn, sql) == ("22P02", ESCAPED[sql], "8")
