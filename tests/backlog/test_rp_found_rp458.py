"""Gap tests found while working RP-458 (a `regclass` literal read at
analysis).

Each answer is `(sqlstate, message, hint, position)`, measured on
PostgreSQL 15.19 (the shared reference: none of these needs an object). They
fail here whether the literal is read at analysis or when the call runs:
the lookup itself, or the sequence functions after it, answer differently.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


def answer(server: Any, query: str) -> object:
    with psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c:
        try:
            return c.execute(query).fetchall()
        except psycopg.Error as e:
            return (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_hint,
                e.diag.statement_position,
            )


REGCLASSIN_ERRORS = {
    # `RangeVarGetRelid` looks the schema up first: a missing one is 3F000.
    "select 'nosuch.s'::regclass": ("3F000", 'schema "nosuch" does not exist', None, "8"),
    "select pg_relation_size('nosuch.s')": (
        "3F000",
        'schema "nosuch" does not exist',
        None,
        "25",
    ),
    # The error is at the literal however the name inside it is written.
    "select '\"Nosuch\"'::regclass": ("42P01", 'relation "Nosuch" does not exist', None, "8"),
    "select nextval($$nosuchseq$$)": ("42P01", 'relation "nosuchseq" does not exist', None, "16"),
}


@pytest.mark.backlog("RP-675")
@pytest.mark.parametrize("query", list(REGCLASSIN_ERRORS))
def test_regclassin_errors(query: str, rust_pg: Any) -> None:
    """A missing schema is 42P01 naming the whole name, and a 42P01 whose
    name is quoted or dollar-quoted in the statement has no position."""
    assert answer(rust_pg, query) == REGCLASSIN_ERRORS[query]


NOT_A_SEQUENCE = {
    # `init_sequence` opens the relation, then refuses one that is not a
    # sequence (`lock_and_open_sequence`, `relkind != RELKIND_SEQUENCE`).
    "select nextval('pg_class')": ("42809", '"pg_class" is not a sequence', None, None),
    "select currval('pg_class')": ("42809", '"pg_class" is not a sequence', None, None),
    "select setval('pg_class', 1)": ("42809", '"pg_class" is not a sequence', None, None),
    "select nextval('pg_class'::regclass)": (
        "42809",
        '"pg_class" is not a sequence',
        None,
        None,
    ),
    # An oid no relation has is opened by oid, and that fails.
    "select nextval(1234)": ("XX000", "could not open relation with OID 1234", None, None),
}


@pytest.mark.backlog("RP-676")
@pytest.mark.parametrize("query", list(NOT_A_SEQUENCE))
def test_sequence_function_over_a_relation_not_a_sequence(query: str, rust_pg: Any) -> None:
    """A relation that is not a sequence, or a bare oid, is 42P01 here."""
    assert answer(rust_pg, query) == NOT_A_SEQUENCE[query]


NO_MATCH = (
    "No function matches the given name and argument types. "
    "You might need to add explicit type casts."
)

NAMED_ARGUMENTS = {
    # `nextval`'s parameter has no name in `pg_proc` (`proargnames` NULL),
    # so no candidate takes a named argument: 42883 naming the call.
    "select nextval(regclass := 'nosuchseq')": (
        "42883",
        "function nextval(regclass => unknown) does not exist",
        NO_MATCH,
        "8",
    ),
    "select nextval(x => 'nosuchseq')": (
        "42883",
        "function nextval(x => unknown) does not exist",
        NO_MATCH,
        "8",
    ),
}


@pytest.mark.backlog("RP-677")
@pytest.mark.parametrize("query", list(NAMED_ARGUMENTS))
def test_named_argument_to_a_builtin_without_argument_names(query: str, rust_pg: Any) -> None:
    """A named argument to `nextval` is 0A000 `NamedArgExpr is not
    supported yet` here, with no position."""
    assert answer(rust_pg, query) == NAMED_ARGUMENTS[query]
