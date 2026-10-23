"""Batch 40 of the Rust PG server (2026-10-23): the regression tests of the
part of RP-169 it fixed -- `ALTER EXTENSION ... UPDATE`, `ALTER OPERATOR
... OWNER TO`, `ALTER COLLATION ... RENAME TO` / `REFRESH VERSION`, `CREATE
CONVERSION` and `CREATE ACCESS METHOD` -- and of RP-184 and RP-185, found
there; and the tests of RP-183 and RP-186, found with them and fixed in
batch 41.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch40.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ---------------------------------------------- RP-169 ALTER EXTENSION
    "ALTER EXTENSION UPDATE": (
        "RP-169",
        False,
        [
            "create extension citext",
            "~alter extension citext update",
            "~alter extension citext update to '1.6'",
            "alter extension citext update to '1.5'",
            "alter extension rp40_nosuch update",
            "~alter extension plpgsql update",
            "drop extension citext",
        ],
    ),
    # ------------------------------------------ RP-169 ALTER OPERATOR OWNER
    "ALTER OPERATOR OWNER TO": (
        "RP-169",
        False,
        [
            "create function rp40_f(int, int) returns bool language sql immutable"
            " as 'select $1 < $2'",
            "create operator ===> (leftarg = int, rightarg = int, function = rp40_f)",
            "create role rp40_o",
            "alter operator ===> (int, int) owner to rp40_o",
            "select oprowner::regrole::text from pg_operator where oprname = '===>'",
            "alter operator ===> (int, int) owner to rp40_nosuch",
            "alter operator ===> (text, text) owner to rp40_o",
            "alter operator public.===> (int, int) owner to current_user",
            "select oprowner::regrole::text from pg_operator where oprname = '===>'",
            "drop operator ===> (int, int)",
            "drop function rp40_f(int, int)",
            "drop role rp40_o",
        ],
    ),
    # ---------------------------------------------- RP-169 ALTER COLLATION
    "ALTER COLLATION RENAME and REFRESH VERSION": (
        "RP-169",
        False,
        [
            "create collation rp40_k (provider = icu, locale = 'de-x-icu')",
            "create table rp40_kt (a text collate rp40_k)",
            "alter collation rp40_k rename to rp40_k2",
            "alter collation rp40_k2 rename to rp40_k2",
            "alter collation rp40_nosuch rename to x",
            "create collation rp40_k3 (provider = icu, locale = 'de-x-icu')",
            "alter collation rp40_k3 rename to rp40_k2",
            "~alter collation rp40_k2 refresh version",
            '~alter collation "C" refresh version',
            "alter collation rp40_nosuch refresh version",
            "select collname from pg_collation where collname like 'rp40%' order by 1",
            "select c.collname from pg_attribute a join pg_collation c on c.oid = a.attcollation"
            " where a.attrelid = 'rp40_kt'::regclass and a.attname = 'a'",
            "drop table rp40_kt",
            "drop collation rp40_k2",
            "drop collation rp40_k3",
        ],
    ),
    # --------------------------------------------- RP-169 CREATE CONVERSION
    "CREATE CONVERSION and its errors": (
        "RP-169",
        False,
        [
            "create conversion rp40_c for 'LATIN1' to 'UTF8' from iso8859_1_to_utf8",
            "select conname, connamespace::regnamespace::text, conowner::regrole::text,"
            " pg_encoding_to_char(conforencoding), pg_encoding_to_char(contoencoding),"
            " conproc::text, condefault from pg_conversion where conname = 'rp40_c'",
            "create conversion rp40_c for 'LATIN1' to 'UTF8' from iso8859_1_to_utf8",
            "create conversion rp40_c2 for 'NOSUCH' to 'UTF8' from iso8859_1_to_utf8",
            "create conversion rp40_c2 for 'LATIN1' to 'NOSUCH' from iso8859_1_to_utf8",
            "create conversion rp40_c2 for 'LATIN1' to 'UTF8' from rp40_nosuchfn",
            "create conversion rp40_c2 for 'LATIN1' to 'UTF8' from lower",
            "create conversion rp40_c2 for 'LATIN2' to 'UTF8' from iso8859_1_to_utf8",
            "create conversion rp40_c2 for 'UTF8' to 'LATIN1' from iso8859_1_to_utf8",
            "create default conversion rp40_d1 for 'LATIN1' to 'UTF8' from iso8859_1_to_utf8",
            "create default conversion rp40_d2 for 'LATIN1' to 'UTF8' from iso8859_1_to_utf8",
            "comment on conversion rp40_c is 'c'",
            "select obj_description(oid, 'pg_conversion') from pg_conversion"
            " where conname = 'rp40_c'",
            "create schema rp40_cs",
            "create conversion rp40_cs.c1 for 'latin1' to 'utf8' from iso8859_1_to_utf8",
            "select connamespace::regnamespace::text from pg_conversion where conname = 'c1'",
            "alter conversion rp40_cs.c1 set schema public",
            "alter conversion c1 owner to rp40_nosuch",
            "alter conversion rp40_c rename to rp40_cx",
            "alter conversion rp40_cx rename to c1",
            "drop conversion rp40_cx",
            "drop conversion rp40_cx",
            "~drop conversion if exists rp40_cx",
            "drop conversion rp40_d1, c1",
            "drop schema rp40_cs",
            "select count(*) from pg_conversion where conname like 'rp40%'",
        ],
    ),
    # ------------------------------------------ RP-169 CREATE ACCESS METHOD
    "CREATE ACCESS METHOD and what uses one": (
        "RP-169",
        False,
        [
            "create access method rp40_am type table handler heap_tableam_handler",
            "create access method rp40_am2 type index handler bthandler",
            "select amname, amhandler::text, amtype from pg_am where amname like 'rp40%'"
            " order by 1",
            "create access method rp40_am type table handler heap_tableam_handler",
            "create access method btree type index handler bthandler",
            "create access method rp40_am3 type table handler rp40_nosuch",
            "create access method rp40_am3 type table handler bthandler",
            "create access method rp40_am4 type index handler heap_tableam_handler",
            "create table rp40_u1 (a int[])",
            "create index rp40_u1_i on rp40_u1 using rp40_am2 (a)",
            "create index rp40_u1_i on rp40_u1 using rp40_am2 (a int4_ops)",
            "create index rp40_u1_j on rp40_u1 using heap (a)",
            "create table rp40_u2 (a int) using rp40_am",
            "insert into rp40_u2 values (1)",
            "select * from rp40_u2",
            "create materialized view rp40_mv using rp40_am as select 1 as x",
            "select relname, amname from pg_class c join pg_am a on a.oid = c.relam"
            " where relname in ('rp40_u1', 'rp40_u2', 'rp40_mv') order by 1",
            "!drop access method rp40_am",
            "drop access method heap",
            "drop access method rp40_am2",
            "drop access method rp40_am2",
            "~drop access method if exists rp40_am2",
            "~drop access method rp40_am cascade",
            "select count(*) from pg_class where relname in ('rp40_u2', 'rp40_mv')",
            "drop table rp40_u1",
        ],
    ),
    # ------------------------------- RP-184 USING an access method checked
    "a table USING a missing or index access method": (
        "RP-184",
        False,
        [
            "create table rp40_t (a int) using rp40_nosuch",
            "create table rp40_t (a int) using btree",
            "create table rp40_t as select 1 as a using gin",
            "create table rp40_t (a int) using heap",
            "drop table rp40_t",
        ],
    ),
    # ------------- RP-185 btree / hash over a type with no class for them
    "btree and hash refuse a type with no class for them": (
        "RP-185",
        False,
        [
            "create table rp40_h (a box, b json, c money, d xid)",
            "create index on rp40_h (a)",
            "create index on rp40_h using hash (b)",
            "create index on rp40_h using hash (c)",
            "create index on rp40_h (d)",
            "create index on rp40_h using hash (d)",
            "drop table rp40_h",
        ],
    ),
    # -------------------------- RP-186 the statistics types as column types
    "the statistics types are types": (
        "RP-186",
        False,
        [
            "create table rp40_st (a pg_ndistinct, b pg_dependencies, c pg_mcv_list,"
            " d pg_brin_bloom_summary, e pg_brin_minmax_multi_summary)",
            "select format_type(atttypid, null) from pg_attribute"
            " where attrelid = 'rp40_st'::regclass and attnum > 0 order by attnum",
            "drop table rp40_st",
        ],
    ),
    # ------------------- RP-183 an operator class of another type
    "an operator class for another type": (
        "RP-183",
        False,
        [
            "create table rp40_o (a int[])",
            "create index on rp40_o using btree (a jsonb_ops)",
            "drop table rp40_o",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "ALTER COLLATION RENAME and REFRESH VERSION": [
        "CREATE COLLATION",
        "CREATE TABLE",
        "ALTER COLLATION",
        ("ERR", "42710", 'collation "rp40_k2" already exists in schema "public"', None),
        ("ERR", "42704", 'collation "rp40_nosuch" for encoding "UTF8" does not exist', None),
        "CREATE COLLATION",
        ("ERR", "42710", 'collation "rp40_k2" already exists in schema "public"', None),
        ("ALTER COLLATION", ["version has not changed"]),
        ("ALTER COLLATION", ["version has not changed"]),
        ("ERR", "42704", 'collation "rp40_nosuch" for encoding "UTF8" does not exist', None),
        [("rp40_k2",), ("rp40_k3",)],
        [("rp40_k2",)],
        "DROP TABLE",
        "DROP COLLATION",
        "DROP COLLATION",
    ],
    "ALTER EXTENSION UPDATE": [
        "CREATE EXTENSION",
        ("ALTER EXTENSION", ['version "1.6" of extension "citext" is already installed']),
        ("ALTER EXTENSION", ['version "1.6" of extension "citext" is already installed']),
        (
            "ERR",
            "22023",
            'extension "citext" has no update path from version "1.6" to version "1.5"',
            None,
        ),
        ("ERR", "42704", 'extension "rp40_nosuch" does not exist', None),
        ("ALTER EXTENSION", ['version "1.0" of extension "plpgsql" is already installed']),
        "DROP EXTENSION",
    ],
    "ALTER OPERATOR OWNER TO": [
        "CREATE FUNCTION",
        "CREATE OPERATOR",
        "CREATE ROLE",
        "ALTER OPERATOR",
        [("rp40_o",)],
        ("ERR", "42704", 'role "rp40_nosuch" does not exist', None),
        ("ERR", "42883", "operator does not exist: text ===> text", None),
        "ALTER OPERATOR",
        [("postgres",)],
        "DROP OPERATOR",
        "DROP FUNCTION",
        "DROP ROLE",
    ],
    "CREATE ACCESS METHOD and what uses one": [
        "CREATE ACCESS METHOD",
        "CREATE ACCESS METHOD",
        [("rp40_am", "heap_tableam_handler", "t"), ("rp40_am2", "bthandler", "i")],
        ("ERR", "42710", 'access method "rp40_am" already exists', None),
        ("ERR", "42710", 'access method "btree" already exists', None),
        ("ERR", "42883", "function rp40_nosuch(internal) does not exist", None),
        ("ERR", "42809", "function bthandler must return type table_am_handler", None),
        ("ERR", "42809", "function heap_tableam_handler must return type index_am_handler", None),
        "CREATE TABLE",
        (
            "ERR",
            "42704",
            'data type integer[] has no default operator class for access method "rp40_am2"',
            "You must specify an operator class for the index or "
            "define a default operator class for the data type.",
        ),
        (
            "ERR",
            "42704",
            'operator class "int4_ops" does not exist for access method "rp40_am2"',
            None,
        ),
        (
            "ERR",
            "XX000",
            "index access method handler function 3 did not return an IndexAmRoutine struct",
            None,
        ),
        "CREATE TABLE",
        "INSERT 0 1",
        [("1",)],
        "SELECT 1",
        [("rp40_mv", "rp40_am"), ("rp40_u1", "heap"), ("rp40_u2", "rp40_am")],
        (
            "ERR",
            "2BP01",
            "cannot drop access method rp40_am because other objects depend on it",
            "Use DROP ... CASCADE to drop the dependent objects too.",
            "table rp40_u2 depends on access method rp40_am\n"
            "materialized view rp40_mv depends on access method "
            "rp40_am",
        ),
        (
            "ERR",
            "2BP01",
            "cannot drop access method heap because it is required by the database system",
            None,
        ),
        "DROP ACCESS METHOD",
        ("ERR", "42704", 'access method "rp40_am2" does not exist', None),
        ("DROP ACCESS METHOD", ['access method "rp40_am2" does not exist, skipping']),
        (
            "DROP ACCESS METHOD",
            [
                "drop cascades to 2 other objects\n"
                "drop cascades to table rp40_u2\n"
                "drop cascades to materialized view rp40_mv"
            ],
        ),
        [("0",)],
        "DROP TABLE",
    ],
    "CREATE CONVERSION and its errors": [
        "CREATE CONVERSION",
        [("rp40_c", "public", "postgres", "LATIN1", "UTF8", "iso8859_1_to_utf8", "False")],
        ("ERR", "42710", 'conversion "rp40_c" already exists', None),
        ("ERR", "42704", 'source encoding "NOSUCH" does not exist', None),
        ("ERR", "42704", 'destination encoding "NOSUCH" does not exist', None),
        (
            "ERR",
            "42883",
            "function rp40_nosuchfn(integer, integer, cstring, "
            "internal, integer, boolean) does not exist",
            None,
        ),
        (
            "ERR",
            "42883",
            "function lower(integer, integer, cstring, internal, integer, boolean) does not exist",
            None,
        ),
        ("ERR", "XX000", 'expected source encoding "LATIN1", but got "LATIN2"', None),
        ("ERR", "XX000", 'expected source encoding "LATIN1", but got "UTF8"', None),
        "CREATE CONVERSION",
        ("ERR", "42710", "default conversion for LATIN1 to UTF8 already exists", None),
        "COMMENT",
        [("c",)],
        "CREATE SCHEMA",
        "CREATE CONVERSION",
        [("rp40_cs",)],
        "ALTER CONVERSION",
        ("ERR", "42704", 'role "rp40_nosuch" does not exist', None),
        "ALTER CONVERSION",
        ("ERR", "42710", 'conversion "c1" already exists in schema "public"', None),
        "DROP CONVERSION",
        ("ERR", "42704", 'conversion "rp40_cx" does not exist', None),
        ("DROP CONVERSION", ['conversion "rp40_cx" does not exist, skipping']),
        "DROP CONVERSION",
        "DROP SCHEMA",
        [("0",)],
    ],
    "a table USING a missing or index access method": [
        ("ERR", "42704", 'access method "rp40_nosuch" does not exist', None),
        ("ERR", "55000", 'access method "btree" is not of type TABLE', None),
        ("ERR", "42601", 'syntax error at or near "using"', None),
        "CREATE TABLE",
        "DROP TABLE",
    ],
    "an operator class for another type": [
        "CREATE TABLE",
        ("ERR", "42804", 'operator class "jsonb_ops" does not accept data type integer[]', None),
        "DROP TABLE",
    ],
    "btree and hash refuse a type with no class for them": [
        "CREATE TABLE",
        (
            "ERR",
            "42704",
            'data type box has no default operator class for access method "btree"',
            "You must specify an operator class for "
            "the index or define a default operator "
            "class for the data type.",
        ),
        (
            "ERR",
            "42704",
            'data type json has no default operator class for access method "hash"',
            "You must specify an operator class for "
            "the index or define a default operator "
            "class for the data type.",
        ),
        (
            "ERR",
            "42704",
            'data type money has no default operator class for access method "hash"',
            "You must specify an operator class for "
            "the index or define a default operator "
            "class for the data type.",
        ),
        (
            "ERR",
            "42704",
            'data type xid has no default operator class for access method "btree"',
            "You must specify an operator class for "
            "the index or define a default operator "
            "class for the data type.",
        ),
        "CREATE INDEX",
        "DROP TABLE",
    ],
    "the statistics types are types": [
        "CREATE TABLE",
        [
            ("pg_ndistinct",),
            ("pg_dependencies",),
            ("pg_mcv_list",),
            ("pg_brin_bloom_summary",),
            ("pg_brin_minmax_multi_summary",),
        ],
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
