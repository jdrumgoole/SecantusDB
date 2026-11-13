"""Gap tests of the divergences found while working RP-911 (the index
relations derived once per catalog version).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster (2026-11-13).
"""

from __future__ import annotations

from typing import Any

import pytest


@pytest.mark.backlog("RP-923")
def test_an_index_comment_survives_a_rename(pg: Any) -> None:
    """PostgreSQL keys a comment by the object's oid (`pg_description.objoid`),
    and a rename keeps the oid, so `obj_description` answers the comment
    under the new name: `[('by v',)]` for a `CREATE INDEX`, and `[('pk',)]`
    for a primary key's index. `secantusd-pg` answered NULL for both
    (2026-11-13)."""
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
