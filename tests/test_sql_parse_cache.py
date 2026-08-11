"""The SQL parse cache: repeated statement text parses once, never aliases.

``planner.parse`` caches on SECOND sight (first occurrence only marks), hands
out fresh copies on every hit, and keeps its pristine trees private — so a
downstream consumer that mutates its parse result can never poison a later
parse of the same text. Same-venv benchmark: +26% on repeated-text embedded
statements, no measurable cost on unique-text workloads.
"""

from __future__ import annotations

import pytest
from sqlglot import exp

from secantus.sql import errors, planner


@pytest.fixture(autouse=True)
def fresh_cache():
    with planner._PARSE_CACHE_LOCK:
        planner._PARSE_CACHE.clear()
    yield
    with planner._PARSE_CACHE_LOCK:
        planner._PARSE_CACHE.clear()


SQL = "SELECT id, v FROM t WHERE id = $1 ORDER BY id"


def test_second_sight_caches_and_hits_are_fresh_copies():
    first = planner.parse(SQL)
    assert planner._PARSE_CACHE[SQL] is None  # first sight: marker only
    second = planner.parse(SQL)
    assert planner._PARSE_CACHE[SQL] is not None  # second sight: cached
    third = planner.parse(SQL)
    assert [repr(s) for s in first] == [repr(s) for s in second] == [repr(s) for s in third]
    assert third[0] is not second[0]  # every call owns its trees
    assert third[0] is not planner._PARSE_CACHE[SQL][0]  # pristine stays private


def test_mutating_a_result_cannot_poison_later_parses():
    planner.parse(SQL)
    mine = planner.parse(SQL)  # cached from here on
    for ident in mine[0].find_all(exp.Identifier):
        ident.set("this", "HACKED")
    clean = planner.parse(SQL)
    assert "HACKED" not in repr(clean[0])


def test_multi_statement_text_caches_the_full_list():
    sql = "INSERT INTO t (id) VALUES (1); INSERT INTO t (id) VALUES (2)"
    first = planner.parse(sql)
    second = planner.parse(sql)
    assert len(first) == len(second) == 2
    assert [repr(s) for s in first] == [repr(s) for s in second]


def test_eviction_keeps_the_cache_bounded(monkeypatch):
    monkeypatch.setattr(planner, "_PARSE_CACHE_MAX", 8)
    for i in range(50):
        planner.parse(f"SELECT {i} FROM t")
    assert len(planner._PARSE_CACHE) <= 8


def test_parse_errors_are_not_cached():
    with pytest.raises(errors.SQLError):
        planner.parse("SELEKT nope FROM")
    assert "SELEKT nope FROM" not in planner._PARSE_CACHE
    with pytest.raises(errors.SQLError):
        planner.parse("SELEKT nope FROM")
