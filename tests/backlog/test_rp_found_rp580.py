"""Gap tests of the divergences found while working RP-580 (a cast of a
column to a domain with a modifier applies it): a DROP of a domain another
domain is over drops the base alone, or fails part-way.

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


@pytest.mark.backlog("RP-607")
@pytest.mark.parametrize("query", list(DROPS))
def test_drop_of_a_domain_another_domain_is_over(query: str, dsn: str) -> None:
    assert answer(dsn, query) == DROPS[query]
