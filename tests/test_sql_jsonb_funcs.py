"""jsonb manipulation functions — ``jsonb_set`` / ``jsonb_insert`` /
``jsonb_strip_nulls`` / ``jsonb_pretty`` / ``jsonb_object_keys`` and the ``#-``
delete-at-path operator.

jsonb is stored as a native embedded document (dict / list), so these evaluate in
Python over the structure. The ``path`` argument is a Postgres ``text[]`` (``'{a,b}'``
or a list); the value argument is parsed as JSON (``'5'`` -> 5) the way an implicit
``::jsonb`` cast would.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
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


@pytest.fixture
def t(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, data jsonb)")
    run(storage, session, 'INSERT INTO t VALUES (1, \'{"a": 1, "b": {"c": 2}, "n": null}\')')
    return storage


def test_jsonb_set_existing_key(t, session):
    assert run(t, session, "SELECT jsonb_set(data, '{a}', '5') FROM t").rows == [
        ({"a": 5, "b": {"c": 2}, "n": None},)
    ]


def test_jsonb_set_nested(t, session):
    assert run(t, session, "SELECT jsonb_set(data, '{b,c}', '99') FROM t").rows == [
        ({"a": 1, "b": {"c": 99}, "n": None},)
    ]


def test_jsonb_set_creates_missing(t, session):
    assert run(t, session, "SELECT jsonb_set(data, '{x}', '7') FROM t").rows == [
        ({"a": 1, "b": {"c": 2}, "n": None, "x": 7},)
    ]


def test_jsonb_set_object_value(t, session):
    assert run(t, session, "SELECT jsonb_set(data, '{a}', '{\"k\":1}') FROM t").rows == [
        ({"a": {"k": 1}, "b": {"c": 2}, "n": None},)
    ]


def test_jsonb_insert_new_key(t, session):
    assert run(t, session, "SELECT jsonb_insert(data, '{y}', '8') FROM t").rows == [
        ({"a": 1, "b": {"c": 2}, "n": None, "y": 8},)
    ]


def test_jsonb_insert_existing_key_is_noop(t, session):
    assert run(t, session, "SELECT jsonb_insert(data, '{a}', '99') FROM t").rows == [
        ({"a": 1, "b": {"c": 2}, "n": None},)
    ]


def test_jsonb_strip_nulls(t, session):
    assert run(t, session, "SELECT jsonb_strip_nulls(data) FROM t").rows == [
        ({"a": 1, "b": {"c": 2}},)
    ]


def test_jsonb_delete_at_path_top(t, session):
    assert run(t, session, "SELECT data #- '{a}' FROM t").rows == [({"b": {"c": 2}, "n": None},)]


def test_jsonb_delete_at_path_nested(t, session):
    assert run(t, session, "SELECT data #- '{b,c}' FROM t").rows == [
        ({"a": 1, "b": {}, "n": None},)
    ]


def test_jsonb_object_keys(t, session):
    assert run(t, session, "SELECT jsonb_object_keys(data) FROM t ORDER BY 1").rows == [
        ("a",),
        ("b",),
        ("n",),
    ]


def test_jsonb_pretty(t, session):
    out = run(t, session, "SELECT jsonb_pretty(data) FROM t").rows[0][0]
    assert '"a": 1' in out and "\n" in out


def test_result_types(t, session):
    cols = run(
        t, session, "SELECT jsonb_set(data,'{a}','5') AS s, data #- '{a}' AS d FROM t"
    ).columns
    assert [c.type_tag for c in cols] == ["json", "json"]


def test_jsonb_functions_leave_source_unchanged(t, session):
    run(t, session, "SELECT jsonb_set(data, '{a}', '5') FROM t")
    # The stored row is untouched (the function returns a copy).
    assert run(t, session, "SELECT data FROM t").rows == [({"a": 1, "b": {"c": 2}, "n": None},)]


class TestJsonbConcat:
    """``jsonb || jsonb`` over two OBJECTS merges, right operand winning.

    This used to fall through to the text fallback, where ``str(dict)`` produced
    a **Python repr**: ``'{"x":1}'::jsonb || '{"y":2}'::jsonb`` answered
    ``{'x': 1}{'y': 2}`` — single-quoted, not valid JSON, and silently wrong.
    PG-probed 14.

    The mixed shapes (array||array, object||array, array||scalar) are a
    *separate, still-open* problem: their values are right but the result is
    typed as a PG array, so it renders ``{1,2,3}`` where PG renders the jsonb
    ``[1, 2, 3]``. See tasks/backlog.md — not asserted here so this class does
    not pin the wrong shape.
    """

    def test_two_objects_merge(self, t, session):
        assert run(
            t, session, """SELECT '{"a":1,"b":2}'::jsonb || '{"b":9,"c":3}'::jsonb"""
        ).rows == [({"a": 1, "b": 9, "c": 3},)]

    def test_right_operand_wins_on_conflict(self, t, session):
        assert run(t, session, """SELECT '{"k":"old"}'::jsonb || '{"k":"new"}'::jsonb""").rows == [
            ({"k": "new"},)
        ]

    def test_merging_an_empty_object_is_identity(self, t, session):
        assert run(t, session, """SELECT '{"a":1}'::jsonb || '{}'::jsonb""").rows == [({"a": 1},)]

    def test_a_column_merges_with_a_literal(self, t, session):
        assert run(t, session, """SELECT data || '{"z":9}'::jsonb FROM t""").rows == [
            ({"a": 1, "b": {"c": 2}, "n": None, "z": 9},)
        ]

    def test_plain_text_concatenation_is_untouched(self, t, session):
        # The jsonb branch must not capture ordinary `||`.
        assert run(t, session, "SELECT 'a' || 'b'").rows == [("ab",)]


class TestJsonbDelete:
    """``jsonb - key`` used to raise XX000 — an internal server error.

    The operator reached Python's ``-`` (`dict - str`) and the resulting bare
    TypeError escaped as "internal server error", the least actionable thing a
    server can return. Semantics PG-probed 14.

    Array results are correct in *value* but are still typed as a PG array, so
    they render `{1,3}` where PG renders the jsonb `[1, 3]` — the same open
    typing issue as `||`'s mixed shapes, tracked in tasks/backlog.md. These
    assertions therefore check the decoded value, not the rendered text.
    """

    def test_delete_key_from_object(self, t, session):
        assert run(t, session, """SELECT '{"a":1,"b":2}'::jsonb - 'a'""").rows == [({"b": 2},)]

    def test_deleting_a_missing_key_is_a_no_op(self, t, session):
        assert run(t, session, """SELECT '{"a":1}'::jsonb - 'zz'""").rows == [({"a": 1},)]

    def test_delete_several_keys(self, t, session):
        assert run(t, session, """SELECT '{"a":1,"b":2}'::jsonb - ARRAY['a','b']""").rows == [({},)]

    def test_delete_array_index(self, t, session):
        assert run(t, session, """SELECT '[1,2,3]'::jsonb - 1""").rows == [([1, 3],)]

    def test_a_negative_index_counts_from_the_end(self, t, session):
        assert run(t, session, """SELECT '[1,2,3]'::jsonb - -1""").rows == [([1, 2],)]

    def test_an_out_of_range_index_is_a_no_op(self, t, session):
        assert run(t, session, """SELECT '[1,2]'::jsonb - 9""").rows == [([1, 2],)]

    def test_delete_matching_string_elements_from_an_array(self, t, session):
        assert run(t, session, """SELECT '["a","b",1]'::jsonb - 'a'""").rows == [(["b", 1],)]

    def test_integer_index_against_an_object_is_rejected(self, t, session):
        with pytest.raises(Exception) as exc:
            run(t, session, """SELECT '{"a":1}'::jsonb - 1""")
        assert getattr(exc.value, "sqlstate", None) == "22023"


class TestUnsupportedOperatorIsTyped:
    """An operand pair with no operator answers 42883, not XX000.

    `'\\x01'::bytea + 1` used to die with `TypeError: can't concat int to bytes`
    out of `_eval_arith`, surfacing as an internal server error. PG names the
    problem: `operator does not exist: bytea + integer`.
    """

    def test_bytea_plus_integer(self, t, session):
        with pytest.raises(Exception) as exc:
            run(t, session, r"SELECT '\x01'::bytea + 1")
        assert getattr(exc.value, "sqlstate", None) == "42883"
        assert "operator does not exist" in str(exc.value)

    def test_ordinary_arithmetic_is_untouched(self, t, session):
        assert run(t, session, "SELECT 1 + 2").rows == [(3,)]
