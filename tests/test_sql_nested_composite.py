"""Nested composite types (#105): a composite type whose own field is another
composite. Closes the gap left by #100.

A composite value stores as a subdocument; a nested composite field stores as a
nested subdocument. ``CREATE TYPE`` embeds a composite field's referenced type
fields recursively; ``ROW(a, ROW(b, c))`` builds the nested subdoc; ``(p).home``
and ``((p).home).street`` walk in; whole-value rendering emits the Postgres
nested record literal; reflection points a composite field at its type's oid.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql, typemap
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


def val(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


def col(storage, session, sql):
    return run(storage, session, sql).columns[0]


@pytest.fixture
def nested(storage, session):
    run(storage, session, "CREATE TYPE addr AS (street text, zip int)")
    run(storage, session, "CREATE TYPE person AS (name text, home addr)")
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, p person)")
    run(storage, session, "INSERT INTO t VALUES (1, ROW('Bob', ROW('Main St', 90210)))")
    return storage


def test_create_type_nested_field_recorded(storage, session):
    run(storage, session, "CREATE TYPE addr AS (street text, zip int)")
    run(storage, session, "CREATE TYPE person AS (name text, home addr)")
    from secantus.sql.catalog import Catalog

    cat = Catalog(storage)
    fields = cat.get_composite(DB, "person")
    # home is a composite field: (name, subtype, subfields)
    assert fields[0] == ("name", "text", None)
    assert fields[1][0] == "home" and fields[1][1] == "addr"
    assert fields[1][2] == (("street", "text", None), ("zip", "int4", None))


def test_self_reference_rejected(storage, session):
    from secantus.sql import errors

    with pytest.raises(errors.SQLError):
        run(storage, session, "CREATE TYPE loop AS (x int, me loop)")


def test_unknown_field_type_rejected(storage, session):
    from secantus.sql import errors

    with pytest.raises(errors.SQLError):
        run(storage, session, "CREATE TYPE bad AS (x nonesuch)")


def test_insert_nested_row_stores_subdocument(nested, session):
    assert val(nested, session, "SELECT p FROM t WHERE id = 1") == {
        "name": "Bob",
        "home": {"street": "Main St", "zip": 90210},
    }


def test_whole_value_typed_composite(nested, session):
    assert col(nested, session, "SELECT p FROM t").type_tag == "composite"


def test_top_level_field_access(nested, session):
    assert val(nested, session, "SELECT (p).name FROM t WHERE id = 1") == "Bob"


def test_nested_composite_field_access(nested, session):
    assert val(nested, session, "SELECT (p).home FROM t WHERE id = 1") == {
        "street": "Main St",
        "zip": 90210,
    }


def test_nested_composite_field_typed_composite(nested, session):
    assert col(nested, session, "SELECT (p).home FROM t").type_tag == "composite"


def test_deep_field_access_text(nested, session):
    assert val(nested, session, "SELECT ((p).home).street FROM t WHERE id = 1") == "Main St"


def test_deep_field_access_int(nested, session):
    assert val(nested, session, "SELECT ((p).home).zip FROM t WHERE id = 1") == 90210


def test_deep_field_access_typing(nested, session):
    assert col(nested, session, "SELECT ((p).home).street FROM t").type_tag == "text"
    assert col(nested, session, "SELECT ((p).home).zip FROM t").type_tag == "int4"


def test_where_on_nested_field(nested, session):
    rows = run(nested, session, "SELECT id FROM t WHERE ((p).home).zip = 90210").rows
    assert [r[0] for r in rows] == [1]


def test_update_nested_composite_field(nested, session):
    run(nested, session, "UPDATE t SET p.home = ROW('Elm St', 11111) WHERE id = 1")
    assert val(nested, session, "SELECT ((p).home).street FROM t WHERE id = 1") == "Elm St"
    assert val(nested, session, "SELECT ((p).home).zip FROM t WHERE id = 1") == 11111


def test_nested_record_render_bytes():
    # The whole nested value renders as the Postgres nested record literal, with the
    # inner record quoted and its internal quotes / whitespace escaped.
    value = {"name": "Bob", "home": {"street": "Main St", "zip": 90210}}
    assert typemap.to_pg_text(value, "composite") == b'(Bob,"(""Main St"",90210)")'


def test_three_level_nesting(storage, session):
    run(storage, session, "CREATE TYPE geo AS (lat int, lng int)")
    run(storage, session, "CREATE TYPE addr AS (street text, at geo)")
    run(storage, session, "CREATE TYPE person AS (name text, home addr)")
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, p person)")
    run(storage, session, "INSERT INTO t VALUES (1, ROW('Al', ROW('Main', ROW(10, 20))))")
    assert val(storage, session, "SELECT (((p).home).at).lat FROM t WHERE id = 1") == 10
    assert val(storage, session, "SELECT (((p).home).at).lng FROM t WHERE id = 1") == 20


def test_reflection_field_points_at_subtype(storage, session):
    run(storage, session, "CREATE TYPE addr AS (street text, zip int)")
    run(storage, session, "CREATE TYPE person AS (name text, home addr)")
    rows = run(
        storage,
        session,
        """
        SELECT a.attname, t.typname
        FROM pg_type pt
        JOIN pg_class c ON pt.typrelid = c.oid
        JOIN pg_attribute a ON a.attrelid = c.oid
        JOIN pg_type t ON a.atttypid = t.oid
        WHERE pt.typname = 'person'
        ORDER BY a.attnum
        """,
    ).rows
    assert [tuple(r) for r in rows] == [("name", "text"), ("home", "addr")]
