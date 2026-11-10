"""Gap test of a divergence found while working RP-822 (a CREATE
FUNCTION's `%TYPE` parameter was not resolved): a routine named by its
signature in DROP or ALTER FUNCTION does not resolve one either.

`LookupFuncWithArgs` looks each argument type up with `LookupTypeName`,
so `t.c%TYPE` there is the column's type, with the same NOTICE CREATE
FUNCTION gives. The expected answers were measured on a private, freshly
initdb'd PostgreSQL 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t830 cascade",
    "drop function if exists f830",
    "drop function if exists g830",
    "create table t830 (v varchar(3))",
    "create function f830(x varchar) returns int language sql as $$ select 1 $$",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:3]:
            c.execute(q)


def _run(dsn: str, statement: str) -> tuple[object, list[str]]:
    notices: list[str] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        c.add_notice_handler(lambda d: notices.append(d.message_primary or ""))
        try:
            c.execute(statement)
        except psycopg.Error as e:
            return ("error", e.sqlstate, e.diag.message_primary), notices
        names = c.execute(
            "select proname::text from pg_proc where proname in ('f830', 'g830')"
        ).fetchall()
    return names, notices


NOTICE = ["type reference t830.v%TYPE converted to character varying"]

SIGNATURES = {
    "drop function f830(t830.v%type)": ([], NOTICE),
    "alter function f830(t830.v%type) rename to g830": ([("g830",)], NOTICE),
}


@pytest.mark.backlog("RP-830")
@pytest.mark.parametrize("statement", list(SIGNATURES))
def test_a_signatures_percent_type_is_resolved(statement: str, dsn: str) -> None:
    assert _run(dsn, statement) == SIGNATURES[statement]
