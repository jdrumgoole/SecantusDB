"""Gap tests for what PostgreSQL refuses at parse analysis, before planning.

Each answer was measured on a private PostgreSQL 15.19 cluster with this
module's tables (found while working RP-419).
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.conftest import pg_answer
from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "create table e (a int, b text)",
    "insert into e select g, 'v' || (g % 10) from generate_series(1, 10) g",
    "create table k (a int primary key, b text)",
]


def _answers(server: Any, queries: list[str]) -> list[tuple]:
    """Each query's answer, in autocommit over a fresh database."""
    with psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp427_analysis")
        c.execute("create database rp427_analysis")
    with psycopg.connect(_dsn(server) + " dbname=rp427_analysis", autocommit=True) as c:
        for s in _SETUP:
            c.execute(s)
        return [pg_answer(c, q) for q in queries]


@pytest.mark.backlog_fixed("RP-427")
def test_insert_select_and_cte_are_type_checked(rust_pg: Any) -> None:
    """An operator with no overload for its operands' types was an empty
    answer inside an INSERT's SELECT and a WITH query; it is 42883 there
    too, as `transformWithClause` analyses every CTE's query, used or not,
    and an UPDATE or DELETE's WITH as a SELECT's."""
    integer_boolean = ("err", "42883", "operator does not exist: integer = boolean")
    text_integer = ("err", "42883", "operator does not exist: text = integer")
    assert _answers(
        rust_pg,
        [
            "insert into k select * from e where a = true",
            "with t as (select * from e where a = true) select * from t",
            "explain with t as (select * from e where a = true) select * from t",
            "with t as (select * from e where b = 1) select 1",
            "with t as (select b as a from e), u as (select * from t where a = 1) select 1",
            "insert into k with t as (select a, b from e) select * from t where a = true",
            "with t as (select * from e where a = true) update k set b = 'x'",
            "with t as (select * from e where a = true) delete from k",
            "with x as (update k set b = 'q' where a = true returning *) select * from x",
            "with t as (select a, b from e) insert into k select * from t where a > 8",
            "with recursive r(n) as (select 1 union all select n + 1 from r where n < 3)"
            " select * from r",
        ],
    ) == [
        integer_boolean,
        integer_boolean,
        integer_boolean,
        text_integer,
        text_integer,
        integer_boolean,
        integer_boolean,
        integer_boolean,
        integer_boolean,
        ("ok", "INSERT 0 2"),
        ("ok", [(1,), (2,), (3,)]),
    ]


@pytest.mark.backlog("RP-431")
def test_star_derived_table_is_typed(rust_pg: Any) -> None:
    """A FROM subquery or CTE written `select *` has its relation's column
    types, so a comparison over them is 42883 at parse analysis."""
    integer_boolean = ("err", "42883", "operator does not exist: integer = boolean")
    assert _answers(
        rust_pg,
        [
            "select * from (select * from e) t where a = true",
            "with t as (select * from e) select * from t where a = true",
            "with t as (select * from e) insert into k select * from t where a = true",
        ],
    ) == [integer_boolean, integer_boolean, integer_boolean]


@pytest.mark.backlog_fixed("RP-432")
def test_update_and_delete_read_their_ctes(rust_pg: Any) -> None:
    """An UPDATE or DELETE read the CTEs of its own WITH neither in FROM /
    USING nor in a subquery, and a data-modifying CTE could not read the ones
    before it: each was 42P01 `relation "t" does not exist`."""
    assert _answers(
        rust_pg,
        [
            "insert into k values (1, 'v1'), (2, 'x'), (50, 'w')",
            "with t as (select b from e) update k set a = a + 100 from t where k.b = t.b",
            "with t as (select a from e) update k set b = 'y' where a in (select a from t)",
            "with t as (select 50 as a) delete from k using t where k.a = t.a",
            "with t as (select 2 as a), u as (delete from k where a in (select a from t)"
            " returning a) select * from u",
            "select a, b from k order by a",
        ],
    ) == [
        ("ok", "INSERT 0 3"),
        ("ok", "UPDATE 1"),
        ("ok", "UPDATE 1"),
        ("ok", "DELETE 1"),
        ("ok", [(2,)]),
        ("ok", [(101, "v1")]),
    ]


@pytest.mark.backlog_fixed("RP-432")
def test_writes_read_their_ctes_everywhere(rust_pg: Any) -> None:
    """A write's WITH is in scope for the whole write, as PostgreSQL 15.19's
    `transformWithClause` puts it: a recursive one, an INSERT's VALUES, a
    SET value, RETURNING, a CTE shadowing a table, a CTE body with its own
    subquery, and a data-modifying item read in FROM. A CTE in FROM takes
    part in the unqualified-name check (42702). Each was 42P01."""
    assert _answers(
        rust_pg,
        [
            "insert into k values (1, 'v1'), (2, 'x'), (3, 'v3'), (7, 'z'), (8, 'q'), (50, 'w')",
            "with e as (select 3 as a) update k set b = 'shadow' where a in (select a from e)",
            "with t as (select 2 as a) update k set b = (select 'from-t' || a from t)"
            " where a = 2 returning a, b, (select a from t) as ta",
            "with t as (select a from e where a in (select 1)) delete from k using t"
            " where k.a = t.a returning k.a, t.a",
            "with t as (select 50 as a) update k set b = 'n' from (select * from t) s"
            " where k.a = s.a",
            "with d as (delete from k where a = 7 returning a) update k set b = 'd'"
            " from d where k.a = d.a + 1",
            "with recursive r(n) as (select 2 union all select n + 1 from r where n < 3)"
            " delete from k where a in (select n from r)",
            "with recursive r(n) as (select 10 union all select n + 1 from r where n < 12)"
            " insert into k select n, 'r' from r",
            "with t as (select 20 as a) insert into k values ((select a from t), 'z')",
            "with t as (select 21 as a), u as (insert into k select a, 'q' from t"
            " returning a) select * from u",
            "with t as (select 22 as a), u as (insert into k values ((select a from t), 'q')"
            " returning a) select * from u",
            "with t as (select 10 as a) update k set b = 'pub' from public.e t where k.a = t.a",
            "with t as (select 11 as a) delete from k where exists"
            " (select 1 from t where t.a = k.a)",
            "with t(x) as (select 12) delete from k using t where k.a = t.x returning t.x",
            "with t as (select b from e) delete from k using t where b = 'w'",
            "select a, b from k order by a",
        ],
    ) == [
        ("ok", "INSERT 0 6"),
        ("ok", "UPDATE 1"),
        ("ok", [(2, "from-t2", 2)]),
        ("ok", [(1, 1)]),
        ("ok", "UPDATE 1"),
        ("ok", "UPDATE 1"),
        ("ok", "DELETE 2"),
        ("ok", "INSERT 0 3"),
        ("ok", "INSERT 0 1"),
        ("ok", [(21,)]),
        ("ok", [(22,)]),
        ("ok", "UPDATE 2"),
        ("ok", "DELETE 1"),
        ("ok", [(12,)]),
        ("err", "42702", 'column reference "b" is ambiguous'),
        ("ok", [(8, "pub"), (10, "pub"), (20, "z"), (21, "q"), (22, "q"), (50, "n")]),
    ]


@pytest.mark.backlog("RP-428")
def test_explain_resolves_names_before_options(rust_pg: Any) -> None:
    """`transformExplainStmt` resolves the statement's relations, functions
    and literals before `ExplainQuery` reads an option."""
    assert _answers(
        rust_pg,
        [
            "explain (bogus) select * from nosuch",
            "explain (bogus) select nosuchfn(a) from e",
            "explain (bogus) select * from e where a = 'x'",
            "explain (bogus) select 1/0",
        ],
    ) == [
        ("err", "42P01", 'relation "nosuch" does not exist'),
        ("err", "42883", "function nosuchfn(integer) does not exist"),
        ("err", "22P02", 'invalid input syntax for type integer: "x"'),
        ("err", "42601", 'unrecognized EXPLAIN option "bogus"'),
    ]
