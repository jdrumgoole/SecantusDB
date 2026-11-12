"""Gap tests of the DEFAULT divergences found while working RP-881: RP-886
(a constant DEFAULT is evaluated at CREATE TABLE), RP-887 (CREATE TABLE IF
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

CONSTANT_DEFAULT: dict[str, tuple[tuple[str, ...], Any]] = {
    "1/0": (("create table t890 (a int default 1/0)",), CREATED),
    "1/0, then an INSERT": (
        ("create table t890 (a int default 1/0)", "insert into t890 default values"),
        ("22012", "division by zero", None, None),
    ),
    "an overflow": (("create table t890 (a int default 2147483647 + 1)",), CREATED),
}


@pytest.mark.backlog("RP-886")
@pytest.mark.parametrize("case", list(CONSTANT_DEFAULT))
def test_a_constant_default_is_not_run_at_create(case: str, dsn: str) -> None:
    """A DEFAULT's errors come from the INSERT that runs it, not CREATE."""
    statements, expected = CONSTANT_DEFAULT[case]
    try:
        assert answer(dsn, *statements) == expected
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
