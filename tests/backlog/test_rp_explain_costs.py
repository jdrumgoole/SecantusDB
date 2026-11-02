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
    "limit": (
        "RP-319",
        _ANALYSED,
        "explain select * from e limit 5",
        [
            "Limit  (cost=0.00..0.07 rows=5 width=7)",
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
    "windowagg": (
        "RP-323",
        _ANALYSED,
        "explain select a, row_number() over () from e",
        [
            "WindowAgg  (cost=0.00..27.50 rows=1000 width=12)",
            "  ->  Seq Scan on e  (cost=0.00..15.00 rows=1000 width=4)",
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
    "result": (
        "RP-325",
        [],
        "explain select 1",
        ["Result  (cost=0.00..0.01 rows=1 width=4)"],
    ),
    "values scan": (
        "RP-326",
        [],
        "explain values (1), (2)",
        ['Values Scan on "*VALUES*"  (cost=0.00..0.03 rows=2 width=4)'],
    ),
    "function scan": (
        "RP-327",
        [],
        "explain select * from generate_series(1, 10)",
        ["Function Scan on generate_series  (cost=0.00..0.10 rows=10 width=4)"],
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
    # ------------------------------------------------ found while working
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
    mark = pytest.mark.backlog_fixed if item == "RP-314" else pytest.mark.backlog
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
