"""Batch 48 of the Rust PG server (2026-10-23): the regression tests of
RP-193 (VACUUM, HOT updates, on-access pruning and the free space map, kept
as PostgreSQL 15.19 keeps them; its first test is
`test_rp_batch46.py::test_scenario[VACUUM frees a dead tuple's place]`) and
of RP-195 (`pg_relation_size` was the rows' encoded bytes, found with it),
and the gap tests of what is left of RP-193 (a restart) and of RP-196
(`pg_class` shows no page counts), found with it.

The scenarios run as `test_rp_batch34.py`'s do: each statement in a named
autocommit connection, `a` unless it starts `@name `, so a scenario can
read the free space map from a backend with no target block of its own.
The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster by running this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch48.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every table is made with
`autovacuum_enabled = false`, so that no autovacuum run moves the reference
server's answer, and every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_NOAV = "with (autovacuum_enabled = false)"

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ---------------------------------------------- RP-193 HOT and VACUUM
    "VACUUM leaves a HOT chain's root redirected": (
        "RP-193",
        False,
        [
            f"create table rp48_h (a int, t text) {_NOAV}",
            "insert into rp48_h values (1, 'x'), (2, 'y'), (3, 'z')",
            "update rp48_h set t = 'yy' where a = 2",
            "select ctid::text, a from rp48_h order by a",
            "vacuum rp48_h",
            "insert into rp48_h values (4, 'w'), (5, 'v')",
            "select ctid::text, a from rp48_h order by a",
            "drop table rp48_h",
        ],
    ),
    "VACUUM frees a non-HOT version's line pointer for reuse": (
        "RP-193",
        False,
        [
            f"create table rp48_n (a int primary key, t text) {_NOAV}",
            "insert into rp48_n values (1, 'x'), (2, 'y'), (3, 'z')",
            "update rp48_n set a = 20 where a = 2",
            "vacuum rp48_n",
            "insert into rp48_n values (4, 'w'), (5, 'v')",
            "select ctid::text, a from rp48_n order by a",
            "drop table rp48_n",
        ],
    ),
    "a full page is pruned when a scan reads it": (
        "RP-193",
        False,
        [
            f"create table rp48_p (a int, t text) {_NOAV}",
            "insert into rp48_p select g, repeat('x', 100) from generate_series(1, 58) g",
            "update rp48_p set t = repeat('y', 100) where a = 1",
            "update rp48_p set t = repeat('z', 100) where a = 1",
            "update rp48_p set t = repeat('w', 100) where a = 1",
            "select ctid::text, a from rp48_p where a in (1, 58) order by a",
            "delete from rp48_p where a = 5",
            "select ctid::text, a from rp48_p where a in (1, 58) order by a",
            "update rp48_p set t = repeat('q', 100) where a = 2",
            "select ctid::text, a from rp48_p where a in (1, 2) order by a",
            "insert into rp48_p values (100, 'n')",
            "select ctid::text from rp48_p where a = 100",
            "drop table rp48_p",
        ],
    ),
    "a rolled-back delete keeps its row, a rolled-back insert's place is freed": (
        "RP-193",
        False,
        [
            f"create table rp48_r (a int) {_NOAV}",
            "insert into rp48_r values (1), (2), (3)",
            "begin",
            "delete from rp48_r where a = 2",
            "rollback",
            "vacuum rp48_r",
            "insert into rp48_r values (4)",
            "select ctid::text, a from rp48_r order by a",
            "begin",
            "insert into rp48_r values (5)",
            "rollback",
            "vacuum rp48_r",
            "insert into rp48_r values (6)",
            "select ctid::text, a from rp48_r order by a",
            "drop table rp48_r",
        ],
    ),
    "VACUUM truncates the empty pages at the end": (
        "RP-193",
        False,
        [
            f"create table rp48_t (a int) {_NOAV}",
            "insert into rp48_t select generate_series(1, 600)",
            "select pg_relation_size('rp48_t') / 8192",
            "delete from rp48_t where a > 200",
            "vacuum rp48_t",
            "select pg_relation_size('rp48_t') / 8192",
            "insert into rp48_t values (1)",
            "select max(ctid)::text from rp48_t",
            "drop table rp48_t",
        ],
    ),
    "VACUUM with few dead items skips the index pass": (
        "RP-193",
        False,
        [
            f"create table rp48_b (a int primary key, t text) {_NOAV}",
            "insert into rp48_b select g, repeat('x', 100) from generate_series(1, 5800) g",
            "delete from rp48_b where a = 3",
            "vacuum rp48_b",
            "@b insert into rp48_b values (100000, 'n')",
            "select ctid::text from rp48_b where a = 100000",
            "drop table rp48_b",
        ],
    ),
    "a new backend finds space through the free space map": (
        "RP-193",
        False,
        [
            f"create table rp48_f (a int, t text) {_NOAV}",
            "insert into rp48_f select g, repeat('x', 100) from generate_series(1, 200) g",
            "delete from rp48_f where a in (3, 70, 150)",
            "vacuum rp48_f",
            "@b insert into rp48_f values (1000, 'n')",
            "@c insert into rp48_f values (1001, 'n'), (1002, 'm')",
            "@b insert into rp48_f values (1003, 'o')",
            "select ctid::text, a from rp48_f where a >= 1000 order by a",
            "drop table rp48_f",
        ],
    ),
    "the free space map's upper levels wait for VACUUM": (
        "RP-193",
        False,
        [
            f"create table rp48_g (a int, t text) {_NOAV}",
            "insert into rp48_g select g, repeat('x', 100) from generate_series(1, 100) g",
            "@b insert into rp48_g values (1000, 'n')",
            "select ctid::text from rp48_g where a = 1000",
            "vacuum rp48_g",
            "@c insert into rp48_g values (1001, 'n')",
            "select ctid::text from rp48_g where a = 1001",
            "drop table rp48_g",
        ],
    ),
    "a running writer holds a dead tuple": (
        "RP-193",
        False,
        [
            f"create table rp48_w (a int) {_NOAV}",
            f"create table rp48_w2 (a int) {_NOAV}",
            "insert into rp48_w values (1), (2), (3)",
            "@b begin",
            "@b insert into rp48_w2 values (1)",
            "delete from rp48_w where a = 1",
            "vacuum rp48_w",
            "insert into rp48_w values (4)",
            "@b commit",
            "vacuum rp48_w",
            "insert into rp48_w values (5)",
            "select ctid::text, a from rp48_w order by a",
            "drop table rp48_w, rp48_w2",
        ],
    ),
    "a REPEATABLE READ snapshot holds a dead tuple": (
        "RP-193",
        False,
        [
            f"create table rp48_s (a int) {_NOAV}",
            "insert into rp48_s values (1), (2), (3)",
            "@b begin isolation level repeatable read",
            "@b select count(*) from rp48_s",
            "delete from rp48_s where a = 1",
            "vacuum rp48_s",
            "insert into rp48_s values (4)",
            "@b commit",
            "select ctid::text, a from rp48_s order by a",
            "drop table rp48_s",
        ],
    ),
    # -------------------------------------- RP-196 pg_class's page counts
    "pg_class shows what VACUUM counted": (
        "RP-196",
        False,
        [
            f"create table rp48_c (a int, t text) {_NOAV}",
            "insert into rp48_c select g, repeat('x', 100) from generate_series(1, 200) g",
            "vacuum rp48_c",
            "select relpages, reltuples, relallvisible from pg_class where relname = 'rp48_c'",
            "delete from rp48_c where a <= 100",
            "vacuum rp48_c",
            "select relpages, reltuples from pg_class where relname = 'rp48_c'",
            "drop table rp48_c",
        ],
    ),
    # ------------------------------------------------ RP-195 relation sizes
    "pg_relation_size counts the heap's pages": (
        "RP-195",
        False,
        [
            f"create table rp48_z (a int, t text) {_NOAV}",
            "select pg_relation_size('rp48_z')",
            "insert into rp48_z select g, repeat('x', 100) from generate_series(1, 200) g",
            "select pg_relation_size('rp48_z'), "
            "pg_table_size('rp48_z') >= pg_relation_size('rp48_z')",
            "update rp48_z set a = a + 1000 where a <= 100",
            "select pg_relation_size('rp48_z')",
            "drop table rp48_z",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "VACUUM frees a non-HOT version's line pointer for reuse": [
        "CREATE TABLE",
        "INSERT 0 3",
        "UPDATE 1",
        "VACUUM",
        "INSERT 0 2",
        [("(0,1)", "1"), ("(0,3)", "3"), ("(0,2)", "4"), ("(0,5)", "5"), ("(0,4)", "20")],
        "DROP TABLE",
    ],
    "VACUUM leaves a HOT chain's root redirected": [
        "CREATE TABLE",
        "INSERT 0 3",
        "UPDATE 1",
        [("(0,1)", "1"), ("(0,4)", "2"), ("(0,3)", "3")],
        "VACUUM",
        "INSERT 0 2",
        [("(0,1)", "1"), ("(0,4)", "2"), ("(0,3)", "3"), ("(0,5)", "4"), ("(0,6)", "5")],
        "DROP TABLE",
    ],
    "VACUUM truncates the empty pages at the end": [
        "CREATE TABLE",
        "INSERT 0 600",
        [("3",)],
        "DELETE 400",
        "VACUUM",
        [("1",)],
        "INSERT 0 1",
        [("(0,201)",)],
        "DROP TABLE",
    ],
    "VACUUM with few dead items skips the index pass": [
        "CREATE TABLE",
        "INSERT 0 5800",
        "DELETE 1",
        "VACUUM",
        "INSERT 0 1",
        [("(0,59)",)],
        "DROP TABLE",
    ],
    "a REPEATABLE READ snapshot holds a dead tuple": [
        "CREATE TABLE",
        "INSERT 0 3",
        "BEGIN",
        [("3",)],
        "DELETE 1",
        "VACUUM",
        "INSERT 0 1",
        "COMMIT",
        [("(0,2)", "2"), ("(0,3)", "3"), ("(0,4)", "4")],
        "DROP TABLE",
    ],
    "a full page is pruned when a scan reads it": [
        "CREATE TABLE",
        "INSERT 0 58",
        "UPDATE 1",
        "UPDATE 1",
        "UPDATE 1",
        [("(1,3)", "1"), ("(0,58)", "58")],
        "DELETE 1",
        [("(1,3)", "1"), ("(0,58)", "58")],
        "UPDATE 1",
        [("(1,3)", "1"), ("(0,59)", "2")],
        "INSERT 0 1",
        [("(1,4)",)],
        "DROP TABLE",
    ],
    "a new backend finds space through the free space map": [
        "CREATE TABLE",
        "INSERT 0 200",
        "DELETE 3",
        "VACUUM",
        "INSERT 0 1",
        "INSERT 0 2",
        "INSERT 0 1",
        [("(0,3)", "1000"), ("(1,12)", "1001"), ("(1,59)", "1002"), ("(0,59)", "1003")],
        "DROP TABLE",
    ],
    "a rolled-back delete keeps its row, a rolled-back insert's place is freed": [
        "CREATE TABLE",
        "INSERT 0 3",
        "BEGIN",
        "DELETE 1",
        "ROLLBACK",
        "VACUUM",
        "INSERT 0 1",
        [("(0,1)", "1"), ("(0,2)", "2"), ("(0,3)", "3"), ("(0,4)", "4")],
        "BEGIN",
        "INSERT 0 1",
        "ROLLBACK",
        "VACUUM",
        "INSERT 0 1",
        [("(0,1)", "1"), ("(0,2)", "2"), ("(0,3)", "3"), ("(0,4)", "4"), ("(0,5)", "6")],
        "DROP TABLE",
    ],
    "a running writer holds a dead tuple": [
        "CREATE TABLE",
        "CREATE TABLE",
        "INSERT 0 3",
        "BEGIN",
        "INSERT 0 1",
        "DELETE 1",
        "VACUUM",
        "INSERT 0 1",
        "COMMIT",
        "VACUUM",
        "INSERT 0 1",
        [("(0,2)", "2"), ("(0,3)", "3"), ("(0,4)", "4"), ("(0,1)", "5")],
        "DROP TABLE",
    ],
    "pg_class shows what VACUUM counted": [
        "CREATE TABLE",
        "INSERT 0 200",
        "VACUUM",
        [("4", "200.0", "4")],
        "DELETE 100",
        "VACUUM",
        [("4", "100.0")],
        "DROP TABLE",
    ],
    "pg_relation_size counts the heap's pages": [
        "CREATE TABLE",
        [("0",)],
        "INSERT 0 200",
        [("32768", "True")],
        "UPDATE 100",
        [("49152",)],
        "DROP TABLE",
    ],
    "the free space map's upper levels wait for VACUUM": [
        "CREATE TABLE",
        "INSERT 0 100",
        "INSERT 0 1",
        [("(1,43)",)],
        "VACUUM",
        "INSERT 0 1",
        [("(0,59)",)],
        "DROP TABLE",
    ],
}


@pytest.mark.backlog_fixed("RP-193")
def test_a_dead_tuple_outlives_a_restart(tmp_path: Any) -> None:
    """PostgreSQL 15.19 keeps a heap's dead tuples on their pages across a
    restart, until VACUUM: 60 rows of 140 bytes, row 1 deleted, row 2
    updated (a HOT update on its page, which the UPDATE's scan pruned
    first). After a restart, row 100 goes on the last page; a VACUUM then
    leaves row 2's old place a redirect to its new version, so rows 101
    and 102 take (0,1) and a new line pointer, (0,60)."""
    import psycopg

    from tests.test_rust_pgserver_slice import _Server

    def run(server: Any, statements: list[str]) -> list[Any]:
        out: list[Any] = []
        with psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c:
            for sql in statements:
                cur = c.execute(sql)
                if cur.description is not None:
                    out.append([str(r[0]) for r in cur.fetchall()])
        return out

    with _Server(tmp_path) as server:
        run(
            server,
            [
                f"create table rp48_x (a int, t text) {_NOAV}",
                "insert into rp48_x select g, repeat('x', 100) from generate_series(1, 60) g",
                "delete from rp48_x where a = 1",
                "update rp48_x set t = repeat('y', 100) where a = 2",
            ],
        )
    with _Server(tmp_path) as server:
        got = run(
            server,
            [
                "insert into rp48_x values (100, 'n')",
                "select ctid from rp48_x where a in (2, 100) order by a",
                "vacuum rp48_x",
                "insert into rp48_x values (101, 'm'), (102, 'o')",
                "select ctid from rp48_x where a >= 101 order by a",
            ],
        )
    assert got == [["(0,59)", "(1,3)"], ["(0,1)", "(0,60)"]]


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
