"""What working RP-423 (a regex estimate over a back reference) found:

- a back reference was an invalid regex outside a WHERE: the scalar
  evaluator and the regex builtins compiled with a linear engine that
  refuses it (RP-1022, fixed: they fall back to a backtracking one);
- a back reference under `~*` still compares case-sensitively (RP-1022).

An answer is the rows and the column names, or `(sqlstate, message,
position)` for an error. Measured on PostgreSQL 15.19 (the shared reference
on port 5415): every statement is VALUES-only.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> str:
    return _dsn(rust_pg) + " dbname=postgres"


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        return (cur.fetchall(), [d.name for d in cur.description or []])


BACK_REFERENCES = {
    r"select 'abab' ~ '(ab)\1' as m": ([(True,)], ["m"]),
    r"select regexp_replace('abab', '(ab)\1', 'X') as r": ([("X",)], ["r"]),
    r"select regexp_match('xabab', '(ab)\1') as r": ([(["ab"],)], ["r"]),
    r"select 'abac' ~ '(ab)\1' as m": ([(False,)], ["m"]),
    r"select regexp_replace('ababxcdcd', '(\w\w)\1', '[\1]', 'g') as r": (
        [("[ab]x[cd]",)],
        ["r"],
    ),
    r"select regexp_matches('ababcdcd', '(\w\w)\1', 'g') as r": (
        [(["ab"],), (["cd"],)],
        ["r"],
    ),
    r"select regexp_count('ababab abab', '(ab)\1') as n": ([(2,)], ["n"]),
    r"select regexp_instr('xxabab', '(ab)\1') as n": ([(3,)], ["n"]),
    r"select regexp_substr('xxabab', '(ab)\1') as r": ([("abab",)], ["r"]),
    r"select regexp_replace('ababab', '(ab)\1', 'Y', 2) as r": ([("abY",)], ["r"]),
    r"select substring('xabab' from '(ab)\1') as r": ([("ab",)], ["r"]),
}


@pytest.mark.backlog_fixed("RP-1022")
@pytest.mark.parametrize("query", list(BACK_REFERENCES))
def test_a_back_reference_outside_a_where(query: str, dsn: str) -> None:
    assert answer(dsn, query) == BACK_REFERENCES[query]


@pytest.mark.backlog("RP-1022")
def test_a_case_insensitive_back_reference(dsn: str) -> None:
    """Under `~*` the back reference matches case-insensitively too."""
    assert answer(dsn, r"select 'ABab' ~* '(ab)\1' as m") == ([(True,)], ["m"])
