"""Gap test of the divergence found while working RP-864 (a temporal or
bytea DEFAULT printed its internal form): the text a `date` or `timestamp`
becomes through its output function inside an expression -- `concat`,
`format`, a record's text -- is ISO whatever the session DateStyle (RP-894).

The expected answers were measured on PostgreSQL 15.19; none needs DDL.
`to_json` is the control: JSON writes ISO under every DateStyle.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


# `date_out` / `timestamp_out` write in the session DateStyle, and `concat`,
# `format('%s')` and `record_out` call them. Here each writes ISO.
@pytest.mark.backlog("RP-894")
def test_a_date_as_text_in_an_expression_follows_the_datestyle(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("set datestyle to 'German'")
        got = c.execute(
            "select concat('2020-01-02'::date, '|', '2020-01-01 12:00'::timestamp),"
            " format('%s', '2020-01-02'::date),"
            " row('2020-01-02'::date, '2020-01-01 12:00'::timestamp)::text,"
            " to_json('2020-01-02'::date)::text,"
            " to_json('2020-01-01 12:00'::timestamp)::text"
        ).fetchone()
    assert got == (
        "02.01.2020|01.01.2020 12:00:00",
        "02.01.2020",
        '(02.01.2020,"01.01.2020 12:00:00")',
        '"2020-01-02"',
        '"2020-01-01T12:00:00"',
    )
