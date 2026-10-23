"""Batch 42 of the Rust PG server (2026-10-23): the regression tests of
RP-165 (a built-in text-search configuration is a row a superuser may
change or drop; its first test is in `test_rp_batch34.py`) and RP-190 (a
call of no function among an aggregate's targets was 0A000, found while
fixing RP-165).

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch42.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. A built-in configuration is
changed only inside a transaction that is rolled back, so the cluster keeps
its own.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------------- RP-165 a built-in configuration changed
    "a built-in configuration's map and comment": (
        "RP-165",
        False,
        [
            "begin",
            "alter text search configuration danish drop mapping for asciiword",
            "select to_tsvector('danish', 'hestene')::text",
            "select count(*) from pg_ts_config_map where mapcfg = 13632",
            "comment on text search configuration danish is 'd'",
            "select obj_description(13632, 'pg_ts_config')",
            "select cfgname, cfgowner::regrole::text from pg_ts_config where oid = 13632",
            "rollback",
            "select to_tsvector('danish', 'hestene')::text",
            "select count(*) from pg_ts_config_map where mapcfg = 13632",
        ],
    ),
    "a built-in configuration dropped": (
        "RP-165",
        False,
        [
            "begin",
            "drop text search configuration danish",
            "select count(*) from pg_ts_config where cfgname = 'danish'",
            "select count(*) from pg_ts_config_map where mapcfg = 13632",
            "select to_tsvector('danish', 'x')",
            "rollback",
            "select 'danish'::regconfig::oid",
        ],
    ),
    "a built-in configuration is the bootstrap superuser's": (
        "RP-165",
        False,
        [
            "create role rp42_nosu",
            "set role rp42_nosu",
            "alter text search configuration danish rename to rp42_x",
            "reset role",
            "drop role rp42_nosu",
        ],
    ),
    # ------------------------------ RP-190 an aggregate beside no function
    "a call of no function beside an aggregate": (
        "RP-190",
        False,
        [
            "select rp42_nosuchfn(1), count(*) from pg_class",
            "select rp42_nosuchfn(1, 'a'), count(*) from (values (1)) v(x)",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a built-in configuration dropped": [
        "BEGIN",
        "DROP TEXT SEARCH CONFIGURATION",
        [("0",)],
        [("0",)],
        ("ERR", "42704", 'text search configuration "danish" does not exist', None),
        "ROLLBACK",
        [("13632",)],
    ],
    "a built-in configuration is the bootstrap superuser's": [
        "CREATE ROLE",
        "SET",
        ("ERR", "42501", "must be owner of text search configuration danish", None),
        "RESET",
        "DROP ROLE",
    ],
    "a built-in configuration's map and comment": [
        "BEGIN",
        "ALTER TEXT SEARCH CONFIGURATION",
        [("",)],
        [("18",)],
        "COMMENT",
        [("d",)],
        [("danish", "postgres")],
        "ROLLBACK",
        [("'hest':1",)],
        [("19",)],
    ],
    "a call of no function beside an aggregate": [
        (
            "ERR",
            "42883",
            "function rp42_nosuchfn(integer) does not exist",
            "No function matches the given name and argument "
            "types. You might need to add explicit type "
            "casts.",
        ),
        (
            "ERR",
            "42883",
            "function rp42_nosuchfn(integer, unknown) does not exist",
            "No function matches the given name and argument "
            "types. You might need to add explicit type "
            "casts.",
        ),
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
