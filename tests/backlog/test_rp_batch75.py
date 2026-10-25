"""Batch 75 of the Rust PG server (2026-10-25): regression tests of what it
found and fixed beside RP-061's serializable snapshot isolation -- RP-258
(an SQL function's void result is NULL) and RP-259 (RAISE's condition and
options). Each scenario runs in a transaction that is rolled back, so the
answers come from the shared PostgreSQL 15 reference as well:

    PYTHONPATH=. python tests/backlog/test_rp_batch75.py "host=127.0.0.1 port=5415 user=postgres"

prints `EXPECTED`, measured on PostgreSQL 15.19.
"""

from __future__ import annotations

import sys
from typing import Any

import psycopg
import pytest

from tests.backlog.conftest import pg_answer

RAISE_FORMS = [
    "RAISE SQLSTATE '22012'",
    "RAISE SQLSTATE 'P1234' USING MESSAGE = 'x'",
    "RAISE division_by_zero USING MESSAGE = 'y'",
    "RAISE USING ERRCODE = 'division_by_zero'",
    "RAISE USING ERRCODE = '22012'",
    "RAISE division_by_zero USING ERRCODE = '22012'",
    "RAISE SQLSTATE '22012' USING ERRCODE = '22012'",
    "RAISE 'fmt %', 1 USING MESSAGE = 'z'",
    "RAISE USING HINT = 'h'",
    "RAISE USING HINT = 'h', HINT = 'i'",
    "RAISE EXCEPTION 'a' USING DETAIL = 'd', DETAIL = 'e'",
]


def void_results(conn: Any) -> list[Any]:
    conn.execute("BEGIN")
    try:
        conn.execute("CREATE FUNCTION vf() RETURNS void LANGUAGE sql AS 'SELECT 1'")
        conn.execute("CREATE FUNCTION vp() RETURNS void LANGUAGE plpgsql AS 'BEGIN END'")
        return [pg_answer(conn, "SELECT vf() IS NULL, vp() IS NULL, vf()::text, vp()::text")]
    finally:
        conn.execute("ROLLBACK")


def raise_forms(conn: Any) -> list[Any]:
    out = []
    for form in RAISE_FORMS:
        out.append(pg_answer(conn, f"DO $$ BEGIN {form}; END $$"))
        conn.execute("BEGIN")
        try:
            conn.execute(
                f"CREATE FUNCTION rz() RETURNS void LANGUAGE plpgsql AS $$ BEGIN {form}; END $$"
            )
            out.append(pg_answer(conn, "SELECT rz()"))
        finally:
            conn.execute("ROLLBACK")
    return out


EXPECTED: dict[str, Any] = {
    "void_results": [("ok", [(True, False, None, "")])],
    "raise_forms": [
        ("err", "22012", "22012"),
        ("err", "22012", "22012"),
        ("err", "P1234", "x"),
        ("err", "P1234", "x"),
        ("err", "22012", "y"),
        ("err", "22012", "y"),
        ("err", "22012", "division_by_zero"),
        ("err", "22012", "division_by_zero"),
        ("err", "22012", "22012"),
        ("err", "22012", "22012"),
        ("err", "42601", "RAISE option already specified: ERRCODE"),
        ("err", "42601", "RAISE option already specified: ERRCODE"),
        ("err", "42601", "RAISE option already specified: ERRCODE"),
        ("err", "42601", "RAISE option already specified: ERRCODE"),
        ("err", "42601", "RAISE option already specified: MESSAGE"),
        ("err", "42601", "RAISE option already specified: MESSAGE"),
        ("err", "P0001", "P0001"),
        ("err", "P0001", "P0001"),
        ("err", "42601", "RAISE option already specified: HINT"),
        ("err", "42601", "RAISE option already specified: HINT"),
        ("err", "42601", "RAISE option already specified: DETAIL"),
        ("err", "42601", "RAISE option already specified: DETAIL"),
    ],
}


def _check(name: str, rust_pg: Any) -> None:
    with rust_pg.connect() as conn:
        got = globals()[name](conn)
    assert got == EXPECTED[name]


@pytest.mark.backlog_fixed("RP-258")
def test_sql_void_function_is_null(rust_pg: Any) -> None:
    """PostgreSQL 15.19: a void SQL function answers NULL, a void PL/pgSQL
    one the empty string."""
    _check("void_results", rust_pg)


@pytest.mark.backlog_fixed("RP-259")
def test_raise_condition_and_options(rust_pg: Any) -> None:
    """PostgreSQL 15.19: `RAISE SQLSTATE` takes a MESSAGE option, a
    condition's name is the message only when nothing else is, and each of
    ERRCODE, MESSAGE, DETAIL and HINT may be given once (42601), in a DO
    block and in a function alike."""
    _check("raise_forms", rust_pg)


if __name__ == "__main__":
    with psycopg.connect(sys.argv[1], autocommit=True) as c:
        print("EXPECTED: dict[str, Any] = {")
        for n in ("void_results", "raise_forms"):
            print(f"    {n!r}: {globals()[n](c)!r},")
        print("}")
