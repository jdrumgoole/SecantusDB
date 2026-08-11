"""UDF reflection (#130): CREATE FUNCTION surfaced via pg_catalog.pg_proc,
pg_get_functiondef / pg_get_function_arguments / pg_get_function_result, and
information_schema.routines / parameters — so psql's \\df and SQLAlchemy see
user functions. Driven over the real ``Storage``.
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
    sess = Session(database=DB)
    run_sql(
        s,
        DB,
        "CREATE FUNCTION add(a int, b int) RETURNS int AS $$ SELECT a + b $$ LANGUAGE sql",
        session=sess,
    )
    run_sql(
        s,
        DB,
        "CREATE FUNCTION greet(name text) RETURNS text AS $$ SELECT 'hi ' || name $$ LANGUAGE sql",
        session=sess,
    )
    try:
        yield s
    finally:
        s.close()


def _rows(storage, sql):
    return run_sql(storage, DB, sql, session=Session(database=DB))[-1].rows


def test_pg_proc_rows(storage):
    rows = _rows(
        storage,
        "SELECT proname, pronargs, proargtypes, proargnames, prorettype, prokind "
        "FROM pg_catalog.pg_proc WHERE oid >= 16384 ORDER BY proname",
    )
    assert rows == [
        ("add", 2, "23 23", ["a", "b"], 23, "f"),
        ("greet", 1, "25", ["name"], 25, "f"),
    ]


def test_information_schema_routines(storage):
    rows = _rows(
        storage,
        "SELECT routine_name, routine_type, data_type, external_language "
        "FROM information_schema.routines ORDER BY routine_name",
    )
    assert rows == [
        ("add", "FUNCTION", "integer", "SQL"),
        ("greet", "FUNCTION", "text", "SQL"),
    ]


def test_information_schema_parameters(storage):
    rows = _rows(
        storage,
        "SELECT ordinal_position, parameter_name, parameter_mode, data_type "
        "FROM information_schema.parameters ORDER BY specific_name, ordinal_position",
    )
    assert rows == [
        (1, "a", "IN", "integer"),
        (2, "b", "IN", "integer"),
        (1, "name", "IN", "text"),
    ]


def test_pg_get_function_arguments_and_result(storage):
    rows = _rows(
        storage,
        "SELECT proname, pg_get_function_arguments(oid), pg_get_function_result(oid) "
        "FROM pg_catalog.pg_proc WHERE oid >= 16384 ORDER BY proname",
    )
    assert rows == [
        ("add", "a integer, b integer", "integer"),
        ("greet", "name text", "text"),
    ]


def test_pg_get_functiondef(storage):
    oid = _rows(storage, "SELECT oid FROM pg_catalog.pg_proc WHERE proname = 'add'")[0][0]
    body = _rows(storage, f"SELECT pg_get_functiondef({oid})")[0][0]
    assert "CREATE OR REPLACE FUNCTION public.add(a integer, b integer)" in body
    assert "RETURNS integer" in body
    assert "LANGUAGE sql" in body
    assert "SELECT a + b" in body


def test_dropped_function_leaves_pg_proc(storage):
    run_sql(storage, DB, "DROP FUNCTION greet(text)", session=Session(database=DB))
    assert _rows(
        storage, "SELECT proname FROM pg_catalog.pg_proc WHERE oid >= 16384 ORDER BY proname"
    ) == [("add",)]
