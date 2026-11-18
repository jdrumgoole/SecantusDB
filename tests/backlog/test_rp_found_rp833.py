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


@pytest.mark.backlog_fixed("RP-1018")
def test_a_pg_temp_function_is_labelled_with_its_schema(dsn: str) -> None:
    """A `pg_temp` function's frame is `pg_temp_N.f833()` on PostgreSQL
    15.19 (`format_procedure` qualifies a schema off the search path); it
    was the bare `f833()`, and is now qualified."""
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


@pytest.mark.backlog_fixed("RP-1034")
def test_a_function_label_is_fixed_at_its_first_compile(dsn: str) -> None:
    """`plpgsql_compile` labels a function once per session: `s1034.f()`
    called first off the path keeps that label after the path reaches it.
    It was relabelled at every call; RP-1034 keeps the first label."""
    contexts: list[str | None] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop schema if exists s1034 cascade")
        c.execute("create schema s1034")
        c.execute(
            "create function s1034.f() returns int language plpgsql "
            "as $$ begin raise notice 'in'; return 1; end $$"
        )
        c.add_notice_handler(lambda d: contexts.append(d.context))
        try:
            c.execute("select s1034.f()")
            c.execute("set search_path = s1034, public")
            c.execute("select f()")
        finally:
            c.execute("reset search_path")
            c.execute("drop schema s1034 cascade")
    assert [x for x in contexts if x] == ["PL/pgSQL function s1034.f() line 1 at RAISE"] * 2


@pytest.mark.backlog("RP-1051")
def test_an_identical_replace_relabels_the_function(dsn: str) -> None:
    """Another session's identical CREATE OR REPLACE rewrites the `pg_proc`
    row, so the next call recompiles and labels afresh: on PostgreSQL 15.19
    `s1051.f()` becomes `f()` once the path reaches it."""
    sql = (
        "create or replace function s1051.f() returns int language plpgsql "
        "as $$ begin raise notice 'in'; return 1; end $$"
    )
    contexts: list[str | None] = []
    with psycopg.connect(dsn, autocommit=True) as c, psycopg.connect(dsn, autocommit=True) as o:
        c.execute("drop schema if exists s1051 cascade")
        c.execute("create schema s1051")
        c.execute(sql)
        c.add_notice_handler(lambda d: contexts.append(d.context))
        try:
            c.execute("select s1051.f()")
            o.execute(sql)
            c.execute("set search_path = s1051, public")
            c.execute("select f()")
        finally:
            c.execute("reset search_path")
            c.execute("drop schema s1051 cascade")
    assert [x for x in contexts if x] == [
        "PL/pgSQL function s1051.f() line 1 at RAISE",
        "PL/pgSQL function f() line 1 at RAISE",
    ]
