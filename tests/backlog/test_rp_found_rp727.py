"""What working RP-727 (an ordered row comparison with a subquery) found:

- a row comparison of zero-length rows answered instead of 0A000 (fixed:
  RP-849);
- a row field that is an untyped literal is compared as a string, not
  coerced to its pair's type;
- a row comparison with a pattern operator (`~<~`) is refused.

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


# `make_row_comparison_op` refuses a row of no fields, at the operator,
# for every operator. Here `=`, `<>` and IN answered until RP-849.
ZERO_LENGTH = {
    "select row() = row()": ("0A000", "cannot compare rows of zero length", "14"),
    "select row() <> row()": ("0A000", "cannot compare rows of zero length", "14"),
    "select row() in (select)": ("0A000", "cannot compare rows of zero length", "14"),
}


@pytest.mark.backlog_fixed("RP-849")
@pytest.mark.parametrize("query", list(ZERO_LENGTH))
def test_zero_length_row_comparison_is_0a000(query: str, dsn: str) -> None:
    assert answer(dsn, query) == ZERO_LENGTH[query]


# Each pair's operator is resolved as the statement is analysed, so an
# `unknown` literal field takes the other side's type. Here the literal is
# compared as a string: 0A000 `comparing string with int32 ...`.
UNTYPED_LITERAL = {
    "select (1, '2') = (1, 2)": ([(True,)], ["?column?"]),
    "select (1, '2') < any (select 1, 3)": ([(True,)], ["?column?"]),
    "select (1, 'a') < (1, 2)": (
        "22P02",
        'invalid input syntax for type integer: "a"',
        "12",
    ),
}


@pytest.mark.backlog("RP-850")
@pytest.mark.parametrize("query", list(UNTYPED_LITERAL))
def test_untyped_literal_in_a_row_takes_its_pairs_type(query: str, dsn: str) -> None:
    assert answer(dsn, query) == UNTYPED_LITERAL[query]


# `~<~` is a btree operator of `text_pattern_ops`, so a row comparison
# with it is a RowCompareExpr as one with `<` is. Here 42804 / 0A000.
PATTERN_OPERATOR = {
    "select ('a', 'b') ~<~ ('a', 'c')": ([(True,)], ["?column?"]),
    "select ('a', 'b') ~<~ any (select 'a', 'c')": ([(True,)], ["?column?"]),
}


@pytest.mark.backlog("RP-851")
@pytest.mark.parametrize("query", list(PATTERN_OPERATOR))
def test_row_comparison_with_a_pattern_operator(query: str, dsn: str) -> None:
    assert answer(dsn, query) == PATTERN_OPERATOR[query]
