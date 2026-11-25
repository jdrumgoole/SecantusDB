"""Gap tests of the divergences found while working RP-1046 (a CASE column
takes its ELSE result's name): two more shapes `FigureColnameInternal`
names that the planner's column naming does not.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.20 cluster; the statements create nothing.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


def _names(rust_pg: Any, sql: str) -> object:
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        try:
            return [d.name for d in c.execute(sql).description or []]
        except psycopg.Error as e:
            return f"{e.sqlstate} {e.diag.message_primary}"


# A SQLValueFunction names its column at strength 2 (`user`,
# `current_date`, ...), so a cast above keeps the keyword's name.
@pytest.mark.backlog("RP-1093")
def test_a_cast_of_a_sql_value_function_keeps_its_name(rust_pg: Any) -> None:
    got = _names(
        rust_pg,
        "select user::text, current_user::text, session_user::text,"
        " current_date::text, current_schema::text, current_catalog::text",
    )
    assert got == [
        "user",
        "current_user",
        "session_user",
        "current_date",
        "current_schema",
        "current_catalog",
    ]


# A CollateClause is named for its argument (FigureColnameInternal recurses
# into `arg`), and a cast above keeps that name.
@pytest.mark.backlog("RP-1093")
def test_a_collate_clause_keeps_its_argument_name(rust_pg: Any) -> None:
    got = _names(
        rust_pg,
        'select c.relname collate "C", (c.relname collate "C")::text,'
        " case when true then 'a' else c.relname end collate \"C\", 'a' collate \"C\""
        " from pg_class c limit 1",
    )
    assert got == ["relname", "relname", "relname", "?column?"]
