"""Batch 46 of the Rust PG server (2026-10-23): the regression tests of
RP-069 (the system columns `ctid`, `xmin`, `cmin`, `xmax`, `cmax`, kept as
PostgreSQL keeps them; its first test is
`test_rp_found_2026_10_20.py::test_system_columns`) and of RP-194 (CREATE
TABLE's `WITH (...)` options were not recorded, found while fixing
RP-069), and the gap tests of RP-192 and RP-193, found with them.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch46.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
An xid differs between the two servers, so each is compared with the
transaction's own (`txid_current()`), never printed.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------------------------- RP-069 the system columns
    "a row version's ctid, xmin, cmin and xmax": (
        "RP-069",
        False,
        [
            "create table rp46_s (a int, t text)",
            "insert into rp46_s values (1, 'x'), (2, 'y')",
            "select ctid::text, xmax::text, cmin::text, cmax::text, a from rp46_s order by a",
            "begin",
            "insert into rp46_s values (3, 'z')",
            "insert into rp46_s values (4, 'w')",
            "update rp46_s set t = 'yy' where a = 2",
            "select ctid::text, xmin::text = txid_current()::text, cmin::text, a"
            " from rp46_s order by a",
            "delete from rp46_s where a = 1",
            "commit",
            "select a, t from rp46_s where ctid = '(0,4)'",
            "select a from rp46_s where ctid > '(0,3)' order by ctid",
            "select pg_typeof(ctid)::text, pg_typeof(xmin)::text, pg_typeof(cmin)::text"
            " from rp46_s limit 1",
            "insert into rp46_s values (5, 'v') returning ctid::text, cmin::text",
            "update rp46_s set t = 'q' where a = 5 and xmin = (select xmin from rp46_s"
            " where a = 5)",
            "update rp46_s set t = 'r' where a = 5 and xmin = (select xmin from rp46_s"
            " where a = 4)",
            "drop table rp46_s",
        ],
    ),
    "a heap's pages, a rolled-back row's place, and a conflict that takes none": (
        "RP-069",
        False,
        [
            "create table rp46_w (a int, b text)",
            "insert into rp46_w select g, repeat('x', 100) from generate_series(1, 200) g",
            "select ctid::text from rp46_w where a in (1, 58, 59, 70, 200) order by a",
            "update rp46_w set b = repeat('y', 100) where a = 3",
            "select ctid::text from rp46_w where a = 3",
            "truncate rp46_w",
            "insert into rp46_w values (1, 'a')",
            "select ctid::text from rp46_w",
            "create table rp46_b (id int primary key, t text)",
            "begin",
            "insert into rp46_b values (1, 'a')",
            "rollback",
            "insert into rp46_b values (2, 'b')",
            "begin",
            "insert into rp46_b values (3, 'c')",
            "savepoint s1",
            "insert into rp46_b values (4, 'd')",
            "rollback to s1",
            "insert into rp46_b values (5, 'e')",
            "commit",
            "insert into rp46_b values (5, 'x') on conflict (id) do update set t = excluded.t",
            "select ctid::text, cmin::text, id from rp46_b order by id",
            "create table rp46_big (a int, b text)",
            "insert into rp46_big select g, repeat('x', 3000) from generate_series(1, 5) g",
            "select ctid::text, a from rp46_big order by a",
            "drop table rp46_w, rp46_b, rp46_big",
        ],
    ),
    "a rewrite, a partition's own heap, VACUUM FULL, CLUSTER and fillfactor": (
        "RP-069",
        False,
        [
            "create table rp46_r (a int, b text)",
            "insert into rp46_r values (1, 'a'), (2, 'b'), (3, 'c')",
            "update rp46_r set b = 'bb' where a = 2",
            "alter table rp46_r alter column a type bigint",
            "select ctid::text, a from rp46_r order by a",
            "create table rp46_p (a int, b text) partition by range (a)",
            "create table rp46_p1 partition of rp46_p for values from (0) to (10)",
            "create table rp46_p2 partition of rp46_p for values from (10) to (20)",
            "insert into rp46_p values (1, 'x'), (15, 'y'), (2, 'z')",
            "select ctid::text, a from rp46_p order by a",
            "create table rp46_v (a int, b text)",
            "insert into rp46_v values (1, 'a'), (2, 'b'), (3, 'c')",
            "delete from rp46_v where a = 1",
            "vacuum full rp46_v",
            "select ctid::text, a from rp46_v order by a",
            "create table rp46_c (a int, b text)",
            "insert into rp46_c values (3, 'c'), (1, 'a'), (2, 'b')",
            "create index rp46_ci on rp46_c (a)",
            "cluster rp46_c using rp46_ci",
            "select ctid::text, a from rp46_c order by a",
            "create table rp46_f (a int, b text) with (fillfactor = 50)",
            "insert into rp46_f select g, repeat('x', 100) from generate_series(1, 60) g",
            "select ctid::text from rp46_f where a in (29, 30, 60) order by a",
            "drop table rp46_r, rp46_p, rp46_v, rp46_c, rp46_f",
        ],
    ),
    "a system column's name is taken": (
        "RP-069",
        False,
        [
            "create table rp46_x (xmin int)",
            "create table rp46_y (a int)",
            "alter table rp46_y add column ctid int",
            "alter table rp46_y add column if not exists cmax int",
            "alter table rp46_y rename column a to cmin",
            "alter table rp46_y rename column xmin to b",
            "insert into rp46_y (ctid) values ('(0,1)')",
            "update rp46_y set xmin = 5",
            "drop table rp46_y",
        ],
    ),
    # ----------------------------------- RP-194 CREATE TABLE WITH (options)
    "CREATE TABLE WITH options are the table's": (
        "RP-194",
        False,
        [
            "create table rp46_o (a int) with (fillfactor = 70, autovacuum_enabled = false)",
            "select reloptions::text from pg_class where relname = 'rp46_o'",
            "create table rp46_o2 (a int) with (rp46_nosuch = 1)",
            "drop table rp46_o",
        ],
    ),
    # ----------------------------------------- RP-192 a scan's order is ctid
    "a scan reads rows in ctid order": (
        "RP-192",
        True,
        [
            "create table rp46_q (a int, t text)",
            "insert into rp46_q values (1, 'x'), (2, 'y'), (3, 'z')",
            "update rp46_q set t = 'yy' where a = 2",
            "select string_agg(a::text, ',') from rp46_q",
            "drop table rp46_q",
        ],
    ),
    # ---------------------------------- RP-193 VACUUM frees a dead tuple
    "VACUUM frees a dead tuple's place": (
        "RP-193",
        True,
        [
            "create table rp46_d (a int)",
            "insert into rp46_d values (1), (2), (3)",
            "delete from rp46_d where a = 1",
            "vacuum rp46_d",
            "insert into rp46_d values (4)",
            "select ctid::text, a from rp46_d order by a",
            "drop table rp46_d",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "CREATE TABLE WITH options are the table's": [
        "CREATE TABLE",
        [("{fillfactor=70,autovacuum_enabled=false}",)],
        ("ERR", "22023", 'unrecognized parameter "rp46_nosuch"', None),
        "DROP TABLE",
    ],
    "VACUUM frees a dead tuple's place": [
        "CREATE TABLE",
        "INSERT 0 3",
        "DELETE 1",
        "VACUUM",
        "INSERT 0 1",
        [("(0,2)", "2"), ("(0,3)", "3"), ("(0,1)", "4")],
        "DROP TABLE",
    ],
    "a heap's pages, a rolled-back row's place, and a conflict that takes none": [
        "CREATE TABLE",
        "INSERT 0 200",
        [("(0,1)",), ("(0,58)",), ("(1,1)",), ("(1,12)",), ("(3,26)",)],
        "UPDATE 1",
        [("(3,27)",)],
        "TRUNCATE TABLE",
        "INSERT 0 1",
        [("(0,1)",)],
        "CREATE TABLE",
        "BEGIN",
        "INSERT 0 1",
        "ROLLBACK",
        "INSERT 0 1",
        "BEGIN",
        "INSERT 0 1",
        "SAVEPOINT",
        "INSERT 0 1",
        "ROLLBACK",
        "INSERT 0 1",
        "COMMIT",
        "INSERT 0 1",
        [("(0,2)", "0", "2"), ("(0,3)", "0", "3"), ("(0,6)", "0", "5")],
        "CREATE TABLE",
        "INSERT 0 5",
        [("(0,1)", "1"), ("(0,2)", "2"), ("(0,3)", "3"), ("(0,4)", "4"), ("(0,5)", "5")],
        "DROP TABLE",
    ],
    "a rewrite, a partition's own heap, VACUUM FULL, CLUSTER and fillfactor": [
        "CREATE TABLE",
        "INSERT 0 3",
        "UPDATE 1",
        "ALTER TABLE",
        [("(0,1)", "1"), ("(0,3)", "2"), ("(0,2)", "3")],
        "CREATE TABLE",
        "CREATE TABLE",
        "CREATE TABLE",
        "INSERT 0 3",
        [("(0,1)", "1"), ("(0,2)", "2"), ("(0,1)", "15")],
        "CREATE TABLE",
        "INSERT 0 3",
        "DELETE 1",
        "VACUUM",
        [("(0,1)", "2"), ("(0,2)", "3")],
        "CREATE TABLE",
        "INSERT 0 3",
        "CREATE INDEX",
        "CLUSTER",
        [("(0,1)", "1"), ("(0,2)", "2"), ("(0,3)", "3")],
        "CREATE TABLE",
        "INSERT 0 60",
        [("(0,29)",), ("(1,1)",), ("(2,2)",)],
        "DROP TABLE",
    ],
    "a row version's ctid, xmin, cmin and xmax": [
        "CREATE TABLE",
        "INSERT 0 2",
        [("(0,1)", "0", "0", "0", "1"), ("(0,2)", "0", "0", "0", "2")],
        "BEGIN",
        "INSERT 0 1",
        "INSERT 0 1",
        "UPDATE 1",
        [
            ("(0,1)", "False", "0", "1"),
            ("(0,5)", "True", "2", "2"),
            ("(0,3)", "True", "0", "3"),
            ("(0,4)", "True", "1", "4"),
        ],
        "DELETE 1",
        "COMMIT",
        [("4", "w")],
        [("4",), ("2",)],
        [("tid", "xid", "cid")],
        [("(0,6)", "0")],
        "UPDATE 1",
        "UPDATE 0",
        "DROP TABLE",
    ],
    "a scan reads rows in ctid order": [
        "CREATE TABLE",
        "INSERT 0 3",
        "UPDATE 1",
        [("1,3,2",)],
        "DROP TABLE",
    ],
    "a system column's name is taken": [
        ("ERR", "42701", 'column name "xmin" conflicts with a system column name', None),
        "CREATE TABLE",
        ("ERR", "42701", 'column name "ctid" conflicts with a system column name', None),
        ("ERR", "42701", 'column name "cmax" conflicts with a system column name', None),
        ("ERR", "42701", 'column name "cmin" conflicts with a system column name', None),
        ("ERR", "0A000", 'cannot rename system column "xmin"', None),
        ("ERR", "42703", 'column "ctid" of relation "rp46_y" does not exist', None),
        ("ERR", "0A000", 'cannot assign to system column "xmin"', None),
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
