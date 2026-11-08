"""Regression tests of RP-607, found while working RP-580 (a cast of a
column to a domain with a modifier applies it): a DROP of a domain another
domain was over dropped the base alone, or failed part-way. Since RP-607
DROP DOMAIN and DROP TYPE walk a type's dependents as PostgreSQL's
`findDependentObjects` does.

An answer is `(outcome, types left)`: the outcome `"ok"` or `(sqlstate,
message, detail, hint)` for an error, and the types of the two domains that
remain after it. The expected answers were measured on a private, freshly
initdb'd PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs
after.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_DROPS = [
    "drop domain if exists d607b",
    "drop domain if exists d607a cascade",
]
_SETUP = [
    *_DROPS,
    "create domain d607a as char(3)",
    "create domain d607b as d607a check (value <> 'abc')",
]
_LEFT = "select typname from pg_type where typname in ('d607a', 'd607b') order by 1"


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _DROPS:
            c.execute(q)


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            c.execute(query)
            outcome: Any = "ok"
        except psycopg.Error as e:
            outcome = (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_detail,
                e.diag.message_hint,
            )
        return outcome, [r[0] for r in c.execute(_LEFT).fetchall()]


# `RemoveObjects` goes through `performMultipleDeletions`, which finds the
# domain over the dropped one in `pg_depend` (`typbasetype`): RESTRICT, the
# default, refuses it, and CASCADE drops both, DROP DOMAIN and DROP TYPE
# alike.
_DEPENDED_ON = (
    "2BP01",
    "cannot drop type d607a because other objects depend on it",
    "type d607b depends on type d607a",
    "Use DROP ... CASCADE to drop the dependent objects too.",
)
DROPS = {
    "drop domain d607a": (_DEPENDED_ON, ["d607a", "d607b"]),
    "drop domain d607a restrict": (_DEPENDED_ON, ["d607a", "d607b"]),
    "drop domain d607a cascade": ("ok", []),
    "drop type d607a": (_DEPENDED_ON, ["d607a", "d607b"]),
    "drop type d607a cascade": ("ok", []),
}


@pytest.mark.backlog_fixed("RP-607")
@pytest.mark.parametrize("query", list(DROPS))
def test_drop_of_a_domain_another_domain_is_over(query: str, dsn: str) -> None:
    assert answer(dsn, query) == DROPS[query]


# The walk a level further, measured on the same private 15.19 cluster:
# dependents are visited depth first, newest first, and reported in reverse
# of the order the walk finished them, so each object comes before what
# depends on it; a DROP of several reports, or cascades to, what depends on
# any of them once.
_WALK_TYPES = (
    "select typname from pg_type where typname in "
    "('d607a', 'd607b', 'd607c', 'e607', 'de1', 'de2') order by 1"
)
_WALK_RESET = [
    "drop table if exists t607, t607b, te607 cascade",
    "drop domain if exists d607c, d607b, d607a, de2, de1 cascade",
    "drop type if exists e607 cascade",
]
_WALK_SETUPS = {
    "chain": [
        "create domain d607a as int",
        "create domain d607b as d607a",
        "create domain d607c as d607b",
    ],
    "columns": [
        "create domain d607a as int",
        "create table t607 (x d607a, y int)",
        "create domain d607b as d607a",
        "create table t607b (z d607b, w int)",
        "insert into t607 values (1, 2)",
        "insert into t607b values (3, 4)",
    ],
    "enum": [
        "create type e607 as enum ('x')",
        "create domain de1 as e607",
        "create domain de2 as de1",
        "create table te607 (a de2, b e607, c e607[])",
    ],
}


def _restrict(dropped: str | None, detail: str) -> tuple[str, str, str, str]:
    """RESTRICT's 2BP01, naming the one type dropped, or (`None`) several."""
    return (
        "2BP01",
        f"cannot drop type {dropped} because other objects depend on it"
        if dropped
        else "cannot drop desired object(s) because other objects depend on them",
        detail,
        "Use DROP ... CASCADE to drop the dependent objects too.",
    )


_COLUMNS_KEPT = {"t607": (["x", "y"], [(1, 2)]), "t607b": (["z", "w"], [(3, 4)])}
WALKS = {
    ("chain", "drop domain d607a"): (
        _restrict("d607a", "type d607b depends on type d607a\ntype d607c depends on type d607b"),
        [],
        ["d607a", "d607b", "d607c"],
        {},
    ),
    ("chain", "drop domain d607a cascade"): (
        "ok",
        [
            (
                "drop cascades to 2 other objects",
                "drop cascades to type d607b\ndrop cascades to type d607c",
            )
        ],
        [],
        {},
    ),
    ("chain", "drop domain d607b cascade"): (
        "ok",
        [("drop cascades to type d607c", None)],
        ["d607a"],
        {},
    ),
    ("chain", "drop domain d607a, d607b"): (
        _restrict(None, "type d607c depends on type d607b"),
        [],
        ["d607a", "d607b", "d607c"],
        {},
    ),
    ("columns", "drop domain d607a"): (
        _restrict(
            "d607a",
            "column x of table t607 depends on type d607a\n"
            "type d607b depends on type d607a\n"
            "column z of table t607b depends on type d607b",
        ),
        [],
        ["d607a", "d607b"],
        _COLUMNS_KEPT,
    ),
    ("columns", "drop type d607a cascade"): (
        "ok",
        [
            (
                "drop cascades to 3 other objects",
                "drop cascades to column x of table t607\n"
                "drop cascades to type d607b\n"
                "drop cascades to column z of table t607b",
            )
        ],
        [],
        {"t607": (["y"], [(2,)]), "t607b": (["w"], [(4,)])},
    ),
    ("columns", "drop domain d607b, d607a"): (
        _restrict(
            None,
            "column x of table t607 depends on type d607a\n"
            "column z of table t607b depends on type d607b",
        ),
        [],
        ["d607a", "d607b"],
        _COLUMNS_KEPT,
    ),
    ("enum", "drop type e607"): (
        _restrict(
            "e607",
            "column c of table te607 depends on type e607[]\n"
            "type de1 depends on type e607\n"
            "type de2 depends on type de1\n"
            "column a of table te607 depends on type de2\n"
            "column b of table te607 depends on type e607",
        ),
        [],
        ["de1", "de2", "e607"],
        {},
    ),
    ("enum", "drop domain de1, e607"): (
        ("42809", '"e607" is not a domain', None, None),
        [],
        ["de1", "de2", "e607"],
        {},
    ),
    ("enum", "drop type de2"): (
        _restrict("de2", "column a of table te607 depends on type de2"),
        [],
        ["de1", "de2", "e607"],
        {},
    ),
    ("enum", "drop type de2 cascade"): (
        "ok",
        [("drop cascades to column a of table te607", None)],
        ["de1", "e607"],
        {},
    ),
}


def walk_answer(dsn: str, setup: str, query: str) -> Any:
    notices: list[tuple[str | None, str | None]] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in [*_WALK_RESET, *_WALK_SETUPS[setup]]:
            c.execute(q)
        c.add_notice_handler(lambda d: notices.append((d.message_primary, d.message_detail)))
        try:
            c.execute(query)
            outcome: Any = "ok"
        except psycopg.Error as e:
            outcome = (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_detail,
                e.diag.message_hint,
            )
        types = [r[0] for r in c.execute(_WALK_TYPES).fetchall()]
        # The columns and rows of the tables over the domains, where the
        # setup made them.
        tables = {}
        if setup == "columns":
            for t in ("t607", "t607b"):
                cur = c.execute(f"select * from {t}")
                tables[t] = ([d.name for d in cur.description or []], cur.fetchall())
        answered = list(notices)
        for q in _WALK_RESET:
            c.execute(q)
        return outcome, answered, types, tables


@pytest.mark.backlog_fixed("RP-607")
@pytest.mark.parametrize(("setup", "query"), list(WALKS))
def test_drop_walks_a_types_dependents(setup: str, query: str, dsn: str) -> None:
    assert walk_answer(dsn, setup, query) == WALKS[(setup, query)]
