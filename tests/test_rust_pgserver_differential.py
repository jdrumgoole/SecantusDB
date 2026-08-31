"""The Rust PostgreSQL server, diffed against a real PostgreSQL.

The Python SQL server is the *behaviour* oracle; live PostgreSQL is the
*correctness* one, and where they disagree PostgreSQL wins. Every case here runs
the identical DDL, DML and query against both servers and asserts the answers
match — which is how the NULL-ordering and three-valued-logic rules in
`secantus-pgplan` were derived rather than guessed.

Needs BOTH:
  * `secantusd-pg` built  — cd crates/secantus-pgserver && cargo build
  * a live PostgreSQL     — SECANTUS_PG_ORACLE_DSN, default the local 14
"""

from __future__ import annotations

import os
import time
from collections.abc import Iterator
from pathlib import Path

import pytest

psycopg = pytest.importorskip("psycopg")

from tests.test_rust_pgserver_slice import BINARY, _Server  # noqa: E402

ORACLE_DSN = os.environ.get(
    "SECANTUS_PG_ORACLE_DSN",
    "host=127.0.0.1 port=5432 dbname=postgres user=jdrumgoole",
)


# Short and few on purpose. The failure mode here is a HANG, not a blip:
# Postgres.app gates connections per-application behind a macOS permission
# dialog, so an unapproved process (every pytest-xdist worker) waits for a
# dialog nobody answers until the timeout expires. A generous 15s x 5 budget
# therefore bought nothing and cost 75s of dead waiting per worker.
_CONNECT_TIMEOUT_S = 5
_CONNECT_ATTEMPTS = 2
# Why the last connection attempt failed. A bare "no oracle" skip is
# indistinguishable from "PostgreSQL is not installed", which is how ~100
# silently skipped tests looked like an intentional configuration.
_LAST_ERROR: list[str] = ["never attempted"]


def _oracle() -> psycopg.Connection | None:
    """Connect to the oracle, retrying once on a transient failure.

    **If this skips under the full suite but passes standalone, the cause is
    almost certainly Postgres.app\'s per-application permission gate**, not
    your code and not load. Measured 2026-08-31: every worker got

        FATAL: Postgres.app failed to verify "trust" authentication
        DETAIL: You did not confirm the permission dialog.

    surfacing through psycopg as a bare `ConnectionTimeout`, because the server
    waits on a dialog no background process can answer. It silently disabled
    ~109 tests here and the five pre-existing oracle suites
    (`test_sql_search_path.py` and friends) alongside them. Fix it in
    Postgres.app\'s settings, or point `SECANTUS_PG_ORACLE_DSN` at a plain
    PostgreSQL; there is nothing to fix in the test.
    """
    delay = 0.5
    for attempt in range(_CONNECT_ATTEMPTS):
        try:
            return psycopg.connect(ORACLE_DSN, autocommit=True, connect_timeout=_CONNECT_TIMEOUT_S)
        except Exception as exc:  # noqa: BLE001 - retry, then record why
            _LAST_ERROR[0] = f"{type(exc).__name__}: {exc}"
            if attempt == _CONNECT_ATTEMPTS - 1:
                return None
            time.sleep(delay)
            delay *= 2
    return None


def _oracle_available() -> bool:
    """Probe for the oracle WITHOUT leaking the probe's connection.

    The first cut called `_oracle()` straight into a `skipif` and dropped the
    connection on the floor. Every xdist worker importing the module then held
    one open for the whole session, against a server with max_connections=100.
    """
    conn = _oracle()
    if conn is None:
        return False
    conn.close()
    return True


pytestmark = [
    pytest.mark.skipif(
        not BINARY.exists(),
        reason="secantusd-pg not built (cargo build in crates/secantus-pgserver)",
    ),
    pytest.mark.skipif(
        not _oracle_available(),
        reason=f"no local PostgreSQL oracle ({ORACLE_DSN}): {_LAST_ERROR[0]}",
    ),
]

# One shared fixture table. `n` and `s` are nullable ON PURPOSE: three-valued
# logic is where SQL and MQL diverge, so every case gets a NULL to trip over.
SETUP = [
    "CREATE TABLE d (id int PRIMARY KEY, n int, s text)",
    "INSERT INTO d VALUES (1, 3, 'c'), (2, NULL, 'a'), (3, 1, NULL), (4, 2, 'b')",
]

QUERIES = [
    # --- ORDER BY: null placement is the trap --------------------------------
    "SELECT id FROM d ORDER BY n",
    "SELECT id FROM d ORDER BY n DESC",
    "SELECT id FROM d ORDER BY n ASC NULLS FIRST",
    "SELECT id FROM d ORDER BY n DESC NULLS LAST",
    "SELECT id FROM d ORDER BY s",
    "SELECT id FROM d ORDER BY s DESC",
    "SELECT id FROM d ORDER BY n, id",
    "SELECT id FROM d ORDER BY id DESC",
    # --- LIMIT / OFFSET ------------------------------------------------------
    "SELECT id FROM d ORDER BY id LIMIT 2",
    "SELECT id FROM d ORDER BY id OFFSET 1",
    "SELECT id FROM d ORDER BY id LIMIT 2 OFFSET 1",
    "SELECT id FROM d ORDER BY id LIMIT 0",
    "SELECT id FROM d ORDER BY id OFFSET 99",
    "SELECT id FROM d ORDER BY n LIMIT 3",
    # --- three-valued logic --------------------------------------------------
    "SELECT id FROM d WHERE n IS NULL",
    "SELECT id FROM d WHERE n IS NOT NULL",
    "SELECT id FROM d WHERE s IS NULL",
    "SELECT id FROM d WHERE n IN (1, 3)",
    "SELECT id FROM d WHERE n IN (1, NULL)",
    "SELECT id FROM d WHERE n NOT IN (1)",
    "SELECT id FROM d WHERE n NOT IN (1, NULL)",
    "SELECT id FROM d WHERE s IN ('a', 'c')",
    "SELECT id FROM d WHERE n BETWEEN 1 AND 2",
    "SELECT id FROM d WHERE n NOT BETWEEN 1 AND 2",
    "SELECT id FROM d WHERE n = 1",
    "SELECT id FROM d WHERE n <> 1",
    "SELECT id FROM d WHERE n > 1 AND s IS NOT NULL",
    "SELECT id FROM d WHERE n IS NULL OR n > 2",
    # --- projection ----------------------------------------------------------
    "SELECT id, n, s FROM d ORDER BY id",
    "SELECT s AS label FROM d ORDER BY id",
    "SELECT * FROM d ORDER BY id",
    "SELECT n, id FROM d ORDER BY id",
    "SELECT id, id FROM d ORDER BY id",
    # --- more three-valued logic, hunting for the next `<>`-shaped bug -------
    "SELECT id FROM d WHERE n >= 1",
    "SELECT id FROM d WHERE n <= 2",
    "SELECT id FROM d WHERE n < 3",
    "SELECT id FROM d WHERE s <> 'a'",
    "SELECT id FROM d WHERE s > 'a'",
    "SELECT id FROM d WHERE n <> 1 OR s IS NULL",
    "SELECT id FROM d WHERE NOT (n IS NULL)",
    "SELECT id FROM d WHERE NOT (n IS NOT NULL)",
    "SELECT id FROM d WHERE NOT (n = 1)",
    "SELECT id FROM d WHERE NOT (n <> 1)",
    "SELECT id FROM d WHERE NOT (n > 1)",
    "SELECT id FROM d WHERE NOT (n <= 2)",
    "SELECT id FROM d WHERE NOT (n = 1 AND s IS NULL)",
    "SELECT id FROM d WHERE NOT (n = 1 OR n = 2)",
    "SELECT id FROM d WHERE NOT (NOT (n = 1))",
    "SELECT id FROM d WHERE NOT (n IN (1, 3))",
    "SELECT id FROM d WHERE NOT (n NOT IN (1, 3))",
    "SELECT id FROM d WHERE NOT (n BETWEEN 1 AND 2)",
    "SELECT id FROM d WHERE NOT (n NOT BETWEEN 1 AND 2)",
    "SELECT id FROM d WHERE NOT (s IS NULL) AND n > 1",
    "SELECT id FROM d WHERE n = 1 AND s IS NULL",
    "SELECT id FROM d WHERE (n = 1 OR n = 2) AND id <> 3",
    "SELECT id FROM d WHERE n IN (1)",
    "SELECT id FROM d WHERE n NOT IN (1, 2)",
    "SELECT id FROM d WHERE s NOT IN ('a')",
    "SELECT id FROM d WHERE n BETWEEN 2 AND 1",
    "SELECT id FROM d WHERE n NOT BETWEEN 2 AND 3",
    "SELECT id FROM d WHERE id BETWEEN 1 AND 4",
    # --- ordering interactions ----------------------------------------------
    "SELECT id FROM d WHERE n IS NOT NULL ORDER BY n DESC",
    "SELECT id FROM d ORDER BY s NULLS FIRST",
    "SELECT id FROM d ORDER BY n ASC, s DESC",
    "SELECT id FROM d ORDER BY id LIMIT 10 OFFSET 0",
    # --- aggregates: NULL handling is the whole game ------------------------
    "SELECT count(*) FROM d",
    "SELECT count(n) FROM d",
    "SELECT count(s) FROM d",
    "SELECT sum(n) FROM d",
    "SELECT min(n) FROM d",
    "SELECT max(n) FROM d",
    "SELECT min(s) FROM d",
    "SELECT max(s) FROM d",
    "SELECT count(*), count(n), sum(n), min(n), max(n) FROM d",
    # Over an empty input: count is 0, everything else is NULL.
    "SELECT count(*) FROM d WHERE n > 99",
    "SELECT count(n) FROM d WHERE n > 99",
    "SELECT sum(n) FROM d WHERE n > 99",
    "SELECT min(n) FROM d WHERE n > 99",
    "SELECT max(n) FROM d WHERE n > 99",
    "SELECT count(*) FROM d WHERE n IS NOT NULL",
    "SELECT sum(n) FROM d WHERE n <> 1",
    # --- GROUP BY: NULL is its own group ------------------------------------
    "SELECT s, count(*) FROM d GROUP BY s ORDER BY s",
    "SELECT s, count(n) FROM d GROUP BY s ORDER BY s",
    "SELECT s, sum(n) FROM d GROUP BY s ORDER BY s",
    "SELECT s, min(n), max(n) FROM d GROUP BY s ORDER BY s",
    "SELECT s, count(*) FROM d GROUP BY s ORDER BY s DESC",
    "SELECT s, count(*) FROM d GROUP BY s ORDER BY s NULLS FIRST",
    "SELECT s, count(*) FROM d WHERE n IS NOT NULL GROUP BY s ORDER BY s",
    "SELECT s AS grp, count(*) AS c FROM d GROUP BY s ORDER BY s",
    "SELECT s, count(*) FROM d GROUP BY s ORDER BY s LIMIT 2",
    "SELECT count(*) FROM d GROUP BY s ORDER BY s",
]

# (statement, verification query) — the write is compared by its row count AND
# by what the table looks like afterwards.
MUTATIONS = [
    ("UPDATE d SET n = 99 WHERE id = 1", "SELECT id, n FROM d ORDER BY id"),
    ("UPDATE d SET n = 5 WHERE n IS NULL", "SELECT id, n FROM d ORDER BY id"),
    ("UPDATE d SET s = 'z' WHERE n > 1", "SELECT id, s FROM d ORDER BY id"),
    ("UPDATE d SET n = 7 WHERE id = 999", "SELECT id, n FROM d ORDER BY id"),
    ("UPDATE d SET n = 1", "SELECT id, n FROM d ORDER BY id"),
    ("DELETE FROM d WHERE id = 2", "SELECT id FROM d ORDER BY id"),
    ("DELETE FROM d WHERE n IS NULL", "SELECT id FROM d ORDER BY id"),
    ("DELETE FROM d WHERE n > 1", "SELECT id FROM d ORDER BY id"),
    ("DELETE FROM d WHERE id = 999", "SELECT id FROM d ORDER BY id"),
    ("UPDATE d SET n = NULL WHERE id = 1", "SELECT id, n FROM d ORDER BY id"),
    ("UPDATE d SET s = NULL", "SELECT id, s FROM d ORDER BY id"),
    ("UPDATE d SET n = 4 WHERE n IN (1, 2)", "SELECT id, n FROM d ORDER BY id"),
    ("UPDATE d SET n = 0 WHERE n <> 3", "SELECT id, n FROM d ORDER BY id"),
    ("UPDATE d SET n = 8 WHERE n BETWEEN 1 AND 2", "SELECT id, n FROM d ORDER BY id"),
    ("DELETE FROM d WHERE n IS NOT NULL", "SELECT id FROM d ORDER BY id"),
    ("DELETE FROM d WHERE n IN (1, 3)", "SELECT id FROM d ORDER BY id"),
    ("DELETE FROM d WHERE n <> 1", "SELECT id FROM d ORDER BY id"),
    ("DELETE FROM d", "SELECT id FROM d ORDER BY id"),
]


@pytest.fixture(scope="module")
def oracle() -> Iterator[psycopg.Connection]:
    """The live PostgreSQL, isolated to THIS xdist worker's own schema.

    Our side gets a fresh storage home per test, but there is only one local
    PostgreSQL and every case here creates a table called `d`. Sharing the
    `public` schema across xdist workers made them race on `CREATE TABLE d`
    (`duplicate key ... pg_type_typname_nsp_index`) — 35 failures under `-n
    auto` that every one of them passed serially. A per-worker schema removes
    the shared name entirely.
    """
    conn = _oracle()
    if conn is None:
        # Reachable at import but not now: report it as a skip, matching the
        # other oracle-backed suites. An ERROR here reads as a server bug when
        # it is the oracle that went away.
        pytest.skip(f"PostgreSQL oracle unreachable ({ORACLE_DSN}): {_LAST_ERROR[0]}")
    worker = os.environ.get("PYTEST_XDIST_WORKER", "serial")
    schema = f"secantus_diff_{worker}"
    try:
        cur = conn.cursor()
        # Recreate rather than reuse: a previous run's leftovers would seed the
        # oracle with rows this run never inserted.
        cur.execute(f"DROP SCHEMA IF EXISTS {schema} CASCADE")
        cur.execute(f"CREATE SCHEMA {schema}")
        cur.execute(f"SET search_path TO {schema}")
        yield conn
        cur.execute(f"DROP SCHEMA IF EXISTS {schema} CASCADE")
    finally:
        conn.close()


def _reset_oracle(conn: psycopg.Connection) -> None:
    cur = conn.cursor()
    cur.execute("DROP TABLE IF EXISTS d")
    for sql in SETUP:
        cur.execute(sql)


def _rows(cur: psycopg.Cursor, sql: str) -> list[tuple]:
    cur.execute(sql)
    return cur.fetchall()


@pytest.fixture
def ours(tmp_path: Path) -> Iterator[psycopg.Connection]:
    """A freshly seeded secantusd-pg."""
    home = tmp_path / "pgstore"
    home.mkdir()
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql in SETUP:
            cur.execute(sql)
        yield conn


@pytest.mark.parametrize("sql", QUERIES, ids=lambda s: s[:58])
def test_query_matches_postgres(
    sql: str, ours: psycopg.Connection, oracle: psycopg.Connection
) -> None:
    _reset_oracle(oracle)
    theirs = _rows(oracle.cursor(), sql)
    mine = _rows(ours.cursor(), sql)
    assert mine == theirs, f"{sql}\n  postgres={theirs}\n  ours    ={mine}"


@pytest.mark.parametrize("stmt,verify", MUTATIONS, ids=lambda s: str(s)[:58])
def test_mutation_matches_postgres(
    stmt: str, verify: str, ours: psycopg.Connection, oracle: psycopg.Connection
) -> None:
    _reset_oracle(oracle)
    ocur, mcur = oracle.cursor(), ours.cursor()

    ocur.execute(stmt)
    mcur.execute(stmt)
    # The row count is part of the contract: PostgreSQL's UPDATE tag counts rows
    # MATCHED, so `SET n = 1` reports every row even where the value is unchanged.
    assert mcur.rowcount == ocur.rowcount, (
        f"{stmt}\n  postgres rowcount={ocur.rowcount}\n  ours     rowcount={mcur.rowcount}"
    )
    assert _rows(mcur, verify) == _rows(ocur, verify), f"after {stmt}"
