"""``UPDATE … SET col = expr`` assignment-cast rules (42804), and the coercion
errors a value that IS assignable can still fail with.

`sql/typecheck.py` already refused a cross-category *comparison* with 42883;
assignment was left wholly lenient, so `UPDATE t SET text_col = 42` and
`UPDATE t SET int_col = text_col` were both silently coerced. PostgreSQL
applies ASSIGNMENT casts here, which are more permissive than the implicit
casts a comparison gets — a different rule, not the same one on another node —
so this needed its own analysis rather than reusing `_check_comparison`.

Two coercion-error defects surfaced in the same probe run and are fixed with
it: `UPDATE t SET numeric_col = 'abc'` reached the wire as a raw
`[<class 'decimal.ConversionSyntax'>]` (the `Decimal(str(value))` call sat
outside the `try`), and date/time coercion answered our own wording — or, for
`timestamp`, the wrong SQLSTATE. PostgreSQL uses `22007` for every date/time
type and `22P02` for the numeric ones, and names `timestamp` bare in this
message where a type-mismatch message says `timestamp without time zone`.

Every expectation below is a measured PostgreSQL 14.13 answer; the last test
re-runs all 25 shapes against the live server.
"""

from __future__ import annotations

import pytest

import pg_oracle
from secantus.sql.engine import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.session import Session
from secantus.storage import Storage

_SCHEMA = (
    "CREATE TABLE asg (i int, b bigint, t text, v varchar(10), bo bool, "
    "d date, ts timestamp, n numeric, r real)"
)
_SEED = "INSERT INTO asg VALUES (1,1,'x','y',true,'2020-01-01','2020-01-01 00:00',1.5,1.5)"


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    run(_SCHEMA)
    run(_SEED)
    try:
        yield run
    finally:
        storage.close()


def _err(db, sql) -> tuple[str, str]:
    with pytest.raises(SQLError) as ei:
        db(sql)
    return ei.value.sqlstate, str(ei.value)


class TestRejectedAssignments:
    @pytest.mark.parametrize(
        ("sql", "msg"),
        [
            (
                "UPDATE asg SET i = 'abc'::text",
                'column "i" is of type integer but expression is of type text',
            ),
            (
                "UPDATE asg SET i = t",
                'column "i" is of type integer but expression is of type text',
            ),
            (
                "UPDATE asg SET i = true",
                'column "i" is of type integer but expression is of type boolean',
            ),
            (
                "UPDATE asg SET bo = 1",
                'column "bo" is of type boolean but expression is of type integer',
            ),
            (
                "UPDATE asg SET d = 42",
                'column "d" is of type date but expression is of type integer',
            ),
            (
                "UPDATE asg SET ts = 42",
                'column "ts" is of type timestamp without time zone '
                "but expression is of type integer",
            ),
        ],
    )
    def test_cross_category_assignment_is_42804(self, db, sql, msg):
        assert _err(db, sql) == ("42804", msg)


class TestAcceptedAssignments:
    """The lenient half matters as much: a false 42804 rejects a statement
    PostgreSQL would run, which is worse than the coercion it replaces."""

    @pytest.mark.parametrize(
        "sql",
        [
            # A string target takes an assignment cast from everything.
            "UPDATE asg SET t = 42",
            "UPDATE asg SET t = 42.5",
            "UPDATE asg SET t = true",
            "UPDATE asg SET t = i",
            "UPDATE asg SET t = v",
            "UPDATE asg SET v = 42",
            "UPDATE asg SET t = i::text",
            # Within the numeric family, both directions.
            "UPDATE asg SET b = i",
            "UPDATE asg SET i = b",
            "UPDATE asg SET r = i",
            "UPDATE asg SET i = r",
            "UPDATE asg SET i = 1.7",
            # An unknown literal is parsed at RUNTIME, not judged at plan time.
            "UPDATE asg SET i = '42'",
            "UPDATE asg SET bo = 'yes'",
            # NULL is assignable to anything.
            "UPDATE asg SET i = NULL",
            "UPDATE asg SET t = NULL",
        ],
    )
    def test_assignable(self, db, sql):
        db(sql)  # must not raise


class TestCoercionErrors:
    """An assignable expression whose VALUE cannot be read is a runtime error,
    and PostgreSQL splits the SQLSTATE by type family."""

    @pytest.mark.parametrize(
        ("sql", "state", "msg"),
        [
            ("UPDATE asg SET i = 'abc'", "22P02", 'invalid input syntax for type integer: "abc"'),
            ("UPDATE asg SET n = 'abc'", "22P02", 'invalid input syntax for type numeric: "abc"'),
            ("UPDATE asg SET d = 'nope'", "22007", 'invalid input syntax for type date: "nope"'),
            (
                "UPDATE asg SET ts = 'nope'",
                "22007",
                'invalid input syntax for type timestamp: "nope"',
            ),
        ],
    )
    def test_unreadable_value(self, db, sql, state, msg):
        assert _err(db, sql) == (state, msg)

    def test_numeric_does_not_leak_a_python_exception(self, db):
        """It reached the wire as `[<class 'decimal.ConversionSyntax'>]` — the
        `Decimal(str(value))` call sat outside the try that wrapped
        `Decimal128`."""
        _state, msg = _err(db, "UPDATE asg SET n = 'abc'")
        assert "decimal" not in msg.lower()
        assert "ConversionSyntax" not in msg


class TestInsertAssignments:
    """The same rule applies to ``INSERT INTO t (cols) VALUES (…)``.

    This is what psycopg's `test_return_untyped` exercises: `'{}'` as an
    UNKNOWN literal casts into a `jsonb` column, but the same value with a
    declared `text` type does not. The backlog filed it as a binary-parameter
    nicety; measured against PostgreSQL 14.13 it is neither binary-specific nor
    parameter-specific — a bare `42` into a `jsonb` column diverged the same
    way, and the text (non-binary) mode diverged too.
    """

    @pytest.fixture()
    def jdb(self, tmp_path):
        storage = Storage(str(tmp_path))
        session = Session(database="t")

        def run(sql: str):
            return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

        run("CREATE TABLE ins (i int, t text, j jsonb, bo bool)")
        try:
            yield run
        finally:
            storage.close()

    @pytest.mark.parametrize(
        ("sql", "msg"),
        [
            (
                "INSERT INTO ins (j) VALUES ('{}'::text)",
                'column "j" is of type jsonb but expression is of type text',
            ),
            (
                "INSERT INTO ins (j) VALUES (42)",
                'column "j" is of type jsonb but expression is of type integer',
            ),
            (
                "INSERT INTO ins (i) VALUES ('x'::text)",
                'column "i" is of type integer but expression is of type text',
            ),
            (
                "INSERT INTO ins (bo) VALUES (1)",
                'column "bo" is of type boolean but expression is of type integer',
            ),
        ],
    )
    def test_unassignable_value_is_42804(self, jdb, sql, msg):
        assert _err(jdb, sql) == ("42804", msg)

    @pytest.mark.parametrize(
        "sql",
        [
            # An UNKNOWN literal is resolved by the target type, not judged.
            "INSERT INTO ins (j) VALUES ('{}')",
            "INSERT INTO ins (i) VALUES ('42')",
            "INSERT INTO ins (bo) VALUES ('yes')",
            # A string target still takes anything.
            "INSERT INTO ins (t) VALUES (42)",
            "INSERT INTO ins (t) VALUES (true)",
            # Same category, and NULL.
            "INSERT INTO ins (i) VALUES (1.7)",
            "INSERT INTO ins (i) VALUES (NULL)",
            # Shapes the analysis deliberately does not judge.
            "INSERT INTO ins VALUES (1, 'x', '{}', true)",
            "INSERT INTO ins (i) SELECT 1",
            "INSERT INTO ins (i) VALUES (1), (2)",
        ],
    )
    def test_assignable_or_undecidable(self, jdb, sql):
        jdb(sql)  # must not raise


#: Every shape from the probe that drove this work.
_ORACLE_SHAPES = [
    "UPDATE asg SET t = 42",
    "UPDATE asg SET t = 42.5",
    "UPDATE asg SET t = true",
    "UPDATE asg SET i = 'abc'",
    "UPDATE asg SET i = '42'",
    "UPDATE asg SET i = 'abc'::text",
    "UPDATE asg SET i = true",
    "UPDATE asg SET i = 1.7",
    "UPDATE asg SET bo = 1",
    "UPDATE asg SET bo = 'yes'",
    "UPDATE asg SET d = 'nope'",
    "UPDATE asg SET d = 42",
    "UPDATE asg SET ts = 42",
    "UPDATE asg SET n = 'abc'",
    "UPDATE asg SET b = i",
    "UPDATE asg SET i = b",
    "UPDATE asg SET t = v",
    "UPDATE asg SET i = t",
    "UPDATE asg SET t = i",
    "UPDATE asg SET v = 42",
    "UPDATE asg SET r = i",
    "UPDATE asg SET i = r",
    "UPDATE asg SET t = i::text",
    "UPDATE asg SET i = NULL",
    "UPDATE asg SET t = NULL",
]


@pytest.mark.skipif(not pg_oracle.available(), reason=pg_oracle.skip_reason())
def test_assignment_rules_match_real_postgres(db):
    pg = pg_oracle.connect()
    assert pg is not None
    pg.autocommit = True

    def theirs(sql):
        try:
            pg.execute(sql)
            return ("OK", "")
        except Exception as exc:  # noqa: BLE001
            diag = getattr(exc, "diag", None)
            primary = getattr(diag, "message_primary", None) or str(exc).strip()
            return (getattr(diag, "sqlstate", None), primary)

    def ours(sql):
        try:
            db(sql)
            return ("OK", "")
        except SQLError as exc:
            return (exc.sqlstate, str(exc).strip())

    try:
        pg.execute("DROP TABLE IF EXISTS asg")
        pg.execute(_SCHEMA)
        pg.execute(_SEED)
        for shape in _ORACLE_SHAPES:
            assert ours(shape) == theirs(shape), shape
    finally:
        pg.execute("DROP TABLE IF EXISTS asg")
        pg.close()
