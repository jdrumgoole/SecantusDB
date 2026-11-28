"""A jsonpath literal's input errors (found working RP-1123, 2026-11-28).

PostgreSQL reports where the literal starts and puts the reason for an
unrecognized `like_regex` flag in DETAIL; this server reports no position
and folds the reason into the message. Measured on PostgreSQL 15.20; every
statement is a bare cast.
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
            c.execute(query)
        except psycopg.Error as e:
            return (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_detail,
                e.diag.statement_position,
            )
        return None


INPUT_ERRORS = {
    """select '$ ? (@ like_regex "a" flag "z")'::jsonpath""": (
        "42601",
        "invalid input syntax for type jsonpath",
        'Unrecognized flag character "z" in LIKE_REGEX predicate.',
        "8",
    ),
    "select '$ ?? x'::jsonpath": (
        "42601",
        'syntax error at or near "?" of jsonpath input',
        None,
        "8",
    ),
    """select '$ ? (@ like_regex "(")'::jsonpath""": (
        "2201B",
        "invalid regular expression: parentheses () not balanced",
        None,
        "8",
    ),
}


@pytest.mark.backlog("RP-1236")
@pytest.mark.parametrize("query", list(INPUT_ERRORS))
def test_jsonpath_input_error(query: str, dsn: str) -> None:
    """No position, and the flag's reason in the message."""
    assert answer(dsn, query) == INPUT_ERRORS[query]
