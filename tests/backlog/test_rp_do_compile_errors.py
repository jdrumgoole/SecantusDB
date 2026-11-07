"""Gap tests of a DO block's compile errors, found while working RP-486
(2026-11-07).

- RP-492: a syntax error in a block the function interpreter compiles
  carries no statement position.

The expected answers were measured on PostgreSQL 15.19; a DO block that
does not run creates no object, so the shared reference cluster answers
them as a fresh one does.
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


def _do(dsn: str, body: str) -> Any:
    """A DO block's `(sqlstate, message, statement position)`, or "DO"."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            c.execute(f"do $$ {body} $$")
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        return "DO"


# A body the DO block's own path does not read (a DECLARE section, a
# label) is compiled by the function interpreter; PostgreSQL points each
# error at its token in the DO statement.
COMPILE_ERROR = {
    "declare x int; begin raise notice 'z' end": (
        "42601",
        'syntax error at or near "end"',
        "45",
    ),
    "<<blk>> begin raise notice 'z' end": (
        "42601",
        'syntax error at or near "end"',
        "38",
    ),
    "<<blk>> begin raise notice 'z'; end nope": (
        "42601",
        'end label "nope" differs from block\'s label "blk"',
        "43",
    ),
}


@pytest.mark.backlog("RP-492")
@pytest.mark.parametrize("body", list(COMPILE_ERROR))
def test_compile_error_has_a_position(body: str, dsn: str) -> None:
    assert _do(dsn, body) == COMPILE_ERROR[body]
