"""SET LOCAL + SHOW ALL / pg_settings (#136): transaction-scoped GUCs that revert
at COMMIT/ROLLBACK, SHOW ALL as a three-column table, and the
``pg_catalog.pg_settings`` view. Driven over the real ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "app"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def _run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def test_custom_namespaced_guc_roundtrips(storage):
    # A custom (extension) GUC ``namespace.name`` parses with the namespace as the
    # LHS column's table part; SET must keep the full dotted name so SHOW finds it.
    sess = Session(database=DB)
    _run(storage, sess, "SET custom_option.session_setting = 'abc'")
    assert _run(storage, sess, "SHOW custom_option.session_setting").rows == [("abc",)]


def test_set_local_reverts_at_commit(storage):
    sess = Session(database=DB)
    _run(storage, sess, "BEGIN")
    _run(storage, sess, "SET LOCAL statement_timeout = '5s'")
    assert _run(storage, sess, "SHOW statement_timeout").rows == [("5s",)]
    _run(storage, sess, "COMMIT")
    # Not set before the txn → reverts to unset (empty).
    assert _run(storage, sess, "SHOW statement_timeout").rows == [("",)]


def test_set_local_reverts_at_rollback_to_session_value(storage):
    sess = Session(database=DB)
    _run(storage, sess, "SET statement_timeout = '10s'")  # session value
    _run(storage, sess, "BEGIN")
    _run(storage, sess, "SET LOCAL statement_timeout = '99s'")
    assert _run(storage, sess, "SHOW statement_timeout").rows == [("99s",)]
    _run(storage, sess, "ROLLBACK")
    # Reverts to the pre-SET-LOCAL session value, not unset.
    assert _run(storage, sess, "SHOW statement_timeout").rows == [("10s",)]


def test_set_local_outside_txn_has_no_lasting_effect(storage):
    sess = Session(database=DB)
    _run(storage, sess, "SET LOCAL search_path = 'zzz'")
    # No open transaction → the SET LOCAL is dropped (default search_path stands).
    assert _run(storage, sess, "SHOW search_path").rows == [('"$user", public',)]


def test_plain_set_inside_txn_survives_commit(storage):
    # A non-LOCAL SET is session-scoped: it persists past COMMIT.
    sess = Session(database=DB)
    _run(storage, sess, "BEGIN")
    _run(storage, sess, "SET statement_timeout = '7s'")
    _run(storage, sess, "COMMIT")
    assert _run(storage, sess, "SHOW statement_timeout").rows == [("7s",)]


def test_show_all_columns_and_rows(storage):
    sess = Session(database=DB)
    res = _run(storage, sess, "SHOW ALL")
    assert [c.name for c in res.columns] == ["name", "setting", "description"]
    by_name = {r[0]: r[1] for r in res.rows}
    assert by_name["client_encoding"] == "UTF8"
    assert by_name["TimeZone"] == "UTC"
    # Rows are sorted by name.
    names = [r[0] for r in res.rows]
    assert names == sorted(names)


def test_show_all_reflects_overrides(storage):
    sess = Session(database=DB)
    _run(storage, sess, "SET TimeZone = 'America/New_York'")
    res = _run(storage, sess, "SHOW ALL")
    by_name = {r[0]: r[1] for r in res.rows}
    assert by_name["TimeZone"] == "America/New_York"


def test_pg_settings_rows(storage):
    sess = Session(database=DB)
    _run(storage, sess, "SET statement_timeout = '10s'")
    res = _run(
        storage,
        sess,
        "SELECT name, setting, vartype, source, pending_restart "
        "FROM pg_catalog.pg_settings "
        "WHERE name IN ('statement_timeout', 'client_encoding') ORDER BY name",
    )
    assert res.rows == [
        ("client_encoding", "UTF8", "string", "default", False),
        ("statement_timeout", "10s", "string", "session", False),
    ]


def test_pg_settings_boot_and_reset_val(storage):
    sess = Session(database=DB)
    _run(storage, sess, "SET client_encoding = 'LATIN1'")
    res = _run(
        storage,
        sess,
        "SELECT setting, boot_val, reset_val FROM pg_catalog.pg_settings "
        "WHERE name = 'client_encoding'",
    )
    # setting reflects the override; boot_val/reset_val stay at the default.
    assert res.rows == [("LATIN1", "UTF8", "UTF8")]


class TestTxnScopedSetAndReporting:
    """PG unwinds a plain in-block ``SET`` on ROLLBACK (keeps it on COMMIT),
    and re-reports GUC_REPORT parameters when a SET LOCAL or rolled-back SET
    unwinds — pgjdbc's ParameterStatusTest transactionalParameters* trio."""

    def test_plain_set_unwinds_on_rollback(self, storage):
        sess = Session(database=DB)
        _run(storage, sess, "SET application_name = 'base'")
        _run(storage, sess, "BEGIN")
        _run(storage, sess, "SET application_name = 'txn'")
        assert _run(storage, sess, "SHOW application_name").rows == [("txn",)]
        _run(storage, sess, "ROLLBACK")
        assert _run(storage, sess, "SHOW application_name").rows == [("base",)]

    def test_plain_set_survives_commit(self, storage):
        sess = Session(database=DB)
        _run(storage, sess, "BEGIN")
        _run(storage, sess, "SET application_name = 'kept'")
        _run(storage, sess, "COMMIT")
        assert _run(storage, sess, "SHOW application_name").rows == [("kept",)]

    def test_commit_of_failed_block_unwinds_set(self, storage):
        sess = Session(database=DB)
        _run(storage, sess, "SET application_name = 'base'")
        _run(storage, sess, "BEGIN")
        _run(storage, sess, "SET application_name = 'doomed'")
        from secantus.sql import errors as sql_errors

        with pytest.raises(sql_errors.SQLError):
            _run(storage, sess, "SELECT * FROM missing_relation_xyz")
        _run(storage, sess, "COMMIT")  # acts as ROLLBACK on a failed block
        assert _run(storage, sess, "SHOW application_name").rows == [("base",)]

    def test_unwind_reports_parameter_status(self, storage):
        sess = Session(database=DB)
        _run(storage, sess, "SET application_name = 'base'")
        sess.pending_parameter_status = []
        _run(storage, sess, "BEGIN")
        _run(storage, sess, "SET LOCAL application_name = 'local'")
        _run(storage, sess, "COMMIT")
        assert ("application_name", "base") in sess.pending_parameter_status

    def test_rollback_reports_reverted_plain_set(self, storage):
        sess = Session(database=DB)
        _run(storage, sess, "SET application_name = 'base'")
        sess.pending_parameter_status = []
        _run(storage, sess, "BEGIN")
        _run(storage, sess, "SET application_name = 'txn'")
        _run(storage, sess, "ROLLBACK")
        assert sess.pending_parameter_status[-1] == ("application_name", "base")

    def test_savepoint_alias_shapes_accepted_by_extended_parse(self, storage):
        # SAVEPOINT / RELEASE parse as a bare Alias in sqlglot; the extended
        # protocol's garbage guard (#876) must not reject them.
        import struct

        from secantus.sql.pgextended import ExtendedSession

        ext = ExtendedSession(storage, Session(database=DB))

        def run(q):
            payload = b"\x00" + q.encode() + b"\x00" + struct.pack(">h", 0)
            out = ext.process("P", payload)
            assert b"syntax error" not in bytes(out), q

        run("BEGIN")
        run("SAVEPOINT sp1")
        run("RELEASE SAVEPOINT sp1")
        run("START TRANSACTION")

    def test_bare_start_transaction_opens_a_block(self, storage):
        sess = Session(database=DB)
        res = _run(storage, sess, "START TRANSACTION")
        assert res.command_tag == "BEGIN"
        assert sess.txn_handle is not None
        _run(storage, sess, "COMMIT")
