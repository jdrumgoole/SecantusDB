"""Gap tests of the divergences found while working RP-510 (a malformed
range literal's DETAIL): RP-878 (a malformed multirange literal's DETAIL,
and a bad member reported as a multirange literal).

An answer is `("ok", rows)`, or `(sqlstate, message, detail, hint,
position)` for an error. The expected answers were measured on PostgreSQL
15.19; the queries create nothing.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
            return ("ok", cur.fetchall())
        except psycopg.Error as e:
            d = e.diag
            return (
                e.sqlstate,
                d.message_primary,
                d.message_detail,
                d.message_hint,
                d.statement_position,
            )


# RP-878: `multirange_in` reads the members in place, so every failure,
# a bad member's included, is a malformed MULTIRANGE literal with the
# DETAIL of the state it stopped in.
MULTIRANGE_LITERAL_DETAIL = {
    "select '{[1,2}'::int4multirange": ("{[1,2}", "Unexpected end of input."),
    "select '{[1,2)'::int4multirange": ("{[1,2)", "Unexpected end of input."),
    "select '[1,2)'::int4multirange": ("[1,2)", "Missing left brace."),
    "select '{[1,2)} x'::int4multirange": ("{[1,2)} x", "Junk after closing right brace."),
    "select '{[1,2),x}'::int4multirange": ("{[1,2),x}", "Expected range start."),
}


@pytest.mark.backlog("RP-878")
@pytest.mark.parametrize("query", list(MULTIRANGE_LITERAL_DETAIL))
def test_multirange_literal_detail(query: str, dsn: str) -> None:
    value, detail = MULTIRANGE_LITERAL_DETAIL[query]
    assert answer(dsn, query) == (
        "22P02",
        f'malformed multirange literal: "{value}"',
        detail,
        None,
        "8",
    )
