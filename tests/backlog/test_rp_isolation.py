"""PostgreSQL's isolation tests, replayed against `secantusd-pg`.

Each `isolation/<name>.spec` is a spec from PostgreSQL 15's
`src/test/isolation/specs` (or one written in its format, named in its
header), and `isolation/<name>.json` is every permutation's trace as
PostgreSQL 15.19 ran it on a private cluster, 2026-10-25, recorded with
`tools/probes/pg_isolation.py --record`: each step's rows, command tag or
error, and whether it was still running after the wait (blocked). The test
runs each permutation against this module's `secantusd-pg` and compares.

The SSI specs (RP-061) are the serializable ones PostgreSQL 15's own suite
runs; the specs listed against an open item still diverge.
`insert-conflict-serializable` is not here: it is not in 15.19's suite (it
came later), and 15.19 itself commits the write skew it tests for.
`multiple-row-versions` is vendored but not run. Its setup inserts a million
rows and then adds a primary key in the same transaction, which re-keys
every stored row. That is past the transaction size WiredTiger's cache holds
(RP-256's scope decision), so what is lost is its one SSI permutation, a
pivot over four transactions that `total-cash` and `two-ids` also reach.
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path
from typing import Any

import pytest

HERE = Path(__file__).parent / "isolation"
_RUNNER = Path(__file__).parents[2] / "tools" / "probes" / "pg_isolation.py"
_spec = importlib.util.spec_from_file_location("pg_isolation", _RUNNER)
assert _spec and _spec.loader
iso = importlib.util.module_from_spec(_spec)
# Registered first: its dataclasses look their module up.
sys.modules["pg_isolation"] = iso
_spec.loader.exec_module(iso)

# Seconds a step may run before it counts as blocked.
WAIT = 2.0


def _check(name: str, rust_pg: Any) -> None:
    spec = iso.parse_spec((HERE / f"{name}.spec").read_text())
    expected = json.loads((HERE / f"{name}.json").read_text())
    dsn = f"host=127.0.0.1 port={rust_pg.port} dbname=postgres user=postgres"
    differ = []
    for perm in spec.permutations:
        key = " ".join(perm)
        got = json.loads(json.dumps(iso.run_permutation(spec, perm, dsn, WAIT)))
        if got != expected[key]:
            differ.append((key, got, expected[key]))
    assert not differ, (
        f"{len(differ)} of {len(spec.permutations)} permutations differ: {differ[:2]}"
    )


SSI_SPECS = [
    "classroom-scheduling",
    "index-only-scan",
    "predicate-lock-hot-tuple",
    "project-manager",
    "read-only-anomaly",
    "read-only-anomaly-2",
    "read-only-anomaly-3",
    "receipt-report",
    "referential-integrity",
    "ri-trigger",
    "serializable-parallel",
    "serializable-parallel-2",
    "serializable-parallel-3",
    "simple-write-skew",
    "temporal-range-integrity",
    "total-cash",
    "two-ids",
    "update-conflict-out",
]


@pytest.mark.backlog_fixed("RP-061")
@pytest.mark.parametrize("name", SSI_SPECS)
def test_ssi_spec(name: str, rust_pg: Any) -> None:
    """PostgreSQL 15.19's serializable snapshot isolation, permutation by
    permutation: which transaction fails with 40001, at which step, and
    with which reason code."""
    _check(name, rust_pg)


@pytest.mark.backlog_fixed("RP-253")
def test_ssi_partial_index(rust_pg: Any) -> None:
    """PostgreSQL 15.19 reads `val2 = 1` through a partial index's bitmap
    scan and `id = 10` through its index scan, so SSI locks those index
    pages, not the whole table: a transaction is doomed as it is there,
    at its read rather than at the other's commit."""
    _check("partial-index", rust_pg)


@pytest.mark.backlog("RP-254")
@pytest.mark.parametrize("name", ["predicate-gin", "predicate-gist"])
def test_ssi_index_access_methods(name: str, rust_pg: Any) -> None:
    """PostgreSQL 15.19's GIN and GiST indexes take SIREAD locks on the
    index pages a scan reads, so two transactions writing far apart in the
    index both commit."""
    _check(name, rust_pg)


@pytest.mark.backlog_fixed("RP-254")
def test_ssi_hash_index(rust_pg: Any) -> None:
    """PostgreSQL 15.19's hash index locks the primary page of each bucket
    a scan reads and checks it on insert, so transactions on keys in other
    buckets both commit (`predicate-hash`)."""
    _check("predicate-hash", rust_pg)


@pytest.mark.backlog_fixed("RP-255")
@pytest.mark.parametrize(
    "name",
    [
        "read-write-unique",
        "read-write-unique-2",
        "read-write-unique-3",
        "read-write-unique-4",
        "unique-insert-wait",
        "unique-column-insert-wait",
    ],
)
def test_unique_insert_waits(name: str, rust_pg: Any) -> None:
    """PostgreSQL 15.19 makes an insert of a key another transaction has
    inserted and not committed wait -- a primary key's or a UNIQUE column's,
    and an INSERT inside a function a constant SELECT calls too. When that
    transaction rolls back, the insert goes on. When it commits, the insert
    fails 23505 under READ COMMITTED and REPEATABLE READ, and with SSI's
    reason code under SERIALIZABLE, not 40001 "concurrent update"."""
    _check(name, rust_pg)


@pytest.mark.backlog_fixed("RP-256")
def test_large_transaction(tmp_path: Path) -> None:
    """PostgreSQL 15.19 inserts a million rows in one transaction, as
    `multiple-row-versions`'s setup does; this server, a cache-sized share
    of them (RP-256's scope decision). Measured on a 1G cache, a quarter of
    the 4G the server runs with, so the test's own server stays near 1 GB
    (the 4G one peaked at 4.9 GB, too much beside a CI runner's other
    workers): 250,000 rows commit with the raised eviction triggers, and
    with WiredTiger's defaults (20% dirty, 10% updates) 200,000 already
    roll back."""
    from tests.test_rust_pgserver_slice import _Server

    with (
        _Server(tmp_path, env={"SECANTUS_WT_CONFIG_EXTRA": "cache_size=1G"}) as server,
        server.connect() as conn,
    ):
        got = [
            conn.execute(sql).statusmessage
            for sql in (
                "BEGIN",
                "CREATE TABLE big (id int NOT NULL, txt text) WITH (fillfactor=50)",
                "INSERT INTO big (id) SELECT x FROM generate_series(1, 250000) x",
                "COMMIT",
            )
        ]
    assert got == ["BEGIN", "CREATE TABLE", "INSERT 0 250000", "COMMIT"]


@pytest.mark.backlog_fixed("RP-257")
def test_on_conflict_where_locks_the_row(rust_pg: Any) -> None:
    """PostgreSQL 15.19 locks the conflicting row of `ON CONFLICT DO UPDATE
    ... WHERE` before the WHERE rejects it: a concurrent UPDATE waits for
    the inserter's transaction."""
    _check("on-conflict-where-lock", rust_pg)


@pytest.mark.backlog_fixed("RP-260")
def test_snapshot_at_first_query(rust_pg: Any) -> None:
    """PostgreSQL 15.19 takes a REPEATABLE READ transaction's snapshot at
    its first query, not at SET, SHOW or LOCK."""
    _check("snapshot-at-first-query", rust_pg)
