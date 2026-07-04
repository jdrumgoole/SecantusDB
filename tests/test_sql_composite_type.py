"""Composite types: ``CREATE TYPE t AS (a int, b text)`` — a composite-typed
column stores a subdocument, ``ROW(…)`` writes it, ``(col).field`` reads a field,
and the whole value renders as a Postgres record literal ``(a,b)``. Reflected via
pg_type (``typtype = 'c'``).
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql, typemap
from secantus.sql.session import Session
from sqlfake import FakeStorage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage():
    return FakeStorage()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def sqlstate(storage, session, sql):
    try:
        run(storage, session, sql)
        return None
    except Exception as e:  # noqa: BLE001
        return getattr(e, "sqlstate", getattr(e, "code", None))


@pytest.fixture
def addr(storage, session):
    run(storage, session, "CREATE TYPE addr AS (street text, zip int)")
    return storage


# -- CREATE / DROP TYPE ------------------------------------------------------- #


def test_create_composite_type(addr, session):
    rows = run(addr, session, "SELECT typname, typtype FROM pg_type WHERE typname = 'addr'").rows
    assert rows == [("addr", "c")]


def test_duplicate_composite_type_errors(addr, session):
    assert sqlstate(addr, session, "CREATE TYPE addr AS (a int)") == "42710"


def test_drop_composite_type(addr, session):
    assert run(addr, session, "DROP TYPE addr").command_tag == "DROP TYPE"
    assert run(addr, session, "SELECT typname FROM pg_type WHERE typname = 'addr'").rows == []


def test_drop_missing_type_errors(storage, session):
    assert sqlstate(storage, session, "DROP TYPE nope") == "42704"


def test_drop_missing_type_if_exists(storage, session):
    assert run(storage, session, "DROP TYPE IF EXISTS nope").command_tag == "DROP TYPE"


# -- composite columns: write + read ------------------------------------------ #


@pytest.fixture
def people(addr, session):
    run(addr, session, "CREATE TABLE people (id int PRIMARY KEY, home addr)")
    run(
        addr,
        session,
        "INSERT INTO people VALUES (1, ROW('Main St', 90210)), (2, ROW('Oak Ave', 10001))",
    )
    return addr


def test_field_access(people, session):
    rows = run(people, session, "SELECT id, (home).street, (home).zip FROM people ORDER BY id").rows
    assert rows == [(1, "Main St", 90210), (2, "Oak Ave", 10001)]


def test_field_access_types(people, session):
    cols = run(people, session, "SELECT (home).street, (home).zip FROM people").columns
    assert [c.type_tag for c in cols] == ["text", "int4"]


def test_whole_composite_is_a_subdocument(people, session):
    rows = run(people, session, "SELECT home FROM people ORDER BY id").rows
    assert rows[0][0] == {"street": "Main St", "zip": 90210}


def test_whole_composite_column_type(people, session):
    cols = run(people, session, "SELECT home FROM people").columns
    assert cols[0].type_tag == "composite"


# -- NULL composite ----------------------------------------------------------- #


def test_null_composite(addr, session):
    run(addr, session, "CREATE TABLE p2 (id int PRIMARY KEY, h addr)")
    run(addr, session, "INSERT INTO p2 VALUES (1, NULL)")
    assert run(addr, session, "SELECT (h).street FROM p2").rows == [(None,)]
    assert run(addr, session, "SELECT h FROM p2").rows == [(None,)]


# -- wire text rendering ------------------------------------------------------ #


def test_record_text_rendering():
    assert (
        typemap.to_pg_text({"street": "Main St", "zip": 90210}, "composite") == b'("Main St",90210)'
    )
    # NULL field is empty; a comma-bearing field is double-quoted.
    assert typemap.to_pg_text({"a": None, "b": "x,y", "c": 5}, "composite") == b'(,"x,y",5)'


def test_composite_result_oid_is_record():
    assert typemap.PG_OID["composite"] == 2249


# -- unsupported field type in the composite definition ----------------------- #


def test_bad_field_type_errors(storage, session):
    assert sqlstate(storage, session, "CREATE TYPE bad AS (x notatype)") == "0A000"
