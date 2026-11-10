"""Regression tests of the divergence found while working RP-661 (`CREATE
TABLE AS` keeps a domain column): RP-769, a domain over a collatable type
had no collation here, so a view of its column was refused.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop view if exists v769",
    "drop materialized view if exists m769",
    "drop table if exists t769 cascade",
    "drop table if exists t769c",
    "drop domain if exists d769 cascade",
    "drop domain if exists d769dd cascade",
    "drop domain if exists d769t cascade",
    "drop domain if exists d769n cascade",
    "drop domain if exists d769i cascade",
    "create domain d769 as text",
    "create table t769 (a d769, b text)",
]
_TEARDOWN = 9


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:_TEARDOWN]:
            c.execute(q)


def _connect(dsn: str) -> psycopg.Connection:
    c = psycopg.connect(dsn, autocommit=True)
    for q in _SETUP:
        c.execute(q)
    return c


def _status(c: psycopg.Connection, sql: str) -> str:
    try:
        return c.execute(sql).statusmessage or ""
    except psycopg.Error as e:
        return f"{e.sqlstate} {e}"


# `DefineDomain` copies the base type's collation, so a view's column of
# the domain is collatable AND has one (`CheckAttributeType` passes).
@pytest.mark.backlog_fixed("RP-769")
def test_a_view_of_a_text_domain_column(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _status(c, "create view v769 as select a from t769"),
            _status(c, "create materialized view m769 as select a from t769"),
        ]
    assert got == ["CREATE VIEW", "SELECT 0"]


@pytest.mark.backlog_fixed("RP-769")
def test_a_text_domain_has_the_default_collation(dsn: str) -> None:
    with _connect(dsn) as c:
        atts = c.execute(
            "select attname::text, attcollation::int from pg_attribute"
            " where attrelid = 't769'::regclass and attnum > 0 order by attnum"
        ).fetchall()
        typ = c.execute("select typcollation::int from pg_type where typname = 'd769'").fetchall()
    assert (atts, typ) == ([("a", 100), ("b", 100)], [(100,)])


# `pg_collation_for` of a domain column is its base type's collation,
# through a domain over a domain and the domain's array type; a domain
# over a type with none is 42804 naming the domain (15.19).
@pytest.mark.backlog_fixed("RP-769")
def test_collation_for_a_domain_column(dsn: str) -> None:
    with _connect(dsn) as c:
        for q in [
            "create domain d769t as text",
            "create domain d769dd as d769t",
            "create domain d769n as name",
            "create domain d769i as int",
            "create table t769c (t d769t, dd d769dd, n d769n, i d769i, ta d769t[])",
            "insert into t769c values ('a', 'b', 'c', 1, '{x}')",
        ]:
            c.execute(q)
        got = c.execute(
            "select collation for (t), collation for (dd), collation for (n),"
            " collation for (ta) from t769c"
        ).fetchall()
        try:
            c.execute("select collation for (i) from t769c")
            err = ""
        except psycopg.Error as e:
            err = f"{e.sqlstate} {e.diag.message_primary}"
    assert got == [('"default"', '"default"', '"C"', '"default"')]
    assert err == "42804 collations are not supported by type d769i"
