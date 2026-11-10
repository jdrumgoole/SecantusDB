"""Gap tests of the divergence found while working RP-661 (`CREATE TABLE
AS` keeps a domain column): RP-769, a domain over a collatable type has
no collation here, so a view of its column is refused.

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
    "drop domain if exists d769 cascade",
    "create domain d769 as text",
    "create table t769 (a d769, b text)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:4]:
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
@pytest.mark.backlog("RP-769")
def test_a_view_of_a_text_domain_column(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _status(c, "create view v769 as select a from t769"),
            _status(c, "create materialized view m769 as select a from t769"),
        ]
    assert got == ["CREATE VIEW", "SELECT 0"]


@pytest.mark.backlog("RP-769")
def test_a_text_domain_has_the_default_collation(dsn: str) -> None:
    with _connect(dsn) as c:
        atts = c.execute(
            "select attname::text, attcollation::int from pg_attribute"
            " where attrelid = 't769'::regclass and attnum > 0 order by attnum"
        ).fetchall()
        typ = c.execute("select typcollation::int from pg_type where typname = 'd769'").fetchall()
    assert (atts, typ) == ([("a", 100), ("b", 100)], [(100,)])
