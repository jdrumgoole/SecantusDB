"""Composite type follow-ups (#100): ``(col).field`` in a WHERE predicate,
``UPDATE … SET col.field = v`` / ``SET col = ROW(...)``, and pg_attribute
field-level reflection (pg_type.typrelid -> pg_class relkind='c' -> pg_attribute).
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
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


@pytest.fixture
def people(storage, session):
    run(storage, session, "CREATE TYPE addr AS (street text, zip int)")
    run(storage, session, "CREATE TABLE people (id int PRIMARY KEY, home addr)")
    run(
        storage,
        session,
        "INSERT INTO people VALUES (1, ROW('Main St', 90210)), (2, ROW('Oak Ave', 10001))",
    )
    return storage


# -- (col).field in WHERE ----------------------------------------------------- #


def test_where_field_equality(people, session):
    assert run(people, session, "SELECT id FROM people WHERE (home).zip = 10001").rows == [(2,)]


def test_where_field_text(people, session):
    assert run(people, session, "SELECT id FROM people WHERE (home).street = 'Main St'").rows == [
        (1,)
    ]


def test_where_field_range(people, session):
    assert run(people, session, "SELECT id FROM people WHERE (home).zip > 50000").rows == [(1,)]


# -- UPDATE SET col.field / col = ROW(...) ------------------------------------ #


def test_update_subfield(people, session):
    run(people, session, "UPDATE people SET home.zip = 55555 WHERE id = 1")
    assert run(people, session, "SELECT (home).zip FROM people WHERE id = 1").rows == [(55555,)]
    # The other field is untouched.
    assert run(people, session, "SELECT (home).street FROM people WHERE id = 1").rows == [
        ("Main St",)
    ]


def test_update_whole_composite(people, session):
    run(people, session, "UPDATE people SET home = ROW('New Rd', 12345) WHERE id = 1")
    assert run(
        people, session, "SELECT (home).street, (home).zip FROM people WHERE id = 1"
    ).rows == [("New Rd", 12345)]


def test_update_subfield_filtered_by_composite_field(people, session):
    run(people, session, "UPDATE people SET home.street = 'Renamed' WHERE (home).zip = 10001")
    assert run(people, session, "SELECT id, (home).street FROM people ORDER BY id").rows == [
        (1, "Main St"),
        (2, "Renamed"),
    ]


def test_update_unknown_subfield_errors(people, session):
    try:
        run(people, session, "UPDATE people SET home.nope = 1 WHERE id = 1")
        raise AssertionError("expected an error")
    except Exception as e:  # noqa: BLE001
        assert getattr(e, "sqlstate", getattr(e, "code", None)) == "42703"


# -- pg_attribute field-level reflection -------------------------------------- #


def test_composite_fields_reflect_via_pg_attribute(people, session):
    rows = run(
        people,
        session,
        "SELECT a.attname, a.atttypid, a.attnum FROM pg_type t "
        "JOIN pg_class c ON c.oid = t.typrelid "
        "JOIN pg_attribute a ON a.attrelid = c.oid "
        "WHERE t.typname = 'addr' ORDER BY a.attnum",
    ).rows
    assert rows == [("street", 25, 1), ("zip", 23, 2)]  # text=25, int4=23


def test_composite_relation_is_relkind_c(people, session):
    rows = run(people, session, "SELECT relname FROM pg_class WHERE relkind = 'c'").rows
    assert rows == [("addr",)]


def test_composite_reltype_points_back_to_type(people, session):
    # pg_class.reltype of the composite relation equals the pg_type.oid.
    rows = run(
        people,
        session,
        "SELECT c.reltype = t.oid FROM pg_class c JOIN pg_type t ON t.typname = c.relname "
        "WHERE c.relkind = 'c' AND c.relname = 'addr'",
    ).rows
    assert rows == [(True,)]
