"""Gap tests of divergences found while working RP-898 (a zero-count FETCH
answered no row):

- PL/pgSQL's `FETCH ... INTO` with a count other than one is refused with
  42601 and no position, where PostgreSQL 15.19's `pl_gram.y` raises
  0A000 at the FETCH.

Measured on PostgreSQL 15.19 (the shared reference): a DO block creates no
object.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


@pytest.mark.backlog("RP-942")
def test_a_multi_row_plpgsql_fetch_is_feature_not_supported(dsn: str) -> None:
    """PostgreSQL 15.19 refuses `FETCH FORWARD 2 FROM c INTO r` when the
    function is compiled -- a branch that never runs included -- with 0A000
    `FETCH statement cannot return multiple rows` at the FETCH (position
    72 here). This server answers 42601 with no position."""
    block = (
        "do $$ declare c scroll cursor for select 1; r int; begin"
        " if false then fetch forward 2 from c into r; end if; end $$"
    )
    with psycopg.connect(dsn, autocommit=True) as c, pytest.raises(psycopg.Error) as info:
        c.execute(block)
    e = info.value
    assert (e.sqlstate, str(e).splitlines()[0], e.diag.statement_position) == (
        "0A000",
        "FETCH statement cannot return multiple rows",
        str(block.index("fetch forward") + 1),
    )
