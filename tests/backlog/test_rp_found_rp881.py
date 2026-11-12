"""Gap tests of the DEFAULT divergences found while working RP-881: RP-886
(a constant DEFAULT was evaluated at CREATE TABLE; fixed), RP-887 (CREATE TABLE IF
NOT EXISTS over a table that exists checks its columns) and RP-888 (a
`regclass` literal in a DEFAULT is not resolved at CREATE).

An answer is the last statement's rows, `("rowcount", n)` for a statement
with none, or `(sqlstate, message, hint, position)` for an error, the
statements run in order on one fresh session. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn
from tests.backlog.test_rp_qualified_calls import answer

CREATED = ("rowcount", -1)


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


CLEANUP = "drop table if exists t890"
DROP_DOMAIN = "drop domain if exists d890"

CONSTANT_DEFAULT: dict[str, tuple[tuple[str, ...], Any]] = {
    "1/0": (("create table t890 (a int default 1/0)",), CREATED),
    "1/0, then an INSERT": (
        ("create table t890 (a int default 1/0)", "insert into t890 default values"),
        ("22012", "division by zero", None, None),
    ),
    "an overflow": (("create table t890 (a int default 2147483647 + 1)",), CREATED),
    "a cast of computed text": (
        ("create table t890 (a int default ('x' || 'y')::int)",),
        CREATED,
    ),
    "a literal cast through text": (
        ("create table t890 (a int default 'x'::text::int)",),
        CREATED,
    ),
    "a modifier's overflow": (
        ("create table t890 (a numeric default '123.4'::numeric(3,1))",),
        CREATED,
    ),
    "a narrowing to smallint": (("create table t890 (a int2 default 100000)",), CREATED),
    "a narrowing, then an INSERT": (
        ("create table t890 (a int2 default 100000)", "insert into t890 default values"),
        ("22003", "smallint out of range", None, None),
    ),
    "a domain's CHECK": (
        (
            "create domain d890 as int check (value > 0)",
            "create table t890 (a int default '-1'::d890)",
        ),
        CREATED,
    ),
    "SET DEFAULT": (
        ("create table t890 (a int)", "alter table t890 alter column a set default 1/0"),
        CREATED,
    ),
    "a domain's default": (
        ("create domain d890 as int default 1/0", "create table t890 (a d890)"),
        CREATED,
    ),
    "a domain's default, then an INSERT": (
        (
            "create domain d890 as int default 1/0",
            "create table t890 (a d890)",
            "insert into t890 default values",
        ),
        ("22012", "division by zero", None, None),
    ),
    # ADD COLUMN evaluates the default it keeps (`ATExecAddColumn`), over no
    # rows too, and only that one.
    "ADD COLUMN of 1/0": (
        ("create table t890 (a int)", "alter table t890 add column b int default 1/0"),
        ("22012", "division by zero", None, None),
    ),
    "ADD COLUMN of the domain": (
        (
            "create domain d890 as int default 1/0",
            "create table t890 (a int)",
            "alter table t890 add column b d890",
        ),
        ("22012", "division by zero", None, None),
    ),
    "ADD COLUMN of the domain with a default": (
        (
            "create domain d890 as int default 1/0",
            "create table t890 (a int)",
            "alter table t890 add column b d890 default 5",
        ),
        CREATED,
    ),
}


@pytest.mark.backlog_fixed("RP-886")
@pytest.mark.parametrize("case", list(CONSTANT_DEFAULT))
def test_a_constant_default_is_not_run_at_create(case: str, dsn: str) -> None:
    """A DEFAULT's errors came from the INSERT that ran it, not CREATE: only
    ADD COLUMN evaluates its default when it is declared."""
    statements, expected = CONSTANT_DEFAULT[case]
    try:
        assert answer(dsn, *statements) == expected
    finally:
        answer(dsn, CLEANUP)
        answer(dsn, DROP_DOMAIN)


_NOT_AN_INTEGER = ("22P02", 'invalid input syntax for type integer: "x"', None)

LITERAL_INPUT = {
    "create table t890 (a int default 'x')": (*_NOT_AN_INTEGER, "34"),
    "create table t890 (a int default 'x' + 1)": (*_NOT_AN_INTEGER, "34"),
    "create table t890 (a int default 1/0 + 'x'::int)": (*_NOT_AN_INTEGER, "40"),
    "create table t890 (a int[] default '{1,x}')": (*_NOT_AN_INTEGER, "36"),
}


@pytest.mark.backlog_fixed("RP-886")
@pytest.mark.parametrize("statement", list(LITERAL_INPUT))
def test_a_default_literal_input_error_stands_at_create(statement: str, dsn: str) -> None:
    """A string literal is read by its type's input function while the
    DEFAULT is analysed, so its error was CREATE's, ahead of a `1/0`."""
    try:
        assert answer(dsn, statement) == LITERAL_INPUT[statement]
    finally:
        answer(dsn, CLEANUP)


IF_NOT_EXISTS = {
    "a missing type": "create table if not exists t890 (a nosuchtype)",
    "a column twice": "create table if not exists t890 (a int, a int)",
    "a bad literal default": "create table if not exists t890 (a int default 'x')",
}


@pytest.mark.backlog("RP-887")
@pytest.mark.parametrize("case", list(IF_NOT_EXISTS))
def test_if_not_exists_over_a_table_checks_nothing(case: str, dsn: str) -> None:
    """IF NOT EXISTS over a table that exists skips before any check."""
    try:
        assert answer(dsn, "create table t890 (a int)", IF_NOT_EXISTS[case]) == CREATED
    finally:
        answer(dsn, CLEANUP)


REGCLASS_DEFAULT = {
    "create table t890 (a int default nextval('nosuch_seq'))": (
        "42P01",
        'relation "nosuch_seq" does not exist',
        None,
        "42",
    ),
}


@pytest.mark.backlog("RP-888")
@pytest.mark.parametrize("statement", list(REGCLASS_DEFAULT))
def test_a_regclass_literal_in_a_default_is_resolved(statement: str, dsn: str) -> None:
    """An untyped literal a DEFAULT passes as `regclass` names a relation
    that must exist."""
    try:
        assert answer(dsn, statement) == REGCLASS_DEFAULT[statement]
    finally:
        answer(dsn, CLEANUP)
