"""Batch 78 of the Rust PG server (2026-10-25): found while working RP-255
(an INSERT inside a function that a constant SELECT calls runs while the
statement is planned). Fixed in the same batch: RP-266 (an EXPLAIN without
ANALYZE, or a PREPARE, ran a volatile function's side effects) and RP-267
(EXPLAIN's TIMING and SUMMARY were ignored, and its JSON lacked the actual
times and the Triggers group). The EXPLAIN scenarios use only options whose
output prints no measured time.

Each scenario runs the same statements against a private PostgreSQL 15.19
cluster and this server; the literals are 15.19's answers, measured by
running this file as a script:

    PYTHONPATH=. python -m tests.backlog.test_rp_batch78 "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario rolls back what it
made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_COUNT = "select count(*) from rp78"

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # --------------------------------- RP-266 planning runs no side effect
    "EXPLAIN and PREPARE run no function": (
        "RP-266",
        False,
        [
            "begin",
            "create table rp78 (k int)",
            "create sequence rp78_s",
            "create function rp78_f(n int) returns void language sql as "
            "$$ insert into rp78 values (n) $$",
            "explain (costs off) select rp78_f(1)",
            _COUNT,
            "explain (costs off) select nextval('rp78_s')",
            "select last_value, is_called from rp78_s",
            "prepare rp78_p as select rp78_f(2)",
            _COUNT,
            "execute rp78_p",
            _COUNT,
            "explain (analyze, costs off, timing off, summary off) select rp78_f(3)",
            _COUNT,
            "execute rp78_p",
            _COUNT,
            "deallocate rp78_p",
            "rollback",
        ],
    ),
    # ----------------------------------------- RP-267 TIMING and SUMMARY
    "EXPLAIN's TIMING and SUMMARY": (
        "RP-267",
        False,
        [
            "!explain (timing) select 1",
            "explain (costs off, timing off) select 1",
            "explain (analyze, costs off, timing off, summary off) select 1",
            "explain (analyze, costs off, timing off, summary off, format json) select 1",
            "explain (analyze, costs off, timing off, summary off, format yaml) select 1",
            "explain (analyze, costs off, timing off, summary off, format xml) select 1",
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


EXPECTED: dict[str, Any] = {
    "EXPLAIN and PREPARE run no function": [
        "BEGIN",
        "CREATE TABLE",
        "CREATE SEQUENCE",
        "CREATE FUNCTION",
        [("Result",)],
        [("0",)],
        [("Result",)],
        [("1", "False")],
        "PREPARE",
        [("0",)],
        [("None",)],
        [("1",)],
        [("Result (actual rows=1 loops=1)",)],
        [("2",)],
        [("None",)],
        [("3",)],
        "DEALLOCATE",
        "ROLLBACK",
    ],
    "EXPLAIN's TIMING and SUMMARY": [
        ("ERR", "22023", "EXPLAIN option TIMING requires ANALYZE", None, None),
        [("Result",)],
        [("Result (actual rows=1 loops=1)",)],
        [
            (
                (
                    "[{'Plan': {'Node Type': 'Result', 'Parallel Aware': False, '"
                    "Async Capable': False, 'Actual Rows': 1, 'Actual Loops': 1},"
                    " 'Triggers': []}]"
                ),
            )
        ],
        [
            (
                (
                    "- Plan: \n"
                    '    Node Type: "Result"\n'
                    "    Parallel Aware: false\n"
                    "    Async Capable: false\n"
                    "    Actual Rows: 1\n"
                    "    Actual Loops: 1\n"
                    "  Triggers: "
                ),
            )
        ],
        [
            (
                (
                    '<explain xmlns="http://www.postgresql.org/2009/explain">\n'
                    "  <Query>\n"
                    "    <Plan>\n"
                    "      <Node-Type>Result</Node-Type>\n"
                    "      <Parallel-Aware>false</Parallel-Aware>\n"
                    "      <Async-Capable>false</Async-Capable>\n"
                    "      <Actual-Rows>1</Actual-Rows>\n"
                    "      <Actual-Loops>1</Actual-Loops>\n"
                    "    </Plan>\n"
                    "    <Triggers>\n"
                    "    </Triggers>\n"
                    "  </Query>\n"
                    "</explain>"
                ),
            )
        ],
    ],
}

if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_scenario(dsn, stmts) for name, (_, _, stmts) in SCENARIOS.items()})
