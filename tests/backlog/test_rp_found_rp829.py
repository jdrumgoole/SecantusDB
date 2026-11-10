"""Tests of divergences found while working RP-829 (the implicit block
of a multi-statement simple query):

- a transaction's isolation level, read-write mode and deferrable mode
  could be changed after it ran a query (RP-831, fixed: the server keeps
  PostgreSQL's `FirstSnapshotSet` and asks variable.c's three check hooks).

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


@pytest.mark.backlog_fixed("RP-831")
@pytest.mark.parametrize("query", list(AFTER_A_QUERY))
def test_a_transaction_mode_is_set_before_any_query(query: str, dsn: str) -> None:
    """A mode change after the transaction took its snapshot was 25001."""
    with psycopg.connect(dsn, autocommit=True) as c:
        assert _blocks(c, query) == AFTER_A_QUERY[query]


SUBTRANSACTION = "SET TRANSACTION ISOLATION LEVEL must not be called in a subtransaction"

# The rest of the three hooks, measured on 15.19 with the fix.
CHECK_HOOKS = {
    # A SELECT takes its snapshot before its select list runs: even as the
    # block's first statement, or the only one outside a block.
    "begin; select set_config('transaction_isolation', 'serializable', true); commit": (
        ("error", "25001", ISOLATION),
        [],
        "INERROR",
    ),
    "select set_config('transaction_isolation', 'serializable', false)": (
        ("error", "25001", ISOLATION),
        [],
        "IDLE",
    ),
    "select 1; set transaction_deferrable = on": (
        ("error", "25001", "SET TRANSACTION [NOT] DEFERRABLE must be called before any query"),
        [],
        "IDLE",
    ),
    # Only a DIFFERENT level is refused, and only a move to read-write.
    "begin; set transaction isolation level serializable; select 1; "
    "set transaction isolation level serializable; show transaction_isolation; commit": (
        [
            ("BEGIN", None),
            ("SET", None),
            ("SELECT 1", [(1,)]),
            ("SET", None),
            ("SHOW", [("serializable",)]),
            ("COMMIT", None),
        ],
        [],
        "IDLE",
    ),
    "begin; select 1; set transaction read only; show transaction_read_only; commit": (
        [
            ("BEGIN", None),
            ("SELECT 1", [(1,)]),
            ("SET", None),
            ("SHOW", [("on",)]),
            ("COMMIT", None),
        ],
        [],
        "IDLE",
    ),
    "begin; select 1; set transaction read write; commit": (
        [("BEGIN", None), ("SELECT 1", [(1,)]), ("SET", None), ("COMMIT", None)],
        [],
        "IDLE",
    ),
    # A reset value is installed without the hook.
    "begin isolation level serializable; select 1; set transaction_isolation to default; "
    "show transaction_isolation; commit": (
        [
            ("BEGIN", None),
            ("SELECT 1", [(1,)]),
            ("SET", None),
            ("SHOW", [("read committed",)]),
            ("COMMIT", None),
        ],
        [],
        "IDLE",
    ),
    "begin isolation level serializable; select 1; reset transaction_isolation; "
    "show transaction_isolation; commit": (
        [
            ("BEGIN", None),
            ("SELECT 1", [(1,)]),
            ("RESET", None),
            ("SHOW", [("read committed",)]),
            ("COMMIT", None),
        ],
        [],
        "IDLE",
    ),
    # SHOW and LOCK take no snapshot.
    "begin; show transaction_isolation; set transaction isolation level serializable; "
    "show transaction_isolation; commit": (
        [
            ("BEGIN", None),
            ("SHOW", [("read committed",)]),
            ("SET", None),
            ("SHOW", [("serializable",)]),
            ("COMMIT", None),
        ],
        [],
        "IDLE",
    ),
    "begin; lock table pg_class in access share mode; "
    "set transaction isolation level serializable; commit": (
        [("BEGIN", None), ("LOCK TABLE", None), ("SET", None), ("COMMIT", None)],
        [],
        "IDLE",
    ),
    # A rolled-back savepoint does not take the snapshot back.
    "begin; savepoint s; select 1; rollback to s; release s; "
    "set transaction isolation level serializable; commit": (
        ("error", "25001", ISOLATION),
        [],
        "INERROR",
    ),
    "begin; set local transaction_isolation = 'serializable'; select 1; "
    "set local transaction_isolation = 'repeatable read'; commit": (
        ("error", "25001", ISOLATION),
        [],
        "INERROR",
    ),
    # The subtransaction branches.
    "begin; savepoint s; set transaction isolation level serializable; commit": (
        ("error", "25001", SUBTRANSACTION),
        [],
        "INERROR",
    ),
    "begin; savepoint s; set transaction isolation level read committed; commit": (
        [("BEGIN", None), ("SAVEPOINT", None), ("SET", None), ("COMMIT", None)],
        [],
        "IDLE",
    ),
    "begin read only; savepoint s; set transaction read write; commit": (
        ("error", "25001", "cannot set transaction read-write mode inside a read-only transaction"),
        [],
        "INERROR",
    ),
    "begin; savepoint s; set transaction not deferrable; commit": (
        (
            "error",
            "25001",
            "SET TRANSACTION [NOT] DEFERRABLE cannot be called within a subtransaction",
        ),
        [],
        "INERROR",
    ),
    # A DO block's statements run in the transaction the DO took its
    # snapshot in -- until its COMMIT starts a new one (plpgsql_transaction).
    "do $$ begin perform 1; end $$; set transaction isolation level serializable": (
        ("error", "25001", ISOLATION),
        [],
        "IDLE",
    ),
    "do language plpgsql $$ begin set transaction isolation level repeatable read; end $$": (
        ("error", "25001", ISOLATION),
        [],
        "IDLE",
    ),
    "do language plpgsql $$ begin perform 1; raise info '%', "
    "current_setting('transaction_isolation'); commit; "
    "set transaction isolation level repeatable read; perform 1; raise info '%', "
    "current_setting('transaction_isolation'); commit; "
    "set transaction isolation level repeatable read; reset transaction isolation level; "
    "perform 1; raise info '%', current_setting('transaction_isolation'); commit; end $$": (
        [("DO", None)],
        [("00000", "read committed"), ("00000", "repeatable read"), ("00000", "read committed")],
        "IDLE",
    ),
}


@pytest.mark.backlog_fixed("RP-831")
@pytest.mark.parametrize("query", list(CHECK_HOOKS))
def test_the_transaction_mode_check_hooks(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        assert _blocks(c, query) == CHECK_HOOKS[query]


@pytest.mark.backlog_fixed("RP-831")
def test_an_extended_protocol_block_fixes_its_mode_at_its_first_query(dsn: str) -> None:
    """psycopg's own block: the Execute of a SELECT took the snapshot."""
    with psycopg.connect(dsn) as c:
        assert c.execute("select 1").fetchall() == [(1,)]
        with pytest.raises(psycopg.errors.ActiveSqlTransaction) as e:
            c.execute("set transaction isolation level serializable")
        assert e.value.diag.message_primary == ISOLATION
        c.rollback()
        c.execute("set transaction isolation level serializable")
        assert c.execute("show transaction_isolation").fetchall() == [("serializable",)]
        c.rollback()
