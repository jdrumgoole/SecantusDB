"""Plan-time comparison-operator resolution — Postgres' 42883.

``text_col = 42`` is not a predicate that matches nothing; real Postgres
resolves comparison operators during parse analysis and fails with ``42883
operator does not exist: text = integer`` before reading a row. These tests pin
both halves of that: the pairs that must now error, and — at least as
important — the pairs that must stay lenient, because a spurious 42883 breaks a
query that works. See ``secantus.sql.typecheck``.
"""

from __future__ import annotations

import bson
import pytest

from secantus.sql import errors, planner, run_sql, typecheck
from secantus.sql.catalog import Catalog
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path, session):
    s = Storage(str(tmp_path))
    try:
        run_sql(
            s,
            DB,
            "CREATE TABLE t (id int PRIMARY KEY, txt text, vc varchar(10), n int, "
            "big bigint, num numeric, flag boolean, d date, ts timestamp, "
            "b bytea, u uuid, j jsonb, m money, tm time)",
            session=session,
        )
        run_sql(
            s,
            DB,
            "INSERT INTO t (id, txt, vc, n, big, num, flag, d, ts) VALUES "
            "(1, '42', '42', 42, 42, 42.0, true, '2020-01-01', '2020-01-01 00:00:00')",
            session=session,
        )
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1].rows


# --- the rule, unit-level -------------------------------------------------- #


@pytest.fixture
def rule(storage):
    """The analysis verdict for one statement against the real catalog: None
    when the comparison is resolvable (or undecidable), else the 42883
    message. Drives ``check_statement`` directly so the verdict is observable
    without the rest of the planner in the way."""
    catalog = Catalog(storage)

    def verdict(sql):
        stmt = planner.parse(sql)[0]
        try:
            typecheck.check_statement(stmt, catalog, DB)
        except errors.SQLError as exc:
            assert exc.sqlstate == "42883"
            return exc.message
        return None

    return verdict


@pytest.mark.parametrize(
    ("where", "message"),
    [
        ("txt = 42", "operator does not exist: text = integer"),
        ("42 = txt", "operator does not exist: integer = text"),
        ("txt < 42", "operator does not exist: text < integer"),
        ("num >= txt", "operator does not exist: numeric >= text"),
        ("flag = 1", "operator does not exist: boolean = integer"),
        ("flag <> txt", "operator does not exist: boolean <> text"),
        ("d = n", "operator does not exist: date = integer"),
        ("ts > txt", "operator does not exist: timestamp without time zone > text"),
        ("n = txt::text", "operator does not exist: integer = text"),
        ("txt = n::int", "operator does not exist: text = integer"),
        ("n = lower(txt)", "operator does not exist: integer = text"),
        ("n = substr(txt, 1, 1)", "operator does not exist: integer = text"),
        ("txt = length(txt)", "operator does not exist: text = integer"),
    ],
)
def test_incomparable_pairs_are_42883(rule, where, message):
    assert rule(f"SELECT id FROM t WHERE {where}") == message


@pytest.mark.parametrize(
    "where",
    [
        # Same category: Postgres has implicit casts in both directions.
        "n = num",
        "num > n",
        "n = 42",
        "n = 42.5",
        "txt = txt",
        "flag = true",
        "d = ts",
        "ts = d",
        # An untyped literal takes the other operand's type — the single most
        # important lenient case.
        "txt = '42'",
        "n = '42'",
        "d = '2020-01-01'",
        "flag = 'true'",
        # A parameter's type is not decided here.
        "txt = $1",
        # Casts that agree.
        "txt = n::text",
        "n = txt::int",
        # Categories we deliberately refuse to judge: bytea, uuid, json, money,
        # time, interval, arrays, ranges, geo, network, bit.
        "b = txt",
        "b = 42",
        "j = txt",
        # NULL is not a type.
        "txt = NULL",
        # Function whose argument type we cannot pin stays unjudged.
        "n = lower(b)",
    ],
)
def test_comparable_or_undecidable_pairs_stay_lenient(rule, where):
    assert rule(f"SELECT id FROM t WHERE {where}") is None


def test_unresolvable_column_is_left_to_the_normal_path(rule):
    # No verdict here — the planner raises the correct 42703 later.
    assert rule("SELECT id FROM t WHERE nope = 42") is None


def test_output_alias_shadowing_a_column_is_not_judged(rule):
    # ORDER BY resolves against the select list, and the alias rewrite runs
    # after us — refuse to guess which ``txt`` this is.
    assert rule("SELECT n AS txt FROM t ORDER BY txt = 42") is None


def test_unknown_table_is_not_judged(rule):
    assert rule("SELECT id FROM other WHERE txt = 42") is None


def test_enum_column_is_named_by_its_declared_type(storage, session):
    # An enum column stores the ``text`` tag, but Postgres names the enum in
    # the message ("operator does not exist: mood = integer").
    run(storage, session, "CREATE TYPE mood AS ENUM ('ok', 'sad')")
    run(storage, session, "CREATE TABLE e (id int PRIMARY KEY, how mood)")
    with pytest.raises(errors.SQLError) as exc:
        run(storage, session, "SELECT id FROM e WHERE how = 1")
    assert exc.value.sqlstate == "42883"
    assert exc.value.message == "operator does not exist: mood = integer"
    # …and the enum's own comparisons keep working.
    run(storage, session, "INSERT INTO e VALUES (1, 'ok')")
    assert run(storage, session, "SELECT id FROM e WHERE how = 'ok'") == [(1,)]


def test_update_set_assignment_is_not_a_comparison(rule):
    # sqlglot parses ``SET txt = 42`` as an EQ, but Postgres reports an
    # unassignable value as 42804 datatype_mismatch, under assignment-cast
    # rules — a different analysis, so this one keeps its hands off.
    assert rule("UPDATE t SET txt = 42 WHERE id = 1") is None
    assert rule("UPDATE t SET flag = 1 WHERE id = 1") is None
    # …but the WHERE clause of the same statement is still analysed.
    assert rule("UPDATE t SET n = 1 WHERE txt = 42") == "operator does not exist: text = integer"


# --- end to end through run_sql ------------------------------------------- #


@pytest.mark.parametrize(
    "sql",
    [
        "SELECT id FROM t WHERE txt = 42",
        "SELECT id FROM t WHERE n = txt",
        "SELECT id FROM t WHERE flag = 1",
        "SELECT (txt = 42) AS bad FROM t",
        "SELECT id FROM t WHERE txt = 42 AND n = 42",
        "UPDATE t SET n = 1 WHERE txt = 42",
        "DELETE FROM t WHERE txt = 42",
    ],
)
def test_statement_fails_with_42883(storage, session, sql):
    with pytest.raises(errors.SQLError) as exc:
        run(storage, session, sql)
    assert exc.value.sqlstate == "42883"
    assert exc.value.message.startswith("operator does not exist: ")


def test_error_is_raised_before_any_row_is_read(storage, session):
    # Postgres fails this at parse analysis, so an EMPTY table errors too.
    run(storage, session, "CREATE TABLE empt (id int PRIMARY KEY, txt text)")
    with pytest.raises(errors.SQLError) as exc:
        run(storage, session, "SELECT id FROM empt WHERE txt = 1")
    assert exc.value.sqlstate == "42883"


def test_comparable_queries_still_run(storage, session):
    assert run(storage, session, "SELECT id FROM t WHERE txt = '42'") == [(1,)]
    assert run(storage, session, "SELECT id FROM t WHERE n = 42") == [(1,)]
    assert run(storage, session, "SELECT id FROM t WHERE n = num") == [(1,)]
    assert run(storage, session, "SELECT id FROM t WHERE vc = txt") == [(1,)]
    assert run(storage, session, "SELECT id FROM t WHERE flag = true") == [(1,)]
    assert run(storage, session, "SELECT id FROM t WHERE length(txt) = 2") == [(1,)]
    # date vs timestamp is one operator in Postgres, so it must reach the
    # evaluator rather than 42883 (what the evaluator then makes of the stored
    # ISO-text date is a separate matter — it is not this analysis' business).
    assert run(storage, session, "SELECT id FROM t WHERE d = ts") == []


def test_join_across_declared_tables_is_judged(storage, session):
    run(storage, session, "CREATE TABLE u (id int PRIMARY KEY, s text)")
    with pytest.raises(errors.SQLError) as exc:
        run(storage, session, "SELECT t.id FROM t JOIN u ON u.s = t.n")
    assert exc.value.sqlstate == "42883"
    assert exc.value.message == "operator does not exist: text = integer"


def test_reflected_table_stays_lenient(storage, session):
    """The dual-protocol exemption. A schema-on-read collection's column types
    come from sampling 50 documents, so a heterogeneous BSON field can be
    declared ``text`` while holding integers — erroring there would break
    working queries over pymongo-written data."""
    storage.insert(DB, "mixed", [{"_id": bson.Int64(1), "v": "7"}, {"_id": bson.Int64(2), "v": 7}])
    # ``v`` reflects as text from the first sampled doc, so the reflected path
    # coerces the 7 to the sampled type and answers the string-valued row.
    # Whatever it answers, it must answer — not raise.
    assert run(storage, session, "SELECT _id FROM mixed WHERE v = 7") == [(1,)]


def test_join_touching_a_reflected_table_exempts_the_whole_statement(storage, session):
    # ``t.txt = 42`` would be 42883 on its own; joining a reflected table
    # exempts the whole statement, so the pre-existing lenient answer stands.
    storage.insert(DB, "mixed2", [{"_id": bson.Int64(1), "v": "7"}])
    rows = run(
        storage,
        session,
        "SELECT t.id FROM t JOIN mixed2 m ON m._id = t.id WHERE t.txt = 42",
    )
    assert rows == [(1,)]


def test_subquery_scope_is_not_judged(storage, session):
    # The inner comparison resolves against a scope this analysis does not
    # model, so it is skipped rather than guessed at.
    rows = run(
        storage,
        session,
        "SELECT id FROM t WHERE n = (SELECT max(n) FROM t WHERE txt = '42')",
    )
    assert rows == [(1,)]


def test_cte_is_not_judged(storage, session):
    rows = run(
        storage,
        session,
        "WITH c AS (SELECT n AS v FROM t) SELECT v FROM c WHERE v = 42",
    )
    assert rows == [(42,)]
