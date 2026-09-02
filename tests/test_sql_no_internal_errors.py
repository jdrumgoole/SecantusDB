"""A scalar builtin handed an operand type it does not model must not leak
`XX000 internal error`.

Two such leaks were found by accident in consecutive sweeps
(`substring(x FROM regex)` and `to_char(interval, …)`), so this hunt was run
deliberately: every function against every value type, reporting only the
shapes where SecantusDB answered `XX000`. **397 shapes did.** The cause was
always the same — a bare Python `TypeError` / `ValueError` from a builtin
applied to a type it does not handle, escaping to the wire.

PostgreSQL 14.13 answers `42883 function f(types) does not exist` for 353 of
those 397, which is what the guard now produces. It sits at the two evaluation
boundaries (the typed-node table and the named-function path) rather than in
each builtin, because per-function guards are precisely what leaves 397 holes.

`SQLError` is deliberately re-raised untouched, so a handler that already
diagnosed the problem keeps its own code and message — the guard only catches
the ones that had none.
"""

from __future__ import annotations

import itertools

import pytest

from secantus.sql import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.session import Session
from secantus.storage import Storage

#: One value per interesting type, including the ones that used to crash.
_VALUES = [
    "1",
    "'abc'",
    "NULL",
    "true",
    "1.5",
    "date '2020-03-05'",
    "timestamp '2020-03-05 10:20:30'",
    "interval '3 days'",
    "ARRAY[1,2]",
    "'{\"a\":1}'::jsonb",
    "'\\x01'::bytea",
    "'a'::char(1)",
    "1000000::numeric",
    "time '10:20:30'",
    "'{a,b}'::text[]",
]

_UNARY = [
    "abs",
    "upper",
    "lower",
    "length",
    "md5",
    "btrim",
    "ceil",
    "floor",
    "round",
    "sign",
    "cardinality",
    "reverse",
    "initcap",
    "ascii",
    "justify_hours",
    "age",
    "to_char",
    "date_trunc",
    "char_length",
    "octet_length",
    "quote_ident",
    "unnest",
]
_BINARY = ["to_char", "date_trunc", "lpad", "split_part", "repeat"]


@pytest.fixture(scope="module")
def db(tmp_path_factory):
    storage = Storage(str(tmp_path_factory.mktemp("noxx")))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    try:
        yield run
    finally:
        storage.close()


def _sqlstate(db, sql) -> str:
    try:
        db(sql)
        return "OK"
    except SQLError as exc:
        return exc.sqlstate or "?"


@pytest.mark.parametrize("fn", _UNARY)
def test_no_internal_error_from_any_unary_shape(db, fn):
    bad = [v for v in _VALUES if _sqlstate(db, f"SELECT {fn}({v})") == "XX000"]
    assert bad == [], f"{fn}() leaked XX000 for: {bad}"


@pytest.mark.parametrize("fn", _BINARY)
def test_no_internal_error_from_any_binary_shape(db, fn):
    bad = [
        (v, a)
        for v, a in itertools.product(_VALUES, ["'DD'", "2", "'month'"])
        if _sqlstate(db, f"SELECT {fn}({v}, {a})") == "XX000"
    ]
    assert bad == [], f"{fn}() leaked XX000 for: {bad}"


class TestTheMessageShape:
    @pytest.mark.parametrize(
        ("sql", "message"),
        [
            ("SELECT age(1)", "function age(integer) does not exist"),
            ("SELECT age(true)", "function age(boolean) does not exist"),
            (
                "SELECT age('{a,b}'::text[])",
                "function age(text[]) does not exist",
            ),
        ],
    )
    def test_matches_postgres(self, db, sql, message):
        """These three are byte-identical to PostgreSQL 14.13's answer."""
        with pytest.raises(SQLError) as ei:
            db(sql)
        assert ei.value.sqlstate == "42883"
        assert str(ei.value) == message

    def test_a_handlers_own_error_is_not_replaced(self, db):
        """The guard re-raises `SQLError` untouched — `to_char(interval,'Day')`
        keeps its own 22007 rather than becoming a generic 42883."""
        with pytest.raises(SQLError) as ei:
            db("SELECT to_char(interval '3 days', 'Day')")
        assert ei.value.sqlstate == "22007"

    def test_working_calls_are_unaffected(self, db):
        assert db("SELECT upper('abc')") == [("ABC",)]
        assert db("SELECT abs(-3)") == [(3,)]
        assert db("SELECT to_char(date '2020-03-05','YYYY-MM-DD')") == [("2020-03-05",)]
