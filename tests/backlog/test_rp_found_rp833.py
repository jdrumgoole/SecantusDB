"""Gap tests of divergences found while working RP-833 (a RAISE below
ERROR carried no CONTEXT):

- a function in `pg_temp` is labelled without its schema;
- a label leaves a parameter type off the path unqualified (RP-1090).

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


@pytest.mark.backlog_fixed("RP-1051")
def test_an_identical_replace_relabels_the_function(dsn: str) -> None:
    """Another session's identical CREATE OR REPLACE rewrites the `pg_proc`
    row, so the next call recompiles and labels afresh: on PostgreSQL 15.19
    `s1051.f()` becomes `f()` once the path reaches it. The label was cached
    by the CREATE text and stayed `s1051.f()`; it is now cached by the
    function's oid and its row's xmin and cmin."""
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


# What another session does to `s1051r.f(s1051r.ty)` after this session
# compiled it, and whether PostgreSQL 15.19 then recompiled it: only a
# statement that rewrites the `pg_proc` row does. A comment is in
# `pg_description`, and the row holds the type's and the schema's oids.
_REWRITES = [
    ("alter function s1051r.f(s1051r.ty) owner to r1051r", True),
    ("alter function s1051r.f(s1051r.ty) volatile", True),
    ("alter function s1051r.f(s1051r.ty) owner to postgres", False),
    ("comment on function s1051r.f(s1051r.ty) is 'x'", False),
    ("alter type s1051r.ty rename to ty2", False),
    ("alter schema s1051r rename to s1051s", False),
]


@pytest.mark.backlog_fixed("RP-1051")
@pytest.mark.parametrize(("stmt", "recompiles"), _REWRITES)
def test_only_a_rewritten_row_recompiles_the_function(
    dsn: str, stmt: str, recompiles: bool
) -> None:
    """The cached compile is checked against the `pg_proc` row's xmin and
    ctid (`plpgsql_compile`), so a statement that leaves the row as it was
    -- a same-owner ALTER, a COMMENT, a rename of a type or schema it
    references by oid -- keeps the label, and one that rewrites it
    relabels. Before RP-1051 the label was cached by the CREATE text, which
    follows neither rule."""
    path = "s1051s" if "rename to s1051s" in stmt else "s1051r"
    contexts: list[str | None] = []
    with psycopg.connect(dsn, autocommit=True) as c, psycopg.connect(dsn, autocommit=True) as o:
        for s in ("s1051r", "s1051s"):
            c.execute(f"drop schema if exists {s} cascade")
        c.execute("drop role if exists r1051r")
        c.execute("create role r1051r")
        c.execute("create schema s1051r")
        c.execute("create type s1051r.ty as enum ('a')")
        c.execute(
            "create function s1051r.f(x s1051r.ty) returns int language plpgsql "
            "as $$ begin raise notice 'in'; return 1; end $$"
        )
        c.add_notice_handler(lambda d: contexts.append(d.context))
        try:
            c.execute("select s1051r.f(null)")
            o.execute(stmt)
            c.execute(f"set search_path = {path}, public")
            c.execute("select f(null)")
        finally:
            c.execute("reset search_path")
            for s in ("s1051r", "s1051s"):
                c.execute(f"drop schema if exists {s} cascade")
            c.execute("drop role r1051r")
    first, second = [x for x in contexts if x]
    assert first.startswith("PL/pgSQL function s1051r.f("), contexts
    assert (second != first) == recompiles, contexts
    if recompiles:
        assert second.startswith("PL/pgSQL function f("), contexts


@pytest.mark.backlog_fixed("RP-1051")
def test_creating_a_function_assigns_an_xid(dsn: str) -> None:
    """`ProcedureCreate` writes `pg_proc`, which assigns the transaction an
    xid; an ALTER to the owner it has writes nothing and assigns none
    (PostgreSQL 15.19). CREATE FUNCTION assigned none before RP-1051 stamped
    the row with its writer."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop function if exists f1051x()")
        try:
            c.execute("begin")
            c.execute(
                "create function f1051x() returns int language plpgsql as $$ begin return 1; end $$"
            )
            created = c.execute("select txid_current_if_assigned()").fetchone()
            c.execute("commit")
            c.execute("begin")
            c.execute("alter function f1051x() owner to postgres")
            altered = c.execute("select txid_current_if_assigned()").fetchone()
            c.execute("commit")
        finally:
            c.execute("drop function if exists f1051x()")
    assert created is not None and created[0] is not None
    assert altered == (None,)


@pytest.mark.backlog("RP-1090")
def test_a_label_qualifies_a_parameter_type_off_the_path(dsn: str) -> None:
    """`format_procedure` qualifies each parameter type that is not visible
    on the search path, as it does the function's name: on PostgreSQL 15.19
    `s1100.f(x s1100.ty)` called off the path is labelled
    `s1100.f(s1100.ty)`. `secantusd-pg` labels it `s1100.f(ty)`."""
    contexts: list[str | None] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop schema if exists s1100 cascade")
        c.execute("create schema s1100")
        c.execute("create type s1100.ty as enum ('a')")
        c.execute(
            "create function s1100.f(x s1100.ty) returns int language plpgsql "
            "as $$ begin raise notice 'in'; return 1; end $$"
        )
        c.add_notice_handler(lambda d: contexts.append(d.context))
        try:
            c.execute("select s1100.f(null)")
        finally:
            c.execute("drop schema s1100 cascade")
    assert [x for x in contexts if x] == ["PL/pgSQL function s1100.f(s1100.ty) line 1 at RAISE"]
