"""UNIQUE constraints hold against transactions that committed after you.

A UNIQUE constraint was enforced by probing for an existing row through the
caller's own session, whose snapshot is taken when its transaction begins. A
row another transaction committed after that point was invisible, so the
duplicate passed the check and was stored. Real PostgreSQL 14.13 rejects the
same sequence with 23505 (verified side by side) — a unique index is checked
against committed data even though your *reads* stay on your snapshot.

The autocommit path was never exposed (each statement is its own short
transaction, and a per-collection lock serialises probe-and-insert), which is
why `tests/test_pgserver_concurrency.py` passed throughout; these cover the
multi-statement transaction case it cannot reach.
"""

from __future__ import annotations

import psycopg
import pytest

from secantus.sql.pgserver import SecantusPGServer


@pytest.fixture()
def server(tmp_path):
    srv = SecantusPGServer(storage_path=str(tmp_path), port=0)
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture()
def dsn(server):
    host, port = server.address
    return f"host={host} port={port} dbname=test user=test password=test"


def _setup(dsn: str, ddl: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(ddl)


def _rows(dsn: str, sql: str):
    with psycopg.connect(dsn, autocommit=True) as c:
        return c.execute(sql).fetchone()


class TestCrossTransactionDuplicates:
    def test_value_committed_after_our_snapshot_is_rejected(self, dsn):
        _setup(dsn, "CREATE TABLE uq (id bigint primary key, val int unique)")
        holder = psycopg.connect(dsn)
        try:
            cur = holder.cursor()
            cur.execute("SELECT count(*) FROM uq")  # pins this transaction's snapshot
            with psycopg.connect(dsn, autocommit=True) as other:
                other.execute("INSERT INTO uq VALUES (1, 42)")
            with pytest.raises(psycopg.errors.UniqueViolation):
                cur.execute("INSERT INTO uq VALUES (2, 42)")
                holder.commit()
        finally:
            holder.close()
        assert _rows(dsn, "SELECT count(*), count(distinct val) FROM uq") == (1, 1)

    def test_multi_column_constraint(self, dsn):
        _setup(dsn, "CREATE TABLE uq2 (id bigint primary key, a int, b int, UNIQUE (a, b))")
        holder = psycopg.connect(dsn)
        try:
            cur = holder.cursor()
            cur.execute("SELECT count(*) FROM uq2")
            with psycopg.connect(dsn, autocommit=True) as other:
                other.execute("INSERT INTO uq2 VALUES (1, 7, 8)")
            with pytest.raises(psycopg.errors.UniqueViolation):
                cur.execute("INSERT INTO uq2 VALUES (2, 7, 8)")
                holder.commit()
        finally:
            holder.close()
        assert _rows(dsn, "SELECT count(*) FROM uq2") == (1,)

    def test_a_differing_value_still_inserts(self, dsn):
        """The committed probe must not reject rows that do not collide."""
        _setup(dsn, "CREATE TABLE uq3 (id bigint primary key, val int unique)")
        holder = psycopg.connect(dsn)
        try:
            cur = holder.cursor()
            cur.execute("SELECT count(*) FROM uq3")
            with psycopg.connect(dsn, autocommit=True) as other:
                other.execute("INSERT INTO uq3 VALUES (1, 42)")
            cur.execute("INSERT INTO uq3 VALUES (2, 43)")
            holder.commit()
        finally:
            holder.close()
        assert _rows(dsn, "SELECT count(*), count(distinct val) FROM uq3") == (2, 2)


class TestNoFalsePositives:
    def test_same_transaction_duplicate_still_rejected(self, dsn):
        """The transaction's own uncommitted row must still be seen — the
        committed probe alone would miss it."""
        _setup(dsn, "CREATE TABLE uq4 (id bigint primary key, val int unique)")
        conn = psycopg.connect(dsn)
        try:
            cur = conn.cursor()
            cur.execute("INSERT INTO uq4 VALUES (1, 5)")
            with pytest.raises(psycopg.errors.UniqueViolation):
                cur.execute("INSERT INTO uq4 VALUES (2, 5)")
        finally:
            conn.close()

    def test_delete_then_reinsert_within_a_transaction(self, dsn):
        """The deleted row is still committed as far as a committed-state probe
        is concerned, so this must consult the transaction's own view too."""
        _setup(dsn, "CREATE TABLE uq5 (id bigint primary key, val int unique)")
        with psycopg.connect(dsn, autocommit=True) as c:
            c.execute("INSERT INTO uq5 VALUES (1, 9)")
        conn = psycopg.connect(dsn)
        try:
            cur = conn.cursor()
            cur.execute("DELETE FROM uq5 WHERE val = 9")
            cur.execute("INSERT INTO uq5 VALUES (2, 9)")
            conn.commit()
        finally:
            conn.close()
        assert _rows(dsn, "SELECT count(*), count(distinct val) FROM uq5") == (1, 1)

    def test_update_keeping_its_own_value(self, dsn):
        """A row updated to the value it already holds does not conflict with
        itself."""
        _setup(dsn, "CREATE TABLE uq6 (id bigint primary key, val int unique, note text)")
        with psycopg.connect(dsn, autocommit=True) as c:
            c.execute("INSERT INTO uq6 VALUES (1, 3, 'a')")
        conn = psycopg.connect(dsn)
        try:
            conn.cursor().execute("UPDATE uq6 SET note = 'b' WHERE id = 1")
            conn.commit()
        finally:
            conn.close()
        assert _rows(dsn, "SELECT note FROM uq6 WHERE id = 1") == ("b",)

    def test_rolled_back_row_frees_its_value(self, dsn):
        _setup(dsn, "CREATE TABLE uq7 (id bigint primary key, val int unique)")
        conn = psycopg.connect(dsn)
        conn.cursor().execute("INSERT INTO uq7 VALUES (1, 11)")
        conn.rollback()
        conn.close()
        with psycopg.connect(dsn, autocommit=True) as c:
            c.execute("INSERT INTO uq7 VALUES (2, 11)")
        assert _rows(dsn, "SELECT count(*) FROM uq7") == (1,)
