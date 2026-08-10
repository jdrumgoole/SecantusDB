"""``numeric / numeric`` carries PostgreSQL's derived result scale.

``select_div_scale`` (numeric.c) is ported into ``typemap.numeric_div``: the
quotient's scale comes from the operands' leading base-10000 digits and
display scales — ``5.52 / 2.4`` is ``2.3000000000000000`` (scale 16), not
``2.3``. Every expectation below is the byte-exact text a live PostgreSQL
14.13 returned for the same expression (probed 2026-08-10); a driver reading
``getBigDecimal().scale()`` now sees what it would see on real Postgres.
Driven over the real ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.typemap import to_pg_text
from secantus.storage import Storage

DB = "app"

# (expression, byte-exact PostgreSQL 14.13 text render)
CASES = [
    ("SELECT 5.52 / 2.4", "2.3000000000000000"),
    ("SELECT 5.52 / CAST(2.4 AS NUMERIC(10,2))", "2.3000000000000000"),
    ("SELECT 1::numeric / 3::numeric", "0.33333333333333333333"),
    ("SELECT 10::numeric / 4::numeric", "2.5000000000000000"),
    ("SELECT 100.5 / 2::numeric", "50.2500000000000000"),
    ("SELECT 1::numeric / 300000::numeric", "0.000003333333333333333333"),
    ("SELECT 123456789.123 / 0.001", "123456789123.00000000"),
    ("SELECT 0.001 / 123456789.123", "0.0000000000081000000656399705"),
    ("SELECT 2::numeric / 1::numeric", "2.0000000000000000"),
    ("SELECT 2.00 / 1.00", "2.0000000000000000"),
    ("SELECT 10000000000::numeric / 3::numeric", "3333333333.33333333"),
    ("SELECT 1::numeric / 10000000000::numeric", "0.0000000001000000000000000000"),
    ("SELECT 0.5 / 0.25", "2.0000000000000000"),
    ("SELECT -7.7 / 2.2", "-3.5000000000000000"),
    ("SELECT 3.14159 / 2.71828", "1.1557271509925394"),
    ("SELECT 12345678901234567890::numeric / 7::numeric", "1763668414462081127"),
    (
        "SELECT 1::numeric / 12345678901234567890::numeric",
        "0.000000000000000000081000000729000007",
    ),
    ("SELECT 0::numeric / 5.5", "0.00000000000000000000"),
    ("SELECT 0.000001 / 2::numeric", "0.000000500000000000000000"),
    ("SELECT 99999.99999 / 0.00001", "9999999999.00000000"),
]


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


@pytest.mark.parametrize(("sql", "want"), CASES, ids=[c[0][7:] for c in CASES])
def test_division_scale_matches_postgres(storage, sql, want):
    got = to_pg_text(run_sql(storage, DB, sql)[-1].rows[0][0])
    if isinstance(got, bytes):
        got = got.decode()
    assert got == want


def test_int_division_still_truncates(storage):
    # int / int stays PG integer division — untouched by the numeric path.
    assert run_sql(storage, DB, "SELECT 15 / 10")[-1].rows == [(1,)]


def test_float_division_stays_float(storage):
    # float8 with numeric coerces to float8 in PG — no scale derivation.
    (val,) = run_sql(storage, DB, "SELECT 5.52::float8 / 2.4")[-1].rows[0]
    assert isinstance(val, float)
