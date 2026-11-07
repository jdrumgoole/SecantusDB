"""Gap test of a subquery returning more than one column where one is
expected, found while working RP-450.

An answer is `(sqlstate, message, hint, position)`, or the rows of a
statement that succeeds. Each was measured on PostgreSQL 15.19; none needs a
table, so neither side's answer depends on a schema.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


def answer(server: Any, query: str) -> object:
    with psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c:
        try:
            return c.execute(query).fetchall()
        except psycopg.Error as e:
            return (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_hint,
                e.diag.statement_position,
            )


def _one_column(position: str) -> tuple[str, str, None, str]:
    return ("42601", "subquery must return only one column", None, position)


def _too_many(position: str) -> tuple[str, str, None, str]:
    return ("42601", "subquery has too many columns", None, position)


# `transformSubLink`: an EXPR or ARRAY sublink whose target list has more
# than one entry is refused at the sublink, an ANY / IN one at the operator
# when its left side has fewer columns. An EXISTS ignores its targets.
TOO_WIDE = {
    "select (select 1, 2)": _one_column("8"),
    "select (select 1, 2 from (values (1)) v)": _one_column("8"),
    "select (select a, 1) from (values (1)) t(a)": _one_column("8"),
    "select 1 where 1 = (select 1, 2)": _one_column("20"),
    "select (select 1, 2) is null": _one_column("8"),
    "select array(select 1, 2)": _one_column("8"),
    "select 1 in (select 1, 2)": _too_many("10"),
    "select 1 = any (select 1, 2)": _too_many("10"),
    "select exists (select 1, 2)": [(True,)],
}


@pytest.mark.backlog("RP-452")
@pytest.mark.parametrize("query", list(TOO_WIDE))
def test_subquery_with_more_than_one_column(query: str, rust_pg: Any) -> None:
    """Each runs here on the subquery's FIRST column: `1`, `[1]`, `true`."""
    assert answer(rust_pg, query) == TOO_WIDE[query]
