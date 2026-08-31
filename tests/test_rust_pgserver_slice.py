"""The Rust PostgreSQL server's P1 vertical slice, and its cross-server contract.

The headline assertion is not that the Rust server works on its own -- it is
that the Python server and the Rust server share one on-disk store. A catalog
document written subtly wrong by one is read as truth by the other, which is
silent data loss, so both directions are exercised here.

Skipped unless `secantusd-pg` has been built (it links WiredTiger and is
excluded from the clean workspace):

    cd crates/secantus-pgserver && cargo build
"""

from __future__ import annotations

import shutil
import socket
import subprocess
import time
from collections.abc import Iterator
from pathlib import Path

import pytest

psycopg = pytest.importorskip("psycopg")

REPO = Path(__file__).resolve().parents[1]
BINARY = REPO / "crates" / "secantus-pgserver" / "target" / "debug" / "secantusd-pg"

pytestmark = pytest.mark.skipif(
    not BINARY.exists(),
    reason=f"{BINARY.relative_to(REPO)} not built (cargo build in crates/secantus-pgserver)",
)


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


class _Server:
    """A `secantusd-pg` subprocess over one storage home."""

    def __init__(self, home: Path) -> None:
        self.home = home
        self.port = _free_port()
        self.proc: subprocess.Popen[str] | None = None

    def __enter__(self) -> _Server:
        self.proc = subprocess.Popen(
            [str(BINARY), str(self.home), f"127.0.0.1:{self.port}"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                out = self.proc.stdout.read() if self.proc.stdout else ""
                raise RuntimeError(f"secantusd-pg exited: {out}")
            try:
                with socket.create_connection(("127.0.0.1", self.port), timeout=0.25):
                    return self
            except OSError:
                time.sleep(0.05)
        raise RuntimeError("secantusd-pg did not start")

    def __exit__(self, *exc: object) -> None:
        if self.proc is not None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=10)

    def connect(self) -> psycopg.Connection:
        return psycopg.connect(
            f"host=127.0.0.1 port={self.port} dbname=postgres user=test",
            autocommit=True,
            connect_timeout=10,
        )


@pytest.fixture
def home(tmp_path: Path) -> Iterator[Path]:
    """A storage home the Rust server and the Python server both open.

    Only ONE may hold it at a time -- WiredTiger takes an exclusive lock -- so
    every test stops one before starting the other.
    """
    d = tmp_path / "pgstore"
    d.mkdir()
    yield d
    shutil.rmtree(d, ignore_errors=True)


def _python_sql(home: Path, *statements: str) -> list[tuple]:
    """Run statements through the PYTHON server over the same store."""
    from secantus.sql import run_sql
    from secantus.sql.session import Session
    from secantus.storage import Storage

    storage = Storage(str(home))
    try:
        session = Session()
        rows: list[tuple] = []
        for sql in statements:
            for result in run_sql(storage, "postgres", sql, session=session):
                rows.extend(result.rows)
        return rows
    finally:
        storage.close()


def test_create_insert_select_round_trip(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, name text, n int)")
        cur.execute("INSERT INTO t VALUES (1,'alice',10),(2,'bob',20),(3,'carol',30)")
        cur.execute("SELECT id, name, n FROM t")
        assert sorted(cur.fetchall()) == [
            (1, "alice", 10),
            (2, "bob", 20),
            (3, "carol", 30),
        ]


@pytest.mark.parametrize(
    "where,expected",
    [
        ("id = 1", [1]),
        ("n > 15", [2, 3]),
        ("n >= 20 AND id <> 3", [2]),
        ("name = 'carol' OR n < 15", [1, 3]),
        ("n <= 20 AND (id = 1 OR name = 'bob')", [1, 2]),
    ],
)
def test_predicates_match_postgres(home: Path, where: str, expected: list[int]) -> None:
    """These answers were checked against a live PostgreSQL 14; PG is the oracle."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, name text, n int)")
        cur.execute("INSERT INTO t VALUES (1,'alice',10),(2,'bob',20),(3,'carol',30)")
        cur.execute(f"SELECT id FROM t WHERE {where}")
        assert sorted(r[0] for r in cur.fetchall()) == expected


def test_the_python_server_reads_and_writes_a_rust_created_table(home: Path) -> None:
    """The catalog contract, in the direction that matters most.

    If the Rust server's catalog document diverges, the Python server does not
    fail loudly -- it sees a table with the wrong columns.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, name text, n int)")
        cur.execute("INSERT INTO t VALUES (1,'alice',10),(2,'bob',20)")

    # Rust is stopped; Python opens the same store.
    assert _python_sql(home, "SELECT id, name, n FROM t ORDER BY id") == [
        (1, "alice", 10),
        (2, "bob", 20),
    ]
    _python_sql(home, "INSERT INTO t VALUES (3, 'carol', 30)")

    # ... and Rust sees what Python wrote.
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT id, name, n FROM t WHERE n > 15")
        assert sorted(cur.fetchall()) == [(2, "bob", 20), (3, "carol", 30)]


def test_the_rust_server_reads_a_python_created_table(home: Path) -> None:
    """The same contract in the other direction."""
    _python_sql(
        home,
        "CREATE TABLE py (k int PRIMARY KEY, label text)",
        "INSERT INTO py VALUES (9, 'made-by-python')",
    )
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT k, label FROM py WHERE k = 9")
        assert cur.fetchall() == [(9, "made-by-python")]


def test_duplicate_key_reports_what_postgres_reports(home: Path) -> None:
    """The storage layer speaks MongoDB (`E11000 duplicate key error ...`).
    None of that may reach a PostgreSQL client. Probed against PG 14."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, name text, n int)")
        cur.execute("INSERT INTO t VALUES (1,'alice',10)")
        with pytest.raises(psycopg.errors.UniqueViolation) as exc:
            cur.execute("INSERT INTO t VALUES (1,'dup',1)")
    diag = exc.value.diag
    assert diag.sqlstate == "23505"
    assert diag.message_primary == ('duplicate key value violates unique constraint "t_pkey"')
    assert diag.message_detail == "Key (id)=(1) already exists."
    assert "E11000" not in str(exc.value)


@pytest.mark.parametrize(
    "sql,sqlstate",
    [
        ("SELECT nope FROM t", "42703"),
        ("SELECT * FROM t WHERE nope = 1", "42703"),
        ("SELECT * FROM missing", "42P01"),
        ("CREATE TABLE t (id int PRIMARY KEY)", "42P07"),
        # Unsupported must be an honest 0A000 -- never a wrong row. There is no
        # fallback into Python by design.
        ("SELECT * FROM t JOIN t AS u ON t.id = u.id", "0A000"),
        ("SELECT avg(n) FROM t", "0A000"),
        ("SELECT n, count(*) FROM t", "42803"),
        ("SELECT * FROM t WHERE n LIKE 'x'", "0A000"),
        ("SELECT * FROM t ORDER BY n + 1", "0A000"),
        # The PK is the document's `_id`, which storage treats as immutable.
        ("UPDATE t SET id = 2 WHERE id = 1", "0A000"),
        ("UPDATE t SET nope = 1", "42703"),
        ("DELETE FROM missing", "42P01"),
    ],
)
def test_refusals_carry_the_right_sqlstate(home: Path, sql: str, sqlstate: str) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, name text, n int)")
        with pytest.raises(psycopg.Error) as exc:
            cur.execute(sql)
    assert exc.value.diag.sqlstate == sqlstate


def test_acknowledged_writes_survive_sigterm(home: Path) -> None:
    """Regression: `secantusd-pg` must close WiredTiger on a signal.

    The first cut had no signal handler, so SIGTERM killed the process with no
    checkpoint. Measured 2026-08-31: after CREATE TABLE + INSERT the client had
    been told both succeeded, and reopening the store found the catalog document
    AND the rows gone. The server acknowledged writes it then lost -- which in a
    database is the whole ballgame, not a tidiness issue.

    `_Server.__exit__` sends SIGTERM, so this asserts the real path.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE durable (id int PRIMARY KEY, n int)")
        cur.execute("INSERT INTO durable VALUES (1, 10), (2, 20)")
        cur.execute("SELECT id FROM durable")
        assert sorted(r[0] for r in cur.fetchall()) == [1, 2]

    # Nothing above ran a checkpoint explicitly; only the signal handler can
    # have flushed this.
    assert _python_sql(home, "SELECT id, n FROM durable ORDER BY id") == [(1, 10), (2, 20)]


def test_order_limit_offset_and_dml(home: Path) -> None:
    """The P5 slice end to end over the wire.

    Every expectation here was checked against a live PostgreSQL 14; the
    exhaustive comparison lives in `test_rust_pgserver_differential.py`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, n int, s text)")
        cur.execute("INSERT INTO t VALUES (1,3,'c'),(2,NULL,'a'),(3,1,NULL),(4,2,'b')")

        # PostgreSQL puts NULLs LAST on ASC and FIRST on DESC. MongoDB sorts
        # null LOW, so getting this wrong reorders every nullable column.
        cur.execute("SELECT id FROM t ORDER BY n")
        assert [r[0] for r in cur.fetchall()] == [3, 4, 1, 2]
        cur.execute("SELECT id FROM t ORDER BY n DESC")
        assert [r[0] for r in cur.fetchall()] == [2, 1, 4, 3]
        cur.execute("SELECT id FROM t ORDER BY n ASC NULLS FIRST")
        assert [r[0] for r in cur.fetchall()] == [2, 3, 4, 1]

        cur.execute("SELECT id FROM t ORDER BY id LIMIT 2 OFFSET 1")
        assert [r[0] for r in cur.fetchall()] == [2, 3]

        # Three-valued logic: the NULL row is excluded by <> and by NOT IN.
        cur.execute("SELECT id FROM t WHERE n <> 1")
        assert sorted(r[0] for r in cur.fetchall()) == [1, 4]
        cur.execute("SELECT id FROM t WHERE n NOT IN (1)")
        assert sorted(r[0] for r in cur.fetchall()) == [1, 4]
        cur.execute("SELECT id FROM t WHERE n NOT IN (1, NULL)")
        assert cur.fetchall() == []
        cur.execute("SELECT id FROM t WHERE NOT (n = 1)")
        assert sorted(r[0] for r in cur.fetchall()) == [1, 4]

        # UPDATE's row count is rows MATCHED, as PostgreSQL reports.
        cur.execute("UPDATE t SET s = 'z' WHERE n > 1")
        assert cur.rowcount == 2
        cur.execute("SELECT id FROM t WHERE s = 'z'")
        assert sorted(r[0] for r in cur.fetchall()) == [1, 4]

        cur.execute("DELETE FROM t WHERE n IS NULL")
        assert cur.rowcount == 1
        cur.execute("SELECT id FROM t")
        assert sorted(r[0] for r in cur.fetchall()) == [1, 3, 4]


def test_parameterised_queries_go_over_the_extended_protocol(home: Path) -> None:
    """psycopg switches to Parse/Bind/Execute the moment a query has parameters.

    Before the extended handler existed, that path answered `OK` with ZERO ROWS
    for a query that should return rows -- a wrong answer rather than a missing
    feature, and invisible to every literal-SQL test.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, n int, s text)")
        cur.execute("INSERT INTO t VALUES (1,10,'a'),(2,20,'b'),(3,NULL,'c')")

        cur.execute("SELECT id FROM t WHERE n > %s", (5,))
        assert sorted(r[0] for r in cur.fetchall()) == [1, 2]
        cur.execute("SELECT id FROM t WHERE s = %s", ("b",))
        assert cur.fetchall() == [(2,)]
        cur.execute("SELECT id FROM t WHERE n IN (%s, %s)", (10, 20))
        assert sorted(r[0] for r in cur.fetchall()) == [1, 2]
        cur.execute("SELECT count(*) FROM t WHERE n > %s", (5,))
        assert cur.fetchall() == [(2,)]

        # A bound NULL behaves like a literal one: `= NULL` is never true.
        cur.execute("SELECT id FROM t WHERE n = %s", (None,))
        assert cur.fetchall() == []

        cur.execute("UPDATE t SET n = %s WHERE id = %s", (99, 1))
        assert cur.rowcount == 1
        cur.execute("DELETE FROM t WHERE id = %s", (3,))
        assert cur.rowcount == 1
        cur.execute("INSERT INTO t VALUES (%s, %s, %s)", (4, 40, "d"))
        assert cur.rowcount == 1
        cur.execute("SELECT id, n FROM t ORDER BY id")
        assert cur.fetchall() == [(1, 99), (2, 20), (4, 40)]
