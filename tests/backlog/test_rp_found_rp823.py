"""Gap tests of divergences found while working RP-823 (a cursor FOR loop's
portal was not pinned):

- a FOR loop over a query or an EXECUTE runs no portal: `pg_cursors` lists
  none while it runs, and a CLOSE ALL in its body is not refused;
- a BEGIN inside a multi-statement simple query warned 25001 (RP-829, fixed:
  the implicit block, `TBLOCK_IMPLICIT_INPROGRESS`, is now modelled for
  every block statement).

The expected answers were measured on PostgreSQL 15.19; none of the
statements creates an object (the PREPARE TRANSACTION case was measured on
a private cluster, and its transaction is rolled back).
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _answer(dsn: str, query: str) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        notices: list[str] = []
        c.add_notice_handler(lambda d: notices.append(d.message_primary or ""))
        try:
            c.execute(query)
        except psycopg.Error as e:
            return ("error", e.sqlstate, e.diag.message_primary)
        return ("notice", notices)


QUERY_LOOP_PORTAL = {
    # `exec_stmt_fors` and `exec_stmt_dynfors` open the query as an unnamed
    # portal and walk it through `exec_for_query`, which pins it.
    "do $$ declare x record; h text; begin for x in select 1 loop "
    "select string_agg(name, ',') into h from pg_cursors; end loop; "
    "raise notice '[%]', h; end $$": ("notice", ["[<unnamed portal 1>]"]),
    "do $$ declare x record; h text; begin for x in execute 'select 1' loop "
    "select string_agg(name, ',') into h from pg_cursors; end loop; "
    "raise notice '[%]', h; end $$": ("notice", ["[<unnamed portal 1>]"]),
    "do $$ declare x record; begin for x in select 1 loop execute 'close all'; end loop; end $$": (
        "error",
        "24000",
        'cannot drop pinned portal "<unnamed portal 1>"',
    ),
    "do $$ declare x record; begin for x in execute 'select 1' loop "
    "execute 'close all'; end loop; end $$": (
        "error",
        "24000",
        'cannot drop pinned portal "<unnamed portal 1>"',
    ),
}


@pytest.mark.backlog("RP-828")
@pytest.mark.parametrize("query", list(QUERY_LOOP_PORTAL))
def test_a_query_loop_walks_a_pinned_portal(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == QUERY_LOOP_PORTAL[query]


@pytest.mark.backlog_fixed("RP-829")
@pytest.mark.parametrize("query", ["begin; select 1; commit", "select 1; begin; select 2; commit"])
def test_a_begin_in_a_multi_statement_query_does_not_warn(query: str, dsn: str) -> None:
    # `BeginTransactionBlock` turns an implicit block (TBLOCK_IMPLICIT_
    # INPROGRESS) into a real one with no WARNING; only a BEGIN inside an
    # explicit block warns 25001.
    assert _answer(dsn, query) == ("notice", [])


ALREADY = ("25001", "there is already a transaction in progress")
NO_TXN = ("25P01", "there is no transaction in progress")


def _blocks(c: psycopg.Connection, query: str) -> tuple[object, ...]:
    """Every result of one simple Query (tag and rows), or its error, with
    the notices it raised and the transaction status after it."""
    notices: list[tuple[str | None, str | None]] = []
    c.add_notice_handler(lambda d: notices.append((d.sqlstate, d.message_primary)))
    try:
        cur = c.execute(query)
        results = []
        while True:
            results.append((cur.statusmessage, cur.fetchall() if cur.description else None))
            if not cur.nextset():
                break
        outcome: object = results
    except psycopg.Error as e:
        outcome = ("error", e.sqlstate, e.diag.message_primary)
    return (outcome, notices, c.info.transaction_status.name)


IMPLICIT_BLOCK = {
    # A BEGIN in an explicit block still warns, implicit block or not.
    "begin; begin; select 1; commit": (
        [("BEGIN", None), ("BEGIN", None), ("SELECT 1", [(1,)]), ("COMMIT", None)],
        [ALREADY],
        "IDLE",
    ),
    # `EndTransactionBlock` / `UserAbortTransactionBlock`: the implicit
    # block ends with a WARNING, and the query goes on in a new one.
    "select 1; commit": ([("SELECT 1", [(1,)]), ("COMMIT", None)], [NO_TXN], "IDLE"),
    "select 1; rollback": ([("SELECT 1", [(1,)]), ("ROLLBACK", None)], [NO_TXN], "IDLE"),
    "commit; select 1": ([("COMMIT", None), ("SELECT 1", [(1,)])], [NO_TXN], "IDLE"),
    "rollback; select 1": ([("ROLLBACK", None), ("SELECT 1", [(1,)])], [NO_TXN], "IDLE"),
    "select 1; commit; select 1/0": (("error", "22012", "division by zero"), [NO_TXN], "IDLE"),
    # ... but AND CHAIN, and the savepoint statements, are errors there.
    "select 1; commit and chain": (
        ("error", "25P01", "COMMIT AND CHAIN can only be used in transaction blocks"),
        [],
        "IDLE",
    ),
    "select 1; rollback and chain": (
        ("error", "25P01", "ROLLBACK AND CHAIN can only be used in transaction blocks"),
        [],
        "IDLE",
    ),
    "select 1; savepoint s": (
        ("error", "25P01", "SAVEPOINT can only be used in transaction blocks"),
        [],
        "IDLE",
    ),
    "select 1; release savepoint s": (
        ("error", "25P01", "RELEASE SAVEPOINT can only be used in transaction blocks"),
        [],
        "IDLE",
    ),
    "select 1; rollback to savepoint s": (
        ("error", "25P01", "ROLLBACK TO SAVEPOINT can only be used in transaction blocks"),
        [],
        "IDLE",
    ),
    # After a BEGIN it is a block like any other.
    "select 1; begin; savepoint s; release s; commit": (
        [
            ("SELECT 1", [(1,)]),
            ("BEGIN", None),
            ("SAVEPOINT", None),
            ("RELEASE", None),
            ("COMMIT", None),
        ],
        [],
        "IDLE",
    ),
    "select 1; begin; select 2": (
        [("SELECT 1", [(1,)]), ("BEGIN", None), ("SELECT 1", [(2,)])],
        [],
        "INTRANS",
    ),
    "select 1; start transaction; select 2": (
        [("SELECT 1", [(1,)]), ("START TRANSACTION", None), ("SELECT 1", [(2,)])],
        [],
        "INTRANS",
    ),
    "select 1; begin; commit and chain; show transaction_isolation": (
        [
            ("SELECT 1", [(1,)]),
            ("BEGIN", None),
            ("COMMIT", None),
            ("SHOW", [("read committed",)]),
        ],
        [],
        "INTRANS",
    ),
    # A BEGIN that continues a transaction keeps its modes, and sets only
    # the ones it names.
    "set transaction read only; begin; show transaction_read_only; commit": (
        [("SET", None), ("BEGIN", None), ("SHOW", [("on",)]), ("COMMIT", None)],
        [],
        "IDLE",
    ),
    "set transaction isolation level repeatable read; begin read only; "
    "show transaction_isolation; show transaction_read_only; commit": (
        [
            ("SET", None),
            ("BEGIN", None),
            ("SHOW", [("repeatable read",)]),
            ("SHOW", [("on",)]),
            ("COMMIT", None),
        ],
        [],
        "IDLE",
    ),
    "set transaction isolation level repeatable read; start transaction; "
    "show transaction_isolation; commit": (
        [
            ("SET", None),
            ("START TRANSACTION", None),
            ("SHOW", [("repeatable read",)]),
            ("COMMIT", None),
        ],
        [],
        "IDLE",
    ),
    "begin isolation level serializable; begin; show transaction_isolation; commit": (
        [("BEGIN", None), ("BEGIN", None), ("SHOW", [("serializable",)]), ("COMMIT", None)],
        [ALREADY],
        "IDLE",
    ),
}


@pytest.mark.backlog_fixed("RP-829")
@pytest.mark.parametrize("query", list(IMPLICIT_BLOCK))
def test_the_implicit_blocks_transaction_statements(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        assert _blocks(c, query) == IMPLICIT_BLOCK[query]


@pytest.mark.backlog_fixed("RP-829")
def test_a_prepare_transaction_in_the_implicit_block_prepares_it(dsn: str) -> None:
    # `PrepareTransactionBlock` over `EndTransactionBlock`: the WARNING,
    # then the implicit block's transaction is prepared.
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            assert _blocks(c, "select 1; prepare transaction 'rp829'") == (
                [("SELECT 1", [(1,)]), ("PREPARE TRANSACTION", None)],
                [NO_TXN],
                "IDLE",
            )
            assert c.execute(
                "select gid from pg_prepared_xacts where gid = 'rp829'"
            ).fetchall() == [("rp829",)]
        finally:
            if c.execute("select gid from pg_prepared_xacts where gid = 'rp829'").fetchall():
                c.execute("rollback prepared 'rp829'")
