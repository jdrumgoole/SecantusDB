"""Gap test of the divergence found while working RP-824 (a column retyped
off its domain is no longer one of it): a column retyped INTO a domain with
USING is not one of the domain either.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which the test runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t824",
    "drop domain if exists dx824 cascade",
    "create domain dx824 as int check (value > 0) default 42",
]
_TEARDOWN = 2


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:_TEARDOWN]:
            c.execute(q)


def _answer(c: psycopg.Connection, sql: str) -> object:
    try:
        cur = c.execute(sql)
        return cur.fetchall() if cur.description else cur.statusmessage
    except psycopg.Error as e:
        return f"{e.sqlstate} {e.diag.message_primary}"


# `ATExecAlterColumnType` sets `atttypid` to the domain, so the column is one
# of it as CREATE TABLE declares one: the domain's DEFAULT fills an INSERT
# that names no value for it, `information_schema` reads the domain's base
# as `data_type` and names the domain, and the domain cannot be dropped.
# Here the column is stored under the domain's name with no domain tag, so
# its DEFAULT is NULL, `data_type` is `USER-DEFINED`, and the domain whose
# column was retyped from int names no domain at all.
@pytest.mark.backlog("RP-834")
def test_a_column_retyped_into_a_domain_is_one_of_it(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        got = [
            _answer(c, sql)
            for sql in [
                "create table t824 (k int, a int, b dx824)",
                "insert into t824 values (1, 1, 1)",
                "alter table t824 alter column a type dx824 using a::dx824",
                "alter table t824 alter column b type dx824 using b + 1",
                "insert into t824 (k) values (2)",
                "insert into t824 values (3, -1, 1)",
                "select k, a, b from t824 order by k",
                "select column_name, data_type, domain_name"
                " from information_schema.columns"
                " where table_name = 't824' order by ordinal_position",
                "select attname, atttypid::regtype::text from pg_attribute"
                " where attrelid = 't824'::regclass and attnum > 0 order by attnum",
                "drop domain dx824",
                "drop table t824",
            ]
        ]
    assert got == [
        "CREATE TABLE",
        "INSERT 0 1",
        "ALTER TABLE",
        "ALTER TABLE",
        "INSERT 0 1",
        '23514 value for domain dx824 violates check constraint "dx824_check"',
        [(1, 1, 2), (2, 42, 42)],
        [("k", "integer", None), ("a", "integer", "dx824"), ("b", "integer", "dx824")],
        [("k", "integer"), ("a", "dx824"), ("b", "dx824")],
        "2BP01 cannot drop type dx824 because other objects depend on it",
        "DROP TABLE",
    ]
