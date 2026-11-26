"""Gap tests of divergences found while working RP-693 (a `bpchar` key
keyed with its trailing blanks trimmed):

- COPY in autocommit stamped its rows with an xid it never completed, so
  the next statement ran under the same xid and an ON CONFLICT DO UPDATE
  over a copied row was refused 21000 as a row "this command" wrote
  (RP-1069, fixed). Inside a block the same refusal came from the command
  id: the COPY's was advanced when the statement returned at
  CopyInResponse, before its rows loaded, so they took the next
  statement's.
- EXPLAIN showed a Seq Scan for an indexed `=` ANDed with another filter
  (RP-1070). A text key's unique `=` is fixed; a non-unique text index, a
  `bpchar` key and an expression index remain.

The expected answers were measured on PostgreSQL 15.19 (2026-11-18), on
the same statements over a scratch table; the in-block command ids on a
fresh private 15.19 cluster (2026-11-25); RP-1070's remaining plans on a
fresh private 15.20 cluster in C.UTF-8 (2026-11-26).
"""

from __future__ import annotations

import uuid
from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


@pytest.mark.backlog_fixed("RP-1069")
def test_copy_completes_its_own_xid(dsn: str) -> None:
    """An autocommit COPY completed its xid at CopyDone, as 15.19's does."""
    table = f"copy_xid_{uuid.uuid4().hex[:8]}"
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(f"create table {table} (a int unique, b int)")
        try:
            with c.cursor().copy(f"copy {table} from stdin") as cp:
                cp.write("1\t1\n")
            xmin = c.execute(f"select xmin::text::bigint from {table}").fetchone()
            current = c.execute("select txid_current()").fetchone()
            # 15.19: the next statement's xid is past the COPY's.
            assert xmin is not None and current is not None and current[0] > xmin[0]
            c.execute(f"truncate {table}")
            with c.cursor().copy(f"copy {table} from stdin") as cp:
                cp.write("1\t1\n")
            try:
                answer: object = c.execute(
                    f"insert into {table} values (1, 2) on conflict (a) "
                    "do update set b = excluded.b returning b"
                ).fetchall()
            except psycopg.Error as e:
                answer = (e.sqlstate, e.diag.message_primary)
            assert answer == [(2,)]
        finally:
            c.execute(f"drop table {table}")


@pytest.mark.backlog_fixed("RP-1069")
def test_copy_in_a_block_uses_its_own_command_id(dsn: str) -> None:
    """Inside a block a COPY's rows took the COPY's command id and the
    next statement the one after it, as 15.19's do; a failed autocommit
    COPY that had loaded rows used up an xid of its own."""
    table = f"copy_cid_{uuid.uuid4().hex[:8]}"
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(f"create table {table} (a int unique, b int)")
        try:
            with c.transaction():
                c.execute(f"insert into {table} values (5, 5)")
                with c.cursor().copy(f"copy {table} from stdin") as cp:
                    cp.write("1\t1\n")
                c.execute(f"insert into {table} values (6, 6)")
                upsert = c.execute(
                    f"insert into {table} values (1, 2) on conflict (a) "
                    "do update set b = excluded.b returning b"
                ).fetchall()
                c.execute(f"insert into {table} values (7, 7)")
                cmins = c.execute(f"select a, cmin::text from {table} order by a").fetchall()
            # 15.19: INSERT 0, COPY 1, INSERT 2, the upsert 3, INSERT 4.
            assert upsert == [(2,)]
            assert cmins == [(1, "3"), (5, "0"), (6, "2"), (7, "4")]
            before = c.execute("select txid_current()").fetchone()
            with (
                pytest.raises(psycopg.errors.UniqueViolation),
                c.cursor().copy(f"copy {table} from stdin") as cp,
            ):
                cp.write("2\t2\n1\t1\n")
            after = c.execute("select txid_current()").fetchone()
            assert before is not None and after is not None
            # 15.19: the failed COPY's xid lies between the two.
            assert after[0] - before[0] == 2
            assert c.execute(f"select count(*) from {table}").fetchone() == (4,)
        finally:
            c.execute(f"drop table {table}")


@pytest.mark.backlog_fixed("RP-1070")
def test_explain_indexed_equality_with_a_filter(dsn: str) -> None:
    """An `=` on a text primary key ANDed with another filter was an Index
    Scan with the other clause its Filter, as on 15.19: the cost model
    priced the one-row fetch without the text key's correlation."""
    table = f"ix_and_{uuid.uuid4().hex[:8]}"
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(f"create table {table} (a text primary key, b int)")
        try:
            c.execute(f"insert into {table} select 'x'||i, i from generate_series(1, 2000) i")
            c.execute(f"analyze {table}")
            plan = [
                r[0]
                for r in c.execute(
                    f"explain (costs off) select b from {table} where a = 'x1' and b < 0"
                ).fetchall()
            ]
            # 15.19.
            assert plan == [
                f"Index Scan using {table}_pkey on {table}",
                "  Index Cond: (a = 'x1'::text)",
                "  Filter: (b < 0)",
            ]
        finally:
            c.execute(f"drop table {table}")


def _plan(dsn: str, setup: list[str], query: str, drop: str) -> list[str]:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            for stmt in setup:
                c.execute(stmt)
            return [r[0] for r in c.execute(f"explain (costs off) {query}").fetchall()]
        finally:
            c.execute(f"drop table {drop}")


@pytest.mark.backlog("RP-1070")
def test_explain_non_unique_text_index_with_a_filter(dsn: str) -> None:
    """A non-unique text index fetching 20 rows: the scan's cost turns on
    the column's correlation, which the model does not read for text.
    Measured in C.UTF-8 (this server's collation); the correlation, and so
    the plan, depend on the locale."""
    t = f"ix_nu_{uuid.uuid4().hex[:8]}"
    plan = _plan(
        dsn,
        [
            f"create table {t} (t text, b int)",
            f"create index {t}_t on {t}(t)",
            f"insert into {t} select 'x'||(i%100), i from generate_series(1, 2000) i",
            f"analyze {t}",
        ],
        f"select b from {t} where t = 'x1' and b < 0",
        t,
    )
    # 15.20, C.UTF-8.
    assert plan == [
        f"Bitmap Heap Scan on {t}",
        "  Recheck Cond: (t = 'x1'::text)",
        "  Filter: (b < 0)",
        f"  ->  Bitmap Index Scan on {t}_t",
        "        Index Cond: (t = 'x1'::text)",
    ]


@pytest.mark.backlog("RP-1070")
def test_explain_bpchar_key_with_a_filter(dsn: str) -> None:
    """A `char(5)` primary key: the WHERE's estimate reads no statistics
    for a bpchar constant, so the scan is unpriced (`cost=0.00..0.00
    rows=0 width=0` for the `=` alone) and ANDed with a filter a Seq Scan."""
    t = f"ix_bp_{uuid.uuid4().hex[:8]}"
    plan = _plan(
        dsn,
        [
            f"create table {t} (c char(5) primary key, b int)",
            f"insert into {t} select 'x'||i, i from generate_series(1, 2000) i",
            f"analyze {t}",
        ],
        f"select b from {t} where c = 'x1' and b < 0",
        t,
    )
    # 15.20.
    assert plan == [
        f"Index Scan using {t}_pkey on {t}",
        "  Index Cond: (c = 'x1'::bpchar)",
        "  Filter: (b < 0)",
    ]


@pytest.mark.backlog("RP-1070")
def test_explain_expression_index_with_a_filter(dsn: str) -> None:
    """An index on `lower(t)`: the `=` alone is printed as the scan's
    Filter rather than its Index Cond, and ANDed with a filter the scan is
    a Seq Scan (ANALYZE keeps no statistics for the index's expression)."""
    t = f"ix_ex_{uuid.uuid4().hex[:8]}"
    setup = [
        f"create table {t} (a int, b int, t text)",
        f"create index {t}_l on {t}(lower(t))",
        f"insert into {t} select i, i, 'x'||i from generate_series(1, 2000) i",
        f"analyze {t}",
    ]
    alone = _plan(dsn, setup, f"select b from {t} where lower(t) = 'x1'", t)
    anded = _plan(dsn, setup, f"select b from {t} where lower(t) = 'x1' and b < 0", t)
    # 15.20.
    assert alone == [
        f"Index Scan using {t}_l on {t}",
        "  Index Cond: (lower(t) = 'x1'::text)",
    ]
    assert anded == [
        f"Index Scan using {t}_l on {t}",
        "  Index Cond: (lower(t) = 'x1'::text)",
        "  Filter: (b < 0)",
    ]
