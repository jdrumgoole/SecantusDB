"""Secondary-read RBAC for SQL write statements (#785, #881).

A write statement's *source* clause reads tables its primary write privilege
doesn't cover: ``INSERT ... SELECT``, ``UPDATE ... FROM``, ``DELETE ... USING``,
``CREATE TABLE ... AS SELECT``, and subqueries. Before the fix, RBAC checked
only the primary target, so a principal holding a table-level ``INSERT`` on one
table (and nothing on another) could exfiltrate the second table's rows through
the source clause. Each source table now requires its own ``find`` (SELECT)
grant.

Driven over the real ``Storage`` with hand-built gated sessions, mirroring
``test_sql_authz.py``. A table-level ``GRANT`` is the vehicle because a db-wide
``readWrite`` role grants ``find`` db-wide and so wouldn't exercise the gap.
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "app"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    admin = Session(database=DB)
    run_sql(s, DB, "CREATE TABLE scratch (id bigint primary key, v int)", session=admin)
    run_sql(s, DB, "CREATE TABLE secrets (id bigint primary key, v int)", session=admin)
    run_sql(s, DB, "INSERT INTO secrets VALUES (1, 42), (2, 43)", session=admin)
    # writer may write scratch and read scratch, but has NO grant on secrets.
    run_sql(s, DB, "GRANT INSERT ON scratch TO writer", session=admin)
    run_sql(s, DB, "GRANT UPDATE ON scratch TO writer", session=admin)
    run_sql(s, DB, "GRANT DELETE ON scratch TO writer", session=admin)
    run_sql(s, DB, "GRANT SELECT ON scratch TO writer", session=admin)
    # For CTAS the writer also needs table-create; use a db-wide dbAdmin-ish
    # grant via a role binding plus the scratch read grant, with NO secrets read.
    try:
        yield s
    finally:
        s.close()


def _writer() -> Session:
    # A login named "writer": its table grants resolve by identity, no db-wide
    # role. authz is active, so every statement is gated.
    return Session(database=DB, user="writer", authz_active=True, roles=[])


def _run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)


def _denied(storage, session, sql) -> None:
    with pytest.raises(SQLError) as ei:
        _run(storage, session, sql)
    assert ei.value.sqlstate == "42501", f"expected 42501, got {ei.value.sqlstate}"


def test_insert_select_needs_read_on_source(storage):
    w = _writer()
    # INSERT ... VALUES on the writable table is fine (no source read).
    _run(storage, w, "INSERT INTO scratch VALUES (100, 1)")
    # INSERT ... SELECT FROM secrets must be denied — no SELECT on secrets.
    _denied(storage, w, "INSERT INTO scratch SELECT * FROM secrets")
    # Reading its own table as a source is allowed (writer has SELECT scratch).
    _run(storage, w, "INSERT INTO scratch SELECT id + 1000, v FROM scratch")


def test_update_from_needs_read_on_source(storage):
    w = _writer()
    _denied(
        storage,
        w,
        "UPDATE scratch SET v = secrets.v FROM secrets WHERE scratch.id = secrets.id",
    )


def test_delete_using_needs_read_on_source(storage):
    w = _writer()
    _denied(
        storage,
        w,
        "DELETE FROM scratch USING secrets WHERE scratch.id = secrets.id",
    )


def test_subquery_source_needs_read(storage):
    w = _writer()
    _denied(
        storage,
        w,
        "INSERT INTO scratch SELECT id, v FROM scratch WHERE v < (SELECT max(v) FROM secrets)",
    )


def test_ctas_needs_read_on_source(storage):
    """CREATE TABLE ... AS SELECT authorizes only CREATE on the new table — the
    SELECT source needs its own read grant (#881)."""
    # dbAdmin grants CREATE db-wide but NOT find; the writer identity's SELECT
    # grant is on scratch only. So CTAS from secrets must be denied, from
    # scratch allowed.
    admin = Session(database=DB)
    run_sql(storage, DB, "GRANT SELECT ON scratch TO cadmin", session=admin)
    cadmin = Session(
        database=DB,
        user="cadmin",
        authz_active=True,
        roles=[{"role": "dbAdmin", "db": DB}],
    )
    _denied(storage, cadmin, "CREATE TABLE stolen AS SELECT * FROM secrets")
    run_sql(storage, DB, "CREATE TABLE ok_copy AS SELECT * FROM scratch", session=cadmin)


def test_db_wide_reader_is_unaffected(storage):
    """A principal with a db-wide readWrite role reads any table — the source
    gate must not falsely deny it."""
    rw = Session(database=DB, user="rw", authz_active=True, roles=[{"role": "readWrite", "db": DB}])
    _run(storage, rw, "INSERT INTO scratch SELECT * FROM secrets")
