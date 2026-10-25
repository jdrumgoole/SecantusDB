"""Batch 71 of the Rust PG server (2026-10-25): a B-tree deletes the dead
versions a full leaf holds before it splits (RP-196), and a table outside
`public` names its primary key as PostgreSQL does (RP-249).

Each scenario runs the same statements against a private, freshly
initdb'd PostgreSQL 15.19 cluster and this server; the literals are 15.19's
answers, measured by running this file as a script:

    PYTHONPATH=. python -m tests.backlog.test_rp_batch71 "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it
made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_NOAV = "with (autovacuum_enabled = false)"


def _pages(*indexes: str) -> str:
    return "select " + ", ".join(f"pg_relation_size('{i}') / 8192" for i in indexes)


def _updates(n: int, sql: str, *indexes: str, read: str | None = None) -> list[str]:
    steps: list[str] = []
    for _ in range(n):
        steps.append(sql)
        if read:
            steps.append(read)
        steps.append(_pages(*indexes))
    return steps


SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ----------------------------------------- RP-196 bottom-up deletion
    "an unchanged non-unique key's index deletes its dead versions": (
        "RP-196",
        False,
        [
            f"create table rp71_c (a int, b int, c int) {_NOAV}",
            "create index rp71_c_c on rp71_c (c)",
            "insert into rp71_c select g, 0, g % 50 from generate_series(1, 5000) g",
            *_updates(4, "update rp71_c set b = b + 1", "rp71_c_c"),
            "drop table rp71_c",
        ],
    ),
    "a large primary key under repeated updates": (
        "RP-196",
        False,
        [
            f"create table rp71_k (a int primary key, b int) {_NOAV}",
            "insert into rp71_k select g, 0 from generate_series(1, 10000) g",
            *_updates(3, "update rp71_k set b = b + 1", "rp71_k_pkey"),
            "drop table rp71_k",
        ],
    ),
    "an index whose key changes is not deleted from bottom-up": (
        "RP-196",
        False,
        [
            f"create table rp71_b (a int primary key, b int) {_NOAV}",
            "create index rp71_b_b on rp71_b (b)",
            "insert into rp71_b select g, g from generate_series(1, 3000) g",
            *_updates(3, "update rp71_b set b = b + 1", "rp71_b_pkey", "rp71_b_b"),
            "drop table rp71_b",
        ],
    ),
    "half the rows updated again and again": (
        "RP-196",
        False,
        [
            f"create table rp71_h (a int primary key, b int) {_NOAV}",
            "insert into rp71_h select g, 0 from generate_series(1, 4000) g",
            *_updates(5, "update rp71_h set b = b + 1 where a % 2 = 0", "rp71_h_pkey"),
            "drop table rp71_h",
        ],
    ),
    "a text primary key under repeated updates": (
        "RP-196",
        False,
        [
            f"create table rp71_t (k text primary key, b int) {_NOAV}",
            "insert into rp71_t select 'key-' || g, 0 from generate_series(1, 3000) g",
            *_updates(4, "update rp71_t set b = b + 1", "rp71_t_pkey"),
            "drop table rp71_t",
        ],
    ),
    # Fixed later: batch 74 (every step), batch 81 (index scans).
    "every step of eight updates of a primary key": (
        "RP-196",
        False,
        [
            f"create table rp71_e (a int primary key, b int) {_NOAV}",
            "insert into rp71_e select g, 0 from generate_series(1, 2000) g",
            *_updates(8, "update rp71_e set b = b + 1", "rp71_e_pkey"),
            "drop table rp71_e",
        ],
    ),
    "an index scan marks the dead versions it passes": (
        "RP-196",
        False,
        [
            f"create table rp71_s (a int primary key, b int) {_NOAV}",
            "insert into rp71_s select g, 0 from generate_series(1, 2000) g",
            *_updates(
                4,
                "update rp71_s set b = b + 1",
                "rp71_s_pkey",
                read="select count(*) from (select a from rp71_s order by a) s",
            ),
            "drop table rp71_s",
        ],
    ),
    # ------------------------------------- RP-249 a schema's primary keys
    "a table outside public names its primary key as PostgreSQL does": (
        "RP-249",
        False,
        [
            "create schema rp71_x",
            "create table rp71_x.t (a int primary key, b int)",
            "create index t_i on rp71_x.t (b)",
            "insert into rp71_x.t select g, g from generate_series(1, 1000) g",
            "select pg_relation_size('rp71_x.t') / 8192, pg_relation_size('rp71_x.t_pkey') / 8192,"
            " pg_relation_size('rp71_x.t_i') / 8192",
            "select c.relname, c.relnamespace::regnamespace::text, i.indrelid::regclass::text"
            " from pg_class c join pg_index i on i.indexrelid = c.oid"
            " where c.relname in ('t_pkey', 't_i') order by 1",
            "select conname from pg_constraint where conrelid = 'rp71_x.t'::regclass",
            "delete from rp71_x.t where a % 2 = 0",
            "vacuum rp71_x.t",
            "reindex table rp71_x.t",
            "select pg_relation_size('rp71_x.t_pkey') / 8192",
            "reindex index rp71_x.t_i",
            # REINDEX SCHEMA rebuilds the schema's tables' indexes (RP-196).
            "reindex schema rp71_x",
            "select pg_relation_size('rp71_x.t_i') / 8192",
            "alter table rp71_x.t drop constraint t_pkey",
            "alter table rp71_x.t add primary key (a)",
            "select conname from pg_constraint where conrelid = 'rp71_x.t'::regclass",
            "reindex schema rp71_nosuch",
            "drop schema rp71_x cascade",
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
    "an unchanged non-unique key's index deletes its dead versions": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5000",
        "UPDATE 5000",
        [("11",)],
        "UPDATE 5000",
        [("11",)],
        "UPDATE 5000",
        [("11",)],
        "UPDATE 5000",
        [("15",)],
        "DROP TABLE",
    ],
    "a large primary key under repeated updates": [
        "CREATE TABLE",
        "INSERT 0 10000",
        "UPDATE 10000",
        [("57",)],
        "UPDATE 10000",
        [("63",)],
        "UPDATE 10000",
        [("63",)],
        "DROP TABLE",
    ],
    "an index whose key changes is not deleted from bottom-up": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 3000",
        "UPDATE 3000",
        [("19", "19")],
        "UPDATE 3000",
        [("21", "27")],
        "UPDATE 3000",
        [("21", "27")],
        "DROP TABLE",
    ],
    "half the rows updated again and again": [
        "CREATE TABLE",
        "INSERT 0 4000",
        "UPDATE 2000",
        [("25",)],
        "UPDATE 2000",
        [("25",)],
        "UPDATE 2000",
        [("25",)],
        "UPDATE 2000",
        [("25",)],
        "UPDATE 2000",
        [("25",)],
        "DROP TABLE",
    ],
    "a text primary key under repeated updates": [
        "CREATE TABLE",
        "INSERT 0 3000",
        "UPDATE 3000",
        [("22",)],
        "UPDATE 3000",
        [("22",)],
        "UPDATE 3000",
        [("23",)],
        "UPDATE 3000",
        [("26",)],
        "DROP TABLE",
    ],
    "every step of eight updates of a primary key": [
        "CREATE TABLE",
        "INSERT 0 2000",
        "UPDATE 2000",
        [("13",)],
        "UPDATE 2000",
        [("14",)],
        "UPDATE 2000",
        [("14",)],
        "UPDATE 2000",
        [("16",)],
        "UPDATE 2000",
        [("17",)],
        "UPDATE 2000",
        [("18",)],
        "UPDATE 2000",
        [("18",)],
        "UPDATE 2000",
        [("18",)],
        "DROP TABLE",
    ],
    "an index scan marks the dead versions it passes": [
        "CREATE TABLE",
        "INSERT 0 2000",
        "UPDATE 2000",
        [("2000",)],
        [("13",)],
        "UPDATE 2000",
        [("2000",)],
        [("13",)],
        "UPDATE 2000",
        [("2000",)],
        [("13",)],
        "UPDATE 2000",
        [("2000",)],
        [("14",)],
        "DROP TABLE",
    ],
    "a table outside public names its primary key as PostgreSQL does": [
        "CREATE SCHEMA",
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 1000",
        [("5", "5", "5")],
        [("t_i", "rp71_x", "rp71_x.t"), ("t_pkey", "rp71_x", "rp71_x.t")],
        [("t_pkey",)],
        "DELETE 500",
        "VACUUM",
        "REINDEX",
        [("4",)],
        "REINDEX",
        "REINDEX",
        [("4",)],
        "ALTER TABLE",
        "ALTER TABLE",
        [("t_pkey",)],
        ("ERR", "3F000", 'schema "rp71_nosuch" does not exist', None),
        "DROP SCHEMA",
    ],
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_scenario(dsn, stmts) for name, (_, _, stmts) in SCENARIOS.items()})
