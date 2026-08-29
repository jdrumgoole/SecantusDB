"""RBAC + read-only-transaction gates for the large-object write paths (#836).

The Fastpath sub-protocol (pgjdbc's LargeObjectManager) and the SQL-callable
``lo_*`` scalars both skip the parts of the statement pipeline that enforce
write privileges and the read-only-transaction check. A write-privilege-less
session — or one inside ``BEGIN READ ONLY`` — could therefore create / write /
truncate / unlink large objects. Both paths are now gated.
"""

from __future__ import annotations

import struct

import pytest

from secantus.sql import SQLError, largeobjects, run_sql
from secantus.sql.largeobjects import LO_PROC_OIDS
from secantus.sql.pgserver import SecantusPGServer
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


# --- classifier ------------------------------------------------------------ #


def test_is_write_call_classifies_mutations() -> None:
    for name in ("lo_creat", "lo_create", "lowrite", "lo_truncate", "lo_truncate64", "lo_unlink"):
        assert largeobjects.is_write_call(LO_PROC_OIDS[name]), name
    for name in ("lo_open", "loread", "lo_lseek", "lo_tell", "lo_close"):
        assert not largeobjects.is_write_call(LO_PROC_OIDS[name]), name


# --- Fastpath RBAC + read-only gate ---------------------------------------- #


def _srv(storage) -> SecantusPGServer:
    return SecantusPGServer(port=0, storage=storage)


def test_fastpath_write_denied_without_write_privilege(storage) -> None:
    srv = _srv(storage)
    reader = Session(
        database=DB, user="reader", authz_active=True, roles=[{"role": "read", "db": DB}]
    )
    with pytest.raises(SQLError) as ei:
        srv._authorize_lo_write(reader)
    assert ei.value.sqlstate == "42501"


def test_fastpath_write_allowed_with_readwrite(storage) -> None:
    srv = _srv(storage)
    writer = Session(
        database=DB, user="w", authz_active=True, roles=[{"role": "readWrite", "db": DB}]
    )
    srv._authorize_lo_write(writer)  # no raise


def test_fastpath_write_blocked_in_read_only_txn(storage) -> None:
    srv = _srv(storage)
    # authz off, but a read-only transaction still bars the write.
    sess = Session(database=DB)
    sess.settings["transaction_read_only"] = "on"
    with pytest.raises(SQLError) as ei:
        srv._authorize_lo_write(sess)
    assert ei.value.sqlstate == "25006"


def test_fastpath_read_never_gated(storage) -> None:
    # A read call OID isn't classified as a write, so the handler never invokes
    # the gate — a read-only / unprivileged session can still read.
    assert not largeobjects.is_write_call(LO_PROC_OIDS["loread"])


# --- scalar lo_* read-only gate (the SELECT lo_unlink path) ----------------- #


def test_scalar_lo_unlink_blocked_in_read_only_txn(storage) -> None:
    admin = Session(database=DB)
    # Create an object to unlink.
    oid = struct.unpack(
        ">i",
        largeobjects.call(
            LO_PROC_OIDS["lo_creat"], [struct.pack(">i", -1)], storage=storage, db=DB, session=admin
        ),
    )[0]
    sess = Session(database=DB)
    run_sql(storage, DB, "BEGIN READ ONLY", session=sess)
    with pytest.raises(SQLError) as ei:
        run_sql(storage, DB, f"SELECT lo_unlink({oid})", session=sess)
    assert ei.value.sqlstate == "25006"
    run_sql(storage, DB, "ROLLBACK", session=sess)


def test_scalar_lo_creat_needs_write_privilege(storage) -> None:
    reader = Session(
        database=DB, user="reader", authz_active=True, roles=[{"role": "read", "db": DB}]
    )
    with pytest.raises(SQLError) as ei:
        run_sql(storage, DB, "SELECT lo_creat(-1)", session=reader)
    assert ei.value.sqlstate == "42501"


def test_scalar_lo_read_only_query_still_allowed(storage) -> None:
    reader = Session(
        database=DB, user="reader", authz_active=True, roles=[{"role": "read", "db": DB}]
    )
    # A plain read is unaffected by the LO write classification.
    assert run_sql(storage, DB, "SELECT 1", session=reader)[-1].rows == [(1,)]
