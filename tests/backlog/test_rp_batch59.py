"""Batch 59 of the Rust PG server (2026-10-24): every oid drawn from one
counter, so oids rise in creation order (RP-137) -- a sequence's, a view's,
a function's, and an index's, a constraint's among them -- and what was
found building it: a primary key could not be dropped (RP-234), and a
table made in an open block had no system columns (RP-235).

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
        False,
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
    "a foreign key over the key refuses its drop": (
        "RP-234",
        False,
        [
            "begin",
            "create table rp59_fp (a int primary key, b int unique)",
            "create table rp59_fc (x int references rp59_fp, y int references rp59_fp (b))",
            "savepoint s",
            "!alter table rp59_fp drop constraint rp59_fp_pkey",
            "rollback to s",
            "!alter table rp59_fp drop constraint rp59_fp_b_key",
            "rollback to s",
            "~alter table rp59_fp drop constraint rp59_fp_pkey cascade",
            "select conname from pg_constraint"
            " where conrelid in ('rp59_fp'::regclass, 'rp59_fc'::regclass) order by 1",
            "insert into rp59_fc values (5, null)",
            "select attname, attnotnull from pg_attribute"
            " where attrelid = 'rp59_fp'::regclass and attnum > 0 order by attnum",
            "rollback",
        ],
    ),
    "a composite key dropped and its rows kept": (
        "RP-234",
        False,
        [
            "begin",
            "create schema rp59_s",
            "create table rp59_s.t (a int, b int, c text, primary key (a, b))",
            "create index rp59_sx on rp59_s.t (a, c)",
            "insert into rp59_s.t values (1, 2, 'x'), (1, 3, 'y')",
            "alter table rp59_s.t drop constraint t_pkey",
            "insert into rp59_s.t values (1, 2, 'z')",
            "select a, b, c from rp59_s.t order by a, b, c",
            "select a, b, c from rp59_s.t where a = 1 and c = 'z'",
            "update rp59_s.t set b = 9 where c = 'y'",
            "select a, b, c from rp59_s.t order by a, b, c",
            "savepoint s",
            "!alter table rp59_s.t add primary key (a, b)",
            "rollback to s",
            "delete from rp59_s.t where c = 'z'",
            "alter table rp59_s.t add primary key (a, b)",
            "select a, b, c from rp59_s.t order by a, b",
            "rollback",
        ],
    ),
    "a dropped key leaves the rows where they were": (
        "RP-234",
        False,
        [
            "begin",
            "create table rp59_w (a int primary key, b int)",
            "insert into rp59_w select g, g from generate_series(1, 5) g",
            "delete from rp59_w where a = 2",
            "alter table rp59_w drop constraint rp59_w_pkey",
            "select ctid::text, a from rp59_w order by a",
            "insert into rp59_w values (9, 9)",
            "select ctid::text, a from rp59_w where a = 9",
            "rollback",
        ],
    ),
    # ----------------------------------- RP-235 a block's table's system columns
    "a table made in the block answers its system columns": (
        "RP-235",
        False,
        [
            "begin",
            "create table rp59_y (a int)",
            "insert into rp59_y values (1), (2)",
            "select ctid::text, cmin::text, xmax::text, a from rp59_y order by a",
            "rollback",
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
    "a foreign key over the key refuses its drop": [
        "BEGIN",
        "CREATE TABLE",
        "CREATE TABLE",
        "SAVEPOINT",
        (
            "ERR",
            "2BP01",
            "cannot drop constraint rp59_fp_pkey on table "
            "rp59_fp because other objects depend on it",
            "Use DROP ... CASCADE to drop the dependent objects too.",
            "constraint rp59_fc_x_fkey on table rp59_fc depends on index rp59_fp_pkey",
        ),
        "ROLLBACK",
        (
            "ERR",
            "2BP01",
            "cannot drop constraint rp59_fp_b_key on table "
            "rp59_fp because other objects depend on it",
            "Use DROP ... CASCADE to drop the dependent objects too.",
            "constraint rp59_fc_y_fkey on table rp59_fc depends on index rp59_fp_b_key",
        ),
        "ROLLBACK",
        ("ALTER TABLE", ["drop cascades to constraint rp59_fc_x_fkey on table rp59_fc"]),
        [("rp59_fc_y_fkey",), ("rp59_fp_b_key",)],
        "INSERT 0 1",
        [("a", "True"), ("b", "False")],
        "ROLLBACK",
    ],
    "a composite key dropped and its rows kept": [
        "BEGIN",
        "CREATE SCHEMA",
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 2",
        "ALTER TABLE",
        "INSERT 0 1",
        [("1", "2", "x"), ("1", "2", "z"), ("1", "3", "y")],
        [("1", "2", "z")],
        "UPDATE 1",
        [("1", "2", "x"), ("1", "2", "z"), ("1", "9", "y")],
        "SAVEPOINT",
        (
            "ERR",
            "23505",
            'could not create unique index "t_pkey"',
            None,
            "Key (a, b)=(1, 2) is duplicated.",
        ),
        "ROLLBACK",
        "DELETE 1",
        "ALTER TABLE",
        [("1", "2", "x"), ("1", "9", "y")],
        "ROLLBACK",
    ],
    "a dropped key leaves the rows where they were": [
        "BEGIN",
        "CREATE TABLE",
        "INSERT 0 5",
        "DELETE 1",
        "ALTER TABLE",
        [("(0,1)", "1"), ("(0,3)", "3"), ("(0,4)", "4"), ("(0,5)", "5")],
        "INSERT 0 1",
        [("(0,6)", "9")],
        "ROLLBACK",
    ],
    "a table made in the block answers its system columns": [
        "BEGIN",
        "CREATE TABLE",
        "INSERT 0 2",
        [("(0,1)", "1", "0", "1"), ("(0,2)", "1", "0", "2")],
        "ROLLBACK",
    ],
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_scenario(dsn, stmts) for name, (_, _, stmts) in SCENARIOS.items()})
