"""Composite types: ``CREATE TYPE t AS (a int, b text)`` — a composite-typed
column stores a subdocument, ``ROW(…)`` writes it, ``(col).field`` reads a field,
and the whole value renders as a Postgres record literal ``(a,b)``. Reflected via
pg_type (``typtype = 'c'``).
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


def test_composite_column_atttypid_and_array_typename_collision(storage, session):
    # pgjdbc's customArrayTypeInfo: composite columns report their minted oid
    # (not RECORD), and a composite array type name avoids collision with an
    # existing type of that name — custom[]'s type is __custom because the
    # composite _custom already claims _custom.
    run(storage, session, "CREATE TYPE custom AS (i int)")
    run(storage, session, "CREATE TYPE _custom AS (f float)")
    run(
        storage,
        session,
        "CREATE TABLE customtable (c1 custom, c2 _custom, c3 custom[], c4 _custom[])",
    )
    rows = run(
        storage,
        session,
        "SELECT a.attname, t.typname FROM pg_attribute a"
        " JOIN pg_class c ON a.attrelid = c.oid"
        " JOIN pg_type t ON a.atttypid = t.oid"
        " WHERE c.relname = 'customtable' AND a.attnum > 0 ORDER BY a.attnum",
    ).rows
    assert rows == [
        ("c1", "custom"),
        ("c2", "_custom"),
        ("c3", "__custom"),
        ("c4", "___custom"),
    ]


def test_scalar_array_type_names_unaffected(storage, session):
    run(storage, session, "CREATE TABLE it (a int[], t text[])")
    rows = run(
        storage,
        session,
        "SELECT a.attname, ty.typname FROM pg_attribute a"
        " JOIN pg_class c ON a.attrelid = c.oid"
        " JOIN pg_type ty ON a.atttypid = ty.oid"
        " WHERE c.relname = 'it' AND a.attnum > 0 ORDER BY a.attnum",
    ).rows
    assert rows == [("a", "_int4"), ("t", "_text")]


# --------------------------------------------------------------------------- #
# Anonymous record constructor: ``(a, b, …)`` (pgtest tuple)
# --------------------------------------------------------------------------- #


def test_parenthesized_tuple_is_a_record(storage, session):
    # ``(a, b, …)`` is the anonymous record constructor, like ``ROW(a, b, …)``.
    res = run(storage, session, "SELECT (1::int2, 2::int4, 3::int8, null) AS row")
    assert res.columns[0].pg_oid == 2249  # RECORD
    assert res.columns[0].name == "row"
    assert res.rows[0][0] == {"f1": 1, "f2": 2, "f3": 3, "f4": None}


def test_record_text_render_quotes_and_pads(storage, session):
    val = run(
        storage,
        session,
        "SELECT ('a'::text, 'd'::char(2), 'f'::char(3)) AS row",
    ).rows[0][0]
    # A char(n) field blank-pads; the record text quotes fields with spaces.
    from secantus.sql import typemap as _tm

    assert _tm.to_pg_text(val, "composite").decode() == '(a,"d ","f  ")'


def test_binary_composite_param_errors():
    # The validating binary-composite decoder raises PG's exact wire errors
    # (pgtest tuple corpus pins them via keepErrMessage).
    from secantus.sql import errors
    from secantus.sql.pgextended import _decode_binary_composite

    fields = [("a", "bool", None)]  # CREATE TYPE r AS (a bool)

    def code(raw):
        try:
            _decode_binary_composite(bytes.fromhex(raw), fields)
            return None
        except errors.SQLError as e:
            return e.sqlstate

    assert _decode_binary_composite(bytes.fromhex("00000001000000100000000101"), fields) == "(t)"
    assert _decode_binary_composite(bytes.fromhex("0000000100000010FFFFFFFF"), fields) == "()"
    assert code("") == "08P01"  # no header
    assert code("FFFFFFFF") == "42804"  # wrong column count
    assert code("00000001") == "08P01"  # no element oid
    assert code("0000000100000000") == "42804"  # oid mismatch
    assert code("0000000100000010") == "08P01"  # no element size
    assert code("000000010000001000000000") == "08P01"  # 0-length bool
    assert code("000000010000001000000001") == "22P03"  # length exceeds buffer
