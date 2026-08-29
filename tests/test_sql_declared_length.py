"""Declared `char(n)` / `varchar(n)` widths, and casts to text in a WHERE.

Two findings from the pgtest campaign, both cases of the engine being more
permissive than Postgres:

* an over-length value was stored intact, so a column could hold data that
  violated its own declared schema;
* `WHERE n::text = '2'` matched nothing, because the pushdown compared the
  stored number against the string — the cast was applied on the way out but
  not in the predicate.

Every expectation was probed against a real PostgreSQL 14.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql, typemap
from secantus.sql.errors import SQLError
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


class TestDeclaredLength:
    @pytest.fixture
    def table(self, storage, session):
        run(storage, session, "CREATE TABLE t (v VARCHAR(3), c CHAR(3))")
        return storage, session

    def test_a_value_that_fits_is_stored(self, table):
        storage, session = table
        run(storage, session, "INSERT INTO t VALUES ('abc', 'abc')")
        assert run(storage, session, "SELECT v FROM t").rows == [("abc",)]

    @pytest.mark.parametrize(
        ("sql", "type_name"),
        [
            ("INSERT INTO t (v) VALUES ('abcd')", "character varying(3)"),
            ("INSERT INTO t (c) VALUES ('abcd')", "character(3)"),
        ],
    )
    def test_an_overlong_value_is_refused(self, table, sql, type_name):
        storage, session = table
        with pytest.raises(SQLError) as exc:
            run(storage, session, sql)
        assert exc.value.sqlstate == "22001"
        assert f"value too long for type {type_name}" in str(exc.value)

    def test_trailing_blank_overflow_is_trimmed_not_refused(self, table):
        # PostgreSQL 14 accepts 'abc  ' into varchar(3) and stores 'abc'.
        storage, session = table
        run(storage, session, "INSERT INTO t (v) VALUES ('abc  ')")
        assert run(storage, session, "SELECT v FROM t").rows == [("abc",)]

    def test_update_enforces_it_too(self, table):
        storage, session = table
        run(storage, session, "INSERT INTO t VALUES ('abc', 'abc')")
        with pytest.raises(SQLError) as exc:
            run(storage, session, "UPDATE t SET v = 'abcd'")
        assert exc.value.sqlstate == "22001"
        # ... and the row is untouched.
        assert run(storage, session, "SELECT v FROM t").rows == [("abc",)]

    def test_an_unbounded_declaration_is_unaffected(self, storage, session):
        run(storage, session, "CREATE TABLE u (v VARCHAR, t TEXT)")
        long = "x" * 500
        run(storage, session, f"INSERT INTO u VALUES ('{long}', '{long}')")
        assert run(storage, session, "SELECT length(v), length(t) FROM u").rows == [(500, 500)]

    def test_the_helper_directly(self):
        assert typemap.enforce_declared_length("abc", 1043, 7) == "abc"
        assert typemap.enforce_declared_length("abc  ", 1043, 7) == "abc"
        # Not a bounded string type, or no declared width: untouched.
        assert typemap.enforce_declared_length("abcd", 25, 7) == "abcd"
        assert typemap.enforce_declared_length("abcd", 1043, -1) == "abcd"
        assert typemap.enforce_declared_length(42, 1043, 7) == 42


class TestTextCastInWhere:
    """The cast must be applied in the predicate, not only on output."""

    @pytest.fixture
    def table(self, storage, session):
        run(storage, session, "CREATE TABLE t (n INT8, f FLOAT8, b BOOL, d NUMERIC)")
        run(storage, session, "INSERT INTO t VALUES (2, 2.5, true, 2.50)")
        return storage, session

    @pytest.mark.parametrize(
        ("predicate", "matches"),
        [
            ("n::text = '2'", True),
            ("n::text = '3'", False),
            ("f::text = '2.5'", True),
            ("b::text = 'true'", True),  # 'true', not the wire form 't'
            ("b::text = 't'", False),
            ("d::text = '2.50'", True),  # numeric keeps its scale
        ],
    )
    def test_matches_postgres(self, table, predicate, matches):
        storage, session = table
        rows = run(storage, session, f"SELECT n FROM t WHERE {predicate}").rows
        assert rows == ([(2,)] if matches else [])

    def test_a_plain_comparison_still_pushes_down(self, table):
        # The per-row route must only claim predicates that need it.
        storage, session = table
        assert run(storage, session, "SELECT n FROM t WHERE n = 2").rows == [(2,)]


class TestLengthFunctionsAndTextCasts:
    """Two functions that answered the wrong thing for every non-bit input, and
    a cast that leaked internals. All expectations probed against PostgreSQL 14."""

    @pytest.fixture
    def storage_session(self, storage, session):
        return storage, session

    @pytest.mark.parametrize(
        ("sql", "expected"),
        [
            # octet_length / bit_length were dispatched to the BIT-STRING
            # implementations for every input, so octet_length('abc') answered
            # (3+7)//8 = 1 and bit_length('abc') answered 3.
            ("SELECT octet_length('abc')", 3),
            ("SELECT bit_length('abc')", 24),
            # Encoded bytes, not characters: 'é' is one character, two bytes.
            ("SELECT octet_length('é')", 2),
            ("SELECT bit_length('é')", 16),
            ("SELECT length('é')", 1),
            # The bit-string forms must keep their own semantics.
            ("SELECT octet_length(B'1010')", 1),
            ("SELECT bit_length(B'1010')", 4),
        ],
    )
    def test_length_functions_match_postgres(self, storage_session, sql, expected):
        storage, session = storage_session
        assert run(storage, session, sql).rows == [(expected,)]

    def test_an_interval_cast_to_text_is_postgres_text(self, storage_session):
        # This used to hand the client our internal subdocument —
        # '{"interval": {"months": 0, "days": 1, ...}}'.
        storage, session = storage_session
        run(storage, session, "CREATE TABLE iv (i INTERVAL)")
        run(storage, session, "INSERT INTO iv VALUES ('1 day')")
        assert run(storage, session, "SELECT i::text FROM iv").rows == [("1 day",)]
        assert run(storage, session, "SELECT (INTERVAL '2 hours')::text").rows == [("02:00:00",)]
