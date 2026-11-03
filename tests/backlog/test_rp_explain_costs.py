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
    # ------------------------------ RP-349 an IN list's NULL element's cost
    "an IN list with a NULL": (
        "RP-349",
        _ANALYSED,
        "explain select * from e where a in (1, null, 3)",
        [
            "Seq Scan on e  (cost=0.00..18.75 rows=2 width=7)",
            "  Filter: (a = ANY ('{1,NULL,3}'::integer[]))",
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
    # ---------------------- RP-339 a user table's index path, chosen
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
        "RP-331",
        "RP-332",
        "RP-333",
        "RP-334",
        "RP-335",
        "RP-340",
        "RP-341",
        "RP-342",
        "RP-346",
        "RP-348",
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


if __name__ == "__main__":
    for name, (_, setup, query, expected) in SCENARIOS.items():
        got = run(fresh_db(sys.argv[1]), setup, query)
        if got != expected:
            print(f"{name}: {got!r}")
