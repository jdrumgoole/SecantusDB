"""Gap tests of divergences found while working RP-829 (the implicit block
of a multi-statement simple query):

- a transaction's isolation level, read-write mode and deferrable mode can
  be changed after it ran a query.

The expected answers were measured on PostgreSQL 15.19; none of the
statements creates an object.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn
from tests.backlog.test_rp_found_rp823 import _blocks


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


ISOLATION = "SET TRANSACTION ISOLATION LEVEL must be called before any query"
ALREADY = ("25001", "there is already a transaction in progress")

AFTER_A_QUERY = {
    # `check_XactIsoLevel`: a different level once `FirstSnapshotSet`.
    "begin; select 1; set transaction isolation level serializable; commit": (
        ("error", "25001", ISOLATION),
        [],
        "INERROR",
    ),
    "begin; select 1; set transaction_isolation = 'serializable'; commit": (
        ("error", "25001", ISOLATION),
        [],
        "INERROR",
    ),
    "begin; select 1; select set_config('transaction_isolation', 'serializable', true); commit": (
        ("error", "25001", ISOLATION),
        [],
        "INERROR",
    ),
    # The implicit block took the snapshot, and a BEGIN's modes are set
    # through the same GUC.
    "select 1; set transaction isolation level serializable; select 2": (
        ("error", "25001", ISOLATION),
        [],
        "IDLE",
    ),
    "select 1; begin isolation level serializable; commit": (
        ("error", "25001", ISOLATION),
        [],
        "IDLE",
    ),
    "begin; select 1; begin isolation level serializable; commit": (
        ("error", "25001", ISOLATION),
        [ALREADY],
        "INERROR",
    ),
    # `check_transaction_read_only`: read-write once a query ran.
    "begin read only; select 1; set transaction read write; commit": (
        ("error", "25001", "transaction read-write mode must be set before any query"),
        [],
        "INERROR",
    ),
    # `check_transaction_deferrable`: either way.
    "begin; select 1; set transaction deferrable; commit": (
        ("error", "25001", "SET TRANSACTION [NOT] DEFERRABLE must be called before any query"),
        [],
        "INERROR",
    ),
    "begin; select 1; set transaction not deferrable; commit": (
        ("error", "25001", "SET TRANSACTION [NOT] DEFERRABLE must be called before any query"),
        [],
        "INERROR",
    ),
}


@pytest.mark.backlog("RP-831")
@pytest.mark.parametrize("query", list(AFTER_A_QUERY))
def test_a_transaction_mode_is_set_before_any_query(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        assert _blocks(c, query) == AFTER_A_QUERY[query]
