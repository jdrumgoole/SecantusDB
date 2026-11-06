"""EXPLAIN's costs, rows and widths on the Rust PG server (RP-314 and the
plan node kinds filed after it).

Joe decided on 2026-11-02 to withdraw the "Scope decision -- EXPLAIN" that
printed every cost as zero: each plan node prints PostgreSQL's estimate.
The first slice prices a plan that is one relation's sequential scan (user
tables and catalogs alike); each other node kind is its own item with a
scenario here, asserting PostgreSQL 15.19's plan.

Every scenario runs in a database of its own as `createdb` makes it, in a
transaction it rolls back. A rolled-back CREATE TABLE still leaves dead
catalog tuples, which grow a catalog's heap and so its estimate: measure on
a private, freshly initdb'd PostgreSQL 15.19 cluster (locale en_US.UTF-8),
never the shared reference, by running this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_explain_costs.py \
        "host=127.0.0.1 port=5499 user=postgres"

which prints each scenario that differs from what is written here.
"""

from __future__ import annotations

import sys
from typing import Any

import psycopg
import pytest
from psycopg.types.string import TextLoader

from tests.backlog.test_rp_batch26 import _dsn

# A table never analysed, and two that are.
_E = ["create table e (a int, b text)"]
_ANALYSED = [
    "create table e (a int, b text)",
    "insert into e select g, 'v' || (g % 10) from generate_series(1, 1000) g",
    "create table k (a int primary key, b text)",
    "insert into k select g, 'v' || (g % 10) from generate_series(1, 1000) g",
    "analyze e",
    "analyze k",
]

# Those, and thirty rows of a text column whose lengths vary (RP-386).
_ANALYSED_THIRTY = _ANALYSED + [
    "create table s (a int, c text)",
    "insert into s select g, repeat('x', g % 50) from generate_series(1, 30) g",
    "analyze s",
]

# Those, a ten-row table analysed and one never analysed (RP-329).
_ANALYSED_SMALL = _ANALYSED + [
    "create table zs (a int, b text)",
    "insert into zs select g, 'x' from generate_series(1, 10) g",
    "analyze zs",
    "create table zu (a int, b text)",
]

# Two analysed tables joined on b: one whose b is mostly one value (its
# first MCV 0.6), and one whose b is a quarter NULL against seven values
# (RP-330).
# An array column ANALYZE measured: common elements, NULL arrays and NULL
# elements (RP-368).
_UD_ANALYSED = [
    "create table ud (a int[])",
    "insert into ud select array[i % 10, i % 7] from generate_series(1, 1000) i",
    "insert into ud select null from generate_series(1, 100)",
    "insert into ud select array[i, null] from generate_series(1, 50) i",
    "analyze ud",
]
_UD_TEXTS = [
    "create table ud (a text[])",
    "insert into ud select array['x' || (i % 30), 'y' || (i % 3), 'z']"
    " from generate_series(1, 2000) i",
    "analyze ud",
]

_SKEWED = [
    "create table e (a int, b text)",
    "insert into e select g, case when g < 600 then 'x' else 'v' || (g % 50) end"
    " from generate_series(1, 1000) g",
    "analyze e",
    "create table k (a int, b text)",
    "insert into k select g, 'v' || (g % 10) from generate_series(1, 1000) g",
    "analyze k",
]
_NULLS = [
    "create table e (a int, b text)",
    "insert into e select g, case when g % 4 = 0 then null else 'v' || (g % 10) end"
    " from generate_series(1, 1000) g",
    "analyze e",
    "create table k (a int, b text)",
    "insert into k select g, 'v' || (g % 7) from generate_series(1, 1000) g",
    "analyze k",
]

# `e.b` nine tenths one value, `k.b` ten, both analysed: PostgreSQL merges
# their LEFT join (RP-366).
_SKEW_MOST = [
    "create table e (a int, b text)",
    "insert into e select g, case when g < 900 then 'x' else 'v' || (g % 50) end"
    " from generate_series(1, 1000) g",
    "analyze e",
    "create table k (a int, b text)",
    "insert into k select g, 'v' || (g % 10) from generate_series(1, 1000) g",
    "analyze k",
]

# A table with a defaulted varchar column, analysed (RP-331).
_EC = [
    "create table e (a int, b text, c varchar(10) default 'z')",
    "insert into e select g, 'v' || (g % 10) from generate_series(1, 1000) g",
    "analyze e",
]

# A table with a secondary and a composite index, analysed.
_N = [
    "create table n (a int, b int, c text)",
    "insert into n select g, g % 50, 'x' || (g % 7) from generate_series(1, 5000) g",
    "create index n_b on n (b)",
    "create index n_ab on n (a, b)",
    "analyze n",
]

# name: (item, setup, query, PostgreSQL 15.19's rows)
SCENARIOS: dict[str, tuple[str, list[str], str, list[str]]] = {
    # ------------------------------------------- RP-314 a single Seq Scan
    "a table never analysed": (
        "RP-314",
        _E,
        "explain select * from e",
        ["Seq Scan on e  (cost=0.00..22.70 rows=1270 width=36)"],
    ),
    "a filter on a table never analysed": (
        "RP-314",
        _E,
        "explain select * from e where a > 3 and b <> 'y'",
        [
            "Seq Scan on e  (cost=0.00..29.05 rows=421 width=36)",
            "  Filter: ((a > 3) AND (b <> 'y'::text))",
        ],
    ),
    "a repeated column": (
        "RP-314",
        _E,
        "explain select a, a from e",
        ["Seq Scan on e  (cost=0.00..22.70 rows=1270 width=8)"],
    ),
    "an analysed table": (
        "RP-314",
        _ANALYSED,
        "explain select * from e where a < 100",
        ["Seq Scan on e  (cost=0.00..17.50 rows=99 width=7)", "  Filter: (a < 100)"],
    ),
    "an analysed table's most common value": (
        "RP-314",
        _ANALYSED,
        "explain select b from e where b = 'v3'",
        ["Seq Scan on e  (cost=0.00..17.50 rows=100 width=3)", "  Filter: (b = 'v3'::text)"],
    ),
    "a catalog's column": (
        "RP-314",
        [],
        "explain select relname from pg_class",
        ["Seq Scan on pg_class  (cost=0.00..18.10 rows=410 width=64)"],
    ),
    "a catalog's histogram": (
        "RP-314",
        [],
        "explain select * from pg_attribute where attnum > 0",
        [
            "Seq Scan on pg_attribute  (cost=0.00..94.18 rows=2406 width=240)",
            "  Filter: (attnum > 0)",
        ],
    ),
    "a catalog in JSON": (
        "RP-314",
        [],
        "explain (format json) select * from pg_namespace",
        [
            '[\n  {\n    "Plan": {\n      "Node Type": "Seq Scan",\n'
            '      "Parallel Aware": false,\n      "Async Capable": false,\n'
            '      "Relation Name": "pg_namespace",\n      "Alias": "pg_namespace",\n'
            '      "Startup Cost": 0.00,\n      "Total Cost": 1.04,\n'
            '      "Plan Rows": 4,\n      "Plan Width": 117\n    }\n  }\n]'
        ],
    ),
    "a catalog's size in pg_class": (
        "RP-314",
        [],
        "select relpages::text || ' ' || reltuples::text || ' ' || relallvisible::text"
        " from pg_class where relname = 'pg_type'",
        ["15 611 15"],
    ),
    # ----------------------------------- each other node kind, its own item
    "index scan": (
        "RP-317",
        _ANALYSED,
        "explain select * from k where a = 5",
        [
            "Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=7)",
            "  Index Cond: (a = 5)",
        ],
    ),
    "an index scan's output column": (
        "RP-317",
        _ANALYSED,
        "explain select b from k where a = 500",
        [
            "Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=3)",
            "  Index Cond: (a = 500)",
        ],
    ),
    "sort": (
        "RP-318",
        _ANALYSED,
        "explain select * from e order by b",
        [
            "Sort  (cost=64.83..67.33 rows=1000 width=7)",
            "  Sort Key: b",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a sort past work_mem": (
        "RP-318",
        [
            "set local work_mem = '64kB'",
            "create table s (a int, b text)",
            "insert into s select g, 'v' || (g % 10) from generate_series(1, 5000) g",
            "analyze s",
        ],
        "explain select * from s order by b desc",
        [
            "Sort  (cost=450.19..462.69 rows=5000 width=7)",
            "  Sort Key: b DESC",
            "  ->  Seq Scan on s  (cost=0.00..73.00 rows=5000 width=7)",
        ],
    ),
    "limit": (
        "RP-319",
        _ANALYSED,
        "explain select * from e limit 5",
        [
            "Limit  (cost=0.00..0.07 rows=5 width=7)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "limit with an offset and a filter": (
        "RP-319",
        _ANALYSED,
        "explain select * from e where a < 500 offset 100 limit 5",
        [
            "Limit  (cost=3.51..3.68 rows=5 width=7)",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=499 width=7)",
            "        Filter: (a < 500)",
        ],
    ),
    "limit 0": (
        "RP-319",
        _ANALYSED,
        "explain select a from e limit 0",
        [
            "Limit  (cost=0.00..0.01 rows=1 width=4)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "an offset alone": (
        "RP-319",
        _ANALYSED,
        "explain select * from e offset 990",
        [
            "Limit  (cost=14.85..15.00 rows=10 width=7)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "unique": (
        "RP-320",
        _ANALYSED,
        "explain select distinct a from e order by a",
        [
            "Unique  (cost=64.83..69.83 rows=1000 width=4)",
            "  ->  Sort  (cost=64.83..67.33 rows=1000 width=4)",
            "        Sort Key: a",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "aggregate": (
        "RP-321",
        _ANALYSED,
        "explain select count(*) from e",
        [
            "Aggregate  (cost=17.50..17.51 rows=1 width=8)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=0)",
        ],
    ),
    "a count of a column under a filter": (
        "RP-321",
        _ANALYSED,
        "explain select count(a) from e where a < 500",
        [
            "Aggregate  (cost=18.75..18.76 rows=1 width=8)",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=499 width=4)",
            "        Filter: (a < 500)",
        ],
    ),
    "two counts": (
        "RP-321",
        _ANALYSED,
        "explain select count(*), count(b) from e",
        [
            "Aggregate  (cost=20.00..20.01 rows=1 width=16)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=3)",
        ],
    ),
    "hashaggregate": (
        "RP-322",
        _ANALYSED,
        "explain select b, count(*) from e group by b",
        [
            "HashAggregate  (cost=20.00..20.10 rows=10 width=11)",
            "  Group Key: b",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=3)",
        ],
    ),
    "a grouped count of a column under a filter": (
        "RP-322",
        _ANALYSED,
        "explain select count(a), b from e where a < 500 group by b",
        [
            "HashAggregate  (cost=19.99..20.09 rows=10 width=11)",
            "  Group Key: b",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=499 width=7)",
            "        Filter: (a < 500)",
        ],
    ),
    "a group with two counts": (
        "RP-322",
        _ANALYSED,
        "explain select b, count(*), count(a) from e group by b",
        [
            "HashAggregate  (cost=22.50..22.60 rows=10 width=19)",
            "  Group Key: b",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "windowagg": (
        "RP-323",
        _ANALYSED,
        "explain select a, row_number() over () from e",
        [
            "WindowAgg  (cost=0.00..27.50 rows=1000 width=12)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "two ranking functions alone": (
        "RP-323",
        _ANALYSED,
        "explain select row_number() over (), rank() over () from e",
        [
            "WindowAgg  (cost=0.00..30.00 rows=1000 width=16)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=0)",
        ],
    ),
    "a windowagg over a sort": (
        "RP-347",
        _ANALYSED,
        "explain select a, row_number() over (order by a) from e",
        [
            "WindowAgg  (cost=64.83..82.33 rows=1000 width=12)",
            "  ->  Sort  (cost=64.83..67.33 rows=1000 width=4)",
            "        Sort Key: a",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "a partitioned windowagg over a sort": (
        "RP-347",
        _ANALYSED,
        "explain select a, b, rank() over (partition by b order by a desc) from e",
        [
            "WindowAgg  (cost=64.83..84.83 rows=1000 width=15)",
            "  ->  Sort  (cost=64.83..67.33 rows=1000 width=7)",
            "        Sort Key: b, a DESC",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a window ordered nulls first": (
        "RP-347",
        _ANALYSED,
        "explain select row_number() over (order by a nulls first) from e",
        [
            "WindowAgg  (cost=64.83..82.33 rows=1000 width=12)",
            "  ->  Sort  (cost=64.83..67.33 rows=1000 width=4)",
            "        Sort Key: a NULLS FIRST",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "an aggregate over a window": (
        "RP-351",
        _ANALYSED,
        "explain select a, sum(a) over () from e",
        [
            "WindowAgg  (cost=0.00..27.50 rows=1000 width=12)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "a window function with an argument": (
        "RP-351",
        _ANALYSED,
        "explain select a, lag(a) over (order by a) from e",
        [
            "WindowAgg  (cost=64.83..82.33 rows=1000 width=8)",
            "  ->  Sort  (cost=64.83..67.33 rows=1000 width=4)",
            "        Sort Key: a",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "a count of a column a window reads": (
        "RP-351",
        _ANALYSED,
        "explain select a, count(b) over () from e",
        [
            "WindowAgg  (cost=0.00..27.50 rows=1000 width=12)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "an aggregate with a constant argument": (
        "RP-351",
        _ANALYSED,
        "explain select a, string_agg(b, ',') over () from e",
        [
            "WindowAgg  (cost=0.00..27.50 rows=1000 width=36)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "an aggregate under a filter clause": (
        "RP-352",
        _ANALYSED,
        "explain select a, sum(a) filter (where a > 5) over () from e",
        [
            "WindowAgg  (cost=0.00..30.00 rows=1000 width=12)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "an aggregate of an expression": (
        "RP-352",
        _ANALYSED,
        "explain select a, sum(a+1) over () from e",
        [
            "WindowAgg  (cost=0.00..30.00 rows=1000 width=12)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "two windows stacked": (
        "RP-355",
        _ANALYSED,
        "explain select a, row_number() over (order by a), rank() over (order by b) from e",
        [
            "WindowAgg  (cost=132.16..149.66 rows=1000 width=23)",
            "  ->  Sort  (cost=132.16..134.66 rows=1000 width=15)",
            "        Sort Key: a",
            "        ->  WindowAgg  (cost=64.83..82.33 rows=1000 width=15)",
            "              ->  Sort  (cost=64.83..67.33 rows=1000 width=7)",
            "                    Sort Key: b",
            "                    ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    # Measured on a fresh private PostgreSQL 15.19 cluster (RP-355): the
    # window with the higher first key ref goes below, a window whose keys
    # prefix the order below needs no Sort, and functions over one window
    # share its WindowAgg.
    "two windows stacked, later key first": (
        "RP-355",
        _ANALYSED,
        "explain select a, rank() over (order by b), row_number() over (order by a) from e",
        [
            "WindowAgg  (cost=132.16..149.66 rows=1000 width=23)",
            "  ->  Sort  (cost=132.16..134.66 rows=1000 width=15)",
            "        Sort Key: b",
            "        ->  WindowAgg  (cost=64.83..82.33 rows=1000 width=15)",
            "              ->  Sort  (cost=64.83..67.33 rows=1000 width=7)",
            "                    Sort Key: a",
            "                    ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "two windows stacked on one sort": (
        "RP-355",
        _ANALYSED,
        "explain select a, row_number() over (order by a),"
        " rank() over (partition by a order by b) from e",
        [
            "WindowAgg  (cost=64.83..99.83 rows=1000 width=23)",
            "  ->  WindowAgg  (cost=64.83..84.83 rows=1000 width=15)",
            "        ->  Sort  (cost=64.83..67.33 rows=1000 width=7)",
            "              Sort Key: a, b",
            "              ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "an empty window over a shared one": (
        "RP-355",
        _ANALYSED,
        "explain select a, row_number() over (), rank() over (order by b),"
        " dense_rank() over (order by b) from e",
        [
            "WindowAgg  (cost=64.83..97.33 rows=1000 width=31)",
            "  ->  WindowAgg  (cost=64.83..84.83 rows=1000 width=23)",
            "        ->  Sort  (cost=64.83..67.33 rows=1000 width=7)",
            "              Sort Key: b",
            "              ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a ranking function under a filter": (
        "RP-323",
        _ANALYSED,
        "explain select b, dense_rank() over () from e where a < 500",
        [
            "WindowAgg  (cost=0.00..23.74 rows=499 width=11)",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=499 width=3)",
            "        Filter: (a < 500)",
        ],
    ),
    "append": (
        "RP-324",
        _ANALYSED,
        "explain select a from e union all select a from k",
        [
            "Append  (cost=0.00..40.00 rows=2000 width=4)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
            "  ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "an append of a filtered text column": (
        "RP-346",
        _ANALYSED,
        "explain select a, b from e where a < 100 union all select a, b from k",
        [
            "Append  (cost=0.00..38.98 rows=1099 width=7)",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=99 width=7)",
            "        Filter: (a < 100)",
            "  ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "result": (
        "RP-325",
        [],
        "explain select 1",
        ["Result  (cost=0.00..0.01 rows=1 width=4)"],
    ),
    "a result's widths": (
        "RP-325",
        [],
        "explain select 1, 'abc'::text, 2.5, 1::int8, true",
        ["Result  (cost=0.00..0.01 rows=1 width=77)"],
    ),
    "a result's untyped literal and null": (
        "RP-325",
        [],
        "explain select 'x', null",
        ["Result  (cost=0.00..0.01 rows=1 width=64)"],
    ),
    "a result of a bounded varchar": (
        "RP-325",
        [],
        "explain select 'ab'::varchar(10)",
        ["Result  (cost=0.00..0.01 rows=1 width=38)"],
    ),
    "values scan": (
        "RP-326",
        [],
        "explain values (1), (2)",
        ['Values Scan on "*VALUES*"  (cost=0.00..0.03 rows=2 width=4)'],
    ),
    "a values scan of modified and common types": (
        "RP-326",
        [],
        "explain values ('a'::varchar(10), 1.5), ('bb'::varchar(10), 2)",
        ['Values Scan on "*VALUES*"  (cost=0.00..0.03 rows=2 width=70)'],
    ),
    "a values scan whose typmods differ": (
        "RP-326",
        [],
        "explain values ('a'::varchar(10)), ('bb'), ('c')",
        ['Values Scan on "*VALUES*"  (cost=0.00..0.04 rows=3 width=32)'],
    ),
    "function scan": (
        "RP-327",
        [],
        "explain select * from generate_series(1, 10)",
        ["Function Scan on generate_series  (cost=0.00..0.10 rows=10 width=4)"],
    ),
    "a bigint function scan": (
        "RP-327",
        [],
        "explain select * from generate_series(1::int8, 300)",
        ["Function Scan on generate_series  (cost=0.00..3.00 rows=300 width=8)"],
    ),
    "an empty function scan": (
        "RP-327",
        [],
        "explain select * from generate_series(5, 1)",
        ["Function Scan on generate_series  (cost=0.00..0.01 rows=1 width=4)"],
    ),
    "a function scan with a step": (
        "RP-327",
        [],
        "explain select generate_series from generate_series(10, 1, -2)",
        ["Function Scan on generate_series  (cost=0.00..0.05 rows=5 width=4)"],
    ),
    "subquery scan": (
        "RP-328",
        _ANALYSED,
        "explain select * from (select a from e limit 3) s where a > 1",
        [
            "Subquery Scan on s  (cost=0.00..0.08 rows=2 width=4)",
            "  Filter: (s.a > 1)",
            "  ->  Limit  (cost=0.00..0.04 rows=3 width=4)",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "subquery scan of two clauses": (
        "RP-328",
        _ANALYSED,
        "explain select a from (select a, b from e limit 100) s where a < 3 and b <> 'x'",
        [
            "Subquery Scan on s  (cost=0.00..3.00 rows=1 width=4)",
            "  Filter: ((s.a < 3) AND (s.b <> 'x'::text))",
            "  ->  Limit  (cost=0.00..1.50 rows=100 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "subquery scan over an offset": (
        "RP-328",
        _ANALYSED,
        "explain select * from (select a from e limit 10 offset 5) s where a = 7",
        [
            "Subquery Scan on s  (cost=0.07..0.35 rows=1 width=4)",
            "  Filter: (s.a = 7)",
            "  ->  Limit  (cost=0.07..0.22 rows=10 width=4)",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "subquery scan of a renamed column": (
        "RP-328",
        _ANALYSED,
        "explain select x from (select a as x from e limit 50) s where x > 10",
        [
            "Subquery Scan on s  (cost=0.00..1.38 rows=50 width=4)",
            "  Filter: (s.x > 10)",
            "  ->  Limit  (cost=0.00..0.75 rows=50 width=4)",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "subquery scan of a table never analysed": (
        "RP-328",
        _E,
        "explain select * from (select a, b from e limit 100) s where a > 500",
        [
            "Subquery Scan on s  (cost=0.00..3.04 rows=33 width=36)",
            "  Filter: (s.a > 500)",
            "  ->  Limit  (cost=0.00..1.79 rows=100 width=36)",
            "        ->  Seq Scan on e  (cost=0.00..22.70 rows=1270 width=36)",
        ],
    ),
    # ------------------------------------- RP-348 a Filter's text and order
    "an IN list's filter": (
        "RP-348",
        _ANALYSED,
        "explain select * from e where a in (1, 2, 3)",
        [
            "Seq Scan on e  (cost=0.00..18.75 rows=3 width=7)",
            "  Filter: (a = ANY ('{1,2,3}'::integer[]))",
        ],
    ),
    "a filter's clauses in cost order": (
        "RP-348",
        _E,
        "explain select * from e where a = 5 and b > 'q'",
        [
            "Seq Scan on e  (cost=0.00..29.05 rows=2 width=36)",
            "  Filter: ((b > 'q'::text) AND (a = 5))",
        ],
    ),
    "a text range on an analysed table": (
        "RP-348",
        _ANALYSED,
        "explain select * from e where b > 'q'",
        [
            "Seq Scan on e  (cost=0.00..17.50 rows=1000 width=7)",
            "  Filter: (b > 'q'::text)",
        ],
    ),
    "an IN list of strings array_out quotes": (
        "RP-348",
        _ANALYSED,
        "explain select * from e where b in ('x', 'a b', '', 'NULL', 'it''s', 'q\"r')",
        [
            "Seq Scan on e  (cost=0.00..22.50 rows=1 width=7)",
            '  Filter: (b = ANY (\'{x,"a b","","NULL",it\'\'s,"q\\"r"}\'::text[]))',
        ],
    ),
    "a null test, a range and an equality in cost order": (
        "RP-348",
        _ANALYSED,
        "explain select * from e where b is null and a = 3 and b < 'z'",
        [
            "Seq Scan on e  (cost=0.00..20.00 rows=1 width=7)",
            "  Filter: ((b IS NULL) AND (b < 'z'::text) AND (a = 3))",
        ],
    ),
    "a subquery scan's filter in cost order": (
        "RP-348",
        _ANALYSED,
        "explain select * from (select a, b from e limit 50) s where a = 5 and b > 'q'",
        [
            "Subquery Scan on s  (cost=0.00..1.50 rows=1 width=7)",
            "  Filter: ((s.b > 'q'::text) AND (s.a = 5))",
            "  ->  Limit  (cost=0.00..0.75 rows=50 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    # ----------------------------- RP-349 an IN list's NULL element's cost (fixed)
    "an IN list with a NULL": (
        "RP-349",
        _ANALYSED,
        "explain select * from e where a in (1, null, 3)",
        [
            "Seq Scan on e  (cost=0.00..18.75 rows=2 width=7)",
            "  Filter: (a = ANY ('{1,NULL,3}'::integer[]))",
        ],
    ),
    # ----------------------------- RP-367 an array operator's filter
    "an array containment": (
        "RP-367",
        ["create table ud (a int[])"],
        "explain select * from ud where a @> '{1}'",
        [
            "Seq Scan on ud  (cost=0.00..27.00 rows=7 width=32)",
            "  Filter: (a @> '{1}'::integer[])",
        ],
    ),
    "an array overlap of two elements": (
        "RP-367",
        ["create table ud (a int[])"],
        "explain select * from ud where a && '{1,2}'",
        [
            "Seq Scan on ud  (cost=0.00..27.00 rows=14 width=32)",
            "  Filter: (a && '{1,2}'::integer[])",
        ],
    ),
    "an array contained by a constant": (
        "RP-367",
        ["create table ud (a int[])"],
        "explain select * from ud where a <@ '{1,2}'",
        [
            "Seq Scan on ud  (cost=0.00..27.00 rows=7 width=32)",
            "  Filter: (a <@ '{1,2}'::integer[])",
        ],
    ),
    "an array containing a repeated element": (
        "RP-367",
        ["create table ud (a int[])"],
        "explain select * from ud where a @> '{1,2,2}'",
        [
            "Seq Scan on ud  (cost=0.00..27.00 rows=1 width=32)",
            "  Filter: (a @> '{1,2,2}'::integer[])",
        ],
    ),
    "an array containing a NULL": (
        "RP-367",
        ["create table ud (a int[])"],
        "explain select * from ud where a @> '{1,NULL}'",
        [
            "Seq Scan on ud  (cost=0.00..27.00 rows=1 width=32)",
            "  Filter: (a @> '{1,NULL}'::integer[])",
        ],
    ),
    "two array operators": (
        "RP-367",
        ["create table ud (a int[])"],
        "explain select * from ud where a @> '{1}' and a && '{3}'",
        [
            "Seq Scan on ud  (cost=0.00..30.40 rows=1 width=32)",
            "  Filter: ((a @> '{1}'::integer[]) AND (a && '{3}'::integer[]))",
        ],
    ),
    "a constant contained by an array": (
        "RP-367",
        ["create table ud (a int[])"],
        "explain select * from ud where '{1}' <@ a",
        [
            "Seq Scan on ud  (cost=0.00..27.00 rows=7 width=32)",
            "  Filter: ('{1}'::integer[] <@ a)",
        ],
    ),
    "a constant equal to ANY of an array column": (
        "RP-368",
        ["create table ud (a int[])"],
        "explain select * from ud where 1 = any(a)",
        [
            "Seq Scan on ud  (cost=0.00..40.60 rows=7 width=32)",
            "  Filter: (1 = ANY (a))",
        ],
    ),
    "an analysed array containment": (
        "RP-368",
        [
            "create table ud (a int[])",
            "insert into ud select array[i % 10, i % 7] from generate_series(1, 1000) i",
            "analyze ud",
        ],
        "explain select * from ud where a @> '{1}'",
        [
            "Seq Scan on ud  (cost=0.00..20.50 rows=228 width=29)",
            "  Filter: (a @> '{1}'::integer[])",
        ],
    ),
    "a constant unequal to ALL of an array column": (
        "RP-368",
        ["create table ud (a int[])"],
        "explain select * from ud where 1 <> all(a)",
        ["Seq Scan on ud  (cost=0.00..40.60 rows=1353 width=32)", "  Filter: (1 <> ALL (a))"],
    ),
    "a constant less than ANY of an array column": (
        "RP-368",
        ["create table ud (a int[])"],
        "explain select * from ud where 1 < any(a)",
        ["Seq Scan on ud  (cost=0.00..40.60 rows=1336 width=32)", "  Filter: (1 < ANY (a))"],
    ),
    "a cross-type constant equal to ANY of an array column": (
        "RP-370",
        ["create table ud (a int[])"],
        "explain select * from ud where 1::bigint = any(a)",
        [
            "Seq Scan on ud  (cost=0.00..40.60 rows=66 width=32)",
            "  Filter: ('1'::bigint = ANY (a))",
        ],
    ),
    "a constant equal to ANY of an analysed array column": (
        "RP-368",
        _UD_ANALYSED,
        "explain select * from ud where 1 = any(a)",
        ["Seq Scan on ud  (cost=0.00..34.88 rows=229 width=29)", "  Filter: (1 = ANY (a))"],
    ),
    "a constant equal to ALL of an analysed array column": (
        "RP-368",
        _UD_ANALYSED,
        "explain select * from ud where 1 = all(a)",
        ["Seq Scan on ud  (cost=0.00..34.88 rows=18 width=29)", "  Filter: (1 = ALL (a))"],
    ),
    "a constant unequal to ANY of an analysed array column": (
        "RP-368",
        _UD_ANALYSED,
        "explain select * from ud where 1 <> any(a)",
        ["Seq Scan on ud  (cost=0.00..34.88 rows=1132 width=29)", "  Filter: (1 <> ANY (a))"],
    ),
    "an analysed array overlap": (
        "RP-368",
        _UD_ANALYSED,
        "explain select * from ud where a && '{1,99}'",
        [
            "Seq Scan on ud  (cost=0.00..23.38 rows=229 width=29)",
            "  Filter: (a && '{1,99}'::integer[])",
        ],
    ),
    "an analysed array contained by a constant": (
        "RP-368",
        _UD_ANALYSED,
        "explain select * from ud where a <@ '{1,2,3}'",
        [
            "Seq Scan on ud  (cost=0.00..23.38 rows=137 width=29)",
            "  Filter: (a <@ '{1,2,3}'::integer[])",
        ],
    ),
    "an analysed array contained by every common element": (
        "RP-368",
        _UD_ANALYSED,
        "explain select * from ud where a <@ '{0,1,2,3,4,5,6,7,8,9}'",
        [
            "Seq Scan on ud  (cost=0.00..23.38 rows=965 width=29)",
            "  Filter: (a <@ '{0,1,2,3,4,5,6,7,8,9}'::integer[])",
        ],
    ),
    "an analysed text array containment": (
        "RP-368",
        _UD_TEXTS,
        "explain select * from ud where a @> '{x1,y2}'",
        [
            "Seq Scan on ud  (cost=0.00..44.00 rows=22 width=45)",
            "  Filter: (a @> '{x1,y2}'::text[])",
        ],
    ),
    "an analysed text array contained by a constant": (
        "RP-368",
        _UD_TEXTS,
        "explain select * from ud where a <@ '{x1,y1,y2,z}'",
        [
            "Seq Scan on ud  (cost=0.00..44.00 rows=202 width=45)",
            "  Filter: (a <@ '{x1,y1,y2,z}'::text[])",
        ],
    ),
    # ----------------------------- RP-350 a column equal to two constants
    "a column equal to two constants": (
        "RP-350",
        _ANALYSED,
        "explain select * from e where a = 1 and a = 2",
        [
            "Result  (cost=0.00..17.50 rows=1 width=7)",
            "  One-Time Filter: false",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=1 width=7)",
            "        Filter: (a = 1)",
        ],
    ),
    "a column equal to three constants": (
        "RP-350",
        _ANALYSED,
        "explain select * from e where a = 2 and a = 1 and a = 2 and a = 3",
        [
            "Result  (cost=0.00..17.50 rows=1 width=7)",
            "  One-Time Filter: (false AND false)",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=1 width=7)",
            "        Filter: (a = 2)",
        ],
    ),
    "two classes each equal to two constants": (
        "RP-350",
        _ANALYSED,
        "explain select * from e where a = 1 and b = 'v1' and a = 2 and a > 0 and b = 'v2'",
        [
            "Result  (cost=0.00..22.50 rows=1 width=7)",
            "  One-Time Filter: (false AND false)",
            "  ->  Seq Scan on e  (cost=0.00..22.50 rows=1 width=7)",
            "        Filter: ((a > 0) AND (a = 1) AND (b = 'v1'::text))",
        ],
    ),
    "a repeated equality is one clause": (
        "RP-350",
        _ANALYSED,
        "explain select * from e where a = 1 and b = 'v1' and 1 = a",
        [
            "Seq Scan on e  (cost=0.00..20.00 rows=1 width=7)",
            "  Filter: ((a = 1) AND (b = 'v1'::text))",
        ],
    ),
    "a gated scan under a sort": (
        "RP-350",
        _ANALYSED,
        "explain select * from e where a = 1 and a = 2 order by b",
        [
            "Sort  (cost=17.51..17.52 rows=1 width=7)",
            "  Sort Key: b",
            "  ->  Result  (cost=0.00..17.50 rows=1 width=7)",
            "        One-Time Filter: false",
            "        ->  Seq Scan on e  (cost=0.00..17.50 rows=1 width=7)",
            "              Filter: (a = 1)",
        ],
    ),
    "nested loop": (
        "RP-329",
        _ANALYSED,
        "explain select * from e, e e2 where e.a < e2.a",
        [
            "Nested Loop  (cost=0.00..15032.50 rows=333333 width=14)",
            "  Join Filter: (e.a < e2.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Materialize  (cost=0.00..20.00 rows=1000 width=7)",
            "        ->  Seq Scan on e e2  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "nested loop, the smaller side materialised": (
        "RP-329",
        _ANALYSED_SMALL,
        "explain select * from zs, e where e.a < zs.a",
        [
            "Nested Loop  (cost=0.00..166.12 rows=3333 width=13)",
            "  Join Filter: (e.a < zs.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Materialize  (cost=0.00..1.15 rows=10 width=6)",
            "        ->  Seq Scan on zs  (cost=0.00..1.10 rows=10 width=6)",
        ],
    ),
    "nested loop, a cross join": (
        "RP-329",
        _ANALYSED_SMALL,
        "explain select * from e, zu",
        [
            "Nested Loop  (cost=0.00..15915.20 rows=1270000 width=43)",
            "  ->  Seq Scan on zu  (cost=0.00..22.70 rows=1270 width=36)",
            "  ->  Materialize  (cost=0.00..20.00 rows=1000 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "nested loop, the columns it reads": (
        "RP-329",
        _ANALYSED,
        "explain select e.a from e, e e2 where e.a < e2.a",
        [
            "Nested Loop  (cost=0.00..15032.50 rows=333333 width=4)",
            "  Join Filter: (e.a < e2.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
            "  ->  Materialize  (cost=0.00..20.00 rows=1000 width=4)",
            "        ->  Seq Scan on e e2  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "nested loop, a filtered side": (
        "RP-329",
        _ANALYSED,
        "explain select * from e, e e2 where e.a < e2.a and e.a > 5",
        [
            "Nested Loop  (cost=0.00..14959.99 rows=331667 width=14)",
            "  Join Filter: (e.a < e2.a)",
            "  ->  Seq Scan on e e2  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Materialize  (cost=0.00..22.48 rows=995 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..17.50 rows=995 width=7)",
            "              Filter: (a > 5)",
        ],
    ),
    # ------------------- RP-354 the nested loops RP-329 leaves at zeros
    # Split from RP-354 and fixed: an inequality between the sides is the
    # indexed side's parameterized Index Cond (`DEFAULT_INEQ_SEL` a scan,
    # `loop_count` the outer rows), written inner column first.
    "nested loop into an index": (
        "RP-329",
        _ANALYSED,
        "explain select * from e, k where e.a < k.a",
        [
            "Nested Loop  (cost=0.28..9487.50 rows=333333 width=14)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Index Scan using k_pkey on k  (cost=0.28..6.14 rows=333 width=7)",
            "        Index Cond: (a > e.a)",
        ],
    ),
    # Split from RP-354 and fixed: the other join clauses move into the
    # parameterized scan as its Filter (`join_clause_is_movable_into`), and
    # cut its rows.
    "nested loop into an index with a moved filter": (
        "RP-329",
        _ANALYSED,
        "explain select * from e, k where k.a >= e.a and e.b <> k.b",
        [
            "Nested Loop  (cost=0.28..9990.00 rows=300000 width=14)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Index Scan using k_pkey on k  (cost=0.28..6.98 rows=300 width=7)",
            "        Index Cond: (a >= e.a)",
            "        Filter: (e.b <> b)",
        ],
    ),
    # Split from RP-354 and fixed: a LEFT join's nullable side is read
    # through its index too, its ON's other clause the scan's Filter.
    "nested loop left join into an index": (
        "RP-329",
        _ANALYSED,
        "explain select * from e left join k on e.a < k.a and e.b <> k.b",
        [
            "Nested Loop Left Join  (cost=0.28..9990.00 rows=300000 width=14)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Index Scan using k_pkey on k  (cost=0.28..6.98 rows=300 width=7)",
            "        Index Cond: (a > e.a)",
            "        Filter: (e.b <> b)",
        ],
    ),
    # Split from RP-354 and fixed: a `<>` the Index Cond implies is costed
    # but not printed (`create_indexscan_plan`'s `predicate_implied_by`).
    "nested loop into an index with an implied filter": (
        "RP-329",
        _ANALYSED,
        "explain select * from e, k where e.a > k.a and e.a <> k.a and e.b <> k.b",
        [
            "Nested Loop  (cost=0.28..10822.50 rows=299700 width=14)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Index Scan using k_pkey on k  (cost=0.28..7.81 rows=300 width=7)",
            "        Index Cond: (a < e.a)",
            "        Filter: (e.b <> b)",
        ],
    ),
    # Split from RP-354 and fixed: the index holds every column read, an
    # Index Only Scan, its heap fetches cut by the all-visible fraction
    # (none here: the table was never vacuumed).
    "nested loop into an index-only scan": (
        "RP-329",
        _ANALYSED,
        "explain select k.a from e, k where e.a < k.a",
        [
            "Nested Loop  (cost=0.28..9487.50 rows=333333 width=4)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
            "  ->  Index Only Scan using k_pkey on k  (cost=0.28..6.14 rows=333 width=4)",
            "        Index Cond: (a > e.a)",
        ],
    ),
    # Split from RP-354 and fixed: a JOIN ... ON's conjuncts are the join's
    # clauses, and a `<>` cannot be an indexed side's Index Cond, so an
    # indexed table's plain Seq Scan is the inner PostgreSQL weighs.
    "nested loop on an ON's inequality": (
        "RP-329",
        _ANALYSED,
        "explain select * from e join k on e.a <> k.a",
        [
            "Nested Loop  (cost=0.00..15032.50 rows=999000 width=14)",
            "  Join Filter: (e.a <> k.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Materialize  (cost=0.00..20.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    # Split from RP-354 and fixed: a `<>` conjunct is `neqjoinsel`, one
    # minus `eqjoinsel_inner` over the two columns' MCV lists.
    "nested loop with a not-equal filter": (
        "RP-329",
        _ANALYSED,
        "explain select * from e, e e2 where e.a < e2.a and e.b <> e2.b",
        [
            "Nested Loop  (cost=0.00..17532.50 rows=300000 width=14)",
            "  Join Filter: ((e.a < e2.a) AND (e.b <> e2.b))",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Materialize  (cost=0.00..20.00 rows=1000 width=7)",
            "        ->  Seq Scan on e e2  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    # Split from RP-354 and fixed: a LEFT join's nested loop keeps its
    # nullable side inner, and its rows are never fewer than the outer's.
    "nested loop left join": (
        "RP-329",
        _ANALYSED,
        "explain select * from e left join e e2 on e.a < e2.a",
        [
            "Nested Loop Left Join  (cost=0.00..15032.50 rows=333333 width=14)",
            "  Join Filter: (e.a < e2.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Materialize  (cost=0.00..20.00 rows=1000 width=7)",
            "        ->  Seq Scan on e e2  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    # A LEFT join's ON conjunct over its outer side alone stays a join
    # filter, estimated from that column's statistics.
    "nested loop left join with a one-sided ON conjunct": (
        "RP-354",
        _ANALYSED,
        "explain select * from e left join e e2 on e.a <> e2.a and e.a > 990",
        [
            "Nested Loop Left Join  (cost=0.00..17532.50 rows=9990 width=14)",
            "  Join Filter: ((e.a <> e2.a) AND (e.a > 990))",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Materialize  (cost=0.00..20.00 rows=1000 width=7)",
            "        ->  Seq Scan on e e2  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    # The cheaper order would put e outer; a LEFT join cannot.
    "nested loop left join, the smaller side outer": (
        "RP-329",
        _ANALYSED_SMALL,
        "explain select * from zs left join e on e.a < zs.a",
        [
            "Nested Loop Left Join  (cost=0.00..168.60 rows=3333 width=13)",
            "  Join Filter: (e.a < zs.a)",
            "  ->  Seq Scan on zs  (cost=0.00..1.10 rows=10 width=6)",
            "  ->  Materialize  (cost=0.00..20.00 rows=1000 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "nested loop left join into a primary key's table": (
        "RP-329",
        _ANALYSED,
        "explain select k.a from e left join k on e.a <> k.a",
        [
            "Nested Loop Left Join  (cost=0.00..15032.50 rows=999000 width=4)",
            "  Join Filter: (e.a <> k.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
            "  ->  Materialize  (cost=0.00..20.00 rows=1000 width=4)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "hash join": (
        "RP-330",
        _ANALYSED,
        "explain select * from e join k on e.b = k.b",
        [
            "Hash Join  (cost=27.50..1170.00 rows=100000 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "hash join on a skewed column": (
        "RP-330",
        _SKEWED,
        "explain select * from e join k on e.b = k.b",
        [
            "Hash Join  (cost=27.50..251.00 rows=8100 width=13)",
            "  Hash Cond: (e.b = k.b)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=6)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "hash join over nulls": (
        "RP-330",
        _NULLS,
        "explain select * from k join e on k.b = e.b",
        [
            "Hash Join  (cost=27.50..925.75 rows=71450 width=14)",
            "  Hash Cond: (k.b = e.b)",
            "  ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "hash join hashing the left table": (
        "RP-359",
        _NULLS,
        "explain select * from e join k on e.b = k.b",
        [
            "Hash Join  (cost=27.50..925.75 rows=71450 width=14)",
            "  Hash Cond: (k.b = e.b)",
            "  ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "hash join reading one side's column": (
        "RP-361",
        _ANALYSED,
        "explain select e.a from e join k on e.b = k.b",
        [
            "Hash Join  (cost=27.50..1170.00 rows=100000 width=4)",
            "  Hash Cond: (e.b = k.b)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=3)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=3)",
        ],
    ),
    "a projected LEFT join hashed as a right join": (
        "RP-362",
        _NULLS,
        "explain select e.a from e left join k on e.b = k.b",
        [
            "Hash Right Join  (cost=27.50..925.75 rows=71450 width=4)",
            "  Hash Cond: (k.b = e.b)",
            "  ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=3)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a projected join under a WHERE": (
        "RP-362",
        _NULLS,
        "explain select e.a from e join k on e.b = k.b where e.a = 3",
        [
            "Hash Join  (cost=17.51..36.97 rows=71 width=4)",
            "  Hash Cond: (k.b = e.b)",
            "  ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=3)",
            "  ->  Hash  (cost=17.50..17.50 rows=1 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..17.50 rows=1 width=7)",
            "              Filter: (a = 3)",
        ],
    ),
    "a projected cross join": (
        "RP-362",
        _NULLS,
        "explain select e.a, k.a from e, k",
        [
            "Nested Loop  (cost=0.00..12532.50 rows=1000000 width=8)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
            "  ->  Materialize  (cost=0.00..20.00 rows=1000 width=4)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=4)",
        ],
    ),
    "a projected join whose WHERE fixes the key": (
        "RP-363",
        _NULLS,
        "explain select e.a from e join k on e.b = k.b where e.b = 'v1'",
        [
            "Nested Loop  (cost=0.00..214.00 rows=14300 width=4)",
            "  ->  Seq Scan on k  (cost=0.00..17.50 rows=143 width=3)",
            "        Filter: (b = 'v1'::text)",
            "  ->  Materialize  (cost=0.00..18.00 rows=100 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..17.50 rows=100 width=7)",
            "              Filter: (b = 'v1'::text)",
        ],
    ),
    "a projected LEFT join reduced by its WHERE": (
        "RP-365",
        _NULLS,
        "explain select e.a from e left join k on e.b = k.b where k.a = 3",
        [
            "Hash Join  (cost=17.51..36.97 rows=71 width=4)",
            "  Hash Cond: (e.b = k.b)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=17.50..17.50 rows=1 width=3)",
            "        ->  Seq Scan on k  (cost=0.00..17.50 rows=1 width=3)",
            "              Filter: (a = 3)",
        ],
    ),
    "a projected LEFT join whose WHERE fixes the outer key": (
        "RP-364",
        _NULLS,
        "explain select e.a from e left join k on e.b = k.b where e.b = 'v1'",
        [
            "Hash Right Join  (cost=18.75..197.48 rows=14300 width=4)",
            "  Hash Cond: (k.b = e.b)",
            "  ->  Seq Scan on k  (cost=0.00..17.50 rows=143 width=3)",
            "        Filter: (b = 'v1'::text)",
            "  ->  Hash  (cost=17.50..17.50 rows=100 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..17.50 rows=100 width=7)",
            "              Filter: (b = 'v1'::text)",
        ],
    ),
    "a projected join PostgreSQL merges": (
        "RP-366",
        _SKEWED,
        "explain select e.a from e join k on e.b = k.b where e.a > 10 and k.a <= 400",
        [
            "Merge Join  (cost=101.55..148.70 rows=3208 width=4)",
            "  Merge Cond: (k.b = e.b)",
            "  ->  Sort  (cost=34.79..35.79 rows=400 width=3)",
            "        Sort Key: k.b",
            "        ->  Seq Scan on k  (cost=0.00..17.50 rows=400 width=3)",
            "              Filter: (a <= 400)",
            "  ->  Sort  (cost=66.76..69.23 rows=990 width=6)",
            "        Sort Key: e.b",
            "        ->  Seq Scan on e  (cost=0.00..17.50 rows=990 width=6)",
            "              Filter: (a > 10)",
        ],
    ),
    "a RIGHT join reduced by its WHERE": (
        "RP-365",
        _NULLS,
        "explain select k.a from e right join k on e.b = k.b where e.a = 7",
        [
            "Hash Join  (cost=17.51..36.97 rows=71 width=4)",
            "  Hash Cond: (k.b = e.b)",
            "  ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=17.50..17.50 rows=1 width=3)",
            "        ->  Seq Scan on e  (cost=0.00..17.50 rows=1 width=3)",
            "              Filter: (a = 7)",
        ],
    ),
    "a join with a WHERE on each side": (
        "RP-365",
        _NULLS,
        "explain select * from e join k on e.b = k.b where e.a > 10 and k.a < 900",
        [
            "Hash Join  (cost=29.88..833.87 rows=63591 width=14)",
            "  Hash Cond: (k.b = e.b)",
            "  ->  Seq Scan on k  (cost=0.00..17.50 rows=899 width=7)",
            "        Filter: (a < 900)",
            "  ->  Hash  (cost=17.50..17.50 rows=990 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..17.50 rows=990 width=7)",
            "              Filter: (a > 10)",
        ],
    ),
    "a LEFT join PostgreSQL merges": (
        "RP-366",
        _SKEW_MOST,
        "explain select e.a from e left join k on e.b = k.b",
        [
            "Merge Left Join  (cost=129.66..166.16 rows=2100 width=4)",
            "  Merge Cond: (e.b = k.b)",
            "  ->  Sort  (cost=64.83..67.33 rows=1000 width=6)",
            "        Sort Key: e.b",
            "        ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=6)",
            "  ->  Sort  (cost=64.83..67.33 rows=1000 width=3)",
            "        Sort Key: k.b",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=3)",
        ],
    ),
    "a join whose filtered side is an index scan": (
        "RP-369",
        _ANALYSED,
        "explain select * from e join k on e.b = k.b where k.a = 5",
        [
            "Hash Join  (cost=8.30..27.04 rows=100 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=8.29..8.29 rows=1 width=7)",
            "        ->  Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=7)",
            "              Index Cond: (a = 5)",
        ],
    ),
    # The same side under a narrower select list, and under a LEFT join
    # the WHERE reduces to an inner one (RP-369).
    "a join's index-scan side of the columns read": (
        "RP-369",
        _ANALYSED,
        "explain select e.a from e join k on e.b = k.b where k.a = 5",
        [
            "Hash Join  (cost=8.30..27.04 rows=100 width=4)",
            "  Hash Cond: (e.b = k.b)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=8.29..8.29 rows=1 width=3)",
            "        ->  Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=3)",
            "              Index Cond: (a = 5)",
        ],
    ),
    "a reduced left join's index-scan side": (
        "RP-369",
        _ANALYSED,
        "explain select * from e left join k on e.b = k.b where k.a = 5",
        [
            "Hash Join  (cost=8.30..27.04 rows=100 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=8.29..8.29 rows=1 width=7)",
            "        ->  Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=7)",
            "              Index Cond: (a = 5)",
        ],
    ),
    "a join whose index-scan side reads a key range": (
        "RP-371",
        _ANALYSED,
        "explain select * from e join k on e.b = k.b where k.a < 5",
        [
            "Hash Join  (cost=8.40..34.90 rows=400 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=8.35..8.35 rows=4 width=7)",
            "        ->  Index Scan using k_pkey on k  (cost=0.28..8.35 rows=4 width=7)",
            "              Index Cond: (a < 5)",
        ],
    ),
    "a key fixed by a constant over an index-scan side": (
        "RP-376",
        _ANALYSED,
        "explain select * from e join k on e.b = k.b where k.a = 5 and k.b = 'v5'",
        [
            "Nested Loop  (cost=0.28..26.79 rows=100 width=14)",
            "  ->  Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=7)",
            "        Index Cond: (a = 5)",
            "        Filter: (b = 'v5'::text)",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=100 width=7)",
            "        Filter: (b = 'v5'::text)",
        ],
    ),
    "a key fixed by a constant over an indexed table's sequential scan": (
        "RP-376",
        _ANALYSED,
        "explain select * from e join k on e.b = k.b where k.b = 'v5'",
        [
            "Nested Loop  (cost=0.00..160.25 rows=10000 width=14)",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=100 width=7)",
            "        Filter: (b = 'v5'::text)",
            "  ->  Materialize  (cost=0.00..18.00 rows=100 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..17.50 rows=100 width=7)",
            "              Filter: (b = 'v5'::text)",
        ],
    ),
    "a key fixed on the other side of an index-scan side": (
        "RP-376",
        _ANALYSED,
        "explain select * from e join k on e.b = k.b where e.b = 'v5' and k.a = 5",
        [
            "Nested Loop  (cost=0.28..26.79 rows=100 width=14)",
            "  ->  Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=7)",
            "        Index Cond: (a = 5)",
            "        Filter: (b = 'v5'::text)",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=100 width=7)",
            "        Filter: (b = 'v5'::text)",
        ],
    ),
    "a comma join's key fixed over an index-scan side": (
        "RP-376",
        _ANALYSED,
        "explain select * from e, k where e.b = k.b and k.a = 5 and k.b = 'v5'",
        [
            "Nested Loop  (cost=0.28..26.79 rows=100 width=14)",
            "  ->  Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=7)",
            "        Index Cond: (a = 5)",
            "        Filter: (b = 'v5'::text)",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=100 width=7)",
            "        Filter: (b = 'v5'::text)",
        ],
    ),
    "a join on a key a B-tree leads with": (
        "RP-377",
        _ANALYSED,
        "explain select * from e join k on e.a = k.a",
        [
            "Hash Join  (cost=27.50..45.14 rows=1000 width=14)",
            "  Hash Cond: (e.a = k.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a join on a unique key with the other side filtered": (
        "RP-377",
        _ANALYSED,
        "explain select * from e join k on e.a = k.a where e.b = 'v3'",
        [
            "Hash Join  (cost=18.75..38.50 rows=100 width=14)",
            "  Hash Cond: (k.a = e.a)",
            "  ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=17.50..17.50 rows=100 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..17.50 rows=100 width=7)",
            "              Filter: (b = 'v3'::text)",
        ],
    ),
    "a join on a unique key whose indexed side is filtered": (
        "RP-381",
        _ANALYSED,
        "explain select * from e join k on e.a = k.a where k.b = 'v3'",
        [
            "Hash Join  (cost=18.75..36.39 rows=100 width=14)",
            "  Hash Cond: (e.a = k.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=17.50..17.50 rows=100 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..17.50 rows=100 width=7)",
            "              Filter: (b = 'v3'::text)",
        ],
    ),
    "a left join on a unique key": (
        "RP-381",
        _ANALYSED,
        "explain select * from e left join k on e.a = k.a",
        [
            "Hash Left Join  (cost=27.50..45.14 rows=1000 width=14)",
            "  Hash Cond: (e.a = k.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a join on a unique key with a join filter": (
        "RP-381",
        _ANALYSED,
        "explain select * from e join k on e.a = k.a and e.b < k.b",
        [
            "Hash Join  (cost=27.50..45.12 rows=333 width=14)",
            "  Hash Cond: (e.a = k.a)",
            "  Join Filter: (e.b < k.b)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a comma join's equality over an index-scan side": (
        "RP-372",
        _ANALYSED,
        "explain select * from e, k where e.b = k.b and k.a = 5",
        [
            "Hash Join  (cost=8.30..27.04 rows=100 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=8.29..8.29 rows=1 width=7)",
            "        ->  Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=7)",
            "              Index Cond: (a = 5)",
        ],
    ),
    "a comma join's equality over two sequential scans": (
        "RP-372",
        _ANALYSED,
        "explain select e.a from e, k where e.b = k.b",
        [
            "Hash Join  (cost=27.50..1170.00 rows=100000 width=4)",
            "  Hash Cond: (e.b = k.b)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=3)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=3)",
        ],
    ),
    "a comma join's two equalities": (
        "RP-374",
        _ANALYSED,
        "explain select * from e, k where e.b = k.b and e.a = k.a",
        [
            "Hash Join  (cost=30.00..50.25 rows=100 width=14)",
            "  Hash Cond: ((e.b = k.b) AND (e.a = k.a))",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a JOIN ON's two equalities": (
        "RP-374",
        _ANALYSED,
        "explain select * from e join k on e.b = k.b and e.a = k.a",
        [
            "Hash Join  (cost=30.00..50.25 rows=100 width=14)",
            "  Hash Cond: ((e.b = k.b) AND (e.a = k.a))",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a join clause written inner side first": (
        "RP-373",
        _ANALYSED,
        "explain select * from e join k on k.b = e.b",
        [
            "Hash Join  (cost=27.50..1170.00 rows=100000 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a join's equality with an inequality beside it": (
        "RP-375",
        _ANALYSED,
        "explain select * from e join k on e.b = k.b and e.a < k.a",
        [
            "Hash Join  (cost=27.50..1420.00 rows=33333 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  Join Filter: (e.a < k.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    # The comma form plans alike: PostgreSQL makes an inner join's WHERE
    # and ON quals one list (RP-375).
    "a comma join's equality with an inequality beside it": (
        "RP-375",
        _ANALYSED,
        "explain select * from e, k where e.b = k.b and e.a < k.a",
        [
            "Hash Join  (cost=27.50..1420.00 rows=33333 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  Join Filter: (e.a < k.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    # Two inequalities, one over an expression: each scales the rows by
    # DEFAULT_INEQ_SEL, and each operator is paid on every matched pair.
    "a join's equality with two inequalities beside it": (
        "RP-375",
        _ANALYSED,
        "explain select * from e join k on e.b = k.b and e.a + 1 < k.a and e.a > k.a",
        [
            "Hash Join  (cost=27.50..1920.00 rows=11111 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  Join Filter: ((e.a > k.a) AND ((e.a + 1) < k.a))",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a comma join's equality with two inequalities beside it": (
        "RP-375",
        _ANALYSED,
        "explain select * from e, k where e.b = k.b and e.a + 1 < k.a and e.a > k.a",
        [
            "Hash Join  (cost=27.50..1920.00 rows=11111 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  Join Filter: ((e.a > k.a) AND ((e.a + 1) < k.a))",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a LEFT join's equality with an inequality beside it": (
        "RP-379",
        _ANALYSED,
        "explain select * from e left join k on e.b = k.b and e.a < k.a",
        [
            "Hash Left Join  (cost=27.50..1420.00 rows=33333 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  Join Filter: (e.a < k.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a join's equality with a not-equal beside it": (
        "RP-379",
        _ANALYSED,
        "explain select * from e join k on e.b = k.b and e.a <> k.a",
        [
            "Hash Join  (cost=27.50..1420.00 rows=99900 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  Join Filter: (e.a <> k.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a comma join's equality with a not-equal beside it": (
        "RP-379",
        _ANALYSED,
        "explain select * from e, k where e.b = k.b and e.a <> k.a",
        [
            "Hash Join  (cost=27.50..1420.00 rows=99900 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  Join Filter: (e.a <> k.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a LEFT join's equality with a not-equal beside it": (
        "RP-379",
        _ANALYSED,
        "explain select * from e left join k on e.b = k.b and e.a <> k.a",
        [
            "Hash Left Join  (cost=27.50..1420.00 rows=99900 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  Join Filter: (e.a <> k.a)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "a LEFT join's equality with two inequalities beside it": (
        "RP-379",
        _ANALYSED,
        "explain select * from e left join k on e.b = k.b and e.a < k.a and e.a + 1 > k.a",
        [
            "Hash Left Join  (cost=27.50..1920.00 rows=11111 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  Join Filter: ((e.a < k.a) AND ((e.a + 1) > k.a))",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    "insert": (
        "RP-331",
        _ANALYSED,
        "explain insert into e values (1, 'x')",
        [
            "Insert on e  (cost=0.00..0.01 rows=0 width=0)",
            "  ->  Result  (cost=0.00..0.01 rows=1 width=36)",
        ],
    ),
    "update": (
        "RP-331",
        _ANALYSED,
        "explain update e set b = 'y' where a = 1",
        [
            "Update on e  (cost=0.00..17.50 rows=0 width=0)",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=1 width=38)",
            "        Filter: (a = 1)",
        ],
    ),
    "delete": (
        "RP-331",
        _ANALYSED,
        "explain delete from e where a = 1",
        [
            "Delete on e  (cost=0.00..17.50 rows=0 width=0)",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=1 width=6)",
            "        Filter: (a = 1)",
        ],
    ),
    "insert of some columns": (
        "RP-331",
        _EC,
        "explain insert into e(a) values (1)",
        [
            "Insert on e  (cost=0.00..0.01 rows=0 width=0)",
            "  ->  Result  (cost=0.00..0.01 rows=1 width=74)",
        ],
    ),
    "update of every row": (
        "RP-331",
        _EC,
        "explain update e set c = 'q'",
        [
            "Update on e  (cost=0.00..16.00 rows=0 width=0)",
            "  ->  Seq Scan on e  (cost=0.00..16.00 rows=1000 width=44)",
        ],
    ),
    "update of two columns": (
        "RP-331",
        _EC,
        "explain update e set b = 'y', c = 'w' where a < 5",
        [
            "Update on e  (cost=0.00..18.50 rows=0 width=0)",
            "  ->  Seq Scan on e  (cost=0.00..18.50 rows=4 width=76)",
            "        Filter: (a < 5)",
        ],
    ),
    "delete of every row": (
        "RP-331",
        _EC,
        "explain delete from e",
        [
            "Delete on e  (cost=0.00..16.00 rows=0 width=0)",
            "  ->  Seq Scan on e  (cost=0.00..16.00 rows=1000 width=6)",
        ],
    ),
    "a seq scan beneath another node": (
        "RP-332",
        _ANALYSED,
        "explain select * from e where a < 10 order by b",
        [
            "Sort  (cost=17.64..17.67 rows=9 width=7)",
            "  Sort Key: b",
            "  ->  Seq Scan on e  (cost=0.00..17.50 rows=9 width=7)",
            "        Filter: (a < 10)",
        ],
    ),
    "a seq scan of an expression": (
        "RP-333",
        _ANALYSED,
        "explain select a + 1 from e",
        ["Seq Scan on e  (cost=0.00..17.50 rows=1000 width=4)"],
    ),
    "expressions over a filtered scan": (
        "RP-333",
        _ANALYSED,
        "explain select a, a * 2 + 1, b from e where a < 10",
        [
            "Seq Scan on e  (cost=0.00..17.55 rows=9 width=11)",
            "  Filter: (a < 10)",
        ],
    ),
    "a constant in the select list": (
        "RP-333",
        _ANALYSED,
        "explain select 1 from e",
        ["Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)"],
    ),
    "a text expression": (
        "RP-333",
        _ANALYSED,
        "explain select b || 'x' from e",
        ["Seq Scan on e  (cost=0.00..17.50 rows=1000 width=32)"],
    ),
    "a folded constant expression": (
        "RP-333",
        _ANALYSED,
        "explain select 1 + 2, a from e",
        ["Seq Scan on e  (cost=0.00..15.00 rows=1000 width=8)"],
    ),
    # ------------- RP-339 a user table's index path, chosen (fixed)
    "an index scan with a filter": (
        "RP-339",
        _ANALYSED,
        "explain select * from k where a = 5 and b = 'v5'",
        [
            "Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=7)",
            "  Index Cond: (a = 5)",
            "  Filter: (b = 'v5'::text)",
        ],
    ),
    "an index scan of a key range": (
        "RP-339",
        _ANALYSED,
        "explain select * from k where a < 3",
        [
            "Index Scan using k_pkey on k  (cost=0.28..8.31 rows=2 width=7)",
            "  Index Cond: (a < 3)",
        ],
    ),
    "an index scan of a composite index's leading column": (
        "RP-339",
        _N,
        "explain select * from n where a = 7",
        [
            "Index Scan using n_ab on n  (cost=0.28..8.30 rows=1 width=11)",
            "  Index Cond: (a = 7)",
        ],
    ),
    "a bitmap heap scan of a user table": (
        "RP-339",
        _N,
        "explain select * from n where b = 7",
        [
            "Bitmap Heap Scan on n  (cost=5.06..34.31 rows=100 width=11)",
            "  Recheck Cond: (b = 7)",
            "  ->  Bitmap Index Scan on n_b  (cost=0.00..5.03 rows=100 width=0)",
            "        Index Cond: (b = 7)",
        ],
    ),
    # ------------- RP-378 a user table's index paths not yet chosen
    "a bitmap or of a key's two equalities": (
        "RP-378",
        _ANALYSED,
        "explain select * from k where a = 5 or a = 6",
        [
            "Bitmap Heap Scan on k  (cost=8.57..12.80 rows=2 width=7)",
            "  Recheck Cond: ((a = 5) OR (a = 6))",
            "  ->  BitmapOr  (cost=8.57..8.57 rows=2 width=0)",
            "        ->  Bitmap Index Scan on k_pkey  (cost=0.00..4.28 rows=1 width=0)",
            "              Index Cond: (a = 5)",
            "        ->  Bitmap Index Scan on k_pkey  (cost=0.00..4.28 rows=1 width=0)",
            "              Index Cond: (a = 6)",
        ],
    ),
    "an index-only scan under a computed select list": (
        "RP-378",
        _ANALYSED,
        "explain select a + 1 from k where a = 5",
        [
            "Index Only Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=4)",
            "  Index Cond: (a = 5)",
        ],
    ),
    "an index scan of a composite index's two columns": (
        "RP-378",
        _N,
        "explain select * from n where a = 7 and b = 7",
        [
            "Index Scan using n_ab on n  (cost=0.28..8.30 rows=1 width=11)",
            "  Index Cond: ((a = 7) AND (b = 7))",
        ],
    ),
    # RP-378's regression scenarios, measured on a private 15.19 cluster.
    "a bitmap or whose arm holds a clause no index serves": (
        "RP-378",
        _ANALYSED,
        "explain select * from k where (a = 5 and b = 'v1') or a = 6",
        [
            "Bitmap Heap Scan on k  (cost=8.57..12.81 rows=1 width=7)",
            "  Recheck Cond: ((a = 5) OR (a = 6))",
            "  Filter: (((a = 5) AND (b = 'v1'::text)) OR (a = 6))",
            "  ->  BitmapOr  (cost=8.57..8.57 rows=2 width=0)",
            "        ->  Bitmap Index Scan on k_pkey  (cost=0.00..4.28 rows=1 width=0)",
            "              Index Cond: (a = 5)",
            "        ->  Bitmap Index Scan on k_pkey  (cost=0.00..4.28 rows=1 width=0)",
            "              Index Cond: (a = 6)",
        ],
    ),
    "a bitmap or with an arm on a composite index's two columns": (
        "RP-378",
        _N,
        "explain select * from n where (a = 7 and b = 7) or a = 9",
        [
            "Bitmap Heap Scan on n  (cost=8.58..12.60 rows=1 width=11)",
            "  Recheck Cond: (((a = 7) AND (b = 7)) OR (a = 9))",
            "  ->  BitmapOr  (cost=8.58..8.58 rows=1 width=0)",
            "        ->  Bitmap Index Scan on n_ab  (cost=0.00..4.29 rows=1 width=0)",
            "              Index Cond: ((a = 7) AND (b = 7))",
            "        ->  Bitmap Index Scan on n_ab  (cost=0.00..4.29 rows=1 width=0)",
            "              Index Cond: (a = 9)",
        ],
    ),
    "a composite index's in list and a later equality": (
        "RP-378",
        _N,
        "explain select * from n where a in (1, 2, 3) and b = 2",
        [
            "Index Scan using n_ab on n  (cost=0.28..16.89 rows=1 width=11)",
            "  Index Cond: ((a = ANY ('{1,2,3}'::integer[])) AND (b = 2))",
        ],
    ),
    "a composite index's later range, in key order": (
        "RP-378",
        _N,
        "explain select * from n where a = 7 and b > 3",
        [
            "Index Scan using n_ab on n  (cost=0.28..8.30 rows=1 width=11)",
            "  Index Cond: ((a = 7) AND (b > 3))",
        ],
    ),
    "an index-only scan turned off": (
        "RP-378",
        _ANALYSED + ["set enable_indexonlyscan = off"],
        "explain select a from k where a = 5",
        [
            "Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=4)",
            "  Index Cond: (a = 5)",
        ],
    ),
    # ------------- RP-382 a node over a user table's index path, unpriced
    "a sort over a user table's bitmap scan": (
        "RP-382",
        _N,
        "explain select * from n where b = 7 order by a",
        [
            "Sort  (cost=37.63..37.88 rows=100 width=11)",
            "  Sort Key: a",
            "  ->  Bitmap Heap Scan on n  (cost=5.06..34.31 rows=100 width=11)",
            "        Recheck Cond: (b = 7)",
            "        ->  Bitmap Index Scan on n_b  (cost=0.00..5.03 rows=100 width=0)",
            "              Index Cond: (b = 7)",
        ],
    ),
    "a limit over a user table's index scan": (
        "RP-382",
        _ANALYSED,
        "explain select * from k where a = 5 limit 1",
        [
            "Limit  (cost=0.28..8.29 rows=1 width=7)",
            "  ->  Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=7)",
            "        Index Cond: (a = 5)",
        ],
    ),
    "a count over a user table's bitmap scan": (
        "RP-382",
        _N,
        "explain select count(*) from n where b = 7",
        [
            "Aggregate  (cost=34.56..34.57 rows=1 width=8)",
            "  ->  Bitmap Heap Scan on n  (cost=5.06..34.31 rows=100 width=0)",
            "        Recheck Cond: (b = 7)",
            "        ->  Bitmap Index Scan on n_b  (cost=0.00..5.03 rows=100 width=0)",
            "              Index Cond: (b = 7)",
        ],
    ),
    "a count over a user table's index-only scan": (
        "RP-382",
        _ANALYSED,
        "explain select count(*) from k where a = 5",
        [
            "Aggregate  (cost=8.29..8.30 rows=1 width=8)",
            "  ->  Index Only Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=0)",
            "        Index Cond: (a = 5)",
        ],
    ),
    # A LIMIT of a twentieth of the rows: the Seq Scan starts cheaper
    # (`get_cheapest_fractional_path`), not the cheapest-total bitmap scan.
    "a limit that reads a fraction of an indexed table": (
        "RP-382",
        _N,
        "explain select * from n where b = 7 limit 5",
        [
            "Limit  (cost=0.00..4.53 rows=5 width=11)",
            "  ->  Seq Scan on n  (cost=0.00..90.50 rows=100 width=11)",
            "        Filter: (b = 7)",
        ],
    ),
    "a limit above an index scan's rows": (
        "RP-382",
        _ANALYSED,
        "explain select * from k where a < 50 limit 100",
        [
            "Limit  (cost=0.28..9.13 rows=49 width=7)",
            "  ->  Index Scan using k_pkey on k  (cost=0.28..9.13 rows=49 width=7)",
            "        Index Cond: (a < 50)",
        ],
    ),
    # n_ab gives the order, but sorting the bitmap scan is cheaper.
    "a sort an index could give, cheaper sorted": (
        "RP-382",
        _N,
        "explain select * from n where a < 100 order by a",
        [
            "Sort  (cost=37.57..37.82 rows=99 width=11)",
            "  Sort Key: a",
            "  ->  Bitmap Heap Scan on n  (cost=5.05..34.29 rows=99 width=11)",
            "        Recheck Cond: (a < 100)",
            "        ->  Bitmap Index Scan on n_ab  (cost=0.00..5.02 rows=99 width=0)",
            "              Index Cond: (a < 100)",
        ],
    ),
    "a sort over a user table's index scan": (
        "RP-382",
        _ANALYSED,
        "explain select * from k where a = 5 order by b",
        [
            "Sort  (cost=8.30..8.31 rows=1 width=7)",
            "  Sort Key: b",
            "  ->  Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=7)",
            "        Index Cond: (a = 5)",
        ],
    ),
    # ------------- RP-386 EXPLAIN ANALYZE of a join's children
    # Fixed: a join's children print their own actual rows and loops, a
    # parameterized inner index-only scan its Heap Fetches, a Hash its
    # table and a nested loop its Rows Removed by Join Filter.
    "analyze of a nested loop's children": (
        "RP-386",
        _ANALYSED,
        "explain (analyze, costs off, timing off, summary off)"
        " select k.a from e, k where e.a < k.a",
        [
            "Nested Loop (actual rows=499500 loops=1)",
            "  ->  Seq Scan on e (actual rows=1000 loops=1)",
            "  ->  Index Only Scan using k_pkey on k (actual rows=500 loops=1000)",
            "        Index Cond: (a > e.a)",
            "        Heap Fetches: 499500",
        ],
    ),
    "analyze of a hash join's children": (
        "RP-386",
        _ANALYSED,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from e join e e2 on e.a = e2.a",
        [
            "Hash Join (actual rows=1000 loops=1)",
            "  Hash Cond: (e.a = e2.a)",
            "  ->  Seq Scan on e (actual rows=1000 loops=1)",
            "  ->  Hash (actual rows=1000 loops=1)",
            "        Buckets: 1024  Batches: 1  Memory Usage: 47kB",
            "        ->  Seq Scan on e e2 (actual rows=1000 loops=1)",
        ],
    ),
    "analyze of a materialized nested loop's join filter": (
        "RP-386",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off) select * from s, e where s.a < e.a",
        [
            "Nested Loop (actual rows=29535 loops=1)",
            "  Join Filter: (s.a < e.a)",
            "  Rows Removed by Join Filter: 465",
            "  ->  Seq Scan on e (actual rows=1000 loops=1)",
            "  ->  Materialize (actual rows=30 loops=1000)",
            "        ->  Seq Scan on s (actual rows=30 loops=1)",
        ],
    ),
    "analyze of a hash join of a narrower inner side": (
        "RP-386",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from s left join e on e.a = s.a",
        [
            "Hash Right Join (actual rows=30 loops=1)",
            "  Hash Cond: (e.a = s.a)",
            "  ->  Seq Scan on e (actual rows=1000 loops=1)",
            "  ->  Hash (actual rows=30 loops=1)",
            "        Buckets: 1024  Batches: 1  Memory Usage: 10kB",
            "        ->  Seq Scan on s (actual rows=30 loops=1)",
        ],
    ),
    # ------------- RP-389 a LEFT nested loop's Join Filter count
    "analyze of a left nested loop's join filter": (
        "RP-389",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from s left join e on s.a < e.a",
        [
            "Nested Loop Left Join (actual rows=29535 loops=1)",
            "  Join Filter: (s.a < e.a)",
            "  Rows Removed by Join Filter: 465",
            "  ->  Seq Scan on s (actual rows=30 loops=1)",
            "  ->  Materialize (actual rows=1000 loops=30)",
            "        ->  Seq Scan on e (actual rows=1000 loops=1)",
        ],
    ),
    # Written inner side first: the filter is still run as PostgreSQL wrote it.
    "analyze of a left nested loop's join filter, inner side first": (
        "RP-389",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from s left join e on e.a > s.a",
        [
            "Nested Loop Left Join (actual rows=29535 loops=1)",
            "  Join Filter: (e.a > s.a)",
            "  Rows Removed by Join Filter: 465",
            "  ->  Seq Scan on s (actual rows=30 loops=1)",
            "  ->  Materialize (actual rows=1000 loops=30)",
            "        ->  Seq Scan on e (actual rows=1000 loops=1)",
        ],
    ),
    # ------------- RP-390 a join over a scan with its own Filter
    # Split from RP-390 and fixed: a join's scans count the rows their
    # Filter rejects (`nfiltered1`), and the join's counts are over the
    # rows they kept.
    "analyze of a nested loop over a filtered scan": (
        "RP-386",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from s join e on s.a < e.a where e.b = 'v1'",
        [
            "Nested Loop (actual rows=2940 loops=1)",
            "  Join Filter: (s.a < e.a)",
            "  Rows Removed by Join Filter: 60",
            "  ->  Seq Scan on e (actual rows=100 loops=1)",
            "        Filter: (b = 'v1'::text)",
            "        Rows Removed by Filter: 900",
            "  ->  Materialize (actual rows=30 loops=100)",
            "        ->  Seq Scan on s (actual rows=30 loops=1)",
        ],
    ),
    # Split from RP-390 and fixed: the Materialized inner's Filter too.
    "analyze of a nested loop over two filtered scans": (
        "RP-386",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from s join e on s.a < e.a where e.b = 'v1' and s.a > 20",
        [
            "Nested Loop (actual rows=970 loops=1)",
            "  Join Filter: (s.a < e.a)",
            "  Rows Removed by Join Filter: 30",
            "  ->  Seq Scan on e (actual rows=100 loops=1)",
            "        Filter: (b = 'v1'::text)",
            "        Rows Removed by Filter: 900",
            "  ->  Materialize (actual rows=10 loops=100)",
            "        ->  Seq Scan on s (actual rows=10 loops=1)",
            "              Filter: (a > 20)",
            "              Rows Removed by Filter: 20",
        ],
    ),
    # Split from RP-390 and fixed: a LEFT join's Join Filter of any shape,
    # evaluated per pair as `ExecQual` runs it.
    "analyze of a left nested loop over an expression filter": (
        "RP-389",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from s left join e on s.a + 1 < e.a",
        [
            "Nested Loop Left Join (actual rows=29505 loops=1)",
            "  Join Filter: ((s.a + 1) < e.a)",
            "  Rows Removed by Join Filter: 495",
            "  ->  Seq Scan on s (actual rows=30 loops=1)",
            "  ->  Materialize (actual rows=1000 loops=30)",
            "        ->  Seq Scan on e (actual rows=1000 loops=1)",
        ],
    ),
    # Fixed: a LEFT join's ON conjunct over its nullable side alone was
    # part of its Join Filter; it is that side's scan Filter
    # (`distribute_qual_to_rels`).
    "analyze of a left nested loop over an expression filter and a filtered inner": (
        "RP-395",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from s left join e on s.a * 40 > e.a and e.b = 'v3'",
        [
            "Nested Loop Left Join (actual rows=1800 loops=1)",
            "  Join Filter: ((s.a * 40) > e.a)",
            "  Rows Removed by Join Filter: 1200",
            "  ->  Seq Scan on s (actual rows=30 loops=1)",
            "  ->  Materialize (actual rows=100 loops=30)",
            "        ->  Seq Scan on e (actual rows=100 loops=1)",
            "              Filter: (b = 'v3'::text)",
            "              Rows Removed by Filter: 900",
        ],
    ),
    # A LEFT join's ON conjunct over one table of an inner join on its
    # nullable side is that table's scan Filter too (found while working
    # RP-395).
    "a left join's ON conjunct over a table of an inner join it null-extends": (
        "RP-397",
        _ANALYSED_THIRTY,
        "explain (costs off)"
        " select * from s left join (e join k on e.a < k.a) on s.a = e.a and k.b = 'v3'",
        [
            "Hash Right Join",
            "  Hash Cond: (e.a = s.a)",
            "  ->  Nested Loop",
            "        Join Filter: (e.a < k.a)",
            "        ->  Seq Scan on e",
            "        ->  Materialize",
            "              ->  Seq Scan on k",
            "                    Filter: (b = 'v3'::text)",
            "  ->  Hash",
            "        ->  Seq Scan on s",
        ],
    ),
    # A join's scan Filter in PostgreSQL's qual order: an equality to a
    # constant after the rest (`generate_base_implied_equalities`), as a
    # lone scan prints it (found while working RP-395).
    "a joined scan's filter puts its equality to a constant last": (
        "RP-398",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from s join e on s.a < e.a where e.b = 'v3' and e.a > 5",
        [
            "Nested Loop",
            "  Join Filter: (s.a < e.a)",
            "  ->  Seq Scan on e",
            "        Filter: ((a > 5) AND (b = 'v3'::text))",
            "  ->  Materialize",
            "        ->  Seq Scan on s",
        ],
    ),
    # BETWEEN is two quals of the scan's one list, printed flat (found
    # while working RP-395).
    "a scan's between is two flat conjuncts": (
        "RP-399",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from e where a between 15 and 55 and b <> 'v3'",
        [
            "Seq Scan on e",
            "  Filter: ((a >= 15) AND (a <= 55) AND (b <> 'v3'::text))",
        ],
    ),
    # RP-399's regression scenarios, measured on a private 15.19 cluster:
    # a joined scan's BETWEEN is flat too, an index takes its two clauses
    # as two Index Conds, and a BETWEEN over a LEFT join's nullable side
    # proves it non-NULL as its two clauses do (`reduce_outer_joins`).
    "a joined scan's between is two flat conjuncts": (
        "RP-399",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from s join e on s.a = e.a"
        " where e.a between 15 and 55 and e.b <> 'v3'",
        [
            "Hash Join",
            "  Hash Cond: (e.a = s.a)",
            "  ->  Seq Scan on e",
            "        Filter: ((a >= 15) AND (a <= 55) AND (b <> 'v3'::text))",
            "  ->  Hash",
            "        ->  Seq Scan on s",
        ],
    ),
    "a where between over a left join's nullable side makes it inner": (
        "RP-399",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from s left join e on s.a = e.a where e.a between 15 and 55",
        [
            "Hash Join",
            "  Hash Cond: (e.a = s.a)",
            "  ->  Seq Scan on e",
            "        Filter: ((a >= 15) AND (a <= 55))",
            "  ->  Hash",
            "        ->  Seq Scan on s",
        ],
    ),
    "a composite index's between is two index conds": (
        "RP-399",
        _N,
        "explain select * from n where a between 7 and 9 and b = 3",
        [
            "Index Scan using n_ab on n  (cost=0.28..8.33 rows=1 width=11)",
            "  Index Cond: ((a >= 7) AND (a <= 9) AND (b = 3))",
        ],
    ),
    "a composite index's second column between": (
        "RP-399",
        _N,
        "explain (costs off) select * from n where a = 7 and b between 1 and 3",
        [
            "Index Scan using n_ab on n",
            "  Index Cond: ((a = 7) AND (b >= 1) AND (b <= 3))",
        ],
    ),
    # NOT BETWEEN is an OR, which costs its two operators: after a
    # cheaper conjunct (`order_qual_clauses`; found while working RP-399).
    "a scan filter's or after a cheaper conjunct": (
        "RP-406",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from e where a not between 15 and 55 and b <> 'v3'",
        [
            "Seq Scan on e",
            "  Filter: ((b <> 'v3'::text) AND ((a < 15) OR (a > 55)))",
        ],
    ),
    # A strict operator over a NULL constant folded to NULL
    # (`eval_const_expressions`), and a NULL qual is a constant false:
    # no scan at all (found while working RP-399). The table has no
    # primary key, and the plan named `e_pkey`.
    "a comparison with a null constant is a false one-time filter": (
        "RP-409",
        _ANALYSED,
        "explain (costs off) select * from e where a = null",
        ["Result", "  One-Time Filter: false"],
    ),
    "a between with a null bound is a false one-time filter": (
        "RP-409",
        _ANALYSED,
        "explain (costs off) select * from e where a between null and 15 and b <> 'v3'",
        ["Result", "  One-Time Filter: false"],
    ),
    # The rest of RP-409's simplification, each measured on a private
    # 15.19 cluster: an OR drops a NULL arm and is gone with a TRUE one,
    # an AND with a FALSE is false, a null test of a NULL is TRUE, and
    # the dummy relation sits under an Aggregate, a join or a DELETE.
    "an or drops its null arm": (
        "RP-409",
        _ANALYSED,
        "explain (costs off) select * from e where a = null or b = 'v3'",
        ["Seq Scan on e", "  Filter: (b = 'v3'::text)"],
    ),
    "an or with a true arm is no filter": (
        "RP-409",
        _ANALYSED,
        "explain (costs off) select * from e where a > 5 or true",
        ["Seq Scan on e"],
    ),
    "an and with a false arm is a false one-time filter": (
        "RP-409",
        _ANALYSED,
        "explain (costs off) select * from e where a > 5 and false",
        ["Result", "  One-Time Filter: false"],
    ),
    "a constant false where on a table without a primary key": (
        "RP-409",
        _ANALYSED,
        "explain (costs off) select * from e where false",
        ["Result", "  One-Time Filter: false"],
    ),
    "a null test of a null comparison is true": (
        "RP-409",
        _ANALYSED,
        "explain (costs off) select * from e where a = null is null",
        ["Seq Scan on e"],
    ),
    "an aggregate over a dummy relation": (
        "RP-409",
        _ANALYSED,
        "explain (costs off) select count(*) from e where a = null",
        ["Aggregate", "  ->  Result", "        One-Time Filter: false"],
    ),
    "a join with a null comparison is dummy": (
        "RP-409",
        _ANALYSED,
        "explain (costs off) select * from e join k on e.a = k.a where k.a = null",
        ["Result", "  One-Time Filter: false"],
    ),
    "a delete with a null comparison scans nothing": (
        "RP-409",
        _ANALYSED,
        "explain (costs off) delete from e where a = null",
        ["Delete on e", "  ->  Result", "        One-Time Filter: false"],
    ),
    "two constants and a null are one false filter": (
        "RP-409",
        _ANALYSED,
        "explain (costs off) select * from e where a = 1 and a = 2 and null",
        ["Result", "  One-Time Filter: false"],
    ),
    "analyze of a dummy relation": (
        "RP-409",
        _ANALYSED,
        "explain (analyze, costs off, timing off, summary off) select * from e where a = null",
        ["Result (actual rows=0 loops=1)", "  One-Time Filter: false"],
    ),
    # Nodes above a dummy relation are still priced, and its Result keeps
    # a join's width (found while working RP-409).
    "an aggregate over a dummy relation is priced": (
        "RP-415",
        _ANALYSED,
        "explain select count(*) from e where a = null",
        [
            "Aggregate  (cost=0.00..0.01 rows=1 width=8)",
            "  ->  Result  (cost=0.00..0.00 rows=0 width=0)",
            "        One-Time Filter: false",
        ],
    ),
    "a dummy join keeps its width": (
        "RP-415",
        _ANALYSED,
        "explain select * from e, k where e.a = null",
        ["Result  (cost=0.00..0.00 rows=0 width=14)", "  One-Time Filter: false"],
    ),
    # `eval_const_expressions` folds a COALESCE's constant arguments
    # (found while working RP-409).
    "a coalesce drops its null arguments": (
        "RP-416",
        _ANALYSED,
        "explain (costs off) select * from e where coalesce(null, a) = 1",
        ["Seq Scan on e", "  Filter: (COALESCE(a) = 1)"],
    ),
    "a coalesce of a constant first folds": (
        "RP-416",
        _ANALYSED,
        "explain (costs off) select * from e where coalesce(1, a) = 1",
        ["Seq Scan on e"],
    ),
    # `pull_up_subqueries` merges a simple FROM-subquery into its parent
    # (found while working RP-409).
    "a plain from-subquery is pulled up": (
        "RP-417",
        _ANALYSED,
        "explain (costs off) select * from (select * from e where a > 5) s",
        ["Seq Scan on e", "  Filter: (a > 5)"],
    ),
    # A table whose only index is a composite one, its WHERE constraining
    # both columns (found while working RP-399).
    "a table's only index composite": (
        "RP-410",
        [
            "create table t2 (a int, b int, c text)",
            "insert into t2 select g, g % 50, 'x' || (g % 7) from generate_series(1, 5000) g",
            "create index t2_ab on t2 (a, b)",
            "analyze t2",
        ],
        "explain select * from t2 where a = 15 and b = 3",
        [
            "Index Scan using t2_ab on t2  (cost=0.28..8.30 rows=1 width=11)",
            "  Index Cond: ((a = 15) AND (b = 3))",
        ],
    ),
    # An IN or EXISTS subquery pulled up into a semi join
    # (`convert_ANY_sublink_to_join`, `convert_EXISTS_sublink_to_join`;
    # found while working RP-399).
    "an in subquery is a hash semi join": (
        "RP-411",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from e where a in (select a from s)",
        [
            "Hash Semi Join",
            "  Hash Cond: (e.a = s.a)",
            "  ->  Seq Scan on e",
            "  ->  Hash",
            "        ->  Seq Scan on s",
        ],
    ),
    "an exists subquery is a hash semi join": (
        "RP-411",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from e where exists (select 1 from s where s.a = e.a)",
        [
            "Hash Semi Join",
            "  Hash Cond: (e.a = s.a)",
            "  ->  Seq Scan on e",
            "  ->  Hash",
            "        ->  Seq Scan on s",
        ],
    ),
    # NOT is pushed through AND / OR onto the negator operators
    # (`negate_clause`; found while working RP-399).
    "a not over an or is the and of its negated arms": (
        "RP-412",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from e where not (a > 15 or b = 'v3')",
        ["Seq Scan on e", "  Filter: ((a <= 15) AND (b <> 'v3'::text))"],
    ),
    # With a NULL arm the negated arms' AND is constant false (found
    # while working RP-409).
    "a not over an or with a null arm is a false one-time filter": (
        "RP-412",
        _ANALYSED,
        "explain (costs off) select * from e where not (a > 1 or null)",
        ["Result", "  One-Time Filter: false"],
    ),
    "a not over a between is the or of its negators": (
        "RP-412",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from e where not (a between 15 and 55)",
        ["Seq Scan on e", "  Filter: ((a < 15) OR (a > 55))"],
    ),
    # A LEFT join on an indexed key is a Merge Right Join reading the
    # index in key order, its other side sorted (found while working
    # RP-395).
    "a left join on a primary key merges through its index": (
        "RP-401",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from s left join k on s.a = k.a",
        [
            "Merge Right Join",
            "  Merge Cond: (k.a = s.a)",
            "  ->  Index Scan using k_pkey on k",
            "  ->  Sort",
            "        Sort Key: s.a",
            "        ->  Seq Scan on s",
        ],
    ),
    # Two different constants equal to one column of a joined table make
    # the class contradictory: the whole join is a false One-Time Filter
    # (found while working RP-395).
    "a join whose where equates a column to two constants": (
        "RP-400",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from s join e on s.a < e.a where e.b = 'v1' and e.b = 'v2'",
        [
            "Result",
            "  One-Time Filter: false",
        ],
    ),
    # Split from RP-390 (as RP-403) and fixed: a Hash Join whose scans each have their
    # own Filter, one of them a range on the join key, counts each scan's
    # rows and Rows Removed by Filter as `ExecScan` does.
    "analyze of a hash join over two filtered scans": (
        "RP-403",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from s join e on s.a = e.a where e.b = 'v1' and s.a > 5",
        [
            "Hash Join (actual rows=2 loops=1)",
            "  Hash Cond: (e.a = s.a)",
            "  ->  Seq Scan on e (actual rows=100 loops=1)",
            "        Filter: (b = 'v1'::text)",
            "        Rows Removed by Filter: 900",
            "  ->  Hash (actual rows=25 loops=1)",
            "        Buckets: 1024  Batches: 1  Memory Usage: 10kB",
            "        ->  Seq Scan on s (actual rows=25 loops=1)",
            "              Filter: (a > 5)",
            "              Rows Removed by Filter: 5",
        ],
    ),
    "analyze of a hash join over a filtered hashed side": (
        "RP-403",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from e e1 join e e2 on e1.a = e2.a where e1.b = 'v1'",
        [
            "Hash Join (actual rows=100 loops=1)",
            "  Hash Cond: (e2.a = e1.a)",
            "  ->  Seq Scan on e e2 (actual rows=1000 loops=1)",
            "  ->  Hash (actual rows=100 loops=1)",
            "        Buckets: 1024  Batches: 1  Memory Usage: 12kB",
            "        ->  Seq Scan on e e1 (actual rows=100 loops=1)",
            "              Filter: (b = 'v1'::text)",
            "              Rows Removed by Filter: 900",
        ],
    ),
    # A range on the join key restricts its scan and leaves the join
    # clause in place (`distribute_restrictinfo_to_rels`); only an
    # equality joins the key's equivalence class. The join was unpriced.
    "hash join over a range on each side's key": (
        "RP-403",
        _ANALYSED,
        "explain select * from e join e e2 on e.a = e2.a where e.a < 100 and e2.a > 50",
        [
            "Hash Join  (cost=18.74..40.74 rows=94 width=14)",
            "  Hash Cond: (e2.a = e.a)",
            "  ->  Seq Scan on e e2  (cost=0.00..17.50 rows=950 width=7)",
            "        Filter: (a > 50)",
            "  ->  Hash  (cost=17.50..17.50 rows=99 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..17.50 rows=99 width=7)",
            "              Filter: (a < 100)",
        ],
    ),
    # A parameterized Nested Loop over a filtered outer scan: a comma
    # join's, whose WHERE also holds the join clause, and a JOIN's.
    "analyze of a parameterized nested loop over a filtered scan": (
        "RP-403",
        _ANALYSED,
        "explain (analyze, costs off, timing off, summary off)"
        " select k.a from e, k where e.a < k.a and e.b = 'v1'",
        [
            "Nested Loop (actual rows=50400 loops=1)",
            "  ->  Seq Scan on e (actual rows=100 loops=1)",
            "        Filter: (b = 'v1'::text)",
            "        Rows Removed by Filter: 900",
            "  ->  Index Only Scan using k_pkey on k (actual rows=504 loops=100)",
            "        Index Cond: (a > e.a)",
            "        Heap Fetches: 50400",
        ],
    ),
    "analyze of a parameterized nested loop under a join's WHERE": (
        "RP-403",
        _ANALYSED,
        "explain (analyze, costs off, timing off, summary off)"
        " select k.a from e join k on e.a < k.a where e.a > 990",
        [
            "Nested Loop (actual rows=45 loops=1)",
            "  ->  Seq Scan on e (actual rows=10 loops=1)",
            "        Filter: (a > 990)",
            "        Rows Removed by Filter: 990",
            "  ->  Index Only Scan using k_pkey on k (actual rows=4 loops=10)",
            "        Index Cond: (a > e.a)",
            "        Heap Fetches: 45",
        ],
    ),
    # A LEFT join filter over a char(n) column: still unannotated (RP-390's
    # remainder).
    "analyze of a left nested loop over a char(n) join filter": (
        "RP-390",
        _ANALYSED_THIRTY
        + [
            "create table cc (a int, b char(4))",
            "insert into cc select g, 'v' || (g % 10) from generate_series(1, 30) g",
            "analyze cc",
        ],
        "explain (analyze, costs off, timing off, summary off)"
        " select * from cc left join s on cc.b < s.c",
        [
            "Nested Loop Left Join (actual rows=900 loops=1)",
            "  Join Filter: ((cc.b)::text < s.c)",
            "  ->  Seq Scan on cc (actual rows=30 loops=1)",
            "  ->  Materialize (actual rows=30 loops=30)",
            "        ->  Seq Scan on s (actual rows=30 loops=1)",
        ],
    ),
    # A scan Filter that is an expression or a LIKE: `scalarineqsel`'s
    # default 1/3 for an expression, `patternsel`'s prefix selectivity
    # for `c like 'x%'`. Here the scan prints zeros (found while working
    # RP-403).
    "estimate of a scan filtered by an expression": (
        "RP-404",
        _ANALYSED,
        "explain select * from e where a + 1 > 996",
        [
            "Seq Scan on e  (cost=0.00..20.00 rows=333 width=7)",
            "  Filter: ((a + 1) > 996)",
        ],
    ),
    "estimate of a scan filtered by a LIKE prefix": (
        "RP-404",
        _ANALYSED_THIRTY,
        "explain select * from s where c like 'x%'",
        [
            "Seq Scan on s  (cost=0.00..1.38 rows=30 width=20)",
            "  Filter: (c ~~ 'x%'::text)",
        ],
    ),
    # A join's scan whose WHERE conjunct is an expression printed a
    # `Filter: (...)` line above its real one (found while working RP-403).
    "a join's scan filtered by an expression": (
        "RP-405",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from e left join s on s.a = e.a where e.a + 1 > 996",
        [
            "Hash Left Join",
            "  Hash Cond: (e.a = s.a)",
            "  ->  Seq Scan on e",
            "        Filter: ((a + 1) > 996)",
            "  ->  Hash",
            "        ->  Seq Scan on s",
        ],
    ),
    # The same below an inner join, and for an ON conjunct over the
    # nullable side, which printed the placeholder too.
    "an inner join's scan filtered by an expression": (
        "RP-405",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from e join s on s.a = e.a and e.a + 1 > 996",
        [
            "Hash Join",
            "  Hash Cond: (e.a = s.a)",
            "  ->  Seq Scan on e",
            "        Filter: ((a + 1) > 996)",
            "  ->  Hash",
            "        ->  Seq Scan on s",
        ],
    ),
    # A WHERE over an expression of the nullable side reduces the LEFT join
    # (`find_nonnullable_rels` descends into a strict operator), and its
    # conjunct filters that side's scan. The plan reduced it while EXPLAIN
    # kept the conjunct in the join's `Join Filter`, the scan printing only
    # `Filter: (...)`.
    "a left join reduced by a where over an expression": (
        "RP-405",
        _ANALYSED_THIRTY,
        "explain (costs off) select * from e left join s on s.a = e.a where s.a * 2 > 4",
        [
            "Hash Join",
            "  Hash Cond: (e.a = s.a)",
            "  ->  Seq Scan on e",
            "  ->  Hash",
            "        ->  Seq Scan on s",
            "              Filter: ((a * 2) > 4)",
        ],
    ),
    # An integer literal coerced to numeric for its operator is folded to
    # a typed literal (`eval_const_expressions`) before the Filter is
    # printed; here the coercion prints unfolded (found while working
    # RP-405).
    "a scan filter's coerced constant is folded": (
        "RP-407",
        _ANALYSED,
        "explain (costs off) select * from e where a::numeric > 4",
        [
            "Seq Scan on e",
            "  Filter: ((a)::numeric > '4'::numeric)",
        ],
    ),
    # A scan's Filter lists its conjuncts cheapest first
    # (`order_qual_clauses`): the operator over a column before the one
    # over an expression. Here they keep the WHERE's order (found while
    # working RP-402).
    "a scan filter's conjuncts in cost order": (
        "RP-406",
        _ANALYSED,
        "explain (costs off) select * from e where a + 1 > 996 and b = 'v1'",
        [
            "Seq Scan on e",
            "  Filter: ((b = 'v1'::text) AND ((a + 1) > 996))",
        ],
    ),
    # An inner join's ON conjunct over one table is that table's scan
    # Filter (`distribute_qual_to_rels`), not part of the Join Filter.
    "analyze of an inner join's one-table ON conjunct": (
        "RP-391",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from s join e on s.a < e.a and s.a > 25",
        [
            "Nested Loop (actual rows=4860 loops=1)",
            "  Join Filter: (s.a < e.a)",
            "  Rows Removed by Join Filter: 140",
            "  ->  Seq Scan on e (actual rows=1000 loops=1)",
            "  ->  Materialize (actual rows=5 loops=1000)",
            "        ->  Seq Scan on s (actual rows=5 loops=1)",
            "              Filter: (a > 25)",
            "              Rows Removed by Filter: 25",
        ],
    ),
    # Fixed: every ON conjunct over one table is its table's scan Filter,
    # and the join was a plain cross product with no Join Filter.
    "analyze of an inner join whose ON conjuncts each read one table": (
        "RP-392",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from s join e on s.a > 25 and e.a > 990",
        [
            "Nested Loop (actual rows=50 loops=1)",
            "  ->  Seq Scan on e (actual rows=10 loops=1)",
            "        Filter: (a > 990)",
            "        Rows Removed by Filter: 990",
            "  ->  Materialize (actual rows=5 loops=10)",
            "        ->  Seq Scan on s (actual rows=5 loops=1)",
            "              Filter: (a > 25)",
            "              Rows Removed by Filter: 25",
        ],
    ),
    # An inner join nested inside an outer join: its one-table ON conjunct
    # is still its table's scan Filter (found while working RP-392).
    "analyze of an inner join under an outer join with a one-table ON conjunct": (
        "RP-393",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from s left join (e join k on e.a > 998 and k.a = e.a) on true",
        [
            "Nested Loop Left Join (actual rows=60 loops=1)",
            "  ->  Seq Scan on s (actual rows=30 loops=1)",
            "  ->  Materialize (actual rows=2 loops=30)",
            "        ->  Nested Loop (actual rows=2 loops=1)",
            "              ->  Seq Scan on e (actual rows=2 loops=1)",
            "                    Filter: (a > 998)",
            "                    Rows Removed by Filter: 998",
            "              ->  Index Scan using k_pkey on k (actual rows=1 loops=2)",
            "                    Index Cond: (a = e.a)",
        ],
    ),
    # A join's equivalence classes drop a repeated conjunct
    # (`process_equivalence` finds it redundant): the equality between the
    # tables is one hash clause, priced once (found while working RP-388).
    "comma join with a repeated join equality": (
        "RP-402",
        _ANALYSED,
        "explain select * from e, k where e.b = k.b and e.b = k.b",
        [
            "Hash Join  (cost=27.50..1170.00 rows=100000 width=14)",
            "  Hash Cond: (e.b = k.b)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=15.00..15.00 rows=1000 width=7)",
            "        ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
        ],
    ),
    # ... and a repeated `column = constant` is one scan Filter.
    "comma join with a repeated constant equality": (
        "RP-402",
        _ANALYSED,
        "explain select * from e, k where e.b = k.b and e.a = 5 and e.a = 5",
        [
            "Hash Join  (cost=17.51..37.26 rows=100 width=14)",
            "  Hash Cond: (k.b = e.b)",
            "  ->  Seq Scan on k  (cost=0.00..15.00 rows=1000 width=7)",
            "  ->  Hash  (cost=17.50..17.50 rows=1 width=7)",
            "        ->  Seq Scan on e  (cost=0.00..17.50 rows=1 width=7)",
            "              Filter: (a = 5)",
        ],
    ),
    # A constant true conjunct of an ON was printed as a Join Filter; it is
    # folded away, as `eval_const_expressions` does (found while working
    # RP-392, fixed).
    "analyze of an inner join whose ON has a constant true conjunct": (
        "RP-394",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select s.a, e.a from s join e on true and s.a > 29",
        [
            "Nested Loop (actual rows=1000 loops=1)",
            "  ->  Seq Scan on s (actual rows=1 loops=1)",
            "        Filter: (a > 29)",
            "        Rows Removed by Filter: 29",
            "  ->  Seq Scan on e (actual rows=1000 loops=1)",
        ],
    ),
    # A LEFT Nested Loop with no Join Filter printed no counts below it
    # (found while working RP-394, fixed).
    "analyze of a left join on true": (
        "RP-396",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select s.a, e.a from s left join e on true",
        [
            "Nested Loop Left Join (actual rows=30000 loops=1)",
            "  ->  Seq Scan on s (actual rows=30 loops=1)",
            "  ->  Materialize (actual rows=1000 loops=30)",
            "        ->  Seq Scan on e (actual rows=1000 loops=1)",
        ],
    ),
    "analyze of a left join whose ON has a constant true conjunct": (
        "RP-394",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select s.a, e.a from s left join e on true and s.a < e.a",
        [
            "Nested Loop Left Join (actual rows=29535 loops=1)",
            "  Join Filter: (s.a < e.a)",
            "  Rows Removed by Join Filter: 465",
            "  ->  Seq Scan on s (actual rows=30 loops=1)",
            "  ->  Materialize (actual rows=1000 loops=30)",
            "        ->  Seq Scan on e (actual rows=1000 loops=1)",
        ],
    ),
    "analyze of an inner join on true": (
        "RP-394",
        _ANALYSED_THIRTY,
        "explain (analyze, costs off, timing off, summary off)"
        " select s.a, e.a from s join e on true",
        [
            "Nested Loop (actual rows=30000 loops=1)",
            "  ->  Seq Scan on e (actual rows=1000 loops=1)",
            "  ->  Materialize (actual rows=30 loops=1000)",
            "        ->  Seq Scan on s (actual rows=30 loops=1)",
        ],
    ),
    # ------------- RP-383 EXPLAIN ANALYZE counters of a user table's scans
    "analyze of a user table's index-only scan": (
        "RP-383",
        _ANALYSED,
        "explain (analyze, costs off, timing off, summary off) select a from k where a = 5",
        [
            "Index Only Scan using k_pkey on k (actual rows=1 loops=1)",
            "  Index Cond: (a = 5)",
            "  Heap Fetches: 1",
        ],
    ),
    "analyze of a user table's bitmap scan": (
        "RP-383",
        _N,
        "explain (analyze, costs off, timing off, summary off) select * from n where b = 7",
        [
            "Bitmap Heap Scan on n (actual rows=100 loops=1)",
            "  Recheck Cond: (b = 7)",
            "  Heap Blocks: exact=27",
            "  ->  Bitmap Index Scan on n_b (actual rows=100 loops=1)",
            "        Index Cond: (b = 7)",
        ],
    ),
    # ----------------------- RP-384 what ANALYZE's scan counters still miss
    # Split from RP-384 and fixed: an index scan's Filter counts the rows
    # its Index Cond fetched and it rejected (`nfiltered1`).
    "analyze of an index scan's filter": (
        "RP-383",
        _ANALYSED,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from k where a < 30 and b = 'v5'",
        [
            "Index Scan using k_pkey on k (actual rows=3 loops=1)",
            "  Index Cond: (a < 30)",
            "  Filter: (b = 'v5'::text)",
            "  Rows Removed by Filter: 26",
        ],
    ),
    "analyze of an index-only scan over a dead version": (
        "RP-384",
        _ANALYSED + ["update k set b = 'z' where a = 5"],
        "explain (analyze, costs off, timing off, summary off) select a from k where a = 5",
        [
            "Index Only Scan using k_pkey on k (actual rows=1 loops=1)",
            "  Index Cond: (a = 5)",
            "  Heap Fetches: 2",
        ],
    ),
    # Split from RP-384 and fixed: a BitmapOr prints `actual rows=0`, each
    # arm's Bitmap Index Scan its own tuples, and the heap scan the pages
    # of their union.
    "analyze of a BitmapOr": (
        "RP-383",
        _N,
        "explain (analyze, costs off, timing off, summary off)"
        " select * from n where b = 7 or b = 9",
        [
            "Bitmap Heap Scan on n (actual rows=200 loops=1)",
            "  Recheck Cond: ((b = 7) OR (b = 9))",
            "  Heap Blocks: exact=27",
            "  ->  BitmapOr (actual rows=0 loops=1)",
            "        ->  Bitmap Index Scan on n_b (actual rows=100 loops=1)",
            "              Index Cond: (b = 7)",
            "        ->  Bitmap Index Scan on n_b (actual rows=100 loops=1)",
            "              Index Cond: (b = 9)",
        ],
    ),
    # --------------------------------- RP-334 / RP-335 a catalog's index
    "a catalog read by its index": (
        "RP-334",
        [],
        "explain select oid from pg_class where relname = 'pg_class'",
        [
            "Index Scan using pg_class_relname_nsp_index on pg_class"
            "  (cost=0.27..8.29 rows=1 width=4)",
            "  Index Cond: (relname = 'pg_class'::name)",
        ],
    ),
    "a name compared with a literal": (
        "RP-335",
        ["set local enable_indexscan = off", "set local enable_bitmapscan = off"],
        "explain (costs off) select oid from pg_class where relname = 'pg_class'",
        ["Seq Scan on pg_class", "  Filter: (relname = 'pg_class'::name)"],
    ),
    "a catalog read by its oid index": (
        "RP-334",
        [],
        "explain select relname from pg_class where oid = 1259",
        [
            "Index Scan using pg_class_oid_index on pg_class  (cost=0.27..8.29 rows=1 width=64)",
            "  Index Cond: (oid = '1259'::oid)",
        ],
    ),
    "a type read by its oid index": (
        "RP-334",
        [],
        "explain select typname from pg_type where oid = 23",
        [
            "Index Scan using pg_type_oid_index on pg_type  (cost=0.28..8.29 rows=1 width=64)",
            "  Index Cond: (oid = '23'::oid)",
        ],
    ),
    "a function read by its name index": (
        "RP-334",
        [],
        "explain select oid from pg_proc where proname = 'now'",
        [
            "Index Scan using pg_proc_proname_args_nsp_index on pg_proc"
            "  (cost=0.28..8.30 rows=1 width=4)",
            "  Index Cond: (proname = 'now'::name)",
        ],
    ),
    "a catalog index scan with a filter": (
        "RP-334",
        [],
        "explain select oid, relkind from pg_class where relname = 'pg_type' and relkind = 'r'",
        [
            "Index Scan using pg_class_relname_nsp_index on pg_class"
            "  (cost=0.27..8.29 rows=1 width=5)",
            "  Index Cond: (relname = 'pg_type'::name)",
            "  Filter: (relkind = 'r'::\"char\")",
        ],
    ),
    "a small catalog is read sequentially": (
        "RP-334",
        [],
        "explain select oid from pg_namespace where nspname = 'public'",
        [
            "Seq Scan on pg_namespace  (cost=0.00..1.05 rows=1 width=4)",
            "  Filter: (nspname = 'public'::name)",
        ],
    ),
    "a catalog index scan without costs": (
        "RP-334",
        [],
        "explain (costs off) select oid from pg_class where relname = 'pg_class'",
        [
            "Index Scan using pg_class_relname_nsp_index on pg_class",
            "  Index Cond: (relname = 'pg_class'::name)",
        ],
    ),
    "a catalog seq scan turned off": (
        "RP-334",
        ["set local enable_seqscan = off"],
        "explain select oid from pg_class where relkind = 'r'",
        [
            "Seq Scan on pg_class  (cost=10000000000.00..10000000019.12 rows=68 width=4)",
            "  Filter: (relkind = 'r'::\"char\")",
        ],
    ),
    "a catalog index scan as json": (
        "RP-334",
        [],
        "explain (format json) select oid from pg_class where relname = 'pg_class'",
        [
            '[\n  {\n    "Plan": {\n      "Node Type": "Index Scan",\n'
            '      "Parallel Aware": false,\n      "Async Capable": false,\n'
            '      "Scan Direction": "Forward",\n'
            '      "Index Name": "pg_class_relname_nsp_index",\n'
            '      "Relation Name": "pg_class",\n      "Alias": "pg_class",\n'
            '      "Startup Cost": 0.27,\n      "Total Cost": 8.29,\n'
            '      "Plan Rows": 1,\n      "Plan Width": 4,\n'
            '      "Index Cond": "(relname = \'pg_class\'::name)"\n'
            "    }\n  }\n]"
        ],
    ),
    # ------------------------------------------------ found while working
    "a result of a function call": (
        "RP-340",
        [],
        "explain select current_setting('work_mem')",
        ["Result  (cost=0.00..0.01 rows=1 width=32)"],
    ),
    # Four calls cost 0.01 + 4 x 0.0025 = 0.02: the rounding shows each
    # call's cpu_operator_cost (PostgreSQL 15.19, fresh cluster).
    "a result of four session_user": (
        "RP-340",
        [],
        "explain select session_user, session_user, session_user, session_user",
        ["Result  (cost=0.00..0.02 rows=1 width=256)"],
    ),
    "a result of four void calls": (
        "RP-340",
        [],
        "explain select pg_sleep(0), pg_sleep(0), pg_sleep(0), pg_sleep(0)",
        ["Result  (cost=0.00..0.02 rows=1 width=16)"],
    ),
    "a result of nested calls": (
        "RP-340",
        [],
        "explain select pg_terminate_backend(pg_backend_pid()),"
        " pg_terminate_backend(pg_backend_pid())",
        ["Result  (cost=0.00..0.02 rows=1 width=2)"],
    ),
    "a result of four nextval": (
        "RP-340",
        ["create sequence rp340s"],
        "explain select nextval('rp340s'), nextval('rp340s'), nextval('rp340s'), nextval('rp340s')",
        ["Result  (cost=0.00..0.02 rows=1 width=32)"],
    ),
    "a result's one-time filter": (
        "RP-341",
        [],
        "explain select 1 where false",
        ["Result  (cost=0.00..0.01 rows=1 width=4)", "  One-Time Filter: false"],
    ),
    "a result's one-time filter folded to null": (
        "RP-341",
        [],
        "explain select 1 where 1 = null",
        ["Result  (cost=0.00..0.01 rows=1 width=4)", "  One-Time Filter: NULL::boolean"],
    ),
    "a result's one-time filter folded from operators": (
        "RP-341",
        [],
        "explain select 1, 'a' where 1 = 2 or false",
        ["Result  (cost=0.00..0.01 rows=1 width=36)", "  One-Time Filter: false"],
    ),
    "a result's one-time filter of a call": (
        "RP-342",
        [],
        "explain select 1 where current_setting('work_mem') <> ''",
        [
            "Result  (cost=0.01..0.01 rows=1 width=4)",
            "  One-Time Filter: (current_setting('work_mem'::text) <> ''::text)",
        ],
    ),
    "a result's true where drops its filter": (
        "RP-341",
        [],
        "explain select 1 where 1 = 1",
        ["Result  (cost=0.00..0.01 rows=1 width=4)"],
    ),
    "a catalog read index-only": (
        "RP-337",
        [],
        "explain select attname from pg_attribute where attrelid = 1259",
        [
            "Index Only Scan using pg_attribute_relid_attnam_index on pg_attribute"
            "  (cost=0.28..4.96 rows=39 width=64)",
            "  Index Cond: (attrelid = '1259'::oid)",
        ],
    ),
    "an index-only scan's heap fetches": (
        "RP-360",
        [],
        "explain (analyze, timing off, summary off)"
        " select attname from pg_attribute where attrelid = 1259",
        [
            "Index Only Scan using pg_attribute_relid_attnam_index on pg_attribute"
            "  (cost=0.28..4.96 rows=39 width=64) (actual rows=39 loops=1)",
            "  Index Cond: (attrelid = '1259'::oid)",
            "  Heap Fetches: 0",
        ],
    ),
    "a catalog read by a bitmap": (
        "RP-338",
        ["set local enable_indexscan = off"],
        "explain select oid from pg_class where relname = 'pg_class'",
        [
            "Bitmap Heap Scan on pg_class  (cost=4.28..8.29 rows=1 width=4)",
            "  Recheck Cond: (relname = 'pg_class'::name)",
            "  ->  Bitmap Index Scan on pg_class_relname_nsp_index"
            "  (cost=0.00..4.28 rows=1 width=0)",
            "        Index Cond: (relname = 'pg_class'::name)",
        ],
    ),
}


def run(dsn: str, setup: list[str], query: str) -> list[str]:
    with psycopg.connect(dsn) as c:
        # A JSON plan as its text, keys in the order printed.
        c.adapters.register_loader("json", TextLoader)
        try:
            for s in setup:
                c.execute(s)
            return [str(r[0]) for r in c.execute(query)]
        finally:
            c.rollback()


def _marked(name: str, item: str) -> Any:
    fixed = item in (
        "RP-314",
        "RP-317",
        "RP-318",
        "RP-319",
        "RP-320",
        "RP-321",
        "RP-322",
        "RP-323",
        "RP-324",
        "RP-325",
        "RP-326",
        "RP-327",
        "RP-328",
        "RP-329",
        "RP-330",
        "RP-331",
        "RP-332",
        "RP-333",
        "RP-334",
        "RP-335",
        "RP-337",
        "RP-338",
        "RP-339",
        "RP-340",
        "RP-341",
        "RP-342",
        "RP-346",
        "RP-347",
        "RP-348",
        "RP-349",
        "RP-350",
        "RP-351",
        "RP-352",
        "RP-355",
        "RP-359",
        "RP-360",
        "RP-361",
        "RP-362",
        "RP-363",
        "RP-364",
        "RP-365",
        "RP-366",
        "RP-367",
        "RP-368",
        "RP-369",
        "RP-370",
        "RP-371",
        "RP-372",
        "RP-373",
        "RP-374",
        "RP-375",
        "RP-376",
        "RP-377",
        "RP-378",
        "RP-379",
        "RP-381",
        "RP-382",
        "RP-383",
        "RP-386",
        "RP-389",
        "RP-391",
        "RP-392",
        "RP-394",
        "RP-395",
        "RP-396",
        "RP-399",
        "RP-403",
        "RP-405",
        "RP-409",
    )
    mark = pytest.mark.backlog_fixed if fixed else pytest.mark.backlog
    return pytest.param(name, marks=mark(item), id=name)


def fresh_db(dsn: str) -> str:
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp314_costs")
        c.execute("create database rp314_costs")
    return dsn + " dbname=rp314_costs"


@pytest.mark.parametrize("name", [_marked(n, s[0]) for n, s in SCENARIOS.items()])
def test_plan(name: str, rust_pg: Any) -> None:
    _, setup, query, expected = SCENARIOS[name]
    assert run(fresh_db(_dsn(rust_pg)), setup, query) == expected


# Run in autocommit, as VACUUM cannot run in a transaction block.
_VACUUMED = [
    "create table k (a int primary key, b text)",
    "insert into k select g, 'v' || (g % 10) from generate_series(1, 1000) g",
    "create table n (a int, b int, c text)",
    "insert into n select g, g % 50, 'x' || (g % 7) from generate_series(1, 5000) g",
    "create index n_b on n (b)",
    "vacuum analyze k",
    "vacuum analyze n",
]

_ANALYZE = "explain (analyze, costs off, timing off, summary off) "


def run_autocommit(dsn: str, steps: list[str]) -> list[list[str]]:
    """Each statement's rows, in autocommit."""
    out = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for s in steps:
            cur = c.execute(s)
            out.append([str(r[0]) for r in cur] if cur.description else [])
    return out


@pytest.mark.backlog_fixed("RP-383")
def test_vacuum_sets_and_a_write_clears_the_visibility_map(rust_pg: Any) -> None:
    """VACUUM marked every page all-visible, so an index-only scan fetched
    no heap tuple; an insert cleared its page's bit, and a bitmap scan's
    pages grew with it -- PostgreSQL 15.19's answers on a fresh cluster."""
    steps = _VACUUMED + [
        _ANALYZE + "select a from k where a < 300",
        "insert into k values (1001, 'x')",
        _ANALYZE + "select a from k where a < 300",
        _ANALYZE + "select a from k where a > 990",
        "insert into n values (5001, 7, 'y')",
        _ANALYZE + "select * from n where b = 7",
        "update k set b = 'z' where a = 5",
        "vacuum k",
        _ANALYZE + "select a from k where a = 5",
    ]
    got = run_autocommit(fresh_db(_dsn(rust_pg)), steps)
    n = len(_VACUUMED)
    fetches = "Heap Fetches: {}"
    assert got[n] == [
        "Index Only Scan using k_pkey on k (actual rows=299 loops=1)",
        "  Index Cond: (a < 300)",
        "  " + fetches.format(0),
    ]
    assert got[n + 2][-1] == "  " + fetches.format(0)
    assert got[n + 3] == [
        "Index Only Scan using k_pkey on k (actual rows=11 loops=1)",
        "  Index Cond: (a > 990)",
        "  " + fetches.format(11),
    ]
    assert got[n + 5] == [
        "Bitmap Heap Scan on n (actual rows=101 loops=1)",
        "  Recheck Cond: (b = 7)",
        "  Heap Blocks: exact=28",
        "  ->  Bitmap Index Scan on n_b (actual rows=101 loops=1)",
        "        Index Cond: (b = 7)",
    ]
    assert got[n + 8] == [
        "Index Only Scan using k_pkey on k (actual rows=1 loops=1)",
        "  Index Cond: (a = 5)",
        "  " + fetches.format(0),
    ]


@pytest.mark.backlog_fixed("RP-385")
def test_an_index_only_scan_after_a_delete_and_vacuum(rust_pg: Any) -> None:
    """PostgreSQL 15.19 keeps the index-only scan of a table VACUUM saw
    shrink; this server printed a Seq Scan, as VACUUM marked a heap that
    lost rows as unmodelled."""
    steps = _VACUUMED + [
        "delete from k where a = 7",
        "vacuum k",
        _ANALYZE + "select a from k where a < 10",
    ]
    got = run_autocommit(fresh_db(_dsn(rust_pg)), steps)
    assert got[-1] == [
        "Index Only Scan using k_pkey on k (actual rows=8 loops=1)",
        "  Index Cond: (a < 10)",
        "  Heap Fetches: 0",
    ]


if __name__ == "__main__":
    for name, (_, setup, query, expected) in SCENARIOS.items():
        got = run(fresh_db(sys.argv[1]), setup, query)
        if got != expected:
            print(f"{name}: {got!r}")
