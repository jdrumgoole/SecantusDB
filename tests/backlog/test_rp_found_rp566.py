"""Gap tests of what was found while working RP-566 (a SQL routine over a
type that does not exist was created).

An answer is `"ok"` for a statement with no rows, or `(sqlstate, message,
position)` for an error. The expected answers were measured on a private,
freshly initdb'd PostgreSQL 15.19 cluster.
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


def _located(c: psycopg.Connection, query: str) -> Any:
    try:
        c.execute(query)
    except psycopg.Error as e:
        return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
    return "ok"


# `plpgsql_validator` compiles the body at CREATE FUNCTION
# (`check_function_bodies`), and compiling a DECLARE looks its type up
# (`parse_datatype`), so an unknown one is 42704 at the name in the body.
# Here the function is created, and the error waits for the first call.
@pytest.mark.backlog("RP-573")
def test_plpgsql_declare_of_an_undefined_type_is_refused(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            got = _located(
                c,
                "create function b573() returns int language plpgsql"
                " as $$ declare x nosuch573; begin return 1; end $$",
            )
        finally:
            c.execute("drop function if exists b573")
    assert got == ("42704", 'type "nosuch573" does not exist', "69")
