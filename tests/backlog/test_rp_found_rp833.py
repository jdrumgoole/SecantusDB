"""Gap tests of divergences found while working RP-833 (a RAISE below
ERROR carried no CONTEXT):

- a function in `pg_temp` is labelled without its schema.

The expected answers were measured on PostgreSQL 15.19. The test creates
one function, in its own session's `pg_temp`.
"""

from __future__ import annotations

import re
from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


@pytest.mark.backlog("RP-1018")
def test_a_pg_temp_function_is_labelled_with_its_schema(dsn: str) -> None:
    """A `pg_temp` function's frame is `pg_temp_N.f833()` on PostgreSQL
    15.19 (`format_procedure` qualifies a schema off the search path); here
    it is the bare `f833()`."""
    contexts: list[str | None] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(
            "create function pg_temp.f833() returns int language plpgsql "
            "as $$ begin raise notice 'in'; return 1; end $$"
        )
        c.add_notice_handler(lambda d: contexts.append(d.context))
        c.execute("select pg_temp.f833()")
    assert len(contexts) == 1
    assert re.fullmatch(
        r"PL/pgSQL function pg_temp_\d+\.f833\(\) line 1 at RAISE", contexts[0] or ""
    ), contexts
