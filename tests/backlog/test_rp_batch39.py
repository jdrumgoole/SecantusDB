"""Batch 39 of the Rust PG server (2026-10-23): the regression tests of
what it fixed -- RP-178's citext functions (the extension's own functions,
and its regexp functions with flags or in FROM) and RP-182 (a citext
column of a VALUES list, a subquery or a CTE compared as text, found while
fixing RP-178). RP-180's test is in `test_rp_batch38.py`.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch39.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------------------------- RP-178 citext's functions
    "citext's own functions": (
        "RP-178",
        False,
        [
            "create extension citext",
            "select citext('X')::text, pg_typeof(citext('X'))::text",
            "select citext(true)::text, citext('ab '::char(4))::text,"
            " citext('10.0.0.1'::inet)::text",
            "select citext_cmp('a', 'B'), citext_eq('a', 'A'), citext_ne('a', 'A'),"
            " citext_lt('a', 'B'), citext_ge('a', 'B')",
            "select citext_hash('a') = citext_hash('A'),"
            " citext_hash_extended('a', 5) = citext_hash_extended('A', 5)",
            "select citext_larger('a', 'B')::text, citext_smaller('a', 'B')::text",
            "select citext_pattern_cmp('a', 'B'), citext_pattern_lt('a', 'B'),"
            " citext_pattern_ge('a', 'B')",
            "select texticlike('ABC'::citext, '%b%'), texticnlike('ABC'::citext, '%b%'),"
            " texticregexeq('ABC'::citext, 'b'), texticregexne('ABC'::citext, 'b')",
            "drop extension citext",
        ],
    ),
    "citext's regexp functions with flags and in FROM": (
        "RP-178",
        False,
        [
            "create extension citext",
            "select regexp_match('ABC'::citext, 'b')::text,"
            " regexp_match('ABC'::citext, 'b', 'c')::text",
            "select regexp_matches('ABAB'::citext, 'b', 'g')::text",
            "select regexp_replace('ABC'::citext, 'b', 'x'),"
            " regexp_replace('ABCB'::citext, 'b', 'x', 'g')",
            "select regexp_split_to_array('aXbxc'::citext, 'x', 'c')::text",
            "select * from regexp_split_to_table('aXbxc'::citext, 'x')",
            "select replace('aBcb'::citext, 'b', 'z'), split_part('aXbxc'::citext, 'x', 2),"
            " strpos('aBc'::citext, 'b'), translate('aBc'::citext, 'b', 'z')",
            "drop extension citext",
        ],
    ),
    # ------------------------------ RP-182 citext through a derived relation
    "a derived citext column compares as citext": (
        "RP-182",
        False,
        [
            "create extension citext",
            "select x = 'A', pg_typeof(x)::text from (values ('a'::citext)) v(x)",
            "select x = 'A' from (select 'a'::citext as x) s",
            "with c(x) as (values ('a'::citext)) select x = 'A' from c",
            "select min(x)::text, max(x)::text from (values ('a'::citext), ('B'), ('c')) v(x)",
            "select x::text from (values ('b'::citext), ('A'), ('c')) v(x) order by x",
            "drop extension citext",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a derived citext column compares as citext": [
        "CREATE EXTENSION",
        [("True", "citext")],
        [("True",)],
        [("True",)],
        [("a", "c")],
        [("A",), ("b",), ("c",)],
        "DROP EXTENSION",
    ],
    "citext's own functions": [
        "CREATE EXTENSION",
        [("X", "citext")],
        [("true", "ab", "10.0.0.1/32")],
        [("-1", "True", "False", "True", "False")],
        [("True", "True")],
        [("B", "a")],
        [("-1", "True", "False")],
        [("True", "False", "True", "False")],
        "DROP EXTENSION",
    ],
    "citext's regexp functions with flags and in FROM": [
        "CREATE EXTENSION",
        [("{B}", "None")],
        [("{B}",), ("{B}",)],
        [("AxC", "AxCx")],
        [("{aXb,c}",)],
        [("a",), ("b",), ("c",)],
        [("azcz", "b", "2", "azc")],
        "DROP EXTENSION",
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
