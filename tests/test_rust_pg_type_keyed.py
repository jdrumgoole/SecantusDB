"""A `pg_type` read pinned to a user type's oid builds that type's row
alone (RP-970). Each keyed read must answer the row the whole catalog
lists: an element type's, an array's (whose name depends on every other
type's), a multirange's, a domain's and a table's row type."""

from __future__ import annotations

from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

from tests.test_rust_pgserver_slice import BINARY, _Server

pytestmark = pytest.mark.skipif(not BINARY.exists(), reason="secantusd-pg not built")

COLUMNS = "oid, typname, typtype, typarray, typelem, typrelid, typnamespace, typbasetype"


@pytest.fixture
def conn(tmp_path: Path) -> Iterator[Any]:
    with _Server(tmp_path) as server, server.connect() as c:
        c.autocommit = True
        yield c


def test_a_keyed_user_type_read_answers_the_row_the_whole_catalog_lists(conn: Any) -> None:
    conn.execute("CREATE SCHEMA tk")
    conn.execute("CREATE TYPE tk.mood AS ENUM ('a')")
    conn.execute("CREATE TYPE _custom AS ENUM ('b')")
    conn.execute("CREATE TYPE custom AS ENUM ('c')")
    conn.execute("CREATE TYPE pair AS (x int, y text)")
    conn.execute("CREATE TYPE fr AS RANGE (subtype = float8)")
    conn.execute("CREATE DOMAIN posint AS int CHECK (VALUE > 0)")
    conn.execute("CREATE TABLE tk_t (a tk.mood, b custom)")
    names = ["mood", "_custom", "custom", "pair", "fr", "fr_multirange", "posint", "tk_t"]
    oids: list[int] = []
    for n in names:
        oid, array = conn.execute(
            "SELECT oid::int8, typarray::int8 FROM pg_type WHERE typname = %s", (n,)
        ).fetchone()
        oids += [oid, array]
    whole = {
        r[0]: r
        for r in conn.execute(
            f"SELECT {COLUMNS} FROM pg_type WHERE oid = ANY(%s::oid[])", (oids,)
        ).fetchall()
    }
    assert len(whole) == len(oids)
    assert whole[oids[5]][1] == "___custom"  # `custom`'s array, renamed twice
    for oid in oids:
        conn.execute("CREATE TABLE tk_bump (x int)")  # a new catalog version
        conn.execute("DROP TABLE tk_bump")
        keyed = conn.execute(f"SELECT {COLUMNS} FROM pg_type WHERE oid = %s", (oid,)).fetchone()
        assert keyed == whole[oid], oid
