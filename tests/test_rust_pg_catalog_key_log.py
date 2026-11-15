"""The committed catalog a statement reads after DDL is patched from the
storage's catalog key log, re-reading only the documents each commit
changed (RP-911). A key the log misses is a stale catalog: every DDL here
runs on one connection and is read back on another, whose cached catalog
predates it."""

from __future__ import annotations

from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

from tests.test_rust_pgserver_slice import _Server

COLUMNS = (
    "SELECT attname FROM pg_attribute WHERE attrelid = %s::regclass "
    "AND attnum > 0 AND NOT attisdropped ORDER BY attnum"
)
TABLES = (
    "SELECT relname FROM pg_class WHERE relname LIKE 'kl\\_%%' AND relkind = 'r' ORDER BY relname"
)


@pytest.fixture
def two(tmp_path: Path) -> Iterator[tuple[Any, Any]]:
    with _Server(tmp_path) as server, server.connect() as a, server.connect() as b:
        for i in range(20):
            a.execute(f"CREATE TABLE kl_pad_{i:02} (x int)")
        b.execute(TABLES).fetchall()
        yield a, b


def _cols(conn: Any, table: str) -> list[str]:
    return [r[0] for r in conn.execute(COLUMNS, (table,)).fetchall()]


def test_another_connection_sees_each_committed_table_change(two: tuple[Any, Any]) -> None:
    a, b = two
    a.execute("CREATE TABLE kl_m (p int)")
    assert _cols(b, "kl_m") == ["p"]
    a.execute("ALTER TABLE kl_m ADD COLUMN q text")
    assert _cols(b, "kl_m") == ["p", "q"]
    a.execute("ALTER TABLE kl_m RENAME TO kl_n")
    names = [r[0] for r in b.execute(TABLES).fetchall()]
    assert "kl_n" in names and "kl_m" not in names
    assert _cols(b, "kl_n") == ["p", "q"]
    a.execute("DROP TABLE kl_n")
    assert "kl_n" not in [r[0] for r in b.execute(TABLES).fetchall()]


def test_a_committed_block_is_seen_and_a_rolled_back_one_is_not(two: tuple[Any, Any]) -> None:
    a, b = two
    with a.transaction():
        a.execute("CREATE TABLE kl_t (p int)")
        a.execute("ALTER TABLE kl_pad_05 ADD COLUMN y int")
    assert _cols(b, "kl_t") == ["p"]
    assert _cols(b, "kl_pad_05") == ["x", "y"]
    a.execute("BEGIN")
    a.execute("CREATE TABLE kl_gone (p int)")
    a.execute("ALTER TABLE kl_pad_06 ADD COLUMN z int")
    a.execute("ROLLBACK")
    assert "kl_gone" not in [r[0] for r in b.execute(TABLES).fetchall()]
    assert _cols(b, "kl_pad_06") == ["x"]


def test_a_composite_types_change_is_seen(two: tuple[Any, Any]) -> None:
    a, b = two
    a.execute("CREATE TYPE kl_pair AS (l int, r int)")
    q = (
        "SELECT attname FROM pg_attribute WHERE attrelid = "
        "(SELECT typrelid FROM pg_type WHERE typname = 'kl_pair') ORDER BY attnum"
    )
    assert [r[0] for r in b.execute(q).fetchall()] == ["l", "r"]
    a.execute("ALTER TYPE kl_pair ADD ATTRIBUTE m text")
    assert [r[0] for r in b.execute(q).fetchall()] == ["l", "r", "m"]
    a.execute("DROP TYPE kl_pair")
    assert b.execute("SELECT count(*) FROM pg_type WHERE typname = 'kl_pair'").fetchone() == (0,)


def test_many_changes_between_reads_are_all_seen(two: tuple[Any, Any]) -> None:
    """More changes than a patch re-reads one by one: the reader falls back
    to reading everything."""
    a, b = two
    for i in range(100):
        a.execute(f"CREATE TABLE kl_many_{i:03} (p int)")
    names = [r[0] for r in b.execute(TABLES).fetchall()]
    assert sum(n.startswith("kl_many_") for n in names) == 100
    for i in range(0, 100, 2):
        a.execute(f"DROP TABLE kl_many_{i:03}")
    names = [r[0] for r in b.execute(TABLES).fetchall()]
    assert sum(n.startswith("kl_many_") for n in names) == 50
