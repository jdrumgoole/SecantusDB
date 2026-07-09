"""The SQL-standard ``MERGE`` statement.

For each source row, MERGE finds the target rows the ON condition matches, then
applies the first WHEN clause of the right kind (matched / not-matched) whose
optional ``AND`` condition holds: UPDATE / DELETE / DO NOTHING for a match,
INSERT / DO NOTHING for a non-match. Driven through ``run_sql`` over the real
WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE tgt (id bigint primary key, region text, amt int)")
    s.q("CREATE TABLE src (id bigint primary key, region text, amt int)")
    for i, r, a in [(1, "e", 10), (2, "e", 20), (3, "w", 30)]:
        s.q(f"INSERT INTO tgt (id, region, amt) VALUES ({i}, '{r}', {a})")
    try:
        yield s
    finally:
        s.close()


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


def test_merge_returning(storage):
    load_src(storage, [(2, "e", 200), (4, "n", 40)])
    res = storage.q(
        "MERGE INTO tgt t USING src s ON t.id = s.id "
        "WHEN MATCHED THEN UPDATE SET amt = s.amt "
        "WHEN NOT MATCHED THEN INSERT (id, region, amt) VALUES (s.id, s.region, s.amt) "
        "RETURNING t.id, t.amt"
    )
    assert res.command_tag == "MERGE 2"
    assert [c.name for c in res.columns] == ["id", "amt"]
    assert sorted(res.rows) == [(2, 200), (4, 40)]  # updated post-image + inserted row


def test_merge_returning_computed_and_star(storage):
    load_src(storage, [(1, "e", 5)])
    res = storage.q(
        "MERGE INTO tgt t USING src s ON t.id = s.id "
        "WHEN MATCHED THEN UPDATE SET amt = t.amt + s.amt "
        "RETURNING t.id, t.amt * 2 AS dbl"
    )
    assert res.rows == [(1, 30)]  # amt 10 + 5 = 15, doubled = 30


def test_when_not_matched_by_source_update(storage):
    # Target rows with no matching source row get the BY SOURCE action.
    load_src(storage, [(1, "e", 111)])  # matches only tgt id 1
    res = storage.q(
        "MERGE INTO tgt t USING src s ON t.id = s.id "
        "WHEN MATCHED THEN UPDATE SET amt = s.amt "
        "WHEN NOT MATCHED BY SOURCE THEN UPDATE SET amt = 0"
    )
    assert res.command_tag == "MERGE 3"  # id1 matched, id2/id3 by-source
    assert tgt(storage) == [(1, "e", 111), (2, "e", 0), (3, "w", 0)]


def test_when_not_matched_by_source_delete(storage):
    load_src(storage, [(2, "e", 20)])  # matches only tgt id 2
    res = storage.q(
        "MERGE INTO tgt t USING src s ON t.id = s.id WHEN NOT MATCHED BY SOURCE THEN DELETE"
    )
    assert res.command_tag == "MERGE 2"  # id1 and id3 deleted
    assert tgt(storage) == [(2, "e", 20)]


def test_by_source_returning(storage):
    load_src(storage, [(1, "e", 10)])
    res = storage.q(
        "MERGE INTO tgt t USING src s ON t.id = s.id "
        "WHEN NOT MATCHED BY SOURCE THEN DELETE "
        "RETURNING t.id, t.region"
    )
    assert sorted(res.rows) == [(2, "e"), (3, "w")]  # the deleted rows' pre-images
    assert tgt(storage) == [(1, "e", 10)]


def test_merge_cardinality_violation_on_multiple_source_matches(storage):
    # Two source rows both match target id=1 -> Postgres 21000 (a target row would
    # be affected twice).
    with pytest.raises(SQLError) as ei:
        storage.q(
            "MERGE INTO tgt t USING (SELECT 1 AS id UNION ALL SELECT 1) s "
            "ON t.id = s.id WHEN MATCHED THEN UPDATE SET amt = 999"
        )
    assert ei.value.sqlstate == "21000"


def test_merge_one_source_matching_many_targets_is_allowed(storage):
    # A single source row matching several target rows updates each once (not a
    # cardinality violation).
    storage.q(
        "MERGE INTO tgt t USING (SELECT 'e' AS region) s "
        "ON t.region = s.region WHEN MATCHED THEN UPDATE SET amt = 0"
    )
    assert tgt(storage) == [(1, "e", 0), (2, "e", 0), (3, "w", 30)]


def test_merge_returning_merge_action(storage):
    load_src(storage, [(2, "e", 200), (4, "n", 40)])  # 2 matches, 4 is new
    res = storage.q(
        "MERGE INTO tgt t USING src s ON t.id = s.id "
        "WHEN MATCHED THEN UPDATE SET amt = s.amt "
        "WHEN NOT MATCHED THEN INSERT (id, region, amt) VALUES (s.id, s.region, s.amt) "
        "RETURNING merge_action(), id"
    )
    assert sorted(res.rows) == [("INSERT", 4), ("UPDATE", 2)]
    assert [c.name for c in res.columns] == ["merge_action", "id"]


def test_merge_returning_action_for_delete(storage):
    load_src(storage, [(1, "e", 0), (4, "n", 40)])
    res = storage.q(
        "MERGE INTO tgt t USING src s ON t.id = s.id "
        "WHEN MATCHED THEN DELETE "
        "WHEN NOT MATCHED THEN INSERT (id, region, amt) VALUES (s.id, s.region, s.amt) "
        "RETURNING merge_action() AS act, id"
    )
    assert sorted(res.rows) == [("DELETE", 1), ("INSERT", 4)]


def test_merge_update_primary_key_rekeys(storage):
    # A MERGE UPDATE that changes the PK column re-keys the row (delete +
    # re-insert) rather than leaking an immutable-_id error (#164).
    load_src(storage, [(2, "e", 20)])
    storage.q("MERGE INTO tgt t USING src s ON t.id = s.id WHEN MATCHED THEN UPDATE SET id = 99")
    assert tgt(storage) == [(1, "e", 10), (3, "w", 30), (99, "e", 20)]


def test_merge_update_primary_key_collision_raises(storage):
    load_src(storage, [(2, "e", 20)])
    with pytest.raises(SQLError) as ei:
        storage.q(
            "MERGE INTO tgt t USING src s ON t.id = s.id "
            "WHEN MATCHED THEN UPDATE SET id = 1"  # collides with existing id 1
        )
    assert ei.value.sqlstate == "23505"


def test_merge_update_referenced_pk_restrict(storage):
    # A child row referencing the target's PK blocks a re-key (RESTRICT, 23503).
    storage.q("CREATE TABLE chld (cid bigint primary key, ref bigint references tgt(id))")
    storage.q("INSERT INTO chld (cid, ref) VALUES (100, 2)")
    load_src(storage, [(2, "e", 20)])
    with pytest.raises(SQLError) as ei:
        storage.q(
            "MERGE INTO tgt t USING src s ON t.id = s.id WHEN MATCHED THEN UPDATE SET id = 99"
        )
    assert ei.value.sqlstate == "23503"


def test_merge_update_referenced_pk_cascade(storage):
    storage.q(
        "CREATE TABLE chld (cid bigint primary key, "
        "ref bigint references tgt(id) ON UPDATE CASCADE)"
    )
    storage.q("INSERT INTO chld (cid, ref) VALUES (100, 2)")
    load_src(storage, [(2, "e", 20)])
    storage.q("MERGE INTO tgt t USING src s ON t.id = s.id WHEN MATCHED THEN UPDATE SET id = 99")
    assert storage.q("SELECT ref FROM chld").rows == [(99,)]


def test_merge_returning_source_column(storage):
    load_src(storage, [(2, "e", 200), (4, "n", 40)])
    res = storage.q(
        "MERGE INTO tgt t USING src s ON t.id = s.id "
        "WHEN MATCHED THEN UPDATE SET amt = s.amt "
        "WHEN NOT MATCHED THEN INSERT (id, region, amt) VALUES (s.id, s.region, s.amt) "
        "RETURNING merge_action() AS act, id, s.amt AS src_amt"
    )
    assert sorted(res.rows) == [("INSERT", 4, 40), ("UPDATE", 2, 200)]
