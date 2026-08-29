"""A SQL UNIQUE constraint is backed by a storage unique index.

It used to be upheld only by ``_validate_unique_rows``, a probe read before
writing. That probe reads the writer's own snapshot, so it could not see a
value another transaction had just committed, nor one a second writer was
inserting concurrently — both stored duplicates. A storage unique index makes
WiredTiger the arbiter.

Two SQL rules the index has to respect, and neither is what a Mongo unique
index does by default:

* NULLs are distinct — any number satisfy a UNIQUE constraint. A partial filter
  excludes them (sparse would not: a SQL NULL is an explicit null, not a
  missing field).
* A multi-column constraint is unconstrained if ANY of its columns is NULL.

DEFERRABLE constraints are deliberately left to the old check: they may be
violated transiently inside a transaction and are judged only at COMMIT.
"""

from __future__ import annotations

import threading

import psycopg
import pytest

from secantus.sql.engine import run_sql
from secantus.sql.pgserver import SecantusPGServer
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def q(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return run_sql(storage, "t", sql, session=session)[0]

    run.storage = storage  # type: ignore[attr-defined]
    try:
        yield run
    finally:
        storage.close()


def _sqlstate(fn) -> str | None:
    try:
        fn()
    except Exception as exc:  # noqa: BLE001
        return getattr(exc, "sqlstate", None)
    return None


class TestIndexIsCreated:
    def test_create_table_backs_the_constraint(self, q):
        q("CREATE TABLE u (id int primary key, v int unique)")
        names = [i["name"] for i in q.storage.list_indexes("t", "u")]
        assert "u_v_key" in names

    def test_alter_add_constraint(self, q):
        q("CREATE TABLE u (id int primary key, a int, b int)")
        q("ALTER TABLE u ADD CONSTRAINT u_ab UNIQUE (a, b)")
        assert "u_ab" in [i["name"] for i in q.storage.list_indexes("t", "u")]

    def test_alter_drop_constraint_removes_it(self, q):
        q("CREATE TABLE u (id int primary key, a int)")
        q("ALTER TABLE u ADD CONSTRAINT u_a UNIQUE (a)")
        q("ALTER TABLE u DROP CONSTRAINT u_a")
        assert "u_a" not in [i["name"] for i in q.storage.list_indexes("t", "u")]
        q("INSERT INTO u VALUES (1, 5)")
        q("INSERT INTO u VALUES (2, 5)")  # no longer constrained

    def test_deferrable_constraint_is_not_backed(self, q):
        """Deferred constraints must tolerate a transient violation, so they
        keep the commit-time check instead of an index enforcing every write."""
        q("CREATE TABLE u (id int primary key, v int UNIQUE DEFERRABLE INITIALLY DEFERRED)")
        assert [i["name"] for i in q.storage.list_indexes("t", "u")] == ["_id_"]


class TestSqlSemantics:
    def test_duplicate_rejected(self, q):
        q("CREATE TABLE u (id int primary key, v int unique)")
        q("INSERT INTO u VALUES (1, 5)")
        assert _sqlstate(lambda: q("INSERT INTO u VALUES (2, 5)")) == "23505"

    def test_many_nulls_allowed(self, q):
        q("CREATE TABLE u (id int primary key, v int unique)")
        q("INSERT INTO u VALUES (1, NULL)")
        q("INSERT INTO u VALUES (2, NULL)")
        assert q("SELECT count(*) FROM u").rows == [(2,)]

    def test_multi_column_with_one_null_is_unconstrained(self, q):
        q("CREATE TABLE u (id int primary key, a int, b int, UNIQUE (a, b))")
        q("INSERT INTO u VALUES (1, 1, NULL)")
        q("INSERT INTO u VALUES (2, 1, NULL)")
        assert q("SELECT count(*) FROM u").rows == [(2,)]

    def test_multi_column_full_duplicate_rejected(self, q):
        q("CREATE TABLE u (id int primary key, a int, b int, UNIQUE (a, b))")
        q("INSERT INTO u VALUES (1, 1, 2)")
        assert _sqlstate(lambda: q("INSERT INTO u VALUES (2, 1, 2)")) == "23505"

    def test_delete_frees_the_value(self, q):
        q("CREATE TABLE u (id int primary key, v int unique)")
        q("INSERT INTO u VALUES (1, 5)")
        q("DELETE FROM u WHERE id = 1")
        q("INSERT INTO u VALUES (2, 5)")
        assert q("SELECT count(*) FROM u").rows == [(1,)]


class TestTheHolesThroughTheWire:
    @pytest.fixture()
    def dsn(self, tmp_path):
        srv = SecantusPGServer(storage_path=str(tmp_path), port=0)
        srv.start()
        host, port = srv.address
        conn = f"host={host} port={port} dbname=test user=test password=test"
        with psycopg.connect(conn, autocommit=True) as c:
            c.execute("CREATE TABLE uq (id bigint primary key, val int unique)")
        try:
            yield conn
        finally:
            srv.stop()

    def test_value_committed_after_our_snapshot(self, dsn):
        holder = psycopg.connect(dsn)
        try:
            cur = holder.cursor()
            cur.execute("INSERT INTO uq VALUES (100, 500)")  # write first
            with psycopg.connect(dsn, autocommit=True) as other:
                other.execute("INSERT INTO uq VALUES (1, 42)")
            with pytest.raises(psycopg.Error):
                cur.execute("INSERT INTO uq VALUES (2, 42)")
                holder.commit()
        finally:
            holder.close()
        with psycopg.connect(dsn, autocommit=True) as c:
            n, d = c.execute("SELECT count(*), count(DISTINCT val) FROM uq").fetchone()
        assert n == d, "a duplicate was stored"

    def test_simultaneous_transactions(self, dsn):
        workers = 8
        barrier = threading.Barrier(workers)
        wins = [0] * workers

        def run(i: int) -> None:
            with psycopg.connect(dsn) as c:
                cur = c.cursor()
                barrier.wait()
                try:
                    cur.execute("INSERT INTO uq VALUES (%s, 777)", (2000 + i,))
                    c.commit()
                    wins[i] = 1
                except psycopg.Error:
                    c.rollback()

        threads = [threading.Thread(target=run, args=(i,)) for i in range(workers)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        assert sum(wins) == 1, "exactly one transaction may win"
        with psycopg.connect(dsn, autocommit=True) as c:
            n, d = c.execute(
                "SELECT count(*), count(DISTINCT val) FROM uq WHERE val IS NOT NULL"
            ).fetchone()
        assert n == d, "duplicates were stored"
