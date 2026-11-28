"""Gap tests of divergences found while working RP-1090 (a PL/pgSQL CONTEXT
label left a parameter type off the search path unqualified):

- a parameter of a quoted, mixed-case, schema-qualified type loses its
  argument;
- a function's label is fixed when CREATE FUNCTION validates it, not at its
  first call.

The expected answers were measured on PostgreSQL 15.19.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


@pytest.mark.backlog("RP-1233")
def test_a_quoted_qualified_parameter_type_keeps_its_argument(dsn: str) -> None:
    """`g(x "S1090"."T")` called with `'b'` sees `x` as that enum on
    PostgreSQL 15.19: it returns `b` and `pg_typeof(x)` is `"S1090"."T"`,
    and the frame is `g("S1090"."T")`. `secantusd-pg` returns NULL, says
    `record`, and labels it `g(t)`."""
    seen: list[tuple[str, str | None]] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute('drop schema if exists "S1090" cascade')
        c.execute('create schema "S1090"')
        c.execute('create type "S1090"."T" as enum (\'b\')')
        c.execute(
            'create function public.g1090(x "S1090"."T") returns text language plpgsql '
            "as $$ begin raise notice '%', pg_typeof(x); return x::text; end $$"
        )
        c.add_notice_handler(lambda d: seen.append((d.message_primary or "", d.context)))
        try:
            got = c.execute("select public.g1090('b')").fetchone()
        finally:
            c.execute('drop schema "S1090" cascade')
            c.execute("drop function if exists public.g1090")
    assert got == ("b",)
    # The DROP's cascade NOTICE has no CONTEXT.
    assert [x for x in seen if x[1]] == [
        ('"S1090"."T"', 'PL/pgSQL function g1090("S1090"."T") line 1 at RAISE')
    ]


@pytest.mark.backlog("RP-1234")
def test_a_label_is_fixed_when_create_function_validates_it(dsn: str) -> None:
    """`plpgsql_validator` compiles a function at CREATE FUNCTION, and the
    session keeps that compile, `fn_signature` with it: on PostgreSQL 15.19
    a function made with `s1090` on the search path, then called with the
    path back to `public`, is still labelled `f(ty,integer[],ty[])`. A
    second session, which compiles it at the call, labels it
    `s1090.f(s1090.ty,integer[],s1090.ty[])`. `secantusd-pg` compiles at
    the first call, so the first session gets the second's label."""
    contexts: list[str | None] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop schema if exists s1090 cascade")
        c.execute("create schema s1090")
        try:
            c.execute("set search_path = s1090, public")
            c.execute("create type ty as enum ('a')")
            c.execute(
                "create function f(x ty, y int[], z s1090.ty[]) returns int "
                "language plpgsql as $$ begin raise notice 'in'; return 1; end $$"
            )
            c.execute("set search_path = public")
            c.add_notice_handler(lambda d: contexts.append(d.context))
            c.execute("select s1090.f(null, null, null)")
        finally:
            c.execute("reset search_path")
            c.execute("drop schema s1090 cascade")
    assert [x for x in contexts if x] == ["PL/pgSQL function f(ty,integer[],ty[]) line 1 at RAISE"]
