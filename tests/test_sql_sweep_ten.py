"""A tenth differential sweep — DDL, the catalog, and where a function is legal.

DDL came back strong (38 of 41 shapes already matched PostgreSQL 14.13), so
this file is short. Three findings, all measured:

**A session function was legal at the top of a SELECT and nowhere else.**
`current_setting('x')` worked; `current_setting('x') ~ '…'` — the same call one
level down — answered `42883 function current_setting(text) does not exist`.
Those functions were only ever reached from `plan_constant_select`, so any
operand position, WHERE clause or wrapping call lost them.

**`has_table_privilege` ignored the owner.** It consulted recorded GRANTs only,
so a table the caller had just created and could plainly read reported FALSE.
Postgres' owner holds every privilege implicitly; measured on 14.13 by creating
a table, granting SELECT to another role, and asking as the creator — true.
This is the REPORTING function; the authz gate has its own path and already
permitted the owner, which is exactly why the read worked while this denied it.

**`CREATE TABLE (id int, id int)` was accepted**, leaving a relation whose
second `id` was unreachable. Postgres rejects it at parse analysis with `42701`.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture
def store(tmp_path):
    s = Storage(str(tmp_path / "s10"))
    try:
        yield s
    finally:
        s.close()


def _rows(store, sql, session):
    return [r for r in run_sql(store, "t", sql, session=session)][0].rows


@pytest.fixture
def sess(store):
    s = Session(database="t")
    _rows(store, "CREATE TABLE c10 (id int PRIMARY KEY, n int NOT NULL)", s)
    _rows(store, "INSERT INTO c10 VALUES (1, 1)", s)
    return s


# --------------------------------------------------------------------------- #
# Session functions are legal wherever an expression is
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    "sql",
    [
        # Each of these is the SAME call one level below the top of the SELECT,
        # which is where it used to stop being resolvable.
        "SELECT current_setting('server_version_num') ~ '^[0-9]+$'",
        "SELECT length(current_setting('TimeZone')) > 0",
        "SELECT version() LIKE 'PostgreSQL%'",
        "SELECT coalesce(current_setting('nope', true), 'fallback') = 'fallback'",
        "SELECT id IS NOT NULL FROM c10 WHERE current_setting('TimeZone') IS NOT NULL",
    ],
)
def test_session_functions_in_any_position(store, sess, sql):
    assert _rows(store, sql, sess)[0][0] is True


def test_unknown_setting_still_raises_in_an_operand(store, sess):
    # Widening where these are reachable must not widen what they accept.
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, "SELECT length(current_setting('nope')) > 0", sess)
    assert exc.value.sqlstate == "42704"


# --------------------------------------------------------------------------- #
# has_table_privilege: the owner holds everything implicitly
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize("priv", ["SELECT", "INSERT", "UPDATE", "DELETE"])
def test_owner_has_every_privilege_without_a_grant(store, sess, priv):
    assert _rows(store, f"SELECT has_table_privilege('c10', '{priv}')", sess) == [(True,)]


def test_owner_privilege_tolerates_the_grant_option_suffix(store, sess):
    assert _rows(store, "SELECT has_table_privilege('c10', 'SELECT WITH GRANT OPTION')", sess) == [
        (True,)
    ]


def test_a_stranger_still_has_nothing(store, sess):
    # The owner rule must not hand privileges to anyone else.
    assert _rows(store, "SELECT has_table_privilege('nobody', 'c10', 'SELECT')", sess) == [(False,)]


def test_revoke_from_the_owner_is_honoured(store, sess):
    _rows(store, "REVOKE INSERT ON c10 FROM secantus", sess)
    assert _rows(store, "SELECT has_table_privilege('c10', 'INSERT')", sess) == [(False,)]
    assert _rows(store, "SELECT has_table_privilege('c10', 'SELECT')", sess) == [(True,)]


# --------------------------------------------------------------------------- #
# DDL rejections
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    "ddl",
    [
        "CREATE TABLE dup10 (id int, id int)",
        "CREATE TABLE dup10 (id int, ID int)",  # names fold, so this collides too
    ],
)
def test_duplicate_column_name_is_rejected(store, sess, ddl):
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, ddl, sess)
    assert exc.value.sqlstate == "42701"
    assert "specified more than once" in str(exc.value)


def test_distinct_column_names_still_create(store, sess):
    _rows(store, "CREATE TABLE ok10 (id int, other int)", sess)
    assert _rows(store, "SELECT count(*) FROM ok10", sess) == [(0,)]


def test_drop_column_error_names_the_relation(store, sess):
    # Postgres says `column "x" of relation "t" does not exist` for ALTER TABLE.
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, "ALTER TABLE c10 DROP COLUMN nope", sess)
    assert exc.value.sqlstate == "42703"
    assert 'of relation "c10"' in str(exc.value)


def test_drop_column_if_exists_is_still_a_no_op(store, sess):
    _rows(store, "ALTER TABLE c10 DROP COLUMN IF EXISTS nope", sess)
    assert _rows(store, "SELECT count(*) FROM c10", sess) == [(1,)]
