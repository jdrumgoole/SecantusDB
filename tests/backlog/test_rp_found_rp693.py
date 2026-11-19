"""Gap tests of divergences found while working RP-693 (a `bpchar` key
keyed with its trailing blanks trimmed):

- COPY in autocommit stamped its rows with an xid it never completed, so
  the next statement ran under the same xid and an ON CONFLICT DO UPDATE
  over a copied row was refused 21000 as a row "this command" wrote.

The expected answers were measured on PostgreSQL 15.19 (2026-11-18), on
the same statements over a scratch table.
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


@pytest.mark.backlog("RP-1069")
def test_copy_completes_its_own_xid(dsn: str) -> None:
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


@pytest.mark.backlog("RP-1070")
def test_explain_indexed_equality_with_a_filter(dsn: str) -> None:
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


@pytest.mark.backlog("RP-1072")
def test_a_plain_index_on_an_unsized_bpchar_serves_equality(dsn: str) -> None:
    """A plain CREATE INDEX on an unsized `bpchar` column is a btree over
    `bpcharcmp` on PostgreSQL 15.19, so `a = 'k'` is an Index Scan that
    finds `'k  '`. Here the index is a storage index over the value as
    written, which keeps its blanks since RP-693 step 5, so `=` is refused
    to it and runs as a Seq Scan (right answer, wrong plan)."""
    table = f"ix_bp_{uuid.uuid4().hex[:8]}"
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(f"create table {table} (a bpchar, b int)")
        try:
            c.execute(f"insert into {table} select 'k'||i, i from generate_series(1, 2000) i")
            c.execute(f"insert into {table} values ('k  ', -1)")
            c.execute(f"create index {table}_a on {table} (a)")
            c.execute(f"analyze {table}")
            plan = [
                r[0]
                for r in c.execute(
                    f"explain (costs off) select b from {table} where a = 'k'"
                ).fetchall()
            ]
            assert plan == [
                f"Index Scan using {table}_a on {table}",
                "  Index Cond: (a = 'k'::bpchar)",
            ]
            assert c.execute(f"select b from {table} where a = 'k'").fetchall() == [(-1,)]
        finally:
            c.execute(f"drop table {table}")
