"""Read-only transaction enforcement + the transaction-isolation round-trip.

PG blocks writes in a read-only transaction with 25006, whether the
read-only-ness came from ``BEGIN READ ONLY``, ``SET TRANSACTION READ ONLY``,
or ``SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY``; and
``SHOW TRANSACTION ISOLATION LEVEL`` (the multi-word spelling pgjdbc's
``getTransactionIsolation`` issues verbatim) reports the level a
``SET SESSION CHARACTERISTICS`` planted.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


@pytest.fixture
def session():
    return Session(database=DB)


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def sqlstate(storage, session, sql):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, sql)
    return ei.value.sqlstate


def test_show_transaction_isolation_level_round_trips(storage, session):
    run(storage, session, "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE")
    assert run(storage, session, "SHOW TRANSACTION ISOLATION LEVEL").rows == [("serializable",)]
    run(
        storage,
        session,
        "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ",
    )
    assert run(storage, session, "SHOW TRANSACTION ISOLATION LEVEL").rows == [("repeatable read",)]


def test_session_characteristics_read_only_blocks_writes(storage, session):
    run(storage, session, "CREATE TABLE t (id int)")
    run(storage, session, "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
    assert sqlstate(storage, session, "INSERT INTO t VALUES (1)") == "25006"
    assert sqlstate(storage, session, "UPDATE t SET id = 2") == "25006"
    assert sqlstate(storage, session, "DELETE FROM t") == "25006"
    assert sqlstate(storage, session, "CREATE TABLE u (id int)") == "25006"
    assert sqlstate(storage, session, "DROP TABLE t") == "25006"
    assert sqlstate(storage, session, "TRUNCATE t") == "25006"
    # Reads stay allowed.
    assert run(storage, session, "SELECT count(*) FROM t").rows == [(0,)]
    # READ WRITE restores writes.
    run(storage, session, "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE")
    assert run(storage, session, "INSERT INTO t VALUES (1)").command_tag == "INSERT 0 1"


def test_begin_read_only_blocks_writes_for_the_block_only(storage, session):
    run(storage, session, "CREATE TABLE t (id int)")
    run(storage, session, "BEGIN READ ONLY")
    assert sqlstate(storage, session, "INSERT INTO t VALUES (1)") == "25006"
    run(storage, session, "ROLLBACK")
    assert run(storage, session, "INSERT INTO t VALUES (1)").command_tag == "INSERT 0 1"


def test_set_transaction_read_only_inside_block(storage, session):
    run(storage, session, "CREATE TABLE t (id int)")
    run(storage, session, "BEGIN")
    run(storage, session, "SET TRANSACTION READ ONLY")
    assert sqlstate(storage, session, "INSERT INTO t VALUES (1)") == "25006"
    run(storage, session, "ROLLBACK")
    assert run(storage, session, "INSERT INTO t VALUES (1)").command_tag == "INSERT 0 1"
