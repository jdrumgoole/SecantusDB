"""Batch 63 of the Rust PG server (2026-10-24): a sequential scan a LIMIT
stops counts only what it read (RP-224), and a scan streamed in a
transaction block is counted (RP-238).

Each scenario runs the same statements against a private, freshly
initdb'd PostgreSQL 15.19 cluster and this server; the literals are 15.19's
answers, measured by running this file as a script:

    PYTHONPATH=. python -m tests.backlog.test_rp_batch63 "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it
made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_FLUSH = "select pg_stat_force_next_flush()"
# The pages a table's scans read, since `rp63_c0` noted its count.
_BASE = (
    "create table rp63_c0 as select heap_blks_hit + heap_blks_read n"
    " from pg_statio_user_tables where relname = '{}'"
)
_COUNTS = (
    "select s.seq_scan, s.seq_tup_read, i.heap_blks_hit + i.heap_blks_read - c.n"
    " from pg_stat_user_tables s join pg_statio_user_tables i using (relid), rp63_c0 c"
    " where s.relname = '{}'"
)

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "a LIMIT stops a scan of moved rows at its last row": (
        "RP-224",
        False,
        [
            "create table rp63_m with (autovacuum_enabled = false) as"
            " select g, repeat('x', 100) p from generate_series(1, 2000) g",
            # Five rows move to the end of the heap.
            "update rp63_m set g = g where g <= 5",
            _FLUSH,
            "select 1",
            _BASE.format("rp63_m"),
            _COUNTS.format("rp63_m"),
            "select g from rp63_m limit 3",
            "select g from rp63_m where g = 1500 limit 1",
            "select g from rp63_m where g % 7 = 3 limit 2 offset 1",
            "select g from rp63_m limit 1 offset 1990",
            "select g from rp63_m limit 0",
            "select g from rp63_m where g > 1995 limit 10",
            _FLUSH,
            "select 1",
            _COUNTS.format("rp63_m"),
            "drop table rp63_m, rp63_c0",
        ],
    ),
    # ------------------------------- RP-239 a write's heap buffer accesses
    "a write counts the heap pages PostgreSQL's buffer accesses do": (
        "RP-239",
        True,
        [
            "create table rp63_w with (autovacuum_enabled = false) as"
            " select g, repeat('x', 100) p from generate_series(1, 2000) g",
            _FLUSH,
            "select 1",
            "select heap_blks_hit + heap_blks_read from pg_statio_user_tables"
            " where relname = 'rp63_w'",
            "insert into rp63_w select g, repeat('x', 100) from generate_series(1, 2000) g",
            _FLUSH,
            "select 1",
            "select heap_blks_hit + heap_blks_read from pg_statio_user_tables"
            " where relname = 'rp63_w'",
            "drop table rp63_w",
        ],
    ),
    "a scan streamed in a transaction block is counted": (
        "RP-238",
        False,
        [
            "create table rp63_b as select g from generate_series(1, 100) g",
            _FLUSH,
            "select 1",
            "begin",
            "select g from rp63_b",
            "select g from rp63_b limit 4",
            "commit",
            _FLUSH,
            "select 1",
            "select seq_scan, seq_tup_read from pg_stat_user_tables where relname = 'rp63_b'",
            "drop table rp63_b",
        ],
    ),
}


def _scenario(name: str, server: Any) -> list[Any]:
    return run_scenario(_dsn(server) + " dbname=postgres", SCENARIOS[name][2])


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(name, marks=_marked(item, is_open), id=name)
        for name, (item, is_open, _) in sorted(SCENARIOS.items())
    ],
)
def test_scenario(name: str, rust_pg: Any) -> None:
    assert _scenario(name, rust_pg) == EXPECTED[name]


EXPECTED: dict[str, Any] = {
    "a LIMIT stops a scan of moved rows at its last row": [
        "SELECT 2000",
        "UPDATE 5",
        [("",)],
        [("1",)],
        "SELECT 1",
        [("1", "2000", "0")],
        [("6",), ("7",), ("8",)],
        [("1500",)],
        [("17",), ("24",)],
        [("1996",)],
        [],
        [("1996",), ("1997",), ("1998",), ("1999",), ("2000",)],
        [("",)],
        [("1",)],
        [("6", "7508", "98")],
        "DROP TABLE",
    ],
    "a write counts the heap pages PostgreSQL's buffer accesses do": [
        "SELECT 2000",
        [("",)],
        [("1",)],
        [("35",)],
        "INSERT 0 2000",
        [("",)],
        [("1",)],
        [("2137",)],
        "DROP TABLE",
    ],
    "a scan streamed in a transaction block is counted": [
        "SELECT 100",
        [("",)],
        [("1",)],
        "BEGIN",
        [
            ("1",),
            ("2",),
            ("3",),
            ("4",),
            ("5",),
            ("6",),
            ("7",),
            ("8",),
            ("9",),
            ("10",),
            ("11",),
            ("12",),
            ("13",),
            ("14",),
            ("15",),
            ("16",),
            ("17",),
            ("18",),
            ("19",),
            ("20",),
            ("21",),
            ("22",),
            ("23",),
            ("24",),
            ("25",),
            ("26",),
            ("27",),
            ("28",),
            ("29",),
            ("30",),
            ("31",),
            ("32",),
            ("33",),
            ("34",),
            ("35",),
            ("36",),
            ("37",),
            ("38",),
            ("39",),
            ("40",),
            ("41",),
            ("42",),
            ("43",),
            ("44",),
            ("45",),
            ("46",),
            ("47",),
            ("48",),
            ("49",),
            ("50",),
            ("51",),
            ("52",),
            ("53",),
            ("54",),
            ("55",),
            ("56",),
            ("57",),
            ("58",),
            ("59",),
            ("60",),
            ("61",),
            ("62",),
            ("63",),
            ("64",),
            ("65",),
            ("66",),
            ("67",),
            ("68",),
            ("69",),
            ("70",),
            ("71",),
            ("72",),
            ("73",),
            ("74",),
            ("75",),
            ("76",),
            ("77",),
            ("78",),
            ("79",),
            ("80",),
            ("81",),
            ("82",),
            ("83",),
            ("84",),
            ("85",),
            ("86",),
            ("87",),
            ("88",),
            ("89",),
            ("90",),
            ("91",),
            ("92",),
            ("93",),
            ("94",),
            ("95",),
            ("96",),
            ("97",),
            ("98",),
            ("99",),
            ("100",),
        ],
        [("1",), ("2",), ("3",), ("4",)],
        "COMMIT",
        [("",)],
        [("1",)],
        [("2", "104")],
        "DROP TABLE",
    ],
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_scenario(dsn, stmts) for name, (_, _, stmts) in SCENARIOS.items()})
