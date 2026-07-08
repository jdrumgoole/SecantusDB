"""``CREATE TYPE … AS ENUM`` — enum types, enum-typed columns with value
validation, and ``pg_type`` / ``pg_enum`` reflection.

An enum column stores text but rejects a value outside the enum's declared labels
(``22P02``). Enum types reflect through ``pg_type`` (``typtype = 'e'``) and
``pg_enum`` (one row per label) so SQLAlchemy / psql's ``\\dT`` see them.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def sqlstate(storage, session, sql):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, sql)
    return ei.value.sqlstate


@pytest.fixture
def mood(storage, session):
    run(storage, session, "CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy')")
    return storage


# -- CREATE / DROP TYPE -------------------------------------------------------- #


def test_create_type_enum(storage, session):
    assert run(storage, session, "CREATE TYPE mood AS ENUM ('a', 'b')").command_tag == "CREATE TYPE"


def test_duplicate_type_rejected(mood, session):
    assert sqlstate(mood, session, "CREATE TYPE mood AS ENUM ('x')") == "42710"


def test_drop_type(mood, session):
    assert run(mood, session, "DROP TYPE mood").command_tag == "DROP TYPE"
    assert sqlstate(mood, session, "DROP TYPE mood") == "42704"


def test_drop_type_if_exists(storage, session):
    assert run(storage, session, "DROP TYPE IF EXISTS nope").command_tag == "DROP TYPE"


def test_range_create_type_unsupported(storage, session):
    # Composite types are supported (see test_sql_composite_type); range / base
    # types remain a faithful not-supported (0A000).
    assert sqlstate(storage, session, "CREATE TYPE fr AS RANGE (subtype = float8)") == "0A000"


# -- enum-typed columns -------------------------------------------------------- #


def test_enum_column_accepts_valid_label(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    run(mood, session, "INSERT INTO t (id, m) VALUES (1, 'happy')")
    assert run(mood, session, "SELECT id, m FROM t").rows == [(1, "happy")]


def test_enum_column_rejects_invalid_label(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    assert sqlstate(mood, session, "INSERT INTO t (id, m) VALUES (1, 'furious')") == "22P02"


def test_enum_column_allows_null(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    run(mood, session, "INSERT INTO t (id, m) VALUES (1, NULL)")
    assert run(mood, session, "SELECT m FROM t").rows == [(None,)]


def test_enum_column_update_validates(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    run(mood, session, "INSERT INTO t (id, m) VALUES (1, 'ok')")
    run(mood, session, "UPDATE t SET m = 'sad' WHERE id = 1")
    assert run(mood, session, "SELECT m FROM t").rows == [("sad",)]
    assert sqlstate(mood, session, "UPDATE t SET m = 'nope' WHERE id = 1") == "22P02"


def test_column_of_unknown_type_errors(storage, session):
    assert sqlstate(storage, session, "CREATE TABLE t (id int, m no_such_type)") == "42704"


# -- reflection ---------------------------------------------------------------- #


def test_pg_type_lists_enum(mood, session):
    rows = run(
        mood, session, "SELECT typname, typtype FROM pg_catalog.pg_type WHERE typtype = 'e'"
    ).rows
    assert rows == [("mood", "e")]


def test_pg_enum_lists_labels_in_order(mood, session):
    rows = run(
        mood,
        session,
        "SELECT enumlabel, enumsortorder FROM pg_catalog.pg_enum ORDER BY enumsortorder",
    ).rows
    assert rows == [("sad", 1.0), ("ok", 2.0), ("happy", 3.0)]


def test_enum_column_atttypid_matches_type_oid(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    att = run(mood, session, "SELECT atttypid FROM pg_attribute WHERE attname = 'm'").rows
    typ = run(mood, session, "SELECT oid FROM pg_type WHERE typname = 'mood'").rows
    assert att == typ and att != [(25,)]  # points at the enum oid, not text
