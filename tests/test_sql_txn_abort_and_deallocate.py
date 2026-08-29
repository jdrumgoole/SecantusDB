"""An error inside a transaction aborts it, and DEALLOCATE ALL says so.

Two halves of one driver behaviour, which only work together:

* Postgres aborts a transaction block on ANY error — every later statement
  fails with 25P02 until ROLLBACK. The engine did that for errors raised while
  running a statement, but an error raised in the extended protocol itself (a
  missing prepared statement or portal, an undecodable Bind parameter) never
  reached that path, so the block carried on as if nothing had happened.
* Postgres reports the command tag ``DEALLOCATE ALL`` for the ALL form, and
  drivers key off that exact string: pgjdbc's QueryExecutor watches for it to
  learn its server-side statement cache is gone and re-Parse. We reported a
  bare ``DEALLOCATE``, so it kept executing names the server had dropped.

Fixing only the first made pgjdbc's AutoRollbackTest *worse* (24 failures ->
32): the transaction now died where the driver expected to recover, because it
still did not know its cache was stale. Together they take it 24 -> 8.
"""

from __future__ import annotations

import psycopg
import pytest

from secantus.sql.engine import run_sql
from secantus.sql.pgserver import SecantusPGServer
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def dsn(tmp_path):
    srv = SecantusPGServer(storage_path=str(tmp_path), port=0)
    srv.start()
    host, port = srv.address
    conn_str = f"host={host} port={port} dbname=test user=test password=test"
    with psycopg.connect(conn_str, autocommit=True) as setup:
        setup.execute("CREATE TABLE t (id int primary key, a int)")
        setup.execute("INSERT INTO t VALUES (1, 1)")
    try:
        yield conn_str
    finally:
        srv.stop()


def _sqlstate(exc: Exception) -> str | None:
    return getattr(exc, "sqlstate", None)


class TestDeallocateAllCommandTag:
    def test_all_form_reports_deallocate_all(self, tmp_path):
        storage = Storage(str(tmp_path / "wt"))
        session = Session(database="t")
        try:
            res = run_sql(storage, "t", "DEALLOCATE ALL", session=session)[0]
            assert res.command_tag == "DEALLOCATE ALL"
        finally:
            storage.close()

    def test_named_form_still_reports_deallocate(self, tmp_path):
        storage = Storage(str(tmp_path / "wt"))
        session = Session(database="t")
        try:
            run_sql(storage, "t", "PREPARE p AS SELECT 1", session=session)
            res = run_sql(storage, "t", "DEALLOCATE p", session=session)[0]
            assert res.command_tag == "DEALLOCATE"
        finally:
            storage.close()


class TestErrorInsideTransactionAborts:
    def test_protocol_error_poisons_the_block(self, dsn):
        """A missing prepared statement is raised by the protocol layer, not by
        the engine — that is the path that used to leave the block usable."""
        conn = psycopg.connect(dsn)
        try:
            cur = conn.cursor()
            cur.execute("SELECT * FROM t", prepare=True)
            cur.execute("SELECT * FROM t", prepare=True)  # now server-side cached
            conn.execute("DEALLOCATE ALL")
            with pytest.raises(psycopg.Error) as first:
                cur.execute("SELECT * FROM t", prepare=True)
            assert _sqlstate(first.value) == "26000"
            with pytest.raises(psycopg.Error) as second:
                cur.execute("SELECT 1")
            assert _sqlstate(second.value) == "25P02"
        finally:
            conn.close()

    def test_rollback_recovers_the_block(self, dsn):
        conn = psycopg.connect(dsn)
        try:
            cur = conn.cursor()
            with pytest.raises(psycopg.Error):
                cur.execute("SELECT * FROM nosuchtable")
            conn.rollback()
            cur.execute("SELECT 1")
            assert cur.fetchone() == (1,)
        finally:
            conn.close()

    def test_savepoint_rollback_recovers_the_block(self, dsn):
        """pgjdbc's autosave modes depend on this: an error inside a savepoint
        must not condemn the whole transaction."""
        conn = psycopg.connect(dsn)
        try:
            cur = conn.cursor()
            cur.execute("SAVEPOINT sp1")
            with pytest.raises(psycopg.Error):
                cur.execute("SELECT * FROM nosuchtable")
            cur.execute("ROLLBACK TO SAVEPOINT sp1")
            cur.execute("SELECT 1")
            assert cur.fetchone() == (1,)
        finally:
            conn.close()

    def test_autocommit_is_unaffected(self, dsn):
        """Outside a transaction there is no block to abort — the next
        statement must simply work."""
        with psycopg.connect(dsn, autocommit=True) as conn:
            with pytest.raises(psycopg.Error):
                conn.execute("SELECT * FROM nosuchtable")
            assert conn.execute("SELECT 1").fetchone() == (1,)
