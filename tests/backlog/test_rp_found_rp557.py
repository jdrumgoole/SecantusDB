"""Gap tests of the divergences found while working RP-557 (a domain-typed
result column goes out with its base type's modifier): RP-659 (`min` /
`max` over `char(n)`), RP-660 (a set operation of a domain and its base
type) and RP-661 (`CREATE TABLE AS` over a domain column).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
Each was already wrong before RP-557's change.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t557, c557 cascade",
    "drop domain if exists d557 cascade",
    "create domain d557 as char(3)",
    "create table t557 (c char(3), a d557)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:2]:
            c.execute(q)


def _connect(dsn: str) -> psycopg.Connection:
    c = psycopg.connect(dsn, autocommit=True)
    for q in _SETUP:
        c.execute(q)
    return c


def _typmods(c: psycopg.Connection, query: str) -> list[int]:
    cur = c.execute(query)
    assert cur.pgresult is not None
    return [cur.pgresult.fmod(i) for i in range(cur.pgresult.nfields)]


# An Aggref carries no modifier (`exprTypmod` has no case for it), so
# `min` / `max` over a `char(3)` column, or a domain over it, describe -1.
@pytest.mark.backlog("RP-659")
@pytest.mark.parametrize("query", ["select min(c) from t557", "select max(a) from t557"])
def test_min_max_over_char_n_has_no_modifier(query: str, dsn: str) -> None:
    with _connect(dsn) as c:
        assert _typmods(c, query) == [-1]


# `select_common_type` smashes a domain to its base type when the other
# side is of the base type, and `select_common_typmod` then finds the
# sides' modifiers differ (the domain column's is -1), so the column
# describes -1. Two sides of the domain, or of `char(3)`, keep 7.
SET_OP_TYPMOD = {
    "select a from t557 union select c from t557": -1,
    "select c from t557 union select a from t557": -1,
    "select a from t557 union select a from t557": 7,
    "select c from t557 union select c from t557": 7,
}


@pytest.mark.backlog_fixed("RP-660")
@pytest.mark.parametrize("query", list(SET_OP_TYPMOD))
def test_set_op_of_a_domain_and_its_base_type(query: str, dsn: str) -> None:
    with _connect(dsn) as c:
        assert _typmods(c, query) == [SET_OP_TYPMOD[query]]


# `CREATE TABLE AS` copies the query's tuple descriptor (`intorel_startup`):
# a domain column stays of the domain, with the column's own modifier -1.
# Here the columns were read back from the wire description, which sees
# through the domain, so `a` was `character` with 7 (RP-661).
@pytest.mark.backlog_fixed("RP-661")
def test_create_table_as_keeps_a_domain_column(dsn: str) -> None:
    with _connect(dsn) as c:
        c.execute("create table c557 as select a, c from t557")
        got = c.execute(
            "select attname::text, atttypid::regtype::text, atttypmod"
            " from pg_attribute where attrelid = 'c557'::regclass and attnum > 0"
            " order by attnum"
        ).fetchall()
    assert got == [("a", "d557", -1), ("c", "character", 7)]


_ATTS = (
    "select attname::text, atttypid::regtype::text, atttypmod from pg_attribute"
    " where attrelid = '{}'::regclass and attnum > 0 order by attnum"
)


# Every form of CREATE TABLE AS keeps the domain: WITH NO DATA, SELECT ...
# INTO, a WITH query; a cast to the base type does not. A domain takes no
# modifier, so `pg_attribute.atttypmod` of ANY column of a domain is -1
# (RP-558). Measured on PostgreSQL 15.19.
CTAS_FORMS = {
    "create table c557 as select a from t557 with no data": [("a", "d557", -1)],
    "select a into c557 from t557": [("a", "d557", -1)],
    "create table c557 as with q as (select a from t557) select * from q": [("a", "d557", -1)],
    "create table c557 as select a::bpchar from t557": [("a", "character", -1)],
}


@pytest.mark.backlog_fixed("RP-661")
@pytest.mark.parametrize("statement", list(CTAS_FORMS))
def test_create_table_as_forms_keep_a_domain_column(statement: str, dsn: str) -> None:
    with _connect(dsn) as c:
        c.execute(statement)
        got = c.execute(_ATTS.format("c557")).fetchall()
    assert got == CTAS_FORMS[statement]


# `intorel_receive` coerces nothing, so a value of the domain is copied
# without its CHECKs: a row a `NOT VALID` CHECK would refuse is copied, as
# INSERT ... SELECT copies it (RP-664). Measured on PostgreSQL 15.19.
@pytest.mark.backlog_fixed("RP-661")
def test_create_table_as_does_not_recheck_a_domain_value(dsn: str) -> None:
    with _connect(dsn) as c:
        for q in [
            "drop table if exists tp661, cp661",
            "drop domain if exists dp661",
            "create domain dp661 as int",
            "create table tp661 (a dp661, b int)",
            "insert into tp661 values (0, 1), (5, 2)",
            "alter domain dp661 add constraint pos check (value > 0) not valid",
            "create table cp661 as select a, b from tp661",
        ]:
            c.execute(q)
        got = c.execute("select a, b from cp661 order by b").fetchall()
        typ = c.execute(_ATTS.format("cp661")).fetchall()
        c.execute("drop table tp661, cp661")
        c.execute("drop domain dp661")
    assert (got, typ) == ([(0, 1), (5, 2)], [("a", "dp661", -1), ("b", "integer", -1)])
