"""Batch 45 of the Rust PG server (2026-10-23): the regression tests of
what it fixed of RP-105 (`pg_depend` -- a constant's type, a range's
constructors, a column's collation, an extension's schema and types; its
first test is in `test_rp_batch26.py`) and of RP-191 (an index predicate's
constants printed untyped, found while fixing RP-105), and the gap test of
what is left of RP-105: an extension's functions, operators, casts and
operator classes.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch45.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------------------------------- RP-105 pg_depend rows
    "a range's constructors, a collation's columns and an extension's type": (
        "RP-105",
        False,
        [
            "create type rp45_r as range (subtype = int4)",
            "select p.oid::regprocedure::text, p.prorettype::regtype::text, p.proisstrict,"
            " p.prosrc, p.provariadic::regtype::text from pg_proc p"
            " where p.proname in ('rp45_r', 'rp45_r_multirange') order by p.oid",
            "select p.oid::regprocedure::text, d.refclassid::regclass::text, d.deptype"
            " from pg_depend d join pg_proc p on p.oid = d.objid"
            " where d.classid = 'pg_proc'::regclass"
            " and p.proname in ('rp45_r', 'rp45_r_multirange') order by p.oid, 2, 3",
            "select castsource::regtype::text, casttarget::regtype::text,"
            " castfunc::regprocedure::text, castcontext from pg_cast"
            " where castsource = 'rp45_r'::regtype",
            "select rp45_r(1, 5)::text, rp45_r_multirange(rp45_r(1, 2), rp45_r(4, 5))::text",
            "drop type rp45_r",
            "create collation rp45_k (provider = icu, locale = 'de-x-icu')",
            "create table rp45_kt (a text collate rp45_k, b text)",
            "select refclassid::regclass::text, deptype from pg_depend"
            " where objid = 'rp45_kt'::regclass and refclassid = 'pg_collation'::regclass",
            "drop table rp45_kt",
            "drop collation rp45_k",
            "create extension citext",
            "select refclassid::regclass::text, deptype from pg_depend"
            " where classid = 'pg_extension'::regclass"
            " and objid = (select oid from pg_extension where extname = 'citext')",
            "select count(*) from pg_depend where classid = 'pg_type'::regclass and deptype = 'e'"
            " and refobjid = (select oid from pg_extension where extname = 'citext')",
            "drop extension citext",
        ],
    ),
    "an extension's members": (
        "RP-105",
        True,
        [
            "create extension citext",
            "select classid::regclass::text, count(*) from pg_depend"
            " where refclassid = 'pg_extension'::regclass and deptype = 'e'"
            " and refobjid = (select oid from pg_extension where extname = 'citext')"
            " group by 1 order by 1",
            "drop extension citext",
        ],
    ),
    # ---------------------------------- RP-191 a predicate's typed constants
    "an index predicate's constants are typed": (
        "RP-191",
        False,
        [
            "create type rp45_e as enum ('a', 'b')",
            "create table rp45_p (a int, b rp45_e, i2 int2, i8 int8, n numeric, f8 float8,"
            " d date, ts timestamp, bo bool, u uuid, c char(3), nm name, iv interval, t text)",
            "create index rp45_p01 on rp45_p (a) where a = '5'",
            "create index rp45_p02 on rp45_p (a) where i2 = '5'",
            "create index rp45_p03 on rp45_p (a) where i8 = '5'",
            "create index rp45_p04 on rp45_p (a) where n = '1.50'",
            "create index rp45_p05 on rp45_p (a) where f8 = '1.5'",
            "create index rp45_p06 on rp45_p (a) where d > '2020-1-1'",
            "create index rp45_p07 on rp45_p (a) where ts > '2020-01-01 10:00'",
            "create index rp45_p08 on rp45_p (a) where bo = 'true'",
            "create index rp45_p09 on rp45_p (a) where u = 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'",
            "create index rp45_p10 on rp45_p (a) where c = 'ab'",
            "create index rp45_p11 on rp45_p (a) where nm = 'x'",
            "create index rp45_p12 on rp45_p (a) where iv > '1 day'",
            "create index rp45_p13 on rp45_p (a) where b <> 'b'",
            "create index rp45_p14 on rp45_p (a) where b in ('a', 'b')",
            "create index rp45_p15 on rp45_p (a) where t not in ('x')",
            "create index rp45_p16 on rp45_p (a) where a in (1, 2)",
            "select regexp_replace(indexdef, '^.*WHERE ', '') from pg_indexes"
            " where tablename = 'rp45_p' order by indexname",
            "select count(*) from pg_depend where objid = 'rp45_p13'::regclass"
            " and refclassid = 'pg_type'::regclass",
            "drop table rp45_p",
            "drop type rp45_e",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a range's constructors, a collation's columns and an extension's type": [
        "CREATE TYPE",
        [
            ("rp45_r(integer,integer)", "rp45_r", "False", "range_constructor2", "-"),
            ("rp45_r(integer,integer,text)", "rp45_r", "False", "range_constructor3", "-"),
            ("rp45_r_multirange()", "rp45_r_multirange", "True", "multirange_constructor0", "-"),
            (
                "rp45_r_multirange(rp45_r)",
                "rp45_r_multirange",
                "True",
                "multirange_constructor1",
                "-",
            ),
            (
                "rp45_r_multirange(rp45_r[])",
                "rp45_r_multirange",
                "True",
                "multirange_constructor2",
                "rp45_r",
            ),
        ],
        [
            ("rp45_r(integer,integer)", "pg_namespace", "n"),
            ("rp45_r(integer,integer)", "pg_type", "i"),
            ("rp45_r(integer,integer)", "pg_type", "n"),
            ("rp45_r(integer,integer,text)", "pg_namespace", "n"),
            ("rp45_r(integer,integer,text)", "pg_type", "i"),
            ("rp45_r(integer,integer,text)", "pg_type", "n"),
            ("rp45_r_multirange()", "pg_namespace", "n"),
            ("rp45_r_multirange()", "pg_type", "i"),
            ("rp45_r_multirange()", "pg_type", "n"),
            ("rp45_r_multirange(rp45_r)", "pg_namespace", "n"),
            ("rp45_r_multirange(rp45_r)", "pg_type", "i"),
            ("rp45_r_multirange(rp45_r)", "pg_type", "n"),
            ("rp45_r_multirange(rp45_r)", "pg_type", "n"),
            ("rp45_r_multirange(rp45_r[])", "pg_namespace", "n"),
            ("rp45_r_multirange(rp45_r[])", "pg_type", "i"),
            ("rp45_r_multirange(rp45_r[])", "pg_type", "n"),
            ("rp45_r_multirange(rp45_r[])", "pg_type", "n"),
        ],
        [("rp45_r", "rp45_r_multirange", "rp45_r_multirange(rp45_r)", "e")],
        [("[1,5)", "{[1,2),[4,5)}")],
        "DROP TYPE",
        "CREATE COLLATION",
        "CREATE TABLE",
        [("pg_collation", "n")],
        "DROP TABLE",
        "DROP COLLATION",
        "CREATE EXTENSION",
        [("pg_namespace", "n")],
        [("1",)],
        "DROP EXTENSION",
    ],
    "an extension's members": [
        "CREATE EXTENSION",
        [
            ("pg_cast", "8"),
            ("pg_opclass", "3"),
            ("pg_operator", "26"),
            ("pg_opfamily", "3"),
            ("pg_proc", "47"),
            ("pg_type", "1"),
        ],
        "DROP EXTENSION",
    ],
    "an index predicate's constants are typed": [
        "CREATE TYPE",
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        [
            ("(a = 5)",),
            ("(i2 = '5'::smallint)",),
            ("(i8 = '5'::bigint)",),
            ("(n = 1.50)",),
            ("(f8 = '1.5'::double precision)",),
            ("(d > '2020-01-01'::date)",),
            ("(ts > '2020-01-01 10:00:00'::timestamp without time zone)",),
            ("(bo = true)",),
            ("(u = 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid)",),
            ("(c = 'ab'::bpchar)",),
            ("(nm = 'x'::name)",),
            ("(iv > '1 day'::interval)",),
            ("(b <> 'b'::rp45_e)",),
            ("(b = ANY (ARRAY['a'::rp45_e, 'b'::rp45_e]))",),
            ("(t <> 'x'::text)",),
            ("(a = ANY (ARRAY[1, 2]))",),
        ],
        [("1",)],
        "DROP TABLE",
        "DROP TYPE",
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
