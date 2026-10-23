"""Batch 47 of the Rust PG server (2026-10-23): the regression tests of
RP-192 (a sequential scan reads rows in ctid order; its first test is
`test_rp_batch46.py::test_scenario[a scan reads rows in ctid order]`), and
a gap test of RP-193 found with them: a bitmap heap scan over a HOT-updated
row reads it at its chain's root.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch47.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------------------- RP-192 a seq scan is in ctid order
    "an unordered select reads the heap in ctid order": (
        "RP-192",
        False,
        [
            "create table rp47_a (a int, t text)",
            "insert into rp47_a values (1, 'x'), (2, 'y'), (3, 'z')",
            "update rp47_a set t = 'yy' where a = 2",
            "select a from rp47_a",
            "select a from rp47_a where t <> 'q'",
            "select a from rp47_a limit 2",
            "select a from rp47_a offset 1",
            "delete from rp47_a where a = 3",
            "insert into rp47_a values (4, 'w')",
            "select a from rp47_a",
            "drop table rp47_a",
        ],
    ),
    "a table with a primary key reads in ctid order too": (
        "RP-192",
        False,
        [
            "create table rp47_p (id int primary key, v text)",
            "insert into rp47_p values (3, 'c'), (1, 'a'), (2, 'b')",
            "update rp47_p set v = 'aa' where id = 1",
            "select id from rp47_p",
            "select id from rp47_p where v <> 'q'",
            "drop table rp47_p",
        ],
    ),
    "a hash join probes its outer side in ctid order": (
        "RP-192",
        False,
        [
            "create table rp47_l (a int, t text)",
            "insert into rp47_l values (1, 'x'), (2, 'y'), (3, 'z')",
            "update rp47_l set t = 'yy' where a = 2",
            "create table rp47_r (id int primary key, v text)",
            "insert into rp47_r values (3, 'c'), (1, 'a'), (2, 'b')",
            "select a, id from rp47_l join rp47_r on rp47_l.a = rp47_r.id",
            "drop table rp47_l, rp47_r",
        ],
    ),
    "a transaction's own updates are read in ctid order": (
        "RP-192",
        False,
        [
            "create table rp47_t (a int, t text)",
            "begin",
            "insert into rp47_t values (1, 'x'), (2, 'y'), (3, 'z')",
            "update rp47_t set t = 'xx' where a = 1",
            "select a from rp47_t",
            "select string_agg(a::text, ',') from rp47_t",
            "commit",
            "drop table rp47_t",
        ],
    ),
    # ------------------------- RP-193 a HOT chain is read at its root's place
    "a bitmap heap scan reads a HOT-updated row at its root": (
        "RP-193",
        True,
        [
            "create table rp47_h (id int primary key, v text)",
            "insert into rp47_h values (3, 'c'), (1, 'a'), (2, 'b')",
            "update rp47_h set v = 'aa' where id = 1",
            "select id from rp47_h where id > 0",
            "drop table rp47_h",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a bitmap heap scan reads a HOT-updated row at its root": [
        "CREATE TABLE",
        "INSERT 0 3",
        "UPDATE 1",
        [("3",), ("1",), ("2",)],
        "DROP TABLE",
    ],
    "a hash join probes its outer side in ctid order": [
        "CREATE TABLE",
        "INSERT 0 3",
        "UPDATE 1",
        "CREATE TABLE",
        "INSERT 0 3",
        [("1", "1"), ("3", "3"), ("2", "2")],
        "DROP TABLE",
    ],
    "a table with a primary key reads in ctid order too": [
        "CREATE TABLE",
        "INSERT 0 3",
        "UPDATE 1",
        [("3",), ("2",), ("1",)],
        [("3",), ("2",), ("1",)],
        "DROP TABLE",
    ],
    "a transaction's own updates are read in ctid order": [
        "CREATE TABLE",
        "BEGIN",
        "INSERT 0 3",
        "UPDATE 1",
        [("2",), ("3",), ("1",)],
        [("2,3,1",)],
        "COMMIT",
        "DROP TABLE",
    ],
    "an unordered select reads the heap in ctid order": [
        "CREATE TABLE",
        "INSERT 0 3",
        "UPDATE 1",
        [("1",), ("3",), ("2",)],
        [("1",), ("3",), ("2",)],
        [("1",), ("3",)],
        [("3",), ("2",)],
        "DELETE 1",
        "INSERT 0 1",
        [("1",), ("2",), ("4",)],
        "DROP TABLE",
    ],
}


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(name, marks=_marked(item, is_open), id=name)
        for name, (item, is_open, _) in sorted(SCENARIOS.items())
    ],
)
def test_scenario(name: str, rust_pg: Any) -> None:
    got = run_scenario(_dsn(rust_pg) + " dbname=postgres", SCENARIOS[name][2])
    assert got == EXPECTED[name]


if __name__ == "__main__":
    import pprint

    dsn = sys.argv[1]
    measured = {
        name: run_scenario(dsn + " dbname=postgres", stmts)
        for name, (_, _, stmts) in SCENARIOS.items()
    }
    print("EXPECTED: dict[str, Any] = " + pprint.pformat(measured, width=100, sort_dicts=True))
