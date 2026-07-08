"""hstore key/value type (#117): literals, the -> / @> / <@ / ? / ?& / ?| / ||
operators, akeys / avals / hstore_to_json / defined / delete, and column
round-trips / WHERE routing.
"""

from __future__ import annotations

import pytest

from secantus.sql import hstore, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


# --------------------------------------------------------------------------- #
# Pure hstore.py
# --------------------------------------------------------------------------- #


def test_parse_and_render():
    assert hstore.render(hstore.parse("a=>1, b=>2")) == '"a"=>"1", "b"=>"2"'
    assert hstore.render(hstore.parse('"k"=>"v"')) == '"k"=>"v"'


def test_parse_quoted_with_commas_and_null():
    m = hstore.as_map(hstore.parse('"key one"=>"v, w", k2=>NULL'))
    assert m == {"key one": "v, w", "k2": None}


def test_parse_rejects_null_key():
    with pytest.raises(hstore.HstoreError):
        hstore.parse("NULL=>1")


def test_parse_rejects_missing_arrow():
    with pytest.raises(hstore.HstoreError):
        hstore.parse("a 1")


def test_is_hstore():
    assert hstore.is_hstore(hstore.parse("a=>1")) is True
    assert hstore.is_hstore({"a": 1}) is False
    assert hstore.is_hstore("a=>1") is False


def test_contains_and_contained_by():
    a = hstore.parse("a=>1, b=>2")
    assert hstore.contains(a, hstore.parse("a=>1")) is True
    assert hstore.contains(a, hstore.parse("a=>9")) is False
    assert hstore.contained_by(hstore.parse("a=>1"), a) is True


def test_exists_variants():
    a = hstore.parse("a=>1, b=>2")
    assert hstore.exists(a, "a") is True
    assert hstore.exists(a, "z") is False
    assert hstore.exists_all(a, ["a", "b"]) is True
    assert hstore.exists_all(a, ["a", "z"]) is False
    assert hstore.exists_any(a, ["z", "b"]) is True


def test_lookup_and_defined():
    a = hstore.parse("a=>1, b=>NULL")
    assert hstore.lookup(a, "a") == "1"
    assert hstore.lookup(a, "b") is None
    assert hstore.lookup(a, "z") is None
    assert hstore.defined(a, "a") is True
    assert hstore.defined(a, "b") is False  # present but NULL
    assert hstore.defined(a, "z") is False


def test_merge_and_delete():
    assert hstore.as_map(hstore.merge(hstore.parse("a=>1"), hstore.parse("a=>9, b=>2"))) == {
        "a": "9",
        "b": "2",
    }
    assert hstore.as_map(hstore.delete(hstore.parse("a=>1, b=>2"), "a")) == {"b": "2"}


def test_akeys_avals_to_json():
    a = hstore.parse("a=>1, b=>2")
    assert hstore.akeys(a) == ["a", "b"]
    assert hstore.avals(a) == ["1", "2"]
    assert hstore.to_json(a) == {"a": "1", "b": "2"}


# --------------------------------------------------------------------------- #
# SQL surface
# --------------------------------------------------------------------------- #


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


def val(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


def col(storage, session, sql):
    return run(storage, session, sql).columns[0]


def test_cast_typed(storage, session):
    assert col(storage, session, "SELECT 'a=>1'::hstore").type_tag == "hstore"


def test_cast_canonicalises(storage, session):
    v = val(storage, session, "SELECT 'a=>1, b=>2'::hstore")
    assert hstore.render(v) == '"a"=>"1", "b"=>"2"'


def test_lookup_op_typed_text(storage, session):
    assert col(storage, session, "SELECT 'a=>1'::hstore -> 'a'").type_tag == "text"


def test_lookup_op(storage, session):
    assert val(storage, session, "SELECT 'a=>1'::hstore -> 'a'") == "1"
    assert val(storage, session, "SELECT 'a=>1'::hstore -> 'x'") is None


def test_contains_typed_bool(storage, session):
    assert col(storage, session, "SELECT 'a=>1,b=>2'::hstore @> 'a=>1'").type_tag == "bool"


def test_contains_op(storage, session):
    assert val(storage, session, "SELECT 'a=>1,b=>2'::hstore @> 'a=>1'") is True
    assert val(storage, session, "SELECT 'a=>1,b=>2'::hstore @> 'a=>9'") is False
    assert val(storage, session, "SELECT 'a=>1'::hstore <@ 'a=>1,b=>2'") is True


def test_exists_ops(storage, session):
    assert val(storage, session, "SELECT 'a=>1,b=>2'::hstore ? 'a'") is True
    assert val(storage, session, "SELECT 'a=>1,b=>2'::hstore ? 'z'") is False
    assert val(storage, session, "SELECT 'a=>1,b=>2'::hstore ?& ARRAY['a','b']") is True
    assert val(storage, session, "SELECT 'a=>1,b=>2'::hstore ?| ARRAY['z','b']") is True


def test_merge_op(storage, session):
    v = val(storage, session, "SELECT 'a=>1'::hstore || 'b=>2'::hstore")
    assert hstore.as_map(v) == {"a": "1", "b": "2"}
    assert col(storage, session, "SELECT 'a=>1'::hstore || 'b=>2'::hstore").type_tag == "hstore"


def test_functions(storage, session):
    assert val(storage, session, "SELECT akeys('a=>1,b=>2'::hstore)") == ["a", "b"]
    assert val(storage, session, "SELECT avals('a=>1,b=>2'::hstore)") == ["1", "2"]
    assert val(storage, session, "SELECT hstore_to_json('a=>1'::hstore)") == {"a": "1"}
    assert val(storage, session, "SELECT defined('a=>1,b=>NULL'::hstore, 'b')") is False
    assert hstore.as_map(val(storage, session, "SELECT hstore('k', 'v')")) == {"k": "v"}


@pytest.fixture
def items(storage, session):
    run(storage, session, "CREATE TABLE items (id int PRIMARY KEY, attrs hstore)")
    run(storage, session, "INSERT INTO items VALUES (1, 'color=>red, size=>big')")
    run(storage, session, "INSERT INTO items VALUES (2, 'color=>blue, size=>small')")
    return storage


def test_column_roundtrip(items, session):
    v = val(items, session, "SELECT attrs FROM items WHERE id = 1")
    assert hstore.as_map(v) == {"color": "red", "size": "big"}


def test_column_typed(items, session):
    assert col(items, session, "SELECT attrs FROM items WHERE id = 1").type_tag == "hstore"


def test_where_contains(items, session):
    ids = [
        r[0] for r in run(items, session, "SELECT id FROM items WHERE attrs @> 'color=>red'").rows
    ]
    assert ids == [1]


def test_where_key_exists(items, session):
    ids = [
        r[0]
        for r in run(items, session, "SELECT id FROM items WHERE attrs ? 'size' ORDER BY id").rows
    ]
    assert ids == [1, 2]


def test_where_lookup_pushdown(items, session):
    ids = [
        r[0]
        for r in run(items, session, "SELECT id FROM items WHERE attrs -> 'color' = 'blue'").rows
    ]
    assert ids == [2]


def test_select_lookup_column(items, session):
    v = val(items, session, "SELECT attrs -> 'color' FROM items WHERE id = 1")
    assert v == "red"
