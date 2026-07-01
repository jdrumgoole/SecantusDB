"""The SQL-standard ``MERGE`` statement.

For each source row, MERGE finds the target rows the ON condition matches, then
applies the first WHEN clause of the right kind (matched / not-matched) whose
optional ``AND`` condition holds: UPDATE / DELETE / DO NOTHING for a match,
INSERT / DO NOTHING for a non-match. Driven through ``run_sql`` over
``FakeStorage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from sqlfake import FakeStorage

DB = "testdb"


@pytest.fixture
def storage():
    s = FakeStorage()
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE tgt (id bigint primary key, region text, amt int)")
    s.q("CREATE TABLE src (id bigint primary key, region text, amt int)")
    for i, r, a in [(1, "e", 10), (2, "e", 20), (3, "w", 30)]:
        s.q(f"INSERT INTO tgt (id, region, amt) VALUES ({i}, '{r}', {a})")
    return s


def tgt(storage):
    return storage.q("SELECT id, region, amt FROM tgt ORDER BY id").rows


def load_src(storage, rows):
    for i, r, a in rows:
        storage.q(f"INSERT INTO src (id, region, amt) VALUES ({i}, '{r}', {a})")


def test_upsert_matched_update_not_matched_insert(storage):
    load_src(storage, [(2, "e", 200), (3, "w", 300), (4, "n", 40)])
    res = storage.q(
        "MERGE INTO tgt t USING src s ON t.id = s.id "
        "WHEN MATCHED THEN UPDATE SET amt = s.amt "
        "WHEN NOT MATCHED THEN INSERT (id, region, amt) VALUES (s.id, s.region, s.amt)"
    )
    assert res.command_tag == "MERGE 3"
    assert tgt(storage) == [(1, "e", 10), (2, "e", 200), (3, "w", 300), (4, "n", 40)]


def test_matched_delete(storage):
    load_src(storage, [(1, "e", 0), (2, "e", 20)])
    res = storage.q("MERGE INTO tgt t USING src s ON t.id = s.id WHEN MATCHED THEN DELETE")
    assert res.command_tag == "MERGE 2"
    assert tgt(storage) == [(3, "w", 30)]


def test_conditional_when_clauses(storage):
    # First matching WHEN wins: delete when amt=0, else accumulate.
    load_src(storage, [(1, "e", 0), (2, "e", 200), (5, "n", 50)])
    res = storage.q(
        "MERGE INTO tgt t USING src s ON t.id = s.id "
        "WHEN MATCHED AND s.amt = 0 THEN DELETE "
        "WHEN MATCHED THEN UPDATE SET amt = t.amt + s.amt "
        "WHEN NOT MATCHED THEN INSERT (id, region, amt) VALUES (s.id, s.region, s.amt)"
    )
    assert res.command_tag == "MERGE 3"
    # id1 deleted, id2 = 20+200, id3 untouched, id5 inserted.
    assert tgt(storage) == [(2, "e", 220), (3, "w", 30), (5, "n", 50)]


def test_subquery_source(storage):
    res = storage.q(
        "MERGE INTO tgt t USING (SELECT 3 AS id, 999 AS amt) s ON t.id = s.id "
        "WHEN MATCHED THEN UPDATE SET amt = s.amt "
        "WHEN NOT MATCHED THEN DO NOTHING"
    )
    assert res.command_tag == "MERGE 1"
    assert tgt(storage) == [(1, "e", 10), (2, "e", 20), (3, "w", 999)]


def test_not_matched_do_nothing(storage):
    load_src(storage, [(9, "z", 90)])
    res = storage.q("MERGE INTO tgt t USING src s ON t.id = s.id WHEN NOT MATCHED THEN DO NOTHING")
    assert res.command_tag == "MERGE 0"
    assert tgt(storage) == [(1, "e", 10), (2, "e", 20), (3, "w", 30)]


def test_no_matching_when_leaves_row_untouched(storage):
    # Only a NOT MATCHED clause: matched source rows fall through with no action.
    load_src(storage, [(1, "e", 111), (7, "q", 70)])
    res = storage.q(
        "MERGE INTO tgt t USING src s ON t.id = s.id "
        "WHEN NOT MATCHED THEN INSERT (id, region, amt) VALUES (s.id, s.region, s.amt)"
    )
    assert res.command_tag == "MERGE 1"  # only id 7 inserted; id 1 matched → no clause
    assert tgt(storage) == [(1, "e", 10), (2, "e", 20), (3, "w", 30), (7, "q", 70)]


def test_insert_without_column_list(storage):
    load_src(storage, [(8, "h", 80)])
    res = storage.q(
        "MERGE INTO tgt t USING src s ON t.id = s.id "
        "WHEN NOT MATCHED THEN INSERT VALUES (s.id, s.region, s.amt)"
    )
    assert res.command_tag == "MERGE 1"
    assert (8, "h", 80) in tgt(storage)
