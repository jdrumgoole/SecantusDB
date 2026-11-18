"""Found while working RP-970: a `_`-prefixed type name resolves to the
type of that exact name, as PostgreSQL's `typenameTypeId` does."""

from __future__ import annotations

from typing import Any

import pytest


@pytest.mark.backlog("RP-1055")
def test_an_underscore_type_name_resolves_to_that_type(pg: Any) -> None:
    """PostgreSQL 15.19 (2026-11-18): after `CREATE TYPE _custom AS ENUM
    ('b')` then `CREATE TYPE custom AS ENUM ('c')`, `'_custom'::regtype`
    is the enum `_custom`, and `custom`'s array is `___custom`.
    `secantusd-pg` answered `custom`'s array."""
    pg.execute("CREATE TYPE _custom AS ENUM ('b')")
    pg.execute("CREATE TYPE custom AS ENUM ('c')")
    row = pg.execute(
        "SELECT '_custom'::regtype::oid = (SELECT oid FROM pg_type WHERE typname = '_custom'), "
        "(SELECT typname FROM pg_type WHERE oid = "
        "(SELECT typarray FROM pg_type WHERE typname = 'custom'))"
    ).fetchone()
    assert row == (True, "___custom")
