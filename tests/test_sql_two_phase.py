"""Two-phase commit: PREPARE TRANSACTION / COMMIT PREPARED / ROLLBACK PREPARED (#139).

Driven through ``run_sql`` over the real WT-backed ``Storage``. ``PREPARE
TRANSACTION 'gid'`` detaches the open block's user-transaction into a
server-wide registry (uncommitted); a later ``COMMIT PREPARED 'gid'`` /
``ROLLBACK PREPARED 'gid'`` — possibly on a different session — resolves it.
The cross-connection wire path is covered in ``test_pgserver_pg8000.py``.
"""

from __future__ import annotations

import datetime as _dt

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import PreparedXactRegistry, Session
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
    # A shared registry, as the wire server assigns, so a second session can
    # resolve a gid prepared on the first.
    s = Session(database=DB)
    s.prepared_xacts = PreparedXactRegistry()
    return s


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)


def one(storage, session, sql):
    return run(storage, session, sql)[0]


def rows(storage, session, sql):
    return one(storage, session, sql).rows


def setup_table(storage, session):
    run(storage, session, "CREATE TABLE t (id int primary key, v text)")


# --------------------------------------------------------------------------- #
# Happy paths


def test_prepare_then_commit_prepared(storage, session):
    setup_table(storage, session)
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO t VALUES (1, 'a')")
    res = one(storage, session, "PREPARE TRANSACTION 'g1'")
    assert res.command_tag == "PREPARE TRANSACTION"
    # The block is over on this session; no active transaction.
    assert session.txn_handle is None
    # Uncommitted: an autocommit read sees nothing.
    assert rows(storage, session, "SELECT * FROM t") == []
    res = one(storage, session, "COMMIT PREPARED 'g1'")
    assert res.command_tag == "COMMIT PREPARED"
    assert rows(storage, session, "SELECT * FROM t") == [(1, "a")]


def test_prepare_then_rollback_prepared(storage, session):
    setup_table(storage, session)
    run(storage, session, "INSERT INTO t VALUES (1, 'keep')")
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO t VALUES (2, 'drop')")
    run(storage, session, "PREPARE TRANSACTION 'g2'")
    res = one(storage, session, "ROLLBACK PREPARED 'g2'")
    assert res.command_tag == "ROLLBACK PREPARED"
    # Only the pre-prepared row survives.
    assert rows(storage, session, "SELECT id FROM t ORDER BY id") == [(1,)]


def test_cross_session_commit(storage):
    reg = PreparedXactRegistry()
    a = Session(database=DB)
    a.prepared_xacts = reg
    b = Session(database=DB)
    b.prepared_xacts = reg
    setup_table(storage, a)
    run(storage, a, "BEGIN")
    run(storage, a, "INSERT INTO t VALUES (7, 'x')")
    run(storage, a, "PREPARE TRANSACTION 'shared'")
    # Session B sees nothing until it commits the prepared xact.
    assert rows(storage, b, "SELECT * FROM t") == []
    run(storage, b, "COMMIT PREPARED 'shared'")
    assert rows(storage, b, "SELECT * FROM t") == [(7, "x")]


def test_prepare_updates_and_deletes(storage, session):
    setup_table(storage, session)
    run(storage, session, "INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c')")
    run(storage, session, "BEGIN")
    run(storage, session, "UPDATE t SET v = 'B' WHERE id = 2")
    run(storage, session, "DELETE FROM t WHERE id = 3")
    run(storage, session, "PREPARE TRANSACTION 'gmix'")
    # Still the pre-transaction state until commit.
    assert rows(storage, session, "SELECT id, v FROM t ORDER BY id") == [
        (1, "a"),
        (2, "b"),
        (3, "c"),
    ]
    run(storage, session, "COMMIT PREPARED 'gmix'")
    assert rows(storage, session, "SELECT id, v FROM t ORDER BY id") == [(1, "a"), (2, "B")]


# --------------------------------------------------------------------------- #
# pg_prepared_xacts reflection


def test_pg_prepared_xacts_reflects_open_prepared(storage, session):
    setup_table(storage, session)
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO t VALUES (1, 'a')")
    before = _dt.datetime.now(_dt.timezone.utc)
    run(storage, session, "PREPARE TRANSACTION 'gp'")
    r = one(
        storage,
        session,
        "SELECT gid, owner, database, prepared FROM pg_catalog.pg_prepared_xacts",
    )
    assert len(r.rows) == 1
    gid, owner, database, prepared = r.rows[0]
    assert gid == "gp"
    assert owner == session.effective_user
    assert database == DB
    assert isinstance(prepared, _dt.datetime) and prepared >= before
    # Gone once committed.
    run(storage, session, "COMMIT PREPARED 'gp'")
    r = one(storage, session, "SELECT count(*) FROM pg_catalog.pg_prepared_xacts")
    assert r.rows[0][0] == 0


def test_pg_prepared_xacts_empty_when_none(storage, session):
    r = one(storage, session, "SELECT count(*) FROM pg_catalog.pg_prepared_xacts")
    assert r.rows[0][0] == 0


# --------------------------------------------------------------------------- #
# Error paths


def test_prepare_without_transaction_errors(storage, session):
    setup_table(storage, session)
    with pytest.raises(SQLError) as exc:
        run(storage, session, "PREPARE TRANSACTION 'nope'")
    assert exc.value.sqlstate == "25P01"


def test_commit_prepared_unknown_gid_errors(storage, session):
    with pytest.raises(SQLError) as exc:
        run(storage, session, "COMMIT PREPARED 'ghost'")
    assert exc.value.sqlstate == "42704"


def test_rollback_prepared_unknown_gid_errors(storage, session):
    with pytest.raises(SQLError) as exc:
        run(storage, session, "ROLLBACK PREPARED 'ghost'")
    assert exc.value.sqlstate == "42704"


def test_duplicate_gid_errors(storage, session):
    setup_table(storage, session)
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO t VALUES (1, 'a')")
    run(storage, session, "PREPARE TRANSACTION 'dup'")
    # A second block prepared under the same gid is rejected.
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO t VALUES (2, 'b')")
    with pytest.raises(SQLError) as exc:
        run(storage, session, "PREPARE TRANSACTION 'dup'")
    assert exc.value.sqlstate == "42710"
    # The first prepared xact is intact and still commits.
    run(storage, session, "ROLLBACK")
    run(storage, session, "COMMIT PREPARED 'dup'")
    assert rows(storage, session, "SELECT id FROM t ORDER BY id") == [(1,)]


def test_commit_prepared_inside_transaction_errors(storage, session):
    setup_table(storage, session)
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO t VALUES (1, 'a')")
    run(storage, session, "PREPARE TRANSACTION 'g'")
    # Now open a new block and try to COMMIT PREPARED inside it.
    run(storage, session, "BEGIN")
    with pytest.raises(SQLError) as exc:
        run(storage, session, "COMMIT PREPARED 'g'")
    assert exc.value.sqlstate == "25001"
    run(storage, session, "ROLLBACK")
    run(storage, session, "COMMIT PREPARED 'g'")


def test_gid_with_escaped_quote(storage, session):
    setup_table(storage, session)
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO t VALUES (1, 'a')")
    run(storage, session, "PREPARE TRANSACTION 'a''b'")
    r = one(storage, session, "SELECT gid FROM pg_catalog.pg_prepared_xacts")
    assert r.rows[0][0] == "a'b"
    run(storage, session, "COMMIT PREPARED 'a''b'")
    assert rows(storage, session, "SELECT * FROM t") == [(1, "a")]
