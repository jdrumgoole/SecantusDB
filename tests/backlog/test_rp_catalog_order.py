"""Gap tests for the order a catalog scan returns relations in
(`tasks/backlog-rust-postgres.md`, "Found while working RP-1073").

PostgreSQL's sequential scan of `pg_class` returns its rows in the heap's
order: a table's row where `CREATE TABLE` put it, and a row `ALTER TABLE`
rewrote after the others. Every view of the relations that does not sort
(`pg_tables`, `information_schema.tables`) reads that scan.
"""

from __future__ import annotations

from pathlib import Path

import pytest

#: The unsorted listings a client reads the relations through.
LISTINGS = (
    "select relname from pg_class where relnamespace = 2200 and relkind = 'r'",
    "select tablename from pg_tables where schemaname = 'public'",
    "select table_name from information_schema.tables where table_schema = 'public'",
)


def _listed(conn: object) -> list[list[str]]:
    return [[r[0] for r in conn.execute(q).fetchall()] for q in LISTINGS]  # type: ignore[attr-defined]


@pytest.mark.backlog_fixed("RP-1113")
def test_relations_list_in_the_order_they_were_written_before_and_after_a_restart(
    tmp_path: Path,
) -> None:
    """PostgreSQL 15.20 on a private cluster (2026-11-26) lists `zz, aa,
    mm, t10, t2` in the order they were created, and after `ALTER TABLE
    aa ADD COLUMN b int`, two tables made in one block (`yy`, `cc`) and
    `DROP TABLE mm`, `zz, t10, t2, bb, aa, yy, cc`: the rewritten row comes
    after the others, and the same before and after a restart. A debug
    `secantusd-pg` listed the tables a session created in the order of
    their names (`aa, mm, t10, t2, zz`) and, after a restart, in the order
    they were created: a full read of the table catalog returns its
    documents in RecordId order, while the definitions each commit patched
    were placed by name. Since RP-1073 a patch places each definition it
    reads again by its RecordId, where the full read finds it."""
    from tests.test_rust_pgserver_slice import _Server

    created = ["zz", "aa", "mm", "t10", "t2"]
    with _Server(tmp_path) as server, server.connect() as conn:
        for name in created:
            conn.execute(f"create table {name} (a int)")
        assert _listed(conn) == [created] * len(LISTINGS)
    with _Server(tmp_path) as server, server.connect() as conn:
        assert _listed(conn) == [created] * len(LISTINGS)
        conn.execute("create table bb (a int)")
        conn.execute("alter table aa add column b int")
        with conn.transaction():
            conn.execute("create table yy (a int)")
            conn.execute("create table cc (a int)")
        conn.execute("drop table mm")
        after = ["zz", "t10", "t2", "bb", "aa", "yy", "cc"]
        assert _listed(conn) == [after] * len(LISTINGS)
    with _Server(tmp_path) as server, server.connect() as conn:
        assert _listed(conn) == [after] * len(LISTINGS)
