"""Batch 59 of the Rust PG server (2026-10-24): every oid drawn from one
counter, so oids rise in creation order (RP-137) -- a sequence's, a view's,
a function's, and an index's, a constraint's among them -- and what was
found building it (RP-234).

Each scenario runs the same statements against a private, freshly
initdb'd PostgreSQL 15.19 cluster and this server; the literals are 15.19's
answers, measured by running this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch59.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario rolls back or
drops what it made, and none prints an oid itself, only the order oids put
objects in.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "relations in oid order are in creation order": (
        "RP-137",
        False,
        [
            "begin",
            "create table rp59_t (a int primary key, b int unique, c int,"
            " exclude using btree (c with =))",
            "create index rp59_i on rp59_t (c)",
            "create sequence rp59_s",
            "alter table rp59_t add constraint rp59_u unique (c)",
            "create view rp59_v as select 1 as x",
            "create table rp59_t2 (a serial)",
            "alter table rp59_t2 add primary key (a)",
            "select relname, relkind from pg_class where relname like 'rp59%' order by oid",
            "rollback",
        ],
    ),
    "an index's oid is the same in every catalog": (
        "RP-137",
        False,
        [
            "begin",
            "create table rp59_c (a int primary key, b int unique)",
            "create index rp59_ci on rp59_c (b)",
            "select c.relname, c.oid = i.indexrelid from pg_class c"
            " join pg_index i on i.indexrelid = c.oid where c.relname like 'rp59%' order by c.oid",
            "select conname, conindid::regclass::text from pg_constraint"
            " where conrelid = 'rp59_c'::regclass order by oid",
            "rollback",
        ],
    ),
    "a sequence read in the block it was made in stays a relation": (
        "RP-137",
        False,
        [
            "begin",
            "create sequence rp59_q",
            "select 1 from rp59_q",
            "alter table rp59_q owner to postgres",
            "select relkind from pg_class where relname = 'rp59_q'",
            "rollback",
        ],
    ),
    # ------------------------------------------- RP-234 dropping the key
    "a primary key can be dropped": (
        "RP-234",
        True,
        [
            "create table rp59_k (a int primary key)",
            "insert into rp59_k values (1)",
            "alter table rp59_k drop constraint rp59_k_pkey",
            "insert into rp59_k values (1)",
            "select a from rp59_k order by a",
            "select count(*) from pg_constraint where conrelid = 'rp59_k'::regclass",
            "drop table rp59_k",
        ],
    ),
}


def _marked(item: str, is_open: bool) -> Any:
    return pytest.mark.backlog(item) if is_open else pytest.mark.backlog_fixed(item)


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
    "relations in oid order are in creation order": [
        "BEGIN",
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE SEQUENCE",
        "ALTER TABLE",
        "CREATE VIEW",
        "CREATE TABLE",
        "ALTER TABLE",
        [
            ("rp59_t", "r"),
            ("rp59_t_pkey", "i"),
            ("rp59_t_b_key", "i"),
            ("rp59_t_c_excl", "i"),
            ("rp59_i", "i"),
            ("rp59_s", "S"),
            ("rp59_u", "i"),
            ("rp59_v", "v"),
            ("rp59_t2_a_seq", "S"),
            ("rp59_t2", "r"),
            ("rp59_t2_pkey", "i"),
        ],
        "ROLLBACK",
    ],
    "an index's oid is the same in every catalog": [
        "BEGIN",
        "CREATE TABLE",
        "CREATE INDEX",
        [("rp59_c_pkey", "True"), ("rp59_c_b_key", "True"), ("rp59_ci", "True")],
        [("rp59_c_pkey", "rp59_c_pkey"), ("rp59_c_b_key", "rp59_c_b_key")],
        "ROLLBACK",
    ],
    "a sequence read in the block it was made in stays a relation": [
        "BEGIN",
        "CREATE SEQUENCE",
        [("1",)],
        "ALTER TABLE",
        [("S",)],
        "ROLLBACK",
    ],
    "a primary key can be dropped": [
        "CREATE TABLE",
        "INSERT 0 1",
        "ALTER TABLE",
        "INSERT 0 1",
        [("1",), ("1",)],
        [("0",)],
        "DROP TABLE",
    ],
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_scenario(dsn, stmts) for name, (_, _, stmts) in SCENARIOS.items()})
