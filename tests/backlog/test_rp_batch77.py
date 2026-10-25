"""Batch 77 of the Rust PG server (2026-10-25): found while modelling hash
indexes for SERIALIZABLE (RP-254), whose fill factor is an index storage
parameter. Fixed in the same batch: RP-261 (an index's `WITH (...)` was
ignored, and `ALTER INDEX ... SET` / `RESET` refused), RP-262 (`ROLLBACK
TO SAVEPOINT` did not undo an index's CREATE, DROP, RENAME or ALTER, and
lost the indexes of a table whose DROP it undid), RP-263 (a table's
storage parameter given bare or twice) and RP-264 (a missing relation in
`ALTER TABLE` and its kin was reported with a position). Filed open:
RP-265, a constraint's index's storage parameters.

Each scenario runs the same statements against a private, freshly
initdb'd PostgreSQL 15.19 cluster and this server; the literals are 15.19's
answers, measured by running this file as a script:

    PYTHONPATH=. python -m tests.backlog.test_rp_batch77 "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it
made. A statement starting `!` is answered with its DETAIL.
"""

from __future__ import annotations

import sys
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_RELOPTIONS = "select relname, reloptions from pg_class where relname like 'rp77%' order by 1"

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ----------------------------------- RP-261 index storage parameters
    "an index's storage parameters are checked by its access method": (
        "RP-261",
        False,
        [
            "create table rp77 (a int, p point, t text)",
            "!create index rp77_x on rp77 (a) with (fillfactor = 50, fillfactor = 60)",
            "!create index rp77_x on rp77 (a) with (toast.fillfactor = 50)",
            "!create index rp77_x on rp77 (a) with (deduplicate_items = maybe)",
            "!create index rp77_x on rp77 (a) with (foo = 1)",
            "!create index rp77_x on rp77 (a) with (fillfactor = 5)",
            "!create index rp77_x on rp77 using hash (a) with (deduplicate_items = off)",
            "!create index rp77_x on rp77 (a) with (fillfactor = 'abc')",
            "!create index rp77_x on rp77 using brin (a) with (pages_per_range = 0)",
            "!create index rp77_x on rp77 using gist (p) with (buffering = maybe)",
            "drop table rp77",
        ],
    ),
    "an index's storage parameters are kept and printed": (
        "RP-261",
        False,
        [
            "create table rp77 (a int, p point, t text)",
            "create index rp77_h on rp77 using hash (a) with (fillfactor = 50)",
            "create index rp77_b on rp77 (a) with (fillfactor = 70, deduplicate_items = off)",
            "create index rp77_d on rp77 (t) with (deduplicate_items)",
            "create index rp77_r on rp77 using brin (a) "
            "with (pages_per_range = 32, autosummarize = on)",
            "create index rp77_g on rp77 using gist (p) with (buffering = auto, fillfactor = 90)",
            _RELOPTIONS,
            "select indexdef from pg_indexes where indexname like 'rp77%' order by indexname",
            "select pg_get_indexdef('rp77_b'::regclass)",
            "drop table rp77",
        ],
    ),
    "ALTER INDEX sets and resets its storage parameters": (
        "RP-261",
        False,
        [
            "create table rp77 (a int, t text)",
            "create index rp77_d on rp77 (t) with (deduplicate_items)",
            "create index rp77_h on rp77 using hash (a)",
            "alter index rp77_d set (fillfactor = 10, deduplicate_items = true)",
            _RELOPTIONS,
            "alter index rp77_d reset (fillfactor, nonexistent)",
            _RELOPTIONS,
            "!alter index rp77_d set (fillfactor = 50, fillfactor = 60)",
            "!alter index rp77_h set (deduplicate_items = on)",
            "!alter index rp77_d set (bogus = 1)",
            "!alter index rp77 set (fillfactor = 50)",
            "alter index if exists rp77_nope set (fillfactor = 50)",
            "!alter index rp77_nope set (fillfactor = 50)",
            "begin",
            "alter index rp77_d set (fillfactor = 40)",
            "rollback",
            _RELOPTIONS,
            "drop table rp77",
        ],
    ),
    # ---------------------------------------- RP-262 savepoints and indexes
    "ROLLBACK TO SAVEPOINT undoes an index's DDL": (
        "RP-262",
        False,
        [
            "create table rp77 (a int unique, b int)",
            "create index rp77_i on rp77 (b) with (fillfactor = 70)",
            "insert into rp77 select g, g from generate_series(1, 5) g",
            "begin",
            "savepoint s",
            "drop table rp77",
            "rollback to s",
            "select indexname, indexdef from pg_indexes where tablename = 'rp77' order by 1",
            "commit",
            "begin",
            "savepoint s",
            "drop index rp77_i",
            "rollback to s",
            "create index rp77_j on rp77 (a, b)",
            "savepoint t",
            "alter index rp77_i rename to rp77_k",
            "alter index rp77_i set (fillfactor = 40)",
            "rollback to t",
            "select indexname from pg_indexes where tablename = 'rp77' order by 1",
            _RELOPTIONS,
            "savepoint u",
            "create index rp77_m on rp77 (b)",
            "release u",
            "savepoint v",
            "drop index rp77_m",
            "rollback to v",
            "select indexname from pg_indexes where tablename = 'rp77' order by 1",
            "rollback",
            "select indexname from pg_indexes where tablename = 'rp77' order by 1",
            "select count(*) from rp77 where b = 3",
            "drop table rp77",
        ],
    ),
    # --------------------------------------- RP-263 a table's parameters
    "a table's storage parameter given bare or twice": (
        "RP-263",
        False,
        [
            "!create table rp77 (a int) with (fillfactor)",
            "!create table rp77 (a int) with (fillfactor = 50, fillfactor = 60)",
            "!create table rp77 (a int) with (autovacuum_vacuum_scale_factor = 200)",
            "!create table rp77 (a int) with (vacuum_index_cleanup = maybe)",
            "create table rp77 (a int) with (user_catalog_table)",
            "!alter table rp77 set (fillfactor = 50, fillfactor = 60)",
            _RELOPTIONS,
            "drop table rp77",
        ],
    ),
    # ------------------------------- RP-265 a constraint's index's options
    "a constraint's index keeps its storage parameters": (
        "RP-265",
        True,
        [
            "create table rp77 (a int primary key with (fillfactor = 60), b int, "
            "constraint rp77_b_key unique (b) with (fillfactor = 70))",
            _RELOPTIONS,
            "select indexdef from pg_indexes where tablename = 'rp77' order by indexname",
            "alter index rp77_pkey set (fillfactor = 50)",
            _RELOPTIONS,
            "drop table rp77",
        ],
    ),
}


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


ALTERS = [
    "alter index rp77_nope rename to x",
    "alter index rp77_nope set (fillfactor = 50)",
    "alter table rp77_nope set (fillfactor = 50)",
    "alter table only rp77_nope add column x int",
    "alter view rp77_nope rename to x",
    "alter sequence rp77_nope restart",
    "alter materialized view rp77_nope rename to x",
]


def alter_positions(dsn: str) -> list[Any]:
    out = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for sql in ALTERS:
            try:
                c.execute(sql)
                out.append(("ok",))
            except psycopg.Error as e:
                out.append((e.sqlstate, e.diag.statement_position))
    return out


@pytest.mark.backlog_fixed("RP-264")
def test_alter_missing_relation_has_no_position(rust_pg: Any) -> None:
    """PostgreSQL 15.19 reports a missing relation in ALTER TABLE, INDEX,
    VIEW, SEQUENCE or MATERIALIZED VIEW without a position: the relation is
    looked up by name, outside any expression."""
    assert alter_positions(_dsn(rust_pg) + " dbname=postgres") == [("42P01", None)] * len(ALTERS)


EXPECTED: dict[str, Any] = {
    "an index's storage parameters are checked by its access method": [
        "CREATE TABLE",
        ("ERR", "22023", 'parameter "fillfactor" specified more than once', None, None),
        ("ERR", "22023", 'unrecognized parameter namespace "toast"', None, None),
        ("ERR", "22023", 'invalid value for boolean option "deduplicate_items": maybe', None, None),
        ("ERR", "22023", 'unrecognized parameter "foo"', None, None),
        (
            "ERR",
            "22023",
            'value 5 out of bounds for option "fillfactor"',
            None,
            'Valid values are between "10" and "100".',
        ),
        ("ERR", "22023", 'unrecognized parameter "deduplicate_items"', None, None),
        ("ERR", "22023", 'invalid value for integer option "fillfactor": abc', None, None),
        (
            "ERR",
            "22023",
            'value 0 out of bounds for option "pages_per_range"',
            None,
            'Valid values are between "1" and "131072".',
        ),
        (
            "ERR",
            "22023",
            'invalid value for enum option "buffering": maybe',
            None,
            'Valid values are "on", "off", and "auto".',
        ),
        "DROP TABLE",
    ],
    "an index's storage parameters are kept and printed": [
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        [
            ("rp77", "None"),
            ("rp77_b", "['fillfactor=70', 'deduplicate_items=off']"),
            ("rp77_d", "['deduplicate_items=true']"),
            ("rp77_g", "['buffering=auto', 'fillfactor=90']"),
            ("rp77_h", "['fillfactor=50']"),
            ("rp77_r", "['pages_per_range=32', 'autosummarize=on']"),
        ],
        [
            (
                "CREATE INDEX rp77_b ON public.rp77 "
                "USING btree (a) WITH (fillfactor='70', deduplicate_items=off)",
            ),
            ("CREATE INDEX rp77_d ON public.rp77 USING btree (t) WITH (deduplicate_items='true')",),
            (
                "CREATE INDEX rp77_g ON public.rp77 "
                "USING gist (p) WITH (buffering=auto, fillfactor='90')",
            ),
            ("CREATE INDEX rp77_h ON public.rp77 USING hash (a) WITH (fillfactor='50')",),
            (
                "CREATE INDEX rp77_r ON public.rp77 "
                "USING brin (a) WITH (pages_per_range='32', autosummarize='on')",
            ),
        ],
        [
            (
                "CREATE INDEX rp77_b ON public.rp77 "
                "USING btree (a) WITH (fillfactor='70', deduplicate_items=off)",
            )
        ],
        "DROP TABLE",
    ],
    "ALTER INDEX sets and resets its storage parameters": [
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE INDEX",
        "ALTER INDEX",
        [
            ("rp77", "None"),
            ("rp77_d", "['fillfactor=10', 'deduplicate_items=true']"),
            ("rp77_h", "None"),
        ],
        "ALTER INDEX",
        [("rp77", "None"), ("rp77_d", "['deduplicate_items=true']"), ("rp77_h", "None")],
        ("ERR", "22023", 'parameter "fillfactor" specified more than once', None, None),
        ("ERR", "22023", 'unrecognized parameter "deduplicate_items"', None, None),
        ("ERR", "22023", 'unrecognized parameter "bogus"', None, None),
        ("ERR", "42809", '"rp77" is not an index', None, None),
        "ALTER INDEX",
        ("ERR", "42P01", 'relation "rp77_nope" does not exist', None, None),
        "BEGIN",
        "ALTER INDEX",
        "ROLLBACK",
        [("rp77", "None"), ("rp77_d", "['deduplicate_items=true']"), ("rp77_h", "None")],
        "DROP TABLE",
    ],
    "ROLLBACK TO SAVEPOINT undoes an index's DDL": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "DROP TABLE",
        "ROLLBACK",
        [
            ("rp77_a_key", "CREATE UNIQUE INDEX rp77_a_key ON public.rp77 USING btree (a)"),
            ("rp77_i", "CREATE INDEX rp77_i ON public.rp77 USING btree (b) WITH (fillfactor='70')"),
        ],
        "COMMIT",
        "BEGIN",
        "SAVEPOINT",
        "DROP INDEX",
        "ROLLBACK",
        "CREATE INDEX",
        "SAVEPOINT",
        "ALTER INDEX",
        ("ERR", "42P01", 'relation "rp77_i" does not exist', None),
        "ROLLBACK",
        [("rp77_a_key",), ("rp77_i",), ("rp77_j",)],
        [
            ("rp77", "None"),
            ("rp77_a_key", "None"),
            ("rp77_i", "['fillfactor=70']"),
            ("rp77_j", "None"),
        ],
        "SAVEPOINT",
        "CREATE INDEX",
        "RELEASE",
        "SAVEPOINT",
        "DROP INDEX",
        "ROLLBACK",
        [("rp77_a_key",), ("rp77_i",), ("rp77_j",), ("rp77_m",)],
        "ROLLBACK",
        [("rp77_a_key",), ("rp77_i",)],
        [("1",)],
        "DROP TABLE",
    ],
    "a table's storage parameter given bare or twice": [
        ("ERR", "22023", 'invalid value for integer option "fillfactor": true', None, None),
        ("ERR", "22023", 'parameter "fillfactor" specified more than once', None, None),
        (
            "ERR",
            "22023",
            'value 200 out of bounds for option "autovacuum_vacuum_scale_factor"',
            None,
            'Valid values are between "0.000000" and "100.000000".',
        ),
        (
            "ERR",
            "22023",
            'invalid value for enum option "vacuum_index_cleanup": maybe',
            None,
            'Valid values are "on", "off", and "auto".',
        ),
        "CREATE TABLE",
        ("ERR", "22023", 'parameter "fillfactor" specified more than once', None, None),
        [("rp77", "['user_catalog_table=true']")],
        "DROP TABLE",
    ],
    "a constraint's index keeps its storage parameters": [
        "CREATE TABLE",
        [("rp77", "None"), ("rp77_b_key", "['fillfactor=70']"), ("rp77_pkey", "['fillfactor=60']")],
        [
            (
                "CREATE UNIQUE INDEX rp77_b_key ON public.rp77 "
                "USING btree (b) WITH (fillfactor='70')",
            ),
            (
                "CREATE UNIQUE INDEX rp77_pkey ON public.rp77 "
                "USING btree (a) WITH (fillfactor='60')",
            ),
        ],
        "ALTER INDEX",
        [("rp77", "None"), ("rp77_b_key", "['fillfactor=70']"), ("rp77_pkey", "['fillfactor=50']")],
        "DROP TABLE",
    ],
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_scenario(dsn, stmts) for name, (_, _, stmts) in SCENARIOS.items()})
    print(alter_positions(dsn))
