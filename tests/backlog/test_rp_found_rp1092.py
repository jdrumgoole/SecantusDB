"""Gap tests of the divergences found while working RP-1092
(`client_min_messages` withholding a NOTICE):

- at `client_min_messages = debug5` a statement's transaction sends no
  `StartTransaction` / `CommitTransaction` DEBUG trace.

The expected answers were measured on PostgreSQL 15.19 (the shared
reference on port 5415).
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


@pytest.mark.backlog("RP-1230")
def test_debug5_traces_a_statements_transaction(dsn: str) -> None:
    """At `client_min_messages = debug5`, PostgreSQL 15.19 sends
    `ShowTransactionState`'s two DEBUG messages around a `select 1` (its
    DEBUG5 `StartTransaction` / `CommitTransaction` lines, with the
    transaction's xid, subtransaction id and command id); here none.
    Found while working RP-1092."""
    seen: list[tuple[str, str]] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("set client_min_messages = debug5")
        c.add_notice_handler(lambda d: seen.append((d.severity or "", d.message_primary or "")))
        c.execute("select 1")
    assert seen == [
        (
            "DEBUG",
            "StartTransaction(1) name: unnamed; blockState: DEFAULT; state: INPROGRESS, "
            "xid/subid/cid: 0/1/0",
        ),
        (
            "DEBUG",
            "CommitTransaction(1) name: unnamed; blockState: STARTED; state: INPROGRESS, "
            "xid/subid/cid: 0/1/0",
        ),
    ]
