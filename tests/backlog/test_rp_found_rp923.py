"""Gap tests of the divergences found while working RP-923 (an index
comment keyed by the index's oid).

The expected answers were measured on the PostgreSQL 15.19 reference
(2026-11-13).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import pg_answer


@pytest.mark.backlog("RP-933")
def test_an_index_outside_public_can_be_renamed(pg: Any) -> None:
    """`ALTER INDEX s.ix RENAME TO ix2` renames an index of a table in
    schema `s`, and so does the bare name when `s` is on the search path:
    PostgreSQL answers `ALTER INDEX` for each and lists the new name.
    `secantusd-pg` answered 42P01 `relation "s.ix" does not exist` for
    both forms (2026-11-13)."""
    pg.execute("DROP SCHEMA IF EXISTS rp923_s CASCADE")
    pg.execute("CREATE SCHEMA rp923_s")
    pg.execute("CREATE TABLE rp923_s.t (id int)")
    pg.execute("CREATE INDEX t_i ON rp923_s.t (id)")
    listed = (
        "SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace "
        "WHERE n.nspname = 'rp923_s' AND c.relkind = 'i'"
    )
    answers = [pg_answer(pg, "ALTER INDEX rp923_s.t_i RENAME TO t_i2"), pg_answer(pg, listed)]
    pg.execute("SET search_path = rp923_s, public")
    answers.append(pg_answer(pg, "ALTER INDEX t_i2 RENAME TO t_i3"))
    pg.execute("RESET search_path")
    answers.append(pg_answer(pg, listed))
    pg.execute("DROP SCHEMA rp923_s CASCADE")
    assert answers == [
        ("ok", "ALTER INDEX"),
        ("ok", [("t_i2",)]),
        ("ok", "ALTER INDEX"),
        ("ok", [("t_i3",)]),
    ]
