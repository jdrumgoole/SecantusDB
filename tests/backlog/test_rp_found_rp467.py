"""Gap tests of the divergences found while working RP-467 (`pg_get_viewdef`
of a join names its columns as ruleutils does): a view whose `*` covers a
table with an alias column list (RP-512, fixed), a materialized view a DROP
of what it reads did not reach (RP-525, fixed), and two schemas' tables of one
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
# of its schema, drops the view with it. The DROP succeeded and the view
# stayed behind, reading a relation that was gone (RP-525).
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


@pytest.mark.backlog_fixed("RP-525")
@pytest.mark.parametrize("drop", list(MATVIEW_DEPENDENCY))
def test_drop_reaches_a_dependent_materialized_view(drop: str, dsn: str) -> None:
    assert drop_answer(dsn, drop) == MATVIEW_DEPENDENCY[drop]


# RP-525's regression: a materialized view is a dependent exactly as a view
# is, wherever it sits in the chain -- over a view, under one, over another
# materialized view, in the dropped schema -- and DROP MATERIALIZED VIEW
# refuses, or cascades to, what reads it. The readers are named depth first,
# one relation's in the order they were made. Each answer is `(the DROP's
# result, its notices, the relations left)`; measured on a private, freshly
# initdb'd PostgreSQL 15.19 cluster.
_RQ525_RELATIONS = ["rq525t", "rq525mv", "rq525mv2", "rq525v", "rq525v2", "rq525s.mv", "rq525s.t"]
_T = "create table rq525t (a int)"
_MV = "create materialized view rq525mv as select a from rq525t"
_V_MV = "create view rq525v as select a from rq525mv"
_V = "create view rq525v as select a from rq525t"
_MV_V = "create materialized view rq525mv as select a from rq525v"
_HINT = "Use DROP ... CASCADE to drop the dependent objects too."


def _refused(what: str, detail: str) -> tuple[str, str, str, str]:
    return ("2BP01", f"cannot drop {what} because other objects depend on it", detail, _HINT)


def chain_answer(dsn: str, setup: list[str], drop: str) -> Any:
    notices: list[str] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop schema if exists rq525s cascade")
        c.execute("drop table if exists rq525t cascade")
        for q in setup:
            c.execute(q)
        c.add_notice_handler(
            lambda d: notices.append(
                (d.message_primary or "") + (" | " + d.message_detail if d.message_detail else "")
            )
        )
        try:
            c.execute(drop)
            result: Any = "ok"
        except psycopg.Error as e:
            result = (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_detail,
                e.diag.message_hint,
            )
        finally:
            seen = list(notices)
            left = [
                r
                for r in _RQ525_RELATIONS
                if c.execute("select to_regclass(%s)::text", [r]).fetchone() != (None,)
            ]
            c.execute("drop schema if exists rq525s cascade")
            c.execute("drop table if exists rq525t cascade")
        return (result, seen, left)


MATVIEW_CHAINS: dict[str, tuple[list[str], str, Any]] = {
    "view over a materialized view": (
        [_T, _MV, _V_MV],
        "drop table rq525t",
        (
            _refused(
                "table rq525t",
                "materialized view rq525mv depends on table rq525t\n"
                "view rq525v depends on materialized view rq525mv",
            ),
            [],
            ["rq525t", "rq525mv", "rq525v"],
        ),
    ),
    "view over a materialized view, cascade": (
        [_T, _MV, _V_MV],
        "drop table rq525t cascade",
        (
            "ok",
            [
                "drop cascades to 2 other objects | drop cascades to materialized view "
                "rq525mv\ndrop cascades to view rq525v"
            ],
            [],
        ),
    ),
    "materialized view over a view": (
        [_T, _V, _MV_V],
        "drop table rq525t",
        (
            _refused(
                "table rq525t",
                "view rq525v depends on table rq525t\n"
                "materialized view rq525mv depends on view rq525v",
            ),
            [],
            ["rq525t", "rq525mv", "rq525v"],
        ),
    ),
    "materialized view over a materialized view, cascade": (
        [_T, _MV, "create materialized view rq525mv2 as select a from rq525mv"],
        "drop table rq525t cascade",
        (
            "ok",
            [
                "drop cascades to 2 other objects | drop cascades to materialized view "
                "rq525mv\ndrop cascades to materialized view rq525mv2"
            ],
            [],
        ),
    ),
    "readers in the order they were made": (
        [
            _T,
            "create view rq525v as select a from rq525t",
            _MV,
            "create view rq525v2 as select a from rq525v",
            "create materialized view rq525mv2 as select a from rq525v",
        ],
        "drop table rq525t",
        (
            _refused(
                "table rq525t",
                "view rq525v depends on table rq525t\n"
                "view rq525v2 depends on view rq525v\n"
                "materialized view rq525mv2 depends on view rq525v\n"
                "materialized view rq525mv depends on table rq525t",
            ),
            [],
            ["rq525t", "rq525mv", "rq525mv2", "rq525v", "rq525v2"],
        ),
    ),
    "drop view under a materialized view": (
        [_T, _V, _MV_V],
        "drop view rq525v",
        (
            _refused("view rq525v", "materialized view rq525mv depends on view rq525v"),
            [],
            ["rq525t", "rq525mv", "rq525v"],
        ),
    ),
    "drop view under a materialized view, cascade": (
        [_T, _V, _MV_V],
        "drop view rq525v cascade",
        ("ok", ["drop cascades to materialized view rq525mv"], ["rq525t"]),
    ),
    "drop materialized view under a view": (
        [_T, _MV, _V_MV],
        "drop materialized view rq525mv",
        (
            _refused(
                "materialized view rq525mv", "view rq525v depends on materialized view rq525mv"
            ),
            [],
            ["rq525t", "rq525mv", "rq525v"],
        ),
    ),
    "drop materialized view under a view, cascade": (
        [_T, _MV, _V_MV],
        "drop materialized view rq525mv cascade",
        ("ok", ["drop cascades to view rq525v"], ["rq525t"]),
    ),
    "schema holding a materialized view": (
        [_T, "create schema rq525s", "create materialized view rq525s.mv as select a from rq525t"],
        "drop schema rq525s",
        (
            _refused("schema rq525s", "materialized view rq525s.mv depends on schema rq525s"),
            [],
            ["rq525t", "rq525s.mv"],
        ),
    ),
    "schema holding a materialized view, cascade": (
        [_T, "create schema rq525s", "create materialized view rq525s.mv as select a from rq525t"],
        "drop schema rq525s cascade",
        ("ok", ["drop cascades to materialized view rq525s.mv"], ["rq525t"]),
    ),
    "schema a materialized view elsewhere reads": (
        [
            "create schema rq525s",
            "create table rq525s.t (a int)",
            "create materialized view rq525mv as select a from rq525s.t",
        ],
        "drop schema rq525s",
        (
            _refused(
                "schema rq525s",
                "table rq525s.t depends on schema rq525s\n"
                "materialized view rq525mv depends on table rq525s.t",
            ),
            [],
            ["rq525mv", "rq525s.t"],
        ),
    ),
}


@pytest.mark.backlog_fixed("RP-525")
@pytest.mark.parametrize("case", list(MATVIEW_CHAINS))
def test_drop_follows_materialized_views_in_a_chain(case: str, dsn: str) -> None:
    setup, drop, expected = MATVIEW_CHAINS[case]
    assert chain_answer(dsn, setup, drop) == expected


# A CASCADE that drops a materialized view inside a block is undone by the
# block's ROLLBACK, or by a ROLLBACK TO a savepoint before it: the view, its
# rows and the view over it come back.
@pytest.mark.backlog_fixed("RP-525")
@pytest.mark.parametrize("undo", ["rollback", "rollback to savepoint"])
def test_cascade_to_a_materialized_view_rolls_back(undo: str, dsn: str) -> None:
    def present(c: psycopg.Connection[Any]) -> Any:
        return c.execute(
            "select to_regclass('rq525t')::text, to_regclass('rq525mv')::text, "
            "to_regclass('rq525v')::text, "
            "(select count(*) from pg_matviews where matviewname = 'rq525mv')"
        ).fetchone()

    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists rq525t cascade")
        for q in [_T, "insert into rq525t values (1), (2)", _MV, _V_MV]:
            c.execute(q)
        try:
            c.execute("begin")
            if undo == "rollback to savepoint":
                c.execute("savepoint s")
            c.execute("drop table rq525t cascade")
            inside = present(c)
            if undo == "rollback":
                c.execute("rollback")
            else:
                c.execute("rollback to s")
                c.execute("commit")
            assert (inside, present(c)) == (
                (None, None, None, 0),
                ("rq525t", "rq525mv", "rq525v", 1),
            )
            assert c.execute("select count(*) from rq525v").fetchone() == (2,)
        finally:
            c.execute("drop table if exists rq525t cascade")


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
