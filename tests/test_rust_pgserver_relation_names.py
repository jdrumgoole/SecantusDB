"""The Rust PG server's relation namespace: a new table's name against the
indexes and composite types already there (RP-928).

`CREATE TABLE` finds a clashing index through the storage's index-name
lookup, kept current from the index catalog's change log, and a clashing
composite by its catalog key. Each answer below is PostgreSQL 15.19's
(private cluster, 2026-11-14), in one session that renames, drops and
rolls back indexes between the checks, so a lookup that went stale fails.
"""

from __future__ import annotations

from pathlib import Path

import psycopg
import pytest

from tests.test_rust_pgserver_slice import BINARY, _Server

pytestmark = pytest.mark.skipif(not BINARY.exists(), reason="secantusd-pg not built")

#: Each statement and PostgreSQL 15.19's answer: "ok" or the SQLSTATE.
STEPS: list[tuple[str, str]] = [
    ("CREATE TABLE t1 (a int)", "ok"),
    ("CREATE INDEX ix1 ON t1 (a)", "ok"),
    ("CREATE TABLE ix1 (a int)", "42P07"),
    ("ALTER INDEX ix1 RENAME TO ix2", "ok"),
    ("CREATE TABLE ix1 (a int)", "ok"),
    ("CREATE TABLE ix2 (a int)", "42P07"),
    ("DROP INDEX ix2", "ok"),
    ("CREATE TABLE ix2 (a int)", "ok"),
    ("BEGIN", "ok"),
    ("CREATE TABLE t3 (a int)", "ok"),
    ("CREATE INDEX ix3 ON t3 (a)", "ok"),
    ("CREATE TABLE ix3 (a int)", "42P07"),
    ("ROLLBACK", "ok"),
    ("BEGIN", "ok"),
    ("CREATE TABLE t4 (a int)", "ok"),
    ("CREATE INDEX ix4 ON t4 (a)", "ok"),
    ("COMMIT", "ok"),
    ("CREATE TABLE ix4 (a int)", "42P07"),
    ("CREATE TABLE ix3 (a int)", "ok"),
    ("CREATE SCHEMA s", "ok"),
    ("CREATE TYPE s.ct AS (x int)", "ok"),
    ("CREATE TABLE ct (a int)", "ok"),
    ("CREATE TYPE ct2 AS (x int)", "ok"),
    ("CREATE TABLE ct2 (a int)", "42P07"),
    ("CREATE TABLE s.t5 (a int)", "ok"),
    ("CREATE INDEX ix5 ON s.t5 (a)", "ok"),
    ("CREATE TABLE ix5 (a int)", "ok"),
    ("CREATE TABLE s.ix5 (a int)", "42P07"),
    ("DROP TABLE t1", "ok"),
    ("CREATE TABLE t1_a_idx (a int)", "ok"),
]


def test_create_table_finds_a_clashing_index_or_composite_by_name(tmp_path: Path) -> None:
    """Each statement answers as PostgreSQL 15.19 does. Before RP-928 an
    index in schema `s` blocked a table of its name in `public` and not
    one in `s`, and a composite was matched by its bare name in any
    schema."""
    got = []
    with _Server(tmp_path) as server, server.connect() as conn:
        conn.autocommit = True
        for statement, _ in STEPS:
            try:
                conn.execute(statement)
                got.append((statement, "ok"))
            except psycopg.Error as e:
                got.append((statement, e.sqlstate or "?"))
    assert got == STEPS


#: Each statement and PostgreSQL 15.19's rows (`None` for no result set),
#: private cluster, 2026-11-15.
INDEX_STEPS: list[tuple[str, list[tuple[object, ...]] | None]] = [
    ("CREATE TABLE r1 (a int)", None),
    ("CREATE TABLE r2 (a int)", None),
    ("SELECT indexrelid::regclass::text FROM pg_index WHERE indrelid = 'r1'::regclass", []),
    ("CREATE INDEX r1_ix ON r1 (a)", None),
    (
        "SELECT indexrelid::regclass::text FROM pg_index WHERE indrelid = 'r1'::regclass",
        [("r1_ix",)],
    ),
    ("SELECT to_regclass('r1_ix') IS NOT NULL", [(True,)]),
    ("BEGIN", None),
    ("CREATE INDEX r2_ix ON r2 (a)", None),
    ("SELECT to_regclass('r2_ix') IS NOT NULL", [(True,)]),
    ("SELECT indexname FROM pg_indexes WHERE tablename = 'r2'", [("r2_ix",)]),
    ("ROLLBACK", None),
    ("SELECT to_regclass('r2_ix') IS NOT NULL", [(False,)]),
    ("SELECT count(*) FROM pg_indexes WHERE tablename = 'r2'", [(0,)]),
    ("DROP INDEX r1_ix", None),
    ("SELECT to_regclass('r1_ix') IS NOT NULL", [(False,)]),
    ("SELECT count(*) FROM pg_index WHERE indrelid = 'r1'::regclass", [(0,)]),
]


def test_a_tables_index_relations_follow_its_index_ddl(tmp_path: Path) -> None:
    """Each answer is PostgreSQL 15.19's. Since RP-911 a table's index
    relations are derived only for a table the storage's index-name map
    says holds an index, and the relations a `regclass` resolves against
    are listed at a statement's first lookup: an index made, made and
    rolled back in a block, or dropped must still appear and go as the
    catalog says."""
    got = []
    with _Server(tmp_path) as server, server.connect() as conn:
        conn.autocommit = True
        for statement, _ in INDEX_STEPS:
            cur = conn.execute(statement)
            got.append((statement, cur.fetchall() if cur.description else None))
    assert got == INDEX_STEPS
