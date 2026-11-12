"""Gap test of the DEFAULT divergence found while working RP-886: RP-892, a folded
DEFAULT skips its column's length check.

An answer is the last statement's rows, `("rowcount", n)` for a statement
with none, or `(sqlstate, message, hint, position)` for an error, the
statements run in order on one fresh session. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster.
"""

from __future__ import annotations

from collections.abc import Iterator

import pytest

from tests.backlog.test_rp_batch26 import _dsn
from tests.backlog.test_rp_qualified_calls import answer


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


TOO_LONG = {
    "varchar(2) default 'abc'": (
        "22001",
        "value too long for type character varying(2)",
        None,
        None,
    ),
    "char(2) default 'xyz'": ("22001", "value too long for type character(2)", None, None),
}


@pytest.mark.backlog("RP-892")
@pytest.mark.parametrize("column", list(TOO_LONG))
def test_a_folded_default_is_coerced_to_its_column(column: str, dsn: str) -> None:
    """A DEFAULT is coerced to its column's type and modifier when an
    INSERT runs it."""
    try:
        assert (
            answer(
                dsn,
                f"create table t886 (a {column}, i int)",
                "insert into t886 (i) values (1)",
            )
            == TOO_LONG[column]
        )
    finally:
        answer(dsn, "drop table if exists t886")
