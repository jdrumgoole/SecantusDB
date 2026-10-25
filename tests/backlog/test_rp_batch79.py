"""Batch 79 of the Rust PG server (2026-10-25): autovacuum (RP-226).

PostgreSQL 15.19's launcher wakes every `autovacuum_naptime` and its worker
vacuums and analyzes a table whose changes pass the thresholds, with no
statement asking. Measured on a private 15.19 cluster with the naptime at 1s:
2,000 rows inserted into a fresh table are autovacuumed and auto-analyzed
within two seconds, the counters reading as `EXPECTED` says. The test polls
for up to 30 seconds, a bound the launcher's 1-second wakes keep well
within. It runs on a server of its own, since it changes a server setting.
"""

from __future__ import annotations

import time
from pathlib import Path

import pytest

# (autoanalyze_count, n_mod_since_analyze, autovacuum_count,
# n_ins_since_vacuum, analyze_count, vacuum_count), PostgreSQL 15.19.
EXPECTED = (1, 0, 1, 0, 0, 0)

_STATS = (
    "select autoanalyze_count, n_mod_since_analyze, autovacuum_count, "
    "n_ins_since_vacuum, analyze_count, vacuum_count, "
    "last_autoanalyze is not null and last_autovacuum is not null "
    "from pg_stat_user_tables where relname = 'rp79'"
)


@pytest.mark.backlog_fixed("RP-226")
def test_autovacuum_runs_on_its_own(tmp_path: Path) -> None:
    """PostgreSQL 15.19: with `autovacuum_naptime` at 1s, a table's 2,000
    inserts are vacuumed and analyzed by autovacuum, under its own counters,
    and the manual ones stay at zero."""
    from tests.test_rust_pgserver_slice import _Server

    with _Server(tmp_path) as server, server.connect() as conn:
        conn.execute("alter system set autovacuum_naptime = '1s'")
        conn.execute("select pg_reload_conf()")
        conn.execute("create table rp79 (a int)")
        conn.execute("insert into rp79 select g from generate_series(1, 2000) g")
        got = None
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            row = conn.execute(_STATS).fetchone()
            if row and row[6]:
                got = tuple(row[:6])
                break
            time.sleep(0.25)
        conn.execute("alter system reset autovacuum_naptime")
        conn.execute("select pg_reload_conf()")
    assert got == EXPECTED
