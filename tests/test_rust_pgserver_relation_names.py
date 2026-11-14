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
