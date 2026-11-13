"""Gap tests of the divergences found while working RP-911 (the index
relations derived once per catalog version).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster (2026-11-13).
"""

from __future__ import annotations

from typing import Any

import pytest


@pytest.mark.backlog_fixed("RP-923")
def test_an_index_comment_survives_a_rename(pg: Any) -> None:
    """PostgreSQL keys a comment by the object's oid (`pg_description.objoid`),
    and a rename keeps the oid, so `obj_description` answers the comment
    under the new name: `[('by v',)]` for a `CREATE INDEX`, and `[('pk',)]`
    for a primary key's index. `secantusd-pg` answered NULL for both
    (2026-11-13): it kept index comments under the index's name. They are
    keyed by the index's oid since."""
    pg.execute("DROP TABLE IF EXISTS rp911_ir")
    pg.execute("CREATE TABLE rp911_ir (id int PRIMARY KEY, v int)")
    pg.execute("CREATE INDEX rp911_ir_v ON rp911_ir (v)")
    pg.execute("COMMENT ON INDEX rp911_ir_v IS 'by v'")
    pg.execute("ALTER INDEX rp911_ir_v RENAME TO rp911_ir_v2")
    pg.execute("COMMENT ON INDEX rp911_ir_pkey IS 'pk'")
    pg.execute("ALTER INDEX rp911_ir_pkey RENAME TO rp911_ir_pk2")
    answers = [
        pg.execute(f"SELECT obj_description('{name}'::regclass, 'pg_class')").fetchall()
        for name in ("rp911_ir_v2", "rp911_ir_pk2")
    ]
    assert answers == [[("by v",)], [("pk",)]]


@pytest.mark.backlog_fixed("RP-923")
def test_a_constraint_index_keeps_its_oid_and_comment_through_a_rename(pg: Any) -> None:
    """A UNIQUE constraint's index keeps its oid through `ALTER INDEX ...
    RENAME` and `ALTER TABLE ... RENAME CONSTRAINT`, so its comment
    follows it, as on PostgreSQL 15.19: `[(True, 'u')]` for each.
    `secantusd-pg` drew the renamed constraint a new oid, which lost the
    comment once comments were keyed by oid (2026-11-13)."""
    pg.execute("DROP TABLE IF EXISTS rp923_u")
    pg.execute("CREATE TABLE rp923_u (id int, w int CONSTRAINT rp923_u_w UNIQUE)")
    pg.execute("COMMENT ON INDEX rp923_u_w IS 'u'")
    answers = []
    for old, new, rename in [
        ("rp923_u_w", "rp923_u_w2", "ALTER INDEX rp923_u_w RENAME TO rp923_u_w2"),
        (
            "rp923_u_w2",
            "rp923_u_w3",
            "ALTER TABLE rp923_u RENAME CONSTRAINT rp923_u_w2 TO rp923_u_w3",
        ),
    ]:
        (before,) = pg.execute(f"SELECT '{old}'::regclass::oid").fetchone()
        pg.execute(rename)
        answers.append(
            pg.execute(
                f"SELECT '{new}'::regclass::oid = %s::oid, "
                f"obj_description('{new}'::regclass, 'pg_class')",
                (before,),
            ).fetchall()
        )
    pg.execute("DROP TABLE rp923_u")
    assert answers == [[(True, "u")], [(True, "u")]]
