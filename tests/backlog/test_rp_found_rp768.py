"""What working RP-768 (a derived relation's column keeps its type) found.

A column alias list over a FROM subquery whose select list is a join's `*`
was refused, unless the outer query read only the alias list's first
column (RP-773): the unread-output pruning took the `*` for one output. Measured on a
private PostgreSQL 15.19 cluster, since every statement needs a `CREATE
TABLE`.
"""

from __future__ import annotations

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_J = "(select * from ja768 join jb768 on ja768.i = jb768.i) q(p, r, s, t)"

# Each measured on PostgreSQL 15.19.
ALIASED_JOIN_STAR = {
    f"select t from {_J} order by t": [(100,), (200,)],
    f"select r from {_J} order by r": [(10,), (20,)],
    f"select t from {_J} where t > 100": [(200,)],
    # A star that is not the whole select list: the names after it.
    "select s from (select ja768.*, jb768.y from ja768 join jb768"
    " on ja768.i = jb768.i) q(p, r, s) order by s": [(100,), (200,)],
    "select r from (select (ja768).*, 7 from ja768) q(p, r, s) order by r": [(10,), (20,)],
    "select s from (select (ja768).*, 7 from ja768) q(p, r, s) order by r": [(7,), (7,)],
}


@pytest.fixture(scope="module")
def _tables(rust_pg: object) -> object:
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("create table ja768 (i int, x int)")
        c.execute("create table jb768 (i int, y int)")
        c.execute("insert into ja768 values (1, 10), (2, 20)")
        c.execute("insert into jb768 values (1, 100), (2, 200)")
    return rust_pg


@pytest.mark.backlog_fixed("RP-773")
@pytest.mark.parametrize("query", list(ALIASED_JOIN_STAR))
def test_an_alias_list_over_a_joined_star(query: str, _tables: object) -> None:
    """An alias list names each of a joined `*`'s columns on PostgreSQL
    15.19, so any of them can be read. Here every one but the first was
    42601 `table "q" has 1 columns available but 4 columns specified`."""
    with psycopg.connect(_dsn(_tables) + " dbname=postgres", autocommit=True) as c:
        try:
            got: object = [tuple(r) for r in c.execute(query).fetchall()]
        except psycopg.Error as e:
            got = (e.sqlstate, str(e))
        assert got == ALIASED_JOIN_STAR[query]
