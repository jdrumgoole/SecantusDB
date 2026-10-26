"""Divergences found by the top-ten batch (2026-10-26), each measured on
PostgreSQL 15.19 with the same statements in a rolled-back transaction.

RP-280 .. RP-283 were found while transcribing PostgreSQL's parse analysis
for the rule trees (RP-203); RP-287 is what the GIN and GiST page models
(RP-254) do not cover yet.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import pg_answer


def _in_block(pg: Any, setup: list[str], query: str) -> tuple:
    pg.execute("begin")
    try:
        for s in setup:
            got = pg_answer(pg, s)
            if got[0] == "err":
                return got
        return pg_answer(pg, query)
    finally:
        pg.execute("rollback")


@pytest.mark.backlog("RP-280")
def test_a_column_functionally_dependent_on_the_grouped_key(pg: Any) -> None:
    """GROUP BY a table's primary key lets its other columns be selected
    (`check_functional_grouping`): PostgreSQL answers the rows."""
    got = _in_block(
        pg,
        [
            "create table rp280_t (id int primary key, name text)",
            "insert into rp280_t values (1, 'a'), (2, 'b')",
        ],
        "select id, name from rp280_t group by id order by id",
    )
    assert got == ("ok", [(1, "a"), (2, "b")])


@pytest.mark.backlog("RP-281")
def test_a_deferrable_primary_key_is_deferrable(pg: Any) -> None:
    """`PRIMARY KEY (a, b) DEFERRABLE` records `condeferrable`."""
    got = _in_block(
        pg,
        ["create table rp281_t (a int, b int, primary key (a, b) deferrable)"],
        "select condeferrable, condeferred from pg_constraint"
        " where conrelid = 'rp281_t'::regclass and contype = 'p'",
    )
    assert got == ("ok", [(True, False)])


@pytest.mark.backlog("RP-282")
@pytest.mark.parametrize(
    "setup, statement, error",
    [
        (
            ["create table rp282_t (a int, b text)", "create table rp282_l (a int, b text)"],
            "create rule rp282_r as on insert to rp282_t do also insert into rp282_l"
            " values (new.a, new.b) returning *",
            ("0A000", "RETURNING lists are not supported in non-INSTEAD rules"),
        ),
        (
            ["create table rp282_t (a int, b text)", "create table rp282_d (a date)"],
            "create rule rp282_r2 as on delete to rp282_t do also insert into rp282_d select old.*",
            ("42601", "INSERT has more expressions than target columns"),
        ),
        (
            ["create table rp282_g (id int, parent int)"],
            "create view rp282_v as with recursive w(id) as (select id from rp282_g"
            " union all select g.id from rp282_g g join w on g.parent = w.id)"
            " search depth first by id set ord select * from w",
            ("42P16", 'column "ord" has pseudo-type record[]'),
        ),
    ],
    ids=[
        "returning in a non-instead rule",
        "old.* into a narrower table",
        "search column in a view",
    ],
)
def test_rules_and_views_postgres_refuses(
    pg: Any, setup: list[str], statement: str, error: tuple[str, str]
) -> None:
    """Parse analysis refuses each; this server stored the rule or view."""
    got = _in_block(pg, setup, statement)
    assert got[:2] == ("err", error[0])
    assert got[2].endswith(error[1])


@pytest.mark.backlog("RP-283")
def test_a_hypothetical_set_aggregate_over_two_columns(pg: Any) -> None:
    """`dense_rank(2, 'x') WITHIN GROUP (ORDER BY a, b)` ranks a row of two
    values; over no rows it is 1."""
    got = _in_block(
        pg,
        ["create table rp283_t (a int, b text)"],
        "select dense_rank(2, 'x') within group (order by a, b) from rp283_t",
    )
    assert got == ("ok", [(1,)])


@pytest.mark.backlog("RP-287")
def test_gin_and_gist_indexes_have_their_pages(pg: Any) -> None:
    """PostgreSQL 15.19 sizes these two indexes at 4 and 18 pages; the page
    models know their pages, but `pg_relation_size` reads 0."""
    got = _in_block(
        pg,
        [
            "create table rp287_t (a int[])",
            "insert into rp287_t select array[g % 100, g % 7] from generate_series(1, 2000) g",
            "create index rp287_g on rp287_t using gin (a)",
            "create table rp287_p (p point)",
            "insert into rp287_p select point(g % 50, g / 50) from generate_series(1, 2000) g",
            "create index rp287_s on rp287_p using gist (p)",
        ],
        "select pg_relation_size('rp287_g') / 8192, pg_relation_size('rp287_s') / 8192",
    )
    assert got == ("ok", [(4, 18)])


_RP019_SETUP = [
    "create table rp19_a (a int)",
    "create table rp19_b (b int)",
    "insert into rp19_a values (1), (2), (3)",
    "insert into rp19_b values (1), (2), (3)",
    "create sequence rp19_s",
]


@pytest.mark.backlog_fixed("RP-019")
def test_a_volatile_condition_runs_once_a_row_of_its_table(pg: Any) -> None:
    """PostgreSQL 15.19 runs a base relation's quals in its scan: a
    volatile call over one table runs once for each of its rows, in a join
    too, and is never folded while planning. Here it ran once (folded), or
    once per joined row; the counts are the sequence's after each query."""
    queries = [
        "select count(*) from rp19_a where rp19_a.a > nextval('rp19_s') * 0",
        "select last_value from rp19_s",
        "select count(*) from rp19_a, rp19_b where rp19_a.a > nextval('rp19_s') * 0",
        "select last_value from rp19_s",
        "select count(*) from rp19_a join rp19_b on rp19_a.a = rp19_b.b"
        " where rp19_b.b + nextval('rp19_s') * 0 > 1",
        "select last_value from rp19_s",
    ]
    pg.execute("begin")
    try:
        for s in _RP019_SETUP:
            pg.execute(s)
        got = [pg_answer(pg, q) for q in queries]
    finally:
        pg.execute("rollback")
    assert got == [
        ("ok", [(3,)]),
        ("ok", [(3,)]),
        ("ok", [(9,)]),
        ("ok", [(6,)]),
        ("ok", [(2,)]),
        ("ok", [(9,)]),
    ]


@pytest.mark.backlog("RP-288")
def test_a_select_list_nextval_in_an_autocommit_join_runs_once_a_row(rust_pg: Any) -> None:
    """In autocommit, a join whose select list calls `nextval` took one
    value more than its rows (in a transaction block it did not)."""
    with rust_pg.connect() as conn:
        for s in ["drop table if exists rp19_a, rp19_b", "drop sequence if exists rp19_s"]:
            conn.execute(s)
        for s in _RP019_SETUP:
            conn.execute(s)
        rows = conn.execute(
            "select rp19_a.a, nextval('rp19_s') from rp19_a, rp19_b"
            " where rp19_a.a = rp19_b.b order by 1"
        ).fetchall()
        last = conn.execute("select last_value from rp19_s").fetchone()
        conn.execute("drop table rp19_a, rp19_b")
        conn.execute("drop sequence rp19_s")
    assert (rows, last) == ([(1, 1), (2, 2), (3, 3)], (3,))
