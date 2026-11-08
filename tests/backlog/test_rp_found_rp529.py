"""Gap tests of the divergences found while working RP-529 (a domain
column's whole row is typed by the domain): RP-551 (a cast to a domain
over `char(n)` was not blank-padded; fixed), RP-552 (a domain's CHECK error inside a
record literal carries no position), RP-553 (a schema's domain names its
CHECK after the qualified name), RP-554 (a scalar subquery's array of a row
type was described as `text[]`; fixed), RP-555 (`ALTER DOMAIN ... NOT VALID`
validated the rows; fixed) and RP-556 (`ALTER COLUMN TYPE` to a domain over the
column's type is refused).

An answer is the rows, the command tag for a statement that returns none,
or `(sqlstate, message, hint, position)` for an error. The expected answers
were measured on a private, freshly initdb'd PostgreSQL 15.19 cluster with
`_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator
from decimal import Decimal
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop type if exists ctp",
    "drop table if exists fc, fcn, fnv, falt, fs cascade",
    "drop domain if exists dc3n, dc3, dnv, pint, dv3, dn52n, dn52 cascade",
    "drop schema if exists s cascade",
    "create domain dc3 as char(3)",
    "create domain dc3n as dc3",
    "create domain dnv as int",
    "create domain pint as int check (value > 0)",
    "create domain dv3 as varchar(3) check (value <> 'abcd')",
    "create domain dn52 as numeric(5,2)",
    "create domain dn52n as dn52",
    "create schema s",
    "create domain s.sd as int check (value < 100)",
    "create table fc (a dc3, b int)",
    "create table fcn (a dc3n)",
    "create table fnv (a dnv)",
    "create table falt (a int)",
    "create table fs (a int, c text)",
    "insert into fc values ('ab', 1)",
    "insert into fcn values ('ab')",
    "insert into fnv values (1)",
    "insert into falt values (3)",
    "insert into fs values (1, 'y')",
    "create type ctp as (a pint, b text)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:4]:
            c.execute(q)


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_hint,
                e.diag.statement_position,
            )
        return cur.fetchall() if cur.description else cur.statusmessage


# A cast to a domain over `char(3)` is a cast to `char(3)` first: the value
# is blank-padded to the width, through a domain over the domain too, so the
# padded column's whole row equals a record of the cast.
CHAR_DOMAIN = {
    "select octet_length('ab'::dc3)": [(3,)],
    "select 'ab'::dc3": [("ab ",)],
    "select octet_length('ab'::dc3n)": [(3,)],
    "select octet_length(a) from fcn": [(3,)],
    "select fcn from fcn": [('("ab ")',)],
    "select fc = row('ab '::dc3, 1) from fc": [(True,)],
    # An explicit cast truncates to the width; a write is refused (22001).
    "select 'abcd'::dc3n": [("abc",)],
    "insert into fcn values ('abcd')": (
        "22001",
        "value too long for type character(3)",
        None,
        None,
    ),
    # The padded value is a char(3) to an operator: its blanks do not count.
    "select 'ab'::dc3 = 'ab'": [(True,)],
    "select ('ab'::dc3)::text || '|'": [("ab|",)],
}


@pytest.mark.backlog_fixed("RP-551")
@pytest.mark.parametrize("query", list(CHAR_DOMAIN))
def test_char_domain_cast_pads(query: str, dsn: str) -> None:
    assert answer(dsn, query) == CHAR_DOMAIN[query]


# Every modifier is the base's, as `coerce_to_domain` applies it before the
# domain's CHECK runs: `'abcd'::dv3` is `abc`, which passes `value <>
# 'abcd'`, and a numeric(5,2) rounds, through a domain over it too.
BASE_TYPMOD = {
    "select 'abcd'::dv3": [("abc",)],
    "select 1.234::dn52": [(Decimal("1.23"),)],
    "select 1.234::dn52n": [(Decimal("1.23"),)],
    "select 12345.6::dn52n": ("22003", "numeric field overflow", None, None),
}


@pytest.mark.backlog_fixed("RP-551")
@pytest.mark.parametrize("query", list(BASE_TYPMOD))
def test_domain_cast_applies_base_typmod(query: str, dsn: str) -> None:
    assert answer(dsn, query) == BASE_TYPMOD[query]


# A field's domain CHECK failing in a record literal is reported at the
# literal, as its other input errors are.
@pytest.mark.backlog("RP-552")
def test_record_literal_domain_error_position(dsn: str) -> None:
    assert answer(dsn, "select '(0,x)'::ctp") == (
        "23514",
        'value for domain pint violates check constraint "pint_check"',
        None,
        "8",
    )


# An unnamed CHECK of a domain in a schema is named after the domain's own
# name, not its qualified one.
SCHEMA_DOMAIN = {
    "select 200::s.sd": (
        "23514",
        'value for domain s.sd violates check constraint "sd_check"',
        None,
        None,
    ),
    "select conname from pg_constraint where contypid = 's.sd'::regtype": [("sd_check",)],
}


@pytest.mark.backlog("RP-553")
@pytest.mark.parametrize("query", list(SCHEMA_DOMAIN))
def test_schema_domain_check_name(query: str, dsn: str) -> None:
    assert answer(dsn, query) == SCHEMA_DOMAIN[query]


# A scalar subquery's `array_agg` of a whole row is an array of the row
# type, as the same aggregate outside a subquery is described here already.
# It was `text[]`, which psycopg read as `[None]`; so was an ARRAY(...) of a
# whole row, and a scalar subquery's whole row was `text` (RP-554).
@pytest.mark.backlog_fixed("RP-554")
@pytest.mark.parametrize(
    "query",
    [
        "select (select array_agg(x) from (select fs as x from fs) q)",
        "select (select array_agg(fs) from fs)",
        "select (select array[fs] from fs)",
        "select array(select fs from fs)",
    ],
)
def test_scalar_subquery_row_type_array(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        fs_array = c.execute("select 'fs[]'::regtype::oid").fetchone()[0]
        cur = c.execute(query)
        assert [d.type_code for d in cur.description] == [fs_array]
        assert cur.fetchall() == [('{"(1,y)"}',)]
    assert answer(dsn, f"select pg_typeof(({query[7:]}))") == [("fs[]",)]


# A scalar subquery of a whole row is of the row type, and a subquery that
# finds no row is a NULL of it, or an empty array of it (RP-554).
@pytest.mark.backlog_fixed("RP-554")
@pytest.mark.parametrize(
    ("query", "type_name", "rows"),
    [
        ("select (select fs from fs)", "fs", [("(1,y)",)]),
        ("select (select fs from fs where false)", "fs", [(None,)]),
        ("select (select array_agg(fs) from fs where false)", "fs[]", [(None,)]),
        ("select array(select fs from fs where false)", "fs[]", [("{}",)]),
    ],
)
def test_scalar_subquery_row_type(
    query: str, type_name: str, rows: list[tuple[Any, ...]], dsn: str
) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        oid = c.execute(f"select '{type_name}'::regtype::oid").fetchone()[0]
        cur = c.execute(query)
        assert [d.type_code for d in cur.description] == [oid]
        assert cur.fetchall() == rows
    assert answer(dsn, f"select pg_typeof(({query[7:]}))") == [(type_name,)]


def _then(dsn: str, statement: str, query: str) -> Any:
    """`statement`'s answer after `_SETUP`, then `query`'s on the same
    connection when the statement succeeded."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            tag = c.execute(statement).statusmessage
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary)
        return tag, c.execute(query).fetchall()


# NOT VALID added the constraint without checking the rows already there.
@pytest.mark.backlog_fixed("RP-555")
def test_domain_constraint_not_valid(dsn: str) -> None:
    assert _then(
        dsn,
        "alter domain dnv add constraint big check (value > 5) not valid",
        "select convalidated from pg_constraint where conname = 'big'",
    ) == ("ALTER DOMAIN", [(False,)])


_BIG = (
    "select convalidated, pg_get_constraintdef(oid),"
    " pg_get_constraintdef(oid, true) from pg_constraint where conname = 'big'"
)

# A NOT VALID constraint still checks every new value; VALIDATE CONSTRAINT
# checks the stored ones (`validateDomainConstraint`), then marks it
# validated, and its definition loses the NOT VALID. `fnv` holds 1.
NOT_VALID_THEN_VALIDATE = [
    ("alter domain dnv add constraint big check (value > 5) not valid", "ALTER DOMAIN"),
    (
        _BIG,
        [(False, "CHECK ((VALUE > 5)) NOT VALID", "CHECK (VALUE > 5) NOT VALID")],
    ),
    (
        "insert into fnv values (2)",
        ("23514", 'value for domain dnv violates check constraint "big"'),
    ),
    ("insert into fnv values (7)", "INSERT 0 1"),
    (
        "alter domain dnv validate constraint big",
        (
            "23514",
            'column "a" of table "fnv" contains values that violate the new constraint',
        ),
    ),
    (
        "alter domain dnv validate constraint nosuch",
        ("42704", 'constraint "nosuch" of domain "dnv" does not exist'),
    ),
    ("delete from fnv where a < 5", "DELETE 1"),
    ("alter domain dnv validate constraint big", "ALTER DOMAIN"),
    (_BIG, [(True, "CHECK ((VALUE > 5))", "CHECK (VALUE > 5)")]),
    # Validating a validated constraint does nothing.
    ("alter domain dnv validate constraint big", "ALTER DOMAIN"),
    ("alter domain dnv drop constraint big", "ALTER DOMAIN"),
    ("select count(*) from pg_constraint where conname = 'big'", [(0,)]),
]


def _in_order(dsn: str, steps: list[str]) -> list[Any]:
    """Each statement's answer after `_SETUP`, in order on one connection."""
    out: list[Any] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        for q in steps:
            try:
                cur = c.execute(q)
            except psycopg.Error as e:
                out.append((e.sqlstate, e.diag.message_primary))
                continue
            out.append(cur.fetchall() if cur.description else cur.statusmessage)
    return out


@pytest.mark.backlog_fixed("RP-555")
def test_domain_constraint_not_valid_then_validate(dsn: str) -> None:
    steps = [q for q, _ in NOT_VALID_THEN_VALIDATE]
    assert _in_order(dsn, steps) == [a for _, a in NOT_VALID_THEN_VALIDATE]


# A column retyped to a domain over its own type needs no USING.
@pytest.mark.backlog("RP-556")
def test_alter_column_type_to_domain(dsn: str) -> None:
    assert _then(
        dsn,
        "alter table falt alter column a type pint",
        "select atttypid::regtype::text from pg_attribute"
        " where attrelid = 'falt'::regclass and attname = 'a'",
    ) == ("ALTER TABLE", [("pint",)])
