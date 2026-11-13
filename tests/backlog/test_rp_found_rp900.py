"""Gap tests of divergences found while working RP-900 (`pg_cursors.statement`
was a deparse of the cursor's query):

- the extended protocol's unnamed portal is listed in `pg_cursors`, under
  pgwire's placeholder name `POSTGRESQL_DEFAULT_NAME`.

The expected answers were measured on PostgreSQL 15.19 (the shared reference
on port 5415).
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


@pytest.mark.backlog("RP-936")
def test_the_unnamed_portal_is_not_in_pg_cursors(dsn: str) -> None:
    """PostgreSQL 15.19 creates the unnamed portal of a Bind invisible
    (`exec_bind_message`), so `pg_cursors` does not list it. Here a
    parameterised statement in a block leaves a row named
    `POSTGRESQL_DEFAULT_NAME` there until the block ends."""
    with psycopg.connect(dsn, autocommit=True) as c, c.transaction():
        c.execute("select %s::int", (1,))
        assert c.execute("select name from pg_cursors").fetchall() == []
