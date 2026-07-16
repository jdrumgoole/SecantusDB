"""Unit tests for the per-row scalar expression evaluator (secantus.sql.scalar).

Pins the semantics the catalog-reflection queries rely on: catalog functions,
CASE, comparisons with NULL, and correlated scalar subqueries over (empty)
virtual catalog tables.
"""

from __future__ import annotations

import pytest
import sqlglot

from secantus.paths import get_path
from secantus.sql import scalar
from secantus.sql.catalog import Catalog
from secantus.storage import Storage


def _expr(sql: str):
    return sqlglot.parse_one(f"SELECT {sql}", read="postgres").expressions[0]


@pytest.fixture
def ctx(tmp_path):
    st = Storage(str(tmp_path))
    try:
        yield scalar.ScalarContext(storage=st, catalog=Catalog(st), db="db", session=None)
    finally:
        st.close()


def _scope(row: dict):
    def resolve(node):
        alias = node.table or None
        key = f"{alias}.{node.name}" if alias else node.name
        return get_path(row, key)

    return resolve


def test_format_type_maps_oids(ctx):
    row = {"a": {"t": 20, "m": -1}}
    assert scalar.evaluate(_expr("format_type(a.t, a.m)"), _scope(row), ctx) == "bigint"
    row = {"a": {"t": 25, "m": -1}}
    assert scalar.evaluate(_expr("format_type(a.t, a.m)"), _scope(row), ctx) == "text"
    row = {"a": {"t": 1700, "m": -1}}
    assert scalar.evaluate(_expr("format_type(a.t, a.m)"), _scope(row), ctx) == "numeric"


def test_case_with_else(ctx):
    assert scalar.evaluate(_expr("CASE WHEN 1 = 1 THEN 'y' ELSE 'n' END"), _scope({}), ctx) == "y"
    assert scalar.evaluate(_expr("CASE WHEN 1 = 2 THEN 'y' ELSE 'n' END"), _scope({}), ctx) == "n"
    # No matching branch and no ELSE -> NULL.
    assert scalar.evaluate(_expr("CASE WHEN 1 = 2 THEN 'y' END"), _scope({}), ctx) is None


def test_comparison_with_null_is_unknown(ctx):
    row = {"x": None}
    # x = 1 where x is NULL -> NULL (unknown), which is falsy in CASE.
    assert scalar.evaluate(_expr("CASE WHEN x = 1 THEN 'y' ELSE 'n' END"), _scope(row), ctx) == "n"


def test_boolean_column_and_not(ctx):
    row = {"a": {"flag": True}}
    assert scalar.evaluate(_expr("NOT a.flag"), _scope(row), ctx) is False
    row = {"a": {"flag": False}}
    assert scalar.evaluate(_expr("NOT a.flag"), _scope(row), ctx) is True


def test_coalesce(ctx):
    assert scalar.evaluate(_expr("coalesce(NULL, NULL, 3)"), _scope({}), ctx) == 3


def test_correlated_subquery_over_empty_catalog_is_null(ctx):
    # pg_attrdef is an (empty) virtual table; a correlated lookup returns NULL.
    row = {"pg_attribute": {"attrelid": 16384, "attnum": 1}}
    expr = _expr(
        "(SELECT d.adbin FROM pg_catalog.pg_attrdef d "
        "WHERE d.adrelid = pg_attribute.attrelid AND d.adnum = pg_attribute.attnum)"
    )
    assert scalar.evaluate(expr, _scope(row), ctx) is None


def test_unsupported_function_raises(ctx):
    from secantus.sql import errors

    with pytest.raises(errors.SQLError):
        scalar.evaluate(_expr("no_such_fn(1, 2)"), _scope({}), ctx)
