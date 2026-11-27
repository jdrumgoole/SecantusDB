"""Gap tests of the divergences found while working RP-1087 (a set
operation's unmatched column types were placed at the wrong column or arm;
fixed): RP-1176 (an array type is named by its internal name, `_int4`, in
a set operation's 42804 and 42846).

An answer is `(sqlstate, message, position)` for an error. The expected
answers were measured on PostgreSQL 15.19.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        return cur.statusmessage


# `format_type_be` names an array type by its element: `integer[]`.
NAMED = {
    "select 1 union select array[1]": (
        "42804",
        "UNION types integer and integer[] cannot be matched",
        "23",
    ),
    "select array[1] union select array['a'::text]": (
        "42846",
        "UNION could not convert type text[] to integer[]",
        "30",
    ),
}


@pytest.mark.backlog("RP-1176")
@pytest.mark.parametrize("query", list(NAMED))
def test_set_op_error_names_an_array_type_by_its_element(dsn: str, query: str) -> None:
    """The set operation's error names the array type as `integer[]`."""
    assert answer(dsn, query) == NAMED[query]
