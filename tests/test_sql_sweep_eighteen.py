"""An eighteenth differential sweep — jsonpath, `to_number`, and a scout.

This batch started by scouting several surfaces shallowly rather than picking
one and going deep, which turned out to be the right order: advisory locks came
back **perfect** across 15 two-connection scenarios (reentrancy, shared locks,
the two-key form, transaction scoping, release on ROLLBACK, `pg_locks`
columns), and the lead that sent me there — a batch-10 reading of "zero locks
held" — was a probe artifact: `pg_advisory_xact_lock` in autocommit commits
immediately, so of course the lock was gone.

Three real defects, all found by the scout:

**`jsonb_path_query` returned only the first match.** It is a set-returning
function and was not registered as one, so a path that matches many values
yielded one row: `SELECT count(*) FROM jsonb_path_query('{"a":[1,2,3]}',
'$.a[*]')` answered 1 where PostgreSQL answers 3. Rows silently missing, which
is the worst shape a wrong answer takes. `jsonpath.query()` had always returned
the full list; only the registration was absent.

**A predicate could not be used as a whole path expression.**
`jsonb_path_match(j, 'exists($.a)')` failed to parse. Adding it exposed a rule
worth pinning because it is genuinely surprising: a predicate used as a path
*yields one boolean item*, so `jsonb_path_exists(doc, 'exists($.zz)')` is
**true** even when `$.zz` is absent — an item was produced, and its value
happens to be false. `jsonb_path_query` of the same path returns that `false`.

**`to_number` answered NULL for everything.** sqlglot gives it a dedicated
`exp.ToNumber` node rather than an anonymous call, so the name-keyed dispatch
never saw it — registering the name in three places changed nothing until the
node itself was handled. The parse rules were measured across 24 format masks,
and two are easy to get wrong by reasoning: excess decimals are **truncated,
not rounded** (`to_number('12.345','99.99')` is 12.34), and the mask does not
pad (`to_number('12','99.99')` is 12, not 12.00).

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

from decimal import Decimal

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path / "s18"))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    host, port = srv.address
    c = psycopg.connect(host=host, port=port, dbname="db", user="joe", autocommit=True)
    try:
        yield c
    finally:
        c.close()
        srv.stop()
        st.close()


def one(c, sql):
    return c.execute(sql).fetchone()[0]


# --- jsonb_path_query is set-returning --------------------------------------- #


def test_path_query_returns_every_match(conn):
    assert conn.execute("SELECT jsonb_path_query('{\"a\":[1,2,3]}', '$.a[*]')").fetchall() == [
        (1,),
        (2,),
        (3,),
    ]


def test_path_query_in_from_position(conn):
    """The shape that makes it unmistakable: a row COUNT, not a value."""
    assert one(conn, "SELECT count(*) FROM jsonb_path_query('{\"a\":[1,2,3]}', '$.a[*]') q") == 3


def test_path_query_with_a_filter(conn):
    assert conn.execute("SELECT jsonb_path_query('[1,2,3]', '$[*] ? (@ > 1)')").fetchall() == [
        (2,),
        (3,),
    ]


def test_path_query_with_a_wildcard_member(conn):
    assert conn.execute(
        'SELECT jsonb_path_query(\'{"a":{"b":1},"c":{"b":2}}\', \'$.*.b\')'
    ).fetchall() == [(1,), (2,)]


def test_the_array_and_first_forms_are_unchanged(conn):
    assert one(conn, "SELECT jsonb_path_query_array('{\"a\":[1,2,3]}', '$.a[*]')") == [1, 2, 3]
    assert one(conn, "SELECT jsonb_path_query_first('{\"a\":[1,2,3]}', '$.a[*]')") == 1


# --- a predicate used as a whole path ---------------------------------------- #


def test_exists_predicate_in_match(conn):
    assert one(conn, "SELECT jsonb_path_match('{\"a\":1}', 'exists($.a)')") is True
    assert one(conn, "SELECT jsonb_path_match('{\"a\":1}', 'exists($.zz)')") is False
    assert one(conn, "SELECT jsonb_path_match('{\"a\":{\"b\":1}}', 'exists($.a.b)')") is True


def test_a_predicate_path_yields_a_boolean_item(conn):
    """The surprising one. `jsonb_path_exists` asks whether an ITEM was
    produced, and a predicate always produces one — so it is true even when the
    predicate itself is false, while `jsonb_path_query` returns that false."""
    assert one(conn, "SELECT jsonb_path_exists('{\"a\":1}', 'exists($.zz)')") is True
    assert one(conn, "SELECT jsonb_path_query('{\"a\":1}', 'exists($.zz)')") is False
    assert one(conn, "SELECT jsonb_path_query('{\"a\":1}', 'exists($.a)')") is True


def test_ordinary_paths_are_unaffected(conn):
    assert one(conn, "SELECT jsonb_path_exists('{\"a\":[1,2,3]}', '$.a[*] ? (@ > 2)')") is True
    assert one(conn, "SELECT jsonb_path_exists('{\"a\":[1,2,3]}', '$.a[*] ? (@ > 9)')") is False


# --- to_number --------------------------------------------------------------- #


@pytest.mark.parametrize(
    "value,fmt,expected",
    [
        ("1,234.50", "9,999.99", Decimal("1234.50")),
        ("12.34", "99.99", Decimal("12.34")),
        ("1234", "9999", Decimal("1234")),
        ("  42", "999", Decimal("42")),
        ("0012", "9999", Decimal("12")),
        ("1,2,3", "9,9,9", Decimal("123")),
        ("1 234", "9G999", Decimal("1234")),
        # Decoration is dropped rather than required to line up.
        ("12%", "99%", Decimal("12")),
        ("$12.34", "L99.99", Decimal("12.34")),
        ("123", "999D99", Decimal("123")),
        # Sign may lead, trail, or be angle brackets.
        ("-12", "S99", Decimal("-12")),
        ("+12", "S99", Decimal("12")),
        ("12-", "99MI", Decimal("-12")),
        ("<12>", "99PR", Decimal("-12")),
        ("12", "99PR", Decimal("12")),
        ("-1.5", "S9.9", Decimal("-1.5")),
    ],
)
def test_to_number(conn, value, fmt, expected):
    assert one(conn, f"SELECT to_number('{value}','{fmt}')") == expected


def test_excess_decimals_are_truncated_not_rounded(conn):
    assert one(conn, "SELECT to_number('12.345','99.99')") == Decimal("12.34")


def test_the_mask_does_not_pad(conn):
    assert one(conn, "SELECT to_number('12','99.99')") == Decimal("12")


def test_to_number_reports_numeric(conn):
    r = conn.execute("SELECT to_number('1','9')")
    assert r.description[0].type_code == 1700
    assert one(conn, "SELECT pg_typeof(to_number('1','9'))") == "numeric"


@pytest.mark.parametrize("value", ["abc", ""])
def test_input_without_digits_is_refused(conn, value):
    with pytest.raises(psycopg.Error) as ei:
        conn.execute(f"SELECT to_number('{value}','999')")
    assert getattr(ei.value.diag, "sqlstate", None) == "22P02"


def test_null_propagates(conn):
    assert one(conn, "SELECT to_number(NULL,'999')") is None
