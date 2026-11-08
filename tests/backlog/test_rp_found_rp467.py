"""Gap tests of the divergences found while working RP-467 (`pg_get_viewdef`
of a join names its columns as ruleutils does): a view whose `*` covers a
table with an alias column list (RP-512, fixed), a materialized view a DROP
of what it reads does not reach (RP-525), and two schemas' tables of one
name in a FROM (RP-526).

An answer is `(column names, rows)`, or `(sqlstate, message)` for an
error. The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster after `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists fs cascade",
    "create table fs (a int, c text)",
    "insert into fs values (1, 'y')",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def view_answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            c.execute("create view rp_sv as " + query)
            cur = c.execute("select * from rp_sv")
            return ([d.name for d in cur.description or []], cur.fetchall())
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary)
        finally:
            c.execute("drop view if exists rp_sv")


# PostgreSQL expands a view's `*` into the columns as the alias list names
# them, so the view reads `x, y` (or `x, c` under a shorter list). CREATE
# VIEW's star expansion (`view_deps::expand_stars`) wrote the table's own
# names, `f.a, f.c`, and every read of the view was 42703 (RP-512).
ALIASED_TABLE_STAR = {
    "select * from fs f(x, y)": (["x", "y"], [(1, "y")]),
    "select * from fs f(x)": (["x", "c"], [(1, "y")]),
    "select f.* from fs f(x)": (["x", "c"], [(1, "y")]),
}


@pytest.mark.backlog_fixed("RP-512")
@pytest.mark.parametrize("query", list(ALIASED_TABLE_STAR))
def test_view_star_over_an_aliased_column_list(query: str, dsn: str) -> None:
    assert view_answer(dsn, query) == ALIASED_TABLE_STAR[query]


# Found while working RP-467's qualified aliases: a materialized view is a
# dependent of the relations it reads. PostgreSQL refuses a plain DROP of
# one of them (2BP01, naming the view) and DROP ... CASCADE, of the table or
# of its schema, drops the view with it. Here the DROP succeeds and the
# view stays behind, reading a relation that is gone.
def drop_answer(dsn: str, drop: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop schema if exists rq467m cascade")
        c.execute("drop materialized view if exists rq467mv")
        c.execute("create schema rq467m")
        c.execute("create table rq467m.t (a int)")
        c.execute("create materialized view rq467mv as select a from rq467m.t")
        try:
            c.execute(drop)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.message_detail)
        finally:
            row = c.execute("select to_regclass('rq467mv')::text").fetchone()
            c.execute("drop materialized view if exists rq467mv")
            c.execute("drop schema if exists rq467m cascade")
        return ("dropped", row[0] if row else None)


MATVIEW_DEPENDENCY = {
    "drop table rq467m.t": (
        "2BP01",
        "cannot drop table rq467m.t because other objects depend on it",
        "materialized view rq467mv depends on table rq467m.t",
    ),
    "drop table rq467m.t cascade": ("dropped", None),
    "drop schema rq467m cascade": ("dropped", None),
}


@pytest.mark.backlog("RP-525")
@pytest.mark.parametrize("drop", list(MATVIEW_DEPENDENCY))
def test_drop_reaches_a_dependent_materialized_view(drop: str, dsn: str) -> None:
    assert drop_answer(dsn, drop) == MATVIEW_DEPENDENCY[drop]


# `checkNameSpaceConflicts`: two FROM items conflict when their names are
# equal, except two unaliased tables that are different relations (two
# schemas' `t`), which may share the name `t`. Here the JOIN of two such
# tables is refused, and a view over one aliased `t` beside another
# schema's unaliased `t` is accepted (its definition then compares `t.a`
# with itself).
def namespace_answer(dsn: str, statement: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop schema if exists rq467n1, rq467n2 cascade")
        c.execute("create schema rq467n1")
        c.execute("create schema rq467n2")
        c.execute("create table rq467n1.t (a int)")
        c.execute("create table rq467n2.t (a int)")
        try:
            cur = c.execute(statement)
            return cur.fetchall() if cur.description else ("ok",)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary)
        finally:
            c.execute("drop view if exists rq467nv")
            c.execute("drop schema if exists rq467n1, rq467n2 cascade")


SAME_NAME_RELATIONS = {
    "select 1 from rq467n1.t join rq467n2.t on true": [],
    "create view rq467nv as select t.a from rq467n1.t t join rq467n2.t on rq467n2.t.a = t.a": (
        "42712",
        'table name "t" specified more than once',
    ),
}


@pytest.mark.backlog("RP-526")
@pytest.mark.parametrize("statement", list(SAME_NAME_RELATIONS))
def test_same_name_relations_in_from(statement: str, dsn: str) -> None:
    assert namespace_answer(dsn, statement) == SAME_NAME_RELATIONS[statement]
