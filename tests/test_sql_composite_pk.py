"""Composite (multi-column) PRIMARY KEY support.

A composite PK ``(a, b)`` maps to a MongoDB subdocument ``_id: {a: va, b: vb}`` —
so uniqueness rides the storage layer's ``_id`` index exactly as a single-column
PK does. Each PK column reads/writes through the dotted field ``_id.<name>``; the
``_id`` subdocument's key order is canonicalized to the PK declaration order so
equality is independent of the INSERT's column order.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
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


def sqlstate(storage, session, sql):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, sql)
    return ei.value.sqlstate


@pytest.fixture
def t(storage, session):
    run(storage, session, "CREATE TABLE t (a int, b text, n int, PRIMARY KEY (a, b))")
    run(storage, session, "INSERT INTO t VALUES (1,'x',10),(1,'y',20),(2,'z',30)")
    return storage


# -- create / store ----------------------------------------------------------- #


def test_create_and_select(t, session):
    assert run(t, session, "SELECT a, b, n FROM t ORDER BY a, b").rows == [
        (1, "x", 10),
        (1, "y", 20),
        (2, "z", 30),
    ]


def test_id_is_ordered_subdocument(t, session):
    # The stored _id is a subdocument keyed in PK declaration order.
    docs = t.find_matching(DB, "t", {})
    ids = sorted((d["_id"] for d in docs), key=lambda d: (d["a"], d["b"]))
    assert ids[0] == {"a": 1, "b": "x"}
    assert list(ids[0].keys()) == ["a", "b"]


def test_insert_column_order_independent(storage, session):
    run(storage, session, "CREATE TABLE t (a int, b text, PRIMARY KEY (a, b))")
    run(storage, session, "INSERT INTO t (b, a) VALUES ('z', 2)")  # reversed order
    doc = storage.find_matching(DB, "t", {})[0]
    assert doc["_id"] == {"a": 2, "b": "z"}  # still canonical (a, b) order


# -- uniqueness --------------------------------------------------------------- #


def test_duplicate_composite_pk_rejected(t, session):
    assert sqlstate(t, session, "INSERT INTO t VALUES (1,'x',99)") == "23505"


def test_partial_key_overlap_is_allowed(t, session):
    # Same a, different b — distinct composite key, no conflict.
    assert run(t, session, "INSERT INTO t VALUES (1,'w',40)").rowcount == 1


# -- query -------------------------------------------------------------------- #


def test_where_full_key(t, session):
    assert run(t, session, "SELECT n FROM t WHERE a=1 AND b='y'").rows == [(20,)]


def test_where_partial_key(t, session):
    assert run(t, session, "SELECT b FROM t WHERE a=1 ORDER BY b").rows == [("x",), ("y",)]


# -- update / delete ---------------------------------------------------------- #


def test_update_non_pk_column(t, session):
    run(t, session, "UPDATE t SET n=99 WHERE a=1 AND b='x'")
    assert run(t, session, "SELECT n FROM t WHERE a=1 AND b='x'").rows == [(99,)]


def test_update_pk_column_rekeys(t, session):
    # Updating a PK column re-keys the row (delete + re-insert under the new _id).
    assert run(t, session, "UPDATE t SET a=5 WHERE a=1 AND b='y'").command_tag == "UPDATE 1"
    assert run(t, session, "SELECT a, b, n FROM t ORDER BY a, b").rows == [
        (1, "x", 10),
        (2, "z", 30),
        (5, "y", 20),
    ]


def test_update_pk_and_non_pk_together(t, session):
    run(t, session, "UPDATE t SET b='q', n=99 WHERE a=1 AND b='x'")
    assert run(t, session, "SELECT a, b, n FROM t WHERE a=1 ORDER BY b").rows == [
        (1, "q", 99),
        (1, "y", 20),
    ]


def test_update_pk_collision_is_rejected_and_atomic(t, session):
    # Re-keying (2,'z') to the existing (1,'x') violates the PK.
    assert sqlstate(t, session, "UPDATE t SET a=1, b='x' WHERE a=2 AND b='z'") == "23505"
    # The table is unchanged (statement-atomic).
    assert run(t, session, "SELECT a, b, n FROM t ORDER BY a, b").rows == [
        (1, "x", 10),
        (1, "y", 20),
        (2, "z", 30),
    ]


def test_update_pk_returning(t, session):
    assert run(t, session, "UPDATE t SET a=9 WHERE a=2 AND b='z' RETURNING a, b, n").rows == [
        (9, "z", 30)
    ]


def test_delete_by_full_key(t, session):
    run(t, session, "DELETE FROM t WHERE a=2 AND b='z'")
    assert run(t, session, "SELECT count(*) FROM t").rows == [(2,)]


# -- upsert / merge ----------------------------------------------------------- #


def test_on_conflict_do_update(t, session):
    run(
        t,
        session,
        "INSERT INTO t VALUES (1,'x',77) ON CONFLICT (a,b) DO UPDATE SET n = EXCLUDED.n",
    )
    assert run(t, session, "SELECT n FROM t WHERE a=1 AND b='x'").rows == [(77,)]


def test_on_conflict_do_nothing(t, session):
    assert run(t, session, "INSERT INTO t VALUES (1,'x',88) ON CONFLICT DO NOTHING").rowcount == 0
    assert run(t, session, "SELECT n FROM t WHERE a=1 AND b='x'").rows == [(10,)]


def test_merge_matched_and_not_matched(storage, session):
    run(storage, session, "CREATE TABLE t (a int, b text, n int, PRIMARY KEY (a, b))")
    run(storage, session, "INSERT INTO t VALUES (1,'x',10)")
    run(storage, session, "CREATE TABLE src (a int, b text, n int)")
    run(storage, session, "INSERT INTO src VALUES (1,'x',100),(3,'w',300)")
    run(
        storage,
        session,
        "MERGE INTO t USING src ON t.a=src.a AND t.b=src.b "
        "WHEN MATCHED THEN UPDATE SET n=src.n "
        "WHEN NOT MATCHED THEN INSERT (a,b,n) VALUES (src.a, src.b, src.n)",
    )
    assert run(storage, session, "SELECT a,b,n FROM t ORDER BY a,b").rows == [
        (1, "x", 100),
        (3, "w", 300),
    ]


def test_returning_pk_columns(t, session):
    assert run(t, session, "INSERT INTO t VALUES (7,'q',50) RETURNING a, b").rows == [(7, "q")]


# -- reflection --------------------------------------------------------------- #


def test_pg_index_lists_all_pk_columns(t, session):
    assert run(t, session, "SELECT indkey FROM pg_catalog.pg_index").rows == [([1, 2],)]


def test_pg_constraint_conkey(t, session):
    rows = run(
        t, session, "SELECT conname, conkey FROM pg_catalog.pg_constraint WHERE contype='p'"
    ).rows
    assert rows == [("t_pkey", [1, 2])]


def test_key_column_usage(t, session):
    rows = run(
        t,
        session,
        "SELECT column_name, ordinal_position FROM information_schema.key_column_usage "
        "WHERE table_name='t' ORDER BY ordinal_position",
    ).rows
    assert rows == [("a", 1), ("b", 2)]


# -- single PK still works ---------------------------------------------------- #


def test_single_pk_unaffected(storage, session):
    run(storage, session, "CREATE TABLE s (id int PRIMARY KEY, v text)")
    run(storage, session, "INSERT INTO s VALUES (1,'a')")
    assert run(storage, session, "SELECT id, v FROM s").rows == [(1, "a")]
    assert storage.find_matching(DB, "s", {})[0]["_id"] == 1  # scalar _id, not a subdoc
    assert sqlstate(storage, session, "INSERT INTO s VALUES (1,'b')") == "23505"


def test_single_column_pk_rekey(storage, session):
    run(storage, session, "CREATE TABLE s (id int PRIMARY KEY, v int)")
    run(storage, session, "INSERT INTO s VALUES (1, 10), (2, 20)")
    assert run(storage, session, "UPDATE s SET id = 5 WHERE id = 1").command_tag == "UPDATE 1"
    assert run(storage, session, "SELECT id, v FROM s ORDER BY id").rows == [(2, 20), (5, 10)]
    # collision with an existing key
    assert sqlstate(storage, session, "UPDATE s SET id = 2 WHERE id = 5") == "23505"
