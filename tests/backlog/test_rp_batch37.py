"""Batch 37 of the Rust PG server (2026-10-23): the regression tests of
what it fixed -- RP-117 (a view's row type is a type), RP-157 (a dropped
table column leaves its attnum) and RP-175 (a subquery taken for a view an
earlier statement expanded), found while fixing RP-117.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch37.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # --------------------------------------- RP-117 a view's row type, more
    "a view's row type follows the view": (
        "RP-117",
        False,
        [
            "create table rp37_t (a int, b text)",
            "insert into rp37_t values (1, 'x')",
            "create view rp37_v as select a, b from rp37_t",
            "select pg_typeof(v)::text, v::text, (v).b from rp37_v v",
            "select pg_typeof(rp37_v)::text from rp37_v",
            "select row(2, 'y')::rp37_v",
            "select array[row(1, 'a')::rp37_v]::text",
            "alter view rp37_v rename column b to bb",
            "select ('(3,z)'::rp37_v).bb",
            "alter view rp37_v rename to rp37_w",
            "select 'rp37_w'::regtype::text, to_regtype('rp37_v')",
            "create view rp37_a (x, y) as select 1, 'q'::text",
            "create or replace view rp37_a (x, y, z) as select 1, 'q'::text, 2.5",
            "select ('(1,b,3)'::rp37_a).z",
            "create schema rp37_s",
            "create view rp37_s.v as select now() as t",
            "select 'rp37_s.v'::regtype::text",
            "drop view rp37_w, rp37_a",
            "select to_regtype('rp37_w'), to_regtype('rp37_a')",
            "drop schema rp37_s cascade",
            "drop table rp37_t",
        ],
    ),
    # ------------------------- RP-175 a subquery is not an earlier view
    "a subquery under a view's alias is no view": (
        "RP-175",
        False,
        [
            "create view s as select 1 as one",
            "select pg_typeof(s)::text from s",
            "select pg_typeof(s)::text from (select 2 as two) s",
            "select (select pg_typeof(s)::text from s), pg_typeof(x)::text from (select 3) x",
            "drop view s",
        ],
    ),
    # --------------------------------------- RP-157 a dropped column's slot
    "a dropped column's slot keeps every attnum": (
        "RP-157",
        False,
        [
            "create table rp37_d (a int, b text, c int default 7, d int check (d > 0), "
            "e text, primary key (a, d))",
            "create index rp37_di on rp37_d (e)",
            "create index rp37_dk on rp37_d ((e || 'x'))",
            "alter table rp37_d drop column b",
            "alter table rp37_d add column f int default 9",
            "create unique index rp37_dj on rp37_d (f)",
            "create table rp37_r (x int, y int, foreign key (x, y) references rp37_d (a, d))",
            "select attname, attnum, attisdropped from pg_attribute "
            "where attrelid = 'rp37_d'::regclass and attnum > 0 order by attnum",
            "select column_name, ordinal_position from information_schema.columns "
            "where table_name = 'rp37_d' order by 2",
            "select indexrelid::regclass::text, indkey::text from pg_index "
            "where indrelid = 'rp37_d'::regclass order by 1",
            "select adnum, pg_get_expr(adbin, adrelid) from pg_attrdef "
            "where adrelid = 'rp37_d'::regclass order by 1",
            "select conname, conkey::text, confkey::text from pg_constraint "
            "where conrelid in ('rp37_d'::regclass, 'rp37_r'::regclass) order by 1",
            "insert into rp37_d (a, d, e) values (1, 2, 'x')",
            "select * from rp37_d",
            "comment on column rp37_d.f is 'eff'",
            "select col_description('rp37_d'::regclass, 6), col_description('rp37_d'::regclass, 5)",
            "drop table rp37_r",
            "alter table rp37_d drop column c",
            "select attnum, attisdropped from pg_attribute "
            "where attrelid = 'rp37_d'::regclass and attnum > 0 order by attnum",
            "drop table rp37_d",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a dropped column's slot keeps every attnum": [
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE INDEX",
        "ALTER TABLE",
        "ALTER TABLE",
        "CREATE INDEX",
        "CREATE TABLE",
        [
            ("a", "1", "False"),
            ("........pg.dropped.2........", "2", "True"),
            ("c", "3", "False"),
            ("d", "4", "False"),
            ("e", "5", "False"),
            ("f", "6", "False"),
        ],
        [("a", "1"), ("c", "3"), ("d", "4"), ("e", "5"), ("f", "6")],
        [("rp37_d_pkey", "1 4"), ("rp37_di", "5"), ("rp37_dj", "6"), ("rp37_dk", "0")],
        [("3", "7"), ("6", "9")],
        [
            ("rp37_d_d_check", "{4}", "None"),
            ("rp37_d_pkey", "{1,4}", "None"),
            ("rp37_r_x_y_fkey", "{1,2}", "{1,4}"),
        ],
        "INSERT 0 1",
        [("1", "7", "2", "x", "9")],
        "COMMENT",
        [("eff", "None")],
        "DROP TABLE",
        "ALTER TABLE",
        [
            ("1", "False"),
            ("2", "True"),
            ("3", "True"),
            ("4", "False"),
            ("5", "False"),
            ("6", "False"),
        ],
        "DROP TABLE",
    ],
    "a subquery under a view's alias is no view": [
        "CREATE VIEW",
        [("s",)],
        [("record",)],
        [("s", "record")],
        "DROP VIEW",
    ],
    "a view's row type follows the view": [
        "CREATE TABLE",
        "INSERT 0 1",
        "CREATE VIEW",
        [("rp37_v", "(1,x)", "x")],
        [("rp37_v",)],
        [("(2,y)",)],
        [('{"(1,a)"}',)],
        "ALTER VIEW",
        [("z",)],
        "ALTER VIEW",
        [("rp37_w", "None")],
        "CREATE VIEW",
        "CREATE VIEW",
        [("3",)],
        "CREATE SCHEMA",
        "CREATE VIEW",
        [("rp37_s.v",)],
        "DROP VIEW",
        [("None", "None")],
        "DROP SCHEMA",
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
