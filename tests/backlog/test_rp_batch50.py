"""Batch 50 of the Rust PG server (2026-10-23): the regression tests of
RP-099 (ANALYZE builds extended statistics -- ndistinct, dependencies, MCV
lists, expression statistics -- and `pg_statistic_ext_data`, `pg_stats_ext`
and `pg_stats_ext_exprs` show them; its first test is
`test_rp_batch26.py::test_scenario[ANALYZE computes extended statistics]`)
RP-197 (`pg_get_statisticsobjdef` and its siblings were refused) and
RP-198 (a `real` cast to text printed as the double it is kept in), both
found with it, and the gap tests of what is left of RP-099.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster (C
collation) by running this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch50.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario runs in a
transaction it rolls back. No answer prints an oid.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_EXT = (
    "select statistics_name, attnames::text, exprs::text, kinds::text, inherited, "
    "n_distinct::text, dependencies::text, most_common_vals::text, "
    "most_common_val_nulls::text, most_common_freqs::text, most_common_base_freqs::text "
    "from pg_stats_ext where tablename = '{}' order by 1, 5"
)
_EXPRS = (
    "select statistics_name, expr, null_frac, avg_width, n_distinct, "
    "most_common_vals::text, most_common_freqs::text, histogram_bounds::text, correlation "
    "from pg_stats_ext_exprs where tablename = '{}' order by 1, 2"
)
_DATA = (
    "select s.stxname, d.stxdinherit, d.stxdndistinct::text, d.stxddependencies::text, "
    "d.stxdmcv::text, d.stxdexpr::text from pg_statistic_ext s "
    "join pg_statistic_ext_data d on d.stxoid = s.oid where s.stxname = '{}'"
)
_ITEMS = (
    "select m.index, m.values::text, m.nulls::text, m.frequency, m.base_frequency "
    "from pg_statistic_ext s join pg_statistic_ext_data d on d.stxoid = s.oid, "
    "pg_mcv_list_items(d.stxdmcv) m where s.stxname = '{}' order by 1"
)

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ----------------------------------------------- RP-099 what ANALYZE builds
    "ndistinct, dependencies and MCV lists over columns": (
        "RP-099",
        False,
        [
            "begin",
            "create table rp50_a (a int, b int, c text)",
            "insert into rp50_a select g % 4, g % 2, 'v' || (g % 3) from generate_series(1, 40) g",
            "insert into rp50_a values (null, 1, null), (null, 0, 'v1')",
            "create statistics rp50_as on a, b, c from rp50_a",
            "create statistics rp50_ax (mcv) on a, b from rp50_a",
            "analyze rp50_a",
            _EXT.format("rp50_a"),
            _DATA.format("rp50_ax"),
            _ITEMS.format("rp50_ax"),
            "rollback",
        ],
    ),
    "expression statistics": (
        "RP-099",
        False,
        [
            "begin",
            "create table rp50_e (a int, b int, c text)",
            "insert into rp50_e select g % 4, g % 2, 'v' || (g % 3) from generate_series(1, 40) g",
            "insert into rp50_e values (null, 1, null), (null, 0, 'v1')",
            "create statistics rp50_ee on (a + b), (lower(c)) from rp50_e",
            "create statistics rp50_e1 on (a * 2), lower(c) from rp50_e",
            _EXPRS.format("rp50_e"),
            "analyze rp50_e",
            _EXT.format("rp50_e"),
            _EXPRS.format("rp50_e"),
            _DATA.format("rp50_e1"),
            "rollback",
        ],
    ),
    "ANALYZE of some columns builds only what they cover": (
        "RP-099",
        False,
        [
            "begin",
            "create table rp50_p (a int, b int, c text)",
            "insert into rp50_p select g % 3, g % 2, 'x' from generate_series(1, 10) g",
            "create statistics rp50_p1 on a, b from rp50_p",
            "create statistics rp50_p2 (mcv) on a, b from rp50_p",
            "create statistics rp50_p3 (dependencies, mcv) on a, c from rp50_p",
            "create statistics rp50_p4 on (a * 2) from rp50_p",
            "create statistics rp50_p5 (ndistinct) on b, (a + 1), lower(c) from rp50_p",
            _EXT.format("rp50_p"),
            "~analyze rp50_p (a)",
            _EXT.format("rp50_p"),
            "analyze rp50_p",
            _EXT.format("rp50_p"),
            _EXPRS.format("rp50_p"),
            "alter statistics rp50_p2 rename to rp50_p2r",
            "drop statistics rp50_p1",
            "select statistics_name from pg_stats_ext where tablename = 'rp50_p' order by 1",
            "rollback",
        ],
    ),
    "numeric, wide, empty and a small target": (
        "RP-099",
        False,
        [
            "begin",
            "create table rp50_n (n numeric, a int, w text)",
            "insert into rp50_n select (g % 3) * 1.5, g % 2, case when g = 1 "
            "then repeat('z', 2000) else 'w' || (g % 2) end from generate_series(1, 12) g",
            "create statistics rp50_n1 on n, a from rp50_n",
            "create statistics rp50_n2 on a, w from rp50_n",
            "create table rp50_z (a int, b int)",
            "create statistics rp50_z1 on a, b from rp50_z",
            "analyze rp50_n",
            "analyze rp50_z",
            _EXT.format("rp50_n"),
            _EXT.format("rp50_z"),
            _DATA.format("rp50_n1"),
            "set local default_statistics_target = 2",
            "analyze rp50_n",
            _EXT.format("rp50_n"),
            "rollback",
        ],
    ),
    "an MCV list's bytes over numeric and dates": (
        "RP-099",
        False,
        [
            "begin",
            "create table rp50_m (n numeric, a int, d date)",
            "insert into rp50_m values (0.0, 0, '2020-01-01'), (1.5, 1, '2020-01-02'), "
            "(-12345.678, 0, '1999-12-31'), (100000, 1, '2020-01-01'), "
            "(0.00012, 0, '2020-01-02'), (null, 1, null)",
            "create statistics rp50_m1 (mcv) on n, a from rp50_m",
            "create statistics rp50_m2 (mcv) on d, a from rp50_m",
            "analyze rp50_m",
            _DATA.format("rp50_m1"),
            _ITEMS.format("rp50_m1"),
            _DATA.format("rp50_m2"),
            _ITEMS.format("rp50_m2"),
            "rollback",
        ],
    ),
    "a partitioned table's statistics are inherited": (
        "RP-099",
        False,
        [
            "begin",
            "create table rp50_t (a int, b int) partition by range (a)",
            "create table rp50_t1 partition of rp50_t for values from (0) to (10)",
            "insert into rp50_t select g % 10, g % 2 from generate_series(1, 20) g",
            "create statistics rp50_ts on a, b from rp50_t",
            "analyze rp50_t",
            _EXT.format("rp50_t"),
            "rollback",
        ],
    ),
    # --------------------------------------- RP-197 pg_get_statisticsobjdef
    "pg_get_statisticsobjdef and its siblings": (
        "RP-197",
        False,
        [
            "begin",
            "create table rp50_d (a int, b int, c text)",
            "create statistics rp50_d1 on a, b from rp50_d",
            "create statistics rp50_d2 (mcv) on a, b from rp50_d",
            "create statistics rp50_d3 (dependencies, mcv) on a, c from rp50_d",
            "create statistics rp50_d4 on (a * 2) from rp50_d",
            "create statistics rp50_d5 (ndistinct) on b, (a + 1), lower(c) from rp50_d",
            "create statistics rp50_d6 on a, (a + b * c::int), coalesce(c, 'x') from rp50_d",
            "select stxname, pg_get_statisticsobjdef(oid), pg_get_statisticsobjdef_columns(oid), "
            "pg_get_statisticsobjdef_expressions(oid)::text from pg_statistic_ext "
            "where stxname like 'rp50_d%' order by 1",
            "select pg_get_statisticsobjdef(0) is null",
            "rollback",
        ],
    ),
    # ------------------------------------------- RP-198 a real cast to text
    "a real is cast to text as a real": (
        "RP-198",
        False,
        [
            "begin",
            "create table rp50_f (f float4, g float4[])",
            "insert into rp50_f values (0.4, '{0.4,0.3}'), (1e-7, '{1.5e10}')",
            "select f::text, g::text, f::varchar, (f * 1)::text from rp50_f order by f",
            "create table rp50_fq (a int)",
            "insert into rp50_fq select g % 3 from generate_series(1, 10) g",
            "analyze rp50_fq",
            "select null_frac::text, most_common_freqs::text, correlation::text "
            "from pg_stats where tablename = 'rp50_fq'",
            "rollback",
        ],
    ),
    # ----------------------------------------------- RP-099 what is left
    "an MCV list's bytes over timestamps": (
        "RP-099",
        False,
        [
            "begin",
            "create table rp50_s (t timestamp, a int)",
            "insert into rp50_s select timestamp '2020-01-01 10:00' + (g % 2) * interval '1 day', "
            "g % 2 from generate_series(1, 10) g",
            "create statistics rp50_s1 (mcv) on t, a from rp50_s",
            "analyze rp50_s",
            "select d.stxdmcv is null from pg_statistic_ext s join pg_statistic_ext_data d "
            "on d.stxoid = s.oid where s.stxname = 'rp50_s1'",
            "rollback",
        ],
    ),
    "an inheritance parent's statistics are built both ways": (
        "RP-099",
        False,
        [
            "begin",
            "create table rp50_i (a int, b int)",
            "create table rp50_ic () inherits (rp50_i)",
            "insert into rp50_i select g % 2, g % 3 from generate_series(1, 10) g",
            "insert into rp50_ic select g % 2, g % 3 from generate_series(1, 10) g",
            "create statistics rp50_is on a, b from rp50_i",
            "analyze rp50_i",
            "select statistics_name, inherited, n_distinct::text from pg_stats_ext "
            "where tablename = 'rp50_i' order by 2",
            "rollback",
        ],
    ),
    "a table past ANALYZE's sample has extended statistics": (
        "RP-099",
        False,
        [
            "begin",
            "create table rp50_b (a int, b int)",
            "insert into rp50_b select g % 4, g % 2 from generate_series(1, 31000) g",
            "create statistics rp50_bs (ndistinct) on a, b from rp50_b",
            "analyze rp50_b",
            "select n_distinct::text from pg_stats_ext where tablename = 'rp50_b'",
            "rollback",
        ],
    ),
    # The sample is random, so its most common values' order (by their
    # sampled counts) is too: they are compared as a set.
    "a table past ANALYZE's sample has column statistics": (
        "RP-099",
        True,
        [
            "begin",
            "create table rp50_c (a int, b int)",
            "insert into rp50_c select g % 4, g % 2 from generate_series(1, 31000) g",
            "analyze rp50_c",
            "select attname, null_frac, n_distinct, (select array_agg(v order by v)"
            " from unnest(most_common_vals::text::int[]) v)::text"
            " from pg_stats where tablename = 'rp50_c' order by 1",
            "rollback",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "ANALYZE of some columns builds only what they cover": [
        "BEGIN",
        "CREATE TABLE",
        "INSERT 0 10",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        [],
        (
            "ANALYZE",
            [
                'statistics object "public.rp50_p1" '
                "could not be computed for relation "
                '"public.rp50_p"',
                'statistics object "public.rp50_p2" '
                "could not be computed for relation "
                '"public.rp50_p"',
                'statistics object "public.rp50_p3" '
                "could not be computed for relation "
                '"public.rp50_p"',
                'statistics object "public.rp50_p5" '
                "could not be computed for relation "
                '"public.rp50_p"',
            ],
        ),
        [
            (
                "rp50_p4",
                "None",
                '{"(a * 2)"}',
                "{e}",
                "False",
                "None",
                "None",
                "None",
                "None",
                "None",
                "None",
            )
        ],
        "ANALYZE",
        [
            (
                "rp50_p1",
                "{a,b}",
                "None",
                "{d,f,m}",
                "False",
                '{"1, 2": 6}',
                "None",
                "{{0,1},{1,0},{1,1},{2,0},{0,0},{2,1}}",
                "{{f,f},{f,f},{f,f},{f,f},{f,f},{f,f}}",
                "{0.2,0.2,0.2,0.2,0.1,0.1}",
                "{0.15,0.2,0.2,0.15,0.15,0.15}",
            ),
            (
                "rp50_p2",
                "{a,b}",
                "None",
                "{m}",
                "False",
                "None",
                "None",
                "{{0,1},{1,0},{1,1},{2,0},{0,0},{2,1}}",
                "{{f,f},{f,f},{f,f},{f,f},{f,f},{f,f}}",
                "{0.2,0.2,0.2,0.2,0.1,0.1}",
                "{0.15,0.2,0.2,0.15,0.15,0.15}",
            ),
            (
                "rp50_p3",
                "{a,c}",
                "None",
                "{f,m}",
                "False",
                "None",
                '{"1 => 3": 1.000000}',
                "{{1,x},{0,x},{2,x}}",
                "{{f,f},{f,f},{f,f}}",
                "{0.4,0.3,0.3}",
                "{0.4,0.3,0.3}",
            ),
            (
                "rp50_p4",
                "None",
                '{"(a * 2)"}',
                "{e}",
                "False",
                "None",
                "None",
                "None",
                "None",
                "None",
                "None",
            ),
            (
                "rp50_p5",
                "{b}",
                '{"(a + 1)",lower(c)}',
                "{d,e}",
                "False",
                '{"2, -1": 6, "2, -2": 2, "-1, -2": 3, "2, -1, -2": 6}',
                "None",
                "None",
                "None",
                "None",
                "None",
            ),
        ],
        [
            ("rp50_p4", "(a * 2)", "0.0", "4", "-0.3", "{2,0,4}", "{0.4,0.3,0.3}", "None", "0.2"),
            ("rp50_p5", "(a + 1)", "0.0", "4", "-0.3", "{2,1,3}", "{0.4,0.3,0.3}", "None", "0.2"),
            ("rp50_p5", "lower(c)", "0.0", "5", "1.0", "{x}", "{1}", "None", "1.0"),
        ],
        "ALTER STATISTICS",
        "DROP STATISTICS",
        [("rp50_p2r",), ("rp50_p3",), ("rp50_p4",), ("rp50_p5",)],
        "ROLLBACK",
    ],
    "a partitioned table's statistics are inherited": [
        "BEGIN",
        "CREATE TABLE",
        "CREATE TABLE",
        "INSERT 0 20",
        "CREATE STATISTICS",
        "ANALYZE",
        [
            (
                "rp50_ts",
                "{a,b}",
                "None",
                "{d,f,m}",
                "True",
                '{"1, 2": 10}',
                '{"1 => 2": 1.000000}',
                "{{0,0},{1,1},{2,0},{3,1},{4,0},{5,1},{6,0},{7,1},{8,0},{9,1}}",
                "{{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{f,f}}",
                "{0.1,0.1,0.1,0.1,0.1,0.1,0.1,0.1,0.1,0.1}",
                "{0.05,0.05,0.05,0.05,0.05,0.05,0.05,0.05,0.05,0.05}",
            )
        ],
        "ROLLBACK",
    ],
    "a real is cast to text as a real": [
        "BEGIN",
        "CREATE TABLE",
        "INSERT 0 2",
        ("ERR", "42702", 'ORDER BY "f" is ambiguous', None),
        (
            "ERR",
            "25P02",
            "current transaction is aborted, commands ignored until end of transaction block",
            None,
        ),
        (
            "ERR",
            "25P02",
            "current transaction is aborted, commands ignored until end of transaction block",
            None,
        ),
        (
            "ERR",
            "25P02",
            "current transaction is aborted, commands ignored until end of transaction block",
            None,
        ),
        (
            "ERR",
            "25P02",
            "current transaction is aborted, commands ignored until end of transaction block",
            None,
        ),
        "ROLLBACK",
    ],
    "a table past ANALYZE's sample has extended statistics": [
        "BEGIN",
        "CREATE TABLE",
        "INSERT 0 31000",
        "CREATE STATISTICS",
        "ANALYZE",
        [('{"1, 2": 4}',)],
        "ROLLBACK",
    ],
    "a table past ANALYZE's sample has column statistics": [
        "BEGIN",
        "CREATE TABLE",
        "INSERT 0 31000",
        "ANALYZE",
        [("a", "0.0", "4.0", "{0,1,2,3}"), ("b", "0.0", "2.0", "{0,1}")],
        "ROLLBACK",
    ],
    "an MCV list's bytes over numeric and dates": [
        "BEGIN",
        "CREATE TABLE",
        "INSERT 0 6",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        "ANALYZE",
        [
            (
                "rp50_m1",
                "False",
                "None",
                "None",
                "\\xc251a6e101000000060000000200a406000017000000050000002e00000040000000ffffffff0000000002000000080000000000000004000000010000000800000081a1010029097c1a02000000808006000000ff820100d007060000008080010088130400000001800a0000000000010000000000555555555555c53f555555555555b53f000000000000555555555555c53f555555555555b53f010000000000555555555555c53f555555555555b53f020000000000555555555555c53f555555555555b53f030001000000555555555555c53f555555555555b53f040001000100555555555555c53f555555555555b53f00000100",
                "None",
            )
        ],
        [
            ("0", "{-12345.678,0}", "{f,f}", "0.16666666666666666", "0.08333333333333333"),
            ("1", "{0.0,0}", "{f,f}", "0.16666666666666666", "0.08333333333333333"),
            ("2", "{0.00012,0}", "{f,f}", "0.16666666666666666", "0.08333333333333333"),
            ("3", "{1.5,1}", "{f,f}", "0.16666666666666666", "0.08333333333333333"),
            ("4", "{100000,1}", "{f,f}", "0.16666666666666666", "0.08333333333333333"),
            ("5", "{NULL,1}", "{t,f}", "0.16666666666666666", "0.08333333333333333"),
        ],
        [
            (
                "rp50_m2",
                "False",
                "None",
                "None",
                "\\xc251a6e101000000060000000200170000003a0400000200000008000000000000000400000001000000030000000c0000000000000004000000010000000000000001000000ffffffff891c00008a1c00000000555555555555c53f555555555555b53f000000000000555555555555c53f555555555555c53f000001000000555555555555c53f555555555555c53f000002000000555555555555c53f555555555555c53f010001000000555555555555c53f555555555555c53f010002000001555555555555c53f555555555555b53f01000000",
                "None",
            )
        ],
        [
            ("0", "{0,1999-12-31}", "{f,f}", "0.16666666666666666", "0.08333333333333333"),
            ("1", "{0,2020-01-01}", "{f,f}", "0.16666666666666666", "0.16666666666666666"),
            ("2", "{0,2020-01-02}", "{f,f}", "0.16666666666666666", "0.16666666666666666"),
            ("3", "{1,2020-01-01}", "{f,f}", "0.16666666666666666", "0.16666666666666666"),
            ("4", "{1,2020-01-02}", "{f,f}", "0.16666666666666666", "0.16666666666666666"),
            ("5", "{1,NULL}", "{f,t}", "0.16666666666666666", "0.08333333333333333"),
        ],
        "ROLLBACK",
    ],
    "an MCV list's bytes over timestamps": [
        "BEGIN",
        "CREATE TABLE",
        "INSERT 0 10",
        "CREATE STATISTICS",
        "ANALYZE",
        [("False",)],
        "ROLLBACK",
    ],
    "an inheritance parent's statistics are built both ways": [
        "BEGIN",
        "CREATE TABLE",
        "CREATE TABLE",
        "INSERT 0 10",
        "INSERT 0 10",
        "CREATE STATISTICS",
        "ANALYZE",
        [("rp50_is", "False", '{"1, 2": 6}'), ("rp50_is", "True", '{"1, 2": 6}')],
        "ROLLBACK",
    ],
    "expression statistics": [
        "BEGIN",
        "CREATE TABLE",
        "INSERT 0 40",
        "INSERT 0 2",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        [
            ("rp50_e1", "(a * 2)", "None", "None", "None", "None", "None", "None", "None"),
            ("rp50_e1", "lower(c)", "None", "None", "None", "None", "None", "None", "None"),
            ("rp50_ee", "(a + b)", "None", "None", "None", "None", "None", "None", "None"),
            ("rp50_ee", "lower(c)", "None", "None", "None", "None", "None", "None", "None"),
        ],
        "ANALYZE",
        [
            (
                "rp50_e1",
                "None",
                '{"(a * 2)",lower(c)}',
                "{d,f,m,e}",
                "False",
                '{"-1, -2": 14}',
                '{"-2 => -1": 0.023810}',
                "{{4,v2},{6,v0},{2,v1},{0,v1},{4,v0},{4,v1},{6,v1},{6,v2},{0,v0},{0,v2},{2,v0},{2,v2},{NULL,v1},{NULL,NULL}}",
                "{{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{t,f},{t,t}}",
                "{0.09523809523809523,0.09523809523809523,0.09523809523809523,0.09523809523809523,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.023809523809523808,0.023809523809523808}",
                "{0.07369614512471655,0.07369614512471655,0.08503401360544217,0.08503401360544217,0.07369614512471655,0.08503401360544217,0.08503401360544217,0.07369614512471655,0.07369614512471655,0.07369614512471655,0.07369614512471655,0.07369614512471655,0.017006802721088433,0.0011337868480725622}",
            ),
            (
                "rp50_ee",
                "None",
                '{"(a + b)",lower(c)}',
                "{d,f,m,e}",
                "False",
                '{"-1, -2": 11}',
                '{"-2 => -1": 0.023810}',
                "{{2,v2},{2,v1},{2,v0},{4,v0},{0,v1},{0,v0},{0,v2},{4,v1},{4,v2},{NULL,v1},{NULL,NULL}}",
                "{{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{f,f},{t,f},{t,t}}",
                "{0.16666666666666666,0.16666666666666666,0.14285714285714285,0.09523809523809523,0.09523809523809523,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.023809523809523808,0.023809523809523808}",
                "{0.1473922902494331,0.17006802721088435,0.1473922902494331,0.07369614512471655,0.08503401360544217,0.07369614512471655,0.07369614512471655,0.08503401360544217,0.07369614512471655,0.017006802721088433,0.0011337868480725622}",
            ),
        ],
        [
            (
                "rp50_e1",
                "(a * 2)",
                "0.04761905",
                "4",
                "4.0",
                "{0,2,4,6}",
                "{0.23809524,0.23809524,0.23809524,0.23809524}",
                "None",
                "0.22889306",
            ),
            (
                "rp50_e1",
                "lower(c)",
                "0.023809524",
                "6",
                "3.0",
                "{v1,v0,v2}",
                "{0.35714287,0.30952382,0.30952382}",
                "None",
                "0.30243903",
            ),
            (
                "rp50_ee",
                "(a + b)",
                "0.04761905",
                "4",
                "3.0",
                "{2,0,4}",
                "{0.47619048,0.23809524,0.23809524}",
                "None",
                "0.34427768",
            ),
            (
                "rp50_ee",
                "lower(c)",
                "0.023809524",
                "6",
                "3.0",
                "{v1,v0,v2}",
                "{0.35714287,0.30952382,0.30952382}",
                "None",
                "0.30243903",
            ),
        ],
        [
            (
                "rp50_e1",
                "False",
                '{"-1, -2": 14}',
                '{"-2 => -1": 0.023810}',
                "\\xc251a6e1010000000e000000020017000000190000000400000010000000000000000400000001000000030000001200000018000000ffffffff00000000000000000200000004000000060000000200000076300200000076310200000076320000188661188661b83fb7e42625c0ddb23f020002000000188661188661b83fb7e42625c0ddb23f030000000000188661188661b83f71a57b03cac4b53f010001000000188661188661b83f71a57b03cac4b53f000001000000922449922449b23fb7e42625c0ddb23f020000000000922449922449b23f71a57b03cac4b53f020001000000922449922449b23f71a57b03cac4b53f030001000000922449922449b23fb7e42625c0ddb23f030002000000922449922449b23fb7e42625c0ddb23f000000000000922449922449b23fb7e42625c0ddb23f000002000000922449922449b23fb7e42625c0ddb23f010000000000922449922449b23fb7e42625c0ddb23f010002000100188661188661983f5a84fc353b6a913f000001000101188661188661983fa404b85b7293523f00000000",
                '{"(0,0,f,0.04761905,4,4,1,3,0,0,0,96,97,0,0,0,0,0,0,0,0,\\"{0.23809524,0.23809524,0.23809524,0.23809524}\\",{0.22889306},,,,\\"{0,2,4,6}\\",,,,)","(0,0,f,0.023809524,6,3,1,3,0,0,0,98,664,0,0,0,100,100,0,0,0,\\"{0.35714287,0.30952382,0.30952382}\\",{0.30243903},,,,\\"{v1,v0,v2}\\",,,,)"}',
            )
        ],
        "ROLLBACK",
    ],
    "ndistinct, dependencies and MCV lists over columns": [
        "BEGIN",
        "CREATE TABLE",
        "INSERT 0 40",
        "INSERT 0 2",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        "ANALYZE",
        [
            (
                "rp50_as",
                "{a,b,c}",
                "None",
                "{d,f,m}",
                "False",
                '{"1, 2": 6, "1, 3": 14, "2, 3": 7, "1, 2, 3": 14}',
                '{"1 => 2": 0.952381, "3 => 1": '
                '0.023810, "3 => 2": 0.023810, "1, 2 => '
                '3": 0.047619, "1, 3 => 2": 1.000000, '
                '"2, 3 => 1": 0.023810}',
                "{{2,0,v2},{3,1,v0},{1,1,v1},{0,0,v1},{2,0,v0},{2,0,v1},{3,1,v1},{3,1,v2},{0,0,v0},{0,0,v2},{1,1,v0},{1,1,v2},{NULL,0,v1},{NULL,1,NULL}}",
                "{{f,f,f},{f,f,f},{f,f,f},{f,f,f},{f,f,f},{f,f,f},{f,f,f},{f,f,f},{f,f,f},{f,f,f},{f,f,f},{f,f,f},{t,f,f},{t,f,t}}",
                "{0.09523809523809523,0.09523809523809523,0.09523809523809523,0.09523809523809523,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.07142857142857142,0.023809523809523808,0.023809523809523808}",
                "{0.036848072562358274,0.036848072562358274,0.04251700680272109,0.04251700680272109,0.036848072562358274,0.04251700680272109,0.04251700680272109,0.036848072562358274,0.036848072562358274,0.036848072562358274,0.036848072562358274,0.036848072562358274,0.008503401360544217,0.0005668934240362811}",
            ),
            (
                "rp50_ax",
                "{a,b}",
                "None",
                "{m}",
                "False",
                "None",
                "None",
                "{{0,0},{1,1},{2,0},{3,1},{NULL,0},{NULL,1}}",
                "{{f,f},{f,f},{f,f},{f,f},{t,f},{t,f}}",
                "{0.23809523809523808,0.23809523809523808,0.23809523809523808,0.23809523809523808,0.023809523809523808,0.023809523809523808}",
                "{0.11904761904761904,0.11904761904761904,0.11904761904761904,0.11904761904761904,0.023809523809523808,0.023809523809523808}",
            ),
        ],
        [
            (
                "rp50_ax",
                "False",
                "None",
                "None",
                "\\xc251a6e10100000006000000020017000000170000000400000010000000000000000400000001000000020000000800000000000000040000000100000000000000010000000200000003000000000000000100000000009ee7799ee779ce3f9ee7799ee779be3f0000000000009ee7799ee779ce3f9ee7799ee779be3f0100010000009ee7799ee779ce3f9ee7799ee779be3f0200000000009ee7799ee779ce3f9ee7799ee779be3f030001000100188661188661983f188661188661983f000000000100188661188661983f188661188661983f00000100",
                "None",
            )
        ],
        [
            ("0", "{0,0}", "{f,f}", "0.23809523809523808", "0.11904761904761904"),
            ("1", "{1,1}", "{f,f}", "0.23809523809523808", "0.11904761904761904"),
            ("2", "{2,0}", "{f,f}", "0.23809523809523808", "0.11904761904761904"),
            ("3", "{3,1}", "{f,f}", "0.23809523809523808", "0.11904761904761904"),
            ("4", "{NULL,0}", "{t,f}", "0.023809523809523808", "0.023809523809523808"),
            ("5", "{NULL,1}", "{t,f}", "0.023809523809523808", "0.023809523809523808"),
        ],
        "ROLLBACK",
    ],
    "numeric, wide, empty and a small target": [
        "BEGIN",
        "CREATE TABLE",
        "INSERT 0 12",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        "CREATE TABLE",
        "CREATE STATISTICS",
        "ANALYZE",
        "ANALYZE",
        [
            (
                "rp50_n1",
                "{n,a}",
                "None",
                "{d,f,m}",
                "False",
                '{"1, 2": 6}',
                "None",
                "{{0.0,0},{0.0,1},{1.5,0},{1.5,1},{3.0,0},{3.0,1}}",
                "{{f,f},{f,f},{f,f},{f,f},{f,f},{f,f}}",
                "{0.16666666666666666,0.16666666666666666,0.16666666666666666,0.16666666666666666,0.16666666666666666,0.16666666666666666}",
                "{0.16666666666666666,0.16666666666666666,0.16666666666666666,0.16666666666666666,0.16666666666666666,0.16666666666666666}",
            ),
            (
                "rp50_n2",
                "{a,w}",
                "None",
                "{d,f,m}",
                "False",
                '{"2, 3": 3}',
                '{"2 => 3": 0.916667, "3 => 2": 0.916667}',
                "{{0,w0},{1,w1}}",
                "{{f,f},{f,f}}",
                "{0.5,0.4166666666666667}",
                "{0.25,0.17361111111111113}",
            ),
        ],
        [],
        [
            (
                "rp50_n1",
                "False",
                '{"1, 2": 6}',
                "None",
                "\\xc251a6e101000000060000000200a406000017000000030000001800000020000000ffffffff00000000020000000800000000000000040000000100000002000000808006000000808001008813040000008080030000000000010000000000555555555555c53f555555555555c53f000000000000555555555555c53f555555555555c53f000001000000555555555555c53f555555555555c53f010000000000555555555555c53f555555555555c53f010001000000555555555555c53f555555555555c53f020000000000555555555555c53f555555555555c53f02000100",
                "None",
            )
        ],
        "SET",
        "ANALYZE",
        [
            (
                "rp50_n1",
                "{n,a}",
                "None",
                "{d,f,m}",
                "False",
                '{"1, 2": 6}',
                "None",
                "{{0.0,0},{0.0,1}}",
                "{{f,f},{f,f}}",
                "{0.16666666666666666,0.16666666666666666}",
                "{0.16666666666666666,0.16666666666666666}",
            ),
            (
                "rp50_n2",
                "{a,w}",
                "None",
                "{d,f,m}",
                "False",
                '{"2, 3": 3}',
                '{"2 => 3": 0.916667, "3 => 2": 0.916667}',
                "{{0,w0},{1,w1}}",
                "{{f,f},{f,f}}",
                "{0.5,0.4166666666666667}",
                "{0.25,0.17361111111111113}",
            ),
        ],
        "ROLLBACK",
    ],
    "pg_get_statisticsobjdef and its siblings": [
        "BEGIN",
        "CREATE TABLE",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        [
            ("rp50_d1", "CREATE STATISTICS public.rp50_d1 ON a, b FROM rp50_d", "a, b", "None"),
            (
                "rp50_d2",
                "CREATE STATISTICS public.rp50_d2 (mcv) ON a, b FROM rp50_d",
                "a, b",
                "None",
            ),
            (
                "rp50_d3",
                "CREATE STATISTICS public.rp50_d3 (dependencies, mcv) ON a, c FROM rp50_d",
                "a, c",
                "None",
            ),
            (
                "rp50_d4",
                "CREATE STATISTICS public.rp50_d4 ON (a * 2) FROM rp50_d",
                "(a * 2)",
                '{"(a * 2)"}',
            ),
            (
                "rp50_d5",
                "CREATE STATISTICS public.rp50_d5 (ndistinct) ON b, (a + 1), lower(c) FROM rp50_d",
                "b, (a + 1), lower(c)",
                '{"(a + 1)",lower(c)}',
            ),
            (
                "rp50_d6",
                "CREATE STATISTICS public.rp50_d6 ON a, (a + b * "
                "c::integer), COALESCE(c, 'x'::text) FROM rp50_d",
                "a, (a + b * c::integer), COALESCE(c, 'x'::text)",
                '{"(a + (b * (c)::integer))","COALESCE(c, \'x\'::text)"}',
            ),
        ],
        [("True",)],
        "ROLLBACK",
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
