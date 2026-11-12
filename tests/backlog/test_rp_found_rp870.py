"""What working RP-870 (more aggregates over a window) found:

- RP-904: an aggregate called with the wrong number of arguments
  (`stddev(v, v)`, `corr(v)`) is this server's 0A000 `an aggregate with this
  many arguments`, with or without OVER, where PostgreSQL has no such
  function: 42883 naming the argument types.

Measured on PostgreSQL 15.19 (the shared reference on port 5415), VALUES-only
statements as written.
"""

from __future__ import annotations

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> str:
    return _dsn(rust_pg) + " dbname=postgres"


def _rows(dsn: str, query: str) -> object:
    """The rows, or the error's SQLSTATE and first line."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            return c.execute(query).fetchall()
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


WRONG_ARITY = {
    "select stddev(v, v) from (values (1)) s(v)": "stddev(integer, integer)",
    "select corr(v) from (values (1)) s(v)": "corr(integer)",
    "select stddev(v, v) over () from (values (1)) s(v)": "stddev(integer, integer)",
    "select corr(v) over () from (values (1)) s(v)": "corr(integer)",
}


@pytest.mark.backlog("RP-904")
@pytest.mark.parametrize("query", list(WRONG_ARITY))
def test_an_aggregate_with_the_wrong_arity_is_42883(query: str, dsn: str) -> None:
    assert _rows(dsn, query) == (
        "error",
        "42883",
        f"function {WRONG_ARITY[query]} does not exist",
    )
