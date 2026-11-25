"""Found while working RP-970: a `_`-prefixed type name resolves to the
type of that exact name, as PostgreSQL's `typenameTypeId` does."""

from __future__ import annotations

from typing import Any

import pytest


@pytest.mark.backlog_fixed("RP-1055")
def test_an_underscore_type_name_resolves_to_that_type(pg: Any) -> None:
    """PostgreSQL 15.19 (2026-11-18): after `CREATE TYPE _custom AS ENUM
    ('b')` then `CREATE TYPE custom AS ENUM ('c')`, `'_custom'::regtype`
    is the enum `_custom`, and `custom`'s array is `___custom`.
    `secantusd-pg` answered `custom`'s array until 2026-11-25."""
    pg.execute("CREATE TYPE _custom AS ENUM ('b')")
    pg.execute("CREATE TYPE custom AS ENUM ('c')")
    row = pg.execute(
        "SELECT '_custom'::regtype::oid = (SELECT oid FROM pg_type WHERE typname = '_custom'), "
        "(SELECT typname FROM pg_type WHERE oid = "
        "(SELECT typarray FROM pg_type WHERE typname = 'custom'))"
    ).fetchone()
    assert row == (True, "___custom")


# Each `_`-prefixed spelling, as text, in both creation orders: the
# array names `makeArrayTypeName` gives move with the order.
_SPELLINGS = ("_{s}", "__{s}", "___{s}", "____{s}", "{s}[]", "_{s}[]")

_ORDERS = {
    "underscore-first": ("_{s}", "{s}"),
    "bare-first": ("{s}", "_{s}"),
}


@pytest.mark.backlog_fixed("RP-1055")
@pytest.mark.parametrize("schema", ["public", "rp1055_s"])
@pytest.mark.parametrize("order", list(_ORDERS))
def test_every_underscore_spelling_names_the_type_postgresql_names(
    pg: Any, pg15: Any, order: str, schema: str
) -> None:
    """PostgreSQL 15.19 (2026-11-25), with `CREATE TYPE custom` then
    `CREATE TYPE _custom`: `__custom` is `custom[]` and `___custom` is
    `_custom[]`. In the other order `__custom` is `_custom[]` and
    `___custom` is `custom[]`. `____custom` is no type, and the names
    resolve the same way in a schema on the search path."""
    stem = f"rp1055_{order.replace('-', '_')}_{schema}"
    answers = []
    for conn in (pg, pg15):
        with conn.transaction(force_rollback=True):
            if schema != "public":
                conn.execute(f"CREATE SCHEMA {schema}")
                conn.execute(f"SET LOCAL search_path = {schema}, public")
            for name in _ORDERS[order]:
                conn.execute(f"CREATE TYPE {name.format(s=stem)} AS ENUM ('x')")
            answers.append(
                [
                    conn.execute(
                        "SELECT to_regtype(%s)::text", (spelling.format(s=stem),)
                    ).fetchone()
                    for spelling in _SPELLINGS
                ]
            )
    assert answers[0] == answers[1]
