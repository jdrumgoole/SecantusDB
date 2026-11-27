"""Gap tests of the divergence found while working RP-1086 (a `record`
`INSERT ... SELECT` column written to a non-composite column was 0A000;
fixed): RP-1175 (`CREATE CAST` naming a pseudo-type is 42704 `type
"record" does not exist`, not 42809).

An answer is `(sqlstate, message)` for an error, or the command tag. The
expected answers were measured on PostgreSQL 15.19; `CreateCast` refuses a
pseudo-type source or target before it looks for the function, so none of
these needs one.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


def answer(rust_pg: object, query: str) -> Any:
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary)
        return cur.statusmessage


PSEUDO = {
    "create cast (record as int) with inout": (
        "42809",
        "source data type record is a pseudo-type",
    ),
    "create cast (int as record) with inout": (
        "42809",
        "target data type record is a pseudo-type",
    ),
    "create cast (anyelement as int) with inout": (
        "42809",
        "source data type anyelement is a pseudo-type",
    ),
    "create cast (int as void) with inout": (
        "42809",
        "target data type void is a pseudo-type",
    ),
}


@pytest.mark.backlog("RP-1175")
@pytest.mark.parametrize("query", list(PSEUDO))
def test_create_cast_pseudo_type(query: str, rust_pg: object) -> None:
    """A pseudo-type is a type `CREATE CAST` names and refuses; here it is
    not found at all."""
    assert answer(rust_pg, query) == PSEUDO[query]
