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

import contextlib
import datetime as dt
import decimal as dc
import ipaddress
import re
import shutil
import signal
import subprocess
import sys
import threading
import time
import uuid
from collections.abc import Iterator
from decimal import Decimal
from pathlib import Path

import pytest

psycopg = pytest.importorskip("psycopg")
from psycopg.types.multirange import Multirange  # noqa: E402
from psycopg.types.range import Range  # noqa: E402

REPO = Path(__file__).resolve().parents[1]
# `.exe` on Windows, where the bare name never exists -- so every test in this
# file (1,194 of them) skipped there even with the server built.
BINARY = (
    REPO
    / "crates"
    / "secantus-pgserver"
    / "target"
    / "debug"
    / ("secantusd-pg.exe" if sys.platform == "win32" else "secantusd-pg")
)

pytestmark = pytest.mark.skipif(
    not BINARY.exists(),
    reason=f"{BINARY.relative_to(REPO)} not built (cargo build in crates/secantus-pgserver)",
)

_WINDOWS = sys.platform == "win32"
#: Windows delivers a console control event to a process GROUP, so the server
#: needs its own; without this the break would also reach pytest.
_SPAWN_KWARGS: dict[str, object] = (
    {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if _WINDOWS else {}
)
#: The graceful stop. `ctrlc`'s handler catches CTRL_BREAK on Windows and
#: SIGTERM elsewhere, so both reach the server's real shutdown path.
_STOP_SIGNAL = signal.CTRL_BREAK_EVENT if _WINDOWS else signal.SIGTERM


class _Server:
    """A `secantusd-pg` subprocess over one storage home."""

    def __init__(self, home: Path, *, databases: tuple[str, ...] = ()) -> None:
        self.home = home
        self.port = 0
        self.proc: subprocess.Popen[str] | None = None
        self.databases = databases

    def __enter__(self) -> _Server:
        # Bind port 0 and let the KERNEL name the port, then read it back from
        # the server's readiness line. Probing for a free port and passing it to
        # the child cannot be made safe: the probe socket has to close before the
        # child binds, so under `-n auto` two workers can be handed the same
        # port. The loser's child exits with "address already in use" -- but a
        # liveness probe fired in that gap CONNECTS TO THE WINNER'S SERVER, and
        # the harness then hands the test a connection to another worker's
        # database, which dies when that worker finishes. That is the
        # "server closed the connection unexpectedly" on a `CREATE TABLE` that
        # CI hit on 2026-09-09. Binding 0 removes the gap entirely -- the port
        # is never unbound between being chosen and being listened on.
        self.proc = subprocess.Popen(
            [
                str(BINARY),
                str(self.home),
                "127.0.0.1:0",
                *(f"--database={name}" for name in self.databases),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            **_SPAWN_KWARGS,
        )
        line = self._readline(timeout=30)
        match = re.search(r"listening on \S+:(\d+)", line)
        if not match:
            self.__exit__()
            rest = ""
            with contextlib.suppress(Exception):
                rest = self.proc.stdout.read() if self.proc.stdout else ""
            raise RuntimeError(f"secantusd-pg did not start: {line!r}{rest!r}")
        self.port = int(match.group(1))
        return self

    def _readline(self, *, timeout: float) -> str:
        """The child's first stdout line, or "" if it dies or stalls."""
        out: list[str] = []
        reader = threading.Thread(
            target=lambda: out.append(self.proc.stdout.readline() if self.proc.stdout else ""),
            daemon=True,
        )
        reader.start()
        reader.join(timeout)
        return out[0] if out else ""

    def __exit__(self, *exc: object) -> None:
        if self.proc is None:
            return
        # Ask for a GRACEFUL stop, because the store is handed to another
        # server after this. `Popen.terminate()` is SIGTERM on POSIX but
        # `TerminateProcess` on Windows -- an immediate kill that runs no
        # handler, so WiredTiger never closes and anything not yet
        # checkpointed is simply gone. That is why the hand-off tests read an
        # EMPTY store on Windows. `secantusd-pg` uses the `ctrlc` crate with
        # `termination`, which installs a console control handler there, so a
        # CTRL_BREAK_EVENT reaches the same shutdown path SIGTERM takes on
        # Unix. It goes to a process GROUP, hence CREATE_NEW_PROCESS_GROUP at
        # spawn -- without it the break would also hit the pytest process.
        with contextlib.suppress(Exception):
            self.proc.send_signal(_STOP_SIGNAL)
        try:
            self.proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            # A server that will not drain is a finding, not something to
            # paper over -- but leaving it running would wedge the suite, so
            # kill it and let the test's own assertion report the damage.
            self.proc.kill()
            self.proc.wait(timeout=10)

    def connect(self, *, autocommit: bool = True, dbname: str = "postgres") -> psycopg.Connection:
        return psycopg.connect(
            f"host=127.0.0.1 port={self.port} dbname={dbname} user=test",
            autocommit=autocommit,
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
    # The protocol's constraint fields, available since pgwire 0.39 and read by
    # pgjdbc via getServerErrorMessage().getConstraint(). `column_name` stays
    # unset because PostgreSQL leaves it unset on a 23505 -- it identifies the
    # column through the constraint (probed 14).
    assert diag.constraint_name == "t_pkey"
    assert diag.table_name == "t"
    assert diag.schema_name == "public"
    assert diag.column_name is None


@pytest.mark.parametrize(
    "sql,sqlstate",
    [
        ("SELECT nope FROM t", "42703"),
        ("SELECT * FROM t WHERE nope = 1", "42703"),
        ("SELECT * FROM missing", "42P01"),
        ("CREATE TABLE t (id int PRIMARY KEY)", "42P07"),
        # Unsupported must be an honest 0A000 -- never a wrong row. There is no
        # fallback into Python by design.
        # A self-join is implemented; a BARE column both sides have is
        # PostgreSQL's 42702, never the left side's value taken silently.
        ("SELECT id FROM t JOIN t AS u ON t.id = u.id", "42702"),
        ("SELECT x.id FROM t JOIN t AS u ON t.id = u.id", "42P01"),
        ("SELECT n, count(*) FROM t", "42803"),
        # `LIKE` is implemented now; over an INTEGER column PostgreSQL 14.13
        # has no such operator, so this moved from 0A000 to 42883 rather than
        # becoming legal. It used to return no rows, silently.
        ("SELECT * FROM t WHERE n LIKE 'x'", "42883"),
        # `ORDER BY n + 1` is IMPLEMENTED now, so it is no longer a refusal.
        # `ORDER BY ... USING` still is, and keeps this row exercising the
        # ORDER BY path rather than losing the coverage entirely.
        ("SELECT * FROM t ORDER BY n USING <", "0A000"),
        # The PK is the document's `_id`, which storage treats as immutable.
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


def test_drop_table_removes_the_catalog_entry(home: Path) -> None:
    """A dropped table must leave nothing behind.

    The collection AND its `__sql_catalog__` document both go; a surviving
    catalog row pointing at a vanished collection is the unrecoverable half,
    which is why the drop does the collection first.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, n int)")
        cur.execute("INSERT INTO t VALUES (1, 10)")
        cur.execute("DROP TABLE t")

        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT id FROM t")
        assert exc.value.diag.sqlstate == "42P01"

        # Recreating with a DIFFERENT shape proves the old catalog row is gone
        # rather than merely orphaned.
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, s text)")
        cur.execute("INSERT INTO t VALUES (1, 'fresh')")
        cur.execute("SELECT id, s FROM t")
        assert cur.fetchall() == [(1, "fresh")]


def test_drop_table_if_exists_and_missing(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY)")
        cur.execute("DROP TABLE t")
        # Bare DROP of a missing table is 42P01; IF EXISTS is a no-op that
        # still reports the DROP TABLE tag (probed PG 14).
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("DROP TABLE t")
        assert exc.value.diag.sqlstate == "42P01"
        cur.execute("DROP TABLE IF EXISTS t")
        cur.execute("DROP TABLE IF EXISTS nope1, nope2")


def test_casts_carry_postgres_types_not_value_types(home: Path) -> None:
    """`Describe` precedes `Bind`, so a column's type cannot be read off the
    value — it comes from the cast."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT %s::int", ("42",))
        assert cur.fetchall() == [(42,)]
        assert cur.description[0].type_code == 23  # int4, not varchar
        cur.execute("SELECT 1::text")
        assert cur.fetchall() == [("1",)]
        assert cur.description[0].type_code == 25  # text, NOT varchar (1043)
        cur.execute("SELECT NULL::int")
        assert cur.fetchall() == [(None,)]
        assert cur.description[0].type_code == 23
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT 'x'::int")
        assert exc.value.diag.sqlstate == "22P02"


def test_row_description_carries_typmod_and_typlen(home: Path) -> None:
    """A `RowDescription` reports each column's declared type-modifier
    (`atttypmod`) and fixed byte width (`typlen`), which psycopg turns into
    `precision` / `scale` / `display_size` / `internal_size`. Metadata only:
    the modifier never changes how a value is decoded.

    Measured against PostgreSQL 16 -- every fmod/fsize below is the exact wire
    value the real server sends for the same `select null::<type>`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        # numeric(p, s): typmod = ((p << 16) | (s & 0x7FF)) + 4, typlen -1.
        cur.execute("SELECT NULL::numeric(10,2)")
        col = cur.description[0]
        assert cur.pgresult.fmod(0) == 655366
        assert cur.pgresult.fsize(0) == -1
        assert (col.precision, col.scale) == (10, 2)
        assert col.internal_size is None

        # A negative scale (PostgreSQL 15+) rides the signed low 11 bits.
        cur.execute("SELECT NULL::numeric(2,-3)")
        assert cur.pgresult.fmod(0) == 133121
        assert (cur.description[0].precision, cur.description[0].scale) == (2, -3)

        # varchar(n): typmod = n + 4 (varlena header), typlen -1.
        cur.execute("SELECT NULL::varchar(42)")
        col = cur.description[0]
        assert cur.pgresult.fmod(0) == 46
        assert cur.pgresult.fsize(0) == -1
        assert col.display_size == 42

        # time(p): typmod = p, typlen 8 fixed.
        cur.execute("SELECT NULL::time(6)")
        col = cur.description[0]
        assert cur.pgresult.fmod(0) == 6
        assert cur.pgresult.fsize(0) == 8
        assert col.precision == 6
        assert col.internal_size == 8

        # interval(p): typmod packs the full-range mask over the precision.
        cur.execute("SELECT NULL::interval(2)")
        assert cur.pgresult.fmod(0) == (0x7FFF << 16) | 2
        assert cur.pgresult.fsize(0) == 16
        assert cur.description[0].precision == 2

        # bit(n) is its own oid (1560) with the length itself as the modifier.
        cur.execute("SELECT NULL::bit(5)")
        col = cur.description[0]
        assert col.type_code == 1560
        assert cur.pgresult.fmod(0) == 5
        assert col.display_size == 5

        # A fixed-width type with no modifier: typlen set, typmod -1.
        cur.execute("SELECT NULL::int4")
        assert cur.pgresult.fmod(0) == -1
        assert cur.pgresult.fsize(0) == 4
        assert cur.description[0].internal_size == 4


def test_constant_expressions_match_postgres(home: Path) -> None:
    """Arithmetic, concatenation and comparison in a SELECT list.

    Two corners were probed rather than assumed: integer division TRUNCATES
    (`7/2` is 3, not 3.5) and `5/0` is `22012`, not a NULL or an infinity.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, want, oid in [
            ("SELECT 1+1", 2, 23),
            ("SELECT 7/2", 3, 23),
            ("SELECT 7%2", 1, 23),
            ("SELECT (1+2)*3", 9, 23),
            ("SELECT -3", -3, 23),
            ("SELECT 'a'||'b'", "ab", 25),
            ("SELECT 'n='||1", "n=1", 25),
            ("SELECT 1+NULL", None, 23),
            ("SELECT 1=1", True, 16),
            ("SELECT 1<2", True, 16),
        ]:
            cur.execute(sql)
            assert cur.fetchone()[0] == want, sql
            assert cur.description[0].type_code == oid, sql

        # The type comes from the OPERATOR, not the value: Describe plans this
        # against a NULL placeholder and must still say int4.
        cur.execute("SELECT %s + 1", (41,))
        assert cur.fetchone()[0] == 42
        assert cur.description[0].type_code == 23

        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT 5/0")
        assert exc.value.diag.sqlstate == "22012"


def test_scalar_array_any_all(home: Path) -> None:
    """`col <op> ANY(%s)` / `ALL(%s)` with an array parameter.

    This is how psycopg renders an IN-list, so the WHERE + array-parameter form
    is the one that matters. An untyped array parameter arrives as the array
    literal text and is coerced to the column's element type, and a scalar
    compared to an array with no ANY/ALL is 42883.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE aa (id int PRIMARY KEY, s text)")
        cur.execute("INSERT INTO aa VALUES (1,'x'),(2,'y'),(3,'z')")

        cur.execute("SELECT id FROM aa WHERE id = ANY(%s) ORDER BY id", ([1, 3],))
        assert cur.fetchall() == [(1,), (3,)]
        # An untyped TEXT array parameter is coerced from its literal text.
        cur.execute("SELECT id FROM aa WHERE s = ANY(%s) ORDER BY id", (["x", "z"],))
        assert cur.fetchall() == [(1,), (3,)]
        cur.execute("SELECT id FROM aa WHERE id <> ALL(%s) ORDER BY id", ([2],))
        assert cur.fetchall() == [(1,), (3,)]
        # Empty array: ANY matches nothing, ALL matches everything.
        cur.execute("SELECT id FROM aa WHERE id = ANY(%s)", ([],))
        assert cur.fetchall() == []
        cur.execute("SELECT id FROM aa WHERE id <> ALL(%s) ORDER BY id", ([],))
        assert cur.fetchall() == [(1,), (2,), (3,)]
        # A NULL array matches nothing.
        cur.execute("SELECT id FROM aa WHERE id = ANY(%s)", (None,))
        assert cur.fetchall() == []

        # A scalar compared to an array with no ANY/ALL is 42883.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT id FROM aa WHERE s = ARRAY['x']")
        assert exc.value.diag.sqlstate == "42883"


def test_client_encoding_transcodes_both_directions(home: Path) -> None:
    """`client_encoding` is honoured on the way IN as well as OUT.

    Result text, parameters, the QUERY TEXT itself and RowDescription column
    names all travel in the session encoding, in both protocols -- psycopg
    encodes the query with the encoding it learned from ParameterStatus, so a
    server that decodes it as UTF-8 reads `\u20ac` as three mojibake
    characters. A `client_encoding` in the startup packet is applied before
    the first query and re-reported under its canonical name; an unknown one
    fails the connection with PostgreSQL's FATAL 22023.
    """
    with _Server(home) as server:
        with server.connect() as conn:
            cur = conn.cursor()
            cur.execute("SET client_encoding = 'latin9'")
            assert conn.info.encoding == "iso8859-15"
            # Simple protocol: literal, alias and value all round-trip.
            cur.execute("SELECT 'caf\u00e9 \u20ac' AS \"prix \u20ac\"")
            assert cur.fetchone() == ("caf\u00e9 \u20ac",)
            assert cur.description[0].name == "prix \u20ac"
            # Extended protocol: query text and a text parameter.
            cur.execute("SELECT %s || ' \u20ac'", ("caf\u00e9",))
            assert cur.fetchone() == ("caf\u00e9 \u20ac",)
            # Binary results carry the same bytes.
            cur.execute("SELECT '\u20ac'::text", binary=True)
            assert cur.fetchone() == ("\u20ac",)
            # An untranslatable character is 22P05, as PostgreSQL's is
            # (`chr` builds it server-side: the client could not send it).
            with pytest.raises(psycopg.errors.UntranslatableCharacter):
                cur.execute("SELECT chr(20013)")
            cur.execute("SET client_encoding = 'UTF8'")
            assert conn.info.encoding == "utf-8"
            cur.execute("SELECT chr(20013)")
            assert cur.fetchone() == ("\u4e2d",)

        # Startup packet: libpq's PGCLIENTENCODING / the `client_encoding=`
        # conninfo option. Reported canonically (`utf-8` -> `UTF8`).
        for requested, canonical, py_name in (
            ("utf-8", "UTF8", "utf-8"),
            ("iso8859-15", "LATIN9", "iso8859-15"),
        ):
            with psycopg.connect(
                f"host=127.0.0.1 port={server.port} dbname=postgres user=test "
                f"client_encoding={requested}",
                autocommit=True,
            ) as conn:
                assert conn.info.parameter_status("client_encoding") == canonical
                assert conn.info.encoding == py_name
                assert conn.execute("SELECT '\u20ac'").fetchone() == ("\u20ac",)
        with pytest.raises(psycopg.OperationalError) as exc:
            psycopg.connect(
                f"host=127.0.0.1 port={server.port} dbname=postgres user=test "
                "client_encoding=bogus",
                connect_timeout=10,
            )
        assert 'FATAL:  invalid value for parameter "client_encoding": "bogus"' in str(exc.value)


def test_session_settings(home: Path) -> None:
    """SET / SHOW / RESET and the GUC functions.

    Settings are per CONNECTION, as PostgreSQL's are, and the reported column
    name uses PostgreSQL's canonical casing (`SHOW datestyle` answers a column
    called `DateStyle`) because clients match on it.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SHOW client_encoding")
        assert cur.fetchone()[0] == "UTF8"
        cur.execute("SHOW datestyle")
        assert cur.fetchone()[0] == "ISO, MDY"
        assert cur.description[0].name == "DateStyle"

        # Transaction GUCs psycopg reads to learn the connection's defaults.
        # `max_prepared_transactions` is the two-phase-commit slot count
        # (psycopg's tpc tests skip when it is 0).
        for name, want in (
            ("max_prepared_transactions", "100"),
            ("transaction_isolation", "read committed"),
            ("default_transaction_isolation", "read committed"),
            ("transaction_deferrable", "off"),
            ("default_transaction_read_only", "off"),
        ):
            cur.execute(f"SHOW {name}")
            assert cur.fetchone()[0] == want, name

        cur.execute("SET my.x = '7'")
        assert cur.statusmessage == "SET"
        cur.execute("SELECT current_setting('my.x')")
        assert cur.fetchone()[0] == "7"

        cur.execute("SELECT set_config('my.y', '9', false)")
        assert cur.fetchone()[0] == "9"
        cur.execute("SELECT current_setting('my.y')")
        assert cur.fetchone()[0] == "9"

        # An unknown name errors; with missing_ok it is NULL (probed PG 14).
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT current_setting('nope.zz')")
        assert exc.value.diag.sqlstate == "42704"
        cur.execute("SELECT current_setting('nope.zz', true)")
        assert cur.fetchone()[0] is None

        cur.execute("RESET my.x")
        assert cur.statusmessage == "RESET"

    # A new connection starts from the defaults, not the previous session's.
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        with pytest.raises(psycopg.Error):
            cur.execute("SELECT current_setting('my.y')")


def test_transaction_characteristics_reflected_in_gucs(home: Path) -> None:
    """`BEGIN <modes>` sets the `transaction_*` GUCs for the block.

    Values checked against a live PostgreSQL 14: inside the block the GUCs
    reflect the requested characteristics, and after the block ends they revert
    to the session `default_transaction_*`.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY DEFERRABLE")
        cur.execute(
            "SELECT current_setting('transaction_isolation'), "
            "current_setting('transaction_read_only'), "
            "current_setting('transaction_deferrable')"
        )
        assert cur.fetchone() == ("serializable", "on", "on")
        cur.execute("COMMIT")

        # After the block, the transaction GUCs are back to the defaults.
        cur.execute(
            "SELECT current_setting('transaction_isolation'), "
            "current_setting('transaction_read_only'), "
            "current_setting('transaction_deferrable')"
        )
        assert cur.fetchone() == ("read committed", "off", "off")


def test_set_transaction_inside_a_block(home: Path) -> None:
    """`SET TRANSACTION <modes>` sets the current block's characteristics.

    It answers the `SET` command tag and is reflected in the `transaction_*`
    GUCs until the block ends (checked against PostgreSQL 14).
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("BEGIN")
        cur.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        assert cur.statusmessage == "SET"
        cur.execute(
            "SELECT current_setting('transaction_isolation'), "
            "current_setting('transaction_read_only')"
        )
        assert cur.fetchone() == ("repeatable read", "on")
        cur.execute("ROLLBACK")
        cur.execute("SELECT current_setting('transaction_isolation')")
        assert cur.fetchone()[0] == "read committed"


def test_set_session_characteristics_sets_the_default(home: Path) -> None:
    """`SET SESSION CHARACTERISTICS AS TRANSACTION <modes>` sets the default.

    Outside a block the current `transaction_*` GUCs move with the default, so
    a client reads back its choice immediately (checked against PostgreSQL 14).
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        assert cur.statusmessage == "SET"
        cur.execute(
            "SELECT current_setting('default_transaction_isolation'), "
            "current_setting('transaction_isolation')"
        )
        assert cur.fetchone() == ("serializable", "serializable")


def test_set_config_with_a_bound_parameter(home: Path) -> None:
    """`set_config($1, $2, false)` over the extended protocol round-trips.

    psycopg's transaction-parameter tests call `set_config` with the name and
    value as bound parameters. During the DESCRIBE the value parameters are
    still unbound, so the name argument arrives as NULL -- the plan must fold
    that to NULL rather than error, and the EXECUTE with real values must set
    the GUC.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT set_config(%s, %s, false)",
            ["default_transaction_isolation", "serializable"],
        )
        assert cur.fetchone()[0] == "serializable"
        cur.execute("SELECT current_setting('default_transaction_isolation')")
        assert cur.fetchone()[0] == "serializable"


def test_psycopg_transaction_parameters_round_trip(home: Path) -> None:
    """psycopg's own `.isolation_level` / `.read_only` / `.deferrable`.

    On a non-autocommit connection psycopg emits `BEGIN <modes>` before the
    first statement of each transaction, so the block reflects the client's
    choice and reverts on rollback.
    """
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        conn.isolation_level = psycopg.IsolationLevel.SERIALIZABLE.value
        conn.read_only = True
        conn.deferrable = True
        cur = conn.execute(
            "SELECT current_setting('transaction_isolation'), "
            "current_setting('transaction_read_only'), "
            "current_setting('transaction_deferrable')"
        )
        assert cur.fetchone() == ("serializable", "on", "on")
        conn.rollback()

        conn.isolation_level = None
        conn.read_only = None
        conn.deferrable = None
        cur = conn.execute(
            "SELECT current_setting('transaction_isolation'), "
            "current_setting('transaction_read_only'), "
            "current_setting('transaction_deferrable')"
        )
        assert cur.fetchone() == ("read committed", "off", "off")
        conn.rollback()


def test_copy_from_stdin(home: Path) -> None:
    """`COPY ... FROM STDIN` in PostgreSQL's text format.

    The escaping is the substance: `\\N` is NULL and is distinct from an empty
    string, and a literal tab inside a value arrives as `\\t` and must not be
    read as a field separator. A chunk boundary can also land anywhere,
    including mid-row, so the data is buffered and parsed only at CopyDone.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE cp (id int PRIMARY KEY, s text, n int)")
        with cur.copy("COPY cp FROM STDIN") as cp:
            cp.write("1\ta\t10\n2\t\\N\t20\n3\thas\\ttab\t30\n")
        assert cur.statusmessage == "COPY 3"

        cur.execute("SELECT id, s, n FROM cp ORDER BY id")
        rows = cur.fetchall()
        assert rows[0] == (1, "a", 10)
        assert rows[1] == (2, None, 20), "\\N must be NULL, not the string"
        assert rows[2] == (3, "has\ttab", 30), "an escaped tab is data, not a separator"

        # An explicit column list leaves the others NULL.
        cur.execute("CREATE TABLE cp2 (id int PRIMARY KEY, s text, n int)")
        with cur.copy("COPY cp2 (id, n) FROM STDIN") as cp:
            cp.write("7\t70\n")
        cur.execute("SELECT id, s, n FROM cp2")
        assert cur.fetchall() == [(7, None, 70)]

        # A chunk boundary mid-row must not split a value.
        cur.execute("CREATE TABLE cp3 (id int PRIMARY KEY, s text)")
        with cur.copy("COPY cp3 FROM STDIN") as cp:
            cp.write("1\tab")
            cp.write("c\n2\tdef\n")
        cur.execute("SELECT id, s FROM cp3 ORDER BY id")
        assert cur.fetchall() == [(1, "abc"), (2, "def")]


def test_copy_to_stdout_round_trips(home: Path) -> None:
    """`COPY ... TO STDOUT` in text format.

    Was refused until pgwire 0.38 added the copy-out API (0.31 had no way to
    push CopyData rows from the simple handler). The output is byte-identical
    to PostgreSQL's, so it round-trips straight back through COPY FROM.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE cp (id int, s text, n int)")
        cur.execute("INSERT INTO cp VALUES (1,'a',10),(2,NULL,20),(3,'has\ttab',30)")

        with cur.copy("COPY cp TO STDOUT") as cp:
            out = b"".join(cp).decode()
        # PostgreSQL's own text encoding: `\N` for NULL, escaped tabs.
        assert out == "1\ta\t10\n2\t\\N\t20\n3\thas\\ttab\t30\n"

        cur.execute("DELETE FROM cp")
        with cur.copy("COPY cp FROM STDIN") as cp:
            cp.write(out)
        cur.execute("SELECT id, s, n FROM cp ORDER BY id")
        assert cur.fetchall() == [(1, "a", 10), (2, None, 20), (3, "has\ttab", 30)]


def test_copy_binary_round_trips_scalar_types(home: Path) -> None:
    """`COPY ... TO STDOUT (FORMAT BINARY)` then `FROM STDIN (FORMAT BINARY)`.

    The per-field binary layout is PostgreSQL's own -- fixed-width for the
    numeric and temporal types, length-prefixed raw bytes for `bytea`, the
    element format for arrays. Producing it goes through the same `encode_binary`
    codec the SELECT binary path uses, so a round-trip that survives here proves
    both directions agree on the wire form for every scalar the tests exercise.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "CREATE TABLE cb ("
            "a smallint, b integer, c bigint, d real, e double precision, "
            "f boolean, g text, h numeric, i date, j time, "
            "k timestamp, l bytea, m int4[])"
        )
        cur.execute(
            "INSERT INTO cb VALUES "
            "(1, 2, 9000000000, 1.5, 3.25, true, 'hi', 42.50, "
            "'2026-01-15', '12:34:56.5', '2026-01-15 12:34:56.123456', "
            "'\\x0001ff'::bytea, '{1,2,3}'::int4[])"
        )
        cur.execute("INSERT INTO cb VALUES (" + ", ".join(["NULL"] * 13) + ")")

        with cur.copy("COPY cb TO STDOUT (FORMAT BINARY)") as cp:
            blob = b"".join(cp)

        cur.execute("DELETE FROM cb")
        with cur.copy("COPY cb FROM STDIN (FORMAT BINARY)") as cp:
            cp.write(blob)

        cur.execute("SELECT a,b,c,d,e,f,g,h,i,j,k,l,m FROM cb ORDER BY b NULLS LAST")
        rows = cur.fetchall()
        assert rows[0] == (
            1,
            2,
            9000000000,
            1.5,
            3.25,
            True,
            "hi",
            dc.Decimal("42.50"),
            dt.date(2026, 1, 15),
            dt.time(12, 34, 56, 500000),
            dt.datetime(2026, 1, 15, 12, 34, 56, 123456),
            b"\x00\x01\xff",
            [1, 2, 3],
        )
        assert rows[1] == (None,) * 13


def test_date_and_time_columns(home: Path) -> None:
    """`date` and `time` as real column types.

    Stored as canonical text (the representation the Python server uses, since
    both share one store) but REPORTED with their true oids -- 1082 and 1083.
    That distinction decides whether a client hands back a `date` object or a
    string, and psycopg would never have caught it: it decodes varchar to `str`
    either way.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, d date, tm time)")
        cur.execute("INSERT INTO t VALUES (1, '2026-09-01', '12:34:56')")
        cur.execute("SELECT id, d, tm FROM t")
        rows = cur.fetchall()
        assert rows == [(1, dt.date(2026, 9, 1), dt.time(12, 34, 56))]
        assert [c.type_code for c in cur.description] == [23, 1082, 1083]

        # PostgreSQL accepts several spellings and stores exactly one.
        cur.execute("INSERT INTO t VALUES (2, '2026-9-1', '12:34')")
        cur.execute("SELECT d, tm FROM t WHERE id = 2")
        assert cur.fetchall() == [(dt.date(2026, 9, 1), dt.time(12, 34, 0))]

        # 22007 is "not a date"; 22008 is "a date that cannot exist".
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT 'not-a-date'::date")
        assert exc.value.diag.sqlstate == "22007"
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT '2026-02-30'::date")
        assert exc.value.diag.sqlstate == "22008"


def test_datetime_arithmetic_and_special_values(home: Path) -> None:
    """`date + int`, `timestamp + interval`, `interval + interval`, the `epoch`
    literal, and the `24:00` end-of-day time.

    The result-type OIDs matter as much as the values: psycopg picks its loader
    from the DESCRIBED column type, so `timestamp + interval` must describe as
    1114 (not text/int), `interval + interval` as 1186, and `date + int` as
    1082. An out-of-range arithmetic result is rendered in PostgreSQL's own text
    (a year past 9999, or the `BC` era) so the CLIENT's loader is what rejects
    it -- exactly what psycopg's overflow tests assert. `24:00:00` is a valid
    `time` PostgreSQL renders back verbatim; a Python `time` cannot hold it, so
    the loader raises on the way in.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        # epoch special literal on date / timestamp / timestamptz.
        cur.execute("SELECT 'epoch'::date")
        assert cur.fetchone()[0] == dt.date(1970, 1, 1)
        assert cur.description[0].type_code == 1082
        cur.execute("SELECT 'epoch'::date + 1")
        assert cur.fetchone()[0] == dt.date(1970, 1, 2)
        assert cur.description[0].type_code == 1082

        # date arithmetic types and values.
        cur.execute("SELECT '2000-01-01'::date + 5")
        assert cur.fetchone()[0] == dt.date(2000, 1, 6)
        cur.execute("SELECT '2000-01-01'::date - '1999-01-01'::date")
        assert cur.fetchone()[0] == 365
        assert cur.description[0].type_code == 23

        # timestamp + interval describes as timestamp (1114); interval + interval
        # as interval (1186). The value-type alone would have said text.
        cur.execute("SELECT '2000-01-01 00:00:00'::timestamp + '1s'::interval")
        assert cur.fetchone()[0] == dt.datetime(2000, 1, 1, 0, 0, 1)
        assert cur.description[0].type_code == 1114
        cur.execute("SELECT '1 day'::interval + '1s'::interval")
        assert cur.fetchone()[0] == dt.timedelta(days=1, seconds=1)
        assert cur.description[0].type_code == 1186

        # 24:00:00 is a valid time value PostgreSQL renders verbatim; a Python
        # time cannot hold it, so psycopg's loader raises DataError.
        cur.execute("SELECT '24:00'::time::text")
        assert cur.fetchone()[0] == "24:00:00"
        with pytest.raises(psycopg.DataError):
            cur.execute("SELECT '24:00'::time")
            cur.fetchone()

        # An out-of-range date arithmetic result renders in PG text the client
        # cannot load -- year past 9999 and the BC era.
        cur.execute("SELECT ('9999-12-31'::date + 1)::text")
        assert cur.fetchone()[0] == "10000-01-01"
        cur.execute("SELECT ('0001-01-01'::date + -1)::text")
        assert cur.fetchone()[0] == "0001-12-31 BC"


def test_multidimensional_arrays(home: Path) -> None:
    """Multidimensional arrays over the wire, in both formats.

    The values already STORE and text-render; the gap was RETURNING a nested
    array to the client. `int[]` is binary-encodable, so a 2-D column reaches
    the binary encoder in whichever format the cursor asked for -- text (the
    psycopg default) or binary -- and both must reconstruct `[[1, 2], [3, 4]]`.
    A ragged literal is rejected exactly as PostgreSQL rejects it.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE m (id int PRIMARY KEY, a int[], t text[])")
        cur.execute("INSERT INTO m VALUES (1, '{{1,2},{3,4}}', '{{a,b},{c,d}}')")
        for binary in (False, True):
            c = conn.cursor(binary=binary)
            c.execute("SELECT a, t FROM m WHERE id = 1")
            assert c.fetchone() == ([[1, 2], [3, 4]], [["a", "b"], ["c", "d"]]), binary
            assert [d.type_code for d in c.description] == [1007, 1009]

        cur.execute("SELECT ARRAY[[1, 2], [3, 4]]")
        assert cur.fetchone()[0] == [[1, 2], [3, 4]]
        assert cur.description[0].type_code == 1007

        cur.execute("SELECT ARRAY[[[1, 2]], [[3, 4]]]")
        assert cur.fetchone()[0] == [[[1, 2]], [[3, 4]]]
        cur.execute("SELECT ARRAY[[1, NULL], [3, 4]]::int[]")
        assert cur.fetchone()[0] == [[1, None], [3, 4]]
        cur.execute("INSERT INTO m VALUES (2, %s, NULL)", ([[5, 6], [7, 8]],))
        cur.execute("SELECT a FROM m WHERE id = 2")
        assert cur.fetchone()[0] == [[5, 6], [7, 8]]

        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT ARRAY[[1, 2], [3]]")
        assert exc.value.diag.sqlstate == "2202E"
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT '{{1,2},{3}}'::int[]")
        assert exc.value.diag.sqlstate == "22P02"


def test_uuid_and_timetz_columns(home: Path) -> None:
    """`uuid` and `timetz` as real column types.

    A uuid canonicalises to lowercase 8-4-4-4-12 from any accepted spelling and
    reports oid 2950, so psycopg hands back a `uuid.UUID`. A timetz keeps its
    LITERAL offset -- it is not session-relative the way timestamptz is -- so
    the canonical text is a safe column; it reports oid 1266.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE u (id int PRIMARY KEY, x uuid, t timetz)")
        cur.execute(
            "INSERT INTO u VALUES (1, 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11', '12:34:56+02')"
        )
        cur.execute("SELECT x, t FROM u")
        plus_two = dt.timezone(dt.timedelta(hours=2))
        assert cur.fetchall() == [
            (
                uuid.UUID("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
                dt.time(12, 34, 56, tzinfo=plus_two),
            ),
        ]
        assert [c.type_code for c in cur.description] == [2950, 1266]

        # A no-hyphen spelling stores the same canonical value.
        cur.execute("INSERT INTO u VALUES (2, 'a0eebc999c0b4ef8bb6d6bb9bd380a11', '08:00-05')")
        cur.execute("SELECT x::text FROM u WHERE id = 2")
        assert cur.fetchall() == [("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",)]

        # A malformed uuid is 22P02.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT 'not-a-uuid'::uuid")
        assert exc.value.diag.sqlstate == "22P02"

        # timestamptz IS a column now (stored as a UTC instant, rendered in the
        # session zone); see test_timestamptz_columns_render_in_session_zone.
        cur.execute("CREATE TABLE tstz_ok (id int, t timestamptz)")


def test_timestamp_sub_millisecond_invariant(home: Path) -> None:
    """Timestamps keep microseconds BSON cannot hold, via a hidden companion.

    A BSON date is a MILLISECOND count, so `12:34:56.789012` would truncate to
    `.789000`. The Python server stores the truncated date plus the lost 0-999
    microseconds in a `__us_<field>` companion, and this server writes the same
    thing -- the two share one database, so the representation is a contract.

    THE INVARIANT under test: every write must SET or CLEAR the companion. A
    stale one is worse than truncation, because it reports a time that was
    never stored.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, ts timestamp)")
        cur.execute(
            "INSERT INTO t VALUES (1,'2026-09-01 12:34:56.789012'),"
            "(2,'2026-09-01 12:34:56'),(3,'2026-09-01')"
        )
        cur.execute("SELECT id, ts FROM t ORDER BY id")
        assert cur.fetchall() == [
            (1, dt.datetime(2026, 9, 1, 12, 34, 56, 789012)),
            (2, dt.datetime(2026, 9, 1, 12, 34, 56)),
            # A bare date is midnight.
            (3, dt.datetime(2026, 9, 1, 0, 0)),
        ]
        assert cur.description[1].type_code == 1114  # timestamp, not varchar

        # Overwrite a microsecond row with a whole-millisecond value: the
        # companion must be CLEARED, not left behind.
        cur.execute("UPDATE t SET ts = '2026-09-01 00:00:00' WHERE id = 1")
        cur.execute("SELECT ts FROM t WHERE id = 1")
        assert cur.fetchall() == [(dt.datetime(2026, 9, 1, 0, 0),)]

        # ... and setting microseconds again restores it.
        cur.execute("UPDATE t SET ts = '2026-09-01 01:02:03.456789' WHERE id = 1")
        cur.execute("SELECT ts FROM t WHERE id = 1")
        assert cur.fetchall() == [(dt.datetime(2026, 9, 1, 1, 2, 3, 456789),)]


def test_python_server_reads_rust_timestamps(home: Path) -> None:
    """The companion contract, across servers.

    Getting this wrong is silent corruption rather than an error: the Python
    server would read a time the Rust server never wrote.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, ts timestamp)")
        cur.execute(
            "INSERT INTO t VALUES (1,'2026-09-01 01:02:03.456789'),(2,'2026-09-01 12:34:56')"
        )

    assert _python_sql(home, "SELECT id, ts FROM t ORDER BY id") == [
        (1, dt.datetime(2026, 9, 1, 1, 2, 3, 456789)),
        (2, dt.datetime(2026, 9, 1, 12, 34, 56)),
    ]


def test_numeric_keeps_its_scale(home: Path) -> None:
    """`numeric` carries scale as part of the VALUE, not as formatting.

    PostgreSQL answers `'1.50'` for `1.50::numeric::text`, not `'1.5'`, and a
    client reading oid 1700 gets a Decimal rather than a float. Storing these
    as doubles would have given the right magnitude under the wrong type — the
    same failure that made a cast integer arrive as a string.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT 1.5")
        assert cur.fetchone()[0] == Decimal("1.5")
        assert cur.description[0].type_code == 1700  # numeric, not float8

        cur.execute("SELECT 1.50::numeric::text")
        assert cur.fetchone()[0] == "1.50"
        cur.execute("SELECT '-0.30'::numeric::text")
        assert cur.fetchone()[0] == "-0.30"

        cur.execute("CREATE TABLE n (id int PRIMARY KEY, amt numeric)")
        cur.execute("INSERT INTO n VALUES (1, 1.50), (2, '0.1')")
        cur.execute("SELECT id, amt FROM n ORDER BY id")
        assert cur.fetchall() == [(1, Decimal("1.50")), (2, Decimal("0.1"))]
        assert cur.description[1].type_code == 1700

        # Beyond 34 significant digits is kept exactly, never rounded.
        cur.execute("SELECT '1.2345678901234567890123456789012345'::numeric")
        assert cur.fetchone()[0] == Decimal("1.2345678901234567890123456789012345")


def test_arrays_round_trip_with_their_own_oids(home: Path) -> None:
    """Arrays are their own types, and `int[]` is not `int`.

    libpg_query keeps the array-ness of `int[]` in `array_bounds` rather than
    in the type name, so a server that reads only the name types an array
    column as its element type. That looks harmless until a CAST loses its
    brackets too, at which point `%s::text[] = %s::text[]` quietly degrades to
    comparing two rendered strings — which agrees with PostgreSQL often enough
    to pass for correct.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        cur.execute("SELECT ARRAY[1,2,3]::int[]")
        assert cur.fetchone()[0] == [1, 2, 3]
        assert cur.description[0].type_code == 1007  # int4[], not int4

        cur.execute("SELECT ARRAY['a','b']::text[]")
        assert cur.fetchone()[0] == ["a", "b"]
        assert cur.description[0].type_code == 1009  # text[]

        cur.execute("SELECT '{}'::text[]")
        assert cur.fetchone()[0] == []

        # Inside an array two NULLs are EQUAL and a NULL sorts after every
        # non-NULL — neither rule holds for scalar `=`, where `NULL = NULL` is
        # NULL. All four were probed against a live PostgreSQL 14.
        cur.execute("SELECT ARRAY[NULL]::text[] = ARRAY[NULL]::text[]")
        assert cur.fetchone()[0] is True
        cur.execute("SELECT ARRAY['a',NULL]::text[] > ARRAY['a','z']::text[]")
        assert cur.fetchone()[0] is True
        cur.execute("SELECT ARRAY['a']::text[] < ARRAY['a','b']::text[]")
        assert cur.fetchone()[0] is True

        cur.execute("CREATE TABLE a (id int PRIMARY KEY, xs int[], names text[])")
        cur.execute("INSERT INTO a VALUES (1, '{1,2}', '{x,y}')")
        cur.execute("SELECT xs, names FROM a WHERE id = 1")
        assert cur.fetchone() == ([1, 2], ["x", "y"])

        # A nested array round-trips as a nested list (multidimensional arrays
        # are carried over the wire now — see test_multidimensional_arrays).
        cur.execute("SELECT '{{1,2},{3,4}}'::int[]")
        assert cur.fetchone()[0] == [[1, 2], [3, 4]]


def test_simple_query_runs_a_batch_in_one_implicit_transaction(home: Path) -> None:
    """Several commands in one simple query, as PostgreSQL runs them.

    The transaction is the part that cannot be faked afterwards, and both rules
    below were measured against PostgreSQL 14:

    * a failure anywhere in the batch rolls back what earlier commands wrote —
      the batch is one implicit transaction, not a sequence of autocommits;
    * an explicit ``COMMIT`` inside the batch ends that transaction, so what it
      committed survives a later failure.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        cur.execute("select 1; select 2")
        assert cur.fetchall() == [(1,)]
        assert cur.nextset() is True
        assert cur.fetchall() == [(2,)]

        cur.execute("create table t (id int primary key)")

        # A later failure discards the earlier insert.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("insert into t values (1); select * from nosuchtable")
        assert exc.value.diag.sqlstate == "42P01"
        cur.execute("select count(*) from t")
        assert cur.fetchone()[0] == 0

        # An explicit COMMIT inside the batch is a real commit.
        with pytest.raises(psycopg.Error):
            cur.execute("begin; insert into t values (2); commit; select * from nosuchtable")
        cur.execute("select count(*) from t")
        assert cur.fetchone()[0] == 1

        # Empty commands are accepted and produce no result.
        cur.execute("select 1;;")
        assert cur.fetchone() == (1,)
        cur.execute(";")
        assert cur.statusmessage is None

        # The EXTENDED protocol still refuses several commands: it has one
        # parameter list and one row description, which two commands cannot
        # share. PostgreSQL says exactly this, with this SQLSTATE.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select 1; select %s", (2,))
        assert exc.value.diag.sqlstate == "42601"
        assert "cannot insert multiple commands" in str(exc.value)


def test_deallocate_all_is_accepted(home: Path) -> None:
    """`DEALLOCATE ALL` must succeed, and with PostgreSQL's own tag.

    psycopg issues it to reset its prepared-statement cache, but only when the
    connection happens to have one — so refusing it failed a scattered handful
    of tests depending on execution order, which reads as flakiness rather than
    as a missing feature. The prepared-statement store belongs to the wire
    layer here, so there is nothing to free; the tag is what the client needs.

    `DEALLOCATE <name>` of a name that does not exist is PostgreSQL's 26000
    `InvalidSqlStatementName` (probed PG 16), not a no-op and not 0A000.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("DEALLOCATE ALL")
        assert cur.statusmessage == "DEALLOCATE ALL"

        with pytest.raises(psycopg.errors.InvalidSqlStatementName) as exc:
            cur.execute("DEALLOCATE nosuchstmt")
        assert exc.value.diag.sqlstate == "26000"
        assert 'prepared statement "nosuchstmt" does not exist' in str(exc.value)


def test_pg_typeof_reports_the_display_type(home: Path) -> None:
    """`pg_typeof` prints `integer`, not `int4`, and answers a regtype.

    It reports the STATIC type of its argument, which is why `pg_typeof(NULL)`
    is `unknown`: no value could tell us that.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select pg_typeof(1)")
        assert cur.fetchone()[0] == "integer"
        assert cur.description[0].type_code == 2206  # regtype, not text

        for expr, want in [
            ("1::int8", "bigint"),
            ("1.5", "numeric"),
            ("1.5::float8", "double precision"),
            ("'a'::varchar", "character varying"),
            ("'2026-01-01 12:00'::timestamp", "timestamp without time zone"),
            ("ARRAY[1,2]", "integer[]"),
            ("null", "unknown"),
        ]:
            cur.execute(f"select pg_typeof({expr})::text")
            assert cur.fetchone()[0] == want, expr


# (declared type, value). Each is bound over BOTH wire formats: psycopg picks
# binary for most of these by default, and the two paths decode independently,
# so a type can be right in one and wrong in the other.
_BOUND_VALUES = [
    ("numeric", Decimal("1.50")),
    ("numeric", Decimal("0.1")),
    ("numeric", Decimal("-12345.6789")),
    ("numeric", Decimal("0")),
    ("numeric", Decimal("12345678901234567890.123")),
    ("date", dt.date(2026, 9, 2)),
    ("date", dt.date(1970, 1, 1)),
    ("date", dt.date(1999, 12, 31)),
    ("time", dt.time(12, 34, 56)),
    ("time", dt.time(0, 0, 0)),
    ("time", dt.time(23, 59, 59, 123456)),
    ("timestamp", dt.datetime(2026, 9, 2, 12, 34, 56)),
    ("timestamp", dt.datetime(1969, 7, 20, 20, 17, 40)),
    ("timestamp", dt.datetime(2026, 1, 1, 0, 0, 0, 123456)),
    ("int4[]", [1, 2, 3]),
    ("int4[]", []),
    ("int4[]", [1, None, 3]),
    ("text[]", ["a", "b"]),
    ("text[]", ["a", None]),
    ("int8[]", [10**12, 2]),
    ("float8[]", [1.5, 2.5]),
]


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
@pytest.mark.parametrize("typename,value", _BOUND_VALUES, ids=lambda v: str(v)[:26])
def test_bound_parameter_round_trips_in_both_formats(
    home: Path, typename: str, value: object, binary: bool
) -> None:
    """A bound parameter must survive both wire formats unchanged.

    psycopg sends most of these in BINARY by default, and the two formats are
    decoded by separate code, so a type can be right in one and wrong in the
    other — which is exactly what happened: every one of these was refused in
    binary, and `numeric` in *text* was being parsed as a float, so a client
    binding `Decimal("1.50")` got a float that had already lost the scale that
    distinguishes it from `1.5`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        cur.execute(f"select %s::{typename}", (value,))
        assert cur.fetchone()[0] == value


def test_timestamp_constants_are_not_null(home: Path) -> None:
    """`select '...'::timestamp` must answer the timestamp, not NULL.

    A stored timestamp is reassembled from its column plus a hidden companion
    field carrying sub-millisecond digits. A timestamp CONSTANT never passes
    through a row, so it reached the encoder as that composite with no arm to
    match it and came out as NULL — while the same value read from a column, or
    cast to text, was correct. A wrong answer that only appears in one of three
    paths to the same value.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select '2026-01-01 12:00'::timestamp")
        assert cur.fetchone()[0] == dt.datetime(2026, 1, 1, 12, 0)
        assert cur.description[0].type_code == 1114

        # Sub-millisecond digits survive the same path.
        cur.execute("select '2026-01-01 12:00:00.123456'::timestamp")
        assert cur.fetchone()[0] == dt.datetime(2026, 1, 1, 12, 0, 0, 123456)

        # The other two routes to the same value still agree.
        cur.execute("select '2026-01-01 12:00'::timestamp::text")
        assert cur.fetchone()[0] == "2026-01-01 12:00:00"
        cur.execute("create table ts (id int primary key, t timestamp)")
        cur.execute("insert into ts values (1, '2026-01-01 12:00')")
        cur.execute("select t from ts where id = 1")
        assert cur.fetchone()[0] == dt.datetime(2026, 1, 1, 12, 0)


def test_timestamptz_renders_in_the_session_zone(home: Path) -> None:
    """`timestamptz` is an instant; what you see is the session's view of it.

    Two sign conventions meet here and they run opposite ways. In
    ``SET TimeZone TO '+02:00'`` the sign is POSIX — positive is *west* of
    Greenwich, so it renders as ``-02``. In a literal like ``'12:00+02'`` the
    sign is the ordinary one, two hours *east*. Both were probed against a live
    PostgreSQL; getting either backwards is invisible under UTC and wrong by
    hours everywhere else.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        cur.execute("set timezone to 'UTC'")
        cur.execute("select '2026-01-01 12:00'::timestamptz::text")
        assert cur.fetchone()[0] == "2026-01-01 12:00:00+00"
        cur.execute("select '2026-01-01 12:00+02'::timestamptz::text")
        assert cur.fetchone()[0] == "2026-01-01 10:00:00+00"

        # POSIX sign: '+02:00' is UTC-02.
        cur.execute("set timezone to '+02:00'")
        cur.execute("select '2026-01-01 12:00'::timestamptz::text")
        assert cur.fetchone()[0] == "2026-01-01 12:00:00-02"

        # A named zone carries a DST rule; the same reading differs by season.
        cur.execute("set timezone to 'Europe/Rome'")
        cur.execute("select '2026-01-01 12:00'::timestamptz::text")
        assert cur.fetchone()[0] == "2026-01-01 12:00:00+01"
        cur.execute("select '2026-07-01 12:00'::timestamptz::text")
        assert cur.fetchone()[0] == "2026-07-01 12:00:00+02"

        # An offset may carry seconds.
        cur.execute("set timezone to 'UTC'")
        cur.execute("select '2000-01-01 00:00+01:02:03'::timestamptz::text")
        assert cur.fetchone()[0] == "1999-12-31 22:57:57+00"

        # Its own oid, so a client builds an aware datetime rather than a naive
        # one from the same characters.
        cur.execute("select '2026-01-01 12:00'::timestamptz")
        assert cur.description[0].type_code == 1184
        cur.execute("select pg_typeof('2026-01-01'::timestamptz)::text")
        assert cur.fetchone()[0] == "timestamp with time zone"
        cur.execute("select pg_typeof('12:00'::timetz)::text")
        assert cur.fetchone()[0] == "time with time zone"
        cur.execute("select '12:00+02'::timetz::text")
        assert cur.fetchone()[0] == "12:00:00+02"


def test_bound_aware_datetimes_keep_their_instant(home: Path) -> None:
    """A bound aware datetime must name the same instant PostgreSQL would.

    psycopg sends these in the binary format by default — 8 bytes of
    microseconds from 2000-01-01 — so the text and binary paths are checked
    separately.
    """
    aware = dt.datetime(2026, 1, 1, 12, 0, tzinfo=dt.timezone.utc)
    with _Server(home) as server, server.connect() as conn:
        for binary in (False, True):
            for zone in ("UTC", "Europe/Rome"):
                conn.cursor().execute(f"set timezone to '{zone}'")
                cur = conn.cursor(binary=binary)
                cur.execute("select %s::timestamptz", (aware,))
                assert cur.fetchone()[0] == aware, (binary, zone)


def test_bound_aware_datetime_equals_literal_no_cast(home: Path) -> None:
    """A bound aware datetime compares equal to the ``::timestamptz`` literal
    for the same instant -- WITHOUT an explicit cast on the parameter.

    This is the shape psycopg's own ``test_dump_datetimetz`` asserts
    (``'<expr>'::timestamptz = %(val)s``). The binary parameter path used to
    ship the instant as session-rendered TEXT, whose zone offset was dropped the
    moment the ``=`` re-coerced it against the literal -- so a binary parameter
    landed two hours (the session offset) off and compared FALSE. Text was fine;
    binary was silently wrong, so both formats are checked. The instants span
    the psycopg corpus edges: year 0001, a sub-second fraction, a seconds-only
    offset, and year 9999.
    """
    utc = dt.timezone.utc
    cases = [
        # (bound UTC instant, literal-with-offset that names the same instant)
        (dt.datetime(1, 1, 1, 12, 0, tzinfo=utc), "0001-01-01 00:00-12:00"),
        (dt.datetime(1999, 12, 31, 22, 0, tzinfo=utc), "2000-01-01 00:00+2"),
        (dt.datetime(2000, 12, 31, 21, 59, 59, 999999, tzinfo=utc), "2000-12-31 23:59:59.999999+2"),
        (dt.datetime(1999, 12, 31, 22, 57, 57, tzinfo=utc), "2000-01-01 00:00+01:02:03"),
        # No offset in the literal -> read in the session zone (+02), so the
        # instant is two hours earlier than the wall clock.
        (dt.datetime(9999, 12, 31, 21, 59, 59, 999999, tzinfo=utc), "9999-12-31 23:59:59.999999"),
    ]
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("set timezone to '-02:00'")
        for binary in (False, True):
            cur = conn.cursor(binary=binary)
            for value, expr in cases:
                cur.execute(f"select '{expr}'::timestamptz = %s", (value,))
                assert cur.fetchone()[0] is True, (binary, expr)


def test_regtype_names_a_type(home: Path) -> None:
    """`'int4'::regtype` is the type it names, printed as PostgreSQL prints it."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for given, want in [("int4", "integer"), ("integer", "integer"), ("text", "text")]:
            cur.execute("select %s::regtype::text", (given,))
            assert cur.fetchone()[0] == want


def test_regclass_names_a_relation(home: Path) -> None:
    """`'t'::regclass` is the relation's oid, printed as its name.

    Measured against PostgreSQL 16: the oid is `pg_class.oid` -- the same
    number `pg_type.oid` carries for the table's row type and `pg_attribute
    .attrelid` keys on -- so `'t1'::regclass::oid` equals both; `::text`
    renders the name (quoted when it needs quoting); a catalog relation has
    its fixed oid (`pg_class` is 1259); an unknown name is 42P01 with the
    PARSED name, a malformed one 42602, and a cast to `regtype` 42846.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table t1 as select 1 as f1")
        cur.execute('create table "Order" (x int)')
        cur.execute("select 't1'::regclass, 't1'::regclass::oid, 'public.t1'::regclass::text")
        assert cur.description[0].type_code == 2205
        name, oid, qualified = cur.fetchone()
        assert (name, qualified) == ("t1", "t1")
        assert isinstance(oid, int) and oid > 0
        cur.execute("select oid from pg_type where typname = 't1'")
        assert cur.fetchone()[0] == oid
        cur.execute("select attname from pg_attribute where attrelid = 't1'::regclass")
        assert cur.fetchall() == [("f1",)]
        cur.execute("select 't1'::regclass = 't1'::regclass::oid, %s::regclass::oid", (str(oid),))
        assert cur.fetchone() == (True, oid)
        cur.execute("select 'pg_class'::regclass::oid, '\"Order\"'::regclass::text")
        assert cur.fetchone() == (1259, '"Order"')
        for sql, sqlstate, message in [
            ("select 'nope'::regclass", "42P01", 'relation "nope" does not exist'),
            ("select 'Nope.T1'::regclass", "42P01", 'relation "nope.t1" does not exist'),
            ("select 'Order'::regclass", "42P01", 'relation "order" does not exist'),
            ("select 'a b'::regclass", "42602", "invalid name syntax"),
            ("select 't1'::regclass::regtype", "42846", "cannot cast type regclass to regtype"),
            ("select 1.5::regclass", "42846", "cannot cast type numeric to regclass"),
        ]:
            with pytest.raises(psycopg.Error) as info:
                cur.execute(sql)
            assert info.value.sqlstate == sqlstate, sql
            assert message in str(info.value), sql


def test_row_description_names_each_columns_source_table(home: Path) -> None:
    """`RowDescription` carries the table oid and attnum a column is read
    from (`PQftable` / `PQftablecol`); a computed column reports 0 / 0.

    Measured against PostgreSQL 16 through an alias, a two-table FROM,
    `SELECT *`, `INSERT ... RETURNING` and the extended protocol alike.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table t1 as select 1 as f1")
        cur.execute("create table t2 as select 2 as f2, 3 as f3")
        cur.execute("select 't1'::regclass::oid, 't2'::regclass::oid")
        t1, t2 = cur.fetchone()

        def sources(pgresult) -> list[tuple[int, int]]:
            return [(pgresult.ftable(i), pgresult.ftablecol(i)) for i in range(pgresult.nfields)]

        # The simple protocol, exactly psycopg's own test_ftable_and_col.
        res = conn.pgconn.exec_(
            b"select f1, f3, 't1'::regclass::oid, 't2'::regclass::oid from t1, t2"
        )
        assert sources(res) == [(t1, 1), (t2, 2), (0, 0), (0, 0)]
        assert [res.get_value(0, i) for i in range(4)] == [
            b"1",
            b"3",
            str(t1).encode(),
            str(t2).encode(),
        ]

        # The extended protocol describes the same fields.
        cur.execute("select a.f1 as x, f1 + 1, f1::int, * from t1 a")
        assert sources(cur.pgresult) == [(t1, 1), (0, 0), (0, 0), (t1, 1)]
        cur.execute("select t2.f3, t1.f1 from t1 left join t2 on t1.f1 = t2.f2")
        assert sources(cur.pgresult) == [(t2, 2), (t1, 1)]
        cur.execute("insert into t1 values (2) returning f1, f1 + 1")
        assert sources(cur.pgresult) == [(t1, 1), (0, 0)]


def test_timetz_columns_are_session_independent(home: Path) -> None:
    """A `timetz` column stores its LITERAL offset, stable under any zone.

    Unlike `timestamptz` (whose instant renders in the session zone -- see
    `test_timestamptz_columns_render_in_session_zone`), a `timetz` offset is
    literal: `12:34:56+02` reads back the same under any `SET timezone`, so its
    canonical text is a safe column.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table u (id int primary key, tt timetz)")
        cur.execute("insert into u values (1, '12:34:56+02')")
        for zone in ("UTC", "Asia/Tokyo"):
            cur.execute(f"set timezone = '{zone}'")
            cur.execute("select tt::text from u")
            assert cur.fetchone()[0] == "12:34:56+02", zone


def test_bytea_type_and_functions(home: Path) -> None:
    """`bytea` as a real type: literals, a column, and the byte functions.

    Stored as a `bson.Binary` (the representation the Python server uses, since
    both share one store), reported with oid 17 so psycopg hands back `bytes`,
    and rendered to text as the `\\x…` hex PostgreSQL emits. psycopg sends and
    reads a bytea in the BINARY wire format by default, so the round-trip
    exercises both the binary parameter decoder and the binary result encoder.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        # Hex and escape input both parse; output is always hex.
        cur.execute("select '\\x0102ff'::bytea::text")
        assert cur.fetchone()[0] == "\\x0102ff"
        cur.execute("select 'ab\\001c'::bytea::text")
        assert cur.fetchone()[0] == "\\x61620163"

        # A real column, round-tripped through the binary wire format.
        cur.execute("create table b (id int primary key, v bytea)")
        cur.execute("insert into b values (1, %s)", (b"\xca\xfe\x00\x01",))
        cur.execute("select v from b")
        row = cur.fetchone()
        assert bytes(row[0]) == b"\xca\xfe\x00\x01"
        assert cur.description[0].type_code == 17

        # The byte functions.
        cur.execute("select length('\\x0102ff'::bytea), get_byte('\\x0102ff'::bytea, 2)")
        assert cur.fetchone() == (3, 255)
        cur.execute("select set_byte('\\x0102ff'::bytea, 1, 64)::text")
        assert cur.fetchone()[0] == "\\x0140ff"
        cur.execute("select encode('\\x0102ff'::bytea, 'base64')")
        assert cur.fetchone()[0] == "AQL/"
        cur.execute("select decode('AQL/', 'base64')::text")
        assert cur.fetchone()[0] == "\\x0102ff"
        cur.execute("select ('\\x0102'::bytea || '\\xff'::bytea)::text")
        assert cur.fetchone()[0] == "\\x0102ff"

        # Malformed input and an out-of-range index are distinct SQLSTATEs.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select '\\xZZ'::bytea")
        assert exc.value.diag.sqlstate == "22023"
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select get_byte('\\x0102'::bytea, 5)")
        assert exc.value.diag.sqlstate == "2202E"


def test_inet_and_cidr_columns(home: Path) -> None:
    """`inet` (oid 869) and `cidr` (oid 650) as real column types.

    Stored as canonical `addr/masklen` text (the Python server's form, one
    shared store) and sent in the BINARY wire format psycopg uses for these
    oids, so a `/32` host comes back as an `IPv4Address` while a shorter prefix
    is an `IPv4Interface` -- the same distinction PostgreSQL makes. `cidr` is
    strict: host bits below the netmask are rejected.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table n (id int primary key, a inet, b cidr)")
        cur.execute(
            "insert into n values (1, '1.2.3.4', '10.0.0.0/8'),"
            " (2, '2001:db8::1/64', '2001:db8::/32'), (3, '1.2.3.4/24', '192.168.1.0/24')"
        )
        cur.execute("select a, b from n order by id")
        rows = cur.fetchall()
        assert rows[0] == (ipaddress.ip_address("1.2.3.4"), ipaddress.ip_network("10.0.0.0/8"))
        assert rows[1] == (
            ipaddress.ip_interface("2001:db8::1/64"),
            ipaddress.ip_network("2001:db8::/32"),
        )
        assert rows[2] == (
            ipaddress.ip_interface("1.2.3.4/24"),
            ipaddress.ip_network("192.168.1.0/24"),
        )
        assert [c.type_code for c in cur.description] == [869, 650]

        # The `::text` cast is network_show -- it always keeps the mask, even a
        # full-host /32, unlike inet_out (which the column read above drops).
        cur.execute("select '1.2.3.4'::inet::text")
        assert cur.fetchone()[0] == "1.2.3.4/32"

        # A malformed address, and a cidr with host bits set, are both 22P02.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select 'notanip'::inet")
        assert exc.value.diag.sqlstate == "22P02"
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select '10.1.2.3/8'::cidr")
        assert exc.value.diag.sqlstate == "22P02"


def test_interval_keeps_three_independent_parts(home: Path) -> None:
    """An interval is months, days and microseconds — separately.

    They cannot be collapsed into one number because a month is 28–31 days
    depending on where you start: `2026-01-31 + '1 mon'` is `2026-02-28`, which
    no fixed count of microseconds expresses. Comparison, by contrast, *does*
    flatten them (30-day months, 24-hour days), so `'1 mon' = '30 days'` is
    true while `+ '1 mon'` and `+ '30 days'` land on different dates.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        cur.execute("select '1d 3h 4m 5.678s'::interval::text")
        assert cur.fetchone()[0] == "1 day 03:04:05.678"
        cur.execute("select 'P1Y2M3D'::interval::text")
        assert cur.fetchone()[0] == "1 year 2 mons 3 days"
        # A negative value pluralises — this is PostgreSQL's own spelling.
        cur.execute("select '-1 day'::interval::text")
        assert cur.fetchone()[0] == "-1 days"
        # The time part is not a clock and may pass 24 hours.
        cur.execute("select '25:00:00'::interval::text")
        assert cur.fetchone()[0] == "25:00:00"

        # Units that end in `s` are not plurals of something shorter.
        cur.execute("select '500 ms'::interval::text, '5 s'::interval::text")
        assert cur.fetchone() == ("00:00:00.5", "00:00:05")

        cur.execute("select '1 mon'::interval = '30 days'::interval")
        assert cur.fetchone()[0] is True
        cur.execute("select ('2026-01-31'::timestamp + '1 mon'::interval)::text")
        assert cur.fetchone()[0] == "2026-02-28 00:00:00"
        cur.execute("select ('2026-01-31'::timestamp + '30 days'::interval)::text")
        assert cur.fetchone()[0] == "2026-03-02 00:00:00"

        cur.execute("select pg_typeof('1 day'::interval)::text")
        assert cur.fetchone()[0] == "interval"


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_bound_interval_round_trips(home: Path, binary: bool) -> None:
    """A bound `timedelta` survives both wire formats.

    The binary form is three parts too — microseconds, days, months — for the
    same reason the value is.
    """
    with _Server(home) as server, server.connect() as conn:
        for value in (
            dt.timedelta(days=1),
            dt.timedelta(days=1, hours=2, minutes=3, seconds=4),
            dt.timedelta(seconds=-1),
            dt.timedelta(microseconds=500000),
            dt.timedelta(0),
        ):
            cur = conn.cursor(binary=binary)
            cur.execute("select %s::interval", (value,))
            assert cur.fetchone()[0] == value


def test_decimal_arithmetic_works_and_stays_exact(home: Path) -> None:
    """Regression: arithmetic on decimal literals must work at all.

    When decimal literals became `numeric` rather than floats, every arithmetic
    operator on them started refusing outright — `select 1.5 + 1.5` was an
    error — and nothing caught it. The exactness is the point of the type:
    `0.1 + 0.2` is `0.3`, and the result *scale* is part of the answer, so
    `1.50 + 1.5` is `3.00` while `1.5 + 1.5` is `3.0`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr, want in [
            ("1.5 + 1.5", "3.0"),
            ("1.50 + 1.5", "3.00"),
            ("0.1 + 0.2", "0.3"),
            ("2.5 * 2", "5.0"),
            ("1.50 * 1.50", "2.2500"),
            ("2.00 - 1.0", "1.00"),
            ("-1::numeric", "-1"),
        ]:
            cur.execute(f"select ({expr})::text")
            assert cur.fetchone()[0] == want, expr

        # More digits than a float holds: these are the same f64 and different
        # numerics, so the comparison must not go through one.
        cur.execute("select '12345678901234567890.1'::numeric < '12345678901234567890.2'::numeric")
        assert cur.fetchone()[0] is True

        # Division follows PostgreSQL's result-scale rule (measured on 16):
        # at least 16 fractional digits, more when the operands carry them.
        for expr, want in [
            ("1.5::numeric / 3", "0.50000000000000000000"),
            ("1::numeric / 3", "0.33333333333333333333"),
            ("10::numeric / 4", "2.5000000000000000"),
            ("100::numeric / 7", "14.2857142857142857"),
        ]:
            cur.execute(f"select ({expr})::text")
            assert cur.fetchone()[0] == want, expr


def test_nan_has_a_place_in_the_order(home: Path) -> None:
    """PostgreSQL orders floats totally; IEEE does not.

    NaN equals itself and sorts above every number, infinity included. Rust's
    `partial_cmp` reports each of those comparisons as "no answer", which this
    server turned into an error where PostgreSQL has a result.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr in [
            "'NaN'::float8 = 'NaN'::float8",
            "'NaN'::float8 > 1e308",
            "'NaN'::float8 > 'Infinity'::float8",
            "'Infinity'::float8 > 1e308",
            "-'Infinity'::float8 < -1e308",
            "'NaN'::numeric = 'NaN'::numeric",
        ]:
            cur.execute(f"select {expr}")
            assert cur.fetchone()[0] is True, expr


def test_an_unknown_literal_takes_the_type_beside_it(home: Path) -> None:
    """PostgreSQL resolves an unknown literal to the other operand's type.

    That type then decides both the parse and the error — which is why
    comparing an interval to `'2020-01-01'` is a *bad interval* rather than
    `false`. The rule applies to comparison exactly as it does to arithmetic;
    implementing it for arithmetic alone left five different failures hiding
    behind one "cannot compare these operands" message.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr in [
            "interval '1 day' = '1 day'",
            "'1 day' = interval '1 day'",
            "'2026-01-01'::timestamp = '2026-01-01'",
            "'2026-01-01'::date = '2026-01-01'",
            "ARRAY[1,2] = '{1,2}'",
            "ARRAY['a','b'] = '{a,b}'",
        ]:
            cur.execute(f"select {expr}")
            assert cur.fetchone()[0] is True, expr

        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select interval '1 day' = '2020-01-01'")
        assert exc.value.diag.sqlstate == "22007"


# (sql, argument). Each is bound over BOTH wire formats. The point of these is
# that a parameter's MEANING must not depend on the format it arrived in, and
# the two formats are decoded by separate code.
_TYPED_PARAMS = [
    ("select array['a','b'] = %s", ["a", "b"]),
    ("select '1 day'::interval = %s", dt.timedelta(days=1)),
    ("select '2026-01-01 12:00'::timestamp = %s", dt.datetime(2026, 1, 1, 12, 0)),
    ("select '2026-01-01'::date = %s", dt.date(2026, 1, 1)),
    ("select '12:00'::time = %s", dt.time(12, 0)),
    ("select 1.50::numeric = %s", Decimal("1.5")),
    ("select array[1.5::numeric] = %s", [Decimal("1.5")]),
    ("select array['2026-01-01'::date] = %s", [dt.date(2026, 1, 1)]),
    ("select 'abc' = %s", "abc"),
    ("select 5 = %s", 5),
]


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
@pytest.mark.parametrize("sql,arg", _TYPED_PARAMS, ids=lambda v: str(v)[:34])
def test_typed_parameter_compares_equal_in_both_formats(
    home: Path, sql: str, arg: object, binary: bool
) -> None:
    """A parameter must mean the same thing in either wire format.

    The binary path learned arrays, intervals and timestamps; the text path did
    not, so those values fell through to a plain string and `array[...] = %s`
    compared an array against a string. The error said "cannot compare", which
    pointed at comparison when the cause was one layer earlier, in decoding.

    A client may also leave a parameter's type UNSPECIFIED and let the server
    infer it — psycopg does this for lists and datetimes — in which case the
    value arrives as text whatever the format, and is resolved from the operand
    beside it exactly as a bare literal would be.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("set timezone to 'UTC'")
        cur = conn.cursor(binary=binary)
        cur.execute(sql, (arg,))
        assert cur.fetchone()[0] is True


def test_json_preserves_and_jsonb_normalises(home: Path) -> None:
    """The one difference that matters between the two types.

    `json` validates and stores the text it was given, so whitespace, key order
    and duplicate keys all survive. `jsonb` stores a parsed structure, so it
    comes back with keys sorted, the last of any duplicate pair kept, and one
    canonical spacing.

    Keys sort by BYTE length first and then bytewise, which is neither
    lexicographic nor by character count: `z` (one byte) precedes `é` (two).
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        cur.execute("""select '{"b":1, "a":2}'::json::text""")
        assert cur.fetchone()[0] == '{"b":1, "a":2}'
        cur.execute("""select '{"b":1, "a":2}'::jsonb::text""")
        assert cur.fetchone()[0] == '{"a": 2, "b": 1}'

        cur.execute("""select '{"a":1, "a":2}'::jsonb::text""")
        assert cur.fetchone()[0] == '{"a": 2}'
        cur.execute("""select '{"aa":1,"ab":2,"b":3}'::jsonb::text""")
        assert cur.fetchone()[0] == '{"b": 3, "aa": 1, "ab": 2}'
        cur.execute("""select '{"é":1,"z":2}'::jsonb::text""")
        assert cur.fetchone()[0] == '{"z": 2, "é": 1}'

        # Their own oids, so a client decodes each as the type it is.
        cur.execute("select '{}'::json")
        assert cur.description[0].type_code == 114
        cur.execute("select '{}'::jsonb")
        assert cur.description[0].type_code == 3802


def test_json_binary_wire_form(home: Path) -> None:
    """json and jsonb have a binary wire form, and it follows `client_encoding`.

    PostgreSQL's `json_send` is the text verbatim and `jsonb_send` prefixes a
    one-byte format version (`1`); both are transcoded to the session encoding
    like any text. Before this the server had no binary codec for either, so a
    binary-format `COPY TO` of a json column failed with 22P03 -- and did so
    AFTER CopyOutResponse, which psycopg reports as "cannot mix COPY with other
    operations" (measured against PostgreSQL 16, byte-identical below).
    """
    with _Server(home) as server, server.connect() as conn:
        for jtype, prefix in (("json", b""), ("jsonb", b"\x01")):
            payload = prefix + b'{"a": "\xc3\xa9"}'
            cur = conn.cursor(binary=True)
            cur.execute(f"""select '{{"a": "\u00e9"}}'::{jtype}""")
            assert cur.pgresult.get_value(0, 0) == payload
            assert cur.fetchone()[0] == {"a": "\u00e9"}

            with conn.cursor().copy(
                f"""copy (select '{{"a": "\u00e9"}}'::{jtype}) to stdout (format binary)"""
            ) as cp:
                rows = [bytes(r) for r in cp]
            # signature (11) + flags (4) + extension length (4), then the row:
            # field count (2) + length (4) + payload.
            assert rows[0][19:] == b"\x00\x01" + len(payload).to_bytes(4, "big") + payload
            assert rows[1] == b"\xff\xff"

        # A binary-format PARAMETER takes the same cast as a text one, so a
        # jsonb sent as psycopg's ASCII-escaped dump is normalised to the
        # character (it used to be stored escaped, and compared unequal).
        from psycopg.types.json import Jsonb

        cur = conn.cursor()
        cur.execute(
            """select %b::text, %b::text = '"\u00e0\u20ac"'::jsonb::text""",
            (Jsonb("\u00e0\u20ac"), Jsonb("\u00e0\u20ac")),
        )
        assert cur.fetchone() == ('"\u00e0\u20ac"', True)

        # (Only the wire bytes: psycopg's json loader decodes as UTF-8 whatever
        # the session encoding, and fails the same way against PostgreSQL.)
        conn.execute("set client_encoding to latin1")
        for jtype, prefix in (("json", b""), ("jsonb", b"\x01")):
            cur = conn.cursor(binary=True)
            cur.execute(f"""select '{{"a": "\u00e9"}}'::{jtype}""")
            assert cur.pgresult.get_value(0, 0) == prefix + b'{"a": "\xe9"}'


def test_jsonb_numbers_are_numerics(home: Path) -> None:
    """A `jsonb` number prints the way a `numeric` does.

    So an exponent is expanded, and a trailing zero written in the literal
    survives — it is the value's scale. Routing numbers through a float would
    give the first and lose the second.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for literal, want in [
            ('{"x": 1.10}', '{"x": 1.10}'),
            ('{"n":-1.5e10}', '{"n": -15000000000}'),
            ('{"n":1e3}', '{"n": 1000}'),
            ('{"n":1.5E-3}', '{"n": 0.0015}'),
        ]:
            cur.execute("select %s::jsonb::text", (literal,))
            assert cur.fetchone()[0] == want, literal


def test_malformed_json_is_refused(home: Path) -> None:
    """Invalid JSON is 22P02, and sniffing must not rescue it.

    `'01'` is the interesting one: a bound parameter whose type the client left
    unspecified used to be sniffed into an integer *before* the cast ran, so
    `'01'::json` became `1` and was accepted — invalid JSON turned valid by a
    guess this server made on the client's behalf. Sniffing now requires the
    number to round-trip to the same text.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for bad in ["{bad}", '{"a":}', "[1,]", "01", '{"a":1} x', "", '{"a" 1}']:
            with pytest.raises(psycopg.Error) as exc:
                cur.execute("select %s::json", (bad,))
            assert exc.value.diag.sqlstate == "22P02", bad


def test_scalar_builtins_are_available_bare(home: Path) -> None:
    """A built-in must work without a cast around it.

    The two routes to a value are not the same code: `select upper('a')::text`
    goes through the expression evaluator, while a bare `select upper('a')`
    goes through the target list. Only the first was wired up at first, and a
    probe whose every case carried a `::text` could not see it — every one of
    these passed while the bare form raised "function upper() is not supported".
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr, want in [
            ("upper('aB')", "AB"),
            ("length('héllo')", 5),
            ("octet_length('héllo')", 6),
            ("md5('a')", "0cc175b9c0f1b6a831c399e269772661"),
            ("chr(233)", "é"),
            ("ascii('é')", 233),
            ("left('abcde',-2)", "abc"),
            ("split_part('a,b,c',',',2)", "b"),
            ("concat('a',null,'b')", "ab"),
            ("greatest(1,null)", 1),
            ("coalesce(null,1)", 1),
            ("nullif(1,2)", 1),
        ]:
            cur.execute(f"select {expr}")
            assert cur.fetchone()[0] == want, expr


def test_scalar_builtins_report_their_result_type(home: Path) -> None:
    """The result TYPE is as much of the answer as the value.

    `sign` answers `float8` even for an integer argument, and `div` answers
    `numeric` because that is the type it is defined on. `nullif` answers its
    left operand's type even when the result is NULL — and a NULL cannot report
    a type, so reading it from the value gave `text` where PostgreSQL gives
    `int4`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr, oid in [
            ("length('abc')", 23),
            ("exp(1)", 701),
            ("sign(-3)", 701),
            ("div(7,3)", 1700),
            ("md5('a')", 25),
            ("starts_with('abc','ab')", 16),
            ("nullif(1,1)", 23),
            ("nullif(1.5,1.5)", 1700),
        ]:
            cur.execute(f"select {expr}")
            assert cur.description[0].type_code == oid, expr


def test_ranges_canonicalise_by_element_type(home: Path) -> None:
    """A range over a discrete type has exactly one spelling.

    PostgreSQL rewrites every bound of a discrete range to `[)`, so `[1,5]` is
    stored and printed as `[1,6)`. Over a continuous type there is no such
    rewrite — there is no "next" number to move the bound to — so
    `[1.0,2.0]::numrange` stays inclusive.

    The split matters because it is what makes two spellings of one range the
    same range: `'[1,5]'::int4range = '[1,6)'::int4range` is true.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("set timezone to 'UTC'")

        for expr, want in [
            ("int4range(1,5)", "[1,5)"),
            ("int4range(1,5,'[]')", "[1,6)"),
            ("'(1,5)'::int4range", "[2,5)"),
            ("'[2026-01-01,2026-01-05]'::daterange", "[2026-01-01,2026-01-06)"),
            # continuous: left alone
            ("'[1.0,2.0]'::numrange", "[1.0,2.0]"),
            # an infinite bound prints as nothing
            ("int4range(null,5)", "(,5)"),
            ("'(,)'::int4range", "(,)"),
            # a range containing nothing is empty however it was written
            ("int4range(1,1)", "empty"),
            # a bound with a space in it is quoted
            (
                "tsrange('2026-01-01','2026-01-02')",
                '["2026-01-01 00:00:00","2026-01-02 00:00:00")',
            ),
        ]:
            cur.execute(f"select ({expr})::text")
            assert cur.fetchone()[0] == want, expr

        cur.execute("select '[1,5]'::int4range = '[1,6)'::int4range")
        assert cur.fetchone()[0] is True

        cur.execute("select int4range(1,5)")
        assert cur.description[0].type_code == 3904  # int4range, not text


def test_range_errors_use_three_different_classes(home: Path) -> None:
    """Three different mistakes, three different SQLSTATEs.

    A crossed bound is a data error, a malformed literal an invalid-text one,
    and bad bound flags a syntax error. Collapsing them onto one code would
    still refuse the query, and would tell the client the wrong thing about why.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr, sqlstate in [
            ("int4range(5,1)", "22000"),
            ("'[5,1)'::int4range", "22000"),
            ("'x'::int4range", "22P02"),
            ("int4range(1,5,'x')", "42601"),
        ]:
            with pytest.raises(psycopg.Error) as exc:
                cur.execute(f"select {expr}")
            assert exc.value.diag.sqlstate == sqlstate, expr


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_range_constructor_takes_bound_parameters(home: Path, binary: bool) -> None:
    """`int4range(%s, %s, %s)` must work — including the bounds argument.

    This is a regression test for a describe-time bug. `Describe` runs before
    `Bind`, so every parameter is NULL when the statement is planned. A NULL
    bounds argument *is* an error in PostgreSQL, and treating it as one at plan
    time failed every parameterised range constructor — with a message that
    quoted this server's own internal placeholder text back at the client.

    The two cases have to be told apart at the AST: a `null` written in the
    query is an error, a not-yet-bound parameter is not.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        for args, want in [
            ((10, 20, "[]"), Range(10, 21, "[)")),
            ((None, None, "()"), Range(None, None, "()")),
            ((10, None, "[)"), Range(10, None, "[)")),
        ]:
            cur.execute("select int4range(%s::int4, %s::int4, %s)", args)
            assert cur.fetchone()[0] == want, args

        # A literal null for the flags is still the error PostgreSQL reports.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select int4range(1,5,null)")
        assert exc.value.diag.sqlstate == "22000"


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_range_values_bind_in_both_formats(home: Path, binary: bool) -> None:
    """A range sent as a parameter, in either wire format.

    The binary form is a flags byte and then each present bound in the
    element's own binary format — so it needs the element decoder, not a
    range-specific one.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("set timezone to 'UTC'")
        cur = conn.cursor(binary=binary)
        for sql, value in [
            ("select %s::int4range", Range(10, 20, "[)")),
            ("select %s::int4range", Range(None, 20, "()")),
            ("select %s::int4range", Range(10, None, "[)")),
            ("select %s::int4range", Range(empty=True)),
            ("select %s::numrange", Range(Decimal("1.5"), Decimal("2.5"), "[]")),
            ("select %s::daterange", Range(dt.date(2026, 1, 1), dt.date(2026, 1, 5), "[)")),
        ]:
            cur.execute(sql, (value,))
            assert cur.fetchone()[0] == value, value


def test_timestamp_range_bounds_with_sub_millisecond_digits(home: Path) -> None:
    """A `tsrange` has to ORDER its own bounds, which needs them comparable.

    A timestamp carrying sub-millisecond digits is stored as a composite, and
    two composites had no comparison arm at all — so building the range failed
    with "comparing timestamp range bounds", a message about ranges for a gap
    that was really in timestamp comparison.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("set timezone to 'UTC'")
        cur.execute("select tsrange('2026-01-01 00:00:00.5','2026-01-02')::text")
        assert cur.fetchone()[0] == '["2026-01-01 00:00:00.5","2026-01-02 00:00:00")'


def test_multiranges_merge_what_touches(home: Path) -> None:
    """A multirange is a normalised set: sorted, empties dropped, and any two
    members that overlap *or merely touch* merged into one.

    Adjacency is the part that is easy to get wrong. `{[1,5),[5,8)}` is
    `{[1,8)}` because nothing lies between them, while `{[1,5),[6,8)}` stays
    two members because 5 does — so the test is "does the next one start at or
    before this one ends", not "do they overlap".
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr, want in [
            ("'{[10,20),[1,5)}'::int4multirange", "{[1,5),[10,20)}"),
            ("'{[1,5),[3,8)}'::int4multirange", "{[1,8)}"),
            ("'{[1,5),[5,8)}'::int4multirange", "{[1,8)}"),
            ("'{[1,5),[6,8)}'::int4multirange", "{[1,5),[6,8)}"),
            ("'{[1,2),[2,3),[3,4)}'::int4multirange", "{[1,4)}"),
            ("'{empty}'::int4multirange", "{}"),
            ("'{[1,5),empty,[10,20)}'::int4multirange", "{[1,5),[10,20)}"),
            # members canonicalise first
            ("'{[1,5]}'::int4multirange", "{[1,6)}"),
            ("'{(,5),[10,)}'::int4multirange", "{(,5),[10,)}"),
            # a continuous element type has no adjacency by stepping
            ("'{[1.0,2.0),[2.0,3.0)}'::nummultirange", "{[1.0,3.0)}"),
            ("'{[1.0,2.0),(2.0,3.0)}'::nummultirange", "{[1.0,2.0),(2.0,3.0)}"),
            ("int4multirange()", "{}"),
            ("int4multirange(int4range(1,5),int4range(10,20))", "{[1,5),[10,20)}"),
        ]:
            cur.execute(f"select ({expr})::text")
            assert cur.fetchone()[0] == want, expr

        cur.execute("select '{}'::int4multirange")
        assert cur.description[0].type_code == 4451  # int4multirange, not text


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_multirange_arrays_report_their_element_type(home: Path, binary: bool) -> None:
    """An ARRAY of multiranges is typed as the multirange's array oid, not text.

    Range arrays already reported their element type; multirange arrays fell
    through to varchar. That broke BOTH formats: in text the client read back a
    bare string instead of parsing into Multirange objects, and in binary a
    varchar column stays on the binary path, where the array value could not be
    sent as a binary varchar at all. Their own array oids keep them on the
    text-format path a non-binary-encodable type is already downgraded onto.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        cur.execute(
            "SELECT ARRAY['{[1,5)}'::int4multirange, '{}'::int4multirange, '{(,)}'::int4multirange]"
        )
        rows = cur.fetchall()
        assert rows == [
            (
                [
                    Multirange([Range(1, 5, "[)")]),
                    Multirange([]),
                    Multirange([Range(None, None, "()")]),
                ],
            )
        ]
        assert cur.description[0].type_code == 6150  # int4multirange[], not varchar


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_multirange_values_bind_in_both_formats(home: Path, binary: bool) -> None:
    """A multirange sent as a parameter, in either wire format.

    Probed alongside the literal form deliberately: the previous batch shipped
    ranges whose literal form was correct in every case and whose parameter
    form was broken in every case, because the probe only covered literals.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("set timezone to 'UTC'")
        cur = conn.cursor(binary=binary)
        for sql, value in [
            ("select %s::int4multirange", Multirange([Range(1, 5, "[)")])),
            (
                "select %s::int4multirange",
                Multirange([Range(1, 5, "[)"), Range(10, 20, "[)")]),
            ),
            ("select %s::int4multirange", Multirange([])),
            (
                "select %s::nummultirange",
                Multirange([Range(Decimal("1.0"), Decimal("2.0"), "[]")]),
            ),
        ]:
            cur.execute(sql, (value,))
            assert cur.fetchone()[0] == value, value

        cur.execute("select int4multirange(%s::int4range)", (Range(1, 5, "[)"),))
        assert cur.fetchone()[0] == Multirange([Range(1, 5, "[)")])


def test_cursors_follow_postgres_positions(home: Path) -> None:
    """DECLARE / FETCH / MOVE / CLOSE, with PostgreSQL's position model.

    The cursor sits *on* a 1-based row, with 0 before the first and ``len + 1``
    after the last. Those two extra positions are not decoration: fetching past
    the end leaves the cursor at ``len + 1``, so a later ``MOVE BACKWARD 2``
    lands on the **last** row rather than the second-to-last. A simpler
    "index of the next row" model gets that wrong by one.

    Two more rules that only a real server tells you: a BACKWARD fetch returns
    its rows in reverse order, nearest first; and ``RELATIVE``/``ABSOLUTE``
    fetch a *single* row — the n-th from here, or the n-th from the start —
    where ``FORWARD``/``BACKWARD`` fetch a run of them.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key, n int)")
        conn.execute("insert into t values (1,10),(2,20),(3,30),(4,40),(5,50)")
        with conn.transaction():
            cur = conn.cursor()
            cur.execute("declare c1 cursor for select id, n from t order by id")
            assert cur.statusmessage == "DECLARE CURSOR"

            cur.execute("fetch 2 from c1")
            assert cur.fetchall() == [(1, 10), (2, 20)]
            assert cur.statusmessage == "FETCH 2"

            cur.execute("fetch all from c1")
            assert cur.fetchall() == [(3, 30), (4, 40), (5, 50)]

            # Past the end: no rows, and the cursor parks *after* the last row.
            cur.execute("fetch 1 from c1")
            assert cur.fetchall() == []
            assert cur.statusmessage == "FETCH 0"

            # ...which is why backing up two lands on the last row, not the
            # second-to-last.
            cur.execute("move backward 2 in c1")
            assert cur.statusmessage == "MOVE 2"
            cur.execute("fetch 1 from c1")
            assert cur.fetchall() == [(5, 50)]

            # A backward fetch reads in reverse, nearest first.
            cur.execute("fetch backward all from c1")
            assert cur.fetchall() == [(4, 40), (3, 30), (2, 20), (1, 10)]

            # RELATIVE and ABSOLUTE fetch ONE row.
            cur.execute("fetch absolute 2 from c1")
            assert cur.fetchall() == [(2, 20)]
            cur.execute("fetch relative 2 from c1")
            assert cur.fetchall() == [(4, 40)]
            # ABSOLUTE counts from the end when negative.
            cur.execute("fetch absolute -1 from c1")
            assert cur.fetchall() == [(5, 50)]

            cur.execute("close c1")
            assert cur.statusmessage == "CLOSE CURSOR"

        # A cursor needs a transaction; outside one it could never be used.
        with pytest.raises(psycopg.Error) as exc:
            conn.execute("declare c2 cursor for select 1")
        assert exc.value.diag.sqlstate == "25P01"

        with pytest.raises(psycopg.Error) as exc:
            conn.execute("fetch 1 from nosuch")
        assert exc.value.diag.sqlstate == "34000"


def test_repeated_fetch_survives_statement_preparation(home: Path) -> None:
    """Regression: a FETCH must describe the cursor's columns.

    psycopg prepares any statement it runs more than five times, and a prepared
    statement is described once and then executed. The describe path had no arm
    for FETCH, so it reported *zero* columns — and the sixth identical fetch in
    a loop sent rows the client had no description for, which is a protocol
    violation rather than a wrong answer ("D message without prior row
    description").

    Reading a cursor in a loop is the ordinary way to use one, so this affected
    the normal case and not an edge of it.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key)")
        conn.execute("insert into t values (1),(2),(3),(4),(5),(6),(7),(8)")
        with conn.transaction():
            cur = conn.cursor()
            cur.execute("declare c1 cursor for select id from t order by id")
            seen = []
            # Well past psycopg's prepare threshold of five.
            for _ in range(8):
                cur.execute("fetch 1 from c1")
                assert cur.description is not None, "no row description"
                seen.extend(r[0] for r in cur.fetchall())
            assert seen == [1, 2, 3, 4, 5, 6, 7, 8]


def test_generate_series_is_a_row_source(home: Path) -> None:
    """`generate_series` in FROM, and everything that composes with it.

    It is a *source* rather than a statement of its own, which is the point:
    ORDER BY, LIMIT, OFFSET and the aggregates all work on the generated rows
    without knowing where they came from.

    Two rules worth pinning: counting up towards a smaller stop produces
    nothing rather than reversing (you need a negative step for that), and a
    zero step is refused with `22023` — a value that cannot work, which
    PostgreSQL separates from its generic data-error class.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        cur.execute("select * from generate_series(1,5)")
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3, 4, 5]
        assert cur.description[0].name == "generate_series"
        assert cur.description[0].type_code == 23

        cur.execute("select * from generate_series(1,10,3)")
        assert [r[0] for r in cur.fetchall()] == [1, 4, 7, 10]
        cur.execute("select * from generate_series(5,1,-2)")
        assert [r[0] for r in cur.fetchall()] == [5, 3, 1]
        # Not reversed — empty.
        cur.execute("select * from generate_series(5,1)")
        assert cur.fetchall() == []

        # The alias renames the column; a column alias beats the table one.
        cur.execute("select * from generate_series(1,3) as g")
        assert cur.description[0].name == "g"
        cur.execute("select * from generate_series(1,3) as g(x)")
        assert cur.description[0].name == "x"

        cur.execute("select * from generate_series(1,3) order by 1 desc")
        assert [r[0] for r in cur.fetchall()] == [3, 2, 1]
        cur.execute("select * from generate_series(1,10) limit 2 offset 3")
        assert [r[0] for r in cur.fetchall()] == [4, 5]

        cur.execute("select count(*) from generate_series(1,100)")
        assert cur.fetchone()[0] == 100
        cur.execute("select sum(g) from generate_series(1,10) g")
        assert cur.fetchone()[0] == 55
        cur.execute("select min(g), max(g) from generate_series(3,7) g")
        assert cur.fetchone() == (3, 7)

        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select * from generate_series(1,5,0)")
        assert exc.value.diag.sqlstate == "22023"


def test_a_cursor_over_generate_series(home: Path) -> None:
    """The two features together, which is how the client corpus uses them.

    A cursor needs rows to scroll over, and `generate_series` is the usual way
    to make them without a table — so cursors could be complete and correct
    while every test that used one still failed.
    """
    with _Server(home) as server, server.connect() as conn, conn.transaction():
        cur = conn.cursor()
        cur.execute("declare c cursor for select * from generate_series(1,10)")
        cur.execute("fetch 3 from c")
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3]
        cur.execute("fetch backward 2 from c")
        assert [r[0] for r in cur.fetchall()] == [2, 1]
        cur.execute("fetch all from c")
        assert [r[0] for r in cur.fetchall()] == list(range(2, 11))
        cur.execute("close c")


def test_pg_cursors_lists_open_cursors(home: Path) -> None:
    """`pg_cursors` reports THIS connection's open cursors.

    psycopg's server cursor reads it two ways: the suite queries it directly to
    prove a cursor is gone after ``CLOSE``, and the driver itself probes
    ``SELECT 1 FROM pg_catalog.pg_cursors WHERE name = ...`` before closing a
    cursor it did not declare. Both the ``*`` and the ``SELECT 1`` shapes must
    work, and CLOSE must make the row disappear.
    """
    with _Server(home) as server, server.connect() as conn, conn.transaction():
        cur = conn.cursor()
        cur.execute("declare mycur no scroll cursor for select generate_series(1, 5)")

        row = conn.execute(
            "select name, statement, is_holdable, is_binary, is_scrollable "
            "from pg_cursors where name = 'mycur'"
        ).fetchone()
        assert row is not None
        assert row[0] == "mycur"
        assert "generate_series" in row[1]
        assert row[2] is False  # not WITH HOLD
        assert row[3] is False  # not BINARY
        assert row[4] is False  # NO SCROLL

        # The driver's existence probe (the pg_catalog-qualified SELECT 1).
        assert conn.execute(
            "select 1 from pg_catalog.pg_cursors where name = 'mycur'"
        ).fetchone() == (1,)
        # A name that was never declared is simply absent.
        assert conn.execute("select * from pg_cursors where name = 'nope'").fetchone() is None

        cur.execute("close mycur")
        assert conn.execute("select * from pg_cursors where name = 'mycur'").fetchone() is None


def test_constant_select_list_column(home: Path) -> None:
    """`SELECT 1 FROM <source>` — a literal column, one per row.

    The value ignores the row entirely; an unaliased constant is named
    ``?column?`` as PostgreSQL does. This is how a client counts a source's
    rows (``cur.rowcount``) without reading its values, and it works over both a
    real table and a `generate_series`.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key)")
        conn.execute("insert into t values (1),(2),(3)")

        cur = conn.cursor()
        cur.execute("select 1 from t")
        assert cur.fetchall() == [(1,), (1,), (1,)]
        assert cur.rowcount == 3
        assert cur.description[0].name == "?column?"
        assert cur.description[0].type_code == 23  # int4

        # An alias names the column; the value is still the constant.
        cur.execute("select 7 as k from t")
        assert cur.fetchall() == [(7,), (7,), (7,)]
        assert cur.description[0].name == "k"

        # Over a generate_series, including the empty case.
        cur.execute("select 1 from generate_series(1, 42)")
        assert cur.rowcount == 42
        cur.execute("select 1 from generate_series(1, 0)")
        assert cur.fetchall() == []
        assert cur.rowcount == 0


def test_copy_out_formats_keep_null_and_empty_apart(home: Path) -> None:
    """COPY's three formats differ mainly in how they spell NULL.

    * text writes ``\\N`` for NULL; an empty string is an empty field.
    * CSV writes NULL as an *unquoted* empty field, and therefore has to quote
      the empty string as ``""`` to keep the two apart.
    * binary writes a length of −1 for NULL, where an empty string is length 0.

    Every one of those distinctions was broken at some point in writing this,
    and each broke the same way: NULL and empty string became indistinguishable.
    In binary the cause was match-arm order — a catch-all above the NULL arm
    swallowed it and rendered it as text, which is length 0.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table cp (id int primary key, s text)")
        conn.execute("insert into cp values (1,'plain'),(2,'has,comma'),(5,NULL),(6,'')")

        with conn.cursor().copy("copy cp to stdout") as cp:
            assert b"".join(cp) == b"1\tplain\n2\thas,comma\n5\t\\N\n6\t\n"

        with conn.cursor().copy("copy cp to stdout with (format csv)") as cp:
            assert b"".join(cp) == b'1,plain\n2,"has,comma"\n5,\n6,""\n'

        with conn.cursor().copy("copy cp to stdout with (format binary)") as cp:
            blob = b"".join(cp)
        assert blob.startswith(b"PGCOPY\n\xff\r\n\x00")
        # NULL is a length of -1, not a zero-length value.
        assert b"\xff\xff\xff\xff" in blob
        assert blob.endswith(b"\xff\xff")


def test_copy_out_from_a_query(home: Path) -> None:
    """`COPY (query) TO STDOUT`, including a query with no FROM at all.

    `copy (select 1) to stdout` is the shape clients use to check that a bad
    query is reported properly, so refusing it failed a whole file's worth of
    tests that had nothing to do with COPY.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table cp (id int primary key, s text)")
        conn.execute("insert into cp values (1,'a'),(2,'b'),(3,'c')")

        with conn.cursor().copy("copy (select id from cp order by id) to stdout") as cp:
            assert b"".join(cp) == b"1\n2\n3\n"
        with conn.cursor().copy("copy (select id from cp order by id limit 2) to stdout") as cp:
            assert b"".join(cp) == b"1\n2\n"
        # A generated source works too, since COPY reuses the SELECT path.
        with conn.cursor().copy("copy (select * from generate_series(1,3)) to stdout") as cp:
            assert b"".join(cp) == b"1\n2\n3\n"
        # No FROM at all.
        with conn.cursor().copy("copy (select 1) to stdout") as cp:
            assert b"".join(cp) == b"1\n"


@pytest.mark.parametrize(
    "fmt,data",
    [
        ("", b"1\tplain\n2\thas,comma\n5\t\\N\n6\t\n"),
        ("with (format csv)", b'1,plain\n2,"has,comma"\n5,\n6,""\n'),
    ],
    ids=["text", "csv"],
)
def test_copy_in_formats(home: Path, fmt: str, data: bytes) -> None:
    """COPY FROM in both textual formats, with the NULL rules reversed.

    The CSV parser cannot find rows by splitting on newlines first: a newline
    inside quotes is data.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key, s text)")
        with conn.cursor().copy(f"copy t from stdin {fmt}") as cp:
            cp.write(data)
        cur = conn.cursor()
        cur.execute("select id, s from t order by id")
        assert cur.fetchall() == [(1, "plain"), (2, "has,comma"), (5, None), (6, "")]


def test_copy_binary_round_trips(home: Path) -> None:
    """Binary out and back in, through NULL and empty-string values.

    The input side reuses the same per-type decoder as a bound binary
    parameter — the bytes on the wire are identical, so a second
    implementation could only drift from the first.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table a (id int primary key, s text, n int)")
        conn.execute("insert into a values (1,'x',10),(2,NULL,NULL),(3,'',30)")
        conn.execute("create table b (id int primary key, s text, n int)")

        with conn.cursor().copy("copy a to stdout with (format binary)") as cp:
            blob = b"".join(cp)
        with conn.cursor().copy("copy b from stdin with (format binary)") as cp:
            cp.write(blob)

        cur = conn.cursor()
        cur.execute("select id, s, n from b order by id")
        assert cur.fetchall() == [(1, "x", 10), (2, None, None), (3, "", 30)]


def test_copy_from_stdin_serial_column_stores_integers(home: Path) -> None:
    """A `serial` column typed through COPY stores an integer, not a string.

    `serial` is a PostgreSQL pseudo-type: it resolves to `int4`. Before the
    catalog normalised it the column stayed typed `serial`, so a COPY field
    parsed as text and `40010` came back as the string ``"40010"`` — the shape
    psycopg's `test_copy_in_*` cluster caught.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE s (col1 serial primary key, col2 int, data text)")
        with cur.copy("COPY s (col1, col2, data) FROM STDIN") as cp:
            cp.write("40010\t40020\thello\n40040\t\\N\tworld\n")
        cur.execute("SELECT col1, col2, data FROM s ORDER BY col1")
        assert cur.fetchall() == [(40010, 40020, "hello"), (40040, None, "world")]


def test_copy_out_of_a_values_query(home: Path) -> None:
    """`COPY (VALUES ...) TO STDOUT` reads every literal row.

    A bare multi-row ``VALUES`` list is planned as a `ValuesConstant`. Before
    that it fell through the single-row constant path and produced no rows, so
    `copy (values (1),(2)) to stdout` returned an empty body.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        q = (
            "copy (values (40010::int, 40020::int, 'hello'::text), "
            "(40040, NULL, 'world')) to stdout"
        )
        with cur.copy(q) as cp:
            assert b"".join(cp) == b"40010\t40020\thello\n40040\t\\N\tworld\n"
        # The same VALUES executed directly returns its rows, correctly typed.
        cur.execute("values (1, 'a'), (2, 'b')")
        assert cur.fetchall() == [(1, "a"), (2, "b")]


def test_copy_text_round_trips_control_characters(home: Path) -> None:
    """Text COPY preserves the C0 control bytes PostgreSQL backslash-escapes.

    psycopg escapes exactly ``\\b \\t \\n \\v \\f \\r \\\\`` on the way out;
    reading back only ``\\t \\n \\r \\\\`` turned a backspace into a literal
    ``b`` — silent data corruption of any text carrying those bytes.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE c (id int primary key, data text)")
        payload = "".join(chr(b) for b in range(1, 32)) + "\\end"
        with cur.copy("COPY c (id, data) FROM STDIN") as cp:
            cp.write_row([1, payload])
        cur.execute("SELECT data FROM c WHERE id = 1")
        assert cur.fetchone() == (payload,)
        # And it survives a full COPY TO -> COPY FROM round trip.
        with cur.copy("COPY c TO STDOUT") as cp:
            blob = b"".join(cp)
        cur.execute("DELETE FROM c")
        with cur.copy("COPY c FROM STDIN") as cp:
            cp.write(blob)
        cur.execute("SELECT data FROM c WHERE id = 1")
        assert cur.fetchone() == (payload,)


def test_a_table_created_in_a_transaction_is_visible_to_it(home: Path) -> None:
    """`CREATE TABLE` then use it, without committing in between.

    Planning resolves table names against the catalog, and the catalog is an
    ordinary table — so an uncommitted `CREATE TABLE` was invisible to a plain
    read and the next statement reported that the relation did not exist.

    *Execution* already ran inside the transaction, so selecting from a
    pre-existing table worked and hid this completely. What it broke was the
    ordinary shape of a test fixture: create a table, use it, roll back. It
    accounted for 184 psycopg failures on its own.

    Note what this does NOT claim: that the rollback undoes the CREATE. It does
    not — DDL is not transactional on this server, which is a real divergence
    from PostgreSQL and is recorded in `tasks/backlog.md`.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.autocommit = False
        cur = conn.cursor()

        cur.execute("create table t (id int primary key, s text)")
        cur.execute("select id from t")
        assert cur.fetchall() == []

        cur.execute("insert into t values (1,'x')")
        cur.execute("select id, s from t")
        assert cur.fetchall() == [(1, "x")]

        # COPY into it, still uncommitted. Its rows have to be written inside
        # the transaction like any other write: written outside it, they
        # blocked against the transaction's own locks and hung the connection —
        # a deadlock nobody had seen, because resolving the table failed first
        # whenever a transaction was open.
        with conn.cursor().copy("copy t from stdin") as cp:
            cp.write(b"2\ty\n")
        cur.execute("select count(*) from t")
        assert cur.fetchone()[0] == 2
        conn.rollback()


def test_a_table_dropped_in_a_transaction_is_hidden_from_it(home: Path) -> None:
    """The mirror case: a DROP that has not committed must also be seen.

    The catalog row is deleted but not committed, so a plain read still finds
    it — which would let a statement plan against a table the transaction has
    already dropped.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key)")
        conn.autocommit = False
        cur = conn.cursor()
        cur.execute("drop table t")
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select id from t")
        assert exc.value.diag.sqlstate == "42P01"
        conn.rollback()


def test_order_by_output_position(home: Path) -> None:
    """`ORDER BY 1` is the first output COLUMN, not the constant 1.

    It is an ordinal into the select list, so it has to be resolved against the
    output columns rather than against the table — `select b, a from t order by
    1` orders by `b`. A position with no such column is PostgreSQL's own
    `42P10`.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key, a int, b text)")
        conn.execute("insert into t values (1,3,'c'),(2,1,'a'),(3,2,'b')")
        cur = conn.cursor()

        cur.execute("select a, b from t order by 1")
        assert cur.fetchall() == [(1, "a"), (2, "b"), (3, "c")]
        cur.execute("select a, b from t order by 2 desc")
        assert cur.fetchall() == [(3, "c"), (2, "b"), (1, "a")]
        # The ordinal follows the select list, not the table.
        cur.execute("select b, a from t order by 1")
        assert cur.fetchall() == [("a", 1), ("b", 2), ("c", 3)]

        for bad in ("select a from t order by 5", "select a from t order by 0"):
            with pytest.raises(psycopg.Error) as exc:
                cur.execute(bad)
            assert exc.value.diag.sqlstate == "42P10", bad


def test_generate_series_in_the_select_list(home: Path) -> None:
    """`select generate_series(1,3)` is three rows, not one.

    A set-returning function in the target list of a FROM-less query expands
    into rows, so it is planned as a select over a generated source — the same
    shape `FROM generate_series(...)` produces. Clients reach for this form
    constantly to make rows without a table, and a server cursor over it was
    the single biggest use in psycopg's suite.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select generate_series(1,3)")
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3]
        assert cur.description[0].name == "generate_series"

        cur.execute("select generate_series(1,3) as g")
        assert cur.description[0].name == "g"
        # Counting up towards a smaller stop is still empty, not reversed.
        cur.execute("select generate_series(3,1)")
        assert cur.fetchall() == []
        cur.execute("select generate_series(1,10) order by 1 desc limit 3")
        assert [r[0] for r in cur.fetchall()] == [10, 9, 8]

        # A server cursor over it, which is how the corpus uses the pair.
        with conn.transaction():
            cur.execute("declare c cursor for select generate_series(1,5)")
            cur.execute("fetch 2 from c")
            assert [r[0] for r in cur.fetchall()] == [1, 2]
            cur.execute("close c")


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_multiranges_bind_in_binary_too(home: Path, binary: bool) -> None:
    """A multirange sent as a bound parameter in either format.

    The binary layout is a count of ranges followed by each one length-prefixed
    in the *range's* own binary form, so it reuses the range decoder rather
    than repeating the flags-and-bounds layout.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("set timezone to 'UTC'")
        cur = conn.cursor(binary=binary)
        for sql, value in [
            ("select %s::int4multirange", Multirange([Range(1, 5, "[)")])),
            (
                "select %s::int4multirange",
                Multirange([Range(1, 5, "[)"), Range(10, 20, "[)")]),
            ),
            ("select %s::int4multirange", Multirange([])),
            (
                "select %s::nummultirange",
                Multirange([Range(Decimal("1.0"), Decimal("2.0"), "[]")]),
            ),
        ]:
            cur.execute(sql, (value,))
            assert cur.fetchone()[0] == value, value


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        ("select 1::int2", 1),
        ("select 1::int4", 1),
        ("select 1::int8", 1),
        ("select 1.5::float4", 1.5),
        ("select 1.5::float8", 1.5),
        ("select true", True),
        ("select 'x'::text", "x"),
        ("select 'x'::varchar", "x"),
        ("select 1.50::numeric", Decimal("1.50")),
        ("select (-1.5)::numeric", Decimal("-1.5")),
        ("select 0.00001::numeric", Decimal("0.00001")),
        ("select 100000::numeric", Decimal("100000")),
        ("select 12345678901234567890.123::numeric", Decimal("12345678901234567890.123")),
        ("select 'NaN'::numeric", Decimal("NaN")),
        ("select array[1,2,3]::int4[]", [1, 2, 3]),
        ("select array['a','b']::text[]", ["a", "b"]),
        ("select array[1.50]::numeric[]", [Decimal("1.50")]),
        ("select null::int4", None),
        ("select null::numeric", None),
    ],
)
def test_binary_result_columns(home: Path, sql: str, want: object) -> None:
    """A cursor that asks for BINARY results is answered in binary.

    The values were always right -- the server described every column as text
    and the client duly decoded text -- so this asserts the FORMAT as well as
    the value, which is the only way the divergence shows up at all.
    """
    with _Server(home) as server, server.connect() as conn:
        for binary in (False, True):
            cur = conn.cursor(binary=binary)
            cur.execute(sql)
            got = cur.fetchone()[0]
            assert cur.pgresult.fformat(0) == int(binary), sql
            if isinstance(want, float):
                assert got == pytest.approx(want)
            elif isinstance(want, Decimal) and want.is_nan():
                assert got.is_nan()
            else:
                assert got == want


def test_box_and_regtype_results_are_binary(home: Path) -> None:
    """`box` and `regtype` -- the last two types described as text when a
    client asked for binary -- are sent in binary, byte for byte as
    PostgreSQL 14 sends them (measured 2026-09-30)."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=True)
        cur.execute("select '(1,2),(3,4)'::box")
        assert cur.pgresult.fformat(0) == 1
        assert cur.pgresult.get_value(0, 0) == (
            b"@\x08\x00\x00\x00\x00\x00\x00@\x10\x00\x00\x00\x00\x00\x00"
            b"?\xf0\x00\x00\x00\x00\x00\x00@\x00\x00\x00\x00\x00\x00\x00"
        )
        cur.execute("select 'int4'::regtype")
        assert cur.pgresult.fformat(0) == 1
        assert cur.pgresult.get_value(0, 0) == (23).to_bytes(4, "big")


def test_a_server_cursor_describes_its_portal(home: Path) -> None:
    """psycopg's server cursors, which describe the cursor's PORTAL.

    PostgreSQL exposes a declared cursor as a portal of the same name, and
    psycopg describes it straight after the DECLARE -- before any FETCH -- to
    learn the columns. Without an answer for that describe every server cursor
    failed on its first row.
    """
    with _Server(home) as server, server.connect() as conn:
        # A server cursor needs a transaction to declare into, which the
        # autocommit connection the other tests use does not have.
        conn.autocommit = False
        with conn.cursor("c1") as cur:
            cur.execute("select * from generate_series(1, 5) as n")
            assert [d.name for d in cur.description] == ["n"]
            assert cur.fetchmany(2) == [(1,), (2,)]
            assert cur.fetchone() == (3,)
            assert cur.fetchall() == [(4,), (5,)]
        conn.rollback()
        with conn.cursor("c2") as cur:
            cur.execute("select * from generate_series(1, 3) as n")
            assert list(cur) == [(1,), (2,), (3,)]


def test_a_failed_transaction_refuses_everything_until_it_ends(home: Path) -> None:
    """PostgreSQL's failed-transaction block, which this server had no notion of.

    An error inside a transaction aborts the block: every later statement is
    refused with `25P02` until the block ends, and a `COMMIT` there is a
    rollback that says so in its tag. Without this a client that shrugged off a
    mid-transaction error went on writing and committed work PostgreSQL would
    have discarded.
    """
    from psycopg.pq import TransactionStatus

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table ft (id int4)")

        cur.execute("begin")
        assert TransactionStatus(conn.info.transaction_status).name == "INTRANS"
        cur.execute("insert into ft values (1)")
        with pytest.raises(psycopg.errors.UndefinedColumn):
            cur.execute("select nosuchcolumn")
        assert TransactionStatus(conn.info.transaction_status).name == "INERROR"

        # Every shape, including the parameterised one — a separate protocol
        # path that would otherwise sail past the gate.
        for sql, params in [
            ("select 1", ()),
            ("insert into ft values (2)", ()),
            ("select %s", (1,)),
            ("set timezone to 'UTC'", ()),
        ]:
            with pytest.raises(psycopg.errors.InFailedSqlTransaction):
                cur.execute(sql, params)

        cur.execute("commit")
        assert cur.statusmessage == "ROLLBACK"
        assert TransactionStatus(conn.info.transaction_status).name == "IDLE"
        cur.execute("select count(*) from ft")
        assert cur.fetchone()[0] == 0

        # An error OUTSIDE a transaction leaves the session usable.
        with pytest.raises(psycopg.errors.UndefinedColumn):
            cur.execute("select nosuchcolumn")
        assert TransactionStatus(conn.info.transaction_status).name == "IDLE"
        cur.execute("select 1")
        assert cur.fetchone() == (1,)

        # …and a clean transaction still commits, with its own tag.
        cur.execute("begin")
        cur.execute("insert into ft values (3)")
        cur.execute("commit")
        assert cur.statusmessage == "COMMIT"
        cur.execute("select count(*) from ft")
        assert cur.fetchone()[0] == 1


def test_a_series_takes_its_bounds_as_parameters(home: Path) -> None:
    """`generate_series(1, %s)` — the way clients actually write one.

    An untyped bound parameter arrives as text, and PostgreSQL resolves it
    against the function's own signature. Refusing it made every parameterised
    series fail, which is most of them.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select * from generate_series(1, %s)", (4,))
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3, 4]
        cur.execute("select * from generate_series(%s, 10, %s)", (1, 3))
        assert [r[0] for r in cur.fetchall()] == [1, 4, 7, 10]
        cur.execute("select generate_series(1, %s)", (3,))
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3]
        cur.execute("select * from generate_series(1, %s) order by 1 desc limit 2", (5,))
        assert [r[0] for r in cur.fetchall()] == [5, 4]

        # A NULL bound is an EMPTY series, not an error.
        for sql, params in [
            ("select * from generate_series(1, %s)", (None,)),
            ("select * from generate_series(1, null)", ()),
            ("select * from generate_series(1, 10, null)", ()),
        ]:
            cur.execute(sql, params)
            assert cur.fetchall() == [], sql

        # A bound that is not an integer, and one whose type has no overload.
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as text_err:
            cur.execute("select * from generate_series(1, %s)", ("x",))
        assert 'invalid input syntax for type integer: "x"' in str(text_err.value)
        with pytest.raises(psycopg.errors.UndefinedFunction):
            cur.execute("select * from generate_series(1, 3::float8)")


def test_savepoints_undo_only_what_came_after_them(home: Path) -> None:
    """SAVEPOINT / RELEASE / ROLLBACK TO, which is how nested blocks are built.

    WiredTiger has no savepoint of its own, so one here is a set of pre-images:
    before a statement writes a table, every open savepoint that has not yet
    captured that table captures it. The capture and the restore both have to
    go through the transaction's own session — a read outside it cannot see the
    block's uncommitted rows, and a write outside it lands in another snapshot.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table sp (id int4)")

        cur.execute("begin")
        cur.execute("insert into sp values (1)")
        cur.execute("savepoint s1")
        assert cur.statusmessage == "SAVEPOINT"
        cur.execute("insert into sp values (2)")
        cur.execute("rollback to savepoint s1")
        assert cur.statusmessage == "ROLLBACK"
        cur.execute("select id from sp order by id")
        assert [r[0] for r in cur.fetchall()] == [1]
        # The savepoint stays open, and captures again from what was restored.
        cur.execute("insert into sp values (3)")
        cur.execute("rollback to savepoint s1")
        cur.execute("select id from sp order by id")
        assert [r[0] for r in cur.fetchall()] == [1]
        cur.execute("commit")

        # RELEASE keeps the inner writes, and the OUTER savepoint must still be
        # able to undo them — the released frame's pre-images merge down.
        cur.execute("delete from sp")
        cur.execute("begin")
        cur.execute("insert into sp values (10)")
        cur.execute("savepoint outer_sp")
        cur.execute("insert into sp values (20)")
        cur.execute("savepoint inner_sp")
        cur.execute("insert into sp values (30)")
        cur.execute("release savepoint inner_sp")
        assert cur.statusmessage == "RELEASE"
        cur.execute("rollback to savepoint outer_sp")
        cur.execute("select id from sp order by id")
        assert [r[0] for r in cur.fetchall()] == [10]
        cur.execute("commit")

        # A DDL statement inside a savepoint is undone with it.
        cur.execute("begin")
        cur.execute("savepoint ddl")
        cur.execute("create table sp2 (id int4)")
        cur.execute("rollback to savepoint ddl")
        with pytest.raises(psycopg.errors.UndefinedTable):
            cur.execute("select * from sp2")
        cur.execute("rollback")

        # Rolling back to a savepoint un-poisons a failed block.
        cur.execute("begin")
        cur.execute("insert into sp values (40)")
        cur.execute("savepoint ok")
        with pytest.raises(psycopg.errors.UndefinedColumn):
            cur.execute("select nosuchcolumn from sp")
        with pytest.raises(psycopg.errors.InFailedSqlTransaction):
            cur.execute("select 1")
        cur.execute("rollback to savepoint ok")
        cur.execute("select 1")
        assert cur.fetchone() == (1,)
        cur.execute("commit")
        cur.execute("select id from sp order by id")
        assert [r[0] for r in cur.fetchall()] == [10, 40]

        # The names that do not exist, and the block that is not open.
        cur.execute("begin")
        with pytest.raises(psycopg.errors.InvalidSavepointSpecification):
            cur.execute("rollback to savepoint nope")
        cur.execute("rollback")
        for sql in ["savepoint outside", "release outside", "rollback to savepoint outside"]:
            with pytest.raises(psycopg.errors.NoActiveSqlTransaction):
                cur.execute(sql)


def test_a_nested_psycopg_transaction_block(home: Path) -> None:
    """The API that actually reaches the savepoint code.

    psycopg turns a nested `conn.transaction()` into a savepoint, so this is
    the shape a client produces without ever writing the word.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.autocommit = True
        conn.cursor().execute("create table nested (id int4)")
        with conn.transaction():
            conn.cursor().execute("insert into nested values (1)")
            with contextlib.suppress(ZeroDivisionError), conn.transaction():
                conn.cursor().execute("insert into nested values (2)")
                raise ZeroDivisionError("undo the inner block only")
        cur = conn.cursor()
        cur.execute("select id from nested order by id")
        assert [r[0] for r in cur.fetchall()] == [1]


def test_create_table_if_not_exists_is_a_no_op(home: Path) -> None:
    """…on a table that is already there, where a bare CREATE is `42P07`.

    The idiomatic "create it if it is missing" fixture ran twice in one session
    and failed the second time.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table if not exists inex (id int4)")
        cur.execute("insert into inex values (1)")
        cur.execute("create table if not exists inex (id int4)")
        assert cur.statusmessage == "CREATE TABLE"
        # …and it did not empty the table.
        cur.execute("select count(*) from inex")
        assert cur.fetchone()[0] == 1
        with pytest.raises(psycopg.errors.DuplicateTable):
            cur.execute("create table inex (id int4)")


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_a_range_bound_as_a_parameter_is_the_same_range(home: Path, binary: bool) -> None:
    """`int4range(10, 20, '[]') = %s` with the same range bound.

    A range over a discrete element type has one true spelling — PostgreSQL
    rewrites every bound to `[)` — so a parameter that keeps the literal the
    client wrote compares unequal to the same range written any other way while
    printing identically. Two routes had to agree: a range parameter that
    arrives with its type decodes through the cast, and one that arrives
    untyped takes its type from the operand beside it.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        for sql, params in [
            ("select int4range(10, 20, '[]') = %s", (Range(10, 20, "[]"),)),
            ("select %s = int4range(10, 21, '[)')", (Range(10, 20, "[]"),)),
            ("select int8range(1, 5, '[]') = %s", (Range(1, 5, "[]"),)),
            # Both sides canonicalise to `[12,21)`; the exclusive lower bound
            # moves up as the inclusive upper one does.
            ("select int4range(11, 20, '(]') = %s", (Range(11, 20, "(]"),)),
            (
                "select numrange(1.0, 2.0, '[]') = %s",
                (Range(Decimal("1.0"), Decimal("2.0"), "[]"),),
            ),
            (
                "select int4multirange(int4range(1, 5, '[]')) = %s",
                (Multirange([Range(1, 5, "[]")]),),
            ),
        ]:
            cur.execute(sql, params)
            assert cur.fetchone()[0] is True, sql


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_a_literal_beside_a_range_array_parameter_takes_its_type(home: Path, binary: bool) -> None:
    """`'{"[1,5]"}' = %s` with a LIST of ranges bound: the literal is an
    `int4range[]` and compares as ranges, not as strings.

    psycopg sends `[Int4Range(...)]` as `_int4range` (oid 3905), and the
    planner had no name for that oid -- so `$1` was typed from its decoded
    value (`text[]`), the untyped literal beside it was never coerced, and the
    comparison was refused as `text = text[]`. Every value here was measured
    against PostgreSQL 16: `[1,5]` canonicalises to `[1,6)` on the way in, so
    it equals a bound `[1,6)` and NOT a bound `[1,5)`, a crossed literal is
    22000, and the parameter reports its array type.
    """
    from psycopg.types.multirange import Int4Multirange
    from psycopg.types.range import Int4Range

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        cur.execute(
            """select '{"[1,5]"}' = %s, pg_typeof(%s)::text""",
            ([Int4Range(1, 5, "[]")], [Int4Range(1, 5, "[]")]),
        )
        assert cur.fetchone() == (True, "int4range[]")
        cur.execute("""select '{"[1,5]"}' = %s""", ([Int4Range(1, 6, "[)")],))
        assert cur.fetchone()[0] is True
        cur.execute("""select '{"[1,5]"}' = %s""", ([Int4Range(1, 5, "[)")],))
        assert cur.fetchone()[0] is False
        # The psycopg gauge's own shape: an empty range and an unbounded one.
        cur.execute(
            """select '{empty,"(,)"}' = %s""",
            ([Int4Range(empty=True), Int4Range(bounds="()")],),
        )
        assert cur.fetchone()[0] is True
        # A multirange array too, through its own array oid.
        mr = [Int4Multirange([Int4Range(1, 6), Int4Range(7, 8)])]
        cur.execute(
            """select '{"{[1,5],[7,8)}"}' = %s, pg_typeof(%s)::text""",
            (mr, mr),
        )
        assert cur.fetchone() == (True, "int4multirange[]")
        # The literal is parsed as the parameter's type, so a crossed pair is
        # the range error, not a string mismatch.
        with pytest.raises(psycopg.errors.DataException) as exc:
            cur.execute("""select '{"[5,1]"}' = %s""", ([Int4Range(1, 5, "[]")],))
        assert "range lower bound must be less than or equal to range upper bound" in str(exc.value)


def test_an_untyped_binary_range_parameter_takes_its_type_from_context(home: Path) -> None:
    """A bare `Range(empty=True)` bound in BINARY beside a typed range.

    psycopg sends an untyped `Range` / `Multirange` with oid 0, and in binary
    that is a flag byte (`\\x01` = empty) or an int32 count -- bytes that are
    only a range once something says which range. PostgreSQL's analysis pass
    gives the parameter the type of the operand it is compared against; the
    server now infers the same from the statement before decoding, so the
    byte reaches the range decoder instead of being read as text (which
    answered False for every one of these). Values measured against
    PostgreSQL 16, including that a parameter with NO context is still 42P18.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=True)
        cur.execute("select 'empty'::int4range = %b", (Range(empty=True),))
        assert cur.fetchone()[0] is True
        cur.execute(
            "select 'empty'::numrange = %b, 'empty'::tstzrange = %b",
            (Range(empty=True), Range(empty=True)),
        )
        assert cur.fetchone() == (True, True)
        cur.execute(
            "select '[1,5)'::int4range = %b, %b = '[1,5)'::int4range",
            (Range(1, 5), Range(1, 5)),
        )
        assert cur.fetchone() == (True, True)
        cur.execute("select '[1,5)'::int4range = %b", (Range(1, 6),))
        assert cur.fetchone()[0] is False
        cur.execute(
            "select int4range(NULL::int4, NULL::int4, '()') = %b",
            (Range(None, None, "()"),),
        )
        assert cur.fetchone()[0] is True
        cur.execute("select '{}'::int4multirange = %b", (Multirange(),))
        assert cur.fetchone()[0] is True
        cur.execute(
            "select int4multirange(%s::int4range) = %b",
            (Range(empty=True), Multirange([Range(empty=True)])),
        )
        assert cur.fetchone()[0] is True
        cur.execute("select '{[1,5)}'::int4multirange = %b", (Multirange([Range(1, 5)]),))
        assert cur.fetchone()[0] is True
        # No context, no type: PostgreSQL refuses, and so do we.
        with pytest.raises(psycopg.errors.IndeterminateDatatype) as exc:
            cur.execute(
                "select 'empty'::int4range = %b, pg_typeof(%b)::text",
                (Range(empty=True), Range(empty=True)),
            )
        assert "could not determine data type of parameter $2" in str(exc.value)


def test_custom_range_types_are_created_fetched_and_round_tripped(home: Path) -> None:
    """`CREATE TYPE ... AS RANGE` with the whole psycopg lifecycle around it.

    psycopg's `RangeInfo.fetch` / `register_range` want the type in the
    catalog with its own oid, its subtype oid, and the auto-created
    multirange pointing back at it -- and then the constructor, the casts and
    the dump / load paths in all three formats have to work on the new type.
    Every value here was measured against PostgreSQL 16 by running the same
    script on both servers (`scratchpad/slicecheck.py` during the slice).
    """
    from psycopg.adapt import Dumper
    from psycopg.pq import Format
    from psycopg.types.multirange import MultirangeInfo, register_multirange
    from psycopg.types.range import RangeInfo, register_range

    with _Server(home) as server, server.connect() as conn:
        conn.execute('create type testrange as range (subtype = text, collation = "C")')
        conn.execute("create schema testschema")
        conn.execute("create type testschema.testrange as range (subtype = float8)")
        info = RangeInfo.fetch(conn, "testrange")
        assert info is not None and info.subtype_oid == 25
        register_range(info, conn)
        sinfo = RangeInfo.fetch(conn, "testschema.testrange")
        assert sinfo is not None and sinfo.subtype_oid == 701 and sinfo.oid != info.oid
        register_range(sinfo, conn)
        minfo = MultirangeInfo.fetch(conn, "testmultirange")
        assert minfo is not None and minfo.range_oid == info.oid
        register_multirange(minfo, conn)

        cur = conn.execute(
            "select 'empty'::testrange, '[a,c]'::testrange, pg_typeof('[a,c]'::testrange)::text"
        )
        assert cur.fetchone() == (Range(empty=True), Range("a", "c", "[]"), "testrange")
        cur = conn.execute("select '{[a,c),[e,f]}'::testmultirange")
        assert cur.fetchone()[0] == Multirange([Range("a", "c", "[)"), Range("e", "f", "[]")])
        cur = conn.execute(
            "select testrange('a', 'c'), testrange('a', 'c', '(]'), "
            "testrange('a', NULL), testrange(NULL, NULL, '()')"
        )
        assert cur.fetchone() == (
            Range("a", "c", "[)"),
            Range("a", "c", "(]"),
            Range("a", None, "[)"),
            Range(None, None, "()"),
        )
        cur = conn.execute(
            "select testschema.testrange(1.5, 2.5), '[1.5,2.5)'::testschema.testrange, "
            "pg_typeof(testschema.testrange(1.5, 2.5))::text"
        )
        assert cur.fetchone() == (
            Range(1.5, 2.5, "[)"),
            Range(1.5, 2.5, "[)"),
            "testschema.testrange",
        )
        # The empty-string bound is a value; the quotes are what keep it
        # from being read as an infinite bound.
        cur = conn.execute(
            """select '["",foo)'::testrange, '["",foo)'::testrange::text, """
            """lower('["",foo)'::testrange)"""
        )
        assert cur.fetchone() == (Range("", "foo", "[)"), '["",foo)', "")
        # Dump / load round-trips in every format, including the gauge's
        # quoting-sweep shape: a `"` bound is rendered as a doubled quote.
        for fmt in ("s", "t", "b"):
            cur = conn.execute(
                f"select %{fmt}, %{fmt} = 'empty'::testrange, %{fmt}::text",
                (Range("a", "c"), Range[str](empty=True), Range('"', "#")),
            )
            assert cur.fetchone() == (Range("a", "c", "[)"), True, '["""",#)'), fmt
            cur = conn.execute(f"select %{fmt}", (Multirange([Range("a", "c"), Range("x", None)]),))
            assert cur.fetchone()[0] == Multirange(
                [Range("a", "c", "[)"), Range("x", None, "[)")]
            ), fmt
        # A one-argument constructor call is the cast of a literal, not a
        # lower bound -- so a bare value is the malformed-literal error.
        cur = conn.execute(
            "select testrange('[a,c)'), int4range('[1,3)'), int4multirange('{[1,2)}')"
        )
        assert cur.fetchone() == (
            Range("a", "c", "[)"),
            Range(1, 3, "[)"),
            Multirange([Range(1, 2, "[)")]),
        )
        for sql, literal in [("select testrange('a')", "a"), ("select int4range('1')", "1")]:
            with pytest.raises(psycopg.errors.InvalidTextRepresentation) as exc:
                conn.execute(sql)
            assert exc.value.sqlstate == "22P02"
            assert str(exc.value).splitlines()[0] == f'malformed range literal: "{literal}"'
        # An infinite bound is always exclusive, whatever the subtype.
        cur = conn.execute(
            "select '[,foo)'::testrange::text, '(,5]'::int4range::text, '[,5]'::numrange::text"
        )
        assert cur.fetchone() == ("(,foo)", "(,6)", "(,5]")

        # With the type registered, `Range[str]` is a testrange and the
        # accessors give its bounds (psycopg's `test_dump_quoting` shape).
        cur = conn.execute(
            "select ascii(lower(%(r)s)) = %(low)s and ascii(upper(%(r)s)) = %(up)s",
            {"r": Range('"', "#"), "low": 34, "up": 35},
        )
        assert cur.fetchone() == (True,)

        # A subtype dumper that turns "" into NULL makes the bound infinite,
        # in text and in binary (psycopg's `test_dump_custom_null` shape).
        class StrNoneDumper(Dumper):
            oid = 25

            def dump(self, obj: str) -> bytes | None:
                return obj.encode() if obj else None

        class StrNoneBinaryDumper(StrNoneDumper):
            format = Format.BINARY

        conn.adapters.register_dumper(str, StrNoneDumper)
        conn.adapters.register_dumper(str, StrNoneBinaryDumper)
        for fmt in ("s", "t", "b"):
            cur = conn.execute(f"select %{fmt}::testrange", (Range[str]("", "foo"),))
            assert cur.fetchone()[0] == Range(None, "foo", "()"), fmt


def test_range_accessors_boolean_expressions_and_copy_canonical_form(home: Path) -> None:
    """The pieces the psycopg quoting sweep and COPY tests lean on.

    `lower` / `upper` and the five predicates over every range family
    (NULL for an empty or infinite bound, typed by the subtype), the
    three-valued AND / OR / NOT in a FROM-less select with PostgreSQL's
    42804 for a non-boolean operand and 22P02 for an untyped literal that
    is not a boolean, `ascii` typed as int4, and COPY storing the canonical
    form (`{empty}` is `{}`, `[1,5]` is `[1,6)`). Values measured against
    PostgreSQL 16.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.execute(
            "select lower('[1,5)'::int4range), upper('[1,5)'::int4range), "
            "pg_typeof(lower('[1,5)'::int4range))::text"
        )
        assert cur.fetchone() == (1, 5, "integer")
        cur = conn.execute(
            "select lower('empty'::int4range), upper('empty'::int4range), "
            "lower('(,5)'::int4range), upper('[1,)'::int4range)"
        )
        assert cur.fetchone() == (None, None, None, None)
        cur = conn.execute(
            "select lower_inc('[1,5)'::int4range), upper_inc('[1,5)'::int4range), "
            "lower_inf('(,5)'::int4range), upper_inf('(,5)'::int4range), "
            "isempty('empty'::int4range), isempty('[1,5)'::int4range)"
        )
        assert cur.fetchone() == (True, False, True, False, True, False)
        cur = conn.execute(
            "select lower_inc('empty'::int4range), lower_inf('empty'::int4range), "
            "upper_inf('empty'::int4range)"
        )
        assert cur.fetchone() == (False, False, False)
        cur = conn.execute(
            "select lower('[1.5,2.5]'::numrange), upper('(,)'::numrange), "
            "lower('[2024-01-01,2024-02-01)'::daterange)::text"
        )
        assert cur.fetchone() == (Decimal("1.5"), None, "2024-01-01")
        cur = conn.execute(
            "select lower('{[1,3),[5,7)}'::int4multirange), "
            "upper('{[1,3),[5,7)}'::int4multirange), "
            "isempty('{}'::int4multirange), lower('{}'::int4multirange)"
        )
        assert cur.fetchone() == (1, 7, True, None)
        # An untyped Range goes over as oid 0, and `lower(unknown)` is the
        # TEXT lower -- the range literal itself, case-folded.
        cur = conn.execute(
            "select lower(%s), upper(%s), lower(%s)",
            (Range(1, 5), Range("a", "c"), Range[str](empty=True)),
        )
        assert cur.fetchone() == ("[1,5)", "[A,C)", "empty")
        cur = conn.execute("select lower(NULL::int4range), lower_inc(NULL::int4range)")
        assert cur.fetchone() == (None, None)

        cur = conn.execute(
            "select 1 = 1 and 2 = 2, true or false, not true, 1=1 and 2=3 or 3=3, "
            "true and null, null or true, not null, false and null, null or false"
        )
        assert cur.fetchone() == (True, True, False, True, None, True, None, False, None)
        # Unregistered, the range is text and `lower` / `upper` fold the
        # literal, so `ascii` sees its `[`.
        cur = conn.execute(
            "select ascii(lower(%(r)s)), ascii(upper(%(r)s))", {"r": Range('"', "#")}
        )
        assert cur.fetchone() == (91, 91)
        cur = conn.execute("select ascii(%s)", ("[",))
        assert cur.fetchone() == (91,)
        assert cur.description[0].type_code == 23
        with pytest.raises(psycopg.errors.DatatypeMismatch) as exc:
            conn.execute("select 1 and 2")
        assert exc.value.sqlstate == "42804"
        assert (
            str(exc.value).splitlines()[0]
            == "argument of AND must be type boolean, not type integer"
        )
        cur = conn.execute("select not 'f', 't' and true, 'yes' or null")
        assert cur.fetchone() == (True, True, True)
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as exc2:
            conn.execute("select not 'x'")
        assert exc2.value.sqlstate == "22P02"
        assert str(exc2.value).splitlines()[0] == 'invalid input syntax for type boolean: "x"'

        conn.execute("create table cpr (id int, r int4range, mr int4multirange)")
        cur = conn.cursor()
        with cur.copy("copy cpr (id, r, mr) from stdin") as cp:
            cp.write("1\t[1,5]\t{empty}\n2\tempty\t{[1,5],[8,9)}\n3\t\\N\t\\N\n")
        cur = conn.execute("select id, r::text, mr::text, r, mr from cpr order by id")
        assert cur.fetchall() == [
            (1, "[1,6)", "{}", Range(1, 6, "[)"), Multirange()),
            (
                2,
                "empty",
                "{[1,6),[8,9)}",
                Range(empty=True),
                Multirange([Range(1, 6, "[)"), Range(8, 9, "[)")]),
            ),
            (3, None, None, None, None),
        ]


def test_pg_typeof_reports_the_type_the_client_declared(home: Path) -> None:
    """…which is not the one the decoded value suggests.

    psycopg sends a small integer as `int2`, so the answer is `smallint` where
    the value alone says `integer`. A parameter the client left untyped has no
    type to report at all, and PostgreSQL answers `42P18` rather than guessing.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for params, want in [
            ((1,), "smallint"),
            ((40000,), "integer"),
            ((10**12,), "bigint"),
            ((1.5,), "double precision"),
            ((Decimal("1.5"),), "numeric"),
            ((True,), "boolean"),
            ((dt.date(2026, 1, 1),), "date"),
            (([1, 2],), "smallint[]"),
        ]:
            cur.execute("select pg_typeof(%s)", params)
            assert cur.fetchone()[0] == want, params

        for params in [("x",), (None,)]:
            with pytest.raises(psycopg.errors.IndeterminateDatatype) as err:
                cur.execute("select pg_typeof(%s)", params)
            assert "could not determine data type of parameter $1" in str(err.value)

        # A cast gives an untyped parameter a type.
        cur.execute("select pg_typeof(%s::int4)", ("5",))
        assert cur.fetchone()[0] == "integer"


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_an_array_takes_its_type_from_its_elements(home: Path, binary: bool) -> None:
    """…not from the values it happens to hold.

    The describe pass sees no values at all — every parameter is NULL there —
    so an array typed from its values described `array[%s::float4]` as
    `text[]`, and the client decoded floats as text because the row description
    is what it believes.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        for sql, params, want, oid in [
            ("select array[1,2]", (), [1, 2], 1007),
            ("select array[1::int2]", (), [1], 1005),
            ("select array[1::int8]", (), [1], 1016),
            ("select array[1::float4]", (), [1.0], 1021),
            ("select array[true]", (), [True], 1000),
            ("select array['a'::text]", (), ["a"], 1009),
            ("select array[%s::float4]", ("42",), [42.0], 1021),
            ("select array[%s::int8]", (5,), [5], 1016),
            ("select array[%s]", (5,), [5], 1005),
            # Mixed numerics widen, in PostgreSQL's own order.
            ("select array[1, 1.5]", (), [Decimal("1"), Decimal("1.5")], 1231),
            ("select array[1::float4, 1.5]", (), [1.0, 1.5], 1021),
            ("select array[1::int8, 1::int2]", (), [1, 1], 1016),
            # A bare NULL contributes no type.
            ("select array[null, 1]", (), [None, 1], 1007),
        ]:
            cur.execute(sql, params)
            assert cur.fetchone()[0] == want, sql
            assert cur.pgresult.ftype(0) == oid, sql


def test_a_quoted_brace_is_a_string_not_a_nested_array(home: Path) -> None:
    """`'{"{"}'::text[]` is one element whose text is a brace.

    Only an UNQUOTED `{` opens a sub-array. Treating a quoted one as a nested
    array answered "malformed array literal" for the element — and `{` is an
    ordinary member of any corpus that walks the ASCII range, so it failed
    every round-trip test of a text array.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, params, want in [
            ("select %s::text[]", (["{"],), ["{"]),
            ("select %s::text[]", (["}"],), ["}"]),
            ("select %s::text[]", (["{1,2}"],), ["{1,2}"]),
            ("""select '{"{"}'::text[]""", (), ["{"]),
            ("select %s::varchar[]", (["{"],), ["{"]),
            (
                "select %s::text[]",
                ([chr(i) for i in range(1, 128)],),
                [chr(i) for i in range(1, 128)],
            ),
            # U+0085 and U+00A0 are whitespace to Rust and NOT to PostgreSQL,
            # so trimming an element with `str::trim` returned the empty string
            # for both — a character in, nothing out, and invisible to any test
            # whose alphabet is ASCII.
            ("select %s::text[]", (["\u0085", "\u00a0"],), ["\u0085", "\u00a0"]),
            (
                "select %s::text[]",
                ([chr(i) for i in range(1, 256)],),
                [chr(i) for i in range(1, 256)],
            ),
        ]:
            cur.execute(sql, params)
            assert cur.fetchone()[0] == want, sql


def test_an_array_of_dates_is_described_as_one(home: Path) -> None:
    """A date array was described as `varchar`, so a client read back strings."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select array['2026-01-01'::date]")
        assert cur.fetchone()[0] == [dt.date(2026, 1, 1)]
        assert cur.pgresult.ftype(0) == 1182
        cur.execute("select array['2026-01-01 12:00'::timestamp]")
        assert cur.fetchone()[0] == [dt.datetime(2026, 1, 1, 12)]
        assert cur.pgresult.ftype(0) == 1115


def test_the_json_navigation_operators(home: Path) -> None:
    """`->`, `->>`, `#>`, `#>>` over json and jsonb.

    A json value is carried as its text, so by the time two operands are values
    there is nothing to tell `{"a": 1}` from any other string — the left
    operand's static type is what makes these json operators at all. Every
    lookup that does not apply is SQL NULL rather than an error, which is
    PostgreSQL's rule and the reason they are usable.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, want in [
            ("""select '{"a": 1}'::json ->> 'a'""", "1"),
            ("""select '{"a": 1}'::jsonb ->> 'a'""", "1"),
            ("""select '{"a": "x"}'::json ->> 'a'""", "x"),
            ("""select '{"a": "x"}'::json -> 'a'""", "x"),
            ("select '[1,2]'::json ->> 1", "2"),
            # A negative index counts from the end, which is PostgreSQL's rule.
            ("select '[1,2]'::json ->> -1", "2"),
            # Missing, out of range, or the wrong shape: NULL, not an error.
            ("""select '{"a": 1}'::json ->> 'zz'""", None),
            ("select '[1,2]'::json -> 5", None),
            ("""select '"str"'::json ->> 'a'""", None),
            # A json null reads back as SQL NULL through `->>`.
            ("""select '{"a": null}'::json ->> 'a'""", None),
            ("""select '{"a":{"b":2}}'::json #>> '{a,b}'""", "2"),
            ("""select ('{"a":{"b":2}}'::json -> 'a') ->> 'b'""", "2"),
        ]:
            cur.execute(sql)
            assert cur.fetchone()[0] == want, sql

        # `->` keeps the json flavour, `->>` is text, the key tests are bool.
        for sql, oid in [
            ("""select '{"a": 1}'::json -> 'a'""", 114),
            ("""select '{"a": 1}'::jsonb -> 'a'""", 3802),
            ("""select '{"a": 1}'::json ->> 'a'""", 25),
            ("""select '{"a": 1}'::jsonb ? 'a'""", 16),
        ]:
            cur.execute(sql)
            cur.fetchone()
            assert cur.pgresult.ftype(0) == oid, sql


def test_the_json_key_and_containment_operators(home: Path) -> None:
    """`?`, `?|`, `?&`, `@>` and `<@`.

    Containment compares by VALUE, so key order and whitespace do not count —
    and neither does a number's scale, which is why `{"a": 1.0}` contains
    `{"a": 1}`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, want in [
            ("""select '{"a": 1}'::jsonb ? 'a'""", True),
            ("""select '{"a": 1}'::jsonb ? 'z'""", False),
            ("""select '["a","b"]'::jsonb ? 'a'""", True),
            ("""select '{"a": 1}'::jsonb ?| array['a','z']""", True),
            ("""select '{"a": 1}'::jsonb ?& array['a','z']""", False),
            ("""select '{"a": 1, "b": 2}'::jsonb ?& array['a','b']""", True),
            ("""select '{"a": 1, "b": 2}'::jsonb @> '{"a": 1}'""", True),
            ("""select '{"a": 1}'::jsonb @> '{"a": 2}'""", False),
            ("""select '{"a": 1}'::jsonb <@ '{"a": 1, "b": 2}'""", True),
            ("select '[1,2,3]'::jsonb @> '[1,3]'", True),
            # A top-level array contains a bare scalar it holds.
            ("select '[1,2,3]'::jsonb @> '2'", True),
            # …and a number's scale is not part of its value.
            ("""select '{"a": 1.0}'::jsonb @> '{"a": 1}'""", True),
        ]:
            cur.execute(sql)
            assert cur.fetchone()[0] is want, sql


def test_type_discovery_through_pg_type(home: Path) -> None:
    """psycopg's own `TypeInfo.fetch`, unmodified, against the virtual catalog.

    The query it sends needs five things at once: the `pg_type` table, the
    `to_regtype()` function, the `regtype` cast chain in the select list
    (`oid::regtype::text`), a table alias (`FROM pg_type t ... WHERE t.oid`),
    and column aliases. Any one missing and type discovery fails wholesale.
    """
    from psycopg.types import TypeInfo

    with _Server(home) as server, server.connect() as conn:
        info = TypeInfo.fetch(conn, "text")
        assert (info.name, info.oid, info.array_oid) == ("text", 25, 1009)
        # Both spellings resolve, exactly as PostgreSQL's own catalog does.
        assert TypeInfo.fetch(conn, "integer").oid == 23
        assert TypeInfo.fetch(conn, "int4").oid == 23
        assert TypeInfo.fetch(conn, "jsonb").array_oid == 3807
        # An unknown name is None, not an error — to_regtype's whole point.
        assert TypeInfo.fetch(conn, "nope") is None
        # A QUOTED identifier resolves too, case-sensitively.
        from psycopg import sql as _sql

        assert TypeInfo.fetch(conn, _sql.Identifier("text")).oid == 25
        assert TypeInfo.fetch(conn, _sql.Identifier("TEXT")) is None

        cur = conn.cursor()
        cur.execute("select to_regtype('text')")
        assert cur.fetchone()[0] == "text"
        assert cur.pgresult.ftype(0) == 2206  # regtype
        cur.execute("select to_regtype('nope')")
        assert cur.fetchone()[0] is None
        # A regtype casts onward by its two natures: name as text, oid as int.
        cur.execute("select to_regtype('integer')::text")
        assert cur.fetchone()[0] == "integer"
        cur.execute("select to_regtype('integer')::int4")
        assert cur.fetchone()[0] == 23
        # `::regtype` of an unknown name is an ERROR, unlike to_regtype.
        with pytest.raises(psycopg.errors.UndefinedObject):
            cur.execute("select 'nope'::regtype")

        # The catalog rows themselves, filtered and aliased.
        cur.execute("select typname from pg_type where oid = 25")
        assert cur.fetchone()[0] == "text"
        # By now psycopg has run its TypeInfo query past `prepare_threshold`,
        # so the view lists that one server-side statement — PG 16 answers 1
        # here too; the names are psycopg's own `_pg3_N`.
        cur.execute("select name from pg_prepared_statements")
        assert all(name.startswith("_pg3_") for (name,) in cur.fetchall())


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_the_oid_type(home: Path, binary: bool) -> None:
    """`oid` is an unsigned 32-bit integer with its own type oid.

    A negative literal wraps (`(-1)::oid` is 4294967295), a value past 2^32-1
    is out of range, and a non-numeric string is invalid text — all measured on
    PostgreSQL 14. The binary encoding is the 4-byte unsigned form.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        for sql, params, want in [
            ("select 0::oid", (), 0),
            ("select '25'::oid", (), 25),
            ("select 4294967295::oid", (), 4294967295),
            ("select (-1)::oid", (), 4294967295),
            ("select %s::oid", ("0",), 0),
            ("select %s::oid", (25,), 25),
            ("select 25::oid::int4", (), 25),
            ("select 25::oid::text", (), "25"),
        ]:
            cur.execute(sql, params)
            assert cur.fetchone()[0] == want, sql
        cur.execute("select 25::oid")
        cur.fetchone()
        assert cur.pgresult.ftype(0) == 26

        with pytest.raises(psycopg.errors.NumericValueOutOfRange):
            cur.execute("select 4294967296::oid")
        with pytest.raises(psycopg.errors.InvalidTextRepresentation):
            cur.execute("select 'x'::oid")


def test_a_scalar_call_over_a_column(home: Path) -> None:
    """`regexp_replace(col, ...)` and friends in a table select list.

    The value is computed per row by the executor; the TYPE is fixed at plan
    time, because the describe pass sees no rows and has to name the column's
    type anyway. Routing any function call to the aggregate planner used to
    surface this as a GROUPING error — the wrong error for a plain gap.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table sc (id int4, s text)")
        cur.execute("insert into sc values (1, 'Hello'), (2, 'prepare _pg3_7 as x'), (3, null)")
        cur.execute("select upper(s) from sc order by id")
        assert [r[0] for r in cur.fetchall()] == ["HELLO", "PREPARE _PG3_7 AS X", None]
        cur.execute("select length(s) from sc order by id")
        assert [r[0] for r in cur.fetchall()] == [5, 19, None]
        cur.execute(
            "select regexp_replace(s, 'prepare _pg3_\\d+ as ', '', 'i') as statement"
            " from sc order by id"
        )
        assert [r[0] for r in cur.fetchall()] == ["Hello", "x", None]


def test_regexp_replace(home: Path) -> None:
    """PostgreSQL's rules: FIRST match unless `g`, `\\1` groups, `i` flag."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, want in [
            ("select regexp_replace('aaa', 'a', 'b')", "baa"),
            ("select regexp_replace('aaa', 'a', 'b', 'g')", "bbb"),
            ("select regexp_replace('Hello World', 'o', '0', 'gi')", "Hell0 W0rld"),
            ("select regexp_replace('abc123', '(\\d+)', '<\\1>')", "abc<123>"),
            ("select regexp_replace('a$b', '\\$', 'S')", "aSb"),
        ]:
            cur.execute(sql)
            assert cur.fetchone()[0] == want, sql
        with pytest.raises(psycopg.errors.InvalidRegularExpression):
            cur.execute("select regexp_replace('x', '[', 'y')")
        # A NULL setting name reads back as NULL, not an error.
        cur.execute("select current_setting(%s)", (None,))
        assert cur.fetchone()[0] is None


def test_pg_typeof_answers_a_real_regtype(home: Path) -> None:
    """`pg_typeof(x)::oid` reads the oid, `::text` the name.

    The old string answer could only render; psycopg's wrapper tests read
    `pg_typeof(%s)::oid` for every numeric wrapper, and a display name is not
    a number.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, params, want in [
            ("select pg_typeof(%s)::oid", (1,), 21),
            ("select pg_typeof(%s)::oid", (1.5,), 701),
            ("select pg_typeof(%s)::oid", (Decimal("1"),), 1700),
            ("select pg_typeof(%s)::oid", ([1, 2],), 1005),
            ("select pg_typeof(%s)::text", (1,), "smallint"),
            ("select pg_typeof(1)", (), "integer"),
        ]:
            cur.execute(sql, params)
            assert cur.fetchone()[0] == want, sql


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_a_range_array_carries_its_own_oid(home: Path, binary: bool) -> None:
    """`int4range[]` is 3905, not varchar — a client builds Range objects."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        cur.execute("""select '{"[1,5)"}'::int4range[]""")
        assert cur.fetchone()[0] == [Range(1, 5, "[)")]
        assert cur.pgresult.ftype(0) == 3905
        cur.execute("select array['[1,5)'::int4range]")
        assert cur.fetchone()[0] == [Range(1, 5, "[)")]
        assert cur.pgresult.ftype(0) == 3905


def test_enum_ddl_and_the_catalog(home: Path) -> None:
    """CREATE TYPE ... AS ENUM / DROP TYPE, and the catalog reads behind them.

    The 207 test_enum failures all die in one session fixture running exactly
    this DDL, so nothing past it was measurable until it worked. A duplicate
    name is 42710 (distinct from a table's 42P07), a missing one 42704, and a
    case-sensitive name renders QUOTED through regtype — all measured.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type mood as enum ('sad','ok','happy')")
        assert cur.statusmessage == "CREATE TYPE"
        with pytest.raises(psycopg.errors.DuplicateObject):
            cur.execute("create type mood as enum ('x')")
        # The fixture's exact multi-statement shape.
        cur.execute("drop type if exists mood;\ncreate type mood as enum ('sad','ok');")

        cur.execute("select to_regtype('mood')::text")
        assert cur.fetchone()[0] == "mood"
        cur.execute("select typname, typarray from pg_type where oid = to_regtype('mood')")
        name, typarray = cur.fetchone()
        assert name == "mood"
        # typarray is DERIVED as oid + 100_000 — the shared-store rule.
        cur.execute("select oid from pg_type where typname = 'mood'")
        assert typarray == cur.fetchone()[0] + 100_000

        cur.execute("drop type mood")
        with pytest.raises(psycopg.errors.UndefinedObject):
            cur.execute("drop type mood")
        cur.execute("select to_regtype('mood')")
        assert cur.fetchone()[0] is None

        # A case-sensitive name, which the CamelCaseEnum fixture uses.
        cur.execute("create type \"CamelCase\" as enum ('x')")
        cur.execute("""select to_regtype('"CamelCase"')::text""")
        assert cur.fetchone()[0] == '"CamelCase"'
        cur.execute('drop type "CamelCase"')


def test_enum_column_reports_its_own_oid(home: Path) -> None:
    """An enum COLUMN is described with the enum's own oid, not varchar.

    psycopg reads that oid to decide whether to apply a registered enum loader:
    with the enum's oid it hands back the Python enum MEMBER, with varchar
    (1043) a bare string. The label is stored and returned verbatim -- including
    a non-ASCII one -- so the case-fold tests turn on the oid, not the bytes.
    """
    import enum as _enum

    from psycopg.types.enum import EnumInfo, register_enum

    class Mood(_enum.Enum):
        sad = "sad"
        ok = "ok"
        happy = "happy"

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type mood as enum ('sad','ok','happy')")
        cur.execute("create table t (id int, m mood, ms mood[])")
        cur.execute("insert into t values (1, 'ok', array['sad','happy']::mood[])")

        # Register the loader, then read through a FRESH cursor. (psycopg binds a
        # cursor's transformer on use, so a cursor that ran the DDL keeps the
        # default loaders; a new cursor picks up the registered enum loader --
        # true against real PostgreSQL too, not a server difference.) With the
        # loader in place the value comes back as the enum MEMBER, scalar and
        # array, and the row description still reports the enum's own minted oid
        # (not varchar 1043) beside an untouched int column.
        register_enum(EnumInfo.fetch(conn, "mood"), conn, Mood)
        rc = conn.cursor()
        rc.execute("select id, m, ms from t")
        id_val, m, ms = rc.fetchone()
        id_oid, m_oid, _ = (d.type_code for d in rc.description)
        assert id_val == 1
        assert m is Mood.ok
        assert ms == [Mood.sad, Mood.happy]
        assert id_oid == 23
        assert m_oid != 1043
        rc.execute("select oid from pg_type where typname = 'mood'")
        assert m_oid == rc.fetchone()[0]

        # ::text stays a plain string (oid 25), unaffected by the loader.
        rc.execute("select m::text from t")
        assert rc.fetchone()[0] == "ok"
        assert rc.description[0].type_code == 25

        # A non-ASCII label is returned VERBATIM (no case fold), which is what
        # the loader maps on.
        cur.execute("create type e as enum ('Xà','sad')")
        cur.execute("create table t2 (m e)")
        cur.execute("insert into t2 values ('Xà')")
        cur.execute("select m::text from t2")
        assert cur.fetchone()[0] == "Xà"


def test_an_enum_created_by_one_server_is_the_other_servers_too(home: Path) -> None:
    """The enum catalog is a SHARED-STORE contract, not an implementation.

    The Rust server writes the Python server's representation — `__sql_enums__`
    docs with monotonically minted oids — so an enum created on one side must
    resolve on the other, with the SAME oid.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type rustmood as enum ('a','b')")
        cur.execute("select oid from pg_type where typname = 'rustmood'")
        rust_oid = cur.fetchone()[0]

    # Python opens the same store and sees the type under the same oid…
    assert _python_sql(home, "SELECT to_regtype('rustmood')::oid") == [(rust_oid,)]
    # …and a type Python creates next mints the NEXT oid, not a reused one.
    _python_sql(home, "CREATE TYPE pymood AS ENUM ('x')")

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        # The enums themselves; each also has an array type (`_rustmood`).
        cur.execute(
            "select typname, oid from pg_type where oid >= 65000 and typtype = 'e' order by oid"
        )
        rows = cur.fetchall()
        assert rows == [("rustmood", rust_oid), ("pymood", rust_oid + 1)]


def test_range_info_fetch(home: Path) -> None:
    """psycopg's RangeInfo.fetch — pg_range joined to pg_type in a PLAIN select.

    Unlike EnumInfo's aggregate-over-subquery, this is a top-level JOIN with no
    grouping, so it exercises the join source on `Select` rather than
    `Aggregate`. `pg_range` pairs each builtin range with its element oid, from
    the same catalog the range casts read.
    """
    from psycopg.types.range import RangeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select rngtypid, rngsubtype from pg_range order by rngtypid")
        assert cur.fetchall() == [
            (3904, 23),
            (3906, 1700),
            (3908, 1114),
            (3910, 1184),
            (3912, 1082),
            (3926, 20),
        ]
        for name, oid, sub in [
            ("int4range", 3904, 23),
            ("numrange", 3906, 1700),
            ("tstzrange", 3910, 1184),
            ("daterange", 3912, 1082),
        ]:
            info = RangeInfo.fetch(conn, name)
            assert (info.name, info.oid, info.subtype_oid) == (name, oid, sub), name


def test_composite_info_fetch(home: Path) -> None:
    """psycopg's CompositeInfo.fetch — the 4-layer query behind composite types.

    `pg_type LEFT JOIN (SELECT array_agg(...) FROM (join) GROUP BY ...)` with a
    `coalesce(..., '{}')` per aggregate column. It exercises every composite
    layer at once: a subquery join side materialised from an aggregate, an
    `oid[]` array column (`array_agg(atttypid)`), coalesce projected as a target
    (a real empty array on a miss), and a base type (no fields) surviving the
    LEFT JOIN as two empty arrays. A field whose type is itself a composite
    resolves its own minted oid.
    """
    from psycopg.types.composite import CompositeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type ci_two as (a int, b text)")
        cur.execute("create type ci_varied as (i int, t text, d float8, ts timestamptz, f bool)")
        cur.execute("create type ci_nested as (x int, sub ci_two)")
        cur.execute("select oid from pg_type where typname = 'ci_two'")
        ci_two_oid = cur.fetchone()[0]

        two = CompositeInfo.fetch(conn, "ci_two")
        assert two.name == "ci_two"
        assert list(two.field_names) == ["a", "b"]
        assert list(two.field_types) == [23, 25]  # int4, text -- an oid[], parsed

        varied = CompositeInfo.fetch(conn, "ci_varied")
        assert list(varied.field_names) == ["i", "t", "d", "ts", "f"]
        assert list(varied.field_types) == [23, 25, 701, 1184, 16]

        # A field whose type is itself a composite carries that type's own oid.
        nested = CompositeInfo.fetch(conn, "ci_nested")
        assert list(nested.field_names) == ["x", "sub"]
        assert list(nested.field_types) == [23, ci_two_oid]

        # A base type (no fields) survives the LEFT JOIN: coalesce turns the
        # unmatched NULL side into two EMPTY arrays, not None and not "{}".
        base = CompositeInfo.fetch(conn, "int4")
        assert base.name == "int4"
        assert list(base.field_names) == []
        assert list(base.field_types) == []


def test_composite_binary_result(home: Path) -> None:
    """A composite result column in a BINARY cursor comes back typed.

    psycopg's binary composite loader reads the record wire format -- an int32
    field count, then per field an int32 oid, an int32 length and the bytes --
    and decodes each field by its declared oid. A text cursor renders `(...)`;
    a binary one must send the same values byte-for-byte as PostgreSQL, so the
    float field arrives a float and not the string a text record would carry.
    """
    from psycopg.types.composite import CompositeInfo, register_composite

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create type bc as (foo text, bar int8, baz float8)")
        info = CompositeInfo.fetch(conn, "bc")
        register_composite(info, conn)

        cur = conn.cursor(binary=True)
        res = cur.execute("select row('hi', 10, 20)::bc").fetchone()[0]
        assert res.foo == "hi"
        assert res.bar == 10
        assert res.baz == 20.0
        assert isinstance(res.baz, float)


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_composite_parameter_round_trips_in_both_formats(home: Path, binary: bool) -> None:
    """A registered composite bound as a PARAMETER decodes to its record value.

    psycopg's `register_composite` sends the composite's OID in the `Parse`
    message and the value in the `Bind` -- but pgwire's `Type::from_oid` maps a
    non-builtin oid to `None`, so the raw oid (and with it any hope of resolving
    the parameter's type) was lost before the parser saw it. The vendored
    pgwire patch preserves the raw oids in `StoredStatement::parameter_oids`, so
    a bound composite now gets a declared type (`pg_typeof` answers the type
    name instead of `could not determine data type of parameter $1`) and its
    value is decoded into the same record BSON a `'(..)'::type` literal
    produces -- from the TEXT `(a,b)` form and the binary RECORD form alike.
    """
    from psycopg.types.composite import CompositeInfo, register_composite

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create type cp as (foo text, bar int4, baz float8)")
        info = CompositeInfo.fetch(conn, "cp")
        register_composite(info, conn)
        obj = info.python_type("hi", 42, 3.5)

        cur = conn.cursor(binary=binary)
        # The Parse wall: the parameter's type is resolved from the raw oid.
        # (`::text`: in binary a regtype is its oid, which psycopg has no
        # loader for -- PostgreSQL sends the same four bytes.)
        assert cur.execute("select pg_typeof(%s)::text", [obj]).fetchone()[0] == "cp"
        # The value decodes to the composite and round-trips through a cast.
        got = cur.execute("select %s::cp", [obj]).fetchone()[0]
        assert (got.foo, got.bar, got.baz) == ("hi", 42, 3.5)
        # A NULL field survives, and the text render matches PostgreSQL's.
        obj_null = info.python_type("foo", 1, None)
        assert cur.execute("select %s::text", [obj_null]).fetchone()[0] == "(foo,1,)"
        # The planner NAMES the slot from the compared operand (`row(..)::cp =
        # $1` types `$1` as `cp`), so the parameter arrives with a declared
        # composite type rather than the raw oid alone. That declared type must
        # take the record decoder too: it once fell through to the generic one,
        # which read the text form as a plain string (`comparing document with
        # string`) and refused the binary form outright (`binary parameters of
        # type oid Some(...) are not supported yet`).
        assert cur.execute("select row('hi', 42, 3.5)::cp = %s", [obj]).fetchone()[0] is True
        assert cur.execute("select row('hi', 43, 3.5)::cp = %s", [obj]).fetchone()[0] is False
        # And a composite holding a range field, on a fresh connection (a
        # second `register_composite` on the same connection makes psycopg
        # send the parameter untyped -- PostgreSQL answers `could not
        # determine data type of parameter $1` for that too).
        conn.execute("create type cpr as (num int4, r daterange, nums int4[])")
        conn2 = conn.__class__.connect(conn.info.dsn, autocommit=True)
        from psycopg.types.range import Range

        rinfo = CompositeInfo.fetch(conn2, "cpr")
        register_composite(rinfo, conn2)
        robj = rinfo.python_type(10, Range(empty=True), [])
        cur = conn2.cursor(binary=binary)
        assert cur.execute("select pg_typeof(%s)::text", [robj]).fetchone()[0] == "cpr"
        assert cur.execute("select %s::text", [robj]).fetchone()[0] == "(10,empty,{})"


def test_composite_array_load_text_and_binary(home: Path) -> None:
    """`array[<composite>]` reports the composite's ARRAY oid, so the client
    parses it as a one-element array of composites rather than a bare string.

    Before the fix the column was described as varchar, so the array text
    `{"(hi,10,30)"}` was handed back as a 17-character string. Both wire formats
    now round-trip the array, in a text cursor and a binary one.
    """
    from psycopg.types.composite import CompositeInfo, register_composite

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create type bc2 as (foo text, bar int8, baz float8)")
        info = CompositeInfo.fetch(conn, "bc2")
        register_composite(info, conn)

        for binary in (False, True):
            cur = conn.cursor(binary=binary)
            res = cur.execute("select array[row('hi', 10, 30)::bc2]").fetchone()[0]
            assert len(res) == 1
            assert res[0].foo == "hi"
            assert res[0].baz == 30.0
            assert isinstance(res[0].baz, float)


def test_composite_recursive_load(home: Path) -> None:
    """A composite whose field is itself a composite round-trips, scalar and in
    an array, in both wire formats -- the nested field is encoded as its own
    record (binary) or quoted `(...)` text (text)."""
    from psycopg.types.composite import CompositeInfo, register_composite

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create type rc_inner as (foo text, bar int8, baz float8)")
        conn.execute("create type rc_outer as (qux int8, quux rc_inner)")
        register_composite(CompositeInfo.fetch(conn, "rc_inner"), conn)
        register_composite(CompositeInfo.fetch(conn, "rc_outer"), conn)

        for binary in (False, True):
            cur = conn.cursor(binary=binary)
            res = cur.execute("select row(42, row('hi', 10, 20)::rc_inner)::rc_outer").fetchone()[0]
            assert res.qux == 42
            assert res.quux.foo == "hi"
            assert res.quux.baz == 20.0
            assert isinstance(res.quux.baz, float)

            res = cur.execute(
                "select array[row(42, row('hi', 10, 30)::rc_inner)::rc_outer]"
            ).fetchone()[0]
            assert len(res) == 1
            assert res[0].quux.baz == 30.0
            assert isinstance(res[0].quux.baz, float)


def test_schema_qualified_composite_is_distinct(home: Path) -> None:
    """`create type s.t` is a DISTINCT type from a bare `t`.

    psycopg's composite test fixture creates `testschema.testcomp` beside a bare
    `testcomp`; a server that resolves a qualified type name to its last part
    collides them and the CREATE fails 42710, which -- the fixture being
    session-scoped -- cascades to every composite test. Here the two coexist,
    each fetches its OWN fields, and `to_regtype` resolves each name (bare,
    `schema.name`, and the quoted `"schema"."name"` a `sql.Identifier` renders)
    to the right oid. The bare name resolves only the public type; the schema
    name resolves only the schema-qualified one; `typname` stays unqualified.
    """
    from psycopg import sql
    from psycopg.types.composite import CompositeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        # The whole fixture as one multi-statement simple query, as psycopg sends it.
        cur.execute(
            "create schema if not exists testschema;"
            " create type testcomp as (foo text, bar int8, baz float8);"
            " create type testschema.testcomp as (foo text, bar int8, qux bool);"
        )

        pub = CompositeInfo.fetch(conn, "testcomp")
        sch = CompositeInfo.fetch(conn, "testschema.testcomp")
        assert pub.name == "testcomp" and sch.name == "testcomp"  # typname is bare
        assert pub.oid != sch.oid  # distinct types
        assert list(pub.field_names) == ["foo", "bar", "baz"]
        assert list(sch.field_names) == ["foo", "bar", "qux"]
        assert list(pub.field_types) == [25, 20, 701]  # text, int8, float8
        assert list(sch.field_types) == [25, 20, 16]  # text, int8, bool

        # A quoted schema-qualified reference (what sql.Identifier renders) also
        # resolves to the schema-qualified type.
        ident = CompositeInfo.fetch(conn, sql.Identifier("testschema", "testcomp"))
        assert ident.oid == sch.oid

        # to_regtype: the bare name is the public type; the schema name is the
        # other; a bare name never reaches the schema-qualified type.
        cur.execute("select to_regtype('testcomp')::oid, to_regtype('testschema.testcomp')::oid")
        pub_oid, sch_oid = cur.fetchone()
        assert pub_oid == pub.oid
        assert sch_oid == sch.oid
        assert pub_oid != sch_oid

        # DROP is schema-aware: dropping the qualified type leaves the bare one.
        cur.execute("drop type testschema.testcomp")
        cur.execute("select to_regtype('testschema.testcomp'), to_regtype('testcomp')::oid")
        gone, still = cur.fetchone()
        assert gone is None
        assert still == pub.oid


def test_schema_qualified_range_is_distinct(home: Path) -> None:
    """`create type s.t as range` is a DISTINCT type from a bare `t`.

    psycopg's session-scoped range fixture creates `testschema.testrange` beside
    a bare `testrange` in one script; a server that resolved a qualified type
    name to its last part collided them and the second CREATE failed 42710,
    which -- the fixture being session-scoped -- cascaded to every range test.
    Here the two coexist, each carries its OWN subtype, and `to_regtype` resolves
    each name (bare, `schema.name`, and the quoted `"schema"."name"` a
    `sql.Identifier` renders) to the right oid. The bare name resolves only the
    public range; the schema name resolves only the schema-qualified one;
    `typname` stays unqualified (`testrange`), matching PostgreSQL.
    """
    from psycopg import sql
    from psycopg.types.range import RangeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        # The whole fixture as one multi-statement simple query, as psycopg sends
        # it (subtypes chosen to differ so a collision could not go unnoticed).
        cur.execute(
            "create schema if not exists testschema;"
            ' create type testrange as range (subtype = text, collation = "C");'
            " create type testschema.testrange as range (subtype = float8);"
        )

        text_oid = conn.adapters.types["text"].oid
        float8_oid = conn.adapters.types["float8"].oid

        pub = RangeInfo.fetch(conn, "testrange")
        sch = RangeInfo.fetch(conn, "testschema.testrange")
        assert pub.name == "testrange" and sch.name == "testrange"  # typname is bare
        assert pub.oid != sch.oid  # distinct types
        assert pub.subtype_oid == text_oid
        assert sch.subtype_oid == float8_oid

        # A quoted schema-qualified reference (what sql.Identifier renders) also
        # resolves to the schema-qualified range.
        ident = RangeInfo.fetch(conn, sql.Identifier("testschema", "testrange"))
        assert ident.oid == sch.oid
        bare_ident = RangeInfo.fetch(conn, sql.Identifier("testrange"))
        assert bare_ident.oid == pub.oid

        # to_regtype: the bare name is the public range; the schema name is the
        # other; a bare name never reaches the schema-qualified range.
        cur.execute("select to_regtype('testrange')::oid, to_regtype('testschema.testrange')::oid")
        pub_oid, sch_oid = cur.fetchone()
        assert pub_oid == pub.oid
        assert sch_oid == sch.oid
        assert pub_oid != sch_oid

        # DROP is schema-aware: dropping the qualified type leaves the bare one.
        cur.execute("drop type testschema.testrange")
        cur.execute("select to_regtype('testschema.testrange'), to_regtype('testrange')::oid")
        gone, still = cur.fetchone()
        assert gone is None
        assert still == pub.oid


def test_custom_multirange_fetch_info(home: Path) -> None:
    """`MultirangeInfo.fetch` resolves a custom range's auto-created multirange.

    `CREATE TYPE testrange AS RANGE (...)` gives PostgreSQL a companion
    multirange type named by substituting `multirange` for the first `range`
    in the name (`testrange` -> `testmultirange`). psycopg's `MultirangeInfo.fetch`
    resolves that name with `to_regtype`, then joins `pg_type` to `pg_range` on
    `t.oid = r.rngmultitypid` -- so all three must exist: a resolvable multirange
    oid, a `pg_type` row under it (with its own array oid), and a `pg_range` row
    whose `rngmultitypid` points back at it, carrying the range's subtype. The
    schema-qualified range's multirange resolves distinctly, exactly like the
    range itself; `typname` stays the bare `testmultirange`, as in PostgreSQL.
    """
    from psycopg import sql
    from psycopg.types.multirange import MultirangeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "create schema if not exists testschema;"
            ' create type testrange as range (subtype = text, collation = "C");'
            " create type testschema.testrange as range (subtype = float8);"
        )
        text_oid = conn.adapters.types["text"].oid
        float8_oid = conn.adapters.types["float8"].oid

        # The four forms psycopg's own test_fetch_info exercises: bare name,
        # schema.name, and each as a sql.Identifier.
        for name, subtype_oid in [
            ("testmultirange", text_oid),
            ("testschema.testmultirange", float8_oid),
            (sql.Identifier("testmultirange"), text_oid),
            (sql.Identifier("testschema", "testmultirange"), float8_oid),
        ]:
            info = MultirangeInfo.fetch(conn, name)
            assert info is not None, name
            assert info.name == "testmultirange"  # typname is bare
            assert info.oid > 0
            assert info.oid != info.array_oid > 0
            assert info.subtype_oid == subtype_oid

        # An unknown name resolves to None, not an error.
        assert MultirangeInfo.fetch(conn, "nosuchmultirange") is None


def test_schema_qualified_multirange_is_distinct(home: Path) -> None:
    """A public range's multirange and a `schema.` range's multirange differ.

    Both custom ranges yield a multirange whose bare `typname` is
    `testmultirange`, but they are distinct types with distinct oids and
    subtypes -- `to_regtype('testmultirange')` reaches only the public one and
    `to_regtype('testschema.testmultirange')` only the schema one, mirroring the
    range distinctness #1391 established.
    """
    from psycopg.types.multirange import MultirangeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "create schema if not exists testschema;"
            " create type testrange as range (subtype = text);"
            " create type testschema.testrange as range (subtype = float8);"
        )
        pub = MultirangeInfo.fetch(conn, "testmultirange")
        sch = MultirangeInfo.fetch(conn, "testschema.testmultirange")
        assert pub.name == "testmultirange" and sch.name == "testmultirange"
        assert pub.oid != sch.oid
        assert pub.subtype_oid == conn.adapters.types["text"].oid
        assert sch.subtype_oid == conn.adapters.types["float8"].oid

        # to_regtype resolution keeps the two apart; a bare name never reaches
        # the schema-qualified multirange.
        cur.execute(
            "select to_regtype('testmultirange')::oid, to_regtype('testschema.testmultirange')::oid"
        )
        pub_oid, sch_oid = cur.fetchone()
        assert pub_oid == pub.oid
        assert sch_oid == sch.oid
        assert pub_oid != sch_oid


def test_a_plain_select_over_a_join(home: Path) -> None:
    """A top-level two-table JOIN outside any aggregate.

    The join source lives on `Select` as well as `Aggregate`; this is the
    non-grouped path RangeInfo.fetch takes.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select t.typname, r.rngsubtype from pg_type t "
            "join pg_range r on r.rngtypid = t.oid order by r.rngsubtype limit 2"
        )
        assert cur.fetchall() == [("int8range", 20), ("int4range", 23)]
        # A LEFT JOIN miss over the same tables yields the NULL side.
        cur.execute(
            "select t.typname, r.rngsubtype from pg_type t "
            "left join pg_range r on r.rngtypid = t.oid where t.oid = 25"
        )
        assert cur.fetchall() == [("text", None)]


def test_schema_ddl(home: Path) -> None:
    """CREATE SCHEMA / DROP SCHEMA, and a schema-qualified table.

    A duplicate is 42P06 (distinct from a table's 42P07 and a type's 42710), a
    missing DROP is 3F000, and CASCADE is accepted. Qualified names already
    resolve by their last part, so a table in a schema works once the schema
    DDL is allowed.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create schema testschema")
        assert cur.statusmessage == "CREATE SCHEMA"
        with pytest.raises(psycopg.errors.DuplicateSchema):
            cur.execute("create schema testschema")
        cur.execute("create schema if not exists testschema")
        cur.execute("drop schema if exists testschema cascade")
        with pytest.raises(psycopg.errors.InvalidSchemaName):
            cur.execute("drop schema testschema")
        cur.execute("drop schema if exists never")

        cur.execute("create schema s2")
        cur.execute("create table s2.t (id int4)")
        cur.execute("insert into s2.t values (1)")
        cur.execute("select id from s2.t")
        assert cur.fetchall() == [(1,)]
        cur.execute("drop schema s2 cascade")


def test_composite_type_ddl_and_catalog(home: Path) -> None:
    """CREATE TYPE ... AS (fields), DROP TYPE, and the catalog reads behind it.

    The DDL and the catalog (pg_type / to_regtype / regtype / pg_attribute) are
    the foundation CompositeInfo.fetch reads; the fetch query's nested-subquery
    join is a separate slice. A duplicate name is 42710 across composites,
    enums and builtins alike.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type testcomp as (foo text, bar int8, baz float8)")
        assert cur.statusmessage == "CREATE TYPE"
        with pytest.raises(psycopg.errors.DuplicateObject):
            cur.execute("create type testcomp as (x int4)")

        cur.execute("select typname, typrelid from pg_type where typname = 'testcomp'")
        name, typrelid = cur.fetchone()
        assert name == "testcomp"
        cur.execute("select oid from pg_type where typname = 'testcomp'")
        assert typrelid == cur.fetchone()[0]  # typrelid == its own oid
        cur.execute("select to_regtype('testcomp')::text")
        assert cur.fetchone()[0] == "testcomp"

        # pg_attribute exposes the fields, 1-based, with the element oids.
        cur.execute(
            "select attname, atttypid, attnum from pg_attribute"
            " where attrelid = to_regtype('testcomp') and attnum > 0"
            " and attisdropped = false order by attnum"
        )
        assert cur.fetchall() == [("foo", 25, 1), ("bar", 20, 2), ("baz", 701, 3)]

        cur.execute("drop type testcomp")
        cur.execute("select to_regtype('testcomp')")
        assert cur.fetchone()[0] is None


def test_a_composite_created_by_one_server_is_the_others_too(home: Path) -> None:
    """The composite catalog is a shared-store contract, minted like enums."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type rustcomp as (a int4, b text)")
        cur.execute("select oid from pg_type where typname = 'rustcomp'")
        oid = cur.fetchone()[0]
        assert oid >= 67000
    assert _python_sql(home, "SELECT to_regtype('rustcomp')::oid") == [(oid,)]
    _python_sql(home, "CREATE TYPE pycomp AS (c int8)")
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select attname from pg_attribute where attrelid = to_regtype('pycomp')")
        assert cur.fetchall() == [("c",)]


def test_composite_value_record_cast(home: Path) -> None:
    """`'(1,x)'::testcomp` and `row(1,'x')::testcomp` build a composite VALUE.

    The record cast used to be an outright `FeatureNotSupported` (row form) or a
    mis-routed enum parse (`invalid input value for enum testcomp`, text form).
    Both now parse into a composite whose result column carries the composite's
    own oid, so psycopg's `register_composite` loader fires.
    """
    from psycopg.types.composite import CompositeInfo, register_composite

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type testcomp as (foo int, bar text)")
        register_composite(CompositeInfo.fetch(conn, "testcomp"), conn)

        # Text-literal cast and record-constructor cast both round-trip, and the
        # loader turns them into the registered namedtuple.
        got = conn.execute("select '(1,x)'::testcomp").fetchone()[0]
        assert (got.foo, got.bar) == (1, "x")
        got = conn.execute("select row(2, 'y')::testcomp").fetchone()[0]
        assert (got.foo, got.bar) == (2, "y")


def test_composite_value_dump_and_table_round_trip(home: Path) -> None:
    """A composite param cast on the wire, and INSERT/SELECT through a column."""
    from psycopg.types.composite import CompositeInfo, register_composite

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type testcomp as (foo int, bar text)")
        info = CompositeInfo.fetch(conn, "testcomp")
        register_composite(info, conn)

        # Dump: a composite tuple param, explicitly cast on the wire.
        got = conn.execute("select %s::testcomp", [(7, "seven")]).fetchone()[0]
        assert (got.foo, got.bar) == (7, "seven")

        # Store it in a column of the composite type and read it back.
        cur.execute("create table ct (id int, val testcomp)")
        cur.execute("insert into ct values (1, %s)", [info.python_type(9, "nine")])
        got = conn.execute("select val from ct where id = 1").fetchone()[0]
        assert (got.foo, got.bar) == (9, "nine")


def test_composite_value_field_escaping(home: Path) -> None:
    """A composite VALUE renders its fields exactly as PostgreSQL does.

    NULL is empty, the empty string is `""`, and a field with a comma, quote,
    backslash, parenthesis or whitespace is double-quoted with `"`->`""` and
    `\\`->`\\\\`. The `::text` render is the surface a client compares against.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type tc AS (a int, b text)")
        cases = {
            "(,)": "(,)",
            '(1,"")': '(1,"")',
            '(1,"a,b")': '(1,"a,b")',
            '(1,"a""b")': '(1,"a""b")',
            '(1,"(x)")': '(1,"(x)")',
            '(1," sp ")': '(1," sp ")',
        }
        for literal, want in cases.items():
            got = cur.execute("select (%s::tc)::text", [literal]).fetchone()[0]
            assert got == want, (literal, got, want)


def test_composite_value_array_element_text(home: Path) -> None:
    """An array of composites renders each element as its `(...)` text.

    A record element used to leak Rust's `{:?}` debug form into the array
    literal (`{"Document({...})"}`); it now renders `{"(hello,10,30)"}`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type tc AS (foo text, bar int8, baz float8)")
        got = cur.execute("select array[row('hello', 10, 30)::tc]").fetchone()[0]
        assert got == '{"(hello,10,30)"}'


def test_row_expressions_are_records(home: Path) -> None:
    """`ROW(...)` / `(a, b, ...)` build an anonymous record (oid 2249).

    psycopg decodes a record's text form to a tuple of strings. The text render
    follows PostgreSQL's composite rules -- a NULL field is empty, a field with
    a comma/quote/space is double-quoted, and a bool prints `t`/`f` -- and
    record comparison is three-valued (a NULL field before a decision is NULL).
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT ROW(1, 'a', true)")
        assert cur.fetchone()[0] == ("1", "a", "t")
        assert cur.description[0].type_code == 2249

        cur.execute("SELECT (1, 'a')")
        assert cur.fetchone()[0] == ("1", "a")

        # Text render with quoting and NULL-as-empty.
        cur.execute("SELECT ROW('a,b', 'c')::text, ROW(1, NULL, 3)::text, ROW('')::text")
        assert cur.fetchone() == ('("a,b",c)', "(1,,3)", '("")')

        # Comparison, including three-valued NULL.
        cur.execute("SELECT ROW(1, 2) = ROW(1, 2), ROW(1, 2) < ROW(1, 3)")
        assert cur.fetchone() == (True, True)
        cur.execute("SELECT ROW(1, NULL) = ROW(1, NULL), ROW(1, 2) < ROW(1, NULL)")
        assert cur.fetchone() == (None, None)
        # A decided inequality wins over a later NULL (= examines every pair).
        cur.execute("SELECT ROW(1, NULL, 3) = ROW(1, 2, 4)")
        assert cur.fetchone()[0] is False

        cur.execute("SELECT pg_typeof(ROW(1, 2))::text")
        assert cur.fetchone()[0] == "record"


def test_field_selection_from_record_and_composite(home: Path) -> None:
    """`(expr).field` selects a named field from a record or composite value.

    An anonymous record names its fields `f1`, `f2`, ... by position; a named
    composite carries its own field names, and the selected field reports the
    field's declared type in the row description (so `pg_typeof` reads it back).
    An unknown field is 42703; a field of a NULL composite is NULL.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        # Anonymous record: positional f1, f2, ...
        cur.execute("SELECT (ROW('a'::text, 'b'::text)).f1, (ROW('a'::text, 'b'::text)).f2")
        assert cur.fetchone() == ("a", "b")

        # Named composite: field by name, with the field's declared type.
        cur.execute("create type fc as (foo text, bar int8, baz float8)")
        cur.execute("SELECT (row('x', 42, 3.5)::fc).bar, (row('x', 42, 3.5)::fc).foo")
        assert cur.fetchone() == (42, "x")
        cur.execute("SELECT pg_typeof((row('x', 42, 3.5)::fc).bar)::text")
        assert cur.fetchone()[0] == "bigint"

        # A field of a NULL composite is NULL.
        cur.execute("SELECT (NULL::fc).bar")
        assert cur.fetchone()[0] is None

        # An unknown field is a 42703 UndefinedColumn, worded by the SOURCE
        # type: PostgreSQL names the composite for a named type and says
        # "record data type" for an anonymous row.
        with pytest.raises(psycopg.errors.UndefinedColumn) as exc:
            cur.execute("SELECT (row('x', 42, 3.5)::fc).nope").fetchone()
        assert exc.value.diag.message_primary == 'column "nope" not found in data type fc'
        # An anonymous record has exactly as many fN fields as values: a
        # position past the end, f0, and a non-positional name are all the
        # same error -- a NULL for `.f3` here was a silent divergence.
        for field in ("f3", "f0", "zz"):
            with pytest.raises(psycopg.errors.UndefinedColumn) as exc:
                cur.execute(f"SELECT (row(1, 'x')).{field}").fetchone()
            assert exc.value.diag.message_primary == (
                f'could not identify column "{field}" in record data type'
            )


def test_composite_value_equality_is_null_blind(home: Path) -> None:
    """Comparing composite VALUES treats NULL fields as equal (unlike a bare
    ROW constructor, which is three-valued).

    PostgreSQL: for composite-type values two NULL fields are equal and a NULL
    sorts larger than any non-NULL, so the comparison always resolves to
    true/false; a bare `ROW(...) = ROW(...)` with a NULL is still NULL. Nested
    composite fields compare by the same rule, recursively.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type ce as (foo text, bar int8, baz float8)")
        cur.execute("create type ce2 as (qux int8, quux ce)")

        # Two composite values with a NULL field are EQUAL (not NULL).
        cur.execute(
            "SELECT row('foo', 1, NULL)::ce = row('foo', 1, NULL)::ce,"
            "       row('foo', 1, NULL)::ce <> row('foo', 1, NULL)::ce"
        )
        assert cur.fetchone() == (True, False)

        # A NULL beside a non-NULL is UNEQUAL, and NULL sorts larger.
        cur.execute(
            "SELECT row('foo', 1, NULL)::ce = row('foo', 1, 3.0)::ce,"
            "       row('foo', 1, NULL)::ce < row('foo', 1, 3.0)::ce"
        )
        assert cur.fetchone() == (False, False)

        # A decided inequality in a non-NULL field still wins.
        cur.execute("SELECT row('foo', 2, NULL)::ce = row('foo', 1, NULL)::ce")
        assert cur.fetchone()[0] is False

        # Nested composite fields compare by the same NULL-blind rule.
        cur.execute(
            "SELECT row(42, row('foo', 1, NULL)::ce)::ce2"
            "     = row(42, row('foo', 1, NULL)::ce)::ce2"
        )
        assert cur.fetchone()[0] is True

        # A bare ROW constructor stays three-valued: NULL, not True.
        cur.execute("SELECT ROW('foo', 1, NULL) = ROW('foo', 1, NULL)")
        assert cur.fetchone()[0] is None


def test_timestamptz_columns_render_in_session_zone(home: Path) -> None:
    """A `timestamptz` column stores a UTC INSTANT and renders in the session zone.

    The stored value is the same instant regardless of `SET timezone`; only its
    rendering moves. This needs the server to (a) store an instant, not
    session-rendered text, and (b) report the `TimeZone` GUC via ParameterStatus
    so psycopg re-expresses the instant in the session zone. A plain `timestamp`
    column stays naive (oid 1114).
    """
    import datetime as _dt

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table tz (id int primary key, t timestamptz)")
        cur.execute("insert into tz values (1, '2026-01-01 12:00:00+00')")
        cur.execute("insert into tz values (2, '2026-06-15 08:30:00.123456-04')")
        cur.execute("select t from tz where id = 1")
        assert cur.description[0].type_code == 1184

        # The same instant, rendered in three different session zones.
        expected = {
            "UTC": "2026-01-01T12:00:00+00:00",
            "Asia/Tokyo": "2026-01-01T21:00:00+09:00",
            "America/New_York": "2026-01-01T07:00:00-05:00",
        }
        for zone, iso in expected.items():
            cur.execute(f"set timezone = '{zone}'")
            cur.execute("select t from tz where id = 1")
            assert cur.fetchone()[0].isoformat() == iso, zone

        # `t::text` renders the instant in the session zone WITH its offset --
        # the column's declared type drives the cast, not the stored carrier
        # (a UTC instant, the same as a `timestamp`'s). Measured PG 16.
        cur.execute("set timezone = 'Europe/Berlin'")
        cur.execute("select t::text from tz where id = 1")
        assert cur.fetchone() == ("2026-01-01 13:00:00+01",)

        # Sub-millisecond precision survives the instant round-trip.
        cur.execute("set timezone = 'UTC'")
        cur.execute("select t from tz where id = 2")
        assert cur.fetchone()[0] == _dt.datetime(
            2026, 6, 15, 12, 30, 0, 123456, tzinfo=_dt.timezone.utc
        )

        # A plain `timestamp` column is unaffected -- naive, oid 1114.
        cur.execute("create table ts (id int primary key, t timestamp)")
        cur.execute("insert into ts values (1, '2026-01-01 12:00')")
        cur.execute("set timezone = 'Asia/Tokyo'")
        cur.execute("select t from ts")
        assert cur.description[0].type_code == 1114
        assert cur.fetchone()[0] == _dt.datetime(2026, 1, 1, 12, 0)


def test_custom_range_types(home: Path) -> None:
    """`CREATE TYPE ... AS RANGE (subtype = ...)` — a user range type.

    A user range has no canonical function, so `[1,4]` stays `[1,4]` (unlike the
    builtin int4range, which canonicalises to `[1,5)`). The type resolves as a
    regtype, casts parse/render over its subtype, RangeInfo.fetch finds its
    subtype via `pg_type JOIN pg_range`, and DROP TYPE removes it.
    """
    from psycopg.types.range import RangeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TYPE myr AS RANGE (subtype = int4)")
        cur.execute("SELECT '[1,5)'::myr::text")
        assert cur.fetchone()[0] == "[1,5)"
        # No canonicalisation for a user range.
        cur.execute("SELECT '[1,4]'::myr::text")
        assert cur.fetchone()[0] == "[1,4]"
        cur.execute("SELECT 'empty'::myr::text")
        assert cur.fetchone()[0] == "empty"
        cur.execute("SELECT '(,5)'::myr::text")
        assert cur.fetchone()[0] == "(,5)"

        # RangeInfo.fetch resolves the subtype (compare the type NAME, since the
        # subtype oid is stable but the range oid is minted).
        info = RangeInfo.fetch(conn, "myr")
        cur.execute("SELECT %s::regtype::text", (info.subtype_oid,))
        assert cur.fetchone()[0] == "integer"

        # A numeric-subtype range too.
        cur.execute("CREATE TYPE mynr AS RANGE (subtype = numeric)")
        cur.execute("SELECT '[1.5,3.5)'::mynr::text")
        assert cur.fetchone()[0] == "[1.5,3.5)"

        # DROP removes it: the regtype no longer resolves.
        cur.execute("DROP TYPE myr")
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT 'myr'::regtype")
        assert exc.value.diag.sqlstate == "42704"


def test_binary_array_params_bytea_inet_uuid(home: Path) -> None:
    """bytea[] / inet[] / cidr[] / uuid[] round-trip as BINARY params and columns.

    psycopg sends these as binary array parameters (oids 1001 / 651 / 1041 /
    2951) and reads array columns back in binary; the element decode reuses the
    scalar bytea / inet / cidr / uuid decoders, and the array types report their
    own oid rather than falling through to varchar.

    Every column of a binary-requested row must actually COME BACK binary:
    psycopg decodes the whole row with the first column's format
    (`_py_transformer.py`, `fformat(0)`), so a row mixing a binary `uuid[]`
    with a text `inet[]` is undecodable ("unexpected number of dimensions").
    PG 16 honours the requested format on all eight columns here, reports oids
    `[1001, 2951, 1041, 651, 869, 650, 1041, 869]`, and renders the `::1`
    host address without its `/128` mask in text as well as binary.
    """
    import ipaddress
    import uuid as _uuid

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table arr (b bytea[], u uuid[], n inet[], c cidr[], i inet, d cidr)")
        u1 = _uuid.UUID("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11")
        cur.execute(
            "insert into arr values (%b, %b, %b, %b, %b, %b)",
            (
                [b"\x01", b"\x02\xff"],
                [u1],
                [ipaddress.ip_interface("10.0.0.1/8"), ipaddress.ip_interface("::1")],
                [ipaddress.ip_network("10.0.0.0/8")],
                ipaddress.ip_interface("192.168.0.1/24"),
                ipaddress.ip_network("2001:db8::/32"),
            ),
        )
        for binary in (False, True):
            c = conn.cursor(binary=binary)
            c.execute("select b, u, n, c, i, d, array[null::inet], null::inet from arr")
            assert c.fetchone() == (
                [b"\x01", b"\x02\xff"],
                [u1],
                [ipaddress.ip_interface("10.0.0.1/8"), ipaddress.ip_address("::1")],
                [ipaddress.ip_network("10.0.0.0/8")],
                ipaddress.ip_interface("192.168.0.1/24"),
                ipaddress.ip_network("2001:db8::/32"),
                [None],
                None,
            ), binary
            assert [c.pgresult.fformat(k) for k in range(8)] == [int(binary)] * 8
            assert [d.type_code for d in c.description] == [
                1001,
                2951,
                1041,
                651,
                869,
                650,
                1041,
                869,
            ]


def test_join_multi_predicate_where(home: Path) -> None:
    """A JOIN's WHERE can be several ANDed predicates on either side.

    Foundation for composite `CompositeInfo.fetch` (whose inner subquery joins
    pg_attribute to pg_type with `WHERE t.oid=$1 AND a.attnum>0 AND NOT ...`).
    Verifies `=` plus a `>` on the join's right side over the catalog tables.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT t.typname FROM pg_type t JOIN pg_range r ON r.rngtypid = t.oid "
            "WHERE t.oid = 3904 AND r.rngsubtype > 0"
        )
        assert cur.fetchall() == [("int4range",)]


def test_user_types_are_visible_in_the_transaction_that_creates_them(home: Path) -> None:
    """A type created in a transaction is visible to later statements in it.

    Planning reads the type catalog OUTSIDE the open transaction (wrapping that
    read in the transaction deadlocks COPY), so an uncommitted CREATE TYPE was
    invisible and `CREATE TYPE t ...; SELECT 't'::regtype` in one transaction
    failed with 42704 -- every psycopg composite/enum/range fixture, whose conn
    is non-autocommit, hit it. An overlay consulted before the committed
    catalog fixes it, exactly as the table one does.
    """
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TYPE ictx AS (a int, b text)")
        cur.execute("SELECT 'ictx'::regtype::text")
        assert cur.fetchone() == ("ictx",)
        cur.execute("CREATE TYPE ienum AS ENUM ('a', 'b')")
        cur.execute("SELECT 'ienum'::regtype::text")
        assert cur.fetchone() == ("ienum",)
        cur.execute("SELECT 'a'::ienum::text")
        assert cur.fetchone() == ("a",)
        cur.execute("CREATE TYPE irange AS RANGE (subtype = int4)")
        # The range RESOLVES in the transaction (no 42704). Its regtype text
        # renders the oid rather than the name -- a separate, pre-existing gap
        # that shows in autocommit too, tracked in the backlog -- so this
        # asserts only that the cast succeeds.
        cur.execute("SELECT 'irange'::regtype::text")
        assert cur.fetchone() is not None
        conn.commit()


def test_fetch_info_works_in_the_creating_transaction(home: Path) -> None:
    """psycopg's `*.fetch` helpers find a type created in the same transaction.

    This is the shape every composite/enum/range gauge fixture uses: on a
    non-autocommit connection, CREATE TYPE then `CompositeInfo.fetch` -- which
    returned None (`TypeError: no info passed`) while the type was invisible.
    """
    from psycopg.types.composite import CompositeInfo
    from psycopg.types.enum import EnumInfo
    from psycopg.types.range import RangeInfo

    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TYPE ficomp AS (a int, b text)")
        info = CompositeInfo.fetch(conn, "ficomp")
        assert info is not None
        assert info.name == "ficomp"
        assert info.field_names == ("a", "b")
        cur.execute("CREATE TYPE fienum AS ENUM ('x', 'y')")
        einfo = EnumInfo.fetch(conn, "fienum")
        assert einfo is not None
        assert einfo.name == "fienum"
        assert einfo.labels == ["x", "y"]
        cur.execute("CREATE TYPE firange AS RANGE (subtype = int4)")
        rinfo = RangeInfo.fetch(conn, "firange")
        assert rinfo is not None
        assert rinfo.name == "firange"
        assert rinfo.subtype_oid == 23
        conn.rollback()


def test_rollback_discards_a_type_created_in_the_transaction(home: Path) -> None:
    """ROLLBACK removes an uncommitted CREATE TYPE, COMMIT keeps it."""
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TYPE rb AS (x int)")
        cur.execute("SELECT 'rb'::regtype::text")
        assert cur.fetchone() == ("rb",)
        conn.rollback()
        with pytest.raises(psycopg.errors.UndefinedObject):
            cur.execute("SELECT 'rb'::regtype::text")
        conn.rollback()
        # Committed this time, it persists into the next transaction.
        cur.execute("CREATE TYPE kept AS (x int)")
        conn.commit()
        cur.execute("SELECT 'kept'::regtype::text")
        assert cur.fetchone() == ("kept",)
        conn.commit()


def test_savepoint_rollback_discards_a_type_created_after_it(home: Path) -> None:
    """ROLLBACK TO undoes a CREATE TYPE issued after the savepoint."""
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        cur.execute("SAVEPOINT s1")
        cur.execute("CREATE TYPE sp AS (x int)")
        cur.execute("SELECT 'sp'::regtype::text")
        assert cur.fetchone() == ("sp",)
        cur.execute("ROLLBACK TO SAVEPOINT s1")
        with pytest.raises(psycopg.errors.UndefinedObject):
            cur.execute("SELECT 'sp'::regtype::text")
        conn.rollback()
        # The rolled-back savepoint's type never persists.
        with pytest.raises(psycopg.errors.UndefinedObject):
            cur.execute("SELECT 'sp'::regtype::text")
        conn.rollback()


def test_non_holdable_cursor_is_invalid_after_commit(home: Path) -> None:
    """A `WITHOUT HOLD` cursor is closed by COMMIT; a `WITH HOLD` one survives.

    This is what psycopg's `ServerCursor(withhold=False)` relies on -- after
    `conn.commit()` a fetch must raise `InvalidCursorName` (34000). Silently
    keeping the cursor open would let a client read rows PostgreSQL discarded.
    """
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        # Default (no HOLD): gone after commit.
        cur.execute("declare c1 cursor for select * from generate_series(1, 3)")
        cur.execute("fetch 1 from c1")
        assert [r[0] for r in cur.fetchall()] == [1]
        conn.commit()
        with pytest.raises(psycopg.errors.InvalidCursorName):
            cur.execute("fetch 1 from c1")
        conn.rollback()
        # WITH HOLD: survives commit, keeps its position.
        cur.execute("declare c2 cursor with hold for select * from generate_series(1, 3)")
        cur.execute("fetch 1 from c2")
        assert [r[0] for r in cur.fetchall()] == [1]
        conn.commit()
        cur.execute("fetch 1 from c2")
        assert [r[0] for r in cur.fetchall()] == [2]
        cur.execute("close c2")


def test_rollback_closes_every_cursor_including_holdable(home: Path) -> None:
    """ROLLBACK closes ALL cursors, `WITH HOLD` included."""
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        cur.execute("declare h cursor with hold for select * from generate_series(1, 3)")
        cur.execute("fetch 1 from h")
        assert [r[0] for r in cur.fetchall()] == [1]
        conn.rollback()
        with pytest.raises(psycopg.errors.InvalidCursorName):
            cur.execute("fetch 1 from h")
        conn.rollback()


def test_no_scroll_cursor_rejects_a_backward_fetch(home: Path) -> None:
    """A `NO SCROLL` cursor may only scan forward.

    A backward FETCH/MOVE raises `ObjectNotInPrerequisiteState` (55000), while a
    plain (scrollable) cursor still scrolls both ways.
    """
    with _Server(home) as server, server.connect() as conn:
        with conn.transaction():
            cur = conn.cursor()
            cur.execute("declare ns no scroll cursor for select * from generate_series(0, 5)")
            cur.execute("move 5 from ns")
            with pytest.raises(psycopg.errors.ObjectNotInPrerequisiteState):
                cur.execute("move backward 1 from ns")
        # A plain cursor is scrollable and still walks backward: MOVE 5 lands on
        # the row valued 4, and FETCH BACKWARD 1 then returns the one before it.
        with conn.transaction():
            cur = conn.cursor()
            cur.execute("declare ok cursor for select * from generate_series(0, 5)")
            cur.execute("move 5 from ok")
            cur.execute("fetch backward 1 from ok")
            assert [r[0] for r in cur.fetchall()] == [3]


def test_binary_server_cursor_returns_binary_rows(home: Path) -> None:
    """A binary server cursor's FETCH re-encodes rows in the binary wire format.

    The cursor's rows are frozen in TEXT at DECLARE, but psycopg requests BINARY
    on the FETCH -- so the server must re-encode from the captured typed values.
    A text server cursor over the same query still returns text.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("create table tb (x int4)")
        conn.cursor().execute("insert into tb values (1), (2)")
        with conn.transaction():
            cur = conn.cursor("bc", binary=True)
            cur.execute("select x from tb order by x")
            assert cur.fetchone() == (1,)
            assert cur.pgresult.fformat(0) == 1
            assert cur.pgresult.get_value(0, 0) == b"\x00\x00\x00\x01"
            assert cur.fetchone() == (2,)
            assert cur.pgresult.get_value(0, 0) == b"\x00\x00\x00\x02"
        with conn.transaction():
            tcur = conn.cursor("tc")  # text cursor
            tcur.execute("select x from tb order by x")
            assert tcur.fetchone() == (1,)
            assert tcur.pgresult.fformat(0) == 0
            assert tcur.pgresult.get_value(0, 0) == b"1"


def test_generate_series_with_a_cast_in_the_target_list(home: Path) -> None:
    """`select generate_series(...)::type` casts each generated value.

    The cast rides as an ordinary per-row cast, and the described column type is
    the cast's -- so a binary server cursor over it decodes against the right
    oid. A constant WHERE over the series filters it (PG 16.15: `where false`
    is no rows, `where true` all three).
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select generate_series(1, 3)::int8 as n")
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3]
        assert cur.description[0].name == "n"
        assert cur.description[0].type_code == conn.adapters.types["int8"].oid
        cur.execute("select generate_series(1, 2)::text")
        assert [r[0] for r in cur.fetchall()] == ["1", "2"]
        cur.execute("select generate_series(1, 3) where false")
        assert cur.fetchall() == []
        cur.execute("select generate_series(1, 3) where true")
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3]


def test_pg_backend_pid_returns_the_connections_pid(home: Path) -> None:
    """`pg_backend_pid()` reports the same PID the startup BackendKeyData did.

    The server only knows the PID pgwire assigned during startup, so the
    function is resolved from connection state rather than the stateless
    planner. Probed against PG 14, where the two always agree.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select pg_backend_pid()")
        (pid,) = cur.fetchone()
        assert pid == conn.pgconn.backend_pid
        assert isinstance(pid, int)


def test_pg_terminate_backend_on_self_breaks_the_connection(home: Path) -> None:
    """`pg_terminate_backend(pg_backend_pid())` ends the connection with a
    57P01, exactly as a real backend torn down by an administrator does. The
    connection must read as CLOSED afterwards -- the simple protocol carries no
    ReadyForQuery on a FATAL, so the client learns the socket is gone. Probed
    against PG 14."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        with pytest.raises(psycopg.OperationalError) as exc:
            cur.execute("select pg_terminate_backend(pg_backend_pid())")
        assert exc.value.sqlstate == "57P01"
        assert isinstance(exc.value, psycopg.errors.AdminShutdown)
        assert conn.closed


def test_pg_terminate_backend_with_a_bound_pid_parameter(home: Path) -> None:
    """The same self-termination through the extended protocol: the PID is a
    bound parameter rather than a nested call. psycopg raises AdminShutdown and
    the connection breaks. Probed against PG 14."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        with pytest.raises(psycopg.errors.AdminShutdown):
            cur.execute("select pg_terminate_backend(%s)", [conn.pgconn.backend_pid])
        assert conn.closed


def test_pg_terminate_backend_across_connections(home: Path) -> None:
    """One connection terminates ANOTHER by PID. The victim notices at its next
    statement and ends with 57P01; terminating a PID that is not a live backend
    returns false. Probed against PG 14."""
    with _Server(home) as server, server.connect() as victim, server.connect() as killer:
        victim_pid = victim.pgconn.backend_pid
        kcur = killer.cursor()
        kcur.execute("select pg_terminate_backend(%s)", [victim_pid])
        assert kcur.fetchone() == (True,)
        # The victim finds out on its next statement.
        with pytest.raises(psycopg.OperationalError) as exc:
            victim.execute("select 1")
        assert exc.value.sqlstate == "57P01"
        # A PID nobody is using is not a backend -> false.
        kcur.execute("select pg_terminate_backend(2147483)")
        assert kcur.fetchone() == (False,)


def test_error_inside_transaction_poisons_the_block(home: Path) -> None:
    """A syntax error inside a transaction aborts the block: every later
    statement gets 25P02 until COMMIT/ROLLBACK, and the COMMIT of a failed
    block rolls back. This holds whether the failing statement fails while the
    simple protocol SPLITS it or while the extended protocol DESCRIBES it.
    Probed against PG 14."""
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("create table foo (id int primary key)")
        cur.execute("begin")
        cur.execute("insert into foo values (1)")
        with pytest.raises(psycopg.errors.SyntaxError):
            cur.execute("meh")
        # The block is now aborted: a perfectly valid statement is refused.
        with pytest.raises(psycopg.errors.InFailedSqlTransaction) as exc:
            cur.execute("select 1")
        assert exc.value.sqlstate == "25P02"
        # COMMIT of a failed block discards the write.
        cur.execute("commit")
        cur.execute("select count(*) from foo")
        assert cur.fetchone() == (0,)


def test_array_literal_keeps_unicode_whitespace_elements(home: Path) -> None:
    """The array scanner trims only PostgreSQL's `array_isspace` set (space,
    tab, newline, CR, VT, FF) -- never U+0085 / U+00A0, which Rust's
    `char::is_whitespace` strips. `select %s::text[]` with `['\\x85']` used
    to come back as the EMPTY array. Probed against PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for fmt in ("s", "t", "b"):
            for ch in ("\x85", "\xa0", " a "):
                cur.execute(f"select %{fmt}::text[]", ([ch],))
                assert cur.fetchone() == ([ch],), (fmt, ch)
        cur.execute("select '{ a , b }'::text[]")
        assert cur.fetchone() == (["a", "b"],)
        cur.execute("select E'{\\ta\\t,\\nb\\v\\f}'::text[]")
        assert cur.fetchone() == (["a", "b"],)
        cur.execute("select E'{\\u0085a}'::text[]")
        assert cur.fetchone() == (["\x85a"],)
        cur.execute('select \'{a b, "c d", " e ", NULL, "null"}\'::text[]')
        assert cur.fetchone() == (["a b", "c d", " e ", None, "null"],)
        cur.execute("select '{ { NULL } }'::text[]")
        assert cur.fetchone() == ([[None]],)


def test_array_literal_grammar_matches_postgresql(home: Path) -> None:
    """The literal forms PostgreSQL accepts and rejects (probed against PG 16):
    unquoted `NULL` is the null element while `"null"` is a string, a
    backslash escapes in and out of quotes, and each malformed shape is 22P02."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cases = {
            "{a b}": ["a b"],
            '{ NULL , "null", NuLl}': [None, "null", None],
            '{a\\,b, \\a, "a\\"b"}': ["a,b", "a", 'a"b'],
            "{}": [],
        }
        for literal, expected in cases.items():
            cur.execute("select %s::text[]", (literal,))
            assert cur.fetchone() == (expected,), literal
        for bad in ("{a,}", "{,a}", "{{}}", "{a}x", '{"a"b}', "{a", "a}", "{{a},{b,c}}", "{{a},b}"):
            with pytest.raises(psycopg.errors.InvalidTextRepresentation) as exc:
                cur.execute("select %s::text[]", (bad,))
            assert exc.value.sqlstate == "22P02", bad


def test_array_literal_dimension_decoration(home: Path) -> None:
    """`[lo:hi]...={...}` is accepted, checked against the contents, and
    otherwise discarded -- the value has no lower bounds. Probed against PG
    16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select '[0:1]={a,b}'::text[]")
        assert cur.fetchone() == (["a", "b"],)
        cur.execute("select '[1:1][-2:-1][3:5]={{{1,2,3},{4,5,6}}}'::int[]")
        assert cur.fetchone() == ([[[1, 2, 3], [4, 5, 6]]],)
        cur.execute("select '[0:1]={a,b}'::text[] || %s::text[]", (["c"],))
        assert cur.fetchone() == (["a", "b", "c"],)
        for bad in ("[0:0]={a,b}", "[1:2={a,b}", "[1:2]{a,b}"):
            with pytest.raises(psycopg.errors.InvalidTextRepresentation):
                cur.execute("select %s::text[]", (bad,))


NESTED_TEXT = [[["fo{o", "ba}r"], ['ba"z', "qu'x"], ["qu ux", " "]]]


def test_nested_arrays_round_trip_in_every_format(home: Path) -> None:
    """A multidimensional array parses to nested lists from its text literal,
    from a text parameter, and from a BINARY parameter (whose decoder used to
    refuse `ndim > 1`), and renders back the same way. Probed against PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select '{{{{{{\"NULL\"}}}}}}'::text[]")
        assert cur.fetchone() == ([[[[[["NULL"]]]]]],)
        for fmt in ("s", "t", "b"):
            cur.execute(f"select %{fmt}::text[]", (NESTED_TEXT,))
            assert cur.fetchone() == (NESTED_TEXT,), fmt
            cur.execute(f"select %{fmt}::int[]", ([[1, 2], [3, 4]],))
            assert cur.fetchone() == ([[1, 2], [3, 4]],), fmt
            cur.execute(f"select %{fmt}::text[]", ([[[[[["NULL"]]]]]],))
            assert cur.fetchone() == ([[[[[["NULL"]]]]]],), fmt
        bcur = conn.cursor(binary=True)
        bcur.execute("select %b::int[]", ([[1, None], [3, 4]],))
        assert bcur.fetchone() == ([[1, None], [3, 4]],)


def test_array_concatenation_follows_array_cat(home: Path) -> None:
    """`||` on arrays is `array_cat` / `array_append` / `array_prepend`: same
    dimensionality appends, an (N-1)-dim side becomes a new slice, a NULL side
    yields the other, and the result is typed as the array. Probed against PG
    16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cases = [
            ("select array[1] || null", [1]),
            ("select null || array[1]", [1]),
            ("select array[1] || 2", [1, 2]),
            ("select 0 || array[1]", [0, 1]),
            ("select array[[1,2]] || array[3,4]", [[1, 2], [3, 4]]),
            ("select array[3,4] || array[[1,2]]", [[3, 4], [1, 2]]),
            ("select array[[1,2]] || array[[3,4]]", [[1, 2], [3, 4]]),
            ("select '{}'::int[] || array[1]", [1]),
            ("select '{{1,2},{3,4}}'::int[] || '{5,6}'", [[1, 2], [3, 4], [5, 6]]),
        ]
        for sql, expected in cases:
            cur.execute(sql)
            assert cur.fetchone() == (expected,), sql
        cur.execute("select pg_typeof(array[1] || 2)")
        assert cur.fetchone() == ("integer[]",)
        with pytest.raises(psycopg.errors.InvalidTextRepresentation):
            cur.execute("select array['a'] || 'b'")


def test_expressions_over_generate_series_rows(home: Path) -> None:
    """A select-list expression over a `generate_series` row -- arithmetic, a
    cast, a scalar call, date arithmetic, a comparison -- evaluates per row,
    is named as PostgreSQL names it, and is typed from the row's declared
    column type. Every value, name and `pg_typeof` here is PG 16's."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select i, i + 1, i::int8 as j, i * 2.5, abs(-i), "
            "'2021-01-01'::date + i, i > 1 from generate_series(1, 2) as i"
        )
        assert [d.name for d in cur.description] == [
            "i",
            "?column?",
            "j",
            "?column?",
            "abs",
            "?column?",
            "?column?",
        ]
        assert cur.fetchall() == [
            (1, 2, 1, Decimal("2.5"), 1, dt.date(2021, 1, 2), False),
            (2, 3, 2, Decimal("5.0"), 2, dt.date(2021, 1, 3), True),
        ]
        cur.execute(
            "select pg_typeof(abs(-i)), pg_typeof(i + 1), pg_typeof(i * 2.5), "
            "pg_typeof(i::int8), pg_typeof('2021-01-01'::date + i), "
            "pg_typeof(i > 1), pg_typeof(i) from generate_series(1, 1) as i"
        )
        assert cur.fetchone() == (
            "integer",
            "integer",
            "numeric",
            "bigint",
            "date",
            "boolean",
            "integer",
        )
        # The stream path (Execute with a row limit) takes the same route.
        rows = list(
            cur.stream(
                "select i, '2021-01-01'::date + i from generate_series(1, %s) as i",
                [2],
            )
        )
        assert rows == [(1, dt.date(2021, 1, 2)), (2, dt.date(2021, 1, 3))]


def test_describe_distinguishes_no_columns_from_no_rows(home: Path) -> None:
    """Describe answers `NoData` only for a statement that returns no rows at
    all; a `select` with an empty target list is a RowDescription of zero
    fields and yields one empty row. psycopg's `stream()` reads the difference:
    a NoData select raised `the last operation didn't produce a result`.
    Probed against PG 16 at the protocol level."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        assert list(cur.stream("select")) == [()]
        assert cur.description == []
        cur.execute("create table if not exists nodata_t (a int)")
        assert cur.description is None
        assert list(cur.stream("select %s::int", [1], binary=True)) == [(1,)]
        with conn.cursor(binary=True) as bcur:
            assert list(bcur.stream("select %s::int", [1])) == [(1,)]


def test_serial_columns_and_insert_returning(home: Path) -> None:
    """`serial` / `bigserial` draw from a `<table>_<column>_seq` sequence, an
    empty bound list binds as an empty array, and `RETURNING` answers named,
    computed and `*` columns typed from the row. Every value here is PG 16's
    (a fresh store, so the first id is 1)."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table fp_test (id serial primary key, data date[])")
        for fmt in ("s", "t", "b"):
            cur.execute(f"insert into fp_test (data) values (%{fmt}) returning id", ([],))
            assert (cur.rowcount, cur.statusmessage) == (1, "INSERT 0 1")
            assert [d.type_code for d in cur.description] == [23]
        assert cur.fetchone() == (3,)
        cur.execute("insert into fp_test (data) values (%s), (%s)", (["2021-01-01"], []))
        assert (cur.rowcount, cur.statusmessage) == (2, "INSERT 0 2")
        cur.execute("select id, data from fp_test order by id")
        assert cur.fetchall() == [
            (1, []),
            (2, []),
            (3, []),
            (4, [dt.date(2021, 1, 1)]),
            (5, []),
        ]
        cur.execute("select data from fp_test where id = any(%s)", ([1],))
        assert cur.fetchall() == [([],)]
        cur.execute("select data from fp_test where id = any(%s)", ([],))
        assert cur.fetchall() == []
        # An explicit id neither draws from nor advances the sequence.
        cur.execute("insert into fp_test (id, data) values (100, null) returning id")
        assert cur.fetchone() == (100,)
        cur.execute(
            "insert into fp_test (data) values (null) "
            "returning id, data, id * 2, pg_typeof(id)::text"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("id", 23),
            ("data", 1182),
            ("?column?", 23),
            ("pg_typeof", 25),
        ]
        assert cur.fetchone() == (6, None, 12, "integer")
        cur.execute("insert into fp_test (data) values (null) returning *")
        assert [d.name for d in cur.description] == ["id", "data"]
        assert cur.fetchone() == (7, None)
        # A cast of a column keeps the column's name; a cast of an expression
        # is named after the type.
        cur.execute("insert into fp_test (data) values (null) returning id::int8, (1+2)::int8")
        assert [(d.name, d.type_code) for d in cur.description] == [("id", 20), ("int8", 20)]
        assert cur.fetchone() == (8, 3)
        with pytest.raises(psycopg.errors.UndefinedColumn) as ei:
            cur.execute("insert into fp_test (data) values (null) returning nope")
        assert str(ei.value).startswith('column "nope" does not exist')

        cur.execute("create table fp_big (id bigserial, n int)")
        cur.execute("insert into fp_big (n) values (1) returning id, pg_typeof(id)::text")
        assert [d.type_code for d in cur.description] == [20, 25]
        assert cur.fetchone() == (1, "bigint")
        cur.execute("insert into fp_big (n) values (2), (3) returning id")
        assert cur.fetchall() == [(2,), (3,)]
        cur.executemany(
            "insert into fp_big (n) values (%s) returning n, id",
            [(10,), (20,)],
            returning=True,
        )
        assert (cur.rowcount, cur.statusmessage, cur.fetchone()) == (1, "INSERT 0 1", (10, 4))
        assert cur.nextset() is True
        assert (cur.rowcount, cur.fetchone(), cur.nextset()) == (1, (20, 5), None)

        # Dropping the table drops its sequence: a re-created table starts
        # over at 1.
        cur.execute("drop table fp_test")
        cur.execute("create table fp_test (id serial primary key, data date[])")
        cur.execute("insert into fp_test (data) values (null) returning id")
        assert cur.fetchone() == (1,)


def test_show_and_insert_select_rowcounts(home: Path) -> None:
    """`SHOW` completes with a bare `SHOW` tag and one row; `INSERT ... SELECT`
    inserts the query's rows (it used to write nothing and say `INSERT 0 0`),
    with PostgreSQL's width errors and default-filled trailing columns.
    Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("show timezone")
        assert (cur.rowcount, cur.statusmessage) == (1, "SHOW")
        assert [(d.name, d.type_code) for d in cur.description] == [("TimeZone", 25)]
        cur.execute("create table fp_trc (id int primary key, n int default 7)")
        assert (cur.rowcount, cur.statusmessage) == (-1, "CREATE TABLE")
        cur.execute("insert into fp_trc select generate_series(1, 42)")
        assert (cur.rowcount, cur.statusmessage) == (42, "INSERT 0 42")
        cur.execute("select count(*), min(n), max(n) from fp_trc")
        assert cur.fetchone() == (42, 7, 7)
        cur.execute(
            "insert into fp_trc (id, n) select i * 100, i + 1 "
            "from generate_series(1, 3) as i returning id, n"
        )
        assert (cur.rowcount, cur.statusmessage) == (3, "INSERT 0 3")
        assert cur.fetchall() == [(100, 2), (200, 3), (300, 4)]
        cur.execute("insert into fp_trc select 700 where false")
        assert (cur.rowcount, cur.statusmessage) == (0, "INSERT 0 0")
        with pytest.raises(psycopg.errors.UniqueViolation):
            cur.execute("insert into fp_trc select 1")
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            cur.execute("insert into fp_trc select 'a'")
        assert str(ei.value).startswith('invalid input syntax for type integer: "a"')
        for sql in (
            "insert into fp_trc select 200, 1, 2",
            "insert into fp_trc (id) select 300, 1",
            "insert into fp_trc values (600, 1, 2)",
        ):
            with pytest.raises(psycopg.errors.SyntaxError) as ei:
                cur.execute(sql)
            assert str(ei.value).startswith("INSERT has more expressions than target columns")
        for sql in (
            "insert into fp_trc (id, n) select 400",
            "insert into fp_trc (id, n) values (500)",
        ):
            with pytest.raises(psycopg.errors.SyntaxError) as ei:
                cur.execute(sql)
            assert str(ei.value).startswith("INSERT has more target columns than expressions")
        # COPY of a query evaluates its expressions rather than reading the
        # bare columns.
        with cur.copy(
            "copy (select id + 1, n::text || 'x' from fp_trc where id <= 2 order by id) to stdout"
        ) as cp:
            assert b"".join(cp) == b"2\t7x\n3\t7x\n"


def test_literal_column_defaults(home: Path) -> None:
    """A literal DEFAULT is applied to every column an INSERT omits, whatever
    the INSERT's shape; an explicit NULL is kept; a DEFAULT that will not
    parse as the column's type fails at CREATE. An expression default is
    refused (0A000) rather than silently dropped. Values are PG 16's."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "create table fp_d (id int primary key, n int default 7, t text default 'x', "
            "d date default '2021-01-02', z int default null, f float8 default 1.5, "
            "b bool default true)"
        )
        cur.execute("insert into fp_d (id) values (1)")
        cur.execute("insert into fp_d values (2)")
        cur.execute("insert into fp_d (id, n, t) values (3, null, 'y')")
        cur.execute("insert into fp_d select 4")
        cur.execute("insert into fp_d (id, t) select 5, 'q' returning *")
        assert cur.fetchone() == (5, 7, "q", dt.date(2021, 1, 2), None, 1.5, True)
        cur.execute("select * from fp_d order by id")
        assert cur.fetchall() == [
            (1, 7, "x", dt.date(2021, 1, 2), None, 1.5, True),
            (2, 7, "x", dt.date(2021, 1, 2), None, 1.5, True),
            (3, None, "y", dt.date(2021, 1, 2), None, 1.5, True),
            (4, 7, "x", dt.date(2021, 1, 2), None, 1.5, True),
            (5, 7, "q", dt.date(2021, 1, 2), None, 1.5, True),
        ]
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            cur.execute("create table fp_e (id int, n int default 'a')")
        assert str(ei.value).startswith('invalid input syntax for type integer: "a"')
        # A volatile DEFAULT is kept as its expression and stamps each INSERT,
        # as PostgreSQL does -- it was refused before, rather than frozen at
        # CREATE time.
        cur.execute(
            "create table fp_e (id int primary key, n timestamptz default current_timestamp)"
        )
        cur.execute("insert into fp_e (id) values (1) returning n is not null")
        assert cur.fetchone() == (True,)
    # The default survives a restart: it is in the catalog, not the session.
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("insert into fp_d (id) values (6) returning n, t")
        assert cur.fetchone() == (7, "x")


def test_constant_where_on_a_from_less_select(home: Path) -> None:
    """With no FROM, a WHERE is a constant predicate on the one row: false or
    NULL is zero rows (the predicate used to be ignored), a non-boolean is
    42804, and an unknown literal is read as a boolean. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, args in (
            ("select 1 where false", None),
            ("select 1 where null", None),
            ("select 1 where %s", (False,)),
            ("select %s where %s", (5, False)),
            ("select 1 where 1 < 2 and false", None),
        ):
            cur.execute(sql, args)
            assert (cur.fetchall(), cur.statusmessage) == ([], "SELECT 0"), sql
        for sql, args, row in (
            ("select 1, 'a' where true", None, (1, "a")),
            ("select 1 where 1 = 1", None, (1,)),
            ("select 1 where %s", (True,), (1,)),
            ("select 1 where 't'", None, (1,)),
        ):
            cur.execute(sql, args)
            assert (cur.fetchall(), cur.statusmessage) == ([row], "SELECT 1"), sql
        with pytest.raises(psycopg.errors.DatatypeMismatch) as ei:
            cur.execute("select 1 where 1")
        assert str(ei.value).startswith("argument of WHERE must be type boolean, not type integer")
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            cur.execute("select 1 where 'x'")
        assert str(ei.value).startswith('invalid input syntax for type boolean: "x"')


def test_pg_sleep_waits_and_returns_void(home: Path) -> None:
    """`pg_sleep(seconds)` waits on the connection's thread and answers a
    `void` (oid 2278) column rendered as the empty string in both formats;
    zero or negative seconds return at once, NULL is NULL, and a bad literal
    is 22P02 for double precision. psycopg's `test_executemany_lock` needs
    it. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        started = time.monotonic()
        cur.execute("select pg_sleep(0.2)")
        assert time.monotonic() - started >= 0.2
        assert [(d.name, d.type_code) for d in cur.description] == [("pg_sleep", 2278)]
        assert (cur.fetchall(), cur.statusmessage) == ([("",)], "SELECT 1")
        cur.execute("select pg_sleep(%s)", (0,))
        assert cur.fetchall() == [("",)]
        cur.execute("select pg_sleep(-1), 1")
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("pg_sleep", 2278),
            ("?column?", 23),
        ]
        assert cur.fetchall() == [("", 1)]
        cur.execute("select pg_sleep(null)")
        assert cur.fetchall() == [(None,)]
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            cur.execute("select pg_sleep('x')")
        assert str(ei.value).startswith('invalid input syntax for type double precision: "x"')
        with conn.cursor(binary=True) as bcur:
            bcur.execute("select pg_sleep(0)")
            assert bcur.fetchall() == [(b"",)]
        # A false WHERE drops the row without waiting.
        started = time.monotonic()
        cur.execute("select pg_sleep(5) where false")
        assert cur.fetchall() == []
        assert time.monotonic() - started < 1


def test_crossed_range_inside_an_array_literal_is_a_data_error(home: Path) -> None:
    """A range element whose bounds cross is 22000 from the range parser, not
    a malformed-array 22P02: the array literal's structure is fine. Probed
    PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        with pytest.raises(psycopg.errors.DataException) as ei:
            cur.execute("select '{\"[5,1]\"}'::int4range[]")
        assert str(ei.value).startswith(
            "range lower bound must be less than or equal to range upper bound"
        )


def test_session_user_value_functions(home: Path) -> None:
    """`user` / `current_user` / `session_user` / `current_role` answer the
    role the client connected as (this fixture connects as `test`),
    `current_catalog` / `current_schema` are `postgres` / `public`, every
    column is typed `name` (oid 19) and named after its keyword, and the
    QUOTED form `"user"` is an ordinary (missing) column. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select user, current_user, session_user, current_catalog, current_schema, current_role"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("user", 19),
            ("current_user", 19),
            ("session_user", 19),
            ("current_catalog", 19),
            ("current_schema", 19),
            ("current_role", 19),
        ]
        assert cur.fetchall() == [("test", "test", "test", "postgres", "public", "test")]
        cur.execute("select user as u")
        assert [(d.name, d.type_code) for d in cur.description] == [("u", 19)]
        assert cur.fetchall() == [("test",)]
        with pytest.raises(psycopg.errors.UndefinedColumn) as ei:
            cur.execute('select "user"')
        assert str(ei.value).startswith('column "user" does not exist')


def test_box_type_casts_and_arrays(home: Path) -> None:
    """`box` (oid 603, array 1020): every input spelling normalises to
    `(high),(low)` with the corners re-ordered per coordinate, a NaN lands in
    the high corner, `box[]` is delimited by `;` rather than `,` (so its
    elements are never quoted), a text-typed array of boxes IS quoted, a
    bound text parameter casts, and the errors are PostgreSQL's: 22P02 for
    a malformed box, 22003 for a coordinate out of float8 range, 42846 for
    the casts PostgreSQL does not have. Every value probed on PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select '(1,2),(3,4)'::box, '((1,2),(3,4))'::box, '1,2,3,4'::box, "
            "'(1,4),(3,2)'::box, '(1.5,2),(3,4e2)'::box, '(2,3),(nan,1)'::box, "
            "'(1,2),(3,4)'::box::text, pg_typeof('(1,2),(3,4)'::box)"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("box", 603),
            ("box", 603),
            ("box", 603),
            ("box", 603),
            ("box", 603),
            ("box", 603),
            ("text", 25),
            ("pg_typeof", 2206),
        ]
        assert cur.fetchall() == [
            (
                "(3,4),(1,2)",
                "(3,4),(1,2)",
                "(3,4),(1,2)",
                "(3,4),(1,2)",
                "(3,400),(1.5,2)",
                "(NaN,3),(2,1)",
                "(3,4),(1,2)",
                "box",
            )
        ]
        cur.execute(
            "select array['(1,2),(3,4)'::box, '(5,6),(7,8)'::box], "
            "array['(1,2),(3,4)'::box, null], "
            "'{(1,2),(3,4);(5,6),(7,8)}'::box[], '{\"(1,2),(3,4)\"}'::box[], "
            "array['(1,2),(3,4)'::box]::text[], pg_typeof(array['(1,2),(3,4)'::box])"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("array", 1020),
            ("array", 1020),
            ("box", 1020),
            ("box", 1020),
            ("array", 1009),
            ("pg_typeof", 2206),
        ]
        assert cur.fetchall() == [
            (
                ["(3,4),(1,2)", "(7,8),(5,6)"],
                ["(3,4),(1,2)", None],
                ["(3,4),(1,2)", "(7,8),(5,6)"],
                ["(3,4),(1,2)"],
                ["(3,4),(1,2)"],
                "box[]",
            )
        ]
        # The raw wire text of a box[] uses the `;` delimiter, unquoted.
        cur.execute("select '{(1,2),(3,4);(5,6),(7,8)}'::box[]::text")
        assert cur.fetchall() == [("{(3,4),(1,2);(7,8),(5,6)}",)]
        cur.execute("select %s::box", ("(1,2),(3,4)",))
        assert [(d.name, d.type_code) for d in cur.description] == [("box", 603)]
        assert cur.fetchall() == [("(3,4),(1,2)",)]
        # psycopg dumps a text list with `,` -- which is not box[]'s
        # delimiter -- so the literal is malformed, on PostgreSQL too.
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            cur.execute("select %s::box[]", (["(1,2),(3,4)", "(5,6),(7,8)"],))
        assert str(ei.value).startswith('malformed array literal: "{"(1,2),(3,4)","(5,6),(7,8)"}"')
        for bad in ("(1,2),(3)", "(1,2),(3,4),(5,6)", "x", "(1,2)", "((1,2),(3,4)"):
            with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
                cur.execute("select %s::box", (bad,))
            assert str(ei.value).startswith(f'invalid input syntax for type box: "{bad}"')
        with pytest.raises(psycopg.errors.NumericValueOutOfRange) as ei:
            cur.execute("select '(1e400,2),(3,4)'::box")
        assert str(ei.value).startswith('"1e400" is out of range for type double precision')
        with pytest.raises(psycopg.errors.CannotCoerce) as ei:
            cur.execute("select 5::box")
        assert str(ei.value).startswith("cannot cast type integer to box")
        with pytest.raises(psycopg.errors.CannotCoerce) as ei:
            cur.execute("select '(1,2),(3,4)'::box::float8")
        assert str(ei.value).startswith("cannot cast type box to double precision")


def test_float8_text_is_float8out(home: Path) -> None:
    """A double's text form is PostgreSQL's `float8out` -- shortest round-trip
    digits, exponent form outside 1e-4..1e15 with a two-digit signed
    exponent, `Infinity` / `-Infinity` / `NaN`, a signed `-0` -- in a column,
    in a cast and inside a `float8[]`. A `float8` with a numeric operand is
    float8 arithmetic, unary minus keeps a double's signed zero, and numeric
    has no negative zero at all. Every value probed on PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select 1e20::float8::text, 1e-7::float8::text, 1e15::float8::text, "
            "1e14::float8::text, 0.00001::float8::text, 0.0001::float8::text, "
            "'inf'::float8::text, '-inf'::float8::text, 'nan'::float8::text, "
            "1.5::float8::text, 2::float8::text, 123456789012345678::float8::text, "
            "(0.1::float8 + 0.2)::text"
        )
        assert cur.fetchall() == [
            (
                "1e+20",
                "1e-07",
                "1e+15",
                "100000000000000",
                "1e-05",
                "0.0001",
                "Infinity",
                "-Infinity",
                "NaN",
                "1.5",
                "2",
                "1.2345678901234568e+17",
                "0.30000000000000004",
            )
        ]
        cur.execute(
            "select array[1e20::float8, 0.5]::text, array[1.5::float8, 2]::text, "
            "array[1e20::float8, 0.5], array[1.5::float8, 2]"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("array", 25),
            ("array", 25),
            ("array", 1022),
            ("array", 1022),
        ]
        assert cur.fetchall() == [("{1e+20,0.5}", "{1.5,2}", [1e20, 0.5], [1.5, 2.0])]
        cur.execute(
            "select 0.1::float8 + 0.2, 1.5::numeric + 1::float8, 2::float8 * 1.5, 1.5 / 2::float8"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("?column?", 701),
            ("?column?", 701),
            ("?column?", 701),
            ("?column?", 701),
        ]
        assert cur.fetchall() == [(0.30000000000000004, 2.5, 3.0, 0.75)]
        cur.execute(
            "select (-(0.0::float8))::text, (-0.0::float8)::text, ('-0'::float8)::text, "
            "(- 0.0)::text, (-0.00e3)::text, (0.0 - 0.0)::text, 0.00e3::text"
        )
        assert cur.fetchall() == [("-0", "-0", "-0", "0.0", "0", "0.0", "0")]


def test_constant_select_columns_are_named_as_postgresql_names_them(home: Path) -> None:
    """A FROM-less select names an unaliased cast after its target type's
    catalog name (`int4`, `float8`, `bpchar`, `numeric`, `char`), a nested
    cast after the OUTER type, the constructor keywords after themselves
    (`array`, `row`, `coalesce`, `greatest`, `least`, `nullif`), a cast of a
    call after the call, and everything else `?column?`. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select array[1], row(1), coalesce(1), greatest(1,2), least(1), "
            "('1'::text)::int8, -1::int, 1 + 1, nullif(1,2), not true, "
            "true and false, 1 = any(array[1]), (1), ((1::int)), "
            "1::double precision, 'a'::char(2), 1::decimal, '1'::\"char\", "
            "cast(1 as int), abs(-1)::int8"
        )
        assert [d.name for d in cur.description] == [
            "array",
            "row",
            "coalesce",
            "greatest",
            "least",
            "int8",
            "?column?",
            "?column?",
            "nullif",
            "?column?",
            "?column?",
            "?column?",
            "?column?",
            "int4",
            "float8",
            "bpchar",
            "numeric",
            "char",
            "int4",
            "abs",
        ]


def test_boolean_casts_exist_only_for_integer_and_text(home: Path) -> None:
    """`boolean` has casts from `integer` and text and to `integer`; every
    other numeric, temporal or integer-width pairing is 42846 `cannot cast
    type X to Y` (a `1.5::bool` used to be a 22P02 parse failure). The source
    type is what decides -- `'2021-01-01'::bool` is the 22P02 text failure
    while `'2021-01-01'::date::bool` is 42846 -- and a NULL casts through the
    same rules. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select 1::bool, 0::bool, 2::bool, 'yes'::bool, 't'::bool::int, "
            "1::bool::int, false::int, true::int4"
        )
        assert cur.fetchone() == (True, False, True, True, 1, 1, 0, 1)
        assert [d.type_code for d in cur.description] == [16, 16, 16, 16, 23, 23, 23, 23]
        for sql, message in [
            ("select 1.5::bool", "cannot cast type numeric to boolean"),
            ("select 1::int8::bool", "cannot cast type bigint to boolean"),
            ("select 1::int2::bool", "cannot cast type smallint to boolean"),
            ("select 1.5::float8::bool", "cannot cast type double precision to boolean"),
            ("select '2021-01-01'::date::bool", "cannot cast type date to boolean"),
            ("select null::date::bool", "cannot cast type date to boolean"),
            ("select true::int8", "cannot cast type boolean to bigint"),
            ("select true::int2", "cannot cast type boolean to smallint"),
        ]:
            with pytest.raises(psycopg.errors.CannotCoerce) as ei:
                cur.execute(sql)
            assert str(ei.value).startswith(message), sql
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            cur.execute("select '2021-01-01'::bool")
        assert str(ei.value).startswith('invalid input syntax for type boolean: "2021-01-01"')


def test_pg_prepared_statements_lists_protocol_prepared_statements(home: Path) -> None:
    """`pg_prepared_statements` shows the connection's NAMED protocol-level
    statements with PG's columns: `parameter_types` as regtype display names,
    `result_types` NULL for a statement that returns no rows, `from_sql`
    false, and a statement without parameters counted as one generic plan
    where a parameterised one counts as one custom plan (psycopg executes
    once at prepare time). The unnamed statement never appears. Probed PG 16
    with psycopg 3.3 / libpq 18."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table pp (id serial primary key, num int, s text, j jsonb)")
        cur.execute("insert into pp (num, s) values (1, 'a')")
        cur.execute("select count(*) from pg_prepared_statements")
        assert cur.fetchone() == (0,)
        cur.execute("select num + %s::smallint, s || %s from pp", (1, "x"), prepare=True)
        cur.execute("update pp set num = %s", (5,), prepare=True)
        cur.execute(
            "insert into pp (j) values (%s)", (psycopg.types.json.Jsonb({"a": 1}),), prepare=True
        )
        cur.execute("select 1", prepare=True)
        cur.execute("select 2", prepare=False)
        cur.execute(
            "select name, statement, parameter_types, result_types, from_sql, "
            "generic_plans, custom_plans from pg_prepared_statements order by name"
        )
        assert cur.fetchall() == [
            (
                "_pg3_0",
                "select num + $1::smallint, s || $2 from pp",
                ["smallint", "text"],
                ["integer", "text"],
                False,
                0,
                1,
            ),
            ("_pg3_1", "update pp set num = $1", ["smallint"], None, False, 0, 1),
            ("_pg3_2", "insert into pp (j) values ($1)", ["jsonb"], None, False, 0, 1),
            ("_pg3_3", "select 1", [], ["integer"], False, 1, 0),
        ]
        cur.execute("select * from pg_prepared_statements")
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("name", 25),
            ("statement", 25),
            ("prepare_time", 1184),
            ("parameter_types", 2211),
            ("result_types", 2211),
            ("from_sql", 16),
            ("generic_plans", 20),
            ("custom_plans", 20),
        ]
        cur.execute("select prepare_time from pg_prepared_statements limit 1")
        (prepared_at,) = cur.fetchone()
        assert prepared_at.tzinfo is not None
        # DEALLOCATE of one name, then of all; a name the session does not
        # hold is 26000.
        cur.execute("deallocate _pg3_1")
        cur.execute("select name from pg_prepared_statements order by name")
        assert cur.fetchall() == [("_pg3_0",), ("_pg3_2",), ("_pg3_3",)]
        with pytest.raises(psycopg.errors.InvalidSqlStatementName) as ei:
            cur.execute("deallocate nosuch")
        assert str(ei.value).startswith('prepared statement "nosuch" does not exist')
        cur.execute("deallocate all")
        cur.execute("select count(*) from pg_prepared_statements")
        assert cur.fetchone() == (0,)
        cur.execute("notify foo")
        assert cur.statusmessage == "NOTIFY"


def test_protocol_close_removes_a_prepared_statement(home: Path) -> None:
    """psycopg evicts a prepared statement past `prepared_max` with the
    protocol `Close` message (libpq 18's `PQclosePrepared`), and the row
    leaves `pg_prepared_statements` with it. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        conn.prepare_threshold = 0
        conn.prepared_max = 1
        for i in range(3):
            conn.execute(f"select {i}")
        listing = "select name, statement from pg_prepared_statements order by name"
        assert conn.execute(listing).fetchall() == [("_pg3_2", "select 2"), ("_pg3_3", listing)]
        conn.execute("select 100")
        assert conn.execute(listing).fetchall() == [("_pg3_4", "select 100"), ("_pg3_5", listing)]


def test_uuid_text_input_forms(home: Path) -> None:
    """`uuid_in` accepts the 32-hex form, braces, upper case, and hyphens
    after ANY group of four hex digits -- not only at the canonical
    positions -- and rejects a short string, a non-hex digit, and leading
    whitespace with 22P02. A text parameter bound to a `$n::uuid` is
    canonicalised on the way in. Probed PG 16."""
    canonical = uuid.UUID("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11")
    with _Server(home) as server, server.connect() as conn:
        for text in [
            "a0eebc999c0b4ef8bb6d6bb9bd380a11",
            "{a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11}",
            "a0eebc99-9c0b4ef8-bb6d6bb9-bd380a11",
            "a0ee-bc99-9c0b-4ef8-bb6d-6bb9-bd38-0a11",
            "{a0eebc99-9c0b4ef8-bb6d6bb9-bd380a11}",
            "A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11",
        ]:
            assert conn.execute("select %s::uuid", (text,)).fetchone() == (canonical,), text
        for text in [
            "a0eebc999c0b4ef8bb6d6bb9bd380a1",
            "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a1z",
            " a0eebc999c0b4ef8bb6d6bb9bd380a11",
        ]:
            with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
                conn.execute("select %s::uuid", (text,))
            assert str(ei.value).startswith(f'invalid input syntax for type uuid: "{text}"')
        assert conn.execute(
            "select %s::uuid::text", ("a0eebc999c0b4ef8bb6d6bb9bd380a11",)
        ).fetchone() == (str(canonical),)
        assert conn.execute("select %s", (canonical,)).fetchone() == (canonical,)


def test_bytea_and_uuid_results_are_sent_binary_when_asked(home: Path) -> None:
    """A binary-format request gets `bytea`, `uuid`, and their arrays in
    binary (uuid as its 16 raw bytes), including NULL elements and a NULL
    value; `set_byte` answers bytea. Probed PG 16."""
    u = uuid.UUID("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11")
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=True)
        cur.execute(
            "select 'abc'::bytea, %s::uuid, array[%s::uuid, null], "
            "array['ab'::bytea, null], null::uuid, set_byte('abc'::bytea, 0, 100)",
            (str(u), str(u)),
        )
        assert cur.fetchone() == (b"abc", u, [u, None], [b"ab", None], None, b"dbc")
        assert [d.type_code for d in cur.description] == [17, 2950, 2951, 1001, 2950, 17]
        assert cur.pgresult is not None
        assert cur.pgresult.fformat(0) == 1
        # An untyped text parameter compared with a text expression.
        cur.execute("select %s = chr(%s)", ("a", 97))
        assert cur.fetchone() == (True,)
        cur.execute("select %s = 'a'::text", ("a",))
        assert cur.fetchone() == (True,)


def test_nul_byte_in_a_binary_text_parameter_is_22021(home: Path) -> None:
    """A binary-format text parameter carrying a NUL byte is refused with
    22021 `invalid byte sequence for encoding "UTF8": 0x00` (a text-format
    one never reaches the server: psycopg refuses it client-side). Probed
    PG 16."""
    with _Server(home) as server, server.connect() as conn:
        with pytest.raises(psycopg.errors.CharacterNotInRepertoire) as ei:
            conn.execute("select %b::text", ("a\x00b",))
        assert str(ei.value).startswith('invalid byte sequence for encoding "UTF8": 0x00')


def test_any_typed_functions_refuse_an_untyped_parameter(home: Path) -> None:
    """A function whose arguments are `any` / VARIADIC `any` cannot resolve
    a bare parameter: 42P18 `could not determine data type of parameter
    $n`, naming the FIRST unresolvable one -- `format`'s and `concat_ws`'s
    leading `text` argument resolves, so they fault `$2`. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        for call, faulted in [
            ("concat(%s, %s)", 1),
            ("concat_ws(%s, %s)", 2),
            ("format(%s, %s)", 2),
            ("num_nulls(%s, %s)", 1),
            ("num_nonnulls(%s, %s)", 1),
            ("json_build_array(%s, %s)", 1),
            ("jsonb_build_object(%s, %s)", 1),
        ]:
            with pytest.raises(psycopg.errors.IndeterminateDatatype) as ei:
                conn.execute(f"select {call}", ("a", "b"))
            assert str(ei.value).startswith(
                f"could not determine data type of parameter ${faulted}"
            ), call
        assert conn.execute("select concat_ws(%s, 'a', 'b')", ("x",)).fetchone() == ("axb",)


def test_copy_out_encoding_error_keeps_the_connection(home: Path) -> None:
    """A COPY TO STDOUT that fails mid-stream (a character the client
    encoding cannot represent) surfaces its 22P05 and leaves the connection
    usable and idle -- the server used to follow the error with a CopyFail,
    after which the client saw "you cannot mix COPY with other operations"
    on the next statement. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table enc (s text)")
        conn.execute("insert into enc values ('€')")
        conn.execute("set client_encoding to latin1")
        with (
            pytest.raises(psycopg.errors.UntranslatableCharacter) as ei,
            conn.cursor().copy("copy enc to stdout") as cp,
        ):
            list(cp)
        assert str(ei.value).startswith(
            'character with byte sequence 0xe2 0x82 0xac in encoding "UTF8" '
            'has no equivalent in encoding "LATIN1"'
        )
        assert conn.execute("select 1").fetchone() == (1,)
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.IDLE


def test_update_set_with_a_row_expression(home: Path) -> None:
    """`UPDATE ... SET col = <expression over the row>` -- `num * 2`,
    `upper(s)`, `s || 'x'`, `coalesce(num, 0)`, `num::text`, a bound
    parameter in the expression -- evaluates per matched row from the row's
    pre-update values, and an unknown column is 42703. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table ut (id serial primary key, num int, s text)")
        cur.execute("insert into ut (num, s) values (3, 'a'), (4, 'b'), (null, 'c')")
        rows = "select id, num, s from ut order by id"
        cur.execute("update ut set num = num * 2")
        assert cur.statusmessage == "UPDATE 3"
        assert cur.execute(rows).fetchall() == [(1, 6, "a"), (2, 8, "b"), (3, None, "c")]
        cur.execute("update ut set s = upper(s), num = num + 1 where num > 6")
        assert cur.statusmessage == "UPDATE 1"
        assert cur.execute(rows).fetchall() == [(1, 6, "a"), (2, 9, "B"), (3, None, "c")]
        cur.execute("update ut set num = num + %s where id = %s", (10, 1))
        assert cur.statusmessage == "UPDATE 1"
        assert cur.execute(rows).fetchall() == [(1, 16, "a"), (2, 9, "B"), (3, None, "c")]
        cur.execute("update ut set s = s || 'x', num = coalesce(num, 0)")
        assert cur.statusmessage == "UPDATE 3"
        assert cur.execute(rows).fetchall() == [(1, 16, "ax"), (2, 9, "Bx"), (3, 0, "cx")]
        cur.execute("update ut set s = num::text")
        assert cur.execute("select id, s from ut order by id").fetchall() == [
            (1, "16"),
            (2, "9"),
            (3, "0"),
        ]
        with pytest.raises(psycopg.errors.UndefinedColumn) as ei:
            cur.execute("update ut set num = nosuch * 2")
        assert str(ei.value).startswith('column "nosuch" does not exist')


def test_savepoint_rollback_of_ddl_on_a_fresh_store(home: Path) -> None:
    """On a store with no committed tables yet, ROLLBACK TO a savepoint
    still undoes a CREATE TYPE issued after it (the restore used to read the
    catalog through a session blind to the transaction's own writes, and
    left the type behind: `type ... already exists` on the retry); a table
    created before the savepoint survives while the rows written after it
    are undone. Probed PG 16."""
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        cur.execute("savepoint s1")
        cur.execute("create type prepenum as enum ('foo', 'bar')")
        cur.execute("rollback to savepoint s1")
        cur.execute("create type prepenum as enum ('foo', 'bar')")
        cur.execute("select 'prepenum'::regtype::text")
        assert cur.fetchone() == ("prepenum",)
        conn.rollback()
        cur.execute("create table sp_t (id serial primary key, n int)")
        cur.execute("savepoint s2")
        cur.execute("insert into sp_t (n) values (1)")
        cur.execute("rollback to savepoint s2")
        cur.execute("select count(*) from sp_t")
        assert cur.fetchone() == (0,)
        conn.commit()
        cur.execute("select count(*) from sp_t")
        assert cur.fetchone() == (0,)
        conn.commit()


# ---------------------------------------------------------------------------
# DO blocks, error diagnostics, numeric / oid / enum / record wire paths.
# Every expected value below was measured on PostgreSQL 16.15 (2026-09-09).
# ---------------------------------------------------------------------------


def test_unary_minus_on_numeric_specials(home: Path) -> None:
    """`-'NaN'::numeric` is NaN, and the infinities flip sign."""
    with _Server(home) as server, server.connect() as conn:
        row = conn.execute(
            "select -'NaN'::numeric, -'Infinity'::numeric, -'-Infinity'::numeric, -(1.5::numeric)"
        ).fetchone()
        assert row is not None
        assert row[0].is_nan()
        assert row[1:] == (Decimal("-Infinity"), Decimal("Infinity"), Decimal("-1.5"))


def test_oid_parameters_in_text_and_binary(home: Path) -> None:
    """An `Oid` parameter keeps its type (`pg_typeof` is `oid`, the column
    oid is 26), the full unsigned range round-trips, and an `oid[]` sent as
    text comes back binary as a 1028 array."""
    from psycopg.types.numeric import Oid

    with _Server(home) as server, server.connect() as conn:
        cur = conn.execute("select pg_typeof(%s)::text, %s", [Oid(10), Oid(4294967295)])
        assert cur.fetchone() == ("oid", 4294967295)
        assert cur.description is not None
        assert [d.type_code for d in cur.description] == [25, 26]
        cur = conn.cursor(binary=True)
        cur.execute("select pg_typeof(%s)::text, %s", [Oid(10), Oid(7)])
        assert cur.fetchone() == ("oid", 7)
        cur.execute("select %s, pg_typeof(%s)::text", [[Oid(1), Oid(2)], [Oid(1), Oid(2)]])
        assert cur.fetchone() == ([1, 2], "oid[]")
        assert cur.description is not None
        assert cur.description[0].type_code == 1028


def test_where_over_generate_series(home: Path) -> None:
    """A constant or column predicate filters the generated rows; a
    non-boolean constant is `42804`."""
    with _Server(home) as server, server.connect() as conn:
        assert conn.execute("select 1 from generate_series(1,3) where false").fetchall() == []
        assert conn.execute("select 1 from generate_series(1,3) where true").fetchall() == [
            (1,),
            (1,),
            (1,),
        ]
        assert conn.execute(
            "select count(*) from generate_series(1,5) i where i > 2"
        ).fetchone() == (3,)
        assert conn.execute(
            "select i from generate_series(1,5) i where i > 2 order by i desc"
        ).fetchall() == [(5,), (4,), (3,)]
        with pytest.raises(psycopg.errors.DatatypeMismatch) as ei:
            conn.execute("select 1 from generate_series(1,3) where 1")
        assert str(ei.value).startswith("argument of WHERE must be type boolean, not type integer")


def test_quote_ident_format_and_boolean_concat(home: Path) -> None:
    """`quote_ident` quotes reserved keywords (`order`, `select`) and leaves
    unreserved ones (`int4`, `abort`, `zone`) bare; `format()` handles
    `%s` / `%I` / `%L` / `%%` / `%2$s`, NULL arguments, and its three error
    shapes; a boolean's text inside `concat` is `t` / `f`."""
    with _Server(home) as server, server.connect() as conn:
        assert conn.execute(
            "select quote_ident('order'), quote_ident('select'), quote_ident('int4'),"
            " quote_ident('abort'), quote_ident('zone'), quote_ident('Foo'),"
            " quote_ident('a\"b'), quote_ident('a-b'), quote_ident('1a'), quote_ident('_ok')"
        ).fetchone() == (
            '"order"',
            '"select"',
            "int4",
            "abort",
            "zone",
            '"Foo"',
            '"a""b"',
            '"a-b"',
            '"1a"',
            "_ok",
        )
        assert conn.execute(
            "select format('%s|%I|%L|%%|%2$s', 'x', 'order', 'it''s'),"
            " format('%s-%L-%I', null, null, 'a'), format(null, 1), format('%L', E'a\\\\b')"
        ).fetchone() == ("x|\"order\"|'it''s'|%|order", "-NULL-a", None, "E'a\\\\b'")
        with pytest.raises(psycopg.errors.NullValueNotAllowed) as ei:
            conn.execute("select format('%I', null)")
        assert str(ei.value).startswith("null values cannot be formatted as an SQL identifier")
        with pytest.raises(psycopg.errors.InvalidParameterValue) as ei2:
            conn.execute("select format('%s %s', 1)")
        assert str(ei2.value).startswith("too few arguments for format()")
        with pytest.raises(psycopg.errors.InvalidParameterValue) as ei3:
            conn.execute("select format('%x', 1)")
        assert str(ei3.value).startswith('unrecognized format() type specifier "x"')
        assert ei3.value.diag.message_hint == 'For a single "%" use "%%".'
        assert conn.execute("select concat(true, false, 1, 1.5, 'x'), true::text").fetchone() == (
            "tf11.5x",
            "true",
        )


def test_regtype_quotes_a_reserved_keyword_type_name(home: Path) -> None:
    """A type named `order` renders as `"order"` through `regtype`."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute('create type "order" as (x int)')
        assert conn.execute("select '\"order\"'::regtype::text").fetchone() == ('"order"',)


def test_undefined_table_error_carries_its_position(home: Path) -> None:
    """`42P01` points `P` at the relation's first mention, counted in
    characters across lines (`select 1 +\\n 2 from nope` is 20)."""
    with _Server(home) as server, server.connect() as conn:
        with pytest.raises(psycopg.errors.UndefinedTable) as ei:
            conn.execute("select * from wat")
        assert ei.value.diag.statement_position == "15"
        assert ei.value.diag.severity_nonlocalized == "ERROR"
        with pytest.raises(psycopg.errors.UndefinedTable) as ei2:
            conn.execute("select 1 +\n 2 from nope")
        assert ei2.value.diag.statement_position == "20"


def _notices(conn: psycopg.Connection) -> list[tuple[str, str | None, str, str | None]]:
    seen: list[tuple[str, str | None, str, str | None]] = []
    conn.add_notice_handler(
        lambda d: seen.append(
            (d.severity or "", d.severity_nonlocalized, d.message_primary or "", d.context)
        )
    )
    return seen


def test_do_block_raise_levels_and_notices(home: Path) -> None:
    """`RAISE NOTICE / WARNING / INFO` reach the client as notices with the
    block's context; `DEBUG` and `LOG` do not (below `client_min_messages`).
    Both `DO $$ ... $$ LANGUAGE plpgsql` spellings work, as does `NULL;`."""
    with _Server(home) as server, server.connect() as conn:
        seen = _notices(conn)
        conn.execute(
            "do $$ begin raise notice 'n %', 1; raise warning 'w'; raise debug 'd';"
            " raise info 'i'; raise log 'l'; end $$"
        )
        ctx = "PL/pgSQL function inline_code_block line 1 at RAISE"
        assert seen == [
            ("NOTICE", "NOTICE", "n 1", ctx),
            ("WARNING", "WARNING", "w", ctx),
            ("INFO", "INFO", "i", ctx),
        ]
        seen.clear()
        conn.execute("do $$ begin raise notice 'x'; end $$ language plpgsql")
        conn.execute("do language plpgsql $$ begin raise notice 'y'; end $$")
        assert [n[2] for n in seen] == ["x", "y"]
        conn.execute("do $$ begin null; end $$")
        assert conn.execute("select 1").fetchone() == (1,)


def test_do_block_raise_exception_diagnostics(home: Path) -> None:
    """`RAISE EXCEPTION` carries the message, `USING` fields, the sqlstate
    (default `P0001`, a named condition, or an arbitrary `errcode`), and the
    block context; a non-ASCII message survives."""
    ctx = "PL/pgSQL function inline_code_block line 1 at RAISE"
    with _Server(home) as server, server.connect() as conn:
        with pytest.raises(psycopg.errors.DivisionByZero) as ei:
            conn.execute(
                "do $$ begin raise exception 'boom %', 'x'"
                " using errcode = '22012', detail = 'd', hint = 'h'; end $$"
            )
        d = ei.value.diag
        assert (d.message_primary, d.message_detail, d.message_hint, d.context) == (
            "boom x",
            "d",
            "h",
            ctx,
        )
        assert d.severity_nonlocalized == "ERROR"
        with pytest.raises(psycopg.errors.RaiseException) as ei2:
            conn.execute("do $$ begin raise exception 'boom'; end $$")
        assert (ei2.value.sqlstate, ei2.value.diag.message_primary) == ("P0001", "boom")
        with pytest.raises(psycopg.errors.DivisionByZero) as ei3:
            conn.execute("do $$ begin raise division_by_zero; end $$")
        assert ei3.value.diag.message_primary == "division_by_zero"
        with pytest.raises(psycopg.InternalError) as ei4:
            conn.execute("do $$ begin raise exception 'boom' using errcode = 'XX123'; end $$")
        assert ei4.value.sqlstate == "XX123"
        with pytest.raises(psycopg.errors.RaiseException) as ei5:
            conn.execute("do $$ begin raise exception 'bad é'; end $$")
        assert ei5.value.diag.message_primary == "bad é"
        conn.execute("set client_encoding to latin9")
        with pytest.raises(psycopg.errors.RaiseException) as ei6:
            conn.execute("do $$ begin raise exception 'bad €'; end $$")
        assert ei6.value.diag.message_primary == "bad €"
        assert conn.execute("select 'bad €'").fetchone() == ("bad €",)


def test_do_block_perform_and_execute_contexts(home: Path) -> None:
    """An error inside `PERFORM` stacks the SQL statement under the block
    frame; one inside `EXECUTE` carries the executed query and its position
    in the `q` / `p` fields instead."""
    with _Server(home) as server, server.connect() as conn:
        with pytest.raises(psycopg.errors.DivisionByZero) as ei:
            conn.execute("do $$ begin perform 1/0; end $$")
        assert ei.value.diag.message_primary == "division by zero"
        assert ei.value.diag.context == (
            'SQL statement "SELECT 1/0"\nPL/pgSQL function inline_code_block line 1 at PERFORM'
        )
        with pytest.raises(psycopg.errors.UndefinedTable) as ei2:
            conn.execute("do $$ begin execute 'select * from nope'; end $$")
        d = ei2.value.diag
        assert d.context == "PL/pgSQL function inline_code_block line 1 at EXECUTE"
        assert (d.internal_query, d.internal_position) == ("select * from nope", "15")
        assert d.statement_position is None


def test_do_block_compile_and_condition_errors(home: Path) -> None:
    """A bad body is `42601` positioned inside the statement; an unknown
    condition NAME fails at compile time with the compilation context, while
    an unknown `errcode` fails at the RAISE; a non-plpgsql language is
    `0A000`."""
    with _Server(home) as server, server.connect() as conn:
        with pytest.raises(psycopg.errors.SyntaxError) as ei:
            conn.execute("do $$ begin raise notice 'x' end $$")
        assert str(ei.value).startswith('syntax error at or near "end"')
        assert ei.value.diag.statement_position == "30"
        with pytest.raises(psycopg.errors.SyntaxError) as ei2:
            conn.execute("do $$ raise notice 'x'; $$")
        assert str(ei2.value).startswith('syntax error at or near "raise"')
        assert ei2.value.diag.statement_position == "7"
        with pytest.raises(psycopg.errors.UndefinedObject) as ei3:
            conn.execute("do $$ begin raise unknown_thing; end $$")
        assert str(ei3.value).startswith('unrecognized exception condition "unknown_thing"')
        assert ei3.value.diag.context == (
            'compilation of PL/pgSQL function "inline_code_block" near line 1'
        )
        with pytest.raises(psycopg.errors.UndefinedObject) as ei4:
            conn.execute("do $$ begin raise exception 'x' using errcode = 'unknown_thing'; end $$")
        assert ei4.value.diag.context == "PL/pgSQL function inline_code_block line 1 at RAISE"
        with pytest.raises(psycopg.errors.FeatureNotSupported) as ei5:
            conn.execute("do language sql $$ select 1 $$")
        assert str(ei5.value).startswith('language "sql" does not support inline code execution')


def test_copy_out_renders_inet_without_a_host_mask(home: Path) -> None:
    """`inet` drops a `/32` (`/128`) host mask on output and keeps any other;
    `cidr` always shows its mask; an IPv4-mapped address renders dotted."""
    import io

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table cpi (a inet, b cidr)")
        conn.execute(
            "insert into cpi values ('127.0.0.1/32', '10.0.0.0/8'),"
            " ('::ffff:102:300/128', '::ffff:1.2.3.0/120'), ('192.168.0.1/24', '192.168.0.0/24')"
        )
        buf = io.StringIO()
        with conn.cursor().copy("copy cpi to stdout") as cp:
            for chunk in cp:
                buf.write(bytes(chunk).decode())
        assert buf.getvalue() == (
            "127.0.0.1\t10.0.0.0/8\n"
            "::ffff:1.2.3.0\t::ffff:1.2.3.0/120\n"
            "192.168.0.1/24\t192.168.0.0/24\n"
        )
        assert conn.execute("select a::text, b::text from cpi").fetchall() == [
            ("127.0.0.1/32", "10.0.0.0/8"),
            ("::ffff:1.2.3.0/128", "::ffff:1.2.3.0/120"),
            ("192.168.0.1/24", "192.168.0.0/24"),
        ]


def test_copy_in_keeps_numeric_text_exact(home: Path) -> None:
    """`COPY FROM` stores a numeric at its written scale: a tiny fraction,
    a 34-digit value, `-0.0` (which reads back `0.0`) and `NaN`."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table cpn (n numeric)")
        with conn.cursor().copy("copy cpn from stdin") as cp:
            cp.write("0.000000000000000001\n123456789012345678901234567890.1234\n-0.0\nNaN\n")
        assert conn.execute("select n::text from cpn").fetchall() == [
            ("0.000000000000000001",),
            ("123456789012345678901234567890.1234",),
            ("0.0",),
            ("NaN",),
        ]


def test_enum_parameters_labels_and_arrays(home: Path) -> None:
    """A parameter typed with the enum's oid is checked against its labels
    (`22P02` with the portal context); an enum ARRAY parameter parses in text
    and in binary, and a binary result carries the enum array's oid with
    each element as its label."""
    import enum

    from psycopg.adapt import Dumper
    from psycopg.types.enum import EnumInfo, register_enum

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create type dnm_enum as enum ('ONE', 'TWO', 'THREE')")
        info = EnumInfo.fetch(conn, "dnm_enum")
        assert info is not None
        assert info.labels == ["ONE", "TWO", "THREE"]
        e = enum.Enum("E", {label: label for label in info.labels})
        register_enum(info, conn, e)
        assert conn.execute("select %s::text", [e.ONE]).fetchone() == ("ONE",)
        assert conn.execute("select %s::dnm_enum[]", [["ONE", "TWO"]]).fetchone() == (
            [e.ONE, e.TWO],
        )
        assert conn.execute("select %b::dnm_enum[]", [[e.ONE, e.TWO]]).fetchone() == (
            [e.ONE, e.TWO],
        )
        cur = conn.cursor(binary=True)
        cur.execute("select %s::dnm_enum[]", [[e.ONE, e.TWO]])
        assert cur.description is not None
        assert cur.description[0].type_code == info.array_oid
        assert cur.pgresult is not None
        raw = cur.pgresult.get_value(0, 0)
        assert raw is not None
        # ndim=1, no nulls, element oid, dim 2 lower-bound 1, then the labels.
        assert raw == (
            b"\x00\x00\x00\x01\x00\x00\x00\x00"
            + info.oid.to_bytes(4, "big")
            + b"\x00\x00\x00\x02\x00\x00\x00\x01"
            b"\x00\x00\x00\x03ONE\x00\x00\x00\x03TWO"
        )
        assert cur.fetchone() == ([e.ONE, e.TWO],)

        class WithEnumOid(Dumper):
            oid = info.oid

            def dump(self, obj: str) -> bytes:
                return obj.encode()

        conn.adapters.register_dumper(str, WithEnumOid)
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            conn.execute("select %s::text", ["NOPE"])
        assert str(ei.value).startswith('invalid input value for enum dnm_enum: "NOPE"')
        assert ei.value.diag.context == "unnamed portal parameter $1 = '...'"


def test_anonymous_record_binary_result_carries_field_types(home: Path) -> None:
    """`ROW(...)` in binary: a 4-byte field count, then per field the oid and
    length-prefixed bytes (`-1` for NULL). An untyped literal is `unknown`
    (705), a cast one its type -- byte-identical to PostgreSQL 16.15."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=True)
        expected = {
            "select row()": (b"\x00\x00\x00\x00", ((),)),
            "select row(null)": (b"\x00\x00\x00\x01\x00\x00\x02\xc1\xff\xff\xff\xff", ((None,),)),
            "select row(null, '')": (
                b"\x00\x00\x00\x02\x00\x00\x02\xc1\xff\xff\xff\xff\x00\x00\x02\xc1\x00\x00\x00\x00",
                ((None, b""),),
            ),
            "select row(42, 'foo', 'ba,r')": (
                b"\x00\x00\x00\x03\x00\x00\x00\x17\x00\x00\x00\x04\x00\x00\x00*"
                b"\x00\x00\x02\xc1\x00\x00\x00\x03foo\x00\x00\x02\xc1\x00\x00\x00\x04ba,r",
                ((42, b"foo", b"ba,r"),),
            ),
            "select row(10::int, null::text, 20::float, null::text, 'foo'::text, 'bar'::bytea)": (
                b"\x00\x00\x00\x06\x00\x00\x00\x17\x00\x00\x00\x04\x00\x00\x00\n"
                b"\x00\x00\x00\x19\xff\xff\xff\xff\x00\x00\x02\xbd\x00\x00\x00\x08@4\x00\x00\x00\x00\x00\x00"
                b"\x00\x00\x00\x19\xff\xff\xff\xff\x00\x00\x00\x19\x00\x00\x00\x03foo"
                b"\x00\x00\x00\x11\x00\x00\x00\x03bar",
                ((10, None, 20.0, None, "foo", b"bar"),),
            ),
        }
        for query, (raw, row) in expected.items():
            cur.execute(query)
            assert cur.description is not None
            assert cur.description[0].type_code == 2249, query
            assert cur.pgresult is not None
            assert cur.pgresult.get_value(0, 0) == raw, query
            assert cur.fetchone() == row, query
        assert conn.execute("select row(42, 'foo', 'ba,r')").fetchone() == (("42", "foo", "ba,r"),)


def test_binary_results_cover_every_faker_type(home: Path) -> None:
    """Every type psycopg's faker generates has a BINARY result form.

    psycopg reads EVERY column of a result in the format of column 0
    (`Transformer.set_pgresult` looks only at `PQfformat(res, 0)`, because
    PostgreSQL never mixes formats in one reply), so one text-described column
    in an otherwise binary row made the client run text loaders over binary
    bytes -- `decimal.InvalidOperation`, garbage dates, `UnicodeDecodeError` --
    which is what failed `test_leak` / `test_copy_to_leaks` / `test_random`.
    The bytes below are PostgreSQL 16's (`scratchpad/binpin.py`), so this is
    byte fidelity, not just "psycopg could decode it".
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("set timezone to 'UTC'")
        cases: list[tuple[str, str, object]] = [
            ("'2000-01-02'::date", "00000001", dt.date(2000, 1, 2)),
            ("'12:00:00+05:30'::timetz", "0000000a0eebb000ffffb2a8", None),
            (
                "'2000-01-01 00:00:01'::timestamp",
                "00000000000f4240",
                dt.datetime(2000, 1, 1, 0, 0, 1),
            ),
            (
                "'2000-01-01 00:00:01+00'::timestamptz",
                "00000000000f4240",
                dt.datetime(2000, 1, 1, 0, 0, 1, tzinfo=dt.timezone.utc),
            ),
            (
                "'1 day 2 hours'::interval",
                "00000001ad2748000000000100000000",
                dt.timedelta(days=1, hours=2),
            ),
            ("'-1 mon -3 days'::interval", "0000000000000000fffffffdffffffff", None),
            (
                "'[2000-01-01,2000-01-02)'::tsrange",
                "0200000008000000000000000000000008000000141dd76000",
                Range(dt.datetime(2000, 1, 1), dt.datetime(2000, 1, 2), "[)"),
            ),
            (
                "'[-infinity,infinity]'::tsrange",
                "06000000088000000000000000000000087fffffffffffffff",
                None,
            ),
            ("'empty'::int4range", "01", Range(empty=True)),
            ("'(,5]'::int8range", "08000000080000000000000006", Range(None, 6, "[)")),
            ("'{}'::int4[]", "000000000000000000000017", []),
            ("'{}'::text[]", "000000000000000000000019", []),
            (
                "'{[1,3),[5,7)}'::int4multirange",
                "00000002000000110200000004000000010000000400000003"
                "000000110200000004000000050000000400000007",
                Multirange([Range(1, 3, "[)"), Range(5, 7, "[)")]),
            ),
            ("'{}'::int4multirange", "00000000", Multirange([])),
            (
                "array['{\"a\":1}'::jsonb, null]",
                "000000010000000100000eda000000020000000100000009017b2261223a20317dffffffff",
                [{"a": 1}, None],
            ),
            (
                "array['{\"a\":1}'::json]",
                "0000000100000000000000720000000100000001000000077b2261223a317d",
                [{"a": 1}],
            ),
            (
                "array['2020-01-01'::date, '2000-01-01'::date]",
                "00000001000000000000043a00000002000000010000000400001c890000000400000000",
                [dt.date(2020, 1, 1), dt.date(2000, 1, 1)],
            ),
        ]
        for expr, want_hex, want in cases:
            cur = conn.cursor(binary=True)
            cur.execute(f"select {expr}")
            assert cur.pgresult.fformat(0) == 1, expr
            assert cur.pgresult.get_value(0, 0).hex() == want_hex, expr
            if want is not None:
                assert cur.fetchone()[0] == want, expr

        # The same types read back from a TABLE (the faker's real shape), all
        # columns binary in one row -- the shape psycopg cannot mix formats in.
        cur = conn.cursor()
        cur.execute(
            "create table fk (d date, t time, tz timetz, ts timestamp, tstz timestamptz, "
            "iv interval, r int4range, mr int4multirange, j json[], jb jsonb[], e int4[])"
        )
        cur.execute(
            "insert into fk values ('2020-02-03', '01:02:03.5', '01:02:03+02', "
            "'2020-02-03 04:05:06.789', '2020-02-03 04:05:06+00', '3 mons 2 days 1 hour', "
            "'[1,10)', '{[1,2),[5,9)}', array['[1, 2]'::json], array['{\"b\": true}'::jsonb], '{}')"
        )
        cur = conn.cursor(binary=True)
        cur.execute("select * from fk")
        assert all(cur.pgresult.fformat(i) == 1 for i in range(11))
        assert cur.fetchone() == (
            dt.date(2020, 2, 3),
            dt.time(1, 2, 3, 500000),
            dt.time(1, 2, 3, tzinfo=dt.timezone(dt.timedelta(hours=2))),
            dt.datetime(2020, 2, 3, 4, 5, 6, 789000),
            dt.datetime(2020, 2, 3, 4, 5, 6, tzinfo=dt.timezone.utc),
            dt.timedelta(days=92, hours=1),
            Range(1, 10, "[)"),
            Multirange([Range(1, 2, "[)"), Range(5, 9, "[)")]),
            [[1, 2]],
            [{"b": True}],
            [],
        )


def test_tstz_ranges_and_arrays_render_in_the_session_zone(home: Path) -> None:
    """A `tstzrange` / `tstzmultirange` / `timestamptz[]` column's TEXT form
    renders each bound in the session zone with its offset, as PostgreSQL
    16 does: `["2020-01-01 01:00:00+01","2020-06-01 12:00:00+02")` under
    Europe/Rome. The bounds are stored as naive UTC and used to go out that
    way, so a client under any zone but UTC read the wrong instants.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("set timezone to 'UTC'")
        conn.execute(
            "create table tzr (r tstzrange, m tstzmultirange, a timestamptz[]); "
            "insert into tzr values ('[2020-01-01 00:00+00,2020-06-01 10:00+00)', "
            "'{[2020-01-01 00:00+00,2020-01-02 00:00+00)}', "
            "array['2020-01-01 00:00+00'::timestamptz])"
        )
        rome = dt.datetime(2020, 1, 1, 1, tzinfo=dt.timezone(dt.timedelta(hours=1)))
        for zone, raw in (
            (
                "UTC",
                (
                    b'["2020-01-01 00:00:00+00","2020-06-01 10:00:00+00")',
                    b'{["2020-01-01 00:00:00+00","2020-01-02 00:00:00+00")}',
                    b'{"2020-01-01 00:00:00+00"}',
                ),
            ),
            (
                "Europe/Rome",
                (
                    b'["2020-01-01 01:00:00+01","2020-06-01 12:00:00+02")',
                    b'{["2020-01-01 01:00:00+01","2020-01-02 01:00:00+01")}',
                    b'{"2020-01-01 01:00:00+01"}',
                ),
            ),
        ):
            conn.execute(f"set timezone to '{zone}'")
            cur = conn.cursor()
            cur.execute("select r, m, a from tzr")
            assert tuple(cur.pgresult.get_value(0, i) for i in range(3)) == raw, zone
            row = cur.fetchone()
            assert row[0].lower == rome and row[2] == [rome], zone
            # And the binary form is the same UTC instant whatever the zone.
            cur = conn.cursor(binary=True)
            cur.execute("select r, m, a from tzr")
            assert cur.pgresult.get_value(0, 0).hex() == (
                "020000000800023e0786c260000000000800024a01a067c800"
            )
            assert cur.pgresult.get_value(0, 2).hex() == (
                "0000000100000000000004a000000001000000010000000800023e0786c26000"
            )


def test_set_config_reports_the_time_zone_like_set_does(home: Path) -> None:
    """`set_config('TimeZone', ...)` sends the same ParameterStatus as `SET
    TimeZone` (PostgreSQL 16). psycopg builds a timestamptz's tzinfo from that
    report, so without it a zone change through `set_config` was invisible to
    the client's loader and every timestamptz came back in the old zone."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("set timezone to 'UTC'")
        assert conn.info.parameter_status("TimeZone") == "UTC"
        conn.execute("select set_config('TimeZone', 'Europe/Rome', false)")
        assert conn.info.parameter_status("TimeZone") == "Europe/Rome"
        got = conn.execute("select '2020-07-01 12:00+00'::timestamptz").fetchone()[0]
        assert got.utcoffset() == dt.timedelta(hours=2)


def test_jsonb_unicode_escapes_match_postgres(home: Path) -> None:
    """PostgreSQL 16's `jsonb` input: a surrogate PAIR becomes the character,
    a lone surrogate is `22P02 invalid input syntax for type json` (no value
    suffix), and `\\u0000` is `22P05 unsupported Unicode escape sequence`.
    `json` keeps every escape verbatim and only refuses `\\u0000` through an
    operator. The faker emits such strings, and the old parser combined a
    pair into two U+FFFD replacement characters.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("""select '"\\ud83d\\ude00"'::jsonb::text, '"\\u00e9"'::jsonb::text""")
        assert cur.fetchone() == ('"\U0001f600"', '"\u00e9"')
        for lone in ('"\\ud83d"', '"\\ude00"', '"\\ud83dx"', '"\\ud83d\\ud83d"', '{"a":"\\ud83d"}'):
            with pytest.raises(psycopg.errors.InvalidTextRepresentation) as exc:
                cur.execute(f"select '{lone}'::jsonb")
            assert str(exc.value).startswith("invalid input syntax for type json"), lone
            assert exc.value.diag.sqlstate == "22P02"
        with pytest.raises(psycopg.errors.UntranslatableCharacter) as exc:
            cur.execute("""select '"\\u0000"'::jsonb""")
        assert exc.value.diag.sqlstate == "22P05"
        assert str(exc.value).startswith("unsupported Unicode escape sequence")
        # `json` is verbatim, escapes and all.
        cur.execute("""select '"\\ud83d"'::json::text, '"\\u0000"'::json::text""")
        assert cur.fetchone() == ('"\\ud83d"', '"\\u0000"')
        with pytest.raises(psycopg.errors.InvalidTextRepresentation):
            cur.execute("""select '"\\ud83d"'::json ->> 0""")
        with pytest.raises(psycopg.errors.UntranslatableCharacter):
            cur.execute("""select '"\\u0000"'::json ->> 0""")


def test_copy_inside_a_transaction_stays_in_it(home: Path) -> None:
    """A simple-protocol COPY inside a transaction leaves the connection
    INTRANS, and a COPY of NO rows too (PostgreSQL 16: status 2, rowcount 0).

    The vendored pgwire answered CopyDone with `ReadyForQuery(Idle)` whatever
    the transaction state; psycopg then believed the connection idle, its
    `rollback()` sent nothing, and the COPY'd rows survived the rollback.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        conn.execute("create table cp (id int, s text)")
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        with cur.copy("copy cp from stdin") as cp:
            pass
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.INTRANS
        assert cur.rowcount == 0
        with cur.copy("copy cp from stdin") as cp:
            cp.write("1\ta\n2\tb\n")
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.INTRANS
        assert cur.rowcount == 2
        assert conn.execute("select count(*) from cp").fetchone() == (2,)
        conn.rollback()
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.IDLE
        assert conn.execute("select count(*) from cp").fetchone() == (0,)
        conn.rollback()
        # A COPY that fails leaves the transaction in error, as any statement.
        with (
            pytest.raises(psycopg.errors.InvalidTextRepresentation),
            cur.copy("copy cp from stdin") as cp,
        ):
            cp.write("x\ta\n")
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.INERROR
        conn.rollback()


def test_copy_fills_the_columns_it_omits_from_their_defaults(home: Path) -> None:
    """A COPY with a column list fills the omitted columns like an INSERT
    does -- a `serial` from its sequence (PostgreSQL 16: `(1, None, 'hello'),
    (2, None, 'world')`), a literal default from its expression. The rows
    used to be stored with those columns NULL. And the fill runs INSIDE the
    open transaction: done outside, its `nextval` conflicted with the
    transaction's own earlier INSERT and spun on the write-conflict retry
    until the client gave up.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        conn.execute(
            "create table ci (id serial primary key, n int, data text, k text default 'dflt')"
        )
        with conn.cursor().copy("copy ci (n, data) from stdin") as cp:
            cp.write("\\N\thello\n\\N\tworld\n")
        assert conn.execute("select * from ci order by id").fetchall() == [
            (1, None, "hello", "dflt"),
            (2, None, "world", "dflt"),
        ]
        conn.execute("create table ct (id serial primary key, n int, data text)")
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        conn.execute("insert into ct (data) values ('a')")
        with conn.cursor().copy("copy ct (data) from stdin") as cp:
            cp.write("hello\n")
        conn.execute("insert into ct (data) values ('b')")
        assert conn.execute("select * from ct order by id").fetchall() == [
            (1, None, "a"),
            (2, None, "hello"),
            (3, None, "b"),
        ]
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.INTRANS
        conn.rollback()
        assert conn.execute("select count(*) from ct").fetchone() == (0,)


def test_copy_in_parses_arrays_and_binary_reads_stored_timestamps(home: Path) -> None:
    """A COPY'd `text[]` is stored as the array it is (it was one raw string,
    and read back as `{"{ab,cd}"}`, which psycopg reports as "malformed
    array: hit the end of the buffer"), a COPY'd date in its canonical text;
    and a stored `timestamp` read by a BINARY cursor is the i64 instant, not
    its text bytes (psycopg read `2020-01-01 00:00:00` as an integer:
    "timestamp too large").
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table ca (id int, a text[], d date, ts timestamp, tz timestamptz)")
        with conn.cursor().copy("copy ca from stdin") as cp:
            cp.write("1\t{ab,cd}\t2020-01-05\t2020-01-01 00:00:00.25\t2020-01-01 00:00:00+00\n")
        conn.execute("set timezone to 'UTC'")
        for binary in (False, True):
            cur = conn.cursor(binary=binary)
            cur.execute("select a, d, ts, tz from ca")
            assert cur.pgresult.fformat(2) == int(binary)
            assert cur.fetchone() == (
                ["ab", "cd"],
                dt.date(2020, 1, 5),
                dt.datetime(2020, 1, 1, 0, 0, 0, 250000),
                dt.datetime(2020, 1, 1, tzinfo=dt.timezone.utc),
            )
        with conn.cursor().copy("copy ca (a) to stdout") as cp:
            assert b"".join(cp) == b"{ab,cd}\n"


def test_an_empty_multirange_binary_parameter_is_typed_from_its_column(home: Path) -> None:
    """psycopg sends `Multirange([])` UNTYPED (no element to name the type
    from) and, in binary, as four zero bytes. Read as text those were
    `malformed multirange literal: "\\0\\0\\0\\0"` -- and that message carried
    the NULs onto the wire, where the client found bytes after the message's
    fields and dropped the connection. PostgreSQL resolves the parameter's
    type from the column it goes into, and so does this server now.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table mr (id int, m int4multirange)")
        cur = conn.cursor()
        cur.execute("insert into mr values (%s, %b)", (1, Multirange([])))
        cur.execute("insert into mr values (%s, %b)", (2, Multirange([Range(1, 3, "[)")])))
        assert conn.execute("select m from mr order by id").fetchall() == [
            (Multirange([]),),
            (Multirange([Range(1, 3, "[)")]),),
        ]
        # The connection survived every step.
        assert conn.execute("select 1").fetchone() == (1,)


def test_datetime_array_text_is_not_a_debug_dump(home: Path) -> None:
    """`timestamp[]::text` / `interval[]::text` render each element as its
    scalar text (PostgreSQL 16: `{"2020-01-01 00:00:00.5"}`, `{"1 day"}`).
    They rendered the BSON value's Rust Debug form, `{"DateTime(2020-01-01
    0:00:00.5 +00:00:00)"}`, which no client can read.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("set timezone to 'UTC'")
        cur = conn.cursor()
        cur.execute(
            "select array['2020-01-01 00:00:00.5'::timestamp]::text, "
            "array['1 day'::interval]::text, array['12:00'::time]::text"
        )
        assert cur.fetchone() == ('{"2020-01-01 00:00:00.5"}', '{"1 day"}', "{12:00:00}")


_WIDE = Decimal("1.2345678901234567890123456789012345")  # 35 significant digits
_HUGE = Decimal("1e40")


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_numeric_wider_than_decimal128_round_trips(home: Path, binary: bool) -> None:
    """A `numeric` with more than 34 significant digits, or beyond
    Decimal128's exponent range, round-trips EXACTLY with its display scale
    -- in the text and the binary formats -- where it used to be refused
    (22003). Every expectation is PostgreSQL 16's own rendering.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        conn.execute("create table wn (id int primary key, n numeric)")
        rows = [
            (1, _WIDE),
            (2, _HUGE),
            (3, Decimal("-99999999999999999999999999999999999.5")),
            (4, Decimal("123456789012345678901234567890.123456789012345678901234567890")),
            (5, Decimal("1.000000000000000000000000000000000000000000")),
            (6, Decimal("1E-7000")),
            (7, Decimal("1.50")),
        ]
        cur.executemany("insert into wn values (%s, %s)", rows)
        cur.execute("select id, n from wn order by id")
        assert cur.fetchall() == rows
        cur.execute("select n::text from wn where id = 5")
        assert cur.fetchone() == ("1.000000000000000000000000000000000000000000",)
        cur.execute("select n * 2, n + 0.5, -n, abs(n), round(n, 3) from wn where id = 1")
        assert cur.fetchone() == (
            Decimal("2.4691357802469135780246913578024690"),
            Decimal("1.7345678901234567890123456789012345"),
            Decimal("-1.2345678901234567890123456789012345"),
            _WIDE,
            Decimal("1.235"),
        )
        # PostgreSQL's division scale rule, on a wide dividend.
        cur.execute("select n / 3 from wn where id = 1")
        assert cur.fetchone() == (Decimal("0.4115226300411522630041152263004115"),)
        # A computed numeric column is DESCRIBED as numeric (oid 1700), even
        # when the value is small: `n * 2` was typed int4 and the client's
        # int loader choked on `3.0`.
        assert cur.description[0].type_code == 1700
        # Beyond PostgreSQL's own limits is still refused, never rounded.
        with pytest.raises(psycopg.errors.NumericValueOutOfRange):
            cur.execute("select 1e131072::numeric")


def test_numeric_wider_than_decimal128_compares_and_sorts_exactly(home: Path) -> None:
    """Mixed-width rows compare by VALUE in every WHERE operator and in ORDER
    BY (`1.50` ties `1.5`; the tie breaks on `id`), through a plain column and
    through a numeric PRIMARY KEY -- the `_id` index -- alike. NaN takes
    PostgreSQL's place, above infinity. Expectations were produced by
    PostgreSQL 16 from the same script.
    """
    vals = [
        "-1e40", "-99999999999999999999999999999999999.5", "-100", "-0.5", "0", "0.00",
        "0.5", "1.50", "1.5", "99999999999999999999999999999999999",
        "100000000000000000000000000000000000", "1.2345678901234567890123456789012345",
        "1.234567890123456789012345678901234", "1e40", "NaN", "Infinity", "-Infinity", "42",
    ]  # fmt: skip
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table wn (id int primary key, n numeric)")
        cur = conn.cursor()
        for i, v in enumerate(vals):
            cur.execute("insert into wn values (%s, %s)", (i, Decimal(v)))
        cur.execute("insert into wn values (99, null)")

        def ids(sql: str, *args: object) -> list[int]:
            cur.execute(sql, args)
            return [r[0] for r in cur.fetchall()]

        assert ids("select id from wn order by n, id") == [
            16, 0, 1, 2, 3, 4, 5, 6, 12, 11, 7, 8, 17, 9, 10, 13, 15, 14, 99,
        ]  # fmt: skip
        assert ids("select id from wn order by n desc, id") == [
            99, 14, 15, 13, 10, 9, 17, 7, 8, 11, 12, 6, 4, 5, 3, 2, 1, 0, 16,
        ]  # fmt: skip
        assert ids("select id from wn where n = %s order by id", _WIDE) == [11]
        assert ids("select id from wn where n = %s order by id", _HUGE) == [13]
        assert ids("select id from wn where n = %s order by id", Decimal("1.50")) == [7, 8]
        assert ids("select id from wn where n > %s order by id", _WIDE) == [
            7, 8, 9, 10, 13, 14, 15, 17,
        ]  # fmt: skip
        assert ids("select id from wn where n < %s order by id", _WIDE) == [
            0, 1, 2, 3, 4, 5, 6, 12, 16,
        ]  # fmt: skip
        assert ids("select id from wn where n >= %s order by id", _HUGE) == [13, 14, 15]
        assert ids("select id from wn where n <> %s order by id", _HUGE) == [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 15, 16, 17,
        ]  # fmt: skip
        assert ids(
            "select id from wn where n in (%s, %s, %s) order by id",
            Decimal("1.5"), _HUGE, _WIDE,
        ) == [7, 8, 11, 13]  # fmt: skip
        assert ids(
            "select id from wn where n between %s and %s order by id", Decimal("0"), _WIDE
        ) == [4, 5, 6, 11, 12]
        # NaN: equal to itself, above infinity; `> NaN` is no row.
        nan = Decimal("NaN")
        assert ids("select id from wn where n = %s", nan) == [14]
        assert ids("select id from wn where n > %s", nan) == []
        assert ids("select id from wn where n > %s order by id", Decimal("Infinity")) == [14]
        assert ids("select id from wn where n < %s order by id", nan) == [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15, 16, 17,
        ]  # fmt: skip
        # `sum(numeric)` is exact and keeps the widest input scale.
        cur.execute("select sum(n), min(n), max(n) from wn where n < 1e100 and n > -1e100")
        assert cur.fetchone() == (
            Decimal("99999999999999999999999999999999946.9691357802469135780246913578024685"),
            Decimal("-1e40"),
            Decimal("1e40"),
        )
        assert cur.description[0].type_code == 1700

        # The same through a numeric PRIMARY KEY, where a wide `_id` is a
        # document the index keys by its text: equality, range, update and
        # delete resolve by value, and a duplicate that differs only in its
        # display scale is still a 23505.
        conn.execute("create table wpk (n numeric primary key, tag text)")
        for i, v in enumerate(["-1e40", "1.5", "1e40", str(_WIDE), "NaN"]):
            cur.execute("insert into wpk values (%s, %s)", (Decimal(v), f"t{i}"))
        for dup in ("1.50", "1e40", "10000000000000000000000000000000000000000.0", "NaN"):
            with pytest.raises(psycopg.errors.UniqueViolation):
                cur.execute("insert into wpk values (%s, 'dup')", (Decimal(dup),))
        cur.execute("select tag from wpk order by n")
        assert [r[0] for r in cur.fetchall()] == ["t0", "t3", "t1", "t2", "t4"]
        cur.execute("select tag from wpk where n = %s", (_HUGE,))
        assert cur.fetchall() == [("t2",)]
        cur.execute("select tag from wpk where n > %s order by tag", (Decimal("1.5"),))
        assert cur.fetchall() == [("t2",), ("t4",)]
        cur.execute("select tag from wpk where n < %s order by tag", (Decimal("1.5"),))
        assert cur.fetchall() == [("t0",), ("t3",)]
        cur.execute("update wpk set tag = 'upd' where n = %s", (_HUGE,))
        assert cur.rowcount == 1
        cur.execute("delete from wpk where n = %s", (_WIDE,))
        assert cur.rowcount == 1
        cur.execute("select count(*) from wpk")
        assert cur.fetchone() == (4,)


def _diag(exc: psycopg.Error) -> tuple:
    d = exc.diag
    return (
        d.sqlstate,
        d.message_primary,
        d.message_detail,
        d.schema_name,
        d.table_name,
        d.column_name,
        d.constraint_name,
    )


def test_not_null_and_check_constraints_report_what_postgres_reports(home: Path) -> None:
    """NOT NULL (23502) and CHECK (23514) are enforced on INSERT and UPDATE
    with PostgreSQL 16's message, `Failing row contains (...)` detail
    (every column in declaration order, NULL as `null`), and diagnostic
    fields. Unnamed CHECKs take PG's names: `<table>_<col>_check` when the
    expression names one column, `<table>_check` (then `_check1`, ...) otherwise.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table nn (a int not null, b text)")
        with pytest.raises(psycopg.errors.NotNullViolation) as exc:
            conn.execute("insert into nn (b) values ('x')")
        assert _diag(exc.value) == (
            "23502",
            'null value in column "a" of relation "nn" violates not-null constraint',
            "Failing row contains (null, x).",
            "public",
            "nn",
            "a",
            None,
        )
        conn.execute("insert into nn values (1, 'z')")
        with pytest.raises(psycopg.errors.NotNullViolation):
            conn.execute("update nn set a = null")
        assert conn.execute("select a from nn").fetchall() == [(1,)]

        conn.execute("create table ck (a int check (a > 0), b int, c int check (a < b))")
        with pytest.raises(psycopg.errors.CheckViolation) as exc:
            conn.execute("insert into ck values (-1, 2, 3)")
        assert _diag(exc.value) == (
            "23514",
            'new row for relation "ck" violates check constraint "ck_a_check"',
            "Failing row contains (-1, 2, 3).",
            "public",
            "ck",
            None,
            "ck_a_check",
        )
        with pytest.raises(psycopg.errors.CheckViolation) as exc:
            conn.execute("insert into ck values (5, 2, 3)")
        assert exc.value.diag.constraint_name == "ck_check"
        # A CHECK that evaluates to NULL passes (SQL's rule), and a violation
        # on a later row inserts none of them.
        conn.execute("insert into ck values (null, 2, 3)")
        with pytest.raises(psycopg.errors.CheckViolation):
            conn.execute("insert into ck values (1, 2, 3), (0, 1, 1)")
        assert conn.execute("select count(*) from ck").fetchone() == (1,)
        with pytest.raises(psycopg.errors.CheckViolation):
            conn.execute("update ck set a = 9")

        # A TEMP table's schema is the session's pg_temp namespace.
        conn.execute("create temp table tt (data int constraint chk_eq1 check (data = 1))")
        with pytest.raises(psycopg.errors.CheckViolation) as exc:
            conn.execute("insert into tt values (2)")
        assert exc.value.diag.schema_name.startswith("pg_temp")
        assert exc.value.diag.constraint_name == "chk_eq1"
        assert exc.value.diag.severity_nonlocalized == "ERROR"


def test_foreign_keys_are_enforced_on_both_sides(home: Path) -> None:
    """FOREIGN KEY (23503): the child side on INSERT / UPDATE, the parent side
    on DELETE with NO ACTION, CASCADE and SET NULL, NULL keys pass, and a
    self-reference sees the rows of its own statement. Messages and fields
    probed against PostgreSQL 16.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table p (id int primary key)")
        conn.execute("create table c (id serial primary key, p int references p)")
        with pytest.raises(psycopg.errors.ForeignKeyViolation) as exc:
            conn.execute("insert into c (p) values (7)")
        assert _diag(exc.value) == (
            "23503",
            'insert or update on table "c" violates foreign key constraint "c_p_fkey"',
            'Key (p)=(7) is not present in table "p".',
            "public",
            "c",
            None,
            "c_p_fkey",
        )
        conn.execute("insert into c (p) values (null)")
        conn.execute("insert into p values (1)")
        conn.execute("insert into c (p) values (1)")
        with pytest.raises(psycopg.errors.ForeignKeyViolation) as exc:
            conn.execute("delete from p where id = 1")
        assert _diag(exc.value) == (
            "23503",
            'update or delete on table "p" violates foreign key constraint "c_p_fkey" on table "c"',
            'Key (id)=(1) is still referenced from table "c".',
            "public",
            "c",
            None,
            "c_p_fkey",
        )
        assert conn.execute("select count(*) from p").fetchone() == (1,)
        with pytest.raises(psycopg.errors.ForeignKeyViolation):
            conn.execute("update c set p = 99 where p = 1")
        with pytest.raises(psycopg.errors.UndefinedTable):
            conn.execute("create table bad (p int references nosuch)")

        conn.execute("create table p2 (id int primary key)")
        conn.execute(
            "create table c2 (id serial primary key, p int references p2 on delete cascade)"
        )
        conn.execute("insert into p2 values (1), (2)")
        conn.execute("insert into c2 (p) values (1), (1), (2)")
        conn.execute("delete from p2 where id = 1")
        assert conn.execute("select p from c2").fetchall() == [(2,)]

        conn.execute("create table p3 (id int primary key)")
        conn.execute(
            "create table c3 (id serial primary key, p int references p3 on delete set null)"
        )
        conn.execute("insert into p3 values (1)")
        conn.execute("insert into c3 (p) values (1)")
        conn.execute("delete from p3 where id = 1")
        assert conn.execute("select p from c3").fetchall() == [(None,)]

        conn.execute("create table s (id int primary key, r int references s)")
        conn.execute("insert into s values (1, 2), (2, 1)")
        assert conn.execute("select count(*) from s").fetchone() == (2,)


def test_deferred_foreign_key_fails_at_commit_and_leaves_the_connection_idle(
    home: Path,
) -> None:
    """`DEFERRABLE INITIALLY DEFERRED` is checked at COMMIT inside a
    transaction (and at the statement in autocommit). The COMMIT answers the
    23503, the transaction is rolled back, and the connection is IDLE -- not
    "in a failed transaction" -- so the next statement runs. PostgreSQL 16.
    """
    from psycopg.pq import TransactionStatus

    with _Server(home) as server, server.connect() as conn:
        conn.execute(
            "create table selfref (x serial primary key, "
            "y int references selfref (x) deferrable initially deferred)"
        )
        with pytest.raises(psycopg.errors.ForeignKeyViolation):
            conn.execute("insert into selfref (y) values (-1)")
        assert conn.execute("select count(*) from selfref").fetchone() == (0,)
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        conn.execute("insert into selfref (y) values (-1)")
        assert conn.info.transaction_status == TransactionStatus.INTRANS
        with pytest.raises(psycopg.errors.ForeignKeyViolation) as exc:
            conn.commit()
        assert exc.value.diag.constraint_name == "selfref_y_fkey"
        assert conn.info.transaction_status == TransactionStatus.IDLE
        assert conn.execute("select count(*) from selfref").fetchone() == (0,)
        conn.rollback()
        # The catalog carries the constraints for the Python server too.
    assert _python_sql(home, "select count(*) from selfref") == [(0,)]


def test_a_pipeline_error_rolls_back_the_statements_before_it(home: Path) -> None:
    """Every extended-protocol statement between two Syncs runs in one
    transaction that the Sync commits, so an error in the pipeline rolls
    back the earlier statements of its group and libpq skips the later ones
    (`PIPELINE_ABORTED`); after the Sync the connection is IDLE and the next
    group commits on its own. A `BEGIN` inside a group makes it a block, and
    `DECLARE` in a group is still refused (`25P01`). PostgreSQL 16.
    """
    from psycopg.pq import TransactionStatus

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table pipe (n int primary key)")
        with pytest.raises(psycopg.errors.DivisionByZero), conn.pipeline():
            conn.execute("insert into pipe values (1)")
            conn.execute("select 1/0")
            conn.execute("insert into pipe values (2)")
        assert conn.info.transaction_status == TransactionStatus.IDLE
        conn.execute("insert into pipe values (3)")
        assert conn.execute("select n from pipe order by n").fetchall() == [(3,)]
        # The group is NOT a transaction block.
        with pytest.raises(psycopg.errors.NoActiveSqlTransaction), conn.pipeline():
            conn.execute("declare c cursor for select 1")
        # Unless a BEGIN inside it makes it one: the status after the Sync
        # is INTRANS, and the rows wait for the COMMIT.
        with conn.pipeline():
            conn.execute("begin")
            conn.execute("insert into pipe values (4)")
        assert conn.info.transaction_status == TransactionStatus.INTRANS
        conn.execute("commit")
        assert conn.execute("select count(*) from pipe").fetchone() == (2,)
        # A CREATE TABLE and an INSERT into it in one group see each other.
        with conn.pipeline():
            conn.execute("create table pipe2 (n int)")
            conn.execute("insert into pipe2 values (5)")
        assert conn.execute("select n from pipe2").fetchall() == [(5,)]


def test_an_insert_prepared_without_parameter_types_takes_the_column_types(
    home: Path,
) -> None:
    """libpq's `PQprepare` with `nParams = 0` leaves every `$n` for the server
    to type from the column it lands in. The parameters live in the VALUES
    list, which pg_query's node walk skips -- so the statement used to be
    sized at zero parameters and fail with `there is no parameter $1`.
    """
    from psycopg import pq

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table typed (n int, t text, when_ timestamp)")
        pgconn = conn.pgconn
        pgconn.send_prepare(b"ins", b"insert into typed values ($1, $2, $3)")
        assert pgconn.get_result().status == pq.ExecStatus.COMMAND_OK
        pgconn.get_result()
        pgconn.send_query_prepared(b"ins", [b"7", b"seven", b"2024-01-02 03:04:05"])
        res = pgconn.get_result()
        assert res.status == pq.ExecStatus.COMMAND_OK, res.error_message
        pgconn.get_result()
        assert conn.execute("select * from typed").fetchall() == [
            (7, "seven", dt.datetime(2024, 1, 2, 3, 4, 5))
        ]


def test_a_cancel_request_interrupts_the_running_statement(home: Path) -> None:
    """`conn.cancel_safe()` opens a second connection carrying the backend's
    pid and secret key; the server matches it against the backend it
    handed out at startup and interrupts the statement with `57014`. The
    connection then goes back to IDLE and keeps working. While the sleep
    runs, `pg_stat_activity` shows it as the backend's active query -- and
    the cancel connection is served DURING the sleep, which needs the
    synchronous statement to run off the async runtime's I/O thread.
    PostgreSQL 16.
    """
    import threading

    from psycopg.pq import TransactionStatus

    with _Server(home) as server, server.connect() as conn, server.connect() as other:
        pid = conn.info.backend_pid
        seen: list[tuple] = []

        def cancel_after_activity() -> None:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                rows = other.execute(
                    "select state, query, backend_type from pg_stat_activity where pid = %s",
                    (pid,),
                ).fetchall()
                if rows and rows[0][0] == "active":
                    seen.extend(rows)
                    break
                time.sleep(0.02)
            conn.cancel_safe()

        t = threading.Thread(target=cancel_after_activity)
        t.start()
        with pytest.raises(psycopg.errors.QueryCanceled) as info:
            conn.execute("select pg_sleep(30)")
        t.join()
        assert _diag(info.value) == (
            "57014",
            "canceling statement due to user request",
            None,
            None,
            None,
            None,
            None,
        )
        assert info.value.diag.severity == "ERROR"
        assert seen == [("active", "select pg_sleep(30)", "client backend")]
        assert conn.info.transaction_status == TransactionStatus.IDLE
        assert conn.execute("select 1").fetchone() == (1,)
        assert other.execute(
            "select state from pg_stat_activity where pid = %s", (pid,)
        ).fetchone() == ("idle",)


def test_idle_timeouts_end_the_session_with_a_fatal_error(home: Path) -> None:
    """`idle_in_transaction_session_timeout` fires while a block is open
    (aborted or not) and `idle_session_timeout` while none is; each sends a
    FATAL error and closes the connection, which the client sees on its next
    round trip. Neither fires in the other state, `0` disables them, and
    the values are validated and rendered like PostgreSQL's (`60000` shows
    as `1min`). PostgreSQL 16.
    """
    with _Server(home) as server:
        with server.connect(autocommit=False) as conn:
            conn.execute("set session idle_in_transaction_session_timeout = 150")
            assert conn.execute("show idle_in_transaction_session_timeout").fetchone() == ("150ms",)
            time.sleep(0.5)
            with pytest.raises(psycopg.errors.IdleInTransactionSessionTimeout) as info:
                conn.execute("select 1")
            assert info.value.diag.severity == "FATAL"
            assert _diag(info.value)[:2] == (
                "25P03",
                "terminating connection due to idle-in-transaction timeout",
            )
            assert conn.closed and conn.broken
        with server.connect(autocommit=False) as conn:
            conn.execute("set idle_in_transaction_session_timeout = '150ms'")
            conn.commit()
            with pytest.raises(psycopg.errors.DivisionByZero):
                conn.execute("select 1/0")
            time.sleep(0.5)
            with pytest.raises(psycopg.errors.IdleInTransactionSessionTimeout):
                conn.execute("select 1")
        with server.connect() as conn:
            conn.execute("set idle_in_transaction_session_timeout = 100")
            time.sleep(0.3)
            assert conn.execute("select 1").fetchone() == (1,)
            conn.execute("set idle_session_timeout = 60000")
            assert conn.execute("show idle_session_timeout").fetchone() == ("1min",)
            for value, message in [
                ("'abc'", 'invalid value for parameter "idle_session_timeout": "abc"'),
                (
                    "-1",
                    '-1 ms is outside the valid range for parameter "idle_session_timeout" '
                    "(0 .. 2147483647)",
                ),
            ]:
                with pytest.raises(psycopg.errors.InvalidParameterValue) as info:
                    conn.execute(f"set idle_session_timeout = {value}")
                assert info.value.diag.message_primary == message
            conn.execute("set idle_session_timeout = 200")
            time.sleep(0.5)
            with pytest.raises(psycopg.errors.IdleSessionTimeout) as info:
                conn.execute("select 1")
            assert info.value.diag.severity == "FATAL"
            assert _diag(info.value)[:2] == (
                "57P05",
                "terminating connection due to idle-session timeout",
            )
            assert conn.closed


def test_connecting_to_an_unknown_database_fails_at_startup(home: Path) -> None:
    """`dbname=nosuchdb` is FATAL 3D000 BEFORE AuthenticationOk, as PostgreSQL's.

    libpq reports it as a failed connect (`OperationalError` with the FATAL
    line in the message and no diag), which is what psycopg's
    `test_connect_bad` / `test_pgconn_error` assert. `template0` is a database
    but never accepts connections (55000); `template1` and the daemon's
    `--database` names do, and `pg_database` lists exactly that set.
    """
    with _Server(home, databases=("gauge_db",)) as server:
        with pytest.raises(psycopg.OperationalError) as info:
            server.connect(dbname="nosuchdb")
        assert 'FATAL:  database "nosuchdb" does not exist' in str(info.value)
        with pytest.raises(psycopg.OperationalError) as info:
            server.connect(dbname="template0")
        assert 'FATAL:  database "template0" is not currently accepting connections' in str(
            info.value
        )
        for name in ("postgres", "template1", "gauge_db"):
            with server.connect(dbname=name) as conn:
                assert conn.execute("select current_database(), current_catalog").fetchone() == (
                    name,
                    name,
                )
                (pid,) = conn.execute("select pg_backend_pid()").fetchone()
                assert conn.execute(
                    "select datname from pg_stat_activity where pid = %s", (pid,)
                ).fetchone() == (name,)
        with server.connect() as conn:
            rows = conn.execute(
                "select oid, datname, datistemplate, datallowconn, datdba, encoding"
                " from pg_database order by oid"
            ).fetchall()
            assert rows == [
                (1, "template1", True, True, 10, 6),
                (4, "template0", True, False, 10, 6),
                (5, "postgres", False, True, 10, 6),
                (16385, "gauge_db", False, True, 10, 6),
            ]


def test_create_and_drop_database(home: Path) -> None:
    """CREATE / DROP DATABASE with PostgreSQL's errors, and a dropped database's
    data is gone when the name is created again (probed PG 16)."""
    with _Server(home) as server:
        with server.connect() as conn:
            with pytest.raises(psycopg.errors.InvalidCatalogName) as info:
                conn.execute("drop database probe_x")
            assert _diag(info.value)[:2] == ("3D000", 'database "probe_x" does not exist')
            notices: list[str] = []
            conn.add_notice_handler(lambda d: notices.append(d.message_primary))
            conn.execute("drop database if exists probe_x")
            assert notices == ['database "probe_x" does not exist, skipping']

            conn.execute("begin")
            with pytest.raises(psycopg.errors.ActiveSqlTransaction) as info:
                conn.execute("create database probe_x")
            assert _diag(info.value)[:2] == (
                "25001",
                "CREATE DATABASE cannot run inside a transaction block",
            )
            conn.execute("rollback")

            conn.execute("create database probe_x")
            with pytest.raises(psycopg.errors.DuplicateDatabase) as info:
                conn.execute("create database probe_x")
            assert _diag(info.value)[:2] == ("42P04", 'database "probe_x" already exists')
            with pytest.raises(psycopg.errors.ObjectInUse) as info:
                conn.execute("drop database postgres")
            assert _diag(info.value)[:2] == ("55006", "cannot drop the currently open database")
            with pytest.raises(psycopg.errors.WrongObjectType) as info:
                conn.execute("drop database template1")
            assert _diag(info.value)[:2] == ("42809", "cannot drop a template database")
            assert conn.execute(
                "select oid >= 16384 from pg_database where datname = 'probe_x'"
            ).fetchone() == (True,)

        with server.connect(dbname="probe_x") as conn:
            conn.execute("create table t (a int)")
            conn.execute("insert into t values (1)")
            assert conn.execute("select a from t").fetchall() == [(1,)]

        with server.connect() as conn:
            conn.execute("drop database probe_x")
            assert conn.execute(
                "select count(*) from pg_database where datname = 'probe_x'"
            ).fetchone() == (0,)
            conn.execute("create database probe_x")
        with (
            server.connect(dbname="probe_x") as conn,
            pytest.raises(psycopg.errors.UndefinedTable),
        ):
            conn.execute("select a from t")

    # The registry is persisted: the database survives a restart.
    with _Server(home) as server, server.connect(dbname="probe_x") as conn:
        assert conn.execute("select current_database()").fetchone() == ("probe_x",)


def test_the_first_ddl_on_a_fresh_store_does_not_block_a_second_connection(home: Path) -> None:
    """Two connections may both create objects on a store nobody has written to.

    The catalog collections (`__sql_catalog__`, `__sql_schemas__`,
    `__sql_enum_meta__`) were created lazily, on the transaction session of
    whichever statement first needed them. Inside an open transaction that
    row stayed uncommitted, and a second connection's identical lazy create
    then hit WiredTiger's WriteConflict -- so `test_copy_table_across`, which
    creates a table on a fresh store from one connection and then a second
    table from another, failed with an internal error that PostgreSQL never
    raises. The collections are now created OUTSIDE the user transaction
    (measured 2026-09-09).
    """
    with _Server(home) as server:
        with (
            server.connect(autocommit=False) as first,
            server.connect(autocommit=False) as second,
        ):
            first.execute("create table t1 (a int)")
            first.execute("insert into t1 values (1)")
            second.execute("create table t2 (b text)")
            second.execute("insert into t2 values ('x')")
            first.commit()
            second.commit()
        with server.connect() as conn:
            assert conn.execute("select a from t1").fetchall() == [(1,)]
            assert conn.execute("select b from t2").fetchall() == [("x",)]

        # The same shape for the schema and type catalogs, which are separate
        # collections and so still unwritten at this point.
        with (
            server.connect(autocommit=False) as first,
            server.connect(autocommit=False) as second,
        ):
            first.execute("create schema s1")
            second.execute("create schema s2")
            # The type oid counter is minted OUTSIDE the block like
            # PostgreSQL's OID counter: two open blocks advance it
            # independently, and one that rolls back just skips an oid.
            first.execute("create type e1 as enum ('a')")
            second.execute("create type e2 as enum ('b')")
            first.execute("create type c1 as (x int)")
            second.execute("create type c2 as (y int)")
            first.commit()
            second.rollback()
        with server.connect() as conn:
            conn.execute("create type e3 as enum ('c')")
            assert conn.execute("select 'a'::e1, 'c'::e3").fetchone() == ("a", "c")
            oids = conn.execute(
                "select typname, oid from pg_type where typname in ('e1', 'e2', 'e3', 'c1', 'c2')"
                " order by typname"
            ).fetchall()
            assert [n for n, _ in oids] == ["c1", "e1", "e3"]
            assert len({o for _, o in oids}) == 3


def test_group_by_an_expression_or_a_position(home: Path) -> None:
    """``GROUP BY length(data)`` / ``GROUP BY 1, 2, 3`` key on the expression.

    Every answer here was measured on PostgreSQL 16 (2026-09-09), including the
    output column names (``length``, ``?column?``), the ``int4`` / ``bool``
    types, and the 42P10 for a position past the select list. This is the
    shape psycopg's ``test_copy_in_allchars`` checks its 256 rows with.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        conn.execute("create table gp (id int primary key, data text, col2 text)")
        conn.execute(
            "insert into gp values (1, 'a', null), (2, 'bb', null), (3, 'cc', null),"
            " (4, null, null), (5, 'a', null)"
        )
        cur = conn.execute("select length(data), count(*) from gp group by length(data) order by 1")
        assert [(d.name, d.type_code) for d in cur.description] == [("length", 23), ("count", 20)]
        assert cur.fetchall() == [(1, 2), (2, 2), (None, 1)]
        cur = conn.execute(
            "select col2 is null, ascii(data), count(*) from gp group by 1, 2 order by 2"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("?column?", 16),
            ("ascii", 23),
            ("count", 20),
        ]
        assert cur.fetchall() == [(True, 97, 2), (True, 98, 1), (True, 99, 1), (True, None, 1)]
        assert conn.execute(
            "select length(data) as len, count(*) from gp group by len order by len desc"
        ).fetchall() == [(None, 1), (2, 2), (1, 2)]
        assert conn.execute(
            "select length(data) + 1, count(*) from gp group by length(data) + 1 order by 1"
        ).fetchall() == [(2, 2), (3, 2), (None, 1)]
        # psycopg's allchars check, verbatim.
        assert conn.execute(
            "select 97 = ascii(data), col2 is null, length(data), count(*) from gp"
            " where data = 'a' group by 1, 2, 3"
        ).fetchall() == [(True, True, 1, 2)]
        with pytest.raises(psycopg.errors.InvalidColumnReference) as ex:
            conn.execute("select length(data), count(*) from gp group by 3")
        assert _diag(ex.value)[:2] == ("42P10", "GROUP BY position 3 is not in select list")


def test_is_null_over_a_constant_and_a_from_less_unnest(home: Path) -> None:
    """``x IS [NOT] NULL`` as a value, and ``select unnest(array)`` as rows.

    Measured on PostgreSQL 16: a row is null only when EVERY field is, and not
    null only when NONE is, so ``row(1, null)`` answers false to both. The
    unnest literal carries the characters psycopg's ``test_copy_out_allchars``
    sends -- a quote, a backslash, a comma, both braces -- through the array
    literal parser.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.execute(
            "select row(null, null) is null, row(null, null) is not null,"
            " row(1, null) is null, row(1, null) is not null, null is null,"
            " 1 is not null, '{}'::int[] is null, 1 is null as x"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [("?column?", 16)] * 7 + [
            ("x", 16)
        ]
        assert cur.fetchall() == [(True, False, False, False, True, True, False, False)]
        cur = conn.execute("""select unnest('{a,"b",",","\\\\","{","}",€}'::text[])""")
        assert [(d.name, d.type_code) for d in cur.description] == [("unnest", 25)]
        assert [r[0] for r in cur.fetchall()] == ["a", "b", ",", "\\", "{", "}", "€"]
        cur = conn.execute("select unnest('{1,2}'::int[]) as u")
        assert [(d.name, d.type_code) for d in cur.description] == [("u", 23)]
        assert cur.fetchall() == [(1,), (2,)]
        assert conn.execute("select unnest(null::text[])").fetchall() == []


def test_assignment_needs_an_assignment_cast(home: Path) -> None:
    """A typed expression assigned to a column with no assignment cast is 42804.

    Measured on PostgreSQL 16: psycopg's binary-format string is declared
    ``text`` (its text-format one is untyped), and ``text`` has no assignment
    cast to ``jsonb`` / ``integer`` -- ``column "data" is of type jsonb but
    expression is of type text`` with PostgreSQL's hint, on INSERT and UPDATE
    alike. Into a string column any type stores (an I/O cast), an untyped
    string still coerces through the column's parser, and ``bigint`` into
    ``integer`` is a plain assignment cast. Before this the server coerced the
    text through the column's parser whatever the client declared.
    """
    hint = "You will need to rewrite or cast the expression."
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        conn.execute("create table testjson(id int, data jsonb, n int, s text)")
        conn.execute("insert into testjson (id, data) values (1, %t)", ["{}"])
        assert conn.execute("select data from testjson").fetchone() == ({},)
        for sql, args in [
            ("insert into testjson (data) values (%b)", ["{}"]),
            ("update testjson set data = %b", ["{}"]),
            ("insert into testjson (data) values (%s::text)", ["{}"]),
        ]:
            with pytest.raises(psycopg.errors.DatatypeMismatch) as ex:
                conn.execute(sql, args)
            assert ex.value.diag.message_primary == (
                'column "data" is of type jsonb but expression is of type text'
            )
            assert ex.value.diag.message_hint == hint
        with pytest.raises(psycopg.errors.DatatypeMismatch) as ex:
            conn.execute("update testjson set n = %s::varchar", ["1"])
        assert ex.value.diag.message_primary == (
            'column "n" is of type integer but expression is of type character varying'
        )
        with pytest.raises(psycopg.errors.DatatypeMismatch) as ex:
            conn.execute("update testjson set n = true")
        assert ex.value.diag.message_primary == (
            'column "n" is of type integer but expression is of type boolean'
        )
        conn.execute("update testjson set s = %b, n = %s::bigint where id = 1", ["x", 7])
        conn.execute("update testjson set s = 5::int where id = 1")
        assert conn.execute("select s, n from testjson").fetchone() == ("5", 7)


def test_startup_parameter_status_matches_show(home: Path) -> None:
    """The ``ParameterStatus`` sent at startup is what ``SHOW`` then reports.

    psycopg's ``test_parameter_status`` compares the two; the startup value
    came from the wire library's defaults (``Etc/UTC``) while ``SHOW
    TimeZone`` answered the session's ``UTC``.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        for name in ("TimeZone", "DateStyle"):
            shown = conn.execute(f"show {name}").fetchone()[0]
            assert conn.info.parameter_status(name) == shown


def test_a_table_is_also_its_row_type(home: Path) -> None:
    """``CREATE TABLE t`` also creates the composite type ``t``.

    psycopg's ``test_array_register`` casts to a table's row type and its
    array; everything below is measured on PostgreSQL 16.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        conn.execute("create table rtt (a int, b text)")
        conn.execute("create type rten as enum ('a')")
        assert conn.execute(
            """select '(1,foo)'::rtt, ('(1,foo)'::rtt).b,
                      '{"(1,foo)","(2,bar)"}'::rtt[], row(1,'x')::rtt"""
        ).fetchone() == ("(1,foo)", "foo", '{"(1,foo)","(2,bar)"}', "(1,x)")
        assert conn.execute(
            """select pg_typeof('(1,foo)'::rtt)::text, to_regtype('rtt')::text,
                      to_regtype('rtt[]')::text, to_regtype('_rtt')::text,
                      to_regtype('rten[]')::text"""
        ).fetchone() == ("rtt", "rtt", "rtt[]", "rtt[]", "rten[]")

        for sql, sqlstate, message, detail in [
            (
                "select '(1,foo,extra)'::rtt",
                "22P02",
                'malformed record literal: "(1,foo,extra)"',
                "Too many columns.",
            ),
            ("select '(1)'::rtt", "22P02", 'malformed record literal: "(1)"', "Too few columns."),
            (
                "select row(1)::rtt",
                "42846",
                "cannot cast type record to rtt",
                "Input has too few columns.",
            ),
            (
                "select row(1,2,3)::rtt",
                "42846",
                "cannot cast type record to rtt",
                "Input has too many columns.",
            ),
        ]:
            with pytest.raises(psycopg.Error) as exc:
                conn.execute(sql)
            assert _diag(exc.value)[:3] == (sqlstate, message, detail), sql

        for sql, sqlstate, message, hint in [
            (
                "drop type rtt",
                "2BP01",
                "cannot drop type rtt because table rtt requires it",
                "You can drop table rtt instead.",
            ),
            ("create type rtt as enum ('a')", "42710", 'type "rtt" already exists', None),
            ("create type rtt as (x int)", "42710", 'type "rtt" already exists', None),
            (
                "create table rten (x int)",
                "42710",
                'type "rten" already exists',
                "A relation has an associated type of the same name, so you must use "
                "a name that doesn't conflict with any existing type.",
            ),
        ]:
            with pytest.raises(psycopg.Error) as exc:
                conn.execute(sql)
            assert (exc.value.sqlstate, exc.value.diag.message_primary) == (sqlstate, message), sql
            assert exc.value.diag.message_hint == hint, sql

        # Dropping the table takes its row type with it.
        conn.execute("drop table rtt")
        assert conn.execute("select to_regtype('rtt')").fetchone() == (None,)
        conn.execute("create type rtt as (x int)")
        with pytest.raises(psycopg.Error) as exc:
            conn.execute("create table rtt (x int)")
        assert (exc.value.sqlstate, exc.value.diag.message_primary) == (
            "42P07",
            'relation "rtt" already exists',
        )


def test_aclitem_parses_and_renders_as_postgresql(home: Path) -> None:
    """``aclitem`` — oid 1033 / array 1034 — with PostgreSQL 16's parser.

    psycopg's ``test_array_of_unknown_builtin`` reads the session user's
    grant back through both. The grantee and grantor are roles: the one this
    server knows is the session user.
    """
    from psycopg.types import TypeInfo

    with _Server(home) as server, server.connect(autocommit=True) as conn:
        notices: list[tuple[str, str, str]] = []
        conn.add_notice_handler(
            lambda d: notices.append((d.severity, d.sqlstate, d.message_primary))
        )
        user = conn.execute("select user").fetchone()[0]
        assert user == "test"
        info = TypeInfo.fetch(conn, "aclitem")
        assert (info.oid, info.array_oid) == (1033, 1034)

        cur = conn.execute(
            "select 'test=arwdDxt/test'::aclitem, array['test=arwdDxt/test']::aclitem[],"
            " '{test=r/test, \"\\\"test\\\"=w*a/test\"}'::aclitem[], '=r/test'::aclitem,"
            " pg_typeof('test=r/test'::aclitem)::text, %s::aclitem, %s::aclitem[]",
            ("group test=r/test", "{user test=wr/test}"),
        )
        assert cur.fetchone() == (
            "test=arwdDxt/test",
            ["test=arwdDxt/test"],
            ["test=r/test", "test=aw*/test"],
            "=r/test",
            "aclitem",
            "test=r/test",
            ["test=rw/test"],
        )
        assert [d.type_code for d in cur.description[:2]] == [1033, 1034]

        # An omitted grantor defaults to the superuser, with PostgreSQL's WARNING.
        assert conn.execute("select 'test=r'::aclitem").fetchone() == ("test=r/test",)
        assert notices == [("WARNING", "0L000", "defaulting grantor to user ID 10")]

        for sql, sqlstate, message, hint in [
            ("select 'nobody=r/test'::aclitem", "42704", 'role "nobody" does not exist', None),
            ("select 'test=r/nobody'::aclitem", "42704", 'role "nobody" does not exist', None),
            (
                "select 'test=q/test'::aclitem",
                "22P02",
                'invalid mode character: must be one of "arwdDxtXUCTcsA"',
                None,
            ),
            (
                "select 'junk'::aclitem",
                "22P02",
                'unrecognized key word: "junk"',
                'ACL key word must be "group" or "user".',
            ),
            ("select 'test=r/'::aclitem", "22P02", 'a name must follow the "/" sign', None),
            (
                "select 'test=r/test extra'::aclitem",
                "22P02",
                "extra garbage at the end of the ACL specification",
                None,
            ),
        ]:
            with pytest.raises(psycopg.Error) as exc:
                conn.execute(sql)
            assert (exc.value.sqlstate, exc.value.diag.message_primary) == (sqlstate, message), sql
            assert exc.value.diag.message_hint == hint, sql


def test_standard_conforming_strings_off_is_honoured_and_reported(home: Path) -> None:
    """``SET standard_conforming_strings TO off`` changes how the server READS
    a plain string literal, and is reported to the client only because it does.

    psycopg's ``test_quote_stable_despite_deranged_libpq`` flips the setting
    and checks libpq's ``PQescapeString`` follows the report. Every message
    and position here was measured on PostgreSQL 16.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        notices: list[tuple[str, str | None, str, str | None, str | None]] = []
        conn.add_notice_handler(
            lambda d: notices.append(
                (
                    d.severity or "",
                    d.sqlstate,
                    d.message_primary or "",
                    d.message_hint,
                    d.statement_position,
                )
            )
        )
        assert conn.info.parameter_status("standard_conforming_strings") == "on"
        assert conn.execute("select 'a\\nb', '\\\\'").fetchone() == ("a\\nb", "\\\\")
        assert notices == []

        conn.execute("set standard_conforming_strings to off")
        assert conn.info.parameter_status("standard_conforming_strings") == "off"
        assert conn.execute("show standard_conforming_strings").fetchone() == ("off",)
        cur = conn.execute(
            "select 'a\\'b', 'x\\\\y', 'p\\nq', 'r\\101s', 'c''d', E'\\\\', $$e\\f$$, %s",
            ["z"],
        )
        assert cur.fetchone() == ("a'b", "x\\y", "p\nq", "rAs", "c'd", "\\", "e\\f", "z")
        # One WARNING per literal, for its FIRST escape, worded by that escape
        # and positioned at the literal.
        assert notices == [
            (
                "WARNING",
                "22P06",
                "nonstandard use of \\' in a string literal",
                "Use '' to write quotes in strings, or use the escape string syntax (E'...').",
                "8",
            ),
            (
                "WARNING",
                "22P06",
                "nonstandard use of \\\\ in a string literal",
                "Use the escape string syntax for backslashes, e.g., E'\\\\'.",
                "16",
            ),
            (
                "WARNING",
                "22P06",
                "nonstandard use of escape in a string literal",
                "Use the escape string syntax for escapes, e.g., E'\\r\\n'.",
                "24",
            ),
            (
                "WARNING",
                "22P06",
                "nonstandard use of escape in a string literal",
                "Use the escape string syntax for escapes, e.g., E'\\r\\n'.",
                "32",
            ),
        ]
        notices.clear()

        # A literal continued over a newline is one literal; comments, quoted
        # identifiers and dollar-quoted bodies are not literals.
        sql = "select 'a\\'b'\n'\\\\' as \"c'\\\" /* '\\' */ -- 'x\\'"
        assert conn.execute(sql).fetchone() == ("a'b\\",)
        assert [n[2] for n in notices] == ["nonstandard use of \\' in a string literal"]
        notices.clear()

        # The warnings precede the error of an unterminated literal, which is
        # named as written, not as rewritten.
        with pytest.raises(psycopg.errors.SyntaxError) as exc:
            conn.execute("select 'q\\'")
        assert _diag(exc.value)[:2] == ("42601", "unterminated quoted string at or near \"'q\\'\"")
        assert [n[2] for n in notices] == ["nonstandard use of \\' in a string literal"]
        notices.clear()

        with pytest.raises(psycopg.errors.FeatureNotSupported) as exc:
            conn.execute("select U&'d\\0061t'")
        assert _diag(exc.value)[:3] == (
            "0A000",
            "unsafe use of string constant with Unicode escapes",
            "String constants with Unicode escapes cannot be used when"
            " standard_conforming_strings is off.",
        )

        # `escape_string_warning` silences the notices, not the reading.
        conn.execute("set escape_string_warning to off")
        assert conn.execute("select 'a\\nb'").fetchone() == ("a\nb",)
        assert notices == []
        conn.execute("reset escape_string_warning")

        # A statement is read under the setting in force when it is PREPARED.
        cur = conn.cursor()
        assert cur.execute("select 'a\\nb' || %s", ["!"], prepare=True).fetchone() == ("a\nb!",)
        conn.execute("set standard_conforming_strings to on")
        assert conn.info.parameter_status("standard_conforming_strings") == "on"
        assert cur.execute("select 'a\\nb' || %s", ["!"], prepare=True).fetchone() == ("a\nb!",)
        assert conn.execute("select 'a\\nb'").fetchone() == ("a\\nb",)

        # `set_config` reports it too; the value is a Boolean in any spelling.
        assert conn.execute(
            "select set_config('standard_conforming_strings', 'of', false)"
        ).fetchone() == ("off",)
        assert conn.info.parameter_status("standard_conforming_strings") == "off"
        for spelling, value in [("yes", "on"), ("0", "off"), ("TRUE", "on"), ("n", "off")]:
            conn.execute(f"set standard_conforming_strings to {spelling}")
            assert conn.execute("show standard_conforming_strings").fetchone() == (value,)
        for guc in ["standard_conforming_strings", "escape_string_warning"]:
            with pytest.raises(psycopg.errors.InvalidParameterValue) as exc:
                conn.execute(f"set {guc} to bogus")
            assert _diag(exc.value)[:2] == ("22023", f'parameter "{guc}" requires a Boolean value')
        conn.execute("reset standard_conforming_strings")
        assert conn.info.parameter_status("standard_conforming_strings") == "on"


def test_syntax_errors_carry_postgresqls_message_only(home: Path) -> None:
    """A syntax error's ``message_primary`` is PostgreSQL's text, without the
    ``Error splitting: `` label libpg_query's Rust binding prefixes it with."""
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        for sql, message in [
            ("selct 1", 'syntax error at or near "selct"'),
            ("select 1 from", "syntax error at end of input"),
            ("select 'q", 'unterminated quoted string at or near "\'q"'),
            ("select $$x", 'unterminated dollar-quoted string at or near "$$x"'),
            ('select "q', 'unterminated quoted identifier at or near ""q"'),
        ]:
            with pytest.raises(psycopg.errors.SyntaxError) as exc:
                conn.execute(sql)
            assert _diag(exc.value)[:2] == ("42601", message)
            with pytest.raises(psycopg.errors.SyntaxError) as exc:
                conn.execute(sql, prepare=True)
            assert _diag(exc.value)[:2] == ("42601", message)


# ---------------------------------------------------------------------------
# Shell types, base types and their LANGUAGE internal I/O functions -- the
# `CREATE TYPE "a-b"; CREATE FUNCTION invin(cstring) RETURNS "a-b" ...;
# CREATE TYPE "a-b" (input=invin, output=invout, like=text)` sequence that
# psycopg's `TestLiteral::test_invalid_name` runs. Every value below was
# measured on PostgreSQL 16.15 (2026-09-09).
# ---------------------------------------------------------------------------

_SHELL_TYPE_DDL = """
create type "{name}";
create function invin(cstring) returns "{name}" language internal immutable strict as 'textin';
create function invout("{name}") returns cstring language internal immutable strict as 'textout';
create type "{name}" (input=invin, output=invout, like=text);
"""


def _notice_diags(conn: psycopg.Connection) -> list[tuple[str, str | None, str, str | None]]:
    seen: list[tuple[str, str | None, str, str | None]] = []
    conn.add_notice_handler(
        lambda d: seen.append(
            (d.severity or "", d.sqlstate, d.message_primary or "", d.message_detail)
        )
    )
    return seen


@pytest.mark.parametrize("name", ["a-b", "€", "order", "foo bar", "FooBar"])
def test_base_type_over_a_shell_round_trips_text_and_arrays(home: Path, name: str) -> None:
    """The full shell -> I/O functions -> base type sequence, then values.

    A completed base type casts a string literal to itself and to its array
    type, both described with the type's OWN oids: `pg_type` reports the
    scalar row (typarray = oid + 100_000, the shared-store rule) and regtype
    renders the name quoted whenever its spelling needs it -- which is what
    the five spellings here exercise (measured on 16: `"a-b"`, `"€"`,
    `"order"`, `"foo bar"`, `"FooBar"`).
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute(_SHELL_TYPE_DDL.format(name=name))
        assert cur.statusmessage == "CREATE TYPE"
        cur.execute("select oid, typarray from pg_type where typname = %s", (name,))
        oid, typarray = cur.fetchone()
        assert typarray == oid + 100_000
        cur.execute(f"""select '{name}'::regtype::text""".replace(name, f'"{name}"'))
        assert cur.fetchone()[0] == f'"{name}"'

        cur.execute(f"""select 'hello-inv'::"{name}" """)
        assert cur.fetchone() == ("hello-inv",)
        assert cur.description[0].type_code == oid
        # Unregistered, the array oid has no loader and the text stays text
        # (as with PostgreSQL); psycopg's TypeInfo.fetch is what the gauge
        # test registers, and it reads pg_type's typarray.
        cur.execute(f"""select '{{hello-inv}}'::"{name}"[]""")
        assert cur.fetchone() == ("{hello-inv}",)
        assert cur.description[0].type_code == typarray
        info = psycopg.types.TypeInfo.fetch(conn, f'"{name}"')
        assert (info.oid, info.array_oid, info.name) == (oid, typarray, name)
        info.register(conn)
        # (A cursor snapshots the adapters at creation: a fresh one sees it.)
        assert conn.execute(f"""select '{{hello-inv}}'::"{name}"[]""").fetchone() == (
            ["hello-inv"],
        )

        with pytest.raises(psycopg.errors.CannotCoerce) as exc:
            cur.execute(f"""select 1::"{name}" """)
        assert _diag(exc.value)[:2] == ("42846", f'cannot cast type integer to "{name}"')


def test_shell_type_is_only_a_shell_until_completed(home: Path) -> None:
    """A bare `CREATE TYPE t` is a shell: nothing can be cast to it, regtype
    refuses it, to_regtype hides it, `pg_type` shows no array type, and a
    second shell of the same name is a duplicate (all measured on 16)."""
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute('create type "a-b"')
        assert cur.statusmessage == "CREATE TYPE"
        cur.execute("select typarray from pg_type where typname = 'a-b'")
        assert cur.fetchone() == (0,)
        cur.execute("""select to_regtype('"a-b"')""")
        assert cur.fetchone() == (None,)
        for sql in ["""select 'x'::"a-b" """, """select '"a-b"'::regtype"""]:
            with pytest.raises(psycopg.errors.UndefinedObject) as exc:
                cur.execute(sql)
            assert _diag(exc.value)[:2] == ("42704", 'type "a-b" is only a shell')
        with pytest.raises(psycopg.errors.DuplicateObject) as exc:
            cur.execute('create type "a-b"')
        assert _diag(exc.value)[:2] == ("42710", 'type "a-b" already exists')
        with pytest.raises(psycopg.errors.UndefinedObject) as exc:
            cur.execute('create table tt (c "a-b")')
        assert _diag(exc.value)[:2] == ("42704", 'type "a-b" is only a shell')


def test_full_create_type_checks_its_shell_and_io_functions(home: Path) -> None:
    """Each refusal of the full `CREATE TYPE name (input=, output=)` form, in
    PostgreSQL 16's order and words: no shell first (42710 -- not 42704 --
    with the shell hint), then each I/O option missing (42P17), each function
    missing with its exact signature (42883), and each returning the wrong
    type (42P17). Once completed the type is a duplicate."""
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        with pytest.raises(psycopg.errors.DuplicateObject) as exc:
            cur.execute('create type "c-d" (input=invin, output=invout)')
        assert _diag(exc.value)[:2] == ("42710", 'type "c-d" does not exist')
        assert exc.value.diag.message_hint == (
            "Create the type as a shell type, then create its I/O functions, "
            "then do a full CREATE TYPE."
        )
        cur.execute('create type "a-b"')
        for sql, message in [
            ('create type "a-b" (output=invout)', "type input function must be specified"),
            ('create type "a-b" (input=invin)', "type output function must be specified"),
        ]:
            with pytest.raises(psycopg.errors.InvalidObjectDefinition) as exc:
                cur.execute(sql)
            assert _diag(exc.value)[:2] == ("42P17", message)
        with pytest.raises(psycopg.errors.UndefinedFunction) as exc:
            cur.execute('create type "a-b" (input=invin, output=invout)')
        assert _diag(exc.value)[:2] == ("42883", "function invin(cstring) does not exist")

        cur.execute(
            """create function invin(cstring) returns "a-b" language internal as 'textin'"""
        )
        with pytest.raises(psycopg.errors.UndefinedFunction) as exc:
            cur.execute('create type "a-b" (input=invin, output=invout)')
        assert _diag(exc.value)[:2] == ("42883", 'function invout("a-b") does not exist')
        cur.execute("create function textin2(cstring) returns text language internal as 'textin'")
        with pytest.raises(psycopg.errors.InvalidObjectDefinition) as exc:
            cur.execute('create type "a-b" (input=textin2, output=invout)')
        assert _diag(exc.value)[:2] == (
            "42P17",
            'type input function textin2 must return type "a-b"',
        )
        cur.execute("""create function badout("a-b") returns text language internal as 'textout'""")
        with pytest.raises(psycopg.errors.InvalidObjectDefinition) as exc:
            cur.execute('create type "a-b" (input=invin, output=badout)')
        assert _diag(exc.value)[:2] == (
            "42P17",
            "type output function badout must return type cstring",
        )
        cur.execute(
            """create function invout("a-b") returns cstring language internal as 'textout'"""
        )
        cur.execute('create type "a-b" (input=invin, output=invout)')
        with pytest.raises(psycopg.errors.DuplicateObject) as exc:
            cur.execute('create type "a-b" (input=invin, output=invout)')
        assert _diag(exc.value)[:2] == ("42710", 'type "a-b" already exists')


def test_internal_function_ddl_notices_and_errors(home: Path) -> None:
    """`CREATE FUNCTION ... LANGUAGE internal`: a shell argument / return
    type is accepted with a 42809 NOTICE naming the type UNQUOTED; an
    unknown return type becomes a new shell with a 42704 NOTICE (and its
    `Creating a shell type definition.` detail); an unknown argument type is
    a 42704 error (unquoted); an unknown built-in is 42883; a duplicate
    signature is 42723 unless OR REPLACE. All measured on 16."""
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        notices = _notice_diags(conn)
        cur = conn.cursor()
        cur.execute('create type "a-b"')
        cur.execute(
            """create function invin(cstring) returns "a-b" language internal as 'textin'"""
        )
        assert cur.statusmessage == "CREATE FUNCTION"
        assert notices == [("NOTICE", "42809", "return type a-b is only a shell", None)]
        notices.clear()
        cur.execute(
            """create function invout("a-b") returns cstring language internal as 'textout'"""
        )
        assert notices == [("NOTICE", "42809", "argument type a-b is only a shell", None)]
        notices.clear()

        cur.execute("""create function f3(cstring) returns nosuch language internal as 'textin'""")
        assert notices == [
            (
                "NOTICE",
                "42704",
                'type "nosuch" is not yet defined',
                "Creating a shell type definition.",
            )
        ]
        cur.execute("select typarray from pg_type where typname = 'nosuch'")
        assert cur.fetchone() == (0,)

        with pytest.raises(psycopg.errors.UndefinedObject) as exc:
            cur.execute("""create function f2(nosuch2) returns int language internal as 'int4in'""")
        assert _diag(exc.value)[:2] == ("42704", "type nosuch2 does not exist")
        with pytest.raises(psycopg.errors.UndefinedFunction) as exc:
            cur.execute("""create function f4(cstring) returns text language internal as 'nope'""")
        assert _diag(exc.value)[:2] == ("42883", 'there is no built-in function named "nope"')
        with pytest.raises(psycopg.errors.DuplicateFunction) as exc:
            cur.execute(
                """create function invin(cstring) returns "a-b" language internal as 'textin'"""
            )
        assert _diag(exc.value)[:2] == (
            "42723",
            'function "invin" already exists with same argument types',
        )
        cur.execute(
            'create or replace function invin(cstring) returns "a-b" '
            "language internal as 'textin'"
        )
        assert cur.statusmessage == "CREATE FUNCTION"


def test_drop_type_restricts_on_its_io_functions_and_cascades_with_a_notice(
    home: Path,
) -> None:
    """A base type's I/O functions depend on it: plain DROP TYPE is 2BP01
    with one DETAIL line per function and the CASCADE hint; CASCADE drops
    them with `drop cascades to N other objects` (one dependent is named in
    the message itself instead); IF EXISTS on a missing type is a NOTICE.
    Inside a transaction the cascade rolls back. Measured on 16."""
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        notices = _notice_diags(conn)
        cur = conn.cursor()
        cur.execute(_SHELL_TYPE_DDL.format(name="a-b"))
        with pytest.raises(psycopg.errors.DependentObjectsStillExist) as exc:
            cur.execute('drop type "a-b"')
        assert _diag(exc.value)[:3] == (
            "2BP01",
            'cannot drop type "a-b" because other objects depend on it',
            'function invin(cstring) depends on type "a-b"\n'
            'function invout("a-b") depends on type "a-b"',
        )
        assert exc.value.diag.message_hint == (
            "Use DROP ... CASCADE to drop the dependent objects too."
        )

        conn.autocommit = False
        notices.clear()
        cur.execute('drop type "a-b" cascade')
        assert notices == [
            (
                "NOTICE",
                "00000",
                "drop cascades to 2 other objects",
                'drop cascades to function invin(cstring)\ndrop cascades to function invout("a-b")',
            )
        ]
        conn.rollback()
        conn.autocommit = True
        cur.execute("""select 'still'::"a-b" """)
        assert cur.fetchone() == ("still",)

        notices.clear()
        cur.execute('drop type "a-b" cascade')
        cur.execute("select count(*) from pg_type where typname = 'a-b'")
        assert cur.fetchone() == (0,)
        with pytest.raises(psycopg.errors.UndefinedObject) as exc:
            cur.execute('drop type "a-b"')
        assert _diag(exc.value)[:2] == ("42704", 'type "a-b" does not exist')
        notices.clear()
        cur.execute('drop type if exists "a-b"')
        assert cur.statusmessage == "DROP TYPE"
        assert notices == [("NOTICE", "00000", 'type "a-b" does not exist, skipping', None)]

        # A shell with ONE dependent: the function is named in the message.
        cur.execute("create type sh")
        cur.execute("create function shin(cstring) returns sh language internal as 'textin'")
        notices.clear()
        cur.execute("drop type sh cascade")
        assert notices == [("NOTICE", "00000", "drop cascades to function shin(cstring)", None)]


def test_drop_function_resolves_signature_types_and_dependents(home: Path) -> None:
    """DROP FUNCTION: an argument type that does not exist is the TYPE's
    42704 (quoted), a missing signature 42883 with the signature, a bare name
    that matches nothing `could not find a function named`, and IF EXISTS
    turns each into a `... does not exist, skipping` NOTICE. A defined base
    type depends on its I/O functions: 2BP01 lists the type and the type's
    other function; CASCADE drops both and says so. Measured on 16."""
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        notices = _notice_diags(conn)
        cur = conn.cursor()
        cur.execute(_SHELL_TYPE_DDL.format(name="a-b"))
        with pytest.raises(psycopg.errors.DependentObjectsStillExist) as exc:
            cur.execute("drop function invin(cstring)")
        assert _diag(exc.value)[:3] == (
            "2BP01",
            "cannot drop function invin(cstring) because other objects depend on it",
            'type "a-b" depends on function invin(cstring)\n'
            'function invout("a-b") depends on type "a-b"',
        )
        with pytest.raises(psycopg.errors.UndefinedFunction) as exc:
            cur.execute("drop function invout(cstring)")
        assert _diag(exc.value)[:2] == ("42883", "function invout(cstring) does not exist")
        with pytest.raises(psycopg.errors.UndefinedFunction) as exc:
            cur.execute("drop function nosuch")
        assert _diag(exc.value)[:2] == ("42883", 'could not find a function named "nosuch"')
        notices.clear()
        cur.execute("drop function if exists invout(cstring)")
        cur.execute("drop function if exists nosuch")
        assert cur.statusmessage == "DROP FUNCTION"
        assert notices == [
            ("NOTICE", "00000", "function invout(cstring) does not exist, skipping", None),
            ("NOTICE", "00000", "function nosuch() does not exist, skipping", None),
        ]

        notices.clear()
        cur.execute('drop function invout("a-b") cascade')
        assert notices == [
            (
                "NOTICE",
                "00000",
                "drop cascades to 2 other objects",
                'drop cascades to type "a-b"\ndrop cascades to function invin(cstring)',
            )
        ]
        cur.execute("select count(*) from pg_type where typname = 'a-b'")
        assert cur.fetchone() == (0,)
        # The type is gone, so its name in a signature fails as a TYPE.
        with pytest.raises(psycopg.errors.UndefinedObject) as exc:
            cur.execute('drop function invout("a-b")')
        assert _diag(exc.value)[:2] == ("42704", 'type "a-b" does not exist')
        notices.clear()
        cur.execute('drop function if exists invout("a-b")')
        assert notices == [("NOTICE", "00000", 'type "a-b" does not exist, skipping', None)]


def test_listen_notify_delivers_to_every_listener_at_commit(home: Path) -> None:
    """LISTEN / NOTIFY / `pg_notify()` / UNLISTEN / `pg_listening_channels()`.

    Every shape here was measured on PostgreSQL 16 over the raw wire: a
    NOTIFY outside a block reaches the listeners -- the sender included --
    as a `NotificationResponse` BEFORE the statement's `ReadyForQuery`; inside
    a block the NOTIFYs queue, duplicates of one `(channel, payload)` collapse
    to the first, and they go out after the COMMIT's tag (or nowhere on
    ROLLBACK); an idle listener hears without asking, a listener idle in its
    own block hears at its COMMIT; an unquoted channel folds to lower case;
    `pg_notify` refuses an empty or NULL channel with 22023 and takes a NULL
    payload as the empty string; a payload of 8000 bytes is 22023 too.
    """
    with _Server(home) as server, server.connect() as a, server.connect() as b:
        a_pid, b_pid = a.info.backend_pid, b.info.backend_pid
        a.execute("listen foo")
        assert a.execute("select pg_listening_channels()").fetchall() == [("foo",)]

        # The sender hears its own NOTIFY, before the statement returns.
        a.execute("notify FOO, 'self'")
        assert [(n.pid, n.channel, n.payload) for n in a.notifies(timeout=0, stop_after=1)] == [
            (a_pid, "foo", "self")
        ]

        # An idle listener hears a NOTIFY from another session unprompted.
        b.execute("notify foo, 'idle'")
        assert [(n.pid, n.channel, n.payload) for n in a.notifies(timeout=2, stop_after=1)] == [
            (b_pid, "foo", "idle")
        ]

        # Queued in a block, deduplicated, delivered at COMMIT; not on ROLLBACK.
        with b.transaction():
            b.execute("notify foo, 'a'")
            b.execute("select pg_notify('foo', 'b')")
            b.execute("notify foo, 'a'")
            assert list(a.notifies(timeout=0.2)) == []
        assert [n.payload for n in a.notifies(timeout=2, stop_after=2)] == ["a", "b"]
        with contextlib.suppress(ZeroDivisionError), b.transaction():
            b.execute("notify foo, 'lost'")
            raise ZeroDivisionError
        assert list(a.notifies(timeout=0.2)) == []

        # A listener idle in its own block hears nothing until it commits.
        with a.transaction():
            b.execute("notify foo, 'held'")
            assert list(a.notifies(timeout=0.2)) == []
        assert [n.payload for n in a.notifies(timeout=2, stop_after=1)] == ["held"]

        # NULL payload is the empty string; empty / NULL channel is 22023.
        b.execute("select pg_notify('foo', NULL)")
        assert [n.payload for n in a.notifies(timeout=2, stop_after=1)] == [""]
        for sql in ["select pg_notify('', 'x')", "select pg_notify(NULL, 'x')"]:
            with pytest.raises(psycopg.errors.InvalidParameterValue) as info:
                b.execute(sql)
            assert info.value.diag.message_primary == "channel name cannot be empty"
        with pytest.raises(psycopg.errors.InvalidParameterValue) as info:
            b.execute("select pg_notify('foo', %s)", ("x" * 8000,))
        assert info.value.diag.message_primary == "payload string too long"
        b.execute("select pg_notify('foo', %s)", ("x" * 7999,))
        assert [len(n.payload) for n in a.notifies(timeout=2, stop_after=1)] == [7999]

        # UNLISTEN one, then all; LISTEN in a rolled-back block never lands.
        a.execute("listen bar")
        assert a.execute("select pg_listening_channels()").fetchall() == [("foo",), ("bar",)]
        a.execute("unlisten foo")
        b.execute("notify foo, 'gone'")
        b.execute("notify bar, 'still'")
        assert [(n.channel, n.payload) for n in a.notifies(timeout=2, stop_after=1)] == [
            ("bar", "still")
        ]
        a.execute("unlisten *")
        assert a.execute("select pg_listening_channels()").fetchall() == []
        with contextlib.suppress(ZeroDivisionError), a.transaction():
            a.execute("listen foo")
            raise ZeroDivisionError
        assert a.execute("select pg_listening_channels()").fetchall() == []
        b.execute("notify foo, 'nobody'")
        assert list(a.notifies(timeout=0.2)) == []


def test_pg_cancel_and_terminate_backend_signal_a_running_statement(home: Path) -> None:
    """`pg_cancel_backend(pid)` interrupts the victim's statement with
    `57014` and leaves the session usable; `pg_terminate_backend(pid)`
    ends it with FATAL `57P01` and closes the socket -- both within a few
    milliseconds of the signal, even while the victim is inside
    `pg_sleep`, and also while the victim is idle (a terminated idle
    session fails on its next statement). Both answer `true` for a known
    pid, `false` plus a `01000` WARNING for an unknown one, and NULL for
    NULL. PostgreSQL 16.
    """
    import threading

    from psycopg.pq import TransactionStatus

    with _Server(home) as server, server.connect() as conn, server.connect() as other:
        pid = conn.info.backend_pid

        def signal(fn: str, delay: float) -> None:
            time.sleep(delay)
            assert other.execute(f"select {fn}(%s)", (pid,)).fetchone() == (True,)

        t = threading.Thread(target=signal, args=("pg_cancel_backend", 0.2))
        t0 = time.monotonic()
        t.start()
        with pytest.raises(psycopg.errors.QueryCanceled) as info:
            conn.execute("select pg_sleep(5)")
        t.join()
        assert time.monotonic() - t0 < 1.0
        assert info.value.sqlstate == "57014"
        assert conn.info.transaction_status == TransactionStatus.IDLE
        assert conn.execute("select 1").fetchone() == (1,)

        # An idle cancel is a no-op the next statement does not see.
        assert other.execute("select pg_cancel_backend(%s)", (pid,)).fetchone() == (True,)
        time.sleep(0.05)
        assert conn.execute("select 2").fetchone() == (2,)

        notices = _notice_diags(other)
        assert other.execute("select pg_cancel_backend(999999)").fetchone() == (False,)
        assert other.execute("select pg_terminate_backend(999999)").fetchone() == (False,)
        assert other.execute("select pg_terminate_backend(NULL::int)").fetchone() == (None,)
        assert notices == [
            ("WARNING", "01000", "PID 999999 is not a PostgreSQL backend process", None),
            ("WARNING", "01000", "PID 999999 is not a PostgreSQL backend process", None),
        ]

        t = threading.Thread(target=signal, args=("pg_terminate_backend", 0.2))
        t0 = time.monotonic()
        t.start()
        with pytest.raises(psycopg.errors.AdminShutdown) as info:
            conn.execute("select pg_sleep(5)")
        t.join()
        assert time.monotonic() - t0 < 1.0
        assert info.value.sqlstate == "57P01"
        assert info.value.diag.severity == "FATAL"
        assert conn.closed

        # Terminating an IDLE session: it dies on its next statement.
        with server.connect() as idle:
            idle_pid = idle.info.backend_pid
            assert other.execute("select pg_terminate_backend(%s)", (idle_pid,)).fetchone() == (
                True,
            )
            time.sleep(0.1)
            with pytest.raises(psycopg.errors.AdminShutdown) as info:
                idle.execute("select 1")
            assert info.value.sqlstate == "57P01"
            assert idle.closed
            # The CLIENT observing its socket close and the SERVER reaping the
            # activity-registry entry are not synchronised -- the handler thread
            # unregisters after the peer is already gone. Asserting immediately
            # read `(1,)` under CI load on 2026-09-29 (green on the eleven runs
            # before it, which is exactly how a race this narrow presents).
            # Poll instead: still requires the entry to disappear, and now says
            # so deterministically rather than depending on who wins.
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                remaining = other.execute(
                    "select count(*) from pg_stat_activity where pid = %s", (idle_pid,)
                ).fetchone()
                if remaining == (0,):
                    break
                time.sleep(0.02)
            assert remaining == (0,), (
                f"a terminated session was still in pg_stat_activity after 10s: {remaining}"
            )


def test_create_table_as_function_sources_and_expression_aggregates(home: Path) -> None:
    """The planner shapes psycopg's own suite leans on, measured on PG 16.

    `CREATE TABLE ... AS query` takes the query's columns (renamed by a column
    list) and answers `SELECT n`, or `CREATE TABLE AS` for `WITH NO DATA` and
    for an `IF NOT EXISTS` that found the table; a duplicate is 42P07. A
    function in FROM is the row source (`select 'ok' from pg_sleep(0)` is one
    row named `?column?`; `select * from pg_listening_channels()` is one row
    per channel). An aggregate over an expression evaluates it per row.
    `pg_tables` lists every table with PostgreSQL's eight columns. `now()` is
    a `timestamptz`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.execute("create table t1 as select 1 as f1")
        assert cur.statusmessage == "SELECT 1"
        cur = conn.execute("create temp table tt (a, b) as select 1, 'x'::text")
        assert cur.statusmessage == "SELECT 1"
        cur = conn.execute("select a, b from tt")
        assert [(d.name, d.type_code) for d in cur.description] == [("a", 23), ("b", 25)]
        assert cur.fetchall() == [(1, "x")]
        cur = conn.execute("create table t2 as select f1, f1 * 2 as d from t1 with no data")
        assert cur.statusmessage == "CREATE TABLE AS"
        assert conn.execute("select count(*) from t2").fetchone() == (0,)
        with pytest.raises(psycopg.errors.DuplicateTable) as info:
            conn.execute("create table t1 as select 2")
        assert info.value.diag.message_primary == 'relation "t1" already exists'
        cur = conn.execute("create table if not exists t1 as select 2")
        assert cur.statusmessage == "CREATE TABLE AS"
        assert conn.execute("select f1 from t1").fetchall() == [(1,)]
        cur = conn.execute("create table accessed as (select now() as value)")
        assert cur.statusmessage == "SELECT 1"
        cur = conn.execute("select value from accessed")
        assert cur.description[0].type_code == 1184
        assert cur.fetchone()[0].tzinfo is not None

        # pg_tables: the psycopg pipeline tests probe it for a table's presence.
        cur = conn.execute("select * from pg_tables where tablename in ('t1', 'tt') order by 2")
        assert [d.name for d in cur.description] == [
            "schemaname",
            "tablename",
            "tableowner",
            "tablespace",
            "hasindexes",
            "hasrules",
            "hastriggers",
            "rowsecurity",
        ]
        assert cur.fetchall() == [
            ("public", "t1", "test", None, False, False, False, False),
            ("pg_temp_1", "tt", "test", None, False, False, False, False),
        ]
        cur = conn.execute("select count(*) from pg_tables where tablename = 'nope'")
        assert cur.fetchone() == (0,)

        # A function in FROM.
        cur = conn.execute("select 'ok' from pg_sleep(0)")
        assert [(d.name, d.type_code) for d in cur.description] == [("?column?", 25)]
        assert cur.fetchall() == [("ok",)]
        assert conn.execute("select * from pg_listening_channels()").fetchall() == []
        conn.execute("listen a")
        conn.execute("listen b")
        cur = conn.execute("select * from pg_listening_channels()")
        assert cur.description[0].name == "pg_listening_channels"
        assert cur.fetchall() == [("a",), ("b",)]
        assert conn.execute("select ch from pg_listening_channels() ch").fetchall() == [
            ("a",),
            ("b",),
        ]
        with pytest.raises(psycopg.errors.UndefinedColumn):
            conn.execute("select nope from pg_sleep(0)")

        # Aggregates over expressions.
        conn.execute("create table copy_in (col1 int primary key, col2 int, data text)")
        conn.execute("insert into copy_in values (1, 2, 'abc'), (2, 3, 'de')")
        cur = conn.execute(
            "select min(col1), max(col1), count(*), max(length(data)), sum(col1 * 2) from copy_in"
        )
        assert [d.type_code for d in cur.description] == [23, 23, 20, 23, 20]
        assert cur.fetchall() == [(1, 2, 2, 3, 6)]
        assert conn.execute(
            "select col2 % 2, max(length(data)) from copy_in group by 1 order by 1"
        ).fetchall() == [(0, 3), (1, 2)]


def test_describe_shapes_and_cursor_portals_match_postgres(home: Path) -> None:
    """The libpq-level Describe shapes psycopg's `tests/pq` checks, on PG 16.

    `Describe` of a statement reports a parameter's type from the CAST over
    it (`$1::int4, $2::text` is 23 / 25; nothing types it -> 705), and an
    integer expression over cast parameters types from its operands
    (`$1::int8 + $2::int8` is int8, not int4). A `begin; declare ...` simple
    query leaves the session IN a transaction with the cursor open; the
    cursor is a PORTAL of its name, so `Describe portal` sees its columns and
    a wire `Close portal` closes it (the next Describe is 34000). The
    `password_encryption` GUC is `scram-sha-256`, and `ALTER USER` of a role
    that does not exist is 42704.
    """
    from psycopg import pq

    with _Server(home) as server:
        conn = pq.PGconn.connect(
            f"host=127.0.0.1 port={server.port} dbname=postgres user=test".encode()
        )
        assert conn.status == pq.ConnStatus.OK, conn.error_message
        try:
            assert (
                conn.prepare(b"", b"select $1::int4, $2::text").status == pq.ExecStatus.COMMAND_OK
            )
            res = conn.describe_prepared(b"")
            assert [res.param_type(i) for i in range(res.nparams)] == [23, 25]
            assert [(res.fname(i), res.ftype(i)) for i in range(res.nfields)] == [
                (b"int4", 23),
                (b"text", 25),
            ]
            conn.prepare(b"p2", b"select $1::int8 + $2::int8 as fld")
            res = conn.describe_prepared(b"p2")
            assert [res.param_type(i) for i in range(res.nparams)] == [20, 20]
            assert [(res.fname(i), res.ftype(i)) for i in range(res.nfields)] == [(b"fld", 20)]
            conn.prepare(b"p3", b"select $1")
            res = conn.describe_prepared(b"p3")
            assert [res.param_type(i) for i in range(res.nparams)] == [705]

            res = conn.exec_(
                b"begin; declare cur cursor for select * from generate_series(1,10) foo;"
            )
            assert res.status == pq.ExecStatus.COMMAND_OK, res.error_message
            assert conn.transaction_status == pq.TransactionStatus.INTRANS
            res = conn.describe_portal(b"cur")
            assert res.status == pq.ExecStatus.COMMAND_OK, res.error_message
            assert [(res.fname(i), res.ftype(i)) for i in range(res.nfields)] == [(b"foo", 23)]
            res = conn.exec_(b"fetch 2 from cur")
            assert [res.get_value(r, 0) for r in range(res.ntuples)] == [b"1", b"2"]
            res = conn.close_portal(b"cur")
            assert res.status == pq.ExecStatus.COMMAND_OK, res.error_message
            res = conn.describe_portal(b"cur")
            assert res.status == pq.ExecStatus.FATAL_ERROR
            assert res.error_field(pq.DiagnosticField.SQLSTATE) == b"34000"
            assert conn.exec_(b"rollback").status == pq.ExecStatus.COMMAND_OK
            res = conn.exec_(b"begin; select 1; commit; select 2")
            assert res.status == pq.ExecStatus.TUPLES_OK
            assert conn.transaction_status == pq.TransactionStatus.IDLE

            res = conn.exec_(b"show password_encryption")
            assert (res.ftype(0), res.get_value(0, 0)) == (25, b"scram-sha-256")
            res = conn.exec_(b"alter user \"ashesh\" password 'x'")
            assert res.status == pq.ExecStatus.FATAL_ERROR
            assert res.error_field(pq.DiagnosticField.SQLSTATE) == b"42704"
            assert (
                res.error_field(pq.DiagnosticField.MESSAGE_PRIMARY)
                == b'role "ashesh" does not exist'
            )
            res = conn.exec_(b"alter user test password 'x'")
            assert res.status == pq.ExecStatus.COMMAND_OK, res.error_message
            assert res.command_status == b"ALTER ROLE"
        finally:
            conn.finish()


def test_catalog_changes_are_visible_across_connections(home: Path) -> None:
    """The process-wide catalog cache never serves a stale or an uncommitted row.

    Every step's expectation was measured on PostgreSQL 16 (2026-09-09): a
    table created on one connection is found by another that had already
    looked it up as missing; a block's uncommitted CREATE TYPE is its own
    business until COMMIT; a DROP rolled back leaves the table for everyone;
    a CREATE rolled back to a savepoint leaves nothing, on either connection.
    """

    def outcome(conn: psycopg.Connection, sql: str) -> object:
        try:
            return conn.execute(sql).fetchall()
        except psycopg.Error as e:
            if not conn.autocommit:
                conn.rollback()
            return e.sqlstate

    with _Server(home) as server:
        a = server.connect()
        b = server.connect()
        try:
            # 1. A negative lookup on b must not outlive a's CREATE TABLE.
            assert outcome(b, "select * from cc_t") == "42P01"
            a.execute("create table cc_t (id int)")
            assert outcome(b, "select * from cc_t") == []
            a.execute("insert into cc_t values (1)")
            assert outcome(b, "select * from cc_t") == [(1,)]

            # 2. An uncommitted CREATE TYPE is visible to its block only.
            a.autocommit = False
            a.execute("create type cc_mood as enum ('sad', 'ok')")
            assert outcome(a, "select 'ok'::cc_mood") == [("ok",)]
            assert isinstance(outcome(b, "select 'ok'::cc_mood"), str)
            a.commit()
            assert outcome(b, "select 'ok'::cc_mood") == [("ok",)]

            # 3. A DROP rolled back leaves the table, on both connections.
            a.execute("drop table cc_t")
            assert outcome(a, "select * from cc_t") == "42P01"
            a.rollback()
            assert outcome(b, "select * from cc_t") == [(1,)]
            assert outcome(a, "select * from cc_t") == [(1,)]
            a.rollback()

            # 4. A CREATE rolled back to a savepoint leaves nothing.
            a.execute("savepoint sp")
            a.execute("create table cc_s (id int)")
            assert outcome(a, "select * from cc_s") == []
            a.execute("rollback to savepoint sp")
            assert outcome(a, "select * from cc_s") == "42P01"
            a.commit()
            assert outcome(b, "select * from cc_s") == "42P01"

            # 5. b's autocommit CREATE is found by a once a's block ends.
            b.execute("create table cc_s (id int, v text)")
            a.commit()
            assert outcome(a, "select * from cc_s") == []

            # 6. A composite REDEFINED on a is resolved in its new shape by
            # b's very next statement. The planner's type tables are
            # published per thread and skipped while the catalog version
            # stands still, so this is the case that would serve the old
            # shape if a redefinition ever failed to move the version.
            a.commit()
            a.autocommit = True
            a.execute("create type cc_pt as (a int)")
            assert outcome(b, "select '(1)'::cc_pt") == [("(1)",)]
            a.execute("drop type cc_pt")
            a.execute("create type cc_pt as (a int, b text)")
            assert outcome(b, "select '(1,x)'::cc_pt") == [("(1,x)",)]
            assert outcome(b, "select '(1)'::cc_pt") == "22P02"
        finally:
            a.close()
            b.close()


def test_now_casts_to_text_in_session_zone(home: Path) -> None:
    """`now()::text` carries the session-zone offset, like PostgreSQL 16.

    A timestamptz instant is stored exactly like a naive timestamp, so the cast
    has to learn the source type from the expression; before that it rendered
    `2026-09-09 20:58:09.043676` where PostgreSQL renders `...+00` under UTC.
    `current_timestamp` inside an expression is the same value.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("set timezone to 'UTC'")
        cur.execute("select now()::text, current_timestamp::text, pg_typeof(now())::text")
        now_text, ts_text, typ = cur.fetchone()
        assert re.fullmatch(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}(\.\d{1,6})?\+00", now_text)
        assert re.fullmatch(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}(\.\d{1,6})?\+00", ts_text)
        assert typ == "timestamp with time zone"
        cur.execute("set timezone to 'Europe/Dublin'")
        cur.execute("select now()::text")
        assert re.search(r"\+0[01]$", cur.fetchone()[0])


# --- Two-phase commit --------------------------------------------------------
#
# Every expectation below was measured on PostgreSQL 16.15 with
# `max_prepared_transactions = 100` (2026-09-10 / 2026-09-17).


def _sqlstate(conn: psycopg.Connection, sql: str) -> str | None:
    """The SQLSTATE `sql` fails with, or None when it succeeds."""
    try:
        conn.execute(sql)
    except psycopg.Error as e:
        return e.sqlstate
    return None


def test_two_phase_commit_resolves_from_another_connection(home: Path) -> None:
    """PREPARE ends the block, the work stays invisible, and ANY connection
    may COMMIT PREPARED or ROLLBACK PREPARED it -- including after the
    preparing connection has gone away."""
    with _Server(home) as server:
        setup = server.connect()
        setup.execute("create table tpc (a int primary key, b int)")
        setup.execute("insert into tpc values (1, 10)")
        assert setup.execute("show max_prepared_transactions").fetchone() == ("100",)

        a = server.connect()
        a.execute("begin")
        a.execute("insert into tpc values (2, 20)")
        a.execute("update tpc set b = 11 where a = 1")
        a.execute("prepare transaction 'gid-commit'")
        # The block is over: this connection is IDLE, not in a transaction.
        assert a.info.transaction_status == psycopg.pq.TransactionStatus.IDLE
        # Neither the preparer nor anyone else sees the prepared writes.
        assert a.execute("select a, b from tpc order by a").fetchall() == [(1, 10)]
        assert setup.execute("select a, b from tpc order by a").fetchall() == [(1, 10)]
        a.close()

        b = server.connect()
        b.execute("begin")
        b.execute("insert into tpc values (3, 30)")
        b.execute("prepare transaction 'gid-rollback'")
        b.close()

        row = setup.execute(
            "select gid, owner, database, transaction > 0, prepared is not null "
            "from pg_prepared_xacts order by gid"
        ).fetchall()
        assert row == [
            ("gid-commit", "test", "postgres", True, True),
            ("gid-rollback", "test", "postgres", True, True),
        ]

        setup.execute("commit prepared 'gid-commit'")
        setup.execute("rollback prepared 'gid-rollback'")
        assert setup.execute("select a, b from tpc order by a").fetchall() == [(1, 11), (2, 20)]
        assert setup.execute("select count(*) from pg_prepared_xacts").fetchone() == (0,)
        setup.close()


def test_prepared_transaction_survives_a_restart(home: Path) -> None:
    """The prepared write set -- DDL included -- outlives the process.

    The daemon is stopped with the transaction prepared and started again on
    the same store; the gid is still listed, its rows and its table are still
    invisible, and COMMIT PREPARED then applies all of it.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table tpc_r (a int primary key, b int)")
        conn.execute("insert into tpc_r values (1, 10)")
        conn.execute("begin")
        conn.execute("create table tpc_made (x int)")
        conn.execute("insert into tpc_made values (9)")
        conn.execute("insert into tpc_r values (2, 20)")
        conn.execute("update tpc_r set b = 11 where a = 1")
        conn.execute("prepare transaction 'p-restart'")
        conn.execute("begin")
        conn.execute("insert into tpc_r values (3, 30)")
        conn.execute("prepare transaction 'p-discard'")

    with _Server(home) as server, server.connect() as conn:
        assert conn.execute("select gid from pg_prepared_xacts order by gid").fetchall() == [
            ("p-discard",),
            ("p-restart",),
        ]
        assert conn.execute("select a, b from tpc_r order by a").fetchall() == [(1, 10)]
        assert _sqlstate(conn, "select * from tpc_made") == "42P01"
        conn.execute("commit prepared 'p-restart'")
        conn.execute("rollback prepared 'p-discard'")
        assert conn.execute("select a, b from tpc_r order by a").fetchall() == [(1, 11), (2, 20)]
        assert conn.execute("select x from tpc_made").fetchall() == [(9,)]
        assert conn.execute("select count(*) from pg_prepared_xacts").fetchone() == (0,)

    # And the resolution itself is durable.
    with _Server(home) as server, server.connect() as conn:
        assert conn.execute("select a, b from tpc_r order by a").fetchall() == [(1, 11), (2, 20)]
        assert conn.execute("select x from tpc_made").fetchall() == [(9,)]
        assert conn.execute("select count(*) from pg_prepared_xacts").fetchone() == (0,)


def test_a_recovered_prepared_transaction_still_holds_its_rows(home: Path) -> None:
    """After a restart a prepared transaction keeps its rows, as PostgreSQL's does.

    It used to be RECORDED but not live, so it held nothing: another session
    updated the row and committed, then COMMIT PREPARED replayed the old write
    over it and the committed value was silently lost. Now the writer waits
    for the transaction (55P03 under lock_timeout) and the commit is the only
    write.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table tpc_hold (id int primary key, n int)")
        conn.execute("insert into tpc_hold values (1, 0), (2, 0)")
        conn.execute("begin")
        conn.execute("update tpc_hold set n = 5 where id = 1")
        conn.execute("delete from tpc_hold where id = 2")
        conn.execute("prepare transaction 'p-hold'")

    for _ in range(2):  # revived on every open, not just the first
        with _Server(home) as server, server.connect() as conn:
            assert conn.execute("select id, n from tpc_hold order by id").fetchall() == [
                (1, 0),
                (2, 0),
            ]
            conn.execute("set lock_timeout = '200ms'")
            assert _sqlstate(conn, "update tpc_hold set n = 7 where id = 1") == "55P03"
            assert _sqlstate(conn, "update tpc_hold set n = 7 where id = 2") == "55P03"

    with _Server(home) as server, server.connect() as conn:
        conn.execute("commit prepared 'p-hold'")
        assert conn.execute("select id, n from tpc_hold order by id").fetchall() == [(1, 5)]
        conn.execute("update tpc_hold set n = 6 where id = 1")
        assert conn.execute("select n from tpc_hold").fetchall() == [(6,)]


def test_a_blocks_earlier_write_survives_a_later_conflict(home: Path) -> None:
    """A write that loses a conflict never takes the block's earlier writes with it.

    The retry on a fresh transaction is only invisible for a transaction that
    has not written. The check read a flag the snapshot refresh sets, BEFORE
    the refresh ran, so a block's second UPDATE that waited on another
    session rolled back and retried -- and the block's first UPDATE vanished
    while both statements and the COMMIT reported success. The block now
    fails whole (40001) and nothing it wrote is half-kept.
    """
    with _Server(home) as server, server.connect() as a, server.connect() as b:
        a.execute("create table tq_c (id int primary key, n int)")
        a.execute("insert into tq_c values (1, 0), (2, 0)")
        b.execute("begin")
        b.execute("update tq_c set n = n + 1 where id = 2")
        a.execute("begin")
        a.execute("update tq_c set n = n + 1 where id = 1")
        outcome: dict[str, str] = {}

        def second_write() -> None:
            outcome["b"] = _sqlstate(b, "update tq_c set n = n + 10 where id = 1") or "ok"

        t = threading.Thread(target=second_write)
        t.start()
        time.sleep(0.5)
        a.execute("commit")
        t.join(10)
        b.execute("commit")
        rows = a.execute("select id, n from tq_c order by id").fetchall()
        # Either the whole block (PostgreSQL waits and applies both) or
        # none of it -- never its second write without its first.
        assert (outcome["b"], rows) in [
            ("ok", [(1, 11), (2, 1)]),
            ("40001", [(1, 1), (2, 0)]),
        ]


def test_prepared_gid_is_byte_exact(home: Path) -> None:
    """A gid is an arbitrary string up to 199 bytes: quotes, unicode and the
    empty string all round-trip through pg_prepared_xacts unchanged, and the
    200-byte one is `22023`."""
    from psycopg import sql

    gids = ["", "it's", 'say "hi"', "üñíçødé", "a" * 199]
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table tpc_g (a int)")
        for gid in gids:
            conn.execute("begin")
            conn.execute("insert into tpc_g values (1)")
            conn.execute(sql.SQL("prepare transaction {}").format(sql.Literal(gid)))
        listed = conn.execute("select gid from pg_prepared_xacts order by gid").fetchall()
        assert sorted(g for (g,) in listed) == sorted(gids)
        conn.execute("begin")
        conn.execute("insert into tpc_g values (1)")
        with pytest.raises(psycopg.Error) as info:
            conn.execute(sql.SQL("prepare transaction {}").format(sql.Literal("a" * 200)))
        assert info.value.sqlstate == "22023"
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.IDLE
        for gid in gids:
            conn.execute(sql.SQL("commit prepared {}").format(sql.Literal(gid)))
        assert conn.execute("select count(*) from tpc_g").fetchone() == (len(gids),)


def test_two_phase_commit_refusals_match_postgres(home: Path) -> None:
    """The error surface, as PostgreSQL 16 answers it."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table tpc_e (a int)")
        conn.execute("begin")
        conn.execute("insert into tpc_e values (1)")
        conn.execute("prepare transaction 'dup'")

        # PREPARE outside a block: a WARNING and a ROLLBACK tag, no error.
        notices: list[tuple[str, str]] = []
        conn.add_notice_handler(lambda d: notices.append((d.severity, d.sqlstate)))
        cur = conn.execute("prepare transaction 'nowhere'")
        assert cur.statusmessage == "ROLLBACK"
        assert notices == [("WARNING", "25P01")]

        # Every PREPARE failure ends the block.
        conn.execute("begin")
        conn.execute("insert into tpc_e values (2)")
        assert _sqlstate(conn, "prepare transaction 'dup'") == "42710"
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.IDLE
        assert conn.execute("select count(*) from tpc_e").fetchone() == (0,)

        # PREPARE in a failed block is a plain ROLLBACK.
        conn.execute("begin")
        assert _sqlstate(conn, "select nosuch") == "42703"
        assert conn.execute("prepare transaction 'failed'").statusmessage == "ROLLBACK"
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.IDLE

        # Temporary tables, holdable cursors and LISTEN/NOTIFY are 0A000.
        for body in (
            "create temp table tpc_tmp (x int)",
            "declare tpc_c cursor with hold for select 1",
            "listen tpc_chan",
            "notify tpc_chan",
        ):
            conn.execute("begin")
            conn.execute(body)
            assert _sqlstate(conn, "prepare transaction 'feature'") == "0A000", body
            assert conn.info.transaction_status == psycopg.pq.TransactionStatus.IDLE
        # ... but a plain (non-holdable) cursor is allowed.
        conn.execute("begin")
        conn.execute("declare tpc_plain cursor for select 1")
        conn.execute("prepare transaction 'cursor-ok'")
        conn.execute("rollback prepared 'cursor-ok'")

        # COMMIT / ROLLBACK PREPARED inside a block fail the block (25001).
        conn.execute("begin")
        assert _sqlstate(conn, "commit prepared 'dup'") == "25001"
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.INERROR
        conn.execute("rollback")

        # An unknown gid is 42704, and resolving one twice is the same.
        assert _sqlstate(conn, "commit prepared 'nope'") == "42704"
        conn.execute("commit prepared 'dup'")
        assert _sqlstate(conn, "rollback prepared 'dup'") == "42704"
        assert conn.execute("select a from tpc_e").fetchall() == [(1,)]


def test_prepared_ddl_is_invisible_until_committed(home: Path) -> None:
    """A table created in a prepared transaction does not exist for anyone
    until COMMIT PREPARED, and ROLLBACK PREPARED makes it never have existed."""
    with _Server(home) as server:
        a = server.connect()
        b = server.connect()
        try:
            a.execute("begin")
            a.execute("create table tpc_ddl (x int)")
            a.execute("insert into tpc_ddl values (1)")
            a.execute("prepare transaction 'ddl'")
            assert _sqlstate(a, "select * from tpc_ddl") == "42P01"
            assert _sqlstate(b, "select * from tpc_ddl") == "42P01"
            b.execute("commit prepared 'ddl'")
            assert a.execute("select x from tpc_ddl").fetchall() == [(1,)]

            a.execute("begin")
            a.execute("create table tpc_gone (x int)")
            a.execute("prepare transaction 'gone'")
            b.execute("rollback prepared 'gone'")
            assert _sqlstate(a, "select * from tpc_gone") == "42P01"
            # The name is free again.
            a.execute("create table tpc_gone (y int)")
        finally:
            a.close()
            b.close()


def test_truncate_matches_postgres(home: Path) -> None:
    """`TRUNCATE` empties tables, refuses an FK parent without CASCADE
    (0A000, PostgreSQL's detail and hint), cascades with a NOTICE per table,
    and RESTART IDENTITY rewinds the serials."""
    with _Server(home) as server, server.connect() as conn:
        notices: list[str] = []
        conn.add_notice_handler(lambda d: notices.append(d.message_primary))
        conn.execute("create table tp (id serial primary key, n int)")
        conn.execute("create table tc (id int primary key, p int references tp(id))")
        conn.execute("insert into tp (n) values (1), (2)")
        conn.execute("insert into tc values (1, 1)")
        assert _sqlstate(conn, "truncate nosuch") == "42P01"

        with pytest.raises(psycopg.Error) as info:
            conn.execute("truncate tp")
        assert info.value.sqlstate == "0A000"
        assert info.value.diag.message_primary == (
            "cannot truncate a table referenced in a foreign key constraint"
        )
        assert info.value.diag.message_detail == 'Table "tc" references "tp".'
        assert info.value.diag.message_hint == (
            'Truncate table "tc" at the same time, or use TRUNCATE ... CASCADE.'
        )
        assert conn.execute("select count(*) from tp").fetchone() == (2,)

        # Both at once is fine, and the tag is TRUNCATE TABLE.
        assert conn.execute("truncate table tp, tc").statusmessage == "TRUNCATE TABLE"
        assert conn.execute("select count(*) from tp").fetchone() == (0,)
        assert conn.execute("select count(*) from tc").fetchone() == (0,)
        # The serial carries on where it was ...
        conn.execute("insert into tp (n) values (3)")
        assert conn.execute("select id from tp").fetchall() == [(3,)]
        # ... unless RESTART IDENTITY rewinds it.
        conn.execute("truncate tp restart identity cascade")
        conn.execute("insert into tp (n) values (4)")
        assert conn.execute("select id from tp").fetchall() == [(1,)]

        assert notices == ['truncate cascades to table "tc"']

        conn.execute("insert into tc values (7, 1)")
        notices.clear()
        assert conn.execute("truncate tp cascade").statusmessage == "TRUNCATE TABLE"
        assert notices == ['truncate cascades to table "tc"']
        assert conn.execute("select count(*) from tc").fetchone() == (0,)


def test_create_extension_hstore_installs_the_type_and_its_io(home: Path) -> None:
    """`CREATE EXTENSION hstore` brings the `hstore` type with PostgreSQL's
    text I/O and binary send/recv, a `pg_extension` row, and refusals shaped
    as 16's: an unknown extension is 0A000, a repeat 42710 (a NOTICE under IF
    NOT EXISTS), the type cannot be dropped on its own (2BP01, "extension
    hstore requires it"), and the extension cannot be dropped while a column
    uses it (2BP01 with the column in DETAIL). All measured on 16 / hstore 1.8.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        notices = _notices(conn)
        with pytest.raises(psycopg.errors.FeatureNotSupported) as exc:
            cur.execute("create extension nosuch")
        assert _diag(exc.value)[:2] == ("0A000", 'extension "nosuch" is not available')
        cur.execute("create extension hstore")
        assert cur.statusmessage == "CREATE EXTENSION"
        with pytest.raises(psycopg.errors.DuplicateObject) as exc:
            cur.execute("create extension hstore")
        assert _diag(exc.value)[:2] == ("42710", 'extension "hstore" already exists')
        cur.execute("create extension if not exists hstore")
        assert notices[-1][2] == 'extension "hstore" already exists, skipping'
        cur.execute("select extname, extversion from pg_extension order by extname")
        assert cur.fetchall() == [("hstore", "1.8"), ("plpgsql", "1.0")]

        # Text I/O: PostgreSQL's canonical `"k"=>"v"` rendering, duplicate
        # keys resolved first-wins, keys ordered by length then bytes.
        cur.execute(
            "select null::hstore, ''::hstore, 'a => b'::hstore,"
            """ 'bb=>1, a=>2, a=>3, "x y"=>NULL'::hstore"""
        )
        assert cur.fetchone() == (None, "", '"a"=>"b"', '"a"=>"2", "bb"=>"1", "x y"=>NULL')
        with pytest.raises(psycopg.errors.SyntaxError) as exc:
            cur.execute("select 'a=>'::hstore")
        assert _diag(exc.value)[1] == "syntax error in hstore: unexpected end of string"

        # psycopg's own adapter over TypeInfo.fetch, in both formats.
        info = psycopg.types.TypeInfo.fetch(conn, "hstore")
        assert info.name == "hstore"
        assert info.array_oid == info.oid + 100_000
        from psycopg.types.hstore import register_hstore

        register_hstore(info, conn)
        for fmt in (psycopg.pq.Format.TEXT, psycopg.pq.Format.BINARY):
            c = conn.cursor(binary=fmt)
            sample = {"a": "1", "b": None, "c d": '"q"'}
            c.execute("select %s, %s", (sample, [sample, {}]))
            assert c.fetchone() == (sample, [sample, {}])
            c.execute("select pg_typeof(%s)::text", (sample,))
            assert c.fetchone() == ("hstore",)
        cur.execute("select hstore('k', 'v'), akeys('b=>1, a=>2'), 'a=>1'::hstore -> 'a'")
        assert cur.fetchone() == ('"k"=>"v"', ["a", "b"], "1")
        # The operators, measured against PostgreSQL 16 / hstore 1.8. A bare
        # literal beside an hstore IS an hstore (`h - 'a=>1'`), a key needs
        # `::text`.
        cur.execute("create table hs_ops (h hstore)")
        cur.execute("insert into hs_ops values ('a=>1, b=>NULL')")
        cur.execute(
            "select h ? 'a', h ?| array['zz','b'], h ?& array['a','zz'], h @> 'a=>1',"
            " 'a=>1'::hstore <@ h, h - array['a'], h - 'a=>1'::hstore, h - 'b=>9'::hstore,"
            " h - 'a'::text, h || 'c=>3, a=>7', h -> array['b','zz'],"
            " pg_typeof(h - 'a'::text)::text, pg_typeof(h || h)::text, h - 'a=>1',"
            " h -> 'zz', h -> 'a'"
            " from hs_ops"
        )
        assert cur.fetchone() == (
            True,
            True,
            False,
            True,
            True,
            '"b"=>NULL',
            '"b"=>NULL',
            '"a"=>"1", "b"=>NULL',
            '"b"=>NULL',
            '"a"=>"7", "b"=>NULL, "c"=>"3"',
            [None, None],
            "hstore",
            "hstore",
            '"b"=>NULL',
            None,
            "1",
        )
        cur.execute("drop table hs_ops")

        with pytest.raises(psycopg.errors.DependentObjectsStillExist) as exc:
            cur.execute("drop type hstore")
        assert _diag(exc.value)[:2] == (
            "2BP01",
            "cannot drop type hstore because extension hstore requires it",
        )
        cur.execute("create table hs_t (h hstore)")
        cur.execute("""insert into hs_t values ('x=>y')""")
        with pytest.raises(psycopg.errors.DependentObjectsStillExist) as exc:
            cur.execute("drop extension hstore")
        assert _diag(exc.value)[:2] == (
            "2BP01",
            "cannot drop extension hstore because other objects depend on it",
        )
        assert exc.value.diag.message_detail == "column h of table hs_t depends on type hstore"
        cur.execute("drop table hs_t")
        cur.execute("drop extension hstore")
        assert cur.statusmessage == "DROP EXTENSION"
        with pytest.raises(psycopg.errors.UndefinedObject) as exc:
            cur.execute("drop extension hstore")
        assert _diag(exc.value)[:2] == ("42704", 'extension "hstore" does not exist')
        cur.execute("drop extension if exists hstore")
        assert notices[-1][2] == 'extension "hstore" does not exist, skipping'
        # Without the extension the type and its functions are gone.
        with pytest.raises(psycopg.errors.Error):
            cur.execute("select 'a=>b'::hstore")
        with pytest.raises(psycopg.errors.Error):
            cur.execute("select hstore('a', 'b')")


def test_create_extension_postgis_brings_geometry_as_ewkb(home: Path) -> None:
    """`CREATE EXTENSION postgis` brings `geometry`: hex-EWKB text (what
    PostGIS 3.4.6 renders), EWKB binary in both directions, WKT / EWKT input,
    `ST_GeomFromGeoJSON` (SRID 4326 unless the JSON names a CRS), `ST_AsText`
    / `ST_AsEWKT` / `ST_SRID` / `ST_GeomFromText`, and a geometry column that
    round-trips through a table. Storage is the EWKB, opaque to the planner.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("create extension postgis")
        cur.execute("select typname, typdelim from pg_type where typname = 'geometry'")
        assert cur.fetchone() == ("geometry", ":")
        cur.execute(
            "select 'POINT(1 2)'::geometry, 'SRID=4326;POINT(1 2)'::geometry, "
            "ST_AsText('0101000000000000000000F03F0000000000000040'::geometry), "
            "ST_AsEWKT(ST_GeomFromText('LINESTRING(0 0, 1 1)', 4326)), "
            'ST_SRID(ST_GeomFromGeoJSON(\'{"type":"Point","coordinates":[1,2]}\')), '
            "ST_AsEWKT(ST_GeomFromGeoJSON("
            '\'{"type":"MultiPolygon","coordinates":[[[[0,0],[1,0],[1,1],[0,0]]]]}\')), '
            "pg_typeof('POINT(1 2)'::geometry)::text"
        )
        assert cur.fetchone() == (
            "0101000000000000000000F03F0000000000000040",
            "0101000020E6100000000000000000F03F0000000000000040",
            "POINT(1 2)",
            "SRID=4326;LINESTRING(0 0,1 1)",
            4326,
            "SRID=4326;MULTIPOLYGON(((0 0,1 0,1 1,0 0)))",
            "geometry",
        )
        with pytest.raises(psycopg.errors.InternalError_) as exc:
            cur.execute("select 'POINT(1)'::geometry")
        assert _diag(exc.value)[:2] == ("XX000", "parse error - invalid geometry")

        cur.execute("create table sample_geoms (id serial primary key, geom geometry)")
        cur.execute("insert into sample_geoms (geom) values ('POINT(0 0)'), (null)")
        info = psycopg.types.TypeInfo.fetch(conn, "geometry")
        assert info.name == "geometry"
        from psycopg.types.shapely import register_shapely
        from shapely.geometry import Point

        register_shapely(info, conn)
        for fmt in (psycopg.pq.Format.TEXT, psycopg.pq.Format.BINARY):
            c = conn.cursor(binary=fmt)
            c.execute(
                "insert into sample_geoms (geom) values (%s) returning geom", (Point(1.5, 2),)
            )
            assert c.fetchone()[0] == Point(1.5, 2)
            c.execute("select geom from sample_geoms order by id")
            rows = [r[0] for r in c.fetchall()]
            assert rows[0] == Point(0, 0)
            assert rows[1] is None
            assert rows[-1] == Point(1.5, 2)
        with pytest.raises(psycopg.errors.DependentObjectsStillExist) as exc:
            cur.execute("drop extension postgis")
        assert exc.value.diag.message_detail == (
            "column geom of table sample_geoms depends on type geometry"
        )
        cur.execute("drop table sample_geoms")
        cur.execute("drop extension postgis")
        cur.execute("select extname from pg_extension")
        assert cur.fetchall() == [("plpgsql",)]


def test_create_role_records_the_role_and_its_verifier(home: Path) -> None:
    """`CREATE / ALTER / DROP ROLE` (and their `USER` spellings) keep a
    cluster-wide role catalog: `pg_roles` / `pg_user` mask every password as
    `********`, `pg_authid` carries the SCRAM-SHA-256 verifier a plaintext
    `PASSWORD` derives (4096 iterations, as `password_encryption =
    scram-sha-256` does) or the verifier a client sent verbatim, `PASSWORD
    NULL` clears it, and the errors and notices are PostgreSQL 16's: 42710,
    42704 (and the IF EXISTS notice), 2BP01 for the bootstrap superuser,
    55006 for the session's own user, 22007 for a bad `VALID UNTIL`. A
    multi-name DROP is all or nothing. Passwords are recorded, never checked:
    every connection is still trusted.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        notices = _notices(conn)
        cur.execute("create user ashesh login password 'psycopg2'")
        assert cur.statusmessage == "CREATE ROLE"
        with pytest.raises(psycopg.errors.DuplicateObject) as exc:
            cur.execute("create role ashesh")
        assert _diag(exc.value)[:2] == ("42710", 'role "ashesh" already exists')
        cur.execute(
            "create role r2 superuser createdb createrole noinherit"
            " connection limit 3 valid until '2030-01-01'"
        )
        cur.execute(
            "select oid, rolname, rolsuper, rolinherit, rolcreaterole, rolcreatedb,"
            " rolcanlogin, rolreplication, rolconnlimit, rolpassword, rolvaliduntil::text,"
            " rolbypassrls, rolconfig from pg_roles order by oid"
        )
        rows = cur.fetchall()
        assert rows[0] == (
            10,
            "postgres",
            True,
            True,
            True,
            True,
            True,
            True,
            -1,
            "********",
            None,
            True,
            None,
        )
        assert [r[1:] for r in rows[1:]] == [
            ("ashesh", False, True, False, False, True, False, -1, "********", None, False, None),
            (
                "r2",
                True,
                False,
                True,
                True,
                False,
                False,
                3,
                "********",
                "2030-01-01 00:00:00+00",
                False,
                None,
            ),
        ]
        assert rows[1][0] > 16384 and rows[2][0] > rows[1][0]
        cur.execute("select usename, usesuper, passwd from pg_user order by usesysid")
        assert cur.fetchall() == [("postgres", True, "********"), ("ashesh", False, "********")]

        cur.execute("select rolpassword from pg_authid where rolname = 'ashesh'")
        verifier = cur.fetchone()[0]
        assert verifier.startswith("SCRAM-SHA-256$4096:")
        cur.execute("alter user ashesh password NULL")
        assert cur.statusmessage == "ALTER ROLE"
        cur.execute("select rolpassword from pg_authid where rolname = 'ashesh'")
        assert cur.fetchone() == (None,)
        # What libpq's PQchangePassword sends: a client-side verifier, kept.
        # It is inlined as a literal -- `PASSWORD $1` is a syntax error on
        # PostgreSQL (gram.y takes only a string constant there).
        sent = "SCRAM-SHA-256$4096:c2FsdA==$c3RvcmVk:c2VydmVy"
        with pytest.raises(psycopg.errors.SyntaxError) as exc:
            cur.execute("alter user ashesh password %s", (sent,))
        assert _diag(exc.value)[:2] == ("42601", 'syntax error at or near "$1"')
        from psycopg import sql

        cur.execute(sql.SQL("alter user ashesh password {}").format(sql.Literal(sent)))
        cur.execute("select rolpassword from pg_authid where rolname = 'ashesh'")
        assert cur.fetchone() == (sent,)
        cur.execute("create role emptypw password ''")
        assert notices[-1][2] == "empty string is not a valid password, clearing password"
        cur.execute("alter role r2 valid until 'infinity' nocreatedb")
        cur.execute("select rolvaliduntil::text, rolcreatedb from pg_roles where rolname = 'r2'")
        assert cur.fetchone() == ("infinity", False)
        with pytest.raises(psycopg.errors.InvalidDatetimeFormat) as exc:
            cur.execute("create role badvu valid until 'nonsense'")
        assert _diag(exc.value)[:2] == (
            "22007",
            'invalid input syntax for type timestamp with time zone: "nonsense"',
        )

        with pytest.raises(psycopg.errors.UndefinedObject) as exc:
            cur.execute("alter user nosuch password 'x'")
        assert _diag(exc.value)[:2] == ("42704", 'role "nosuch" does not exist')
        with pytest.raises(psycopg.errors.UndefinedObject):
            cur.execute("drop user nosuch")
        cur.execute("drop user if exists nosuch")
        assert notices[-1][2] == 'role "nosuch" does not exist, skipping'
        with pytest.raises(psycopg.errors.DependentObjectsStillExist) as exc:
            cur.execute("drop role postgres")
        assert _diag(exc.value)[:2] == (
            "2BP01",
            "cannot drop role postgres because it is required by the database system",
        )
        # The session's own user exists (it connected) even with no record.
        cur.execute("alter user test password 'x'")
        with pytest.raises(psycopg.errors.ObjectInUse) as exc:
            cur.execute("drop role test")
        assert _diag(exc.value)[:2] == ("55006", "current user cannot be dropped")
        with pytest.raises(psycopg.errors.UndefinedObject):
            cur.execute("drop role ashesh, nosuch")
        cur.execute("select count(*) from pg_roles where rolname = 'ashesh'")
        assert cur.fetchone() == (1,)
        cur.execute("drop role ashesh, r2, emptypw")
        assert cur.statusmessage == "DROP ROLE"
        # Another session's user can drop it: 55006 is about the CURRENT user.
        with psycopg.connect(
            f"host=127.0.0.1 port={server.port} dbname=postgres user=postgres",
            autocommit=True,
        ) as other:
            other.execute("drop role test")
        cur.execute("select rolname from pg_roles")
        assert cur.fetchall() == [("postgres",)]


def test_catalog_collections_are_created_in_a_store_that_has_none(home: Path) -> None:
    """A brand-new store must still get its catalog collections created.

    `ensure_collection` caches its verdict per connection, so the seven
    `CATALOG_COLLECTIONS` are probed once rather than before every statement
    (measured 2026-09-18: that probing was ~20us of a 54.5us per-statement
    overhead -- seven fresh WiredTiger sessions, each opening and closing a
    cursor). The cache must never let the FIRST caller skip the creation: a
    catalog write against a collection nobody created is a WiredTiger ENOENT,
    not a no-op. The store here is empty, so every write below lands on a
    catalog that did not exist when the connection opened.
    """
    with _Server(home) as server:
        conn = server.connect()
        cur = conn.cursor()

        # A table writes CATALOG_COLLECTION -- the one catalog write with no
        # `ensure_collection` of its own beside it, so the blanket ensure in
        # `open_transaction_handle` is what it relies on.
        cur.execute("create table fresh_t (k int primary key, v text)")
        cur.execute("insert into fresh_t values (1, 'a')")
        assert cur.execute("select v from fresh_t where k = 1").fetchone() == ("a",)

        # A serial column writes SEQUENCE_COLLECTION.
        cur.execute("create table fresh_s (id serial primary key, v text)")
        cur.execute("insert into fresh_s (v) values ('x')")
        assert cur.execute("select id from fresh_s").fetchall() == [(1,)]

        # A composite type writes COMPOSITE_COLLECTION + ENUM_META_COLLECTION.
        cur.execute("create type fresh_c as (a int, b text)")
        assert cur.execute("select '(1,z)'::fresh_c").fetchone() is not None

        # A schema writes SCHEMA_COLLECTION.
        cur.execute("create schema fresh_sch")
        cur.execute("create table fresh_sch.t (k int)")
        cur.execute("insert into fresh_sch.t values (7)")
        assert cur.execute("select k from fresh_sch.t").fetchall() == [(7,)]


def test_a_second_connection_re_probes_the_catalog(home: Path) -> None:
    """The cache is per connection, so a second connection must re-probe rather
    than inherit a verdict -- and must find what the first one created."""
    with _Server(home) as server:
        first = server.connect()
        first.execute("create table shared_t (k int primary key)")
        first.execute("insert into shared_t values (1)")

        second = server.connect()
        assert second.execute("select k from shared_t").fetchall() == [(1,)]
        second.execute("insert into shared_t values (2)")
        assert second.execute("select count(*) from shared_t").fetchone() == (2,)


# ---------------------------------------------------------------------------
# UNIQUE constraints. Before 2026-09-18 a `unique` column constraint planned as
# a plain column: the server ACCEPTED the declaration and then let a duplicate
# in, which is silent data corruption rather than a missing feature. The error
# surface below was probed against PostgreSQL 14.13, not copied from the
# backlog entry, which cited a PG 16 that is not installed on this box.
# ---------------------------------------------------------------------------


def test_column_unique_rejects_a_duplicate(home: Path) -> None:
    """The headline: the second equal value must not be stored.

    PostgreSQL 14.13 answers 23505 naming the constraint it generated,
    `<table>_<column>_key`, with the offending value in DETAIL.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE u1 (id int PRIMARY KEY, tag text UNIQUE)")
        cur.execute("INSERT INTO u1 VALUES (1,'dup')")

        with pytest.raises(psycopg.errors.UniqueViolation) as exc:
            cur.execute("INSERT INTO u1 VALUES (2,'dup')")
        diag = exc.value.diag
        assert diag.sqlstate == "23505"
        assert diag.constraint_name == "u1_tag_key"
        assert diag.message_primary == (
            'duplicate key value violates unique constraint "u1_tag_key"'
        )
        assert diag.message_detail == "Key (tag)=(dup) already exists."

        # ... and the row really is not there.
        cur.execute("SELECT count(*) FROM u1")
        assert cur.fetchone() == (1,)


def test_unique_allows_many_nulls(home: Path) -> None:
    """SQL NULLs are DISTINCT, so any number of them satisfy a UNIQUE.

    This is the case a Mongo unique index gets wrong by default, and the reason
    the backing index carries a partial filter rather than `sparse`: a SQL NULL
    is stored as an explicit null, not a missing field, so a sparse index would
    still index it and still collide.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE u2 (id int PRIMARY KEY, tag text UNIQUE)")
        cur.execute("INSERT INTO u2 VALUES (1, NULL), (2, NULL), (3, NULL)")
        cur.execute("SELECT count(*) FROM u2")
        assert cur.fetchone() == (3,)


def test_multi_column_unique_and_its_generated_name(home: Path) -> None:
    """`UNIQUE (a,b)` -> `<table>_a_b_key`, and only the whole tuple collides."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE u3 (a int, b int, UNIQUE (a,b))")
        cur.execute("INSERT INTO u3 VALUES (1,2)")
        cur.execute("INSERT INTO u3 VALUES (1,3)")  # same a, different b: fine

        with pytest.raises(psycopg.errors.UniqueViolation) as exc:
            cur.execute("INSERT INTO u3 VALUES (1,2)")
        assert exc.value.diag.constraint_name == "u3_a_b_key"
        assert exc.value.diag.message_detail == "Key (a, b)=(1, 2) already exists."


def test_named_unique_constraint_keeps_its_name(home: Path) -> None:
    """`CONSTRAINT my_uq UNIQUE` reports `my_uq`, not a generated name."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE u4 (a int CONSTRAINT my_uq UNIQUE)")
        cur.execute("INSERT INTO u4 VALUES (7)")
        with pytest.raises(psycopg.errors.UniqueViolation) as exc:
            cur.execute("INSERT INTO u4 VALUES (7)")
        assert exc.value.diag.constraint_name == "my_uq"


def test_update_into_a_duplicate_is_rejected(home: Path) -> None:
    """Enforcement is on every write, not just INSERT."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE u5 (id int PRIMARY KEY, tag text UNIQUE)")
        cur.execute("INSERT INTO u5 VALUES (1,'a'), (2,'b')")
        with pytest.raises(psycopg.errors.UniqueViolation):
            cur.execute("UPDATE u5 SET tag='a' WHERE id=2")
        cur.execute("SELECT tag FROM u5 WHERE id=2")
        assert cur.fetchone() == ("b",)


def test_the_python_server_sees_the_unique_constraint(home: Path) -> None:
    """The catalog shape is a cross-server contract.

    A table the Rust server created with a UNIQUE must read back in the Python
    server with that constraint intact — dropping the key would silently
    rewrite the other server's catalog, and a later duplicate would land.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("CREATE TABLE u6 (id int PRIMARY KEY, tag text UNIQUE)")

    from secantus.sql.catalog import Catalog
    from secantus.storage import Storage

    storage = Storage(str(home))
    try:
        table = Catalog(storage).get("postgres", "u6")
        assert table is not None
        assert [(u.name, tuple(u.columns)) for u in table.unique_constraints] == [
            ("u6_tag_key", ("tag",))
        ]
    finally:
        storage.close()


# Cross-PROTOCOL contract: the Rust PG server and the Rust MongoDB server over
# one store. Measured 2026-09-18; before that it was asserted on the website and
# in CLAUDE.md on the strength of "same secantus-storage, so it must follow",
# which is reasoning rather than evidence. It holds in one direction only, and
# the asymmetry is the whole reason these two tests exist.
# ---------------------------------------------------------------------------

_MONGO_BANNER = re.compile(r"secantusd-rs listening on (\S+):(\d+)")


def _mongo_binary() -> Path | None:
    """The standalone `secantusd-rs`, the way test_rust_binary_smoke finds it."""
    import os

    env = os.environ.get("SECANTUSDB_BIN")
    if env:
        p = Path(env)
        return p if p.exists() else None
    for profile in ("release", "debug"):
        p = REPO / "crates" / "secantusdb" / "target" / profile / "secantusd-rs"
        if p.exists():
            return p
    return None


_MONGO_BIN = _mongo_binary()
_needs_mongo_binary = pytest.mark.skipif(
    _MONGO_BIN is None,
    reason=(
        "secantusd-rs not built (cargo build --manifest-path "
        "crates/secantusdb/Cargo.toml, or set SECANTUSDB_BIN)"
    ),
)


@contextlib.contextmanager
def _mongo_server(home: Path) -> Iterator[tuple[str, int]]:
    """Serve `home` over the MongoDB wire. Nothing else may hold it."""
    assert _MONGO_BIN is not None
    proc = subprocess.Popen(
        [str(_MONGO_BIN), "--port", "0", "--storage-path", str(home)],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        assert proc.stdout is not None
        line = proc.stdout.readline()
        m = _MONGO_BANNER.search(line)
        assert m, f"no listening banner in first stdout line: {line!r}"
        yield m.group(1), int(m.group(2))
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


@_needs_mongo_binary
def test_a_mongodb_client_reads_a_table_the_rust_sql_server_wrote(home: Path) -> None:
    """SQL in, BSON out: one store, both Rust servers, sequentially.

    A table written over the PostgreSQL wire is a collection over the MongoDB
    wire, in the database the SQL session was connected to, with the PRIMARY KEY
    landing as `_id`. Measured 2026-09-18 -- the shape below is what a pymongo
    client actually saw, not what the storage layer was assumed to imply.

    Sequential by necessity: WiredTiger's exclusive lock means the PG server has
    to stop before the MongoDB server can open the same home (see the `home`
    fixture, and the companion test below).
    """
    pymongo = pytest.importorskip("pymongo")

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE widgets (id int PRIMARY KEY, name text, qty int)")
        cur.execute("INSERT INTO widgets VALUES (1,'bolt',40),(2,'nut',75)")

    with _mongo_server(home) as (host, port):
        client = pymongo.MongoClient(f"mongodb://{host}:{port}", serverSelectionTimeoutMS=15_000)
        try:
            assert "widgets" in client["postgres"].list_collection_names()
            docs = sorted(client["postgres"]["widgets"].find({}), key=lambda d: d["_id"])
            assert docs == [
                {"_id": 1, "name": "bolt", "qty": 40},
                {"_id": 2, "name": "nut", "qty": 75},
            ]
        finally:
            client.close()


@_needs_mongo_binary
def test_the_two_rust_servers_cannot_hold_one_store_at_the_same_time(home: Path) -> None:
    """The contract is SEQUENTIAL, and saying otherwise would mislead.

    WiredTiger takes an exclusive file lock, so "point both servers at one
    directory" is a hand-off, never concurrent serving. This is pinned because
    the claim is tempting to write on a marketing page -- it was, briefly.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("CREATE TABLE held (id int PRIMARY KEY)")

        assert _MONGO_BIN is not None
        clash = subprocess.run(
            [str(_MONGO_BIN), "--port", "0", "--storage-path", str(home)],
            capture_output=True,
            text=True,
            timeout=60,
        )
        assert clash.returncode != 0, "the MongoDB server opened a store the PG server holds"
        assert "WiredTiger.lock" in (clash.stdout + clash.stderr)


@_needs_mongo_binary
def test_a_pymongo_written_collection_is_not_yet_a_table(home: Path) -> None:
    """The reverse direction does NOT work on the Rust pair, unlike the Python one.

    The Python SQL layer samples an uncatalogued collection and serves it as a
    table (`docs/sql.md`); the Rust PG server resolves names through
    `__sql_catalog__`, which only `CREATE TABLE` writes, so a pymongo-written
    collection is invisible to it.

    If this test starts failing, schema inference has landed on the Rust side --
    that is an improvement, not a regression. Turn it into the positive
    assertion, and update the cross-protocol bullet in CLAUDE.md's Project
    section, which currently records this asymmetry.
    """
    pymongo = pytest.importorskip("pymongo")

    with _mongo_server(home) as (host, port):
        client = pymongo.MongoClient(f"mongodb://{host}:{port}", serverSelectionTimeoutMS=15_000)
        try:
            client["postgres"]["gadgets"].insert_many([{"_id": 1, "name": "cog"}])
        finally:
            client.close()

    with (
        _Server(home) as server,
        server.connect() as conn,
        pytest.raises(psycopg.errors.UndefinedTable),
    ):
        conn.cursor().execute("SELECT * FROM gadgets")


def test_uncommitted_types_stay_private_and_stay_current(home: Path) -> None:
    """A block's uncommitted types are its own, and its own view keeps up.

    Two invariants that one caching decision holds up between. The catalog is
    cached process-wide, and the planner's user-type tables are cached per
    thread, so both caches face the same question inside a transaction that
    has done DDL: whose view is this?

    - **Private.** A type created in an open block must be invisible to every
      other connection until COMMIT. The process-wide catalog cache may be
      filled only from a read that did NOT run on the block's own WiredTiger
      session, because that session sees the block's uncommitted writes.
    - **Current.** The block's own view must track its own later DDL. The
      planner's per-thread type tables are keyed by catalog version and by the
      session that owns an uncommitted overlay; a stale key would leave a
      dropped-and-recreated type resolving to its old definition.

    Before 2026-09-19 the second was paid for by republishing everything on
    every statement of the block and re-reading the catalog 20 times per
    statement (~115us each), because the version the gate compared against
    could never match again once the block's own DDL had bumped it.
    """
    with (
        _Server(home) as server,
        server.connect(autocommit=False) as writer,
        server.connect() as reader,
    ):
        w = writer.cursor()
        w.execute("CREATE TABLE anchor (id int PRIMARY KEY)")
        w.execute("CREATE TYPE mood AS ENUM ('ok')")
        w.execute("SELECT 'ok'::mood::text")
        assert w.fetchone() == ("ok",)

        # Private: the other connection cannot see it before COMMIT --
        # neither by name nor, the sharper question, as a CAST TARGET. The
        # planner's type tables are a THREAD-LOCAL, and two connections share
        # worker threads, so a view recorded without naming the session that
        # owns it leaks straight across: with the owner dropped from the key
        # this cast returned 'ok' on the first alternation while
        # `to_regtype` still (correctly) said the type did not exist.
        r = reader.cursor()
        r.execute("SELECT to_regtype('mood')")
        assert r.fetchone() == (None,)
        for _ in range(8):
            w.execute("SELECT 'ok'::mood::text")
            r.execute("SELECT to_regtype('mood')")
            assert r.fetchone() == (None,)
            # Any error will do: what matters is that the cast does NOT
            # resolve. (We answer `0A000 a cast to mood is not supported
            # yet` where PostgreSQL 16.15 answers `42704 type "mood" does
            # not exist` -- a separate fidelity gap, deliberately not pinned
            # here.)
            with pytest.raises(psycopg.Error):
                r.execute("SELECT 'ok'::mood")

        # Current: the block's own view follows its own later DDL.
        w.execute("DROP TYPE mood")
        w.execute("CREATE TYPE mood AS ENUM ('sad')")
        w.execute("SELECT 'sad'::mood::text")
        assert w.fetchone() == ("sad",)
        with pytest.raises(psycopg.errors.InvalidTextRepresentation):
            w.execute("SELECT 'ok'::mood")
        writer.rollback()

        # Still private after the rollback discarded it.
        r.execute("SELECT to_regtype('mood')")
        assert r.fetchone() == (None,)

        # And published on COMMIT.
        w = writer.cursor()
        w.execute("CREATE TYPE mood AS ENUM ('fine')")
        writer.commit()
        r.execute("SELECT to_regtype('mood')::text")
        assert r.fetchone() == ("mood",)


def test_an_open_blocks_uncommitted_type_is_invisible_to_other_connections(
    home: Path,
) -> None:
    """The isolation `may_fill_catalog_cache` is written to defend.

    That gate refuses to publish a catalog read taken on the transaction's own
    WiredTiger session, because such a read can see the block's uncommitted
    writes and the cache it would fill is process-wide. Measured 2026-09-19,
    the gate never actually fires -- catalog reads happen while planning,
    outside the user transaction -- so forcing it open leaks nothing, and the
    property below is held up by the per-connection `uncommitted_types`
    overlay instead.

    Which is exactly why this test exists. The gate is unfalsifiable on its
    own; the BEHAVIOUR it protects is not. If some future change routes a
    catalog read through the transaction's session, the `debug_assert` in that
    gate fires first, and if the assert is ever removed this test is what
    still notices.
    """
    with _Server(home) as server:
        writer = server.connect(autocommit=False)
        w = writer.cursor()
        w.execute("CREATE TYPE mood AS ENUM ('ok')")

        # The writer's own view: its uncommitted type resolves, both as a cast
        # and in the catalog.
        assert w.execute("SELECT 'ok'::mood").fetchone() == ("ok",)
        assert w.execute("SELECT typname FROM pg_type WHERE typname = 'mood'").fetchall() == [
            ("mood",)
        ]

        # Every other connection must see nothing of it until COMMIT.
        reader = server.connect()
        assert reader.execute("SELECT typname FROM pg_type WHERE typname = 'mood'").fetchall() == []
        with pytest.raises(psycopg.errors.Error):
            reader.execute("SELECT 'ok'::mood")

        writer.commit()

        # And see it immediately afterwards -- the cache must not have pinned
        # the pre-commit answer either.
        fresh = server.connect()
        assert fresh.execute("SELECT typname FROM pg_type WHERE typname = 'mood'").fetchall() == [
            ("mood",)
        ]
        assert fresh.execute("SELECT 'ok'::mood").fetchone() == ("ok",)


def test_a_rolled_back_type_never_becomes_visible(home: Path) -> None:
    """The same property on the failure path: ROLLBACK must leave no trace.

    A cache filled from the block's own session would survive the rollback and
    hand every later connection a type that no longer exists.
    """
    with _Server(home) as server:
        writer = server.connect(autocommit=False)
        w = writer.cursor()
        w.execute("CREATE TYPE ghost AS ENUM ('x')")
        assert w.execute("SELECT 'x'::ghost").fetchone() == ("x",)
        writer.rollback()

        after = server.connect()
        assert after.execute("SELECT typname FROM pg_type WHERE typname = 'ghost'").fetchall() == []
        with pytest.raises(psycopg.errors.Error):
            after.execute("SELECT 'x'::ghost")


def test_group_by_a_numeric_groups_on_the_value(home: Path) -> None:
    """A `numeric` carries its display scale, so `1.5` and `1.50` are different
    Decimal128s and a value past 34 significant digits is a DOCUMENT holding
    its text -- `1e40.0` and `1e40.00` differ there too. Grouping compared
    those stored forms, so PostgreSQL's three groups came back as seven.

    Expected values measured against PostgreSQL 14.24 (2026-09-20), including
    the printed text: a group shows its FIRST row, and `sum` keeps the widest
    scale of its inputs.
    """
    wide = "1234567890123456789012345678901234567890"
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int, v numeric)")
        conn.execute(
            "insert into t values "
            f"(1, {wide}), (2, {wide}.0), (3, {wide}.00), "
            "(4, 1.5), (5, 1.50), (6, 2), (7, 2.000)"
        )
        rows = conn.execute("select v, count(*), sum(v) from t group by v order by v").fetchall()
    assert [(str(v), n, str(s)) for v, n, s in rows] == [
        ("1.5", 2, "3.00"),
        ("2", 2, "4.000"),
        (wide, 3, str(int(wide) * 3) + ".00"),
    ]


def _rows(conn, sql: str) -> list[tuple]:
    return [
        tuple(str(v) if v is not None else None for v in r) for r in conn.execute(sql).fetchall()
    ]


def test_select_distinct_dedups(home: Path) -> None:
    """`SELECT DISTINCT` was IGNORED -- the duplicates came straight through.

    Values measured against PostgreSQL 14.24 (2026-09-20). `1.5` and `1.50`
    are ONE value to Postgres, and NULLs are one group.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int, s text, v numeric, n int)")
        conn.execute(
            "insert into t values (1,'a',1.5,10),(2,'a',1.50,10),"
            "(3,'b',2,null),(4,'b',2.000,null),(5,null,3,7),(6,null,3,7)"
        )
        assert _rows(conn, "select distinct s from t order by s") == [("a",), ("b",), (None,)]
        assert _rows(conn, "select distinct v from t order by v") == [("1.5",), ("2",), ("3",)]
        assert _rows(conn, "select distinct s, n from t order by s, n") == [
            ("a", "10"),
            ("b", None),
            (None, "7"),
        ]
        assert _rows(conn, "select distinct s from t order by s limit 2") == [("a",), ("b",)]


def test_select_distinct_on(home: Path) -> None:
    """`DISTINCT ON (k)` keeps the first row per key IN SORT ORDER, so the
    `id desc` below picks the larger id of each pair."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int, s text, n int)")
        conn.execute("insert into t values (1,'a',1),(2,'a',2),(3,'b',1),(4,null,5),(5,null,6)")
        assert _rows(conn, "select distinct on (s) s, id from t order by s, id desc") == [
            ("a", "2"),
            ("b", "3"),
            (None, "5"),
        ]
        assert _rows(conn, "select distinct on (s, n) s, n, id from t order by s, n, id") == [
            ("a", "1", "1"),
            ("a", "2", "2"),
            ("b", "1", "3"),
            (None, "5", "4"),
            (None, "6", "5"),
        ]


def test_union_intersect_except(home: Path) -> None:
    """Set operations answered a single EMPTY row before 2026-09-20 -- the
    outer statement has no FROM, so it fell through to the constant planner.

    `ALL` keeps multiplicities: INTERSECT ALL pairs each right row with one
    left row, EXCEPT ALL subtracts them. All measured against 14.24.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int, s text, v numeric)")
        conn.execute("insert into t values (1,'a',1.5),(2,'a',1.50),(3,'b',2),(4,null,3)")
        conn.execute("create table u (id int, s text, v numeric)")
        conn.execute("insert into u values (1,'a',1.5),(2,'c',9)")
        assert _rows(conn, "select s from t union select s from u order by 1") == [
            ("a",),
            ("b",),
            ("c",),
            (None,),
        ]
        assert _rows(conn, "select s from t union all select s from u order by 1") == [
            ("a",),
            ("a",),
            ("a",),
            ("b",),
            ("c",),
            (None,),
        ]
        assert _rows(conn, "select s from t intersect select s from u order by 1") == [("a",)]
        assert _rows(conn, "select s from t intersect all select s from u order by 1") == [("a",)]
        assert _rows(conn, "select s from t except select s from u order by 1") == [
            ("b",),
            (None,),
        ]
        assert _rows(conn, "select s from t except all select s from u order by 1") == [
            ("a",),
            ("b",),
            (None,),
        ]
        # `1.5` and `1.50` are one value, so the union of the two tables'
        # numerics is three rows, not four.
        assert _rows(conn, "select v from t union select v from u order by 1") == [
            ("1.5",),
            ("2",),
            ("3",),
            ("9",),
        ]
        assert _rows(conn, "select s from t union select s from u order by 1 desc limit 2") == [
            (None,),
            ("c",),
        ]


def test_set_operation_type_rules(home: Path) -> None:
    """PostgreSQL unifies the two sides within a type category and refuses
    across one; an untyped NULL takes the other side's type (14.24)."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.execute("select 1::int4 union select 1::int8")
        cur.fetchall()
        assert cur.description[0].type_code == 20  # int8
        cur = conn.execute("select 1::int4 union select null")
        cur.fetchall()
        assert cur.description[0].type_code == 23  # int4, not text
        for sql, state in [
            ("select 'x'::text union select 1::int4", "42804"),
            ("select 1::int4 union select true", "42804"),
            ("select id, s from t2 union select id from t2", "42601"),
        ]:
            conn.execute("create table if not exists t2 (id int, s text)")
            with pytest.raises(psycopg.Error) as exc:
                conn.execute(sql).fetchall()
            assert exc.value.sqlstate == state
        assert "UNION types text and integer cannot be matched" in str(
            _error_of(conn, "select 'x'::text union select 1::int4")
        )


def _error_of(conn, sql: str) -> str:
    try:
        conn.execute(sql).fetchall()
    except psycopg.Error as exc:  # noqa: BLE001 - the message is the assertion
        return str(exc)
    raise AssertionError(f"{sql} did not fail")


def test_distinct_inside_an_aggregate(home: Path) -> None:
    """`count(DISTINCT col)` was `0A000 DISTINCT inside an aggregate`.

    Values measured against PostgreSQL 14.24 (2026-09-20). The group's values
    are deduped BY VALUE -- `1.5` and `1.50` are one -- with NULLs already
    dropped, except for `array_agg`, whose DISTINCT keeps NULL as a value and
    returns the result SORTED rather than in group order.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int, s text, v numeric, g int)")
        conn.execute(
            "insert into t values (1,'a',1.5,1),(2,'a',1.50,1),(3,'b',2,1),(4,null,3,2),(5,'b',2,2)"
        )
        assert _rows(conn, "select count(distinct s), count(s), count(*) from t") == [
            ("2", "4", "5")
        ]
        assert _rows(conn, "select count(distinct v) from t") == [("3",)]
        assert _rows(conn, "select sum(distinct v) from t") == [("6.5",)]
        assert _rows(conn, "select g, count(distinct s) from t group by g order by g") == [
            ("1", "2"),
            ("2", "1"),
        ]
        assert _rows(conn, "select array_agg(distinct s) from t") == [("['a', 'b', None]",)]
        # An empty input still counts zero, not NULL.
        assert _rows(conn, "select count(distinct s) from t where id > 10") == [("0",)]


def test_select_distinct_over_an_aggregate(home: Path) -> None:
    """`SELECT DISTINCT` over aggregate output: two groups with the same count
    collapse into one row (PostgreSQL 14.24). `DISTINCT ON` there is refused
    as unsupported rather than answered wrongly."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int, g int, s text)")
        conn.execute("insert into t values (1,1,'a'),(2,1,'b'),(3,2,'a'),(4,2,'b'),(5,3,'c')")
        assert _rows(conn, "select distinct count(*) from t") == [("5",)]
        assert _rows(conn, "select distinct g, count(*) from t group by g order by 1") == [
            ("1", "2"),
            ("2", "2"),
            ("3", "1"),
        ]
        # DISTINCT ON over the groups: one row per key, the first in ORDER BY.
        assert _rows(
            conn, "select distinct on (g) g, count(*) from t group by g order by g desc"
        ) == [("3", "1"), ("2", "2"), ("1", "2")]


def test_update_and_delete_returning(home: Path) -> None:
    """`UPDATE ... RETURNING` and `DELETE ... RETURNING` answered NO ROWSET.

    The write happened and the tag was right, so a client that asked which
    rows it had just changed silently got nothing. UPDATE returns the rows as
    they are AFTER the update and DELETE as they were before, which is what
    PostgreSQL 14.24 does.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table d (id int primary key, n int, s text)")
        conn.execute("insert into d values (1,1,'a'),(2,2,'b'),(3,3,'c')")
        assert conn.execute("update d set n = n + 100 where id = 1 returning id, n").fetchall() == [
            (1, 101)
        ]
        assert conn.execute("update d set n = n + 1 where id = 2 returning *").fetchall() == [
            (2, 3, "b")
        ]
        assert conn.execute("delete from d where id = 3 returning id, s").fetchall() == [(3, "c")]
        # The write itself still happened, exactly once.
        assert conn.execute("select id, n from d order by id").fetchall() == [(1, 101), (2, 3)]
        # A statement that matches nothing returns no rows, not an error.
        assert conn.execute("update d set n = 0 where id = 99 returning id").fetchall() == []


def test_an_aliased_primary_key_keeps_its_type(home: Path) -> None:
    """`select id as k` described the column as text.

    A primary key is STORED as `_id`, and the row description looked the
    column up by its stored field and then by its output name -- neither of
    which is `id` once an alias renames it -- so it fell through to the
    varchar default and the client decoded an integer as a string. Only
    aliased primary keys were affected, which is why it survived this long.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table d (id int primary key, n int, s text)")
        conn.execute("insert into d values (1, 2, 'a')")
        for sql in (
            "select id as k from d",
            "select id as k, n as m from d",
            "select id as k, upper(s) from d",
        ):
            row = conn.execute(sql).fetchone()
            assert isinstance(row[0], int), f"{sql} described the key as {type(row[0]).__name__}"
        assert conn.execute("insert into d values (9,9,'i') returning id as k").fetchall() == [(9,)]


def test_bool_and_bool_or(home: Path) -> None:
    """`bool_and` / `bool_or` were not recognised as aggregates at all, so
    they reached the per-row scalar evaluator. Values from PostgreSQL 14.24:
    NULLs are skipped, and an empty input is NULL rather than no row."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table b (id int primary key, ok boolean, g int)")
        conn.execute("insert into b values (1,true,1),(2,true,1),(3,false,2),(4,null,2)")
        assert conn.execute("select bool_and(ok), bool_or(ok) from b").fetchall() == [(False, True)]
        assert conn.execute(
            "select g, bool_and(ok), bool_or(ok) from b group by g order by g"
        ).fetchall() == [(1, True, True), (2, False, False)]
        assert conn.execute("select bool_and(ok) from b where id > 10").fetchall() == [(None,)]
        assert conn.execute("select bool_and(id > 0) from b").fetchall() == [(True,)]


def test_array_agg_over_an_empty_input_is_null(home: Path) -> None:
    """Every aggregate but `count` is NULL over an empty input; `array_agg`
    answered an empty ARRAY."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table e (id int primary key, n int)")
        assert conn.execute("select array_agg(n) from e").fetchall() == [(None,)]
        assert conn.execute("select count(*), sum(n), min(n) from e").fetchall() == [
            (0, None, None)
        ]


def test_an_unsupported_aggregate_refuses_the_same_way_on_an_empty_table(home: Path) -> None:
    """The silent half of a missing feature.

    An unimplemented shape used to be planned as a plain SELECT with a
    computed column, so its refusal came from EVALUATING a row -- and over an
    empty table no row was evaluated, so the client got zero rows and no error
    where PostgreSQL answers one. Refusing while planning makes the answer the
    same either way.

    The shapes that showed this (`string_agg`, `count(*) + 1`, an ORDER BY
    over an expression) have since been implemented, so the property is
    pinned with an error PostgreSQL raises while planning: a DISTINCT
    aggregate ordered by something other than its argument (42P10).
    """
    sql = "select array_agg(distinct s order by length(s)) from t"
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key, n int, s text)")
        with pytest.raises(psycopg.Error) as empty:
            conn.execute(sql).fetchall()
        assert empty.value.sqlstate == "42P10"
        conn.execute("insert into t values (1, 1, 'a')")
        with pytest.raises(psycopg.Error) as filled:
            conn.execute(sql).fetchall()
        assert filled.value.sqlstate == "42P10"


def test_char_n_is_blank_padded_and_carries_its_width(home: Path) -> None:
    """`char(n)` is a blank-padded type, and its width reaches the client.

    The Rust server described `char(4)` as an unsized `bpchar` and sent `ab`
    where PostgreSQL 14.24 sends `ab  ` with a declared width of 4. The value
    is STORED unpadded -- `length()` ignores trailing blanks and a cast to
    text strips them -- so the padding belongs on the way out, which is how
    the Python server has always done it.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table c (id int primary key, c char(4), v varchar(6))")
        conn.execute("insert into c values (1, 'ab', 'xy')")
        cur = conn.execute("select c, v from c")
        assert cur.fetchall() == [("ab  ", "xy")]
        assert [(d.type_code, d.display_size) for d in cur.description] == [(1042, 4), (1043, 6)]
        # A cast to text strips the padding, as PostgreSQL does.
        assert conn.execute("select c::text from c").fetchall() == [("ab",)]


def test_char_n_comparison_ignores_trailing_blanks(home: Path) -> None:
    """`bpchar` comparison strips trailing blanks from BOTH sides, so a
    `char(4)` holding `ab` matches `'ab'` and `'ab  '` alike. `varchar` is
    blank-SENSITIVE and must not be touched (both measured on 14.24)."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table c (id int primary key, c char(4), v varchar(4))")
        conn.execute("insert into c values (1,'ab','ab'),(2,'abcd','abcd')")
        assert conn.execute("select id from c where c = 'ab'").fetchall() == [(1,)]
        assert conn.execute("select id from c where c = 'ab  '").fetchall() == [(1,)]
        assert conn.execute("select id from c where c <> 'ab  '").fetchall() == [(2,)]
        assert conn.execute("select id from c where v = 'ab  '").fetchall() == []


def test_a_declared_width_survives_the_hand_off_to_the_other_server(home: Path) -> None:
    """The catalog is shared, and so is a column's DECLARED type.

    The Python server writes a `char(n)` column as `type: "text"` with
    `decl_oid: 1042` and an `atttypmod`; the Rust side modelled neither, so it
    read such a column as plain `text` (oid 25) with no width -- and wrote
    `type: "bpchar"`, which the Python side read as text in turn. The values
    always survived; the declared type did not, in either direction.
    """
    from secantus.sql import engine as sql_engine
    from secantus.storage import Storage as PyStorage

    store = PyStorage(str(home), durable=True)
    try:
        sql_engine.run_sql(store, "postgres", "create table c (id int primary key, c char(4))")
        sql_engine.run_sql(store, "postgres", "insert into c values (1, 'ab')")
    finally:
        store.close()

    with _Server(home) as server, server.connect() as conn:
        cur = conn.execute("select c from c")
        assert cur.fetchall() == [("ab  ",)]
        assert (cur.description[0].type_code, cur.description[0].display_size) == (1042, 4)


def test_sum_result_types(home: Path) -> None:
    """`sum()`'s result type is PostgreSQL's, which is not a uniform widening.

    Everything but a numeric was described as `int8`, so `sum(f)` over a
    `float8` column declared an integer and sent `1.5` -- psycopg raised
    `invalid literal for int() with base 10: '1.5'` rather than returning a
    number. The oids below are PostgreSQL 14.24's: int2/int4 sum as bigint,
    int8 as NUMERIC, and a float sums as itself.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute(
            "create table a (id int primary key, s2 int2, s4 int4, s8 int8,"
            " f4 float4, f8 float8, nu numeric)"
        )
        conn.execute("insert into a values (1,1,1,1,1.5,1.5,1.5),(2,2,2,2,2.5,2.5,2.5)")
        for col, oid in [
            ("s2", 20),
            ("s4", 20),
            ("s8", 1700),
            ("f4", 700),
            ("f8", 701),
            ("nu", 1700),
        ]:
            cur = conn.execute(f"select sum({col}) from a")
            rows = cur.fetchall()
            assert cur.description[0].type_code == oid, f"sum({col})"
            assert rows[0][0] is not None
        assert conn.execute("select sum(f8) from a").fetchall() == [(4.0,)]


def test_having(home: Path) -> None:
    """`HAVING` was refused outright (`0A000 HAVING is not supported yet`).

    Every expectation measured against PostgreSQL 14.24 (2026-09-20). Note
    `count(s)` and `min(n)`: an aggregate written in HAVING need not be in the
    SELECT list, so it is computed for the test and never projected. A NULL
    makes a comparison UNKNOWN, so `sum(n) > 10` drops the group whose sum is
    NULL rather than keeping it.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key, g int, n int, s text)")
        conn.execute(
            "insert into t values (1,1,10,'a'),(2,1,20,'b'),(3,2,5,'c'),(4,2,null,'d'),(5,3,7,null)"
        )
        q = lambda sql: conn.execute(sql).fetchall()  # noqa: E731
        assert q("select g, count(*) from t group by g having count(*) > 1 order by g") == [
            (1, 2),
            (2, 2),
        ]
        assert q("select g from t group by g having count(*) = 1 order by g") == [(3,)]
        assert q("select g, sum(n) from t group by g having sum(n) > 10 order by g") == [(1, 30)]
        assert q("select g from t group by g having sum(n) is null order by g") == []
        # An aggregate only HAVING asks for -- `count(s)` skips the NULL `s`,
        # so the group whose only row has none counts 0.
        assert q("select g from t group by g having count(s) = 2 order by g") == [(1,), (2,)]
        assert q("select g from t group by g having count(s) = 0 order by g") == [(3,)]
        assert q("select g from t group by g having min(n) >= 5 order by g") == [(1,), (2,), (3,)]
        # A constant on the left, a group key, and the connectives.
        assert q("select g from t group by g having 1 < count(*) order by g") == [(1,), (2,)]
        assert q("select g from t group by g having g > 1 order by g") == [(2,), (3,)]
        assert q("select g from t group by g having count(*) > 1 or g = 3 order by g") == [
            (1,),
            (2,),
            (3,),
        ]
        assert q("select g from t group by g having not (count(*) > 1) order by g") == [(3,)]
        # HAVING with no GROUP BY filters the single row.
        assert q("select count(*) from t having count(*) > 3") == [(5,)]
        assert q("select count(*) from t having count(*) > 99") == []


def test_aggregate_filter(home: Path) -> None:
    """`agg(...) FILTER (WHERE ...)` was `0A000 FILTER on an aggregate`.

    Only the matching rows contribute, and a group where NONE match is the
    empty input -- `count` is 0, everything else NULL. Measured against
    PostgreSQL 14.24 (2026-09-22).
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key, g int, n int, s text)")
        conn.execute(
            "insert into t values (1,1,10,'a'),(2,1,20,'b'),(3,2,5,'c'),(4,2,null,'d'),(5,3,7,null)"
        )
        q = lambda sql: conn.execute(sql).fetchall()  # noqa: E731
        assert q("select count(*) filter (where n > 8) from t") == [(2,)]
        assert q("select g, count(*) filter (where n > 8) from t group by g order by g") == [
            (1, 2),
            (2, 0),
            (3, 0),
        ]
        # No matching row is the empty input: count 0, sum NULL.
        assert q("select count(n) filter (where id > 99) from t") == [(0,)]
        assert q("select sum(n) filter (where id > 99) from t") == [(None,)]
        # Two aggregates, different filters, and one with none.
        assert q("select count(*) filter (where n > 8), count(*) from t") == [(2, 5)]
        assert q("select min(n) filter (where g = 2), max(n) filter (where g = 1) from t") == [
            (5, 20)
        ]
        # FILTER inside HAVING, and beside DISTINCT.
        assert q(
            "select g from t group by g having count(*) filter (where n > 8) = 2 order by g"
        ) == [(1,)]
        assert q("select count(distinct s) filter (where id < 4) from t") == [(3,)]


def test_avg(home: Path) -> None:
    """`avg()` was deferred because it "returns PostgreSQL numeric with its own
    scale rules".

    Those rules are the ones numeric DIVISION already follows here: 16 decimal
    places for a small quotient, more when an input carries more. So `avg` is
    the exact sum over the count, divided the same way, and gets the scale
    right without a second opinion. Values measured on PostgreSQL 14.24.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute(
            "create table t (id int primary key, g int, i4 int4, f8 float8, nu numeric, ns numeric)"
        )
        conn.execute(
            "insert into t values (1,1,1,1.0,1.5,'1.00000000000000000001'),"
            "(2,1,2,2.0,2.5,'2.00000000000000000002'),(3,2,4,4.0,1.25,3),(4,2,null,null,null,null)"
        )
        one = lambda sql: conn.execute(sql).fetchall()[0][0]  # noqa: E731
        assert str(one("select avg(i4) from t")) == "2.3333333333333333"
        assert str(one("select avg(nu) from t")) == "1.7500000000000000"
        # An input with 20 decimals keeps 20, not 16.
        assert str(one("select avg(ns) from t")) == "2.00000000000000000001"
        # A float averages as a float, not a numeric.
        assert one("select avg(f8) from t") == pytest.approx(2.3333333333333335)
        cur = conn.execute("select avg(i4), avg(f8) from t")
        cur.fetchall()
        assert [d.type_code for d in cur.description] == [1700, 701]
        # Empty input is NULL, and avg composes with the rest.
        assert one("select avg(i4) from t where id > 99") is None
        assert str(one("select avg(i4) filter (where id < 3) from t")) == "1.5000000000000000"
        assert str(one("select avg(distinct i4) from t")) == "2.3333333333333333"
        assert conn.execute(
            "select g from t group by g having avg(i4) > 2 order by g"
        ).fetchall() == [(2,)]


def test_string_agg(home: Path) -> None:
    """`string_agg` was refused: it takes TWO arguments, which the aggregate
    planner rejected outright.

    Measured against PostgreSQL 14.24 (2026-09-22): the non-NULL values are
    joined in group order, an empty input is NULL, a NULL separator joins with
    nothing between them, and DISTINCT dedups AND sorts.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key, g int, s text, n int)")
        conn.execute(
            "insert into t values (1,1,'b',2),(2,1,'a',1),(3,2,'c',3),(4,2,null,4),(5,3,'d',5)"
        )
        one = lambda sql: conn.execute(sql).fetchall()[0][0]  # noqa: E731
        assert one("select string_agg(s, ',') from t") == "b,a,c,d"
        assert conn.execute(
            "select g, string_agg(s, ',') from t group by g order by g"
        ).fetchall() == [(1, "b,a"), (2, "c"), (3, "d")]
        assert one("select string_agg(s, '-') from t where id > 99") is None
        assert one("select string_agg(s, ',') from t where s is null") is None
        assert one("select string_agg(distinct s, ',') from t") == "a,b,c,d"
        assert one("select string_agg(s, null) from t") == "bacd"
        assert one("select string_agg(s, ',') filter (where id < 3) from t") == "b,a"
        # A non-text argument is a missing FUNCTION, as PostgreSQL has it.
        with pytest.raises(psycopg.Error) as exc:
            conn.execute("select string_agg(n, ',') from t").fetchall()
        assert exc.value.sqlstate == "42883"


def test_order_by_inside_an_aggregate(home: Path) -> None:
    """`array_agg(x ORDER BY y)` / `string_agg(x, s ORDER BY y)` sort the
    group's rows before the values are collected -- the only thing that gives
    either aggregate a defined order. Values from PostgreSQL 14.24."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key, g int, s text, n int)")
        conn.execute(
            "insert into t values (1,1,'b',2),(2,1,'a',1),(3,2,'c',3),(4,2,null,4),(5,3,'d',5)"
        )
        one = lambda sql: conn.execute(sql).fetchall()[0][0]  # noqa: E731
        assert one("select array_agg(s order by s) from t") == ["a", "b", "c", "d", None]
        assert one("select array_agg(s order by id desc) from t") == ["d", None, "c", "a", "b"]
        assert one("select array_agg(s order by s nulls first) from t") == [
            None,
            "a",
            "b",
            "c",
            "d",
        ]
        assert one("select string_agg(s, ',' order by id desc) from t") == "d,c,a,b"
        assert conn.execute(
            "select g, array_agg(s order by id desc) from t group by g order by g"
        ).fetchall() == [(1, ["a", "b"]), (2, [None, "c"]), (3, ["d"])]
        # An expression key is refused rather than answered in another order.
        # ORDER BY over an expression (n: b=2 a=1 c=3 NULL=4 d=5).
        assert one("select array_agg(s order by n * -1) from t") == ["d", None, "c", "b", "a"]


def test_aggregates_inside_expressions(home: Path) -> None:
    """`count(*) + 1` and friends were planned as a plain SELECT with a
    computed column, so they reached the per-row scalar evaluator -- which has
    no `sum` -- and over an EMPTY table answered no rows at all.

    The aggregates inside the expression are now ordinary items, computed per
    group, and the expression runs over their results. Values and oids from
    PostgreSQL 14.24 (2026-09-22).
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key, g int, n int)")
        conn.execute("insert into t values (1,1,10),(2,1,20),(3,2,5),(4,2,null)")
        one = lambda sql: conn.execute(sql).fetchall()[0][0]  # noqa: E731
        assert one("select count(*) + 1 from t") == 5
        assert one("select sum(n) * 2 from t") == 70
        assert one("select sum(n) + count(*) from t") == 39
        assert one("select coalesce(sum(n), -1) from t") == 35
        assert one("select (sum(n))::text from t") == "35"
        # The empty-table case, which used to answer NO ROWS with no error.
        assert one("select coalesce(sum(n), 0) from t where id > 99") == 0
        assert conn.execute("select count(*) + 1 from t where id > 99").fetchall() == [(1,)]
        # Grouped, mixing a key with an aggregate, and beside plain outputs.
        assert conn.execute("select g, count(*) + 1 from t group by g order by g").fetchall() == [
            (1, 3),
            (2, 3),
        ]
        assert sorted(conn.execute("select g + count(*) from t group by g").fetchall()) == [
            (3,),
            (4,),
        ]
        assert conn.execute(
            "select g, sum(n), sum(n) * 2 from t group by g order by g"
        ).fetchall() == [(1, 30, 60), (2, 5, 10)]
        cur = conn.execute("select count(*) + 1 from t")
        cur.fetchall()
        assert cur.description[0].type_code == 20  # int8, as PostgreSQL types it


# --- pg_constraint -----------------------------------------------------------
#
# Every expected value below was MEASURED against PostgreSQL 14.24 on
# 2026-09-28, not recalled: the column set from `pg_attribute`, the rows from
# the same DDL these tests run. PostgreSQL is the exemplar for this server --
# never the Python PG server (CLAUDE.md, "Design constraints").

_CONSTRAINT_DDL = (
    "CREATE TABLE par (id int PRIMARY KEY, tag text UNIQUE)",
    """CREATE TABLE ch (
         id int PRIMARY KEY,
         n int NOT NULL CHECK (n > 0),
         pid int REFERENCES par(id) ON DELETE CASCADE ON UPDATE RESTRICT,
         u1 int UNIQUE,
         CONSTRAINT named_ck CHECK (n < 100)
       )""",
)


def _seed_constraints(server: _Server) -> psycopg.Connection:
    conn = server.connect()
    cur = conn.cursor()
    for sql in _CONSTRAINT_DDL:
        cur.execute(sql)
    return conn


def test_pg_constraint_lists_every_kind_but_not_null(home: Path) -> None:
    """The five rows PostgreSQL 14.24 reports for this table, and only those.

    NOT NULL is the point of the test: PostgreSQL records it as
    `pg_attribute.attnotnull`, NOT as a `pg_constraint` row, so a table with
    one NOT NULL column still has exactly five rows here. A sixth would be a
    divergence we invented.
    """
    with _Server(home) as server, _seed_constraints(server) as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT conname, contype FROM pg_constraint "
            "WHERE conrelid = 'ch'::regclass ORDER BY conname"
        )
        assert cur.fetchall() == [
            ("ch_n_check", "c"),
            ("ch_pid_fkey", "f"),
            ("ch_pkey", "p"),
            ("ch_u1_key", "u"),
            ("named_ck", "c"),
        ]


def test_pg_constraint_keys_are_attnums(home: Path) -> None:
    """`conkey` / `confkey` are 1-based attnum arrays; `confkey` is NULL off an FK."""
    with _Server(home) as server, _seed_constraints(server) as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT conname, conkey, confkey FROM pg_constraint "
            "WHERE conrelid = 'ch'::regclass ORDER BY conname"
        )
        assert cur.fetchall() == [
            ("ch_n_check", [2], None),
            ("ch_pid_fkey", [3], [1]),
            ("ch_pkey", [1], None),
            ("ch_u1_key", [4], None),
            ("named_ck", [2], None),
        ]


def test_pg_constraint_multi_column_check_conkey_is_in_expression_order(
    home: Path,
) -> None:
    """`conkey` follows the EXPRESSION, not the column declaration order.

    Measured on PostgreSQL 14.24: over a table `(a, b, c)`, `check (c > a)`
    reports `conkey = {3,1}`. Listing the table's columns in declaration order
    and filtering gave `{1,3}` -- backwards. Order is load-bearing in this
    column generally, since an FK's `conkey` pairs with `confkey` positionally.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "CREATE TABLE mc (a int, b int, c int, CHECK (a < b), CONSTRAINT rev CHECK (c > a))"
        )
        cur.execute(
            "SELECT conname, conkey FROM pg_constraint "
            "WHERE conrelid = 'mc'::regclass AND contype = 'c' ORDER BY conname"
        )
        assert cur.fetchall() == [("mc_check", [1, 2]), ("rev", [3, 1])]


def test_pg_constraint_fk_action_codes(home: Path) -> None:
    """The one-letter action codes, and the blanks every non-FK row carries."""
    with _Server(home) as server, _seed_constraints(server) as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT confupdtype, confdeltype, confmatchtype, confrelid "
            "FROM pg_constraint WHERE conname = 'ch_pid_fkey'"
        )
        upd, delete, match, confrelid = cur.fetchone()
        # ON UPDATE RESTRICT, ON DELETE CASCADE, MATCH SIMPLE.
        assert (upd, delete, match) == ("r", "c", "s")
        cur.execute("SELECT 'par'::regclass::oid")
        assert confrelid == cur.fetchone()[0], "confrelid is the parent's relation oid"

        cur.execute(
            "SELECT DISTINCT confupdtype, confdeltype, confmatchtype "
            "FROM pg_constraint WHERE conrelid = 'ch'::regclass AND contype <> 'f'"
        )
        assert cur.fetchall() == [(" ", " ", " ")]


def test_pg_constraint_flags_and_namespace(home: Path) -> None:
    """`connoinherit` is false for a CHECK and true for the rest; `public` is 2200."""
    with _Server(home) as server, _seed_constraints(server) as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT conname, condeferrable, condeferred, convalidated, "
            "       conislocal, coninhcount, connoinherit, contypid, conparentid "
            "FROM pg_constraint WHERE conrelid = 'ch'::regclass ORDER BY conname"
        )
        assert cur.fetchall() == [
            ("ch_n_check", False, False, True, True, 0, False, 0, 0),
            ("ch_pid_fkey", False, False, True, True, 0, True, 0, 0),
            ("ch_pkey", False, False, True, True, 0, True, 0, 0),
            ("ch_u1_key", False, False, True, True, 0, True, 0, 0),
            ("named_ck", False, False, True, True, 0, False, 0, 0),
        ]
        cur.execute(
            "SELECT DISTINCT connamespace FROM pg_constraint WHERE conrelid = 'ch'::regclass"
        )
        assert cur.fetchall() == [(2200,)]


def test_pg_constraint_oids_are_distinct(home: Path) -> None:
    """Synthetic, but distinct per constraint -- a client may join on them."""
    with _Server(home) as server, _seed_constraints(server) as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT count(DISTINCT oid), count(*) FROM pg_constraint "
            "WHERE conrelid IN ('ch'::regclass, 'par'::regclass)"
        )
        distinct, total = cur.fetchone()
        assert total == 7, "5 rows for ch + 2 for par"
        assert distinct == total


def test_pg_constraint_column_wire_types(home: Path) -> None:
    """The RowDescription oids PostgreSQL 14.24 sends for these columns.

    `contype` is the INTERNAL `"char"` (18), not `bpchar` (1042), and `conbin`
    is `pg_node_tree` (194) -- both measured. A client reads these oids.
    """
    with _Server(home) as server, _seed_constraints(server) as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT conname, contype, conkey, conbin FROM pg_constraint WHERE conname = 'named_ck'"
        )
        cur.fetchall()
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("conname", 19),  # name
            ("contype", 18),  # "char"
            ("conkey", 1005),  # int2[]
            ("conbin", 194),  # pg_node_tree
        ]


def test_pg_constraint_is_listed_in_pg_tables(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT count(*) FROM pg_tables "
            "WHERE schemaname = 'pg_catalog' AND tablename = 'pg_constraint'"
        )
        assert cur.fetchone()[0] == 1


def test_pg_constraint_qualified_by_pg_catalog(home: Path) -> None:
    with _Server(home) as server, _seed_constraints(server) as conn:
        cur = conn.cursor()
        cur.execute("SELECT count(*) FROM pg_catalog.pg_constraint WHERE conrelid = 'ch'::regclass")
        assert cur.fetchone()[0] == 5


@pytest.mark.parametrize(
    "predicate",
    [
        "conrelid IN ('ch'::regclass, 'par'::regclass)",
        "conrelid = ANY(ARRAY['ch'::regclass, 'par'::regclass])",
    ],
)
def test_a_regclass_list_selects_the_same_rows_as_or(home: Path, predicate: str) -> None:
    """A regclass operand in a LIST must compare by oid, as `=` already did.

    Regression test for a silent WRONG-ROWS bug: a regclass value is a
    one-field document carrying its oid, the stored column is a number, and
    only the scalar path unwrapped it. `WHERE conrelid IN (...)` therefore
    compared documents against numbers, matched NOTHING, and returned zero
    rows with no error -- while the same predicate written with `OR` returned
    the right ones. That is the shape catalog reflection emits (SQLAlchemy and
    pgjdbc both use `IN` / `= ANY`), so it looked like a server with no
    constraints rather than like a bug. Measured 7 on PostgreSQL 14.24.
    """
    with _Server(home) as server, _seed_constraints(server) as conn:
        cur = conn.cursor()
        cur.execute(f"SELECT count(*) FROM pg_constraint WHERE {predicate}")
        listed = cur.fetchone()[0]
        cur.execute(
            "SELECT count(*) FROM pg_constraint "
            "WHERE conrelid = 'ch'::regclass OR conrelid = 'par'::regclass"
        )
        assert listed == cur.fetchone()[0] == 7


# --------------------------------------------------------------------------- #
# `INSERT ... ON CONFLICT`.
#
# The clause used to be PARSED AND DROPPED: `on conflict do nothing` raised
# 23505 where PostgreSQL succeeds, and `do update` never upserted. That is worse
# than refusing it -- the client gets a confident wrong answer instead of an
# honest 0A000 -- and it is the failure CLAUDE.md's wire-fidelity rule names.
#
# Every expectation below was measured against PostgreSQL 14.13, not derived
# from the Python server, which has its own implementation of this clause.
# --------------------------------------------------------------------------- #


def _oc_table(conn) -> None:
    cur = conn.cursor()
    cur.execute("create table t (id int primary key, tag text unique, v int)")
    cur.execute("insert into t values (1, 'a', 10), (2, 'b', 20)")


def test_on_conflict_do_nothing_absorbs_a_conflict(home: Path) -> None:
    """The headline: no error, no write, and the row count is 0."""
    with _Server(home) as server, server.connect() as conn:
        _oc_table(conn)
        cur = conn.cursor()
        cur.execute("insert into t values (1, 'z', 99) on conflict do nothing")
        assert cur.rowcount == 0
        cur.execute("select id, tag, v from t order by id")
        assert cur.fetchall() == [(1, "a", 10), (2, "b", 20)]


def test_on_conflict_do_nothing_still_inserts_a_fresh_row(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _oc_table(conn)
        cur = conn.cursor()
        cur.execute("insert into t values (3, 'c', 30) on conflict do nothing")
        assert cur.rowcount == 1
        cur.execute("select count(*) from t")
        assert cur.fetchone()[0] == 3


def test_on_conflict_do_update_upserts_from_excluded(home: Path) -> None:
    """`excluded.v` must read the PROPOSED row, not the existing one.

    Both resolve to the bare name `v` through the planner's column resolver,
    which takes the LAST name of a qualified reference -- so without the
    rename to `EXCLUDED_PREFIX` this assignment reads the row it is updating
    and the statement is a silent no-op that still reports a row.
    """
    with _Server(home) as server, server.connect() as conn:
        _oc_table(conn)
        cur = conn.cursor()
        cur.execute(
            "insert into t values (1, 'a', 99) on conflict (id) do update set v = excluded.v"
        )
        assert cur.rowcount == 1
        cur.execute("select v from t where id = 1")
        assert cur.fetchone()[0] == 99


def test_on_conflict_do_update_reads_both_rows(home: Path) -> None:
    """`t.v * 100 + excluded.v` — the existing row AND the proposed one."""
    with _Server(home) as server, server.connect() as conn:
        _oc_table(conn)
        cur = conn.cursor()
        cur.execute(
            "insert into t values (1, 'a', 7) on conflict (id) "
            "do update set v = t.v * 100 + excluded.v"
        )
        cur.execute("select v from t where id = 1")
        assert cur.fetchone()[0] == 1007


def test_on_conflict_do_update_where_gates_the_write(home: Path) -> None:
    """A false WHERE writes nothing and counts 0, like an UPDATE matching none."""
    with _Server(home) as server, server.connect() as conn:
        _oc_table(conn)
        cur = conn.cursor()
        cur.execute(
            "insert into t values (1, 'a', 99) on conflict (id) "
            "do update set v = excluded.v where t.v < 0"
        )
        assert cur.rowcount == 0
        cur.execute("select v from t where id = 1")
        assert cur.fetchone()[0] == 10


def test_on_conflict_returning_projects_only_affected_rows(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _oc_table(conn)
        cur = conn.cursor()
        cur.execute(
            "insert into t values (1, 'a', 42) on conflict (id) "
            "do update set v = excluded.v returning id, v"
        )
        assert cur.fetchall() == [(1, 42)]
        # A skipped DO NOTHING returns NO row, not a null one.
        cur.execute("insert into t values (1, 'a', 1) on conflict do nothing returning id, v")
        assert cur.fetchall() == []


def test_on_conflict_arbitrates_only_on_its_target(home: Path) -> None:
    """A conflict on a DIFFERENT constraint than the target is still 23505.

    `on conflict (tag)` does not absorb a PRIMARY KEY collision — measured on
    PostgreSQL 14.13, and the reason the executor compares the index the
    storage layer reports against the clause's target.
    """
    with _Server(home) as server, server.connect() as conn:
        _oc_table(conn)
        cur = conn.cursor()
        with pytest.raises(psycopg.errors.UniqueViolation):
            cur.execute("insert into t values (1, 'zz', 9) on conflict (tag) do nothing")
        conn.rollback()
        # ... while a bare DO NOTHING takes either constraint.
        cur.execute("insert into t values (1, 'zz', 9) on conflict do nothing")
        assert cur.rowcount == 0


def test_on_conflict_on_constraint_names_the_pk(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _oc_table(conn)
        cur = conn.cursor()
        cur.execute("insert into t values (1, 'z', 9) on conflict on constraint t_pkey do nothing")
        assert cur.rowcount == 0


def test_on_conflict_unique_constraint_not_just_the_pk(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _oc_table(conn)
        cur = conn.cursor()
        cur.execute(
            "insert into t values (3, 'a', 30) on conflict (tag) do update set v = excluded.v"
        )
        cur.execute("select id, v from t where tag = 'a'")
        assert cur.fetchall() == [(1, 30)]


def test_on_conflict_target_matching_nothing_is_a_plan_error(home: Path) -> None:
    """PostgreSQL decides the arbiter BEFORE touching a row.

    A column list matching no unique index is `42P10`, and an unknown
    constraint NAME is `42704` — two different codes, measured, not assumed.
    """
    with _Server(home) as server, server.connect() as conn:
        _oc_table(conn)
        cur = conn.cursor()
        with pytest.raises(psycopg.Error) as info:
            cur.execute("insert into t values (3, 'c', 3) on conflict (v) do nothing")
        assert info.value.sqlstate == "42P10"
        conn.rollback()
        with pytest.raises(psycopg.Error) as info:
            cur.execute(
                "insert into t values (3, 'c', 3) on conflict on constraint nope do nothing"
            )
        assert info.value.sqlstate == "42704"


def test_on_conflict_do_update_still_checks_constraints(home: Path) -> None:
    """The upsert path is not a hole in CHECK enforcement."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table c (id int primary key, n int check (n < 100))")
        cur.execute("insert into c values (1, 1)")
        with pytest.raises(psycopg.errors.CheckViolation):
            cur.execute("insert into c values (1, 1) on conflict (id) do update set n = 500")


def test_on_conflict_where_without_a_partial_index_infers_the_key(home: Path) -> None:
    """A WHERE on the TARGET selects a partial unique index whose predicate
    it implies; with none, PostgreSQL's inference falls back to the plain
    unique key (here the primary key), and the conflict is absorbed."""
    with _Server(home) as server, server.connect() as conn:
        _oc_table(conn)
        cur = conn.cursor()
        cur.execute("insert into t values (1, 'z', 9) on conflict (id) where id > 0 do nothing")
        assert cur.rowcount == 0


# --------------------------------------------------------------------------- #
# `GROUP BY GROUPING SETS / ROLLUP / CUBE`.
#
# These used to be PARSED AND DROPPED: the `GroupingSet` node fell through to
# the expression arm, failed to resolve as a column, and the statement died with
# `42803 column "a" must appear in the GROUP BY clause` -- an error blaming the
# user's own query for a clause the server had discarded. Worse than refusing
# it, and the second of the two such cases the 2026-09-28 survey found.
#
# Every expectation below was measured against PostgreSQL 14.13.
# --------------------------------------------------------------------------- #


def _gs_table(conn) -> None:
    cur = conn.cursor()
    cur.execute("create table s (a text, b text, n int)")
    cur.execute("insert into s values ('x','p',1),('x','q',2),('y','p',4),('y','q',8)")


def test_grouping_sets_adds_the_empty_set_total(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _gs_table(conn)
        cur = conn.cursor()
        cur.execute("select a, sum(n) from s group by grouping sets ((a),()) order by a nulls last")
        assert cur.fetchall() == [("x", 3), ("y", 12), (None, 15)]


def test_grouping_sets_over_two_separate_columns(home: Path) -> None:
    """Each set groups on its OWN columns; the others are NULL-padded."""
    with _Server(home) as server, server.connect() as conn:
        _gs_table(conn)
        cur = conn.cursor()
        cur.execute(
            "select a, b, sum(n) from s group by grouping sets ((a),(b)) "
            "order by a nulls last, b nulls last"
        )
        assert cur.fetchall() == [
            ("x", None, 3),
            ("y", None, 12),
            (None, "p", 5),
            (None, "q", 10),
        ]


def test_a_multi_key_set_is_a_row_expression(home: Path) -> None:
    """`((a,b),())` — the multi-key set parses as a RowExpr, not a nested
    GroupingSet.

    Assuming the latter left `b` out of the keys entirely, so the statement
    failed with the very 42803 this change removes. Pinned because the shape is
    not guessable from the grammar.
    """
    with _Server(home) as server, server.connect() as conn:
        _gs_table(conn)
        cur = conn.cursor()
        cur.execute(
            "select a, b, sum(n) from s group by grouping sets ((a,b),()) "
            "order by a nulls last, b nulls last"
        )
        assert cur.fetchall() == [
            ("x", "p", 1),
            ("x", "q", 2),
            ("y", "p", 4),
            ("y", "q", 8),
            (None, None, 15),
        ]


def test_rollup_is_prefixes_longest_first(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _gs_table(conn)
        cur = conn.cursor()
        cur.execute(
            "select a, b, sum(n) from s group by rollup (a,b) order by a nulls last, b nulls last"
        )
        assert cur.fetchall() == [
            ("x", "p", 1),
            ("x", "q", 2),
            ("x", None, 3),
            ("y", "p", 4),
            ("y", "q", 8),
            ("y", None, 12),
            (None, None, 15),
        ]


def test_cube_is_every_subset(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _gs_table(conn)
        cur = conn.cursor()
        cur.execute(
            "select a, b, sum(n) from s group by cube (a,b) order by a nulls last, b nulls last"
        )
        assert len(cur.fetchall()) == 9  # 4 pairs + 2 a-only + 2 b-only + 1 total


def test_grouping_sets_keeps_a_duplicate_set(home: Path) -> None:
    """PostgreSQL does NOT deduplicate: each group comes back twice."""
    with _Server(home) as server, server.connect() as conn:
        _gs_table(conn)
        cur = conn.cursor()
        cur.execute("select a, sum(n) from s group by grouping sets ((a),(a)) order by a")
        assert cur.fetchall() == [("x", 3), ("x", 3), ("y", 12), ("y", 12)]


def test_grouping_sets_with_having_and_count(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _gs_table(conn)
        cur = conn.cursor()
        cur.execute(
            "select a, sum(n) from s group by grouping sets ((a),()) "
            "having sum(n) > 3 order by a nulls last"
        )
        assert cur.fetchall() == [("y", 12), (None, 15)]
        cur.execute(
            "select a, count(*) from s group by grouping sets ((a),()) order by a nulls last"
        )
        assert cur.fetchall() == [("x", 2), ("y", 2), (None, 4)]


def test_the_empty_grouping_set_alone(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _gs_table(conn)
        cur = conn.cursor()
        cur.execute("select sum(n) from s group by grouping sets (())")
        assert cur.fetchall() == [(15,)]


def test_the_grouping_function(home: Path) -> None:
    """`GROUPING(col)` is 1 on the rows a rollup produced WITHOUT that key --
    each group carries the grouping set that produced it (PostgreSQL 14)."""
    with _Server(home) as server, server.connect() as conn:
        _gs_table(conn)
        cur = conn.cursor()
        cur.execute(
            "select a, grouping(a), sum(n) from s group by rollup (a) order by a nulls last"
        )
        assert cur.fetchall() == [("x", 0, 3), ("y", 0, 12), (None, 1, 15)]


# --------------------------------------------------------------------------- #
# Pattern matching (`LIKE` / `ILIKE` / `~`) and `CASE`.
#
# All measured against PostgreSQL 14.13. `LIKE` and friends arrive as their OWN
# AExpr kind rather than as operators, so they never reached the operator path
# and every one of them answered `this operator form is not supported yet`.
# `CASE` was a missing arm in the VALUE evaluator only -- the column-reference
# walker already descended into it.
# --------------------------------------------------------------------------- #


def _pat_table(conn) -> None:
    cur = conn.cursor()
    cur.execute("create table p (a text, n int)")
    cur.execute("insert into p values ('abc',1),('ABC',2),('zed',3),(null,4)")


def test_like_matches_the_whole_string(home: Path) -> None:
    """SQL `LIKE` is anchored, unlike `~`: `'b'` does not match `'abc'`."""
    with _Server(home) as server, server.connect() as conn:
        _pat_table(conn)
        cur = conn.cursor()
        cur.execute("select a from p where a like 'a%' order by a")
        assert cur.fetchall() == [("abc",)]
        cur.execute("select a from p where a like 'b'")
        assert cur.fetchall() == []
        cur.execute("select a from p where a like '_bc' order by a")
        assert cur.fetchall() == [("abc",)]


def test_ilike_is_case_insensitive(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _pat_table(conn)
        cur = conn.cursor()
        cur.execute("select a from p where a ilike 'a%' order by a")
        assert cur.fetchall() == [("ABC",), ("abc",)]


def test_a_negated_pattern_match_excludes_null(home: Path) -> None:
    """The half that needed help.

    A NULL column matches no regex, so the positive form is right for free.
    But MQL's `$not` MATCHES a null or missing field, where PostgreSQL's
    `NOT LIKE` over NULL is NULL and selects nothing. The first cut returned
    the NULL row and a comment asserted the opposite; the differential caught
    it.
    """
    with _Server(home) as server, server.connect() as conn:
        _pat_table(conn)
        cur = conn.cursor()
        cur.execute("select a from p where a not like 'a%' order by a")
        assert cur.fetchall() == [("ABC",), ("zed",)]
        cur.execute("select a from p where a !~ '^a' order by a")
        assert cur.fetchall() == [("ABC",), ("zed",)]


def test_like_metacharacters_are_literal(home: Path) -> None:
    """Everything but `%` and `_` is literal, so a regex metacharacter in the
    pattern must be escaped or `'a.c'` would match `'abc'`."""
    with _Server(home) as server, server.connect() as conn:
        _pat_table(conn)
        cur = conn.cursor()
        cur.execute("select a from p where a like 'a.c'")
        assert cur.fetchall() == []
        cur.execute("select 'a%b' like 'a\\%b'")
        assert cur.fetchone()[0] is True


def test_like_honours_an_explicit_escape(home: Path) -> None:
    """`LIKE p ESCAPE e` folds into a `like_escape(p, e)` CALL, not a third
    operand — reading that call is what makes ESCAPE work at all."""
    with _Server(home) as server, server.connect() as conn:
        _pat_table(conn)
        cur = conn.cursor()
        cur.execute("select 'a%b' like 'a#%b' escape '#'")
        assert cur.fetchone()[0] is True
        cur.execute("select 'axb' like 'a#%b' escape '#'")
        assert cur.fetchone()[0] is False


def test_the_regex_operators(home: Path) -> None:
    """`~` matches anywhere, and `~*` ignores case."""
    with _Server(home) as server, server.connect() as conn:
        _pat_table(conn)
        cur = conn.cursor()
        cur.execute("select a from p where a ~ '^a' order by a")
        assert cur.fetchall() == [("abc",)]
        cur.execute("select a from p where a ~* '^a' order by a")
        assert cur.fetchall() == [("ABC",), ("abc",)]
        cur.execute("select a from p where a ~ 'b' order by a")
        assert cur.fetchall() == [("abc",)]


def test_like_as_a_value_not_just_a_predicate(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _pat_table(conn)
        cur = conn.cursor()
        cur.execute("select a, a like 'a%' from p order by n")
        assert cur.fetchall() == [("abc", True), ("ABC", False), ("zed", False), (None, None)]


def test_case_in_both_forms(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _pat_table(conn)
        cur = conn.cursor()
        cur.execute("select case when n=1 then 'one' else 'other' end from p order by n")
        assert cur.fetchall() == [("one",), ("other",), ("other",), ("other",)]
        cur.execute("select case n when 1 then 'a' when 2 then 'b' else 'z' end from p order by n")
        assert cur.fetchall() == [("a",), ("b",), ("z",), ("z",)]


def test_case_without_else_is_null(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _pat_table(conn)
        cur = conn.cursor()
        cur.execute("select case when n=1 then 'one' end from p order by n")
        assert cur.fetchall() == [("one",), (None,), (None,), (None,)]


def test_case_takes_only_a_true_branch(home: Path) -> None:
    """NULL is not TRUE, so a NULL condition falls through — the same
    three-valued rule a WHERE uses. And a NULL subject in the simple form
    matches nothing, not even `WHEN NULL`, because `NULL = NULL` is NULL."""
    with _Server(home) as server, server.connect() as conn:
        _pat_table(conn)
        cur = conn.cursor()
        cur.execute("select case when null then 'yes' else 'no' end")
        assert cur.fetchone()[0] == "no"
        cur.execute("select case a when null then 'matched' else 'no' end from p where n = 4")
        assert cur.fetchone()[0] == "no"


def test_case_nests_inside_a_function(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _pat_table(conn)
        cur = conn.cursor()
        cur.execute("select upper(case when n=1 then 'x' else 'y' end) from p order by n")
        assert cur.fetchall() == [("X",), ("Y",), ("Y",), ("Y",)]


# --------------------------------------------------------------------------- #
# `ORDER BY` over an expression, and a WHERE that does not lower to MQL.
#
# Both were `0A000`. Measured against PostgreSQL 14.13.
# --------------------------------------------------------------------------- #


def _expr_table(conn) -> None:
    cur = conn.cursor()
    cur.execute("create table e (a text, n int)")
    cur.execute("insert into e values ('abc',1),('ABC',2),(null,3)")


def test_order_by_a_computed_expression(home: Path) -> None:
    """The expression is materialised per row into a synthetic field, so the
    sort stays one comparison routine rather than growing a second path."""
    with _Server(home) as server, server.connect() as conn:
        _expr_table(conn)
        cur = conn.cursor()
        cur.execute("select n from e order by n * -1")
        assert cur.fetchall() == [(3,), (2,), (1,)]
        # `upper('abc')` and `upper('ABC')` are BOTH `'ABC'`, so those two rows
        # tie and their relative order is not determined by the SQL. An
        # explicit tiebreaker keeps the assertion about what the clause
        # actually specifies: the NULL sorts last. The first cut of this test
        # asserted a tie order copied from the two-key query below, and failed.
        cur.execute("select n from e order by upper(a) nulls last, n")
        assert cur.fetchall() == [(1,), (2,), (3,)]


def test_order_by_two_expressions_do_not_collide(home: Path) -> None:
    """Synthetic sort fields are named by POSITION, so two of them differ."""
    with _Server(home) as server, server.connect() as conn:
        _expr_table(conn)
        cur = conn.cursor()
        cur.execute("select n from e order by upper(a) nulls last, n * -1")
        assert cur.fetchall() == [(2,), (1,), (3,)]


def test_a_where_that_does_not_lower_becomes_a_residual(home: Path) -> None:
    """`where (case ... end)` has no MQL form, so it is evaluated per row."""
    with _Server(home) as server, server.connect() as conn:
        _expr_table(conn)
        cur = conn.cursor()
        cur.execute("select n from e where (case when n > 1 then true else false end) order by n")
        assert cur.fetchall() == [(2,), (3,)]


def test_a_residual_keeps_only_true(home: Path) -> None:
    """SQL's three-valued logic: a NULL predicate excludes the row, as a
    lowered filter would — not the `NULL is falsey so keep it` mistake."""
    with _Server(home) as server, server.connect() as conn:
        _expr_table(conn)
        cur = conn.cursor()
        cur.execute(
            "select n from e where (case when a is null then null else true end) order by n"
        )
        assert cur.fetchall() == [(1,), (2,)]


def test_the_residual_fallback_does_not_swallow_real_errors(home: Path) -> None:
    """Only `Unsupported` falls back to a residual.

    An undefined column stays `42703`, including INSIDE a CASE predicate —
    otherwise a typo would become a silent full scan that returns nothing,
    trading a loud error for a wrong answer.
    """
    with _Server(home) as server, server.connect() as conn:
        _expr_table(conn)
        cur = conn.cursor()
        with pytest.raises(psycopg.Error) as info:
            cur.execute("select * from e where nosuchcol = 1")
        assert info.value.sqlstate == "42703"
        conn.rollback()
        with pytest.raises(psycopg.Error) as info:
            cur.execute("select * from e where (case when nosuchcol=1 then true else false end)")
        assert info.value.sqlstate == "42703"


def _dept_emp(conn: psycopg.Connection) -> None:
    """Two related tables, for the subquery and CTE tests below.

    `dan` has a NULL salary and `empty` has no employees on purpose: those are
    the rows that separate `NOT IN`'s three-valued answer from a naive one, and
    an anti-join from an inner one.
    """
    cur = conn.cursor()
    cur.execute("CREATE TABLE sq_dept (id int PRIMARY KEY, name text, budget int)")
    cur.execute("CREATE TABLE sq_emp (id int PRIMARY KEY, dept_id int, name text, salary int)")
    cur.execute("INSERT INTO sq_dept VALUES (1,'eng',1000),(2,'sales',500),(3,'empty',0)")
    cur.execute(
        "INSERT INTO sq_emp VALUES (1,1,'ann',100),(2,1,'bob',200),(3,2,'cat',150),(4,2,'dan',NULL)"
    )


@pytest.mark.parametrize(
    "sql,expected",
    [
        # Scalar subqueries, with and without a FROM around them.
        ("SELECT (SELECT 1)", [(1,)]),
        ("SELECT (SELECT 1) + 2", [(3,)]),
        ("SELECT (SELECT max(salary) FROM sq_emp)", [(200,)]),
        # No rows is a NULL VALUE, not zero rows.
        ("SELECT (SELECT name FROM sq_dept WHERE id = 99)", [(None,)]),
        (
            "SELECT id, (SELECT count(*) FROM sq_emp) FROM sq_dept ORDER BY id",
            [(1, 4), (2, 4), (3, 4)],
        ),
        # EXISTS, over a non-empty and an empty subquery.
        ("SELECT 1 WHERE EXISTS (SELECT 1 FROM sq_dept)", [(1,)]),
        ("SELECT 1 WHERE EXISTS (SELECT 1 FROM sq_dept WHERE id = 99)", []),
        ("SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM sq_dept WHERE id = 99)", [(1,)]),
        # IN / NOT IN over a subquery.
        (
            "SELECT id FROM sq_dept WHERE id IN (SELECT dept_id FROM sq_emp) ORDER BY id",
            [(1,), (2,)],
        ),
        ("SELECT id FROM sq_dept WHERE id NOT IN (SELECT dept_id FROM sq_emp) ORDER BY id", [(3,)]),
        # A NULL anywhere in a NOT IN subquery makes the whole predicate NULL,
        # so PostgreSQL returns NOTHING -- not "every row that isn't listed".
        ("SELECT id FROM sq_dept WHERE id NOT IN (SELECT salary FROM sq_emp) ORDER BY id", []),
        # An EMPTY subquery: IN matches nothing, NOT IN matches everything.
        (
            "SELECT id FROM sq_dept WHERE id IN (SELECT dept_id FROM sq_emp WHERE salary > 9999)",
            [],
        ),
        (
            "SELECT id FROM sq_dept WHERE id NOT IN "
            "(SELECT dept_id FROM sq_emp WHERE salary > 9999) ORDER BY id",
            [(1,), (2,), (3,)],
        ),
        # ANY / ALL, which `IN` and `NOT IN` are spellings of.
        (
            "SELECT id FROM sq_dept WHERE id = ANY (SELECT dept_id FROM sq_emp) ORDER BY id",
            [(1,), (2,)],
        ),
        (
            "SELECT id FROM sq_dept WHERE budget > ALL "
            "(SELECT salary FROM sq_emp WHERE salary IS NOT NULL) ORDER BY id",
            [(1,), (2,)],
        ),
        # An empty ALL is vacuously true, an empty ANY vacuously false.
        (
            "SELECT id FROM sq_dept WHERE budget > ALL "
            "(SELECT salary FROM sq_emp WHERE salary > 9999) ORDER BY id",
            [(1,), (2,), (3,)],
        ),
        (
            "SELECT id FROM sq_dept WHERE budget > ANY "
            "(SELECT salary FROM sq_emp WHERE salary > 9999)",
            [],
        ),
        # Nested: a subquery inside a subquery.
        (
            "SELECT id FROM sq_dept WHERE id IN "
            "(SELECT dept_id FROM sq_emp WHERE salary > (SELECT 120)) ORDER BY id",
            [(1,), (2,)],
        ),
        ("SELECT ARRAY(SELECT id FROM sq_dept ORDER BY id)", [([1, 2, 3],)]),
        # A subquery in a HAVING.
        (
            "SELECT dept_id, count(*) FROM sq_emp GROUP BY dept_id "
            "HAVING count(*) > (SELECT 1) ORDER BY dept_id",
            [(1, 2), (2, 2)],
        ),
    ],
)
def test_uncorrelated_subqueries_match_postgres(
    home: Path, sql: str, expected: list[tuple]
) -> None:
    """Answers checked against a live PostgreSQL 14.13; PostgreSQL is the reference.

    An uncorrelated subquery is evaluated ONCE and replaced by the values it
    returned, which is what PostgreSQL does too -- so these are the shapes
    that prove the substitution keeps SQL's three-valued logic, not just its
    row counts.
    """
    with _Server(home) as server, server.connect() as conn:
        _dept_emp(conn)
        cur = conn.cursor()
        cur.execute(sql)
        assert cur.fetchall() == expected


@pytest.mark.parametrize(
    "sql,expected",
    [
        ("SELECT y FROM (SELECT 1 AS y) s", [(1,)]),
        ("SELECT s.n FROM (SELECT count(*) AS n FROM sq_emp) s", [(4,)]),
        (
            "SELECT s.dept_id, s.c FROM "
            "(SELECT dept_id, count(*) AS c FROM sq_emp GROUP BY dept_id) s ORDER BY s.dept_id",
            [(1, 2), (2, 2)],
        ),
        # A WHERE and an aggregate OVER the subquery, not inside it.
        (
            "SELECT s.c FROM (SELECT dept_id, count(*) AS c FROM sq_emp GROUP BY dept_id) s "
            "WHERE s.dept_id = 1",
            [(2,)],
        ),
        (
            "SELECT max(s.c) FROM (SELECT dept_id, count(*) AS c FROM sq_emp GROUP BY dept_id) s",
            [(2,)],
        ),
        # A column alias list renames the outputs positionally.
        ("SELECT a, b FROM (SELECT 1, 'x') s(a, b)", [(1, "x")]),
        # Joined to a real table.
        (
            "SELECT d.name, s.c FROM sq_dept d JOIN "
            "(SELECT dept_id, count(*) AS c FROM sq_emp GROUP BY dept_id) s ON s.dept_id = d.id "
            "ORDER BY d.name",
            [("eng", 2), ("sales", 2)],
        ),
        # Two subqueries cross-joined, referenced WITHOUT qualifiers. This
        # answered `(None, 2)` -- a wrong answer, not an error -- until the
        # unqualified reference learned to ask which side actually has the
        # column.
        ("SELECT x, y FROM (SELECT 1 AS x) a, (SELECT 2 AS y) b", [(1, 2)]),
        ("SELECT y, x FROM (SELECT 1 AS x) a, (SELECT 2 AS y) b", [(2, 1)]),
    ],
)
def test_from_subqueries_match_postgres(home: Path, sql: str, expected: list[tuple]) -> None:
    with _Server(home) as server, server.connect() as conn:
        _dept_emp(conn)
        cur = conn.cursor()
        cur.execute(sql)
        assert cur.fetchall() == expected


@pytest.mark.parametrize(
    "sql,expected",
    [
        ("WITH c AS (SELECT 1 AS x) SELECT x FROM c", [(1,)]),
        ("WITH c AS (SELECT id, name FROM sq_dept) SELECT name FROM c WHERE id = 1", [("eng",)]),
        (
            "WITH c AS (SELECT dept_id, count(*) AS n FROM sq_emp GROUP BY dept_id) "
            "SELECT * FROM c ORDER BY dept_id",
            [(1, 2), (2, 2)],
        ),
        ("WITH c AS (SELECT id FROM sq_dept WHERE budget > 400) SELECT count(*) FROM c", [(2,)]),
        # Two CTEs, cross-joined.
        ("WITH a AS (SELECT 1 AS x), b AS (SELECT 2 AS y) SELECT x, y FROM a, b", [(1, 2)]),
        # One CTE referencing the one declared before it.
        (
            "WITH a AS (SELECT id, budget FROM sq_dept), "
            "b AS (SELECT id FROM a WHERE budget > 400) SELECT count(*) FROM b",
            [(2,)],
        ),
        # A CTE joined to a real table.
        (
            "WITH c AS (SELECT dept_id, count(*) AS n FROM sq_emp GROUP BY dept_id) "
            "SELECT d.name, c.n FROM sq_dept d JOIN c ON c.dept_id = d.id ORDER BY d.name",
            [("eng", 2), ("sales", 2)],
        ),
        # An uncorrelated subquery INSIDE a CTE body: the CTE is inlined after
        # subqueries are resolved, so one left here would reach the lowering
        # unresolved and be refused.
        (
            "WITH c AS (SELECT id FROM sq_dept WHERE id IN (SELECT dept_id FROM sq_emp)) "
            "SELECT count(*) FROM c",
            [(2,)],
        ),
        # A CTE referenced under a different alias.
        ("WITH c AS (SELECT 1 AS x) SELECT z.x FROM c AS z", [(1,)]),
    ],
)
def test_ctes_match_postgres(home: Path, sql: str, expected: list[tuple]) -> None:
    """A non-recursive CTE is inlined, which is what PostgreSQL 12+ does too.

    Inlining and materialising return the same ROWS for a pure-SELECT body, so
    the answers are PostgreSQL's even though the plan is not.
    """
    with _Server(home) as server, server.connect() as conn:
        _dept_emp(conn)
        cur = conn.cursor()
        cur.execute(sql)
        assert cur.fetchall() == expected


def test_a_subquery_sees_the_transactions_own_uncommitted_writes(home: Path) -> None:
    """An uncorrelated subquery runs during PLANNING, which is outside the
    `with_user_transaction` scope the statement's execution runs in.

    Without entering the transaction for the read, WiredTiger served the
    subquery its own snapshot: `insert; select ... where id in (select ...)`
    counted the rows from BEFORE the insert while the same query without a
    subquery counted correctly.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE tq (id int PRIMARY KEY)")
        cur.execute("INSERT INTO tq VALUES (1), (2)")
        conn.commit()
        cur.execute("INSERT INTO tq VALUES (3)")
        cur.execute("SELECT count(*) FROM tq")
        assert cur.fetchall() == [(3,)]
        cur.execute("SELECT count(*) FROM tq WHERE id IN (SELECT id FROM tq)")
        assert cur.fetchall() == [(3,)]
        cur.execute("SELECT (SELECT count(*) FROM tq)")
        assert cur.fetchall() == [(3,)]
        conn.rollback()


def test_a_scalar_subquery_returning_two_rows_is_21000(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _dept_emp(conn)
        cur = conn.cursor()
        with pytest.raises(psycopg.Error) as info:
            cur.execute("SELECT (SELECT id FROM sq_dept)")
        assert info.value.sqlstate == "21000"


def test_an_undefined_column_inside_a_subquery_stays_42703(home: Path) -> None:
    """A typo must not be reported as an unsupported correlation.

    The correlation check asks whether the unresolved name is one the OUTER
    query has; a name neither side has is an ordinary `42703`, or a mistyped
    column would look like a missing feature.
    """
    with _Server(home) as server, server.connect() as conn:
        _dept_emp(conn)
        cur = conn.cursor()
        with pytest.raises(psycopg.Error) as info:
            cur.execute("SELECT id FROM sq_dept WHERE id IN (SELECT nosuchcol FROM sq_emp)")
        assert info.value.sqlstate == "42703"


@pytest.mark.parametrize(
    "sql,expected",
    [
        # An EXISTS whose subquery reads the outer row. This once ANSWERED --
        # with every row -- because the lowering resolves a column by its last
        # name part and ignores the qualifier, so `d.id` bound to `sq_emp`'s
        # own `id`. It is evaluated per outer row now; the qualifier check that
        # caught the wrong answer is what routes it there.
        (
            "SELECT id FROM sq_dept d WHERE EXISTS "
            "(SELECT 1 FROM sq_emp e WHERE e.dept_id = d.id) ORDER BY id",
            [(1,), (2,)],
        ),
        (
            "SELECT id FROM sq_dept d WHERE NOT EXISTS "
            "(SELECT 1 FROM sq_emp e WHERE e.dept_id = d.id) ORDER BY id",
            [(3,)],
        ),
        (
            "SELECT d.name, (SELECT count(*) FROM sq_emp e WHERE e.dept_id = d.id) "
            "FROM sq_dept d ORDER BY d.id",
            [("eng", 2), ("sales", 2), ("empty", 0)],
        ),
        # Correlated through an UNQUALIFIED name the inner table does not have.
        (
            "SELECT id FROM sq_dept WHERE EXISTS (SELECT 1 FROM sq_emp WHERE dept_id = budget)",
            [],
        ),
        # `x > ALL (no rows)` is TRUE even for a NULL `x`.
        (
            "SELECT id FROM sq_dept d WHERE d.budget > ALL "
            "(SELECT salary FROM sq_emp e WHERE e.dept_id = d.id) ORDER BY id",
            [(1,), (3,)],
        ),
    ],
)
def test_a_correlated_subquery_answers_per_row(home: Path, sql: str, expected: list[tuple]) -> None:
    """Matches PostgreSQL 14.13 on the same data."""
    with _Server(home) as server, server.connect() as conn:
        _dept_emp(conn)
        cur = conn.cursor()
        cur.execute(sql)
        assert cur.fetchall() == expected


def test_correlated_subqueries_in_update_and_delete(home: Path) -> None:
    """A correlated SET value, and a correlated WHERE that cannot lower to a
    filter -- narrowed per row before the write, never widened."""
    with _Server(home) as server, server.connect() as conn:
        _dept_emp(conn)
        cur = conn.cursor()
        cur.execute(
            "UPDATE sq_dept d SET budget = (SELECT count(*) FROM sq_emp e WHERE e.dept_id = d.id)"
        )
        cur.execute("SELECT id, budget FROM sq_dept ORDER BY id")
        assert cur.fetchall() == [(1, 2), (2, 2), (3, 0)]
        cur.execute(
            "DELETE FROM sq_dept d WHERE NOT EXISTS "
            "(SELECT 1 FROM sq_emp e WHERE e.dept_id = d.id) RETURNING id"
        )
        assert cur.fetchall() == [(3,)]


def test_a_data_modifying_with_runs_once(home: Path) -> None:
    """A write inside WITH runs exactly once, however many times it is
    referenced, and the query reads its RETURNING rows."""
    with _Server(home) as server, server.connect() as conn:
        _dept_emp(conn)
        cur = conn.cursor()
        cur.execute("WITH RECURSIVE c AS (SELECT 1 AS x) SELECT x FROM c")
        assert cur.fetchall() == [(1,)]
        cur.execute(
            "WITH c AS (INSERT INTO sq_dept VALUES (9,'x',1) RETURNING id) "
            "SELECT a.id, b.id FROM c a, c b"
        )
        assert cur.fetchall() == [(9, 9)]
        cur.execute("SELECT count(*) FROM sq_dept WHERE id = 9")
        assert cur.fetchall() == [(1,)]


def test_a_qualified_aggregate_argument_resolves_to_the_column(home: Path) -> None:
    """`max(t.n)` is `n` qualified by the relation.

    The aggregate planner read the FIRST name part, so every qualified
    aggregate argument answered `42703 column "t" does not exist` -- over a
    plain table as much as over a subquery.
    """
    with _Server(home) as server, server.connect() as conn:
        _dept_emp(conn)
        cur = conn.cursor()
        cur.execute("SELECT max(sq_emp.salary), count(sq_emp.id) FROM sq_emp")
        assert cur.fetchall() == [(200, 4)]


def test_subqueries_work_in_update_and_delete_predicates(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _dept_emp(conn)
        cur = conn.cursor()
        cur.execute(
            "UPDATE sq_dept SET budget = 1 WHERE id IN "
            "(SELECT dept_id FROM sq_emp WHERE salary > 180)"
        )
        cur.execute("SELECT id, budget FROM sq_dept ORDER BY id")
        assert cur.fetchall() == [(1, 1), (2, 500), (3, 0)]
        cur.execute("DELETE FROM sq_emp WHERE dept_id IN (SELECT id FROM sq_dept WHERE budget = 1)")
        cur.execute("SELECT count(*) FROM sq_emp")
        assert cur.fetchall() == [(2,)]


def _windowed(conn: psycopg.Connection) -> None:
    """A table shaped so the window edge cases are reachable.

    `dan` has a NULL `v` and rows 2 and 3 TIE on it: the NULL separates the
    aggregates (which skip it) from `count(*)` (which does not), and the tie is
    what makes `rank` differ from `row_number` and a RANGE frame differ from a
    ROWS one.
    """
    cur = conn.cursor()
    cur.execute("CREATE TABLE w9 (id int PRIMARY KEY, g text, v int)")
    cur.execute(
        "INSERT INTO w9 VALUES (1,'a',10),(2,'a',20),(3,'a',20),(4,'b',5),(5,'b',NULL),(6,'b',30)"
    )


@pytest.mark.parametrize(
    "sql,expected",
    [
        # Ranking. `rank` skips after a tie, `dense_rank` does not, and
        # `row_number` never ties. NULL sorts LAST ascending in PostgreSQL.
        (
            "SELECT id, row_number() OVER (ORDER BY id) FROM w9 ORDER BY id",
            [(1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (6, 6)],
        ),
        (
            "SELECT id, rank() OVER (ORDER BY v), dense_rank() OVER (ORDER BY v) "
            "FROM w9 ORDER BY id",
            [(1, 2, 2), (2, 3, 3), (3, 3, 3), (4, 1, 1), (5, 6, 5), (6, 5, 4)],
        ),
        (
            "SELECT id, rank() OVER (ORDER BY v NULLS FIRST) FROM w9 ORDER BY id",
            [(1, 3), (2, 4), (3, 4), (4, 2), (5, 1), (6, 6)],
        ),
        # The DEFAULT frame is RANGE UNBOUNDED PRECEDING TO CURRENT ROW, so a
        # running sum gives TIED rows the SAME total -- ids 2 and 3 both see
        # each other. This is the single most important window behaviour to
        # get right, and the one a ROWS-shaped implementation gets wrong.
        (
            "SELECT id, sum(v) OVER (ORDER BY v) FROM w9 ORDER BY id",
            [(1, 15), (2, 55), (3, 55), (4, 5), (5, 85), (6, 85)],
        ),
        # No ORDER BY at all: every row is a peer, so the frame is the whole
        # partition and the same rule gives the partition total.
        (
            "SELECT id, sum(v) OVER (PARTITION BY g) FROM w9 ORDER BY id",
            [(1, 50), (2, 50), (3, 50), (4, 35), (5, 35), (6, 35)],
        ),
        # count(*) counts rows including the NULL; count(v) skips it.
        (
            "SELECT id, count(v) OVER (), count(*) OVER () FROM w9 ORDER BY id",
            [(i, 5, 6) for i in range(1, 7)],
        ),
        # lag / lead, with and without an explicit offset and default.
        (
            "SELECT id, lag(v) OVER (ORDER BY id), lead(v) OVER (ORDER BY id) FROM w9 ORDER BY id",
            [
                (1, None, 20),
                (2, 10, 20),
                (3, 20, 5),
                (4, 20, None),
                (5, 5, 30),
                (6, None, None),
            ],
        ),
        (
            "SELECT id, lag(v, 2, -7) OVER (ORDER BY id) FROM w9 ORDER BY id",
            [(1, -7), (2, -7), (3, 10), (4, 20), (5, 20), (6, 5)],
        ),
        # A ROWS frame counts ROWS, so a tie does NOT pull its partner in.
        (
            "SELECT id, sum(v) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING "
            "AND CURRENT ROW) FROM w9 ORDER BY id",
            [(1, 10), (2, 30), (3, 50), (4, 55), (5, 55), (6, 85)],
        ),
        (
            "SELECT id, sum(v) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) "
            "FROM w9 ORDER BY id",
            [(1, 30), (2, 50), (3, 45), (4, 25), (5, 35), (6, 30)],
        ),
        # A frame that reaches past the partition on both sides is EMPTY: the
        # aggregates answer NULL and count answers 0.
        (
            "SELECT id, sum(v) OVER (ORDER BY id ROWS BETWEEN 3 PRECEDING AND 2 PRECEDING), "
            "count(*) OVER (ORDER BY id ROWS BETWEEN 5 FOLLOWING AND 6 FOLLOWING) "
            "FROM w9 ORDER BY id LIMIT 1",
            [(1, None, 1)],
        ),
        # first_value / last_value under the DEFAULT frame -- the classic
        # surprise, because the frame ends at the current row's peers rather
        # than at the partition's end.
        (
            "SELECT id, first_value(v) OVER (ORDER BY id), last_value(v) OVER (ORDER BY id) "
            "FROM w9 ORDER BY id",
            [(1, 10, 10), (2, 10, 20), (3, 10, 20), (4, 10, 5), (5, 10, None), (6, 10, 30)],
        ),
        (
            "SELECT id, nth_value(v, 2) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING "
            "AND UNBOUNDED FOLLOWING) FROM w9 ORDER BY id",
            [(i, 20) for i in range(1, 7)],
        ),
        # ntile spreads the remainder over the EARLIEST buckets.
        (
            "SELECT id, ntile(4) OVER (ORDER BY id) FROM w9 ORDER BY id",
            [(1, 1), (2, 1), (3, 2), (4, 2), (5, 3), (6, 4)],
        ),
        # PARTITION BY plus ORDER BY, and a DESC order.
        (
            "SELECT id, row_number() OVER (PARTITION BY g ORDER BY v DESC) FROM w9 ORDER BY id",
            [(1, 3), (2, 1), (3, 2), (4, 3), (5, 1), (6, 2)],
        ),
        # A named window, in both spellings. `OVER w` and `OVER (w ...)` put
        # the reference in DIFFERENT parser fields, and reading only one of
        # them made `OVER w` lose its ORDER BY -- a whole-partition total where
        # PostgreSQL gives a running one.
        (
            "SELECT id, sum(v) OVER w FROM w9 WINDOW w AS (ORDER BY id) ORDER BY id",
            [(1, 10), (2, 30), (3, 50), (4, 55), (5, 55), (6, 85)],
        ),
        (
            "SELECT id, sum(v) OVER (w ORDER BY id) FROM w9 WINDOW w AS (PARTITION BY g) "
            "ORDER BY id",
            [(1, 10), (2, 30), (3, 50), (4, 5), (5, 5), (6, 35)],
        ),
        # EXCLUDE, all three forms, over a window with a tie. It is part of
        # the FRAME clause, so PostgreSQL needs one before it -- `over (order
        # by v exclude current row)` is a syntax error there as well as here,
        # and writing it that way made three corpus lines agree on the error
        # while testing nothing.
        (
            "SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN UNBOUNDED PRECEDING "
            "AND CURRENT ROW EXCLUDE CURRENT ROW) FROM w9 ORDER BY id",
            [(1, 5), (2, 35), (3, 35), (4, None), (5, 85), (6, 55)],
        ),
        (
            "SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN UNBOUNDED PRECEDING "
            "AND UNBOUNDED FOLLOWING EXCLUDE CURRENT ROW) FROM w9 ORDER BY id",
            [(1, 75), (2, 65), (3, 65), (4, 80), (5, 85), (6, 55)],
        ),
        (
            "SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN UNBOUNDED PRECEDING "
            "AND UNBOUNDED FOLLOWING EXCLUDE GROUP) FROM w9 ORDER BY id",
            [(1, 75), (2, 45), (3, 45), (4, 80), (5, 85), (6, 55)],
        ),
        # TIES drops the current row's PEERS but keeps the row itself, so it
        # is not simply a narrower frame.
        (
            "SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN UNBOUNDED PRECEDING "
            "AND UNBOUNDED FOLLOWING EXCLUDE TIES) FROM w9 ORDER BY id",
            [(1, 85), (2, 65), (3, 65), (4, 85), (5, 85), (6, 85)],
        ),
        # GROUPS counts PEER GROUPS rather than rows, so the tie counts once.
        (
            "SELECT id, sum(v) OVER (ORDER BY v GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW) "
            "FROM w9 ORDER BY id",
            [(1, 15), (2, 50), (3, 50), (4, 5), (5, 30), (6, 70)],
        ),
        # RANGE with a VALUE offset: the bound is the current row's value
        # shifted, not a row count.
        (
            "SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN 5 PRECEDING AND 5 FOLLOWING) "
            "FROM w9 ORDER BY id",
            [(1, 15), (2, 40), (3, 40), (4, 15), (5, None), (6, 30)],
        ),
        # FILTER removes a row from the AGGREGATION, not from the output.
        (
            "SELECT id, count(*) FILTER (WHERE v > 10) OVER (ORDER BY id) FROM w9 ORDER BY id",
            [(1, 0), (2, 1), (3, 2), (4, 2), (5, 2), (6, 3)],
        ),
        # The aggregate family as windows, through the ordinary accumulator.
        (
            "SELECT id, min(v) OVER (ORDER BY id), max(v) OVER (ORDER BY id) "
            "FROM w9 ORDER BY id LIMIT 3",
            [(1, 10, 10), (2, 10, 20), (3, 10, 20)],
        ),
        (
            "SELECT id, string_agg(g, '-') OVER (ORDER BY id) FROM w9 ORDER BY id LIMIT 3",
            [(1, "a"), (2, "a-a"), (3, "a-a-a")],
        ),
        (
            "SELECT id, array_agg(v) OVER (ORDER BY id) FROM w9 ORDER BY id LIMIT 2",
            [(1, [10]), (2, [10, 20])],
        ),
        # The WHERE runs BEFORE the window, the ORDER BY / LIMIT after it.
        (
            "SELECT id, count(*) OVER () FROM w9 WHERE v IS NOT NULL ORDER BY id",
            [(1, 5), (2, 5), (3, 5), (4, 5), (6, 5)],
        ),
        (
            "SELECT id, row_number() OVER (ORDER BY id) AS rn FROM w9 ORDER BY id DESC LIMIT 2",
            [(6, 6), (5, 5)],
        ),
    ],
)
def test_window_functions_match_postgres(home: Path, sql: str, expected: list[tuple]) -> None:
    """Answers checked against a live PostgreSQL 14.13; PostgreSQL is the reference."""
    with _Server(home) as server, server.connect() as conn:
        _windowed(conn)
        cur = conn.cursor()
        cur.execute(sql)
        assert cur.fetchall() == expected


def test_a_window_reports_postgres_column_types(home: Path) -> None:
    """`row_number()` is int8, `ntile` int4, `percent_rank` float8.

    The synthetic `__winN` field is not in any table, so a reader that rebuilt
    the def from the catalog could not type it and fell through to the varchar
    default -- every window value went over the wire as TEXT while its VALUE
    was right, which a row comparison alone does not catch.
    """
    with _Server(home) as server, server.connect() as conn:
        _windowed(conn)
        cur = conn.cursor()
        cur.execute(
            "SELECT row_number() OVER (), ntile(2) OVER (), percent_rank() OVER (ORDER BY id), "
            "sum(v) OVER (), avg(v) OVER () FROM w9"
        )
        oids = [c.type_code for c in cur.description]
        # int8, int4, float8, int8 (sum of int4 widens), numeric.
        assert oids == [20, 23, 701, 20, 1700]


def test_windows_run_before_distinct_and_can_be_ordered_by_their_alias(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _windowed(conn)
        cur = conn.cursor()
        cur.execute("SELECT DISTINCT count(*) OVER (PARTITION BY g) FROM w9 ORDER BY 1")
        assert cur.fetchall() == [(3,)]
        # `ORDER BY rn` names the OUTPUT column, which is the only way to sort
        # by a window's result.
        cur.execute("SELECT id, row_number() OVER (ORDER BY v) AS rn FROM w9 ORDER BY rn LIMIT 2")
        assert cur.fetchall() == [(4, 1), (1, 2)]


def test_a_window_is_visible_through_a_subquery_and_a_cte(home: Path) -> None:
    """The synthetic column has to reach the subquery's published def, or the
    query around it cannot name what the window computed."""
    with _Server(home) as server, server.connect() as conn:
        _windowed(conn)
        cur = conn.cursor()
        cur.execute(
            "SELECT rn FROM (SELECT row_number() OVER (ORDER BY id) AS rn FROM w9) s "
            "WHERE rn > 4 ORDER BY rn"
        )
        assert cur.fetchall() == [(5,), (6,)]
        cur.execute(
            "WITH c AS (SELECT id, rank() OVER (ORDER BY v) AS r FROM w9) "
            "SELECT id, r FROM c WHERE r = 1 ORDER BY id"
        )
        assert cur.fetchall() == [(4, 1)]


def test_order_by_an_output_alias_resolves_to_the_output(home: Path) -> None:
    """PostgreSQL's ORDER BY sees the select list's names, and prefers them.

    Independent of windows -- `select id as d ... order by d` answered
    `42703 column "d" does not exist` -- but it is what makes a window usable,
    since `ORDER BY rn` is the only way to sort by one.
    """
    with _Server(home) as server, server.connect() as conn:
        _windowed(conn)
        cur = conn.cursor()
        cur.execute("SELECT id * 2 AS d FROM w9 ORDER BY d LIMIT 2")
        assert cur.fetchall() == [(2,), (4,)]
        # The OUTPUT name wins over a table column of the same name.
        cur.execute("SELECT v AS id FROM w9 ORDER BY id NULLS FIRST LIMIT 2")
        assert cur.fetchall() == [(None,), (5,)]


def test_an_unknown_named_window_and_a_bad_ntile_are_named_errors(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _windowed(conn)
        cur = conn.cursor()
        with pytest.raises(psycopg.Error) as info:
            cur.execute("SELECT sum(v) OVER nosuchwindow FROM w9")
        assert info.value.sqlstate == "42704"
        conn.rollback()
        with pytest.raises(psycopg.Error) as info:
            cur.execute("SELECT ntile(0) OVER (ORDER BY id) FROM w9")
        # PostgreSQL gives ntile its OWN class rather than the generic 22023.
        assert info.value.sqlstate == "22014"


def test_a_window_over_an_aggregate_runs_over_the_grouped_rows(home: Path) -> None:
    """`sum(sum(v)) OVER (...)` beside a GROUP BY runs the window over the
    GROUPED rows -- planned as the grouping, then the window over it.

    It was refused by name before; before THAT, routed into the aggregate
    planner, it came out as `function sum() is not supported yet`, which was
    false. PostgreSQL 14.13 on the same rows answers what is asserted here.
    """
    with _Server(home) as server, server.connect() as conn:
        _windowed(conn)
        cur = conn.cursor()
        cur.execute("SELECT g, sum(sum(v)) OVER (ORDER BY g) FROM w9 GROUP BY g ORDER BY g")
        assert cur.fetchall() == [("a", 50), ("b", 85)]


def test_a_window_aggregate_no_longer_demands_a_group_by(home: Path) -> None:
    """The bug this change exists to remove.

    `sum(v) OVER (...)` is a WINDOW call, but `has_aggregate` matched it on
    the name alone and routed it into the aggregate planner, which answered
    `42803 column "id" must appear in the GROUP BY clause` -- blaming the
    user's query for a feature the server did not have.
    """
    with _Server(home) as server, server.connect() as conn:
        _windowed(conn)
        cur = conn.cursor()
        cur.execute("SELECT id, sum(v) OVER () FROM w9 ORDER BY id LIMIT 1")
        assert cur.fetchall() == [(1, 85)]


@pytest.mark.parametrize(
    "sql,expected",
    [
        # Both bounds on ONE side of the current row. These are the shapes the
        # first implementation got wrong: it walked outward from the current
        # row and chose the direction from the OFFSET'S SIGN, when what
        # decides it is which BOUND is being resolved. `1 FOLLOWING AND 20
        # FOLLOWING` returned the whole partition.
        (
            "SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN 1 FOLLOWING AND 20 FOLLOWING) "
            "FROM w9 ORDER BY id",
            [(1, 70), (2, 30), (3, 30), (4, 50), (5, None), (6, None)],
        ),
        (
            "SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN 20 PRECEDING AND 1 PRECEDING) "
            "FROM w9 ORDER BY id",
            [(1, 5), (2, 15), (3, 15), (4, None), (5, None), (6, 50)],
        ),
        (
            "SELECT id, count(*) OVER (ORDER BY v RANGE BETWEEN 5 FOLLOWING AND 10 FOLLOWING) "
            "FROM w9 ORDER BY id",
            [(1, 2), (2, 1), (3, 1), (4, 1), (5, 1), (6, 0)],
        ),
        # A DESCENDING order: the bound is still `key + shift` once the key is
        # negated, so PRECEDING still means earlier in WINDOW order.
        (
            "SELECT id, count(*) OVER (ORDER BY v DESC RANGE BETWEEN 1 FOLLOWING AND 20 FOLLOWING)"
            " FROM w9 ORDER BY id",
            [(1, 1), (2, 2), (3, 2), (4, 0), (5, 1), (6, 3)],
        ),
        # A zero-width frame is the current row AND ITS PEERS, not just the row.
        (
            "SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN 0 PRECEDING AND 0 FOLLOWING) "
            "FROM w9 ORDER BY id",
            [(1, 10), (2, 40), (3, 40), (4, 5), (5, None), (6, 30)],
        ),
        # GROUPS counts peer GROUPS the same way.
        (
            "SELECT id, count(*) OVER (ORDER BY v GROUPS BETWEEN 1 FOLLOWING AND 2 FOLLOWING) "
            "FROM w9 ORDER BY id",
            [(1, 3), (2, 2), (3, 2), (4, 3), (5, 0), (6, 1)],
        ),
    ],
)
def test_one_sided_range_and_groups_frames_match_postgres(
    home: Path, sql: str, expected: list[tuple]
) -> None:
    """Answers checked against a live PostgreSQL 14.13.

    These were found by probing a shape NEITHER of the first two window
    corpora reached -- every one of them was a wrong answer, silently, while
    83 other lines agreed.
    """
    with _Server(home) as server, server.connect() as conn:
        _windowed(conn)
        cur = conn.cursor()
        cur.execute(sql)
        assert cur.fetchall() == expected


def _altered(conn: psycopg.Connection) -> None:
    """A table with rows already in it, which is what makes ALTER interesting.

    Row 2 has a NULL `n` and row 3 a NULL `s`, so `SET NOT NULL` has something
    to refuse and a `CHECK` has something to be violated by.
    """
    cur = conn.cursor()
    cur.execute("CREATE TABLE a20 (id int PRIMARY KEY, n int, s text)")
    cur.execute("INSERT INTO a20 VALUES (1, 10, 'x'), (2, NULL, 'y'), (3, 30, NULL)")


def test_add_column_fills_the_rows_already_there(home: Path) -> None:
    """PostgreSQL shows the DEFAULT on rows that predate the column.

    The rows are rewritten rather than left short a field: a read would
    otherwise have to treat a MISSING field as "the default" rather than as
    NULL, and the two are different for a column added without one.
    """
    with _Server(home) as server, server.connect() as conn:
        _altered(conn)
        cur = conn.cursor()
        cur.execute("ALTER TABLE a20 ADD COLUMN c1 text")
        cur.execute("SELECT id, c1 FROM a20 ORDER BY id")
        assert cur.fetchall() == [(1, None), (2, None), (3, None)]
        cur.execute("ALTER TABLE a20 ADD COLUMN c2 int DEFAULT 7")
        cur.execute("SELECT id, c2 FROM a20 ORDER BY id")
        assert cur.fetchall() == [(1, 7), (2, 7), (3, 7)]


def test_a_dropped_column_does_not_come_back_when_re_added(home: Path) -> None:
    """The field goes with the column.

    Leaving it in the stored rows would be invisible while the catalog no
    longer named it -- and then `ADD COLUMN` under the same name would
    resurrect the OLD values, which is a wrong answer no error would flag.
    """
    with _Server(home) as server, server.connect() as conn:
        _altered(conn)
        cur = conn.cursor()
        cur.execute("ALTER TABLE a20 ADD COLUMN c int DEFAULT 7")
        cur.execute("ALTER TABLE a20 DROP COLUMN c")
        cur.execute("ALTER TABLE a20 ADD COLUMN c int")
        cur.execute("SELECT id, c FROM a20 ORDER BY id")
        assert cur.fetchall() == [(1, None), (2, None), (3, None)]


def test_alter_validates_against_the_rows_already_there(home: Path) -> None:
    """A NOT NULL or a CHECK that the existing rows fail refuses the ALTER."""
    with _Server(home) as server, server.connect() as conn:
        _altered(conn)
        cur = conn.cursor()
        # `n` has a NULL, so SET NOT NULL is 23502.
        with pytest.raises(psycopg.Error) as info:
            cur.execute("ALTER TABLE a20 ALTER COLUMN n SET NOT NULL")
        assert info.value.sqlstate == "23502"
        conn.rollback()
        # A NOT NULL column with no default cannot be ADDED to a table that
        # has rows: every one of them would violate it at once.
        with pytest.raises(psycopg.Error) as info:
            cur.execute("ALTER TABLE a20 ADD COLUMN c4 int NOT NULL")
        assert info.value.sqlstate == "23502"
        conn.rollback()
        with pytest.raises(psycopg.Error) as info:
            cur.execute("ALTER TABLE a20 ADD CONSTRAINT ck CHECK (id > 100)")
        assert info.value.sqlstate == "23514"
        conn.rollback()
        # One the rows satisfy is accepted, and then enforced on writes.
        cur.execute("ALTER TABLE a20 ADD CONSTRAINT ck_pos CHECK (id > 0)")
        with pytest.raises(psycopg.Error) as info:
            cur.execute("INSERT INTO a20 (id) VALUES (-1)")
        assert info.value.sqlstate == "23514"
        conn.rollback()
        cur.execute("ALTER TABLE a20 DROP CONSTRAINT ck_pos")
        cur.execute("INSERT INTO a20 (id) VALUES (-1)")
        cur.execute("SELECT count(*) FROM a20 WHERE id = -1")
        assert cur.fetchall() == [(1,)]


@pytest.mark.parametrize(
    "from_type,to_type,allowed",
    [
        # PostgreSQL decides this from the TYPES, before looking at a row: the
        # conversion is allowed only where an assignment cast exists. Measured
        # across 31 pairs on 14.24; these are the representative ones.
        ("int", "text", True),
        ("int", "bigint", True),
        ("int", "numeric", True),
        ("numeric", "int", True),
        ("date", "timestamp", True),
        ("date", "text", True),
        ("json", "jsonb", True),
        ("text", "varchar(3)", True),
        # FROM a string to anything but a string needs USING -- even when
        # every value would convert cleanly, which is why this cannot be
        # decided by trying the cast per row.
        ("text", "int", False),
        ("text", "date", False),
        ("text", "json", False),
        ("bool", "int", False),
        ("int", "bool", False),
    ],
)
def test_alter_column_type_follows_postgres_cast_rule(
    home: Path, from_type: str, to_type: str, allowed: bool
) -> None:
    value = {
        "int": "1",
        "text": "'1'",
        "date": "'2020-01-01'",
        "json": "'1'",
        "bool": "true",
        "numeric": "1",
    }[from_type]
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(f"CREATE TABLE ct (v {from_type})")
        cur.execute(f"INSERT INTO ct VALUES ({value})")
        if allowed:
            cur.execute(f"ALTER TABLE ct ALTER COLUMN v TYPE {to_type}")
        else:
            with pytest.raises(psycopg.Error) as info:
                cur.execute(f"ALTER TABLE ct ALTER COLUMN v TYPE {to_type}")
            assert info.value.sqlstate == "42804"
            assert "cannot be cast automatically" in str(info.value)


def test_rename_moves_the_stored_field_except_for_the_primary_key(home: Path) -> None:
    """A PRIMARY KEY column is stored as `_id` whatever it is called.

    So renaming it moves no field, while renaming any other column has to --
    or the catalog would name a field no row carries.
    """
    with _Server(home) as server, server.connect() as conn:
        _altered(conn)
        cur = conn.cursor()
        cur.execute("ALTER TABLE a20 RENAME COLUMN s TO label")
        cur.execute("SELECT label FROM a20 WHERE id = 1")
        assert cur.fetchall() == [("x",)]
        cur.execute("ALTER TABLE a20 RENAME COLUMN id TO pk")
        cur.execute("SELECT pk, label FROM a20 WHERE pk = 1")
        assert cur.fetchall() == [(1, "x")]
        # And the renamed key still keys: a duplicate is still a duplicate.
        with pytest.raises(psycopg.Error) as info:
            cur.execute("INSERT INTO a20 (pk) VALUES (1)")
        assert info.value.sqlstate == "23505"


def test_rename_table_moves_the_rows_and_the_catalog(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _altered(conn)
        cur = conn.cursor()
        cur.execute("ALTER TABLE a20 RENAME TO a21")
        cur.execute("SELECT count(*) FROM a21")
        assert cur.fetchall() == [(3,)]
        cur.execute("INSERT INTO a21 (id) VALUES (9)")
        cur.execute("SELECT count(*) FROM a21")
        assert cur.fetchall() == [(4,)]
        with pytest.raises(psycopg.Error) as info:
            cur.execute("SELECT count(*) FROM a20")
        assert info.value.sqlstate == "42P01"
        conn.rollback()
        # Onto a name that is taken is 42P07.
        cur.execute("CREATE TABLE b20 (id int PRIMARY KEY)")
        with pytest.raises(psycopg.Error) as info:
            cur.execute("ALTER TABLE a21 RENAME TO b20")
        assert info.value.sqlstate == "42P07"


def test_alter_actions_apply_in_order_within_one_statement(home: Path) -> None:
    """`add column m int, alter column m set default 5` needs the second
    action to see the column the first one added."""
    with _Server(home) as server, server.connect() as conn:
        _altered(conn)
        cur = conn.cursor()
        cur.execute("ALTER TABLE a20 ADD COLUMN m int, ALTER COLUMN m SET DEFAULT 5")
        cur.execute("INSERT INTO a20 (id) VALUES (20)")
        cur.execute("SELECT m FROM a20 WHERE id = 20")
        assert cur.fetchall() == [(5,)]


@pytest.mark.parametrize(
    "sql,sqlstate",
    [
        ("ALTER TABLE nosuch20 ADD COLUMN x int", "42P01"),
        ("ALTER TABLE a20 ADD COLUMN id int", "42701"),
        ("ALTER TABLE a20 DROP COLUMN nope", "42703"),
        ("ALTER TABLE a20 ALTER COLUMN nope SET DEFAULT 1", "42703"),
        ("ALTER TABLE a20 DROP CONSTRAINT nope", "42704"),
        ("ALTER TABLE a20 RENAME COLUMN nope TO other", "42703"),
        ("ALTER TABLE a20 RENAME COLUMN s TO n", "42701"),
        ("ALTER TABLE nosuch20 RENAME TO other20", "42P01"),
        # A table with two columns of one name was accepted silently, and the
        # second was unreachable because every lookup takes the first match.
        ("CREATE TABLE bad20 (id int, id int)", "42701"),
    ],
)
def test_alter_error_surface_matches_postgres(home: Path, sql: str, sqlstate: str) -> None:
    with _Server(home) as server, server.connect() as conn:
        _altered(conn)
        cur = conn.cursor()
        with pytest.raises(psycopg.Error) as info:
            cur.execute(sql)
        assert info.value.sqlstate == sqlstate


def test_if_exists_and_if_not_exists_make_an_alter_a_no_op(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        _altered(conn)
        cur = conn.cursor()
        cur.execute("ALTER TABLE a20 ADD COLUMN IF NOT EXISTS s text")
        cur.execute("ALTER TABLE a20 DROP COLUMN IF EXISTS nope")
        cur.execute("ALTER TABLE a20 DROP CONSTRAINT IF EXISTS nope")
        cur.execute("ALTER TABLE IF EXISTS nosuch20 ADD COLUMN x int")
        cur.execute("SELECT count(*) FROM a20")
        assert cur.fetchall() == [(3,)]


def test_an_alter_rolls_back_with_its_transaction(home: Path) -> None:
    """Both halves: the catalog AND the rewritten rows.

    A `ROLLBACK TO` that put the catalog back but left the rows rewritten
    would describe the table with a shape its own rows do not have.
    """
    # NOT autocommit: the default here is autocommit, where `rollback()` is a
    # no-op and this would assert nothing at all.
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        _altered(conn)
        conn.commit()
        cur = conn.cursor()
        cur.execute("ALTER TABLE a20 ADD COLUMN c text DEFAULT 'd'")
        cur.execute("SELECT id, c FROM a20 ORDER BY id LIMIT 1")
        assert cur.fetchall() == [(1, "d")]
        conn.rollback()
        with pytest.raises(psycopg.Error) as info:
            cur.execute("SELECT c FROM a20")
        assert info.value.sqlstate == "42703"
        conn.rollback()
        # A savepoint rollback keeps the ALTER that preceded it.
        cur.execute("ALTER TABLE a20 ADD COLUMN c2 text DEFAULT 'e'")
        conn.commit()
        cur.execute("SAVEPOINT s1")
        cur.execute("ALTER TABLE a20 DROP COLUMN c2")
        cur.execute("ROLLBACK TO s1")
        cur.execute("SELECT c2 FROM a20 ORDER BY id LIMIT 1")
        assert cur.fetchall() == [("e",)]
        conn.commit()


def test_the_python_server_reads_a_rust_altered_catalog(home: Path) -> None:
    """The on-disk contract, which an ALTER is a new way to break.

    An added column with a default, a widened type and a renamed column all
    have to land in the catalog in the shape the PYTHON server reads -- and a
    row it writes afterwards has to read back through the Rust one.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE xr (id int PRIMARY KEY, n int)")
        cur.execute("INSERT INTO xr VALUES (1, 10), (2, 20)")
        cur.execute("ALTER TABLE xr ADD COLUMN tag text DEFAULT 'hi'")
        cur.execute("ALTER TABLE xr ALTER COLUMN n TYPE bigint")
        cur.execute("ALTER TABLE xr RENAME COLUMN n TO num")
    assert _python_sql(home, "SELECT id, num, tag FROM xr ORDER BY id") == [
        (1, 10, "hi"),
        (2, 20, "hi"),
    ]
    _python_sql(home, "INSERT INTO xr (id, num, tag) VALUES (3, 30, 'py')")
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT id, num, tag FROM xr ORDER BY id")
        assert cur.fetchall() == [(1, 10, "hi"), (2, 20, "hi"), (3, 30, "py")]


@pytest.mark.parametrize(
    "sql,expected",
    [
        # A fresh sequence reads as its START, not yet called.
        ("SELECT last_value, is_called FROM w1", [(1, False)]),
        ("SELECT nextval('w1'), nextval('w1')", [(1, 2)]),
        # currval is the value THIS session last drew.
        ("SELECT nextval('w1'), currval('w1')", [(1, 1)]),
        # setval's two-argument form leaves the sequence CALLED, so the next
        # draw is one past it; the three-argument `false` form does not.
        ("SELECT setval('w1', 50), nextval('w1')", [(50, 51)]),
        ("SELECT setval('w1', 50, false), nextval('w1')", [(50, 50)]),
        # A NULL argument is a NULL result, not an error.
        ("SELECT nextval(NULL)", [(None,)]),
        ("SELECT setval(NULL, 1)", [(None,)]),
    ],
)
def test_sequence_functions_match_postgres(home: Path, sql: str, expected: list[tuple]) -> None:
    """Answers checked against a live PostgreSQL 14.13."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE SEQUENCE w1")
        cur.execute(sql)
        assert cur.fetchall() == expected


def test_a_descending_sequence_starts_high_and_stops_at_its_minimum(home: Path) -> None:
    """The bound a sequence runs into depends on its DIRECTION.

    A descending sequence starts at its MAXIMUM and exhausts at its MINIMUM.
    Checking only `max_value` let one run past its floor for ever, and
    reported the wrong bound when it did stop.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE SEQUENCE d1 INCREMENT -3 MINVALUE -10 MAXVALUE -1")
        cur.execute("SELECT nextval('d1'), nextval('d1'), nextval('d1'), nextval('d1')")
        assert cur.fetchall() == [(-1, -4, -7, -10)]
        with pytest.raises(psycopg.Error) as info:
            cur.execute("SELECT nextval('d1')")
        assert info.value.sqlstate == "2200H"
        assert "reached minimum value" in str(info.value)


def test_cycle_wraps_to_the_far_bound(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE SEQUENCE c1 START 1 MAXVALUE 3 CYCLE")
        cur.execute("SELECT nextval('c1'), nextval('c1'), nextval('c1'), nextval('c1')")
        assert cur.fetchall() == [(1, 2, 3, 1)]
        cur.execute("CREATE SEQUENCE c2 INCREMENT -1 MINVALUE 1 MAXVALUE 3 CYCLE")
        cur.execute("SELECT nextval('c2'), nextval('c2'), nextval('c2'), nextval('c2')")
        assert cur.fetchall() == [(3, 2, 1, 3)]


@pytest.mark.parametrize(
    "sql,sqlstate",
    [
        ("SELECT nextval('nope_s')", "42P01"),
        ("SELECT setval('nope_s', 1)", "42P01"),
        ("DROP SEQUENCE nope_s", "42P01"),
        ("CREATE SEQUENCE e1 MINVALUE 5 MAXVALUE 2", "22023"),
        ("CREATE SEQUENCE e1 START 100 MINVALUE 1 MAXVALUE 10", "22023"),
        ("CREATE SEQUENCE e1 INCREMENT 0", "22023"),
    ],
)
def test_sequence_error_surface_matches_postgres(home: Path, sql: str, sqlstate: str) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        with pytest.raises(psycopg.Error) as info:
            cur.execute(sql)
        assert info.value.sqlstate == sqlstate


def test_currval_is_per_session_and_undefined_before_a_draw(home: Path) -> None:
    """PostgreSQL keys `currval` to the SESSION, so it is 55000 before any
    `nextval` in it -- reading the sequence's stored value instead would hand
    one session another's number."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE SEQUENCE p1")
        with pytest.raises(psycopg.Error) as info:
            cur.execute("SELECT currval('p1')")
        assert info.value.sqlstate == "55000"
        conn.rollback()
        cur.execute("SELECT nextval('p1')")
        cur.execute("SELECT currval('p1')")
        assert cur.fetchall() == [(1,)]
    # A SECOND session has drawn nothing, so it is 55000 there even though the
    # sequence has moved.
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        with pytest.raises(psycopg.Error) as info:
            cur.execute("SELECT currval('p1')")
        assert info.value.sqlstate == "55000"


def test_alter_sequence_applies_only_what_it_names(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE SEQUENCE a1 START 7")
        cur.execute("ALTER SEQUENCE a1 INCREMENT BY 100")
        cur.execute("SELECT nextval('a1'), nextval('a1')")
        # The start survived the increment change.
        assert cur.fetchall() == [(7, 107)]
        # Bare RESTART goes back to START; RESTART WITH sets it, and neither
        # counts as called, so the next draw IS that value.
        cur.execute("ALTER SEQUENCE a1 RESTART")
        cur.execute("SELECT nextval('a1')")
        assert cur.fetchall() == [(7,)]
        cur.execute("ALTER SEQUENCE a1 RESTART WITH 500")
        cur.execute("SELECT nextval('a1')")
        assert cur.fetchall() == [(500,)]


def test_a_serial_column_defines_currval_and_names_its_sequence(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE s1 (id serial PRIMARY KEY, big bigserial, n int)")
        cur.execute("INSERT INTO s1 (n) VALUES (1), (2), (3)")
        cur.execute("SELECT id, big FROM s1 ORDER BY id")
        assert cur.fetchall() == [(1, 1), (2, 2), (3, 3)]
        # An INSERT that drew from the sequence defines this session's
        # currval, which is how a client reads back the id it was given.
        cur.execute("SELECT currval('s1_id_seq'), currval('s1_big_seq')")
        assert cur.fetchall() == [(3, 3)]
        cur.execute("SELECT pg_get_serial_sequence('s1', 'id')")
        assert cur.fetchall() == [("public.s1_id_seq",)]
        # A column with no owned sequence is NULL, not an error.
        cur.execute("SELECT pg_get_serial_sequence('s1', 'n')")
        assert cur.fetchall() == [(None,)]


@pytest.mark.parametrize(
    "kind,sql,expected,sqlstate",
    [
        # The whole overriding matrix, measured on PostgreSQL 14.24. ALWAYS is
        # the only kind that refuses a hand-written value, and OVERRIDING USER
        # VALUE discards one for EITHER kind.
        ("ALWAYS", "INSERT INTO idt (id, v) VALUES (50, 1) RETURNING id", None, "428C9"),
        (
            "ALWAYS",
            "INSERT INTO idt (id, v) OVERRIDING SYSTEM VALUE VALUES (50, 1) RETURNING id",
            [(50,)],
            None,
        ),
        (
            "ALWAYS",
            "INSERT INTO idt (id, v) OVERRIDING USER VALUE VALUES (60, 1) RETURNING id",
            [(1,)],
            None,
        ),
        ("ALWAYS", "INSERT INTO idt (v) VALUES (1) RETURNING id", [(1,)], None),
        ("BY DEFAULT", "INSERT INTO idt (id, v) VALUES (70, 1) RETURNING id", [(70,)], None),
        (
            "BY DEFAULT",
            "INSERT INTO idt (id, v) OVERRIDING SYSTEM VALUE VALUES (80, 1) RETURNING id",
            [(80,)],
            None,
        ),
        (
            "BY DEFAULT",
            "INSERT INTO idt (id, v) OVERRIDING USER VALUE VALUES (90, 1) RETURNING id",
            [(1,)],
            None,
        ),
        ("BY DEFAULT", "INSERT INTO idt (v) VALUES (1) RETURNING id", [(1,)], None),
        # An identity column is NOT NULL, and an explicit NULL does not fall
        # back to the sequence -- for either kind, and even under OVERRIDING
        # SYSTEM VALUE, because the override decides whose value wins rather
        # than whether the column may be null.
        (
            "ALWAYS",
            "INSERT INTO idt (id, v) OVERRIDING SYSTEM VALUE VALUES (NULL, 1) RETURNING id",
            None,
            "23502",
        ),
        ("BY DEFAULT", "INSERT INTO idt (id, v) VALUES (NULL, 1) RETURNING id", None, "23502"),
    ],
)
def test_identity_columns_follow_postgres_overriding_rules(
    home: Path, kind: str, sql: str, expected: list[tuple] | None, sqlstate: str | None
) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(f"CREATE TABLE idt (id int GENERATED {kind} AS IDENTITY, v int)")
        if sqlstate is not None:
            with pytest.raises(psycopg.Error) as info:
                cur.execute(sql)
            assert info.value.sqlstate == sqlstate
        else:
            cur.execute(sql)
            assert cur.fetchall() == expected


def test_a_sequence_reads_as_a_relation(home: Path) -> None:
    """PostgreSQL's sequences are relations: `select last_value from s` works."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE SEQUENCE r1 START 10 INCREMENT 2")
        cur.execute("SELECT last_value, is_called FROM r1")
        assert cur.fetchall() == [(10, False)]
        cur.execute("SELECT nextval('r1'), nextval('r1')")
        cur.execute("SELECT last_value, is_called FROM r1")
        assert cur.fetchall() == [(12, True)]


def test_the_python_server_reads_a_rust_identity_table(home: Path) -> None:
    """`identity` is a catalog key the PYTHON server owns and this one did
    not model, so it was written back as NULL and erased on any rewrite.

    Unreachable until `ALTER TABLE` started rewriting catalog rows. Here the
    Rust server creates an identity table, the Python one writes to it, the
    Rust one ALTERs it, and the identity has to still be enforced afterwards.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE ident (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, v int)")
        cur.execute("INSERT INTO ident (v) VALUES (1), (2)")
    assert _python_sql(home, "SELECT id, v FROM ident ORDER BY id") == [(1, 1), (2, 2)]
    _python_sql(home, "INSERT INTO ident (v) VALUES (3)")
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("ALTER TABLE ident ADD COLUMN tag text DEFAULT 'x'")
        cur.execute("SELECT id, v, tag FROM ident ORDER BY id")
        assert cur.fetchall() == [(1, 1, "x"), (2, 2, "x"), (3, 3, "x")]
        # Still GENERATED ALWAYS after the rewrite.
        with pytest.raises(psycopg.Error) as info:
            cur.execute("INSERT INTO ident (id, v) VALUES (99, 9)")
        assert info.value.sqlstate == "428C9"


def _catalogued(conn: psycopg.Connection) -> None:
    """A table shaped so every information_schema column has something to say."""
    cur = conn.cursor()
    cur.execute("CREATE SEQUENCE cc_seq START 5 INCREMENT 2 MINVALUE 1 MAXVALUE 99 CYCLE")
    cur.execute(
        "CREATE TABLE cc_parent (id int PRIMARY KEY, code text UNIQUE, "
        "amount numeric(12,3), label varchar(20), flag bool NOT NULL DEFAULT false, note text)"
    )
    cur.execute("CREATE TABLE cc_child (cid serial PRIMARY KEY, pid int REFERENCES cc_parent(id))")


@pytest.mark.parametrize(
    "sql,expected",
    [
        (
            "SELECT column_name, data_type, is_nullable FROM information_schema.columns "
            "WHERE table_name='cc_parent' ORDER BY ordinal_position",
            [
                ("id", "integer", "NO"),
                ("code", "text", "YES"),
                ("amount", "numeric", "YES"),
                ("label", "character varying", "YES"),
                ("flag", "boolean", "NO"),
                ("note", "text", "YES"),
            ],
        ),
        # `numeric(12,3)` packs both numbers into one `atttypmod`.
        (
            "SELECT numeric_precision, numeric_scale FROM information_schema.columns "
            "WHERE table_name='cc_parent' AND column_name='amount'",
            [(12, 3)],
        ),
        # An unqualified integer reports its natural precision in BITS.
        (
            "SELECT numeric_precision, numeric_scale FROM information_schema.columns "
            "WHERE table_name='cc_parent' AND column_name='id'",
            [(32, 0)],
        ),
        (
            "SELECT character_maximum_length FROM information_schema.columns "
            "WHERE table_name='cc_parent' AND column_name='label'",
            [(20,)],
        ),
        # A default is rendered as a literal with its type stamped on.
        (
            "SELECT column_name, column_default FROM information_schema.columns "
            "WHERE table_name='cc_parent' AND column_default IS NOT NULL ORDER BY column_name",
            [("flag", "false")],
        ),
        # A serial column's default is the nextval CALL, not the stored value.
        (
            "SELECT column_default FROM information_schema.columns "
            "WHERE table_name='cc_child' AND column_name='cid'",
            [("nextval('cc_child_cid_seq'::regclass)",)],
        ),
        (
            "SELECT table_name, table_type FROM information_schema.tables "
            "WHERE table_name LIKE 'cc_%' ORDER BY table_name",
            [("cc_child", "BASE TABLE"), ("cc_parent", "BASE TABLE")],
        ),
        # PostgreSQL records a CHECK for every NOT NULL column, the PRIMARY
        # KEY column included -- so `cc_parent` has two, not one.
        (
            "SELECT constraint_type, count(*) FROM information_schema.table_constraints "
            "WHERE table_name='cc_parent' GROUP BY constraint_type ORDER BY constraint_type",
            [("CHECK", 2), ("PRIMARY KEY", 1), ("UNIQUE", 1)],
        ),
        # Only the KEY constraints appear in key_column_usage -- a CHECK names
        # no key column.
        (
            "SELECT column_name FROM information_schema.key_column_usage "
            "WHERE table_name='cc_parent' ORDER BY column_name",
            [("code",), ("id",)],
        ),
        (
            "SELECT sequence_name, data_type, start_value, minimum_value, maximum_value, "
            "increment, cycle_option FROM information_schema.sequences "
            "WHERE sequence_name='cc_seq'",
            [("cc_seq", "bigint", "5", "1", "99", "2", "YES")],
        ),
        # `relkind` is how a client tells a sequence from a table.
        ("SELECT relname, relkind FROM pg_class WHERE relname='cc_parent'", [("cc_parent", "r")]),
        ("SELECT relname, relkind FROM pg_class WHERE relname='cc_seq'", [("cc_seq", "S")]),
        ("SELECT relnatts FROM pg_class WHERE relname='cc_parent'", [(6,)]),
        (
            "SELECT nspname FROM pg_namespace WHERE nspname IN ('public','pg_catalog') "
            "ORDER BY nspname",
            [("pg_catalog",), ("public",)],
        ),
        # `pg_attribute` over a TABLE, which only listed composites before.
        (
            "SELECT attname, attnotnull FROM pg_attribute WHERE attrelid='cc_parent'::regclass "
            "AND attnum > 0 ORDER BY attnum",
            [
                ("id", True),
                ("code", False),
                ("amount", False),
                ("label", False),
                ("flag", True),
                ("note", False),
            ],
        ),
    ],
)
def test_catalog_views_match_postgres(home: Path, sql: str, expected: list[tuple]) -> None:
    """Answers checked against a live PostgreSQL 14.13.

    These views are what an ORM, a migration tool and `\\d` actually read, so
    the column NAMES and TYPES have to be right, not merely present.
    """
    with _Server(home) as server, server.connect() as conn:
        _catalogued(conn)
        cur = conn.cursor()
        cur.execute(sql)
        assert cur.fetchall() == expected


@pytest.mark.parametrize(
    "sql,expected",
    [
        # These already worked as a BARE select-list target, where they become
        # a `ConstCol` the server resolves. Inside an EXPRESSION the constant
        # evaluator reached them instead and had nowhere to ask, so every one
        # of them answered `0A000` -- which is exactly how a client uses them.
        ("SELECT version() LIKE 'PostgreSQL%'", [(True,)]),
        ("SELECT current_schema(), current_database() IS NOT NULL", [("public", True)]),
        ("SELECT current_setting('server_version_num') ~ '^[0-9]+$'", [(True,)]),
        ("SELECT current_setting('nosuch_guc_cc', true) IS NULL", [(True,)]),
        ("SELECT obj_description('cc_parent'::regclass) IS NULL", [(True,)]),
        # `format_type(oid, NULL)` is NOT null-propagating in its second
        # argument: a NULL typmod means "no modifier".
        (
            "SELECT format_type(23, NULL), format_type(1700, 655366), format_type(1043, 24)",
            [("integer", "numeric(10,2)", "character varying(20)")],
        ),
    ],
)
def test_catalog_functions_work_inside_an_expression(
    home: Path, sql: str, expected: list[tuple]
) -> None:
    with _Server(home) as server, server.connect() as conn:
        _catalogued(conn)
        cur = conn.cursor()
        cur.execute(sql)
        assert cur.fetchall() == expected


def test_an_unknown_guc_is_42704(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        with pytest.raises(psycopg.Error) as info:
            cur.execute("SELECT current_setting('nosuch_guc_cc')")
        assert info.value.sqlstate == "42704"


def test_a_user_table_may_be_called_columns(home: Path) -> None:
    """`information_schema`'s views are called `tables`, `columns`,
    `sequences` -- names a user table may perfectly well have.

    A virtual relation wins over the catalog, so registering them bare would
    make a user's own `columns` table unreachable. They keep their schema in
    the name instead, and both resolve.
    """
    with _Server(home) as server, server.connect() as conn:
        _catalogued(conn)
        cur = conn.cursor()
        cur.execute("CREATE TABLE columns (id int PRIMARY KEY, x int)")
        cur.execute("INSERT INTO columns VALUES (1, 10)")
        cur.execute("SELECT id, x FROM columns")
        assert cur.fetchall() == [(1, 10)]
        cur.execute(
            "SELECT count(*) > 0 FROM information_schema.columns WHERE table_name='cc_parent'"
        )
        assert cur.fetchall() == [(True,)]


def test_a_tables_row_type_does_not_double_its_pg_attribute_rows(home: Path) -> None:
    """A table's ROW TYPE is a composite under the same name and the same
    relation oid, so listing both put every column in twice -- once with
    `attnotnull` true and once false."""
    with _Server(home) as server, server.connect() as conn:
        _catalogued(conn)
        cur = conn.cursor()
        cur.execute(
            "SELECT count(*) FROM pg_attribute WHERE attrelid='cc_parent'::regclass AND attnum > 0"
        )
        assert cur.fetchall() == [(6,)]


# --- arrays: functions, containment operators, subscripting -----------------
#
# Every expectation below is PostgreSQL 14.13's own answer, taken from
# `tools/probes/pg_corpora/arrays{,2}.sql` run through
# `tools/probes/pg_differential.py`. The corpora are the wide net; these pin
# the cases whose rules are surprising enough that a future reader would
# otherwise "fix" them into agreement with intuition.


def test_array_dimension_functions_distinguish_empty_from_null(home: Path) -> None:
    """An EMPTY array and a NULL one answer differently, and `cardinality` is
    the one that separates them: it is 0 for the empty array where every other
    dimension function is NULL."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT array_length(ARRAY[]::int[],1), array_ndims(ARRAY[]::int[]),"
            " array_dims(ARRAY[]::int[]), cardinality(ARRAY[]::int[])"
        )
        assert cur.fetchall() == [(None, None, None, 0)]
        cur.execute(
            "SELECT array_length(NULL::int[],1), cardinality(NULL::int[]),"
            " array_ndims(NULL::int[]), array_dims(NULL::int[])"
        )
        assert cur.fetchall() == [(None, None, None, None)]


def test_dimension_functions_read_every_dimension(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT array_dims(ARRAY[[1,2],[3,4]]), array_ndims(ARRAY[[1,2],[3,4]]),"
            " cardinality(ARRAY[[1,2],[3,4]]), array_length(ARRAY[[1,2],[3,4]],2),"
            " array_upper(ARRAY[1,2],1), array_lower(ARRAY[1,2],1)"
        )
        assert cur.fetchall() == [("[1:2][1:2]", 2, 4, 2, 2, 1)]
        # A dimension the array does not have is NULL, not an error.
        cur.execute("SELECT array_length(ARRAY[1,2],0), array_length(ARRAY[1,2],2)")
        assert cur.fetchall() == [(None, None)]


def test_the_search_functions_match_null_to_null(home: Path) -> None:
    """`array_position` / `array_remove` / `array_replace` find a NULL, which
    is NOT how `@>` behaves — see the test below. The two rules look like one
    another's bug; PostgreSQL really does have both."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT array_position(ARRAY[1,NULL,2], NULL),"
            " array_positions(ARRAY[1,NULL], NULL),"
            " array_remove(ARRAY[1,NULL], NULL),"
            " array_replace(ARRAY[1,NULL], NULL, 9)"
        )
        assert cur.fetchall() == [(2, [2], [1], [1, 9])]


def test_containment_never_matches_a_null(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT ARRAY[1,NULL] @> ARRAY[NULL]::int[],"
            " ARRAY[1,NULL] <@ ARRAY[1,NULL],"
            " ARRAY[1,NULL] && ARRAY[NULL]::int[]"
        )
        assert cur.fetchall() == [(False, False, False)]
        # A NULL operand is a NULL answer; an empty right side is contained.
        cur.execute(
            "SELECT ARRAY[1] @> NULL::int[], ARRAY[1,2] @> ARRAY[]::int[],"
            " ARRAY[1,2] && ARRAY[2,9], ARRAY[1] && ARRAY[9]"
        )
        assert cur.fetchall() == [(None, True, True, False)]
        # Containment ignores dimensionality: both sides are flattened.
        cur.execute("SELECT ARRAY[[1,2],[3,4]] @> ARRAY[3]")
        assert cur.fetchall() == [(True,)]


def test_array_search_and_removal_refuse_a_multidimensional_array(home: Path) -> None:
    """PostgreSQL's own refusals, not this server's gaps — `array_replace`
    beside them works, because replacing cannot change the shape."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql in (
            "SELECT array_position(ARRAY[[1,2],[3,4]], 1)",
            "SELECT array_remove(ARRAY[[1,2],[3,4]], 1)",
        ):
            with pytest.raises(psycopg.Error):
                cur.execute(sql)
            conn.rollback()
        cur.execute("SELECT array_replace(ARRAY[[1,2],[3,4]], 1, 9)")
        assert cur.fetchall() == [([[9, 2], [3, 4]],)]


def test_array_to_string_skips_nulls_unless_given_a_null_string(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT array_to_string(ARRAY[1,NULL,3], ','),"
            " array_to_string(ARRAY[1,NULL,3], ',', 'X'),"
            " array_to_string(ARRAY[[1,2],[3,4]], ','),"
            " array_to_string(ARRAY[1,2], NULL)"
        )
        assert cur.fetchall() == [("1,3", "1,X,3", "1,2,3,4", None)]


def test_string_to_array_separator_shapes(home: Path) -> None:
    """Three shapes PostgreSQL treats differently: an EMPTY separator keeps the
    whole string, a NULL separator splits into characters, and an empty INPUT
    is the empty array whatever the separator."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT string_to_array('abc', ''), string_to_array('abc', NULL),"
            " string_to_array('', ','), string_to_array('a,b,,c', ','),"
            " string_to_array('a,b', ',', 'b'), string_to_array(NULL, ',')"
        )
        assert cur.fetchall() == [
            (["abc"], ["a", "b", "c"], [], ["a", "b", "", "c"], ["a", None], None)
        ]


def test_concatenation_takes_a_null_side_as_the_empty_one(home: Path) -> None:
    """`array_cat` and `array_append` are NOT null-propagating, which is what
    lets them fold over a nullable accumulator. `array_remove` beside them
    is."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT array_cat(NULL::int[], ARRAY[3]), array_cat(ARRAY[1], NULL::int[]),"
            " array_cat(NULL::int[], NULL::int[]), array_append(NULL::int[], 2),"
            " array_append(ARRAY[1], NULL::int), array_prepend(NULL::int, ARRAY[1]),"
            " array_remove(NULL::int[], 1)"
        )
        assert cur.fetchall() == [([3], [1], None, [2], [1, None], [None, 1], None)]


def test_concatenation_joins_by_dimensionality(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT array_cat(ARRAY[[1,2],[3,4]], ARRAY[5,6])")
        assert cur.fetchall() == [([[1, 2], [3, 4], [5, 6]],)]
        with pytest.raises(psycopg.Error):
            cur.execute("SELECT array_cat(ARRAY[1,2], ARRAY[[3,4,5]])")


def test_array_fill_keeps_its_lower_bound(home: Path) -> None:
    """`array_fill(7, ARRAY[2], ARRAY[3])` is `[3:4]={7,7}`: its text shows the
    bound and its subscripts start there, as PostgreSQL's do."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT array_fill(0, ARRAY[2,2]), array_fill(1, ARRAY[0])")
        assert cur.fetchall() == [([[0, 0], [0, 0]], [])]
        cur.execute("SELECT array_fill(7, ARRAY[2], ARRAY[1])")
        assert cur.fetchall() == [([7, 7],)]
        cur.execute(
            "SELECT array_fill(7, ARRAY[2], ARRAY[3])::text, (array_fill(7, ARRAY[2], ARRAY[3]))[3]"
        )
        assert cur.fetchall() == [("[3:4]={7,7}", 7)]


def test_subscripting_reads_elements_and_slices(home: Path) -> None:
    """A subscript out of range is NULL; a SLICE out of range is the EMPTY
    array. Two different answers to what looks like one question."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT (ARRAY[1,2,3])[1], (ARRAY[1,2,3])[0], (ARRAY[1,2,3])[9],"
            " (ARRAY[1,2,3])[2:3], (ARRAY[1,2,3])[2:], (ARRAY[1,2,3])[:2],"
            " (ARRAY[1,2,3])[5:9], (ARRAY[1,2,3])[3:1], (ARRAY[1,2,3])[0:1]"
        )
        assert cur.fetchall() == [(1, None, None, [2, 3], [2, 3], [1, 2], [], [], [1])]
        cur.execute("SELECT (NULL::int[])[1], (ARRAY[1,2])[NULL]")
        assert cur.fetchall() == [(None, None)]


def test_a_bare_index_beside_a_slice_means_one_to_n(home: Path) -> None:
    """PostgreSQL: once ANY subscript is a slice, a subscript written as a
    single number is "from 1 to the number specified".

    So `m[1:2][2]` is the WHOLE second dimension, not its second element —
    and `m[1:2][1]` agrees with the `n:n` reading, which is exactly why a
    probe that only tried `[1]` would call this correct.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT (ARRAY[[1,2],[3,4]])[1:2][1], (ARRAY[[1,2],[3,4]])[1:2][2],"
            " (ARRAY[[1,2],[3,4]])[2:2]"
        )
        assert cur.fetchall() == [([[1], [3]], [[1, 2], [3, 4]], [[3, 4]])]


def test_a_short_subscript_list_selects_nothing(home: Path) -> None:
    """`(ARRAY[[1,2],[3,4]])[1]` is NULL on PostgreSQL, not the inner row."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT (ARRAY[[1,2],[3,4]])[1], (ARRAY[[1,2],[3,4]])[1][2]")
        assert cur.fetchall() == [(None, 2)]


def test_a_subscript_carries_the_element_type_not_the_array_type(home: Path) -> None:
    """An element reference drops the `[]` and a slice keeps it, which the
    VALUE cannot say once a one-element slice has been taken. Described
    wrongly, `length(ta[1])` came back as the string `'1'`."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE arr_t (id int PRIMARY KEY, ia int[], ta text[])")
        cur.execute("INSERT INTO arr_t VALUES (1, ARRAY[1,2,3], ARRAY['a','b'])")
        cur.execute("SELECT length(ta[1]), ia[1] + 1, ta[1] || ta[2] FROM arr_t")
        assert cur.fetchall() == [(1, 2, "ab")]
        cur.execute("SELECT ia[1], ia[1:2] FROM arr_t")
        [element, slice_] = cur.description
        assert (element.type_code, slice_.type_code) == (23, 1007)


def test_a_subscript_bound_may_read_the_row(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE arr_n (id int PRIMARY KEY, ia int[], n int)")
        cur.execute("INSERT INTO arr_n VALUES (1, ARRAY[1,2,3], 2)")
        cur.execute("SELECT ia[n], ia[n:3], ia[1:n] FROM arr_n")
        assert cur.fetchall() == [(2, [2, 3], [1, 2])]


def test_assigning_into_an_array_extends_it_with_nulls(home: Path) -> None:
    """`SET a[i] = v` rewrites the stored array rather than replacing it, and a
    subscript past the end pads the gap with NULLs.

    Before this landed the bulk UPDATE path ran instead, wrote an empty `$set`
    and still answered `UPDATE 1` — a statement that reported success and
    changed nothing.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE arr_u (id int PRIMARY KEY, ia int[], m int[][])")
        cur.execute(
            "INSERT INTO arr_u VALUES (1, ARRAY[1,2,3], ARRAY[[1,2],[3,4]]), (2, NULL, NULL)"
        )
        cur.execute("UPDATE arr_u SET ia[2] = 99 WHERE id=1 RETURNING ia")
        assert cur.fetchall() == [([1, 99, 3],)]
        cur.execute("UPDATE arr_u SET ia[6] = 6 WHERE id=1 RETURNING ia")
        assert cur.fetchall() == [([1, 99, 3, None, None, 6],)]
        # Two assignments to one column in one statement: the second sees the
        # first, rather than both starting from the stored row.
        cur.execute("UPDATE arr_u SET ia[1] = 7, ia[2] = 8 WHERE id=1 RETURNING ia")
        assert cur.fetchall() == [([7, 8, 3, None, None, 6],)]
        cur.execute("UPDATE arr_u SET ia[2:3] = ARRAY[4,5] WHERE id=1 RETURNING ia")
        assert cur.fetchall() == [([7, 4, 5, None, None, 6],)]
        # Assigning into a NULL column builds the array from nothing.
        cur.execute("UPDATE arr_u SET ia[1] = 1 WHERE id=2 RETURNING ia")
        assert cur.fetchall() == [([1],)]
        cur.execute("UPDATE arr_u SET m[1][2] = 42 WHERE id=1 RETURNING m")
        assert cur.fetchall() == [([[1, 42], [3, 4]],)]
        # The rewrite is STORED, not only returned.
        cur.execute("SELECT ia, m FROM arr_u WHERE id=1")
        assert cur.fetchall() == [([7, 4, 5, None, None, 6], [[1, 42], [3, 4]])]


def test_assigning_below_subscript_1_moves_the_bound(home: Path) -> None:
    """PostgreSQL answers it by MOVING the array's lower bound -- `SET ia[0]=0`
    over `{1,2,3}` leaves `[0:3]={0,1,2,3}` -- and every other subscript
    keeps pointing where it did."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE arr_lb (id int PRIMARY KEY, ia int[])")
        cur.execute("INSERT INTO arr_lb VALUES (1, ARRAY[1,2,3])")
        cur.execute("UPDATE arr_lb SET ia[0] = 0 WHERE id=1")
        cur.execute("SELECT ia::text, ia[1] FROM arr_lb WHERE id=1")
        assert cur.fetchall() == [("[0:3]={0,1,2,3}", 1)]


def test_a_slice_assignment_source_must_fill_the_range(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE arr_s (id int PRIMARY KEY, ia int[])")
        cur.execute("INSERT INTO arr_s VALUES (1, ARRAY[1,2,3])")
        with pytest.raises(psycopg.Error):
            cur.execute("UPDATE arr_s SET ia[1:2] = ARRAY[1] WHERE id=1")
        conn.rollback()
        # A source LONGER than the range has its tail ignored.
        cur.execute("UPDATE arr_s SET ia[1:2] = ARRAY[8,9,10] WHERE id=1 RETURNING ia")
        assert cur.fetchall() == [([8, 9, 3],)]


def test_a_multidimensional_array_is_typed_as_its_element_array(home: Path) -> None:
    """`{{1,2},{3,4}}` is `int4[]` (oid 1007), never `int4[][]`, which is no
    type name at all. Reading only the first element's kind typed it `text[]`,
    and a binary-format client then refused the int rows as `_text`."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT array_fill(0, ARRAY[2,2])")
        assert cur.description[0].type_code == 1007
        assert cur.fetchall() == [([[0, 0], [0, 0]],)]
        # A LEADING NULL must not decide the element type either.
        cur.execute("SELECT array_prepend(NULL::int, ARRAY[1])")
        assert cur.description[0].type_code == 1007
        assert cur.fetchall() == [([None, 1],)]


def test_array_functions_report_their_type_when_the_value_is_null_or_empty(
    home: Path,
) -> None:
    """A NULL result cannot say its type and an EMPTY array says the wrong one,
    so both come from the CALL. `array_remove(NULL::int[], 1)` reported `text`
    and `string_to_array('', 'x')` reported `int4[]`."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT array_remove(NULL::int[], 1), array_cat(NULL::int[], NULL::int[])")
        assert [d.type_code for d in cur.description] == [1007, 1007]
        cur.execute("SELECT string_to_array('', 'x'), array_positions(ARRAY[1], 9)")
        assert [d.type_code for d in cur.description] == [1009, 1007]


def test_a_size_postgres_refuses_is_refused_before_it_is_allocated(home: Path) -> None:
    """`array_fill(1, ARRAY[1000000000])` and `SET a[1000000000] = 1` are each
    one line, and each SIZES an array from a user-supplied number.

    PostgreSQL caps an array at 134217727 elements and says so; a server that
    instead tried to build what was asked for would be exhaustible by a single
    statement. The cap is checked before any allocation.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE arr_big (id int PRIMARY KEY, ia int[])")
        cur.execute("INSERT INTO arr_big VALUES (1, ARRAY[1])")
        for sql in (
            "SELECT array_fill(1, ARRAY[1000000000])",
            "SELECT array_fill(1, ARRAY[20000, 20000])",
            "UPDATE arr_big SET ia[1000000000] = 1 WHERE id=1",
        ):
            with pytest.raises(psycopg.Error) as info:
                cur.execute(sql)
            assert "134217727" in str(info.value), sql
            conn.rollback()
        cur.execute("SELECT ia FROM arr_big WHERE id=1")
        assert cur.fetchall() == [([1],)]


# --- strings: padding, hex, translate, overlay, quoting, regex ---------------
#
# Every expectation is PostgreSQL 14.13's own answer, from
# `tools/probes/pg_corpora/strings2.sql` run through the differential.


def test_padding_truncates_and_counts_characters(home: Path) -> None:
    """`lpad` / `rpad` TRUNCATE when the target is shorter than the input, count
    CHARACTERS rather than bytes, and cannot pad at all with an empty fill."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT lpad('abc',5), lpad('abcdef',3), lpad('ab',7,'xy'), lpad('abc',0),"
            " lpad('abc',-1), lpad('abc',5,'')"
        )
        assert cur.fetchall() == [("  abc", "abc", "xyxyxab", "", "", "abc")]
        cur.execute("SELECT rpad('abc',5), rpad('abc',5,'xy'), rpad('abc',2)")
        assert cur.fetchall() == [("abc  ", "abcxy", "ab")]
        # Characters, not bytes: a two-byte codepoint still counts as one.
        cur.execute("SELECT lpad('λx',4,'.'), length(lpad('λx',4,'.'))")
        assert cur.fetchall() == [("..λx", 4)]


def test_to_hex_width_follows_the_argument_type(home: Path) -> None:
    """An `int4` -1 is `ffffffff`; an `int8` -1 is `ffffffffffffffff`. Read off
    the VALUE alone the two would collapse into one answer."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT to_hex(4294967295), to_hex(0), to_hex((-1)::int),"
            " to_hex((-1)::bigint), to_hex(255::bigint)"
        )
        assert cur.fetchall() == [("ffffffff", "0", "ffffffff", "ffffffffffffffff", "ff")]


def test_translate_deletes_what_the_target_does_not_cover(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT translate('abc','abc','xy'), translate('abc','','x'),"
            " translate('aabb','ab','xy'), translate('abcabc','ab','')"
        )
        assert cur.fetchall() == [("xy", "abc", "xxyy", "cc")]


def test_overlay_replaces_a_run_and_refuses_a_zero_offset(home: Path) -> None:
    """`for` defaults to the length of the replacement, so
    `overlay('abcdef' placing 'XY' from 2)` is `aXYdef`. A `from` below 1 is
    PostgreSQL's `22011`, not a generic bad-value `22P02`."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT overlay('abc' placing 'XY' from 1 for 0),"
            " overlay('abcdef' placing 'XY' from 2),"
            " overlay('abc' placing 'XY' from 2 for 2),"
            " overlay('abcdef' placing '' from 2 for 3),"
            " overlay('abc' placing 'XYZ' from 5)"
        )
        assert cur.fetchall() == [("XYabc", "aXYdef", "aXY", "aef", "abcXYZ")]
        with pytest.raises(psycopg.Error) as info:
            cur.execute("SELECT overlay('abc' placing 'X' from 0)")
        assert info.value.sqlstate == "22011"


def test_quote_nullable_answers_the_string_null(home: Path) -> None:
    """`quote_literal(NULL)` is NULL; `quote_nullable(NULL)` is the four-character
    string `NULL`. That difference is the whole reason the two exist as a pair —
    and `psql` renders both as `NULL`, so it has to be probed with `IS NULL`
    rather than read off the screen."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT quote_literal(1), quote_literal(NULL), quote_nullable('a'),"
            " quote_nullable(NULL), quote_nullable(NULL) IS NULL"
        )
        assert cur.fetchall() == [("'1'", None, "'a'", "NULL", False)]
        # A backslash forces the E'...' form, with backslashes doubled.
        cur.execute(r"SELECT quote_literal('a\b'), quote_literal('it''s')")
        assert cur.fetchall() == [(r"E'a\\b'", "'it''s'")]


def test_split_part_counts_from_the_end_when_the_field_is_negative(home: Path) -> None:
    """PostgreSQL has supported a negative field since 14. Refusing every
    non-positive field rejected a working form, and the message named the wrong
    rule: zero is "must not be zero", not "must be greater than zero"."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT split_part('a,b,c',',',-1), split_part('a,b,c',',',-3),"
            " split_part('a,b,c',',',-4), split_part('a,b,c',',',9),"
            " split_part('abc','',1)"
        )
        assert cur.fetchall() == [("c", "a", "", "", "abc")]
        with pytest.raises(psycopg.Error) as info:
            cur.execute("SELECT split_part('a,b,c',',',0)")
        assert "must not be zero" in str(info.value)


def test_substring_from_a_pattern_is_a_different_function(home: Path) -> None:
    """`substring(s FROM pattern)` shares a name with the offset form but takes
    a POSIX regex, and answers the first CAPTURE GROUP when the pattern has one
    and the whole match otherwise. Reading the second argument as an integer
    regardless answered `42601 ... does not exist with that argument list` for
    every regex use."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT substring('abcde' from 'b(c)d'), substring('abc' from '(b)'),"
            " substring('abc' from 'b'), substring('abc' from 'x')"
        )
        assert cur.fetchall() == [("c", "b", "b", None)]
        # The SQL-standard form, where `#\"...#\"` marks the part to return.
        cur.execute("SELECT substring('abcde' from '%#\"c#\"%' for '#')")
        assert cur.fetchall() == [("c",)]
        # The offset form still works, and still refuses a negative length.
        cur.execute("SELECT substring('abcdef' from 2 for 3)")
        assert cur.fetchall() == [("bcd",)]


def test_regexp_replace_expands_the_whole_match_escape(home: Path) -> None:
    r"""PostgreSQL's replacement text takes `\1`..`\9` for groups and `\&` for
    the WHOLE match. `\&` passed through literally, so
    `regexp_replace('abc','b','\&\&')` gave `a\&\&c` where PostgreSQL gives
    `abbc`. An unknown escape stays as written, which PostgreSQL also does."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            r"SELECT regexp_replace('abc','b','\&\&'), regexp_replace('abc','(b)','[\1]'),"
            r" regexp_replace('abc','b','x\&y'), regexp_replace('abc','b','\q')"
        )
        assert cur.fetchall() == [("abbc", "a[b]c", "axbyc", r"a\qc")]


def test_regexp_split_to_array_and_unistr(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT regexp_split_to_array('a1b22c','[0-9]+'), regexp_split_to_array('abc','')"
        )
        assert cur.fetchall() == [(["a", "b", "c"], ["a", "b", "c"])]
        cur.execute(r"SELECT unistr('d\0061t\+000061'), unistr('\\'), unistr('a\0062')")
        assert cur.fetchall() == [("data", "\\", "ab")]
        with pytest.raises(psycopg.Error) as info:
            cur.execute(r"SELECT unistr('\x')")
        assert info.value.sqlstate == "42601"


def test_convert_from_decodes_stored_bytes(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            r"SELECT convert_from('\x616263'::bytea,'UTF8'), convert_from('abc'::bytea,'LATIN1')"
        )
        assert cur.fetchall() == [("abc", "abc")]


# --- set-returning functions as a FROM item ---------------------------------
#
# `generate_series` already worked; these are the ones bounded by their
# arguments, which can be materialised. Expectations from
# `tools/probes/pg_corpora/srf.sql` against PostgreSQL 14.13.


def test_unnest_as_a_from_item(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT * FROM unnest(ARRAY[1,2,3])")
        assert cur.fetchall() == [(1,), (2,), (3,)]
        cur.execute("SELECT * FROM unnest(ARRAY['a','b']) AS t(s)")
        assert cur.fetchall() == [("a",), ("b",)]
        # `AS x` with no column list names the table AND the single column.
        cur.execute("SELECT x FROM unnest(ARRAY[3,1,2]) x")
        assert cur.fetchall() == [(3,), (1,), (2,)]
        cur.execute("SELECT x * 2 FROM unnest(ARRAY[1,2]) x")
        assert cur.fetchall() == [(2,), (4,)]


def test_an_empty_or_null_array_yields_no_rows(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT * FROM unnest(ARRAY[]::int[])")
        assert cur.fetchall() == []
        cur.execute("SELECT * FROM unnest(NULL::int[])")
        assert cur.fetchall() == []


def test_unnest_flattens_a_multidimensional_array(home: Path) -> None:
    """`unnest(ARRAY[[1,2],[3,4]])` is FOUR rows, not two — it yields the
    leaves in row-major order, not the inner arrays."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT * FROM unnest(ARRAY[[1,2],[3,4]])")
        assert cur.fetchall() == [(1,), (2,), (3,), (4,)]


def test_clauses_and_aggregates_over_a_set_returning_source(home: Path) -> None:
    """The source is planned as a FROM-subquery, so WHERE / ORDER BY / LIMIT
    and the aggregates come from the path that already handles
    `FROM (SELECT ...) s` — none of it is written twice."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT x FROM unnest(ARRAY[3,1,2]) x ORDER BY x")
        assert cur.fetchall() == [(1,), (2,), (3,)]
        cur.execute("SELECT x FROM unnest(ARRAY[3,1,2]) x WHERE x > 1 ORDER BY x")
        assert cur.fetchall() == [(2,), (3,)]
        cur.execute("SELECT x FROM unnest(ARRAY[3,1,2]) x ORDER BY x LIMIT 2")
        assert cur.fetchall() == [(1,), (2,)]
        cur.execute("SELECT count(*) FROM unnest(ARRAY[1,2,3])")
        assert cur.fetchall() == [(3,)]
        cur.execute("SELECT array_agg(x) FROM unnest(ARRAY[3,1,2]) x")
        assert cur.fetchall() == [([3, 1, 2],)]


def test_generate_series_still_takes_its_own_path(home: Path) -> None:
    """A series is a RANGE and stays lazy: materialising
    `generate_series(1, 10000000)` into a Vec would be a real regression, so it
    keeps the source it had rather than joining the materialised ones."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT * FROM generate_series(1,3)")
        assert cur.fetchall() == [(1,), (2,), (3,)]
        cur.execute("SELECT g FROM generate_series(1,3) g WHERE g > 1")
        assert cur.fetchall() == [(2,), (3,)]


def test_other_set_returning_functions_in_from(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT * FROM regexp_split_to_table('a,b,c', ',')")
        assert cur.fetchall() == [("a",), ("b",), ("c",)]
        cur.execute("SELECT * FROM generate_subscripts(ARRAY[5,6,7], 1)")
        assert cur.fetchall() == [(1,), (2,), (3,)]
        # A dimension the array does not have yields no rows, not an error.
        cur.execute("SELECT * FROM generate_subscripts(ARRAY[5,6,7], 2)")
        assert cur.fetchall() == []


def test_a_set_returning_function_as_a_bare_target(home: Path) -> None:
    """The FROM-less spelling shares `srf_rows` with the FROM one, so the two
    cannot drift."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT unnest(ARRAY[1,2])")
        assert cur.fetchall() == [(1,), (2,)]
        cur.execute("SELECT generate_subscripts(ARRAY[5,6,7], 1)")
        assert cur.fetchall() == [(1,), (2,), (3,)]


def test_normalize_across_all_four_unicode_forms(home: Path) -> None:
    r"""`normalize(text [, form])`, defaulting to NFC.

    Unicode normalisation is TABLE-driven — the composition and decomposition
    mappings are data, not an algorithm — so this is the one function in the
    string surface that needed a dependency (`unicode-normalization`) rather
    than a few lines. Approximating it would have answered most inputs right
    and a minority silently wrong.

    The form arrives as an ordinary string constant: `NFD` and friends are
    grammar keywords, so an unknown one is a syntax error before it reaches
    the evaluator.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        # NFD decomposes a precomposed character; NFC recomposes it.
        cur.execute(
            r"SELECT normalize(U&'\00E1', NFD) = U&'\0061\0301',"
            r" length(normalize(U&'\00E1', NFD)),"
            r" length(normalize(U&'\0061\0301', NFC))"
        )
        assert cur.fetchall() == [(True, 2, 1)]
        # The COMPATIBILITY forms fold a ligature; the canonical ones do not.
        cur.execute(
            r"SELECT normalize(U&'\FB01', NFKC), normalize(U&'\FB01', NFKD),"
            r" normalize(U&'\FB01', NFC)"
        )
        assert cur.fetchall() == [("fi", "fi", "ﬁ")]
        cur.execute("SELECT normalize(NULL), normalize('abc'), normalize('') = ''")
        assert cur.fetchall() == [(None, "abc", True)]


def test_a_rust_view_and_index_are_read_by_the_python_server(home: Path) -> None:
    """Views and indexes share the on-disk format across the two SQL servers.

    A view is a `__sql_views__` row holding its SELECT as text, and an index
    is a storage index over the table's collection, on both servers -- so a
    store handed from one to the other keeps both. The column-list form is
    stored as a wrapped subquery, which the Python server has to parse too.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE vt (id int PRIMARY KEY, g text, n int)")
        cur.execute("INSERT INTO vt VALUES (1,'a',10),(2,'a',20),(3,'b',NULL)")
        cur.execute("CREATE VIEW v1 AS SELECT id, n FROM vt WHERE n IS NOT NULL")
        cur.execute("CREATE VIEW v2 (k, total) AS SELECT g, sum(n) FROM vt GROUP BY g")
        cur.execute("CREATE INDEX vt_n ON vt (n)")

    assert _python_sql(home, "SELECT id, n FROM v1 ORDER BY id") == [(1, 10), (2, 20)]
    assert _python_sql(home, "SELECT k, total FROM v2 ORDER BY k") == [("a", 30), ("b", None)]
    assert _python_sql(home, "SELECT indexname FROM pg_indexes WHERE indexname = 'vt_n'") == [
        ("vt_n",)
    ]


def test_a_python_view_and_index_are_read_by_the_rust_server(home: Path) -> None:
    """The other direction: the Python server's sqlglot-rendered definition
    has to parse under libpg_query, and its index has to be listed."""
    _python_sql(
        home,
        "CREATE TABLE pt (id int PRIMARY KEY, g text, n int)",
        "INSERT INTO pt VALUES (1,'a',10),(2,'b',20)",
        "CREATE VIEW pv AS SELECT id, n * 2 AS dbl FROM pt WHERE g = 'b'",
        "CREATE INDEX pt_g ON pt (g)",
    )
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT id, dbl FROM pv")
        assert cur.fetchall() == [(2, 40)]
        cur.execute("SELECT indexdef FROM pg_indexes WHERE indexname = 'pt_g'")
        assert cur.fetchall() == [("CREATE INDEX pt_g ON public.pt USING btree (g)",)]


def test_a_unique_index_admits_many_nulls_and_names_itself(home: Path) -> None:
    """`CREATE UNIQUE INDEX` follows SQL's rule that NULLs are distinct, and a
    duplicate names the INDEX -- not a `<t>_<col>_key` guessed from its
    columns, which is what a violation reported before indexes existed."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE u (id int PRIMARY KEY, s text)")
        cur.execute("INSERT INTO u VALUES (1, NULL), (2, NULL), (3, 'x')")
        cur.execute("CREATE UNIQUE INDEX u_s_uniq ON u (s)")
        cur.execute("INSERT INTO u VALUES (4, NULL)")
        with pytest.raises(psycopg.errors.UniqueViolation, match='"u_s_uniq"'):
            cur.execute("INSERT INTO u VALUES (5, 'x')")
        cur.execute("INSERT INTO u VALUES (6, 'y')")
        with pytest.raises(psycopg.errors.UniqueViolation, match="Key \\(s\\)=\\(x\\)"):
            cur.execute("UPDATE u SET s = 'x' WHERE id = 6")


def test_a_view_is_expanded_wherever_it_is_read(home: Path) -> None:
    """FROM, a JOIN, a FROM-subquery, an IN-subquery and a view over a view."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE vt (id int PRIMARY KEY, n int)")
        cur.execute("INSERT INTO vt VALUES (1, 10), (2, 20), (3, 30)")
        cur.execute("CREATE VIEW big AS SELECT id, n FROM vt WHERE n > 15")
        cur.execute("CREATE VIEW bigger AS SELECT id FROM big WHERE n > 25")
        cur.execute("SELECT count(*) FROM big")
        assert cur.fetchone() == (2,)
        cur.execute("SELECT id FROM vt WHERE id IN (SELECT id FROM big) ORDER BY id")
        assert cur.fetchall() == [(2,), (3,)]
        cur.execute("SELECT b.id FROM big b JOIN vt ON vt.id = b.id ORDER BY b.id")
        assert cur.fetchall() == [(2,), (3,)]
        cur.execute("SELECT * FROM bigger")
        assert cur.fetchall() == [(3,)]
        with pytest.raises(psycopg.errors.DependentObjectsStillExist):
            cur.execute("DROP VIEW big")
        with pytest.raises(psycopg.errors.DependentObjectsStillExist):
            cur.execute("DROP TABLE vt")
        # A simple view is automatically updatable: the row lands in `vt`.
        cur.execute("INSERT INTO big VALUES (4, 40)")
        cur.execute("SELECT count(*) FROM vt")
        assert cur.fetchone() == (4,)
        cur.execute("DROP VIEW big CASCADE")
        with pytest.raises(psycopg.errors.UndefinedTable):
            cur.execute("SELECT * FROM bigger")


def test_ddl_rolls_back_with_its_transaction(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE r (id int PRIMARY KEY, n int)")
        cur.execute("BEGIN")
        cur.execute("CREATE VIEW rv AS SELECT id FROM r")
        cur.execute("CREATE INDEX r_n ON r (n)")
        cur.execute("SELECT count(*) FROM rv")
        assert cur.fetchone() == (0,)
        cur.execute("ROLLBACK")
        with pytest.raises(psycopg.errors.UndefinedTable):
            cur.execute("SELECT * FROM rv")
        cur.execute("SELECT count(*) FROM pg_indexes WHERE indexname = 'r_n'")
        assert cur.fetchone() == (0,)


def test_general_joins_answer_like_postgres(home: Path) -> None:
    """A JOIN is planned as a SOURCE and the query around it as any other, so
    aggregates, windows, `*`, USING / NATURAL and the outer joins all work.

    Two regressions it pins: a WHERE on the NULLABLE side of a LEFT JOIN runs
    AFTER the join (a pre-filter kept the NULL-extended rows PostgreSQL drops),
    and a bare name both sides have is 42702 rather than the left side's.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE j (id int PRIMARY KEY, g text, n int)")
        cur.execute("CREATE TABLE k (id int PRIMARY KEY, jid int, v int)")
        cur.execute("INSERT INTO j VALUES (1,'a',10),(2,'b',20),(3,'c',NULL)")
        cur.execute("INSERT INTO k VALUES (10,1,100),(11,1,200),(12,2,300)")

        def rows(sql: str) -> list[tuple]:
            cur.execute(sql)
            return cur.fetchall()

        assert rows(
            "SELECT j.id, count(k.id) FROM j LEFT JOIN k ON k.jid = j.id "
            "GROUP BY j.id ORDER BY j.id"
        ) == [(1, 2), (2, 1), (3, 0)]
        assert rows(
            "SELECT j.id, k.v FROM j LEFT JOIN k ON k.jid = j.id WHERE k.v > 150 ORDER BY 1"
        ) == [(1, 200), (2, 300)]
        assert rows("SELECT * FROM j JOIN k USING (id)") == []
        assert rows("SELECT * FROM j JOIN k ON k.jid = j.id ORDER BY k.id")[0] == (
            1,
            "a",
            10,
            10,
            1,
            100,
        )
        assert rows(
            "SELECT j.id, k.v FROM j FULL JOIN k ON k.jid = j.id AND k.v > 150 ORDER BY 1, 2"
        ) == [(1, 200), (2, 300), (3, None), (None, 100)]
        assert rows(
            "SELECT j.g, row_number() OVER (PARTITION BY j.g ORDER BY k.v DESC) "
            "FROM j JOIN k ON k.jid = j.id ORDER BY 1, 2"
        ) == [("a", 1), ("a", 2), ("b", 1)]
        assert rows("SELECT count(*) FROM j CROSS JOIN k") == [(9,)]
        with pytest.raises(psycopg.errors.AmbiguousColumn):
            cur.execute("SELECT id FROM j JOIN k ON k.jid = j.id")
        with pytest.raises(psycopg.errors.UndefinedColumn, match="j.nosuch"):
            cur.execute("SELECT j.nosuch FROM j JOIN k ON k.jid = j.id")


def test_a_cte_is_visible_inside_a_subquery(home: Path) -> None:
    """A subquery is resolved on its own, before the `WITH` around it is
    inlined -- so it could not see the statement's CTEs (42P01)."""
    with _Server(home) as server, server.connect() as conn:
        _dept_emp(conn)
        cur = conn.cursor()
        cur.execute(
            "WITH big AS (SELECT id FROM sq_emp WHERE salary > 120) "
            "SELECT id FROM sq_emp WHERE id IN (SELECT id FROM big) ORDER BY id"
        )
        assert cur.fetchall() == [(2,), (3,)]


def test_a_volatile_subquery_runs_once_per_execution(home: Path) -> None:
    """An uncorrelated subquery runs at plan time, and a statement is planned
    for its Describe as well as its Execute -- so `nextval` in one advanced
    the sequence two and three times per statement. PostgreSQL: once."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE SEQUENCE s")
        assert cur.execute("SELECT (SELECT nextval('s'))", prepare=True).fetchall() == [(1,)]
        assert cur.execute("SELECT (SELECT nextval('s'))").fetchall() == [(2,)]
        assert cur.execute("SELECT currval('s')").fetchall() == [(2,)]


def test_a_quoted_literal_compares_as_the_columns_type(home: Path) -> None:
    """An unknown-typed literal resolves to the column's type. Left a string,
    `n > '5'` and `t > '2026-03-01'` compared across BSON types and matched
    NOTHING -- silently, for every quoted number, boolean, timestamp and
    interval in a WHERE. A timestamp also compares its sub-millisecond
    remainder, which lives in a hidden companion field."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE lc (id int PRIMARY KEY, n int, t timestamptz, ok bool)")
        cur.execute(
            "INSERT INTO lc VALUES (1, 5, '2026-01-01 10:00:00.123456+00', true), "
            "(2, 7, '2026-06-01 10:00:00.123+00', false)"
        )

        def ids(where: str) -> list[int]:
            cur.execute(f"SELECT id FROM lc WHERE {where} ORDER BY id")
            return [r[0] for r in cur.fetchall()]

        assert ids("n > '5'") == [2]
        assert ids("n IN ('5', '6')") == [1]
        assert ids("ok = 't'") == [1]
        assert ids("t > '2026-03-01'") == [2]
        assert ids("t > '2026-01-01 10:00:00.123+00'") == [1, 2]
        assert ids("t = '2026-01-01 10:00:00.123456+00'") == [1]
        assert ids("t <= '2026-06-01 10:00:00.123+00'") == [1, 2]
        assert ids("t BETWEEN '2025-01-01' AND now()") == [1, 2]
        with pytest.raises(psycopg.errors.InvalidTextRepresentation):
            cur.execute("SELECT id FROM lc WHERE n > 'abc'")


def test_expression_defaults_are_evaluated_per_row(home: Path) -> None:
    """`now()`, `gen_random_uuid()` and `nextval()` defaults, the DEFAULT
    keyword in VALUES and SET, and DEFAULT VALUES -- once per row, where
    PostgreSQL evaluates them, never frozen at CREATE time."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE SEQUENCE ds START 100")
        cur.execute(
            "CREATE TABLE de (id int PRIMARY KEY DEFAULT nextval('ds'), "
            "at timestamptz DEFAULT now(), u uuid DEFAULT gen_random_uuid(), n int DEFAULT 3)"
        )
        cur.execute("INSERT INTO de DEFAULT VALUES RETURNING id, n")
        assert cur.fetchall() == [(100, 3)]
        cur.execute("INSERT INTO de (n) VALUES (5), (6) RETURNING id")
        assert cur.fetchall() == [(101,), (102,)]
        cur.execute("INSERT INTO de (id, n) VALUES (DEFAULT, 7) RETURNING id")
        assert cur.fetchall() == [(103,)]
        cur.execute("SELECT count(DISTINCT u), count(*) FROM de WHERE at <= now()")
        assert cur.fetchall() == [(4, 4)]
        cur.execute("UPDATE de SET n = DEFAULT WHERE id = 101 RETURNING n")
        assert cur.fetchall() == [(3,)]
        cur.execute("UPDATE de SET n = nextval('ds') WHERE id < 102 RETURNING n")
        assert sorted(r[0] for r in cur.fetchall()) == [104, 105]
        cur.execute("ALTER TABLE de ADD COLUMN u2 uuid DEFAULT gen_random_uuid()")
        cur.execute("SELECT count(DISTINCT u2) FROM de")
        assert cur.fetchall() == [(4,)]
        cur.execute(
            "SELECT column_default FROM information_schema.columns "
            "WHERE table_name = 'de' AND column_name = 'id'"
        )
        assert cur.fetchall() == [("nextval('ds'::regclass)",)]


def test_sequence_functions_work_inside_expressions(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE SEQUENCE s")
        cur.execute("CREATE TABLE q (id int PRIMARY KEY, n int)")
        cur.execute("INSERT INTO q VALUES (nextval('s'), 1), (nextval('s') * 10, 2)")
        cur.execute("SELECT id FROM q ORDER BY id")
        assert cur.fetchall() == [(1,), (20,)]
        cur.execute("SELECT nextval('s') + 100, currval('s'), lastval()")
        assert cur.fetchall() == [(103, 3, 3)]


def test_windows_over_aggregates_series_and_inside_expressions(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE w (id int PRIMARY KEY, g text, v int)")
        cur.execute("INSERT INTO w VALUES (1,'a',10),(2,'a',20),(3,'b',5),(4,'c',40)")
        cur.execute("SELECT g, sum(v), sum(sum(v)) OVER (ORDER BY g) FROM w GROUP BY g ORDER BY g")
        assert cur.fetchall() == [("a", 30, 30), ("b", 5, 35), ("c", 40, 75)]
        cur.execute(
            "SELECT g, rank() OVER (ORDER BY count(*) DESC, g) FROM w GROUP BY g ORDER BY g"
        )
        assert cur.fetchall() == [("a", 1), ("b", 2), ("c", 3)]
        cur.execute("SELECT n, sum(n) OVER (ORDER BY n) FROM generate_series(1, 4) n ORDER BY n")
        assert cur.fetchall() == [(1, 1), (2, 3), (3, 6), (4, 10)]
        cur.execute("SELECT id, coalesce(lag(v) OVER (ORDER BY id), 0) FROM w ORDER BY id")
        assert cur.fetchall() == [(1, 0), (2, 10), (3, 20), (4, 5)]


def test_a_composite_primary_key_shares_the_pythons_layout(home: Path) -> None:
    """A composite key is a subdocument `_id` whose fields are the key columns
    in TABLE order -- the Python server's layout -- so either server reads and
    enforces the other's. A duplicate names the columns, not `_id`."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE ck (a int, b text, v int, PRIMARY KEY (a, b))")
        cur.execute("INSERT INTO ck (b, a, v) VALUES ('x', 1, 10), ('y', 1, 20)")
        with pytest.raises(psycopg.errors.UniqueViolation) as info:
            cur.execute("INSERT INTO ck VALUES (1, 'x', 99)")
        assert info.value.diag.message_detail == "Key (a, b)=(1, x) already exists."
        with pytest.raises(psycopg.errors.NotNullViolation):
            cur.execute("INSERT INTO ck VALUES (NULL, 'q', 1)")
        cur.execute("UPDATE ck SET v = v + 1 WHERE a = 1")
        cur.execute("SELECT c.a, sum(c.v) FROM ck c GROUP BY c.a")
        assert cur.fetchall() == [(1, 32)]

    assert _python_sql(home, "SELECT a, b, v FROM ck ORDER BY b") == [(1, "x", 11), (1, "y", 21)]
    _python_sql(home, "INSERT INTO ck VALUES (2, 'z', 5)")
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT a, b, v FROM ck WHERE a = 2")
        assert cur.fetchall() == [(2, "z", 5)]
        with pytest.raises(psycopg.errors.UniqueViolation):
            cur.execute("INSERT INTO ck VALUES (2, 'z', 6)")


def test_a_role_with_a_password_must_prove_it(home: Path) -> None:
    """SCRAM-SHA-256 against the verifier `CREATE ROLE ... PASSWORD` stores.

    Before this every connection was trusted, so a wrong password -- or none
    -- connected as a password-protected role: an authentication bypass. A
    role with no password, and a user the server has never heard of, are
    still trusted, which is what every fixture connecting as a password-less
    `postgres` relies on.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE ROLE alice LOGIN PASSWORD 's3cret'")
        conn.execute("CREATE ROLE gate PASSWORD 'x'")
        dsn = f"host=127.0.0.1 port={server.port} dbname=postgres connect_timeout=10"
        with psycopg.connect(f"{dsn} user=alice password=s3cret") as ok:
            assert ok.execute("SELECT current_user").fetchone() == ("alice",)
        with pytest.raises(psycopg.OperationalError, match="password authentication failed"):
            psycopg.connect(f"{dsn} user=alice password=wrong")
        with pytest.raises(psycopg.OperationalError):
            psycopg.connect(f"{dsn} user=alice")
        with pytest.raises(psycopg.OperationalError, match="not permitted to log in"):
            psycopg.connect(f"{dsn} user=gate password=x")
        with psycopg.connect(f"{dsn} user=stranger") as trusted:
            assert trusted.execute("SELECT 1").fetchone() == (1,)


def test_explain_reports_the_plans_shape(home: Path) -> None:
    """EXPLAIN in PostgreSQL's layout. The node types are real -- an index
    scan exactly when the storage would use the index -- and the costs are
    zeros rather than a fiction, so tests compare plans with COSTS OFF."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE ex (id int PRIMARY KEY, n int, g text)")
        cur.execute("INSERT INTO ex VALUES (1, 1, 'a')")

        def plan(sql: str) -> list[str]:
            cur.execute(sql)
            return [r[0] for r in cur.fetchall()]

        assert plan("EXPLAIN (COSTS OFF) SELECT * FROM ex ORDER BY g LIMIT 1") == [
            "Limit",
            "  ->  Sort",
            "        Sort Key: g",
            "        ->  Seq Scan on ex",
        ]
        assert plan("EXPLAIN (COSTS OFF) SELECT g, count(*) FROM ex GROUP BY g") == [
            "HashAggregate",
            "  Group Key: g",
            "  ->  Seq Scan on ex",
        ]
        assert plan("EXPLAIN SELECT 1") == ["Result  (cost=0.00..0.00 rows=0 width=0)"]
        cur.execute("EXPLAIN (FORMAT JSON, COSTS OFF) SELECT * FROM ex")
        assert cur.fetchone()[0][0]["Plan"]["Node Type"] == "Seq Scan"
        assert plan("EXPLAIN ANALYZE SELECT * FROM ex")[-1] == "Execution Time: 0.000 ms"


def test_multi_column_foreign_keys(home: Path) -> None:
    """A foreign key over several columns, to a composite PRIMARY KEY or a
    UNIQUE constraint: MATCH SIMPLE (a NULL anywhere passes), the DELETE and
    UPDATE actions, and a DETAIL naming the whole key."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE p (a int, b text, PRIMARY KEY (a, b))")
        cur.execute("CREATE TABLE u (id int PRIMARY KEY, x int, y int, UNIQUE (x, y))")
        cur.execute("INSERT INTO p VALUES (1, 'x'), (2, 'y')")
        cur.execute("INSERT INTO u VALUES (1, 5, 5)")
        cur.execute(
            "CREATE TABLE c (id int PRIMARY KEY, pa int, pb text, "
            "FOREIGN KEY (pa, pb) REFERENCES p ON DELETE CASCADE)"
        )
        cur.execute("INSERT INTO c VALUES (1, 1, 'x'), (2, NULL, 'nope')")
        with pytest.raises(psycopg.errors.ForeignKeyViolation) as info:
            cur.execute("INSERT INTO c VALUES (3, 1, 'y')")
        assert info.value.diag.message_detail == 'Key (pa, pb)=(1, y) is not present in table "p".'
        cur.execute("DELETE FROM p WHERE a = 1")
        cur.execute("SELECT id FROM c ORDER BY id")
        assert cur.fetchall() == [(2,)]
        cur.execute(
            "CREATE TABLE g (id int PRIMARY KEY, x int, y int, "
            "FOREIGN KEY (x, y) REFERENCES u (x, y) ON UPDATE CASCADE)"
        )
        cur.execute("INSERT INTO g VALUES (1, 5, 5)")
        cur.execute("UPDATE u SET y = 6 WHERE id = 1")
        cur.execute("SELECT x, y FROM g")
        assert cur.fetchall() == [(5, 6)]
        with pytest.raises(psycopg.errors.ForeignKeyViolation):
            cur.execute("DELETE FROM u WHERE id = 1")


def test_user_defined_functions(home: Path) -> None:
    """`LANGUAGE sql` and `plpgsql` functions: scalar calls in the select list
    and WHERE, recursion, SETOF with RETURN NEXT, exception handlers, and the
    errors PostgreSQL gives for a wrong arity and a duplicate."""
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE ft (id int PRIMARY KEY, n int)")
        cur.execute("INSERT INTO ft VALUES (1, 10), (2, 20)")
        cur.execute("CREATE FUNCTION add2(a int, b int) RETURNS int AS 'SELECT a + b' LANGUAGE sql")
        cur.execute("SELECT id, add2(id, n) FROM ft ORDER BY id")
        assert cur.fetchall() == [(1, 11), (2, 22)]
        cur.execute("SELECT id FROM ft WHERE add2(id, 1) > 2")
        assert cur.fetchall() == [(2,)]
        cur.execute(
            "CREATE FUNCTION fact(n int) RETURNS int AS $$ BEGIN IF n <= 1 THEN RETURN 1; "
            "END IF; RETURN n * fact(n - 1); END $$ LANGUAGE plpgsql"
        )
        cur.execute("SELECT fact(5)")
        assert cur.fetchone() == (120,)
        cur.execute(
            "CREATE FUNCTION evens(m int) RETURNS SETOF int AS $$ BEGIN FOR i IN 1..m LOOP "
            "IF i % 2 = 0 THEN RETURN NEXT i; END IF; END LOOP; END $$ LANGUAGE plpgsql"
        )
        cur.execute("SELECT * FROM evens(7)")
        assert cur.fetchall() == [(2,), (4,), (6,)]
        cur.execute(
            "CREATE FUNCTION safe_div(a int, b int) RETURNS int AS $$ BEGIN RETURN a / b; "
            "EXCEPTION WHEN division_by_zero THEN RETURN NULL; END $$ LANGUAGE plpgsql"
        )
        cur.execute("SELECT safe_div(6, 3), safe_div(1, 0)")
        assert cur.fetchone() == (2, None)
        with pytest.raises(psycopg.errors.UndefinedFunction) as info:
            cur.execute("SELECT add2(1)")
        assert str(info.value.diag.message_primary) == "function add2(integer) does not exist"
        with pytest.raises(psycopg.errors.DuplicateFunction):
            cur.execute("CREATE FUNCTION add2(a int, b int) RETURNS int AS 'SELECT 0' LANGUAGE sql")


def test_triggers(home: Path) -> None:
    """Row and statement triggers around INSERT / UPDATE / DELETE: a BEFORE
    trigger rewriting or skipping a row, an AFTER trigger writing an audit
    table, WHEN and TG_ARGV, a trigger's error aborting the whole statement,
    and DROP FUNCTION refusing while a trigger still calls it."""
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE tt (id int PRIMARY KEY, name text, n int)")
        cur.execute("CREATE TABLE tlog (seq serial PRIMARY KEY, msg text)")
        cur.execute(
            "CREATE FUNCTION up() RETURNS trigger AS $$ BEGIN IF NEW.n < 0 THEN RETURN NULL; "
            "END IF; NEW.name := upper(NEW.name); RETURN NEW; END $$ LANGUAGE plpgsql"
        )
        cur.execute(
            "CREATE FUNCTION audit() RETURNS trigger AS $$ BEGIN INSERT INTO tlog(msg) VALUES "
            "(TG_OP || ' ' || TG_ARGV[0] || ' ' || coalesce(OLD.id, NEW.id)); "
            "RETURN NULL; END $$ LANGUAGE plpgsql"
        )
        cur.execute("CREATE TRIGGER t_up BEFORE INSERT ON tt FOR EACH ROW EXECUTE FUNCTION up()")
        cur.execute(
            "CREATE TRIGGER t_audit AFTER INSERT OR DELETE ON tt FOR EACH ROW "
            "EXECUTE FUNCTION audit('row')"
        )
        cur.execute("INSERT INTO tt VALUES (1, 'a', 1), (2, 'b', -1), (3, 'c', 3)")
        assert cur.rowcount == 2
        cur.execute("SELECT id, name FROM tt ORDER BY id")
        assert cur.fetchall() == [(1, "A"), (3, "C")]
        cur.execute(
            "CREATE FUNCTION guard() RETURNS trigger AS $$ BEGIN RAISE EXCEPTION 'no %', "
            "NEW.id; END $$ LANGUAGE plpgsql"
        )
        cur.execute(
            "CREATE TRIGGER t_guard BEFORE UPDATE ON tt FOR EACH ROW WHEN (NEW.n > 100) "
            "EXECUTE FUNCTION guard()"
        )
        with pytest.raises(psycopg.errors.RaiseException):
            cur.execute("UPDATE tt SET n = 500")
        cur.execute("UPDATE tt SET n = 50 WHERE id = 1")
        cur.execute("SELECT n FROM tt ORDER BY id")
        assert cur.fetchall() == [(50,), (3,)]
        cur.execute("DELETE FROM tt WHERE id = 3")
        cur.execute("SELECT msg FROM tlog ORDER BY seq")
        assert cur.fetchall() == [("INSERT row 1",), ("INSERT row 3",), ("DELETE row 3",)]
        with pytest.raises(psycopg.errors.DependentObjectsStillExist):
            cur.execute("DROP FUNCTION guard()")
        cur.execute("DROP TRIGGER t_guard ON tt")
        cur.execute("DROP FUNCTION guard()")
        cur.execute("SELECT count(*) FROM pg_trigger WHERE tgrelid = 'tt'::regclass")
        assert cur.fetchone() == (2,)
        # ROLLBACK TO undoes what a trigger wrote to ANOTHER table too.
        cur.execute("DELETE FROM tlog")
        cur.execute("BEGIN")
        cur.execute("SAVEPOINT s")
        cur.execute("INSERT INTO tt VALUES (9, 'z', 9)")
        cur.execute("ROLLBACK TO s")
        cur.execute("COMMIT")
        cur.execute("SELECT (SELECT count(*) FROM tt WHERE id = 9), (SELECT count(*) FROM tlog)")
        assert cur.fetchone() == (0, 0)
        # An AFTER trigger that raises takes its statement's rows with it,
        # and so does a DO block, outside any block too.
        cur.execute(
            "CREATE FUNCTION boom() RETURNS trigger AS $$ BEGIN RAISE EXCEPTION 'boom'; "
            "END $$ LANGUAGE plpgsql"
        )
        cur.execute("CREATE TRIGGER t_boom AFTER INSERT ON tt FOR EACH ROW EXECUTE FUNCTION boom()")
        with pytest.raises(psycopg.errors.RaiseException):
            cur.execute("INSERT INTO tt VALUES (10, 'y', 1)")
        with pytest.raises(psycopg.errors.RaiseException):
            cur.execute(
                "DO $$ BEGIN EXECUTE 'INSERT INTO tlog(msg) VALUES (''x'')'; "
                "RAISE EXCEPTION 'boom'; END $$"
            )
        cur.execute("SELECT (SELECT count(*) FROM tt WHERE id = 10), (SELECT count(*) FROM tlog)")
        assert cur.fetchone() == (0, 0)


def test_full_text_search(home: Path) -> None:
    """`tsvector` / `tsquery`: stemming, stop-words, phrases, weights and
    ranking as PostgreSQL 14 answers them, a tsvector column searched with
    `@@`, and the column read by the Python server over the same store --
    and a Python-written one read back here."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT to_tsvector('english', 'The quick brown foxes jumped')")
        assert cur.fetchone()[0] == "'brown':3 'fox':4 'jump':5 'quick':2"
        assert cur.description[0].type_code == 3614
        cur.execute("SELECT to_tsquery('english', 'fox <-> the <-> quick')")
        assert cur.fetchone()[0] == "'fox' <2> 'quick'"
        cur.execute(
            "SELECT ts_rank(to_tsvector('english', 'The quick brown fox jumps over the lazy dog'), "
            "to_tsquery('english', 'fox & dog'))"
        )
        assert cur.fetchone()[0] == pytest.approx(0.09148999)
        cur.execute("CREATE TABLE docs (id int PRIMARY KEY, body text, tv tsvector)")
        cur.execute(
            "INSERT INTO docs VALUES (1, 'cats sat', to_tsvector('english', 'cats sat')), "
            "(2, 'dogs ran', to_tsvector('english', 'dogs ran'))"
        )
        cur.execute("SELECT id FROM docs WHERE tv @@ to_tsquery('english', 'cat') ORDER BY id")
        assert cur.fetchall() == [(1,)]
        cur.execute("SELECT id FROM docs WHERE body @@ 'dogs' ORDER BY id")
        assert cur.fetchall() == [(2,)]

    assert _python_sql(
        home, "SELECT id FROM docs WHERE tv @@ to_tsquery('english', 'dog') ORDER BY id"
    ) == [(2,)]
    _python_sql(home, "INSERT INTO docs VALUES (3, 'birds', to_tsvector('english', 'birds fly'))")
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT tv FROM docs WHERE id = 3")
        assert cur.fetchone()[0] == "'bird':1 'fli':2"
        cur.execute("SELECT id FROM docs WHERE tv @@ to_tsquery('english', 'bird') ORDER BY id")
        assert cur.fetchall() == [(3,)]


def _fetch(conn, sql: str) -> list[tuple]:
    """Every row of `sql`, values as psycopg decoded them."""
    return conn.execute(sql).fetchall()


def test_formatting_datetime_and_jsonpath(home: Path) -> None:
    """Numeric and datetime `to_char` / `to_number`, the datetime function
    family, and SQL/JSON path queries -- each value as PostgreSQL 14 prints
    it (measured with psql)."""
    with _Server(home) as server, server.connect() as conn:
        assert _fetch(
            conn,
            "SELECT to_char(1234.5::numeric, 'FM9,999.00'), "
            "to_number('$1,234.50', 'L9,999.99')::text",
        ) == [("1,234.50", "1234.50")]
        assert _fetch(
            conn, "SELECT to_char(timestamp '2024-03-05 14:07:09', 'Day DD Mon YYYY HH12:MI:SS AM')"
        ) == [("Tuesday   05 Mar 2024 02:07:09 PM",)]
        assert _fetch(
            conn,
            "SELECT extract(epoch FROM timestamp '2024-01-01 00:00:00')::text, "
            "date_trunc('month', timestamp '2024-03-15 10:00')::text, "
            "age(timestamp '2024-03-01', timestamp '2023-01-15')::text",
        ) == [("1704067200.000000", "2024-03-01 00:00:00", "1 year 1 mon 17 days")]
        assert _fetch(
            conn,
            "SELECT jsonb_path_query_array('{\"a\":[1,2,3,4]}', '$.a[*] ? (@ > 2)')::text, "
            "jsonb_path_exists('{\"a\":1}', '$.b'), "
            "('{\"a\":[1,2]}'::jsonb @? '$.a[*] ? (@ == 2)')",
        ) == [("[3, 4]", False, True)]


def test_statistical_and_ordered_set_aggregates(home: Path) -> None:
    """The statistical, ordered-set and hypothetical-set aggregates, and the
    JSON constructors, as PostgreSQL 14 answers them."""
    with _Server(home) as server, server.connect() as conn:
        assert _fetch(
            conn,
            "SELECT stddev_samp(x)::text, var_pop(x)::text, corr(x, y)::text, "
            "regr_slope(y, x)::text "
            "FROM (VALUES (1,2),(2,4),(3,7)) v(x,y)",
        ) == [("1.00000000000000000000", "0.66666666666666666667", "0.9933992677987828", "2.5")]
        assert _fetch(
            conn,
            "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY x), "
            "percentile_disc(0.5) WITHIN GROUP (ORDER BY x), "
            "mode() WITHIN GROUP (ORDER BY x), rank(2) WITHIN GROUP (ORDER BY x) "
            "FROM (VALUES (1),(2),(2),(5)) v(x)",
        ) == [(2.0, 2, 2, 2)]
        assert _fetch(
            conn,
            "SELECT json_build_object('a', 1, 'b', ARRAY[1,2])::text, "
            "jsonb_build_array(1, 'x', NULL)::text, "
            "row_to_json(ROW(1, 'x'))::text",
        ) == [('{"a" : 1, "b" : [1,2]}', '[1, "x", null]', '{"f1":1,"f2":"x"}')]


def test_char_n_padding_and_length(home: Path) -> None:
    """`char(n)` pads on output, compares blank-insensitively, and an
    assignment that is too long is 22001 unless the excess is blanks."""
    with _Server(home) as server, server.connect() as conn:
        conn.autocommit = True
        cur = conn.cursor()
        cur.execute("CREATE TABLE cp (id int PRIMARY KEY, c char(5), v varchar(5))")
        cur.execute("INSERT INTO cp VALUES (1, 'x', 'x'), (2, 'ab      ', 'y')")
        assert _fetch(
            conn, "SELECT c, c = 'x  ', length(c), octet_length(c) FROM cp ORDER BY id"
        ) == [
            ("x    ", True, 1, 5),
            ("ab   ", False, 2, 5),
        ]
        assert _fetch(conn, "SELECT c, count(*) FROM cp GROUP BY c ORDER BY c") == [
            ("ab   ", 1),
            ("x    ", 1),
        ]
        assert _sqlstate(conn, "INSERT INTO cp VALUES (3, 'toolong', 'z')") == "22001"
        assert _sqlstate(conn, "UPDATE cp SET v = 'toolong' WHERE id = 1") == "22001"


def test_update_from_and_delete_using(home: Path) -> None:
    """`UPDATE ... FROM` and `DELETE ... USING` touch only the joined rows --
    the extra FROM used to be dropped, so every row was written -- and an
    unqualified column both sides have is 42702."""
    with _Server(home) as server, server.connect() as conn:
        conn.autocommit = True
        conn.execute("CREATE TABLE uf_t (id int PRIMARY KEY, n int)")
        conn.execute("CREATE TABLE uf_s (id int PRIMARY KEY, tid int, v int)")
        conn.execute("INSERT INTO uf_t VALUES (1, 10), (2, 20), (3, 30)")
        conn.execute("INSERT INTO uf_s VALUES (1, 1, 100), (2, 2, 200)")
        assert _fetch(
            conn,
            "UPDATE uf_t SET n = s.v FROM uf_s s WHERE uf_t.id = s.tid RETURNING uf_t.id, s.v",
        ) == [(1, 100), (2, 200)]
        assert _fetch(conn, "SELECT id, n FROM uf_t ORDER BY id") == [(1, 100), (2, 200), (3, 30)]
        assert (
            _sqlstate(conn, "UPDATE uf_t SET n = 0 FROM uf_s WHERE uf_t.id = tid RETURNING id")
            == "42702"
        )
        assert _fetch(
            conn, "DELETE FROM uf_t USING uf_s WHERE uf_t.id = uf_s.tid RETURNING uf_t.id"
        ) == [
            (1,),
            (2,),
        ]
        assert _fetch(conn, "SELECT id FROM uf_t") == [(3,)]


def test_integer_overflow_numeric_math_and_bits(home: Path) -> None:
    """int4 arithmetic overflows as 22003 instead of widening; the numeric
    transcendentals answer numeric at PostgreSQL's result scale; bit strings
    and the integer bitwise operators work."""
    with _Server(home) as server, server.connect() as conn:
        assert _sqlstate(conn, "SELECT 2147483647 + 1") == "22003"
        assert _sqlstate(conn, "SELECT abs(-2147483648)") == "22003"
        assert _fetch(
            conn,
            "SELECT sqrt(2.0)::text, (2.0 ^ 0.5)::text, ln(10.0)::text, exp(1.0)::text, "
            "power(2::numeric, 100)::text, scale(1.230), trim_scale(1.230)::text",
        ) == [
            (
                "1.414213562373095",
                "1.4142135623730950",
                "2.3025850929940457",
                "2.7182818284590452",
                "1267650600228229401496703205376.0000000000000000",
                3,
                "1.23",
            )
        ]
        assert _fetch(
            conn,
            "SELECT (B'1010' & B'0110')::text, (~B'1010')::text, (B'1010' << 1)::text, "
            "5::bit(4)::text, B'1010'::int, get_bit(B'1010', 0), 5 & 3, 1 << 4",
        ) == [("0010", "0101", "0100", "0101", 10, 1, 1, 16)]
        assert _fetch(
            conn,
            "SELECT '1.1'::float4::float8::text, (1/3.0)::float4::text, upper('é'), "
            "current_setting('lc_ctype')",
        ) == [("1.100000023841858", "0.33333334", "É", "C.UTF-8")]


def test_updatable_views_and_read_only_transactions(home: Path) -> None:
    """A simple view takes INSERT / UPDATE / DELETE onto its base table, WITH
    CHECK OPTION refuses rows it would not show (44000), and a READ ONLY
    transaction refuses every write (25006)."""
    with _Server(home) as server, server.connect() as conn:
        conn.autocommit = True
        conn.execute("CREATE TABLE vw_t (id int PRIMARY KEY, n int)")
        conn.execute("CREATE VIEW vw_v AS SELECT id, n, n * 2 AS dbl FROM vw_t WHERE n > 0")
        conn.execute("CREATE VIEW vw_c AS SELECT id, n FROM vw_t WHERE n > 0 WITH CHECK OPTION")
        assert _fetch(conn, "INSERT INTO vw_v (id, n) VALUES (1, 5) RETURNING dbl") == [(10,)]
        assert _sqlstate(conn, "INSERT INTO vw_v (id, dbl) VALUES (2, 1)") == "0A000"
        assert _fetch(conn, "UPDATE vw_v SET n = n + 1 RETURNING id, n") == [(1, 6)]
        assert _sqlstate(conn, "INSERT INTO vw_c VALUES (3, -1)") == "44000"
        assert _sqlstate(conn, "UPDATE vw_c SET n = -1") == "44000"
        assert _fetch(conn, "DELETE FROM vw_v RETURNING id") == [(1,)]
        conn.execute("BEGIN READ ONLY")
        assert _sqlstate(conn, "INSERT INTO vw_t VALUES (9, 9)") == "25006"
        conn.execute("ROLLBACK")
        assert _fetch(conn, "SELECT count(*) FROM vw_t") == [(0,)]


def test_expression_index_order_by_aggregate_and_grouping(home: Path) -> None:
    """A UNIQUE index over an expression is enforced; ORDER BY may read an
    aggregate result; GROUPING() names the rolled-up keys."""
    with _Server(home) as server, server.connect() as conn:
        conn.autocommit = True
        conn.execute("CREATE TABLE ei (id int PRIMARY KEY, email text, g text, n int)")
        conn.execute("CREATE UNIQUE INDEX ei_email ON ei (lower(email))")
        conn.execute(
            "INSERT INTO ei VALUES (1, 'A@x', 'a', 1), (2, 'b@x', 'a', 2), (3, 'c@x', 'b', 3)"
        )
        assert _sqlstate(conn, "INSERT INTO ei VALUES (4, 'a@X', 'b', 4)") == "23505"
        assert _fetch(conn, "SELECT g, count(*) FROM ei GROUP BY g ORDER BY count(*) DESC, g") == [
            ("a", 2),
            ("b", 1),
        ]
        assert _fetch(
            conn,
            "SELECT g, sum(n), grouping(g) FROM ei GROUP BY ROLLUP (g) ORDER BY g NULLS LAST",
        ) == [("a", 3, 0), ("b", 3, 0), (None, 6, 1)]


def test_general_datetime_input_and_series(home: Path) -> None:
    """Date/time input beyond ISO -- textual months, AM/PM, zone
    abbreviations -- and generate_series over dates and numerics."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("SET timezone = 'UTC'")
        assert _fetch(
            conn,
            "SELECT 'Jan 5, 2020'::date::text, '5 January 2020 10:30 PM'::timestamp::text, "
            "('2020-01-05 10:30 EST'::timestamptz AT TIME ZONE 'UTC')::text",
        ) == [("2020-01-05", "2020-01-05 22:30:00", "2020-01-05 15:30:00")]
        assert _fetch(
            conn,
            "SELECT count(*) FROM generate_series("
            "'2020-01-01'::date, '2020-01-31'::date, '1 week')",
        ) == [(5,)]
        assert _fetch(conn, "SELECT g::text FROM generate_series(1.5, 3, 0.5) g") == [
            ("1.5",),
            ("2.0",),
            ("2.5",),
            ("3.0",),
        ]


def test_geometric_and_range_operators(home: Path) -> None:
    """`box` comparison and overlap operators, range and multirange set
    operators, and PostgreSQL's 42883 for mixing a range with a multirange
    under `+`."""
    with _Server(home) as server, server.connect() as conn:
        assert _fetch(
            conn,
            "SELECT box '(2,2),(0,0)' && box '(3,3),(1,1)', "
            "box '(3,3),(0,0)' @> box '(2,2),(1,1)', box '(1,1),(0,0)' = box '(0,1),(1,0)'",
        ) == [(True, True, True)]
        assert _fetch(
            conn,
            "SELECT (int4range(1, 5) + int4range(3, 8))::text, "
            "(int4range(1, 10) - int4range(5, 12))::text, "
            "(int4multirange(int4range(1, 3)) + int4multirange(int4range(5, 7)))::text",
        ) == [("[1,8)", "[1,5)", "{[1,3),[5,7)}")]
        assert _sqlstate(conn, "SELECT int4range(1, 3) + int4multirange(int4range(5, 7))") == (
            "42883"
        )


def test_sequences_advance_outside_the_transaction(home: Path) -> None:
    """`nextval` is not rolled back with the block that called it."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE SEQUENCE sq_tx")
        conn.execute("BEGIN")
        assert _fetch(conn, "SELECT nextval('sq_tx')") == [(1,)]
        conn.execute("ROLLBACK")
        assert _fetch(conn, "SELECT nextval('sq_tx')") == [(2,)]


def test_subqueries_over_grouped_rows(home: Path) -> None:
    """A subquery in a grouped query's select list, HAVING or ORDER BY runs
    over the grouped rows; an ungrouped outer column is 42803."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE gs_d (id int PRIMARY KEY, g text)")
        conn.execute("CREATE TABLE gs_e (id int PRIMARY KEY, g text)")
        conn.execute("INSERT INTO gs_d VALUES (1, 'a'), (2, 'a'), (3, 'b')")
        conn.execute("INSERT INTO gs_e VALUES (1, 'a'), (2, 'b'), (3, 'b')")
        assert _fetch(
            conn,
            "SELECT g, count(*), (SELECT count(*) FROM gs_e WHERE gs_e.g = gs_d.g) "
            "FROM gs_d GROUP BY g ORDER BY g",
        ) == [("a", 2, 1), ("b", 1, 2)]
        assert _fetch(
            conn,
            "SELECT g FROM gs_d GROUP BY g HAVING count(*) > "
            "(SELECT count(*) FROM gs_e WHERE gs_e.g = gs_d.g) ORDER BY g",
        ) == [("a",)]
        assert (
            _sqlstate(
                conn,
                "SELECT g, (SELECT count(*) FROM gs_e WHERE gs_e.id = gs_d.id) "
                "FROM gs_d GROUP BY g",
            )
            == "42803"
        )


def test_exclusion_constraints_and_match_full(home: Path) -> None:
    """An EXCLUDE constraint over `&&` refuses an overlapping range (23P01);
    a MATCH FULL foreign key refuses a partly-NULL key (23503)."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute(
            "CREATE TABLE ex_r (id int PRIMARY KEY, r int4range, EXCLUDE USING gist (r WITH &&))"
        )
        conn.execute("INSERT INTO ex_r VALUES (1, int4range(1, 5))")
        assert _sqlstate(conn, "INSERT INTO ex_r VALUES (2, int4range(4, 8))") == "23P01"
        conn.execute("INSERT INTO ex_r VALUES (3, int4range(5, 8))")
        conn.execute("CREATE TABLE fk_p (a int, b int, PRIMARY KEY (a, b))")
        conn.execute(
            "CREATE TABLE fk_c (id int PRIMARY KEY, a int, b int, "
            "FOREIGN KEY (a, b) REFERENCES fk_p (a, b) MATCH FULL)"
        )
        assert _sqlstate(conn, "INSERT INTO fk_c VALUES (1, 1, NULL)") == "23503"
        conn.execute("INSERT INTO fk_c VALUES (2, NULL, NULL)")


def test_pg_index_lists_every_index(home: Path) -> None:
    """Every index -- primary key, UNIQUE constraint, CREATE INDEX, over an
    expression -- has a pg_index row whose `indexrelid` is its pg_class oid,
    and `indkey` is an int2vector: `2 3` as text, subscripted from 0."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE pix (id int PRIMARY KEY, a int, b text, c int UNIQUE)")
        conn.execute("CREATE INDEX pix_ab ON pix (a, b)")
        conn.execute("CREATE INDEX pix_expr ON pix (lower(b))")
        assert _fetch(
            conn,
            "SELECT c.relname, c.relkind, i.indisunique, i.indisprimary, i.indkey::text "
            "FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid "
            "WHERE i.indrelid = 'pix'::regclass ORDER BY i.indexrelid",
        ) == [
            ("pix_pkey", "i", True, True, "1"),
            ("pix_c_key", "i", True, False, "4"),
            ("pix_ab", "i", False, False, "2 3"),
            ("pix_expr", "i", False, False, "0"),
        ]
        assert _fetch(
            conn,
            "SELECT indkey[0], pg_typeof(indkey)::text FROM pg_index "
            "WHERE indexrelid = 'pix_ab'::regclass",
        ) == [(2, "int2vector")]
        assert _fetch(
            conn,
            "SELECT a.attname FROM pg_index i JOIN pg_attribute a ON a.attrelid = i.indrelid "
            "AND a.attnum = ANY(i.indkey) WHERE i.indexrelid = 'pix_ab'::regclass ORDER BY 1",
        ) == [("a",), ("b",)]


def test_order_by_a_computed_output_column(home: Path) -> None:
    """`ORDER BY 1` / `ORDER BY alias` over a computed column sorts by the
    computed value -- text order for `n::text` -- in plain, DISTINCT and
    grouped queries alike."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE obo (id int PRIMARY KEY, n int)")
        conn.execute("INSERT INTO obo VALUES (1, 9), (2, 10), (3, 100)")
        expected = [("10",), ("100",), ("9",)]
        assert _fetch(conn, "SELECT n::text FROM obo ORDER BY 1") == expected
        assert _fetch(conn, "SELECT n::text AS s FROM obo ORDER BY s") == expected
        assert _fetch(conn, "SELECT DISTINCT n::text FROM obo ORDER BY 1") == expected
        assert _fetch(conn, "SELECT n::text FROM obo GROUP BY n ORDER BY n") == expected
        assert _fetch(conn, "SELECT -n FROM obo ORDER BY 1") == [(-100,), (-10,), (-9,)]
        assert _fetch(conn, "SELECT n % 2 AS p, sum(n) FROM obo GROUP BY n % 2 ORDER BY p") == [
            (0, 110),
            (1, 9),
        ]
        assert _sqlstate(conn, "SELECT n + id FROM obo GROUP BY n") == "42803"


def test_recursive_functions_and_the_depth_limit(home: Path) -> None:
    """A recursive PL/pgSQL function recurses (its result coerced to the
    declared `numeric`, so no integer overflow), and runaway recursion is
    54001 with the server still serving."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute(
            "CREATE FUNCTION rfact(n int) RETURNS numeric AS $$ BEGIN IF n <= 1 THEN "
            "RETURN 1; END IF; RETURN n * rfact(n - 1); END $$ LANGUAGE plpgsql"
        )
        assert _fetch(conn, "SELECT rfact(25)::text") == [("15511210043330985984000000",)]
        conn.execute(
            "CREATE FUNCTION runaway(n int) RETURNS int AS $$ BEGIN "
            "RETURN runaway(n + 1); END $$ LANGUAGE plpgsql"
        )
        assert _sqlstate(conn, "SELECT runaway(1)") == "54001"
        assert _fetch(conn, "SELECT 1") == [(1,)]


def test_drop_extension_cascade_drops_dependent_columns(home: Path) -> None:
    """RESTRICT refuses (2BP01); CASCADE drops the columns of the
    extension's types and leaves the rest of the table."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE EXTENSION hstore")
        conn.execute("CREATE TABLE dxc (id int PRIMARY KEY, h hstore, n int)")
        conn.execute("INSERT INTO dxc VALUES (1, 'a=>1', 5)")
        assert _sqlstate(conn, "DROP EXTENSION hstore") == "2BP01"
        conn.execute("DROP EXTENSION hstore CASCADE")
        assert _fetch(conn, "SELECT * FROM dxc") == [(1, 5)]
        assert _sqlstate(conn, "SELECT h FROM dxc") == "42703"


def test_text_search_configurations_for_every_language(home: Path) -> None:
    """Every PostgreSQL text-search configuration stems as PostgreSQL does:
    its Snowball stemmer and stop-word list; `russian` sends ASCII words to
    the English stemmer; under the `C.UTF-8` locale the server reports,
    every letter lowercases and only letters make words (PostgreSQL 15 on a
    UTF-8 locale; a `C` locale would call every non-ASCII character a letter
    and fold ASCII only)."""
    with _Server(home) as server, server.connect() as conn:
        assert _fetch(
            conn,
            "SELECT to_tsvector('french', 'Les chats mangeaient des souris')::text, "
            "to_tsvector('german', 'Die Katzen fraßen Mäuse')::text, "
            "to_tsvector('russian', 'Кошки running')::text, "
            "to_tsvector('simple', '«bonjour» a—b')::text",
        ) == [
            (
                "'chat':2 'le':1 'mang':3 'sour':5",
                "'frass':3 'katz':2 'maus':4",
                "'run':2 'кошк':1",
                "'a':2 'b':3 'bonjour':1",
            )
        ]
        assert _fetch(conn, "SELECT to_tsvector('basque', 'etxea')::text") == [("'etxea':1",)]
        assert _sqlstate(conn, "SELECT to_tsvector('klingon', 'x')") == "42704"


def test_read_committed_sees_other_connections_commits(home: Path) -> None:
    """Inside a READ COMMITTED block each statement sees what other
    connections committed before it -- rows and tables -- and may update a
    row another connection changed since the block began. REPEATABLE READ
    keeps the block's first snapshot."""
    with _Server(home) as server, server.connect() as a, server.connect() as b:
        a.execute("CREATE TABLE rcv (id int PRIMARY KEY, v int)")
        a.execute("INSERT INTO rcv VALUES (1, 10)")
        a.execute("BEGIN")
        assert _fetch(a, "SELECT v FROM rcv ORDER BY id") == [(10,)]
        b.execute("INSERT INTO rcv VALUES (2, 20)")
        b.execute("UPDATE rcv SET v = 11 WHERE id = 1")
        assert _fetch(a, "SELECT v FROM rcv ORDER BY id") == [(11,), (20,)]
        a.execute("UPDATE rcv SET v = v + 100 WHERE id = 1")
        assert _fetch(a, "SELECT v FROM rcv ORDER BY id") == [(111,), (20,)]
        a.execute("COMMIT")
        a.execute("BEGIN")
        a.execute("SELECT 1")
        b.execute("CREATE TABLE rcv_new (x int)")
        b.execute("INSERT INTO rcv_new VALUES (7)")
        assert _fetch(a, "SELECT x FROM rcv_new") == [(7,)]
        a.execute("ROLLBACK")
        a.execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        assert _fetch(a, "SELECT count(*) FROM rcv") == [(2,)]
        b.execute("INSERT INTO rcv VALUES (3, 30)")
        assert _fetch(a, "SELECT count(*) FROM rcv") == [(2,)]
        a.execute("COMMIT")


def test_xml_type_constructors_and_xpath(home: Path) -> None:
    """The `xml` type: input is checked (2200N), the SQL/XML constructors
    build what PostgreSQL builds, `xmlagg` concatenates, and `xpath` /
    `xmlexists` evaluate XPath 1.0."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("SET timezone = 'UTC'")
        assert _sqlstate(conn, "SELECT '<a>'::xml") == "2200N"
        assert _fetch(
            conn,
            "SELECT xmlelement(name foo, xmlattributes('a&b' AS x, 1 AS y), 'x<y', "
            "xmlelement(name e))::text, xmlforest('x' AS a, NULL AS b, 3 AS c)::text, "
            "xmlpi(name php, 'echo 1;')::text, xmlcomment('hi')::text",
        ) == [
            (
                '<foo x="a&amp;b" y="1">x&lt;y<e/></foo>',
                "<a>x</a><c>3</c>",
                "<?php echo 1;?>",
                "<!--hi-->",
            )
        ]
        conn.execute("CREATE TABLE xmt (id int PRIMARY KEY, doc xml)")
        conn.execute("INSERT INTO xmt VALUES (1, '<r><i>1</i></r>'), (2, '<r/>')")
        assert _sqlstate(conn, "INSERT INTO xmt VALUES (3, '<bad')") == "2200N"
        assert _fetch(
            conn, "SELECT xmlagg(xmlelement(name i, id) ORDER BY id DESC)::text FROM xmt"
        ) == [("<i>2</i><i>1</i>",)]
        assert _fetch(
            conn, "SELECT id FROM xmt WHERE xmlexists('/r/i' PASSING doc) ORDER BY id"
        ) == [(1,)]
        assert _fetch(
            conn,
            "SELECT xpath('/a/b/text()', '<a><b>x</b><b>y</b></a>')::text[], "
            "xpath('//n:b/text()', '<a xmlns:n=\"u\"><n:b>q</n:b></a>', "
            "ARRAY[ARRAY['n', 'u']])::text[]",
        ) == [(["x", "y"], ["q"])]
        assert _sqlstate(conn, "SELECT '<a/>'::xml = '<a/>'::xml") == "42883"


def test_untyped_range_accessor_is_ambiguous(home: Path) -> None:
    """`isempty('[1,2)')` names no range type: PostgreSQL cannot choose
    between the range and multirange overloads (42725)."""
    with _Server(home) as server, server.connect() as conn:
        assert _sqlstate(conn, "SELECT isempty('[1,2)')") == "42725"
        assert _fetch(conn, "SELECT isempty('[1,2)'::int4range)") == [(False,)]


def test_with_recursive(home: Path) -> None:
    """WITH RECURSIVE: a counter, a tree walk, UNION's cycle stop, and the
    42P19 refusal of a self-reference without a non-recursive term."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE wr_emp (id int PRIMARY KEY, boss int, name text)")
        conn.execute("INSERT INTO wr_emp VALUES (1, NULL, 'ceo'), (2, 1, 'cto'), (3, 2, 'dev')")
        assert _fetch(
            conn,
            "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 5) "
            "SELECT sum(n) FROM r",
        ) == [(15,)]
        assert _fetch(
            conn,
            "WITH RECURSIVE t(id, depth) AS (SELECT id, 0 FROM wr_emp WHERE boss IS NULL "
            "UNION ALL SELECT e.id, t.depth + 1 FROM wr_emp e JOIN t ON e.boss = t.id) "
            "SELECT id, depth FROM t ORDER BY id",
        ) == [(1, 0), (2, 1), (3, 2)]
        assert _fetch(
            conn,
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION SELECT (x % 3) + 1 FROM c) "
            "SELECT x FROM c ORDER BY x",
        ) == [(1,), (2,), (3,)]
        assert _sqlstate(conn, "WITH RECURSIVE r(n) AS (SELECT n FROM r) SELECT * FROM r") == (
            "42P19"
        )
        assert conn.execute(
            "WITH RECURSIVE r(n) AS (SELECT %s::int UNION ALL SELECT n + 1 FROM r WHERE n < %s) "
            "SELECT count(*) FROM r",
            (1, 4),
        ).fetchall() == [(4,)]


def test_from_clause_scope(home: Path) -> None:
    """A LATERAL subquery reads the row to its left; without LATERAL the
    same reference is 42P01, as is a qualifier naming nothing in FROM. A
    select-list `t.*` is the table's columns, and a qualified column works
    in IN."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE fs_c (id int PRIMARY KEY, name text)")
        conn.execute("CREATE TABLE fs_o (id int PRIMARY KEY, cid int, amt int)")
        conn.execute("INSERT INTO fs_c VALUES (1, 'a'), (2, 'b')")
        conn.execute("INSERT INTO fs_o VALUES (1, 1, 10), (2, 1, 20), (3, 2, 5)")
        assert _fetch(
            conn,
            "SELECT c.name, x.total FROM fs_c c, LATERAL "
            "(SELECT sum(amt) AS total FROM fs_o WHERE cid = c.id) x ORDER BY c.id",
        ) == [("a", 30), ("b", 5)]
        assert (
            _sqlstate(
                conn,
                "SELECT c.name FROM fs_c c, (SELECT sum(amt) FROM fs_o WHERE cid = c.id) x",
            )
            == "42P01"
        )
        assert _sqlstate(conn, "SELECT id FROM fs_o WHERE cid = fs_c.id") == "42P01"
        assert _fetch(conn, "SELECT c.* FROM fs_c c ORDER BY 1") == [(1, "a"), (2, "b")]
        assert _fetch(conn, "SELECT c.id FROM fs_c c WHERE c.name IN ('b') ORDER BY 1") == [(2,)]


def test_comment_on_and_descriptions(home: Path) -> None:
    """COMMENT ON a table, column, index and constraint, read back through
    obj_description / col_description; NULL or '' removes a comment."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute(
            "CREATE TABLE cmo (id int PRIMARY KEY, a int CONSTRAINT cmo_chk CHECK (a > 0))"
        )
        conn.execute("CREATE INDEX cmo_i ON cmo (a)")
        conn.execute("COMMENT ON TABLE cmo IS 'the table'")
        conn.execute("COMMENT ON COLUMN cmo.a IS 'col a'")
        conn.execute("COMMENT ON INDEX cmo_i IS 'idx'")
        conn.execute("COMMENT ON CONSTRAINT cmo_chk ON cmo IS 'chk'")
        assert _fetch(
            conn,
            "SELECT obj_description('cmo'::regclass, 'pg_class'), "
            "col_description('cmo'::regclass, 2), obj_description('cmo_i'::regclass)",
        ) == [("the table", "col a", "idx")]
        conn.execute("COMMENT ON TABLE cmo IS NULL")
        assert _fetch(conn, "SELECT obj_description('cmo'::regclass, 'pg_class')") == [(None,)]
        assert _sqlstate(conn, "COMMENT ON COLUMN cmo.nosuch IS 'x'") == "42703"
        assert _sqlstate(conn, "COMMENT ON TABLE nosuch IS 'x'") == "42P01"


def test_grant_and_revoke(home: Path) -> None:
    """GRANT / REVOKE validate their roles (42704) and relations (42P01) and
    privileges (0LP01); role membership and default privileges are
    accepted."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE grt (id int)")
        conn.execute("CREATE ROLE grr")
        conn.execute("GRANT SELECT, INSERT ON grt TO grr")
        conn.execute("GRANT ALL ON grt TO PUBLIC")
        conn.execute("REVOKE INSERT ON grt FROM grr")
        assert _sqlstate(conn, "GRANT SELECT ON grt TO nosuch") == "42704"
        assert _sqlstate(conn, "GRANT SELECT ON nosuch TO grr") == "42P01"
        assert _sqlstate(conn, "GRANT USAGE ON grt TO grr") == "0LP01"
        conn.execute("GRANT SELECT ON ALL TABLES IN SCHEMA public TO grr")
        conn.execute("ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO grr")
        conn.execute("GRANT grr TO CURRENT_USER")
        conn.execute("REVOKE grr FROM CURRENT_USER")


def test_domains(home: Path) -> None:
    """A domain's NOT NULL, CHECKs and DEFAULT apply on INSERT, UPDATE and
    casts; ALTER DOMAIN validates existing data; the wire type is the base
    type's."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE DOMAIN dmn_pos AS int CHECK (VALUE > 0)")
        conn.execute("CREATE DOMAIN dmn_s AS text NOT NULL DEFAULT 'x'")
        conn.execute("CREATE TABLE dmn_t (id int PRIMARY KEY, q dmn_pos, s dmn_s)")
        assert _sqlstate(conn, "INSERT INTO dmn_t VALUES (1, -1, 'a')") == "23514"
        assert _sqlstate(conn, "INSERT INTO dmn_t VALUES (1, 1, NULL)") == "23502"
        conn.execute("INSERT INTO dmn_t (id, q) VALUES (2, 3)")
        assert _fetch(conn, "SELECT q, s FROM dmn_t") == [(3, "x")]
        assert _sqlstate(conn, "UPDATE dmn_t SET q = 0") == "23514"
        assert _sqlstate(conn, "SELECT (-5)::dmn_pos") == "23514"
        assert _sqlstate(conn, "ALTER DOMAIN dmn_pos ADD CONSTRAINT tiny CHECK (VALUE < 2)") == (
            "23514"
        )
        cur = conn.execute("SELECT q FROM dmn_t")
        assert cur.description[0].type_code == 23
        assert _sqlstate(conn, "DROP DOMAIN dmn_pos") == "2BP01"
        assert _sqlstate(conn, "CREATE TABLE dmn_bad (a nosuchtype)") == "42704"


def test_materialized_views(home: Path) -> None:
    """A materialized view keeps its snapshot until REFRESH, refuses writes
    (42809), and is listed in pg_matviews; an aggregate query feeds it."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE mvb (id int PRIMARY KEY, g text, n int)")
        conn.execute("INSERT INTO mvb VALUES (1, 'a', 10), (2, 'a', 20), (3, 'b', 5)")
        conn.execute(
            "CREATE MATERIALIZED VIEW mvs AS SELECT g, sum(n) AS total FROM mvb GROUP BY g"
        )
        conn.execute("INSERT INTO mvb VALUES (4, 'b', 1)")
        assert _fetch(conn, "SELECT g, total FROM mvs ORDER BY g") == [("a", 30), ("b", 5)]
        conn.execute("REFRESH MATERIALIZED VIEW mvs")
        assert _fetch(conn, "SELECT g, total FROM mvs ORDER BY g") == [("a", 30), ("b", 6)]
        assert _sqlstate(conn, "DELETE FROM mvs") == "42809"
        assert _fetch(conn, "SELECT matviewname FROM pg_matviews") == [("mvs",)]
        conn.execute("DROP MATERIALIZED VIEW mvs")
        conn.execute("CREATE TABLE mva (g text, c int)")
        conn.execute("INSERT INTO mva SELECT g, count(*) FROM mvb GROUP BY g")
        assert _fetch(conn, "SELECT * FROM mva ORDER BY g") == [("a", 2), ("b", 2)]


def test_tablesample_and_size_functions(home: Path) -> None:
    """TABLESAMPLE at 100% keeps every row and at 0% none; the size
    functions answer and pg_size_pretty formats as PostgreSQL 14 does."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE tsz (id int PRIMARY KEY)")
        conn.execute("INSERT INTO tsz SELECT generate_series(1, 10)")
        assert _fetch(conn, "SELECT count(*) FROM tsz TABLESAMPLE SYSTEM (100)") == [(10,)]
        assert _fetch(conn, "SELECT count(*) FROM tsz TABLESAMPLE BERNOULLI (0)") == [(0,)]
        assert _sqlstate(conn, "SELECT * FROM tsz TABLESAMPLE SYSTEM (200)") == "2202H"
        assert _fetch(
            conn,
            "SELECT pg_size_pretty(10240::bigint), pg_size_pretty(1.5e12::numeric), "
            "pg_size_bytes('1.5 GB'), pg_total_relation_size('tsz') > 0",
        ) == [("10 kB", "1397 GB", 1610612736, True)]


def test_row_security_and_policies(home: Path) -> None:
    """ALTER TABLE's row-security flags show in pg_class; CREATE / ALTER /
    DROP POLICY keep pg_policies, with ruleutils-rendered expressions and
    PostgreSQL's duplicate / missing errors."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE rlt (id int PRIMARY KEY, owner text)")
        conn.execute("ALTER TABLE rlt ENABLE ROW LEVEL SECURITY")
        conn.execute("ALTER TABLE rlt FORCE ROW LEVEL SECURITY")
        flags = "SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE relname = 'rlt'"
        assert _fetch(conn, flags) == [(True, True)]
        conn.execute("CREATE POLICY p1 ON rlt USING (owner = current_user)")
        assert _sqlstate(conn, "CREATE POLICY p1 ON rlt USING (true)") == "42710"
        conn.execute("CREATE POLICY p2 ON rlt FOR INSERT WITH CHECK (id > 0)")
        conn.execute("ALTER POLICY p2 ON rlt RENAME TO p2b")
        assert _fetch(
            conn,
            "SELECT policyname, cmd, roles, qual, with_check FROM pg_policies ORDER BY 1",
        ) == [
            ("p1", "ALL", ["public"], "(owner = CURRENT_USER)", None),
            ("p2b", "INSERT", ["public"], None, "(id > 0)"),
        ]
        assert _sqlstate(conn, "DROP POLICY nosuch ON rlt") == "42704"
        conn.execute("DROP POLICY IF EXISTS nosuch ON rlt")
        conn.execute("DROP POLICY p1 ON rlt")
        conn.execute("ALTER TABLE rlt DISABLE ROW LEVEL SECURITY")
        assert _fetch(conn, flags) == [(False, True)]


def test_declarative_partitioning(home: Path) -> None:
    """Rows written to a partitioned table route to their partition (23514
    when none takes them), a partition reads only its rows, an UPDATE moves a
    row between partitions, and ATTACH / DETACH / DROP / COPY carry the rows
    with them."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE pr (id int PRIMARY KEY, d date) PARTITION BY RANGE (id)")
        conn.execute("CREATE TABLE pr_a PARTITION OF pr FOR VALUES FROM (MINVALUE) TO (10)")
        conn.execute("CREATE TABLE pr_b PARTITION OF pr FOR VALUES FROM (10) TO (20)")
        conn.execute("INSERT INTO pr VALUES (1, '2024-01-01'), (15, '2024-02-02')")
        assert _fetch(conn, "SELECT id FROM pr_a") == [(1,)]
        assert _fetch(conn, "SELECT id FROM pr_b") == [(15,)]
        assert _sqlstate(conn, "INSERT INTO pr VALUES (99, NULL)") == "23514"
        assert _sqlstate(conn, "INSERT INTO pr_a VALUES (12, NULL)") == "23514"
        assert (
            _sqlstate(conn, "CREATE TABLE pr_x PARTITION OF pr FOR VALUES FROM (5) TO (12)")
            == "42P17"
        )
        conn.execute("UPDATE pr SET id = 11 WHERE id = 1")
        assert _fetch(conn, "SELECT id FROM pr_b ORDER BY id") == [(11,), (15,)]
        assert _fetch(conn, "SELECT tableoid::regclass::text, id FROM pr ORDER BY id") == [
            ("pr_b", 11),
            ("pr_b", 15),
        ]
        cur = conn.cursor()
        with cur.copy("COPY pr_a FROM STDIN") as cp:
            cp.write("3\t2024-03-03\n")
        assert _fetch(conn, "SELECT id FROM pr ORDER BY id") == [(3,), (11,), (15,)]
        with cur.copy("COPY pr_b TO STDOUT") as cp:
            out = b"".join(bytes(b) for b in cp)
        assert sorted(out.splitlines()) == [b"11\t2024-01-01", b"15\t2024-02-02"]
        assert _fetch(
            conn,
            "SELECT relname, relkind, pg_get_expr(relpartbound, oid) FROM pg_class "
            "WHERE relname LIKE 'pr%' ORDER BY 1",
        ) == [
            ("pr", "p", None),
            ("pr_a", "r", "FOR VALUES FROM (MINVALUE) TO (10)"),
            ("pr_a_pkey", "i", None),
            ("pr_b", "r", "FOR VALUES FROM (10) TO (20)"),
            ("pr_b_pkey", "i", None),
            ("pr_pkey", "I", None),
        ]
        conn.execute("ALTER TABLE pr DETACH PARTITION pr_b")
        assert _fetch(conn, "SELECT id FROM pr_b ORDER BY id") == [(11,), (15,)]
        assert _fetch(conn, "SELECT id FROM pr ORDER BY id") == [(3,)]
        conn.execute("ALTER TABLE pr ATTACH PARTITION pr_b FOR VALUES FROM (10) TO (20)")
        assert _fetch(conn, "SELECT count(*) FROM pr") == [(3,)]
        conn.execute("DROP TABLE pr_b")
        assert _fetch(conn, "SELECT id FROM pr") == [(3,)]
        conn.execute("DROP TABLE pr")
        assert _fetch(conn, "SELECT count(*) FROM pg_class WHERE relname = 'pr_a'") == [(0,)]


def test_update_of_primary_key(home: Path) -> None:
    """A PRIMARY KEY column can be updated -- single and composite -- with
    PostgreSQL's row-by-row uniqueness: `id = id + 1` over (1, 2) collides."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE upk (id int PRIMARY KEY, v text)")
        conn.execute("INSERT INTO upk VALUES (1, 'a'), (2, 'b')")
        assert _sqlstate(conn, "UPDATE upk SET id = id + 1") == "23505"
        assert _fetch(conn, "UPDATE upk SET id = id + 10 RETURNING id, v") == [
            (11, "a"),
            (12, "b"),
        ]
        assert _fetch(conn, "SELECT v FROM upk WHERE id = 12") == [("b",)]
        conn.execute("CREATE TABLE upk2 (a int, b int, v text, PRIMARY KEY (a, b))")
        conn.execute("INSERT INTO upk2 VALUES (1, 1, 'x'), (2, 2, 'y')")
        conn.execute("UPDATE upk2 SET a = 5 WHERE a = 1")
        assert _fetch(conn, "SELECT a, b, v FROM upk2 ORDER BY a") == [(2, 2, "y"), (5, 1, "x")]
        assert _sqlstate(conn, "UPDATE upk2 SET a = 2, b = 2 WHERE a = 5") == "23505"


def test_gin_gist_brin_spgist_indexes(home: Path) -> None:
    """The non-btree access methods are accepted with PostgreSQL's operator
    class rules; btree_gin / btree_gist add scalar classes, and dropping one
    takes its dependent indexes (2BP01 without CASCADE)."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE gx (id int PRIMARY KEY, j jsonb, tags text[], n int)")
        conn.execute("INSERT INTO gx VALUES (1, '{\"a\": 1}', '{x}', 3)")
        conn.execute("CREATE INDEX gx_j ON gx USING gin (j jsonb_path_ops)")
        conn.execute("CREATE INDEX gx_tags ON gx USING gin (tags)")
        conn.execute("CREATE INDEX gx_n ON gx USING brin (n)")
        assert _sqlstate(conn, "CREATE INDEX gx_bad ON gx USING gin (n)") == "42704"
        assert _sqlstate(conn, "CREATE UNIQUE INDEX gx_u ON gx USING gin (j)") == "0A000"
        assert _fetch(conn, "SELECT indexdef FROM pg_indexes WHERE indexname = 'gx_j'") == [
            ("CREATE INDEX gx_j ON public.gx USING gin (j jsonb_path_ops)",)
        ]
        assert _fetch(conn, "SELECT id FROM gx WHERE j @> '{\"a\": 1}'") == [(1,)]
        conn.execute("CREATE EXTENSION btree_gin")
        conn.execute("CREATE INDEX gx_gn ON gx USING gin (n)")
        assert _sqlstate(conn, "DROP EXTENSION btree_gin") == "2BP01"
        conn.execute("DROP EXTENSION btree_gin CASCADE")
        assert _fetch(conn, "SELECT count(*) FROM pg_indexes WHERE indexname = 'gx_gn'") == [(0,)]


def test_rollback_to_savepoint_keeps_sequence_draws(home: Path) -> None:
    """ROLLBACK TO after an insert into a `serial` table works, and the draw
    is not undone -- PostgreSQL never rolls a sequence back. (Restoring the
    sequence's catalog there collided with its non-transactional write and
    failed the whole block with 40001.)"""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE sp_s (id serial PRIMARY KEY, v text)")
        conn.autocommit = False
        conn.execute("SAVEPOINT s")
        conn.execute("INSERT INTO sp_s (v) VALUES ('rolled back')")
        conn.execute("ROLLBACK TO s")
        conn.execute("INSERT INTO sp_s (v) VALUES ('kept')")
        conn.commit()
        assert _fetch(conn, "SELECT id, v FROM sp_s") == [(2, "kept")]


def test_composite_and_distinct_aggregate_ordering(home: Path) -> None:
    """Ordering comparisons of COMPOSITE values are decided (not NULL), and a
    DISTINCT aggregate follows its ORDER BY's direction and NULL placement,
    answering NULL over no rows."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TYPE cpo AS (a int, b int)")
        assert _fetch(conn, "SELECT row(1, 2)::cpo < row(1, 3)::cpo") == [(True,)]
        conn.execute("CREATE TABLE dag (id int PRIMARY KEY, s text)")
        assert _fetch(conn, "SELECT array_agg(DISTINCT s) FROM dag") == [(None,)]
        conn.execute("INSERT INTO dag VALUES (1, 'b'), (2, 'a'), (3, NULL), (4, 'b')")
        assert _fetch(conn, "SELECT array_agg(DISTINCT s ORDER BY s DESC) FROM dag") == [
            ([None, "b", "a"],)
        ]
        assert _fetch(conn, "SELECT string_agg(DISTINCT s, ',' ORDER BY s DESC) FROM dag") == [
            ("b,a",)
        ]


def test_sql_standard_and_variadic_functions(home: Path) -> None:
    """`RETURN expr` / `BEGIN ATOMIC ... END` bodies, VARIADIC parameters
    (packed, or passed whole with `VARIADIC ARRAY[...]`), and array
    parameters to a PL/pgSQL function."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE FUNCTION sa(x int) RETURNS int LANGUAGE sql RETURN x + 1")
        conn.execute(
            "CREATE FUNCTION sb(x int) RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT x * 2; END"
        )
        assert _fetch(conn, "SELECT sa(4), sb(4)") == [(5, 8)]
        assert (
            _sqlstate(conn, "CREATE FUNCTION sc() RETURNS int LANGUAGE plpgsql RETURN 5") == "42P13"
        )
        conn.execute(
            "CREATE FUNCTION vf(VARIADIC xs int[]) RETURNS int LANGUAGE sql "
            "AS 'SELECT array_length(xs, 1)'"
        )
        assert _fetch(conn, "SELECT vf(1, 2, 3), vf(7), vf(VARIADIC ARRAY[1, 2])") == [(3, 1, 2)]
        assert _sqlstate(conn, "SELECT vf()") == "42883"
        conn.execute(
            "CREATE FUNCTION vg(p text, VARIADIC xs int[]) RETURNS text LANGUAGE plpgsql "
            "AS $$ BEGIN RETURN p || array_to_string(xs, ','); END $$"
        )
        assert _fetch(conn, "SELECT vg('n=', 4, 5)") == [("n=4,5",)]


def test_instead_of_triggers_on_a_view(home: Path) -> None:
    """INSERT / UPDATE / DELETE on a view with INSTEAD OF triggers run the
    trigger per row with NEW / OLD in the view's columns; a trigger that
    answers NULL leaves the row uncounted. Views are relations in pg_class."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE it_t (id int PRIMARY KEY, name text)")
        conn.execute("INSERT INTO it_t VALUES (1, 'a')")
        conn.execute("CREATE VIEW it_v AS SELECT id, upper(name) AS uname FROM it_t")
        conn.execute(
            "CREATE FUNCTION it_f() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN "
            "IF TG_OP = 'INSERT' THEN INSERT INTO it_t VALUES (NEW.id, lower(NEW.uname)); "
            "RETURN NEW; ELSIF TG_OP = 'UPDATE' THEN UPDATE it_t SET name = lower(NEW.uname) "
            "WHERE id = OLD.id; RETURN NEW; ELSE IF OLD.id = 1 THEN RETURN NULL; END IF; "
            "DELETE FROM it_t WHERE id = OLD.id; RETURN OLD; END IF; END $$"
        )
        conn.execute(
            "CREATE TRIGGER it_trg INSTEAD OF INSERT OR UPDATE OR DELETE ON it_v "
            "FOR EACH ROW EXECUTE FUNCTION it_f()"
        )
        cur = conn.execute("INSERT INTO it_v VALUES (2, 'B'), (3, 'C')")
        assert cur.rowcount == 2
        conn.execute("UPDATE it_v SET uname = 'ZZ' WHERE id = 2")
        cur = conn.execute("DELETE FROM it_v")
        assert cur.rowcount == 2
        assert _fetch(conn, "SELECT id, name FROM it_t ORDER BY id") == [(1, "a")]
        assert (
            _sqlstate(
                conn,
                "CREATE TRIGGER bad INSTEAD OF INSERT ON it_t FOR EACH ROW EXECUTE FUNCTION it_f()",
            )
            == "42809"
        )
        assert _fetch(conn, "SELECT relkind FROM pg_class WHERE oid = 'it_v'::regclass") == [("v",)]


def test_constraint_triggers_and_set_constraints(home: Path) -> None:
    """A DEFERRABLE INITIALLY DEFERRED constraint trigger fires at COMMIT
    (its error fails the COMMIT and rolls back), SET CONSTRAINTS ...
    IMMEDIATE runs what is queued, and a deferred foreign key can be
    satisfied before COMMIT."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE ctt (id int PRIMARY KEY, n int)")
        conn.execute(
            "CREATE FUNCTION ctt_chk() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN "
            "IF NEW.n < 0 THEN RAISE EXCEPTION 'negative'; END IF; RETURN NULL; END $$"
        )
        conn.execute(
            "CREATE CONSTRAINT TRIGGER ctt_def AFTER UPDATE ON ctt DEFERRABLE INITIALLY "
            "DEFERRED FOR EACH ROW EXECUTE FUNCTION ctt_chk()"
        )
        conn.execute("INSERT INTO ctt VALUES (1, 1)")
        conn.autocommit = False
        conn.execute("UPDATE ctt SET n = -1")
        with pytest.raises(psycopg.errors.RaiseException):
            conn.commit()
        conn.rollback()
        assert _fetch(conn, "SELECT n FROM ctt") == [(1,)]
        conn.execute("UPDATE ctt SET n = -2")
        with pytest.raises(psycopg.errors.RaiseException):
            conn.execute("SET CONSTRAINTS ctt_def IMMEDIATE")
        conn.rollback()
        conn.autocommit = True
        conn.execute("CREATE TABLE ctp (id int PRIMARY KEY)")
        conn.execute("CREATE TABLE ctc (id int PRIMARY KEY, p int REFERENCES ctp DEFERRABLE)")
        conn.autocommit = False
        conn.execute("SET CONSTRAINTS ALL DEFERRED")
        conn.execute("INSERT INTO ctc VALUES (1, 10)")
        conn.execute("INSERT INTO ctp VALUES (10)")
        conn.commit()
        assert _fetch(conn, "SELECT p FROM ctc") == [(10,)]


def test_trigger_transition_tables(home: Path) -> None:
    """`REFERENCING NEW TABLE / OLD TABLE` make the statement's rows a table
    inside the trigger, and the table is gone afterwards."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE trt (id int PRIMARY KEY, n int)")
        conn.execute("CREATE TABLE trlog (msg text)")
        conn.execute(
            "CREATE FUNCTION trf() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN "
            "INSERT INTO trlog SELECT count(*) || ':' || coalesce(sum(n), 0) FROM nt; "
            "RETURN NULL; END $$"
        )
        conn.execute(
            "CREATE TRIGGER trg AFTER INSERT ON trt REFERENCING NEW TABLE AS nt "
            "FOR EACH STATEMENT EXECUTE FUNCTION trf()"
        )
        conn.execute("INSERT INTO trt VALUES (1, 10), (2, 20)")
        assert _fetch(conn, "SELECT msg FROM trlog") == [("2:30",)]
        assert _fetch(conn, "SELECT count(*) FROM pg_class WHERE relname = 'nt'") == [(0,)]


def test_geometric_types(home: Path) -> None:
    """point / lseg / line / path / polygon / circle: input and output forms,
    casts, the common operators and functions, stored columns, and binary
    results byte-identical to PostgreSQL 14's `*_send`."""
    with _Server(home) as server, server.connect() as conn:
        assert _fetch(
            conn,
            "SELECT '1 , 2'::point::text, '1,2,3,4'::lseg::text, '[(0,0),(1,1)]'::line::text, "
            "'1,2,3,4'::path::text, '(0,0),(1,0),(1,1)'::polygon::text, '1,2,3'::circle::text",
        ) == [
            (
                "(1,2)",
                "[(1,2),(3,4)]",
                "{1,-1,0}",
                "((1,2),(3,4))",
                "((0,0),(1,0),(1,1))",
                "<(1,2),3>",
            )
        ]
        assert _fetch(
            conn,
            "SELECT point(1.5, 2.25) <-> point(4, 6), circle '<(0,0),5>' @> point '(3,4)', "
            "(point '(1,2)' * point '(3,4)')::text, area(circle '<(0,0),1>')",
        ) == [(4.5069390943299865, True, "(-5,10)", 3.141592653589793)]
        assert _sqlstate(conn, "SELECT area('((0,0),(4,0),(4,3))'::polygon)") == "42883"
        conn.execute("CREATE TABLE gtt (id int PRIMARY KEY, p point, c circle)")
        conn.execute("INSERT INTO gtt VALUES (1, '(3,4)', '<(0,0),1>')")
        assert _fetch(
            conn, "SELECT p[0], p <-> point '(0,0)', length('[(0,0),(3,4)]'::lseg) FROM gtt"
        ) == [(3.0, 5.0, 5.0)]
        cur = conn.cursor(binary=True)
        cur.execute("SELECT p, c FROM gtt")
        assert cur.pgresult.fformat(0) == 1
        assert cur.pgresult.get_value(0, 0) == bytes.fromhex("40080000000000004010000000000000")


def test_data_modifying_with_and_returning_into(home: Path) -> None:
    """A data-modifying WITH item runs once and its RETURNING rows feed the
    query; PL/pgSQL's `INSERT ... RETURNING ... INTO` fills its target."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE dws (id int PRIMARY KEY, v text)")
        conn.execute("CREATE TABLE dwa (id int, v text)")
        conn.execute("INSERT INTO dws VALUES (1, 'a'), (2, 'b')")
        conn.execute(
            "WITH moved AS (DELETE FROM dws WHERE id = 1 RETURNING id, v) "
            "INSERT INTO dwa SELECT id, v FROM moved"
        )
        assert _fetch(conn, "SELECT * FROM dwa") == [(1, "a")]
        assert _fetch(conn, "SELECT id FROM dws") == [(2,)]
        conn.execute("CREATE TABLE dwi (id serial PRIMARY KEY, name text)")
        conn.execute(
            "CREATE FUNCTION dwadd(n text) RETURNS int LANGUAGE plpgsql AS $$ DECLARE x int; "
            "BEGIN INSERT INTO dwi (name) VALUES (n) RETURNING id INTO x; RETURN x; END $$"
        )
        assert _fetch(conn, "SELECT dwadd('p'), dwadd('q')") == [(1, 2)]


def test_function_overloads_and_pg_proc(home: Path) -> None:
    """Two functions with one name and arity resolve by argument type (an
    untyped literal prefers text), DROP names the overload, and pg_proc
    lists them."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE FUNCTION ov(a int) RETURNS text LANGUAGE sql AS 'SELECT ''int'''")
        conn.execute("CREATE FUNCTION ov(a text) RETURNS text LANGUAGE sql AS 'SELECT ''text'''")
        assert _fetch(conn, "SELECT ov(1), ov('x')") == [("int", "text")]
        assert _fetch(conn, "SELECT count(*) FROM pg_proc WHERE proname = 'ov'") == [(2,)]
        assert _sqlstate(conn, "DROP FUNCTION ov") == "42725"
        conn.execute("DROP FUNCTION ov(text)")
        assert _fetch(conn, "SELECT ov('5')") == [("int",)]


def test_role_membership_and_reg_types(home: Path) -> None:
    """CREATE ROLE ... IN ROLE / ROLE / ADMIN record memberships that
    pg_auth_members and pg_has_role answer from; regnamespace / regrole /
    regproc resolve and render names."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE ROLE ra")
        conn.execute("CREATE ROLE rb IN ROLE ra")
        conn.execute("CREATE ROLE rc")
        assert _fetch(
            conn,
            "SELECT pg_has_role('rb', 'ra', 'member'), pg_has_role('rc', 'ra', 'member')",
        ) == [(True, False)]
        assert _fetch(conn, "SELECT count(*) FROM pg_auth_members") == [(1,)]
        conn.execute("CREATE SCHEMA rs")
        conn.execute("CREATE FUNCTION rf(a int) RETURNS int LANGUAGE sql AS 'SELECT a'")
        assert _fetch(
            conn,
            "SELECT 'public'::regnamespace::oid, 'rs'::regnamespace::text, "
            "'rf'::regproc::text, 'rf(integer)'::regprocedure::text",
        ) == [(2200, "rs", "rf", "rf(integer)")]
        assert _sqlstate(conn, "SELECT 'nope'::regnamespace") == "3F000"


def test_jsonpath_datetime_and_nested_srfs(home: Path) -> None:
    """`.datetime()` parses ISO forms and templates, compares by kind, and a
    zone-crossing comparison needs a `*_tz` function; a set-returning call
    inside a select-list expression expands rows."""
    with _Server(home) as server, server.connect() as conn:
        assert _fetch(
            conn, "SELECT jsonb_path_query('\"2020-01-02 03:04:05+03\"', '$.datetime()')::text"
        ) == [('"2020-01-02T03:04:05+03:00"',)]
        assert _fetch(
            conn,
            'SELECT jsonb_path_query(\'["2020-01-02", "2019-05-05"]\', '
            "'$[*] ? (@.datetime() < \"2020-01-01\".datetime())')::text",
        ) == [('"2019-05-05"',)]
        tz_query = "'$.datetime() ? (@ < \"2020-01-02 01:00:00+00\".datetime())'"
        assert _sqlstate(conn, f"SELECT jsonb_path_query('\"2020-01-02\"', {tz_query})") == "0A000"
        assert _fetch(
            conn, f"SELECT count(*) FROM jsonb_path_query_tz('\"2020-01-02\"', {tz_query})"
        ) == [(1,)]
        assert _fetch(conn, "SELECT generate_series(1, 3) * 2") == [(2,), (4,), (6,)]


def test_row_level_security_enforced(home: Path) -> None:
    """With row security on, a non-owner sees and writes only what its
    policies allow: no policy denies everything, permissive policies OR,
    restrictive ones AND, a new row failing a check is 42501, and a
    reading UPDATE / DELETE is filtered by the SELECT policies too. A
    superuser and a BYPASSRLS role are not restricted."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE ROLE ra LOGIN")
        conn.execute("CREATE ROLE rb LOGIN BYPASSRLS")
        conn.execute("CREATE TABLE rt (id int, owner text, lvl int)")
        conn.execute("INSERT INTO rt VALUES (1, 'ra', 1), (2, 'x', 1), (3, 'ra', 5)")
        conn.execute("GRANT ALL ON rt TO ra, rb")
        conn.execute("ALTER TABLE rt ENABLE ROW LEVEL SECURITY")
        conn.execute("SET ROLE ra")
        assert _fetch(conn, "SELECT count(*) FROM rt") == [(0,)]
        conn.execute("RESET ROLE")
        conn.execute("CREATE POLICY s ON rt FOR SELECT USING (owner = current_user)")
        conn.execute("CREATE POLICY i ON rt FOR INSERT WITH CHECK (owner = current_user)")
        conn.execute("CREATE POLICY d ON rt FOR DELETE USING (lvl = 1)")
        conn.execute("CREATE POLICY r ON rt AS RESTRICTIVE FOR SELECT USING (lvl < 3)")
        conn.execute("SET ROLE ra")
        assert _fetch(conn, "SELECT id FROM rt ORDER BY id") == [(1,)]
        assert _sqlstate(conn, "INSERT INTO rt VALUES (9, 'x', 1)") == "42501"
        conn.execute("INSERT INTO rt VALUES (8, 'ra', 1)")
        # RETURNING reads the rows, so row 2 (not visible) is not deleted.
        assert sorted(_fetch(conn, "DELETE FROM rt RETURNING id")) == [(1,), (8,)]
        conn.execute("SET ROLE rb")
        assert _fetch(conn, "SELECT id FROM rt ORDER BY id") == [(2,), (3,)]
        conn.execute("RESET ROLE")
        assert _fetch(conn, "SELECT count(*) FROM rt") == [(2,)]


def test_array_lower_bounds(home: Path) -> None:
    """An array whose lower bound is not 1 keeps it: in its text form, its
    subscripts, `array_lower` / `array_dims`, equality, the functions that
    carry it (`array_append`, `||`), a stored column, and an assignment below
    or past its bounds, which extends it."""
    with _Server(home) as server, server.connect() as conn:
        assert _fetch(
            conn,
            "SELECT '[0:1]={a,b}'::text[]::text, ('[0:1]={a,b}'::text[])[0], "
            "array_lower('[0:1]={a,b}'::text[], 1), array_dims(array_fill(7, ARRAY[2], ARRAY[3]))",
        ) == [("[0:1]={a,b}", "a", 0, "[3:4]")]
        assert _fetch(
            conn,
            "SELECT '[0:1]={a,b}'::text[] = '{a,b}'::text[], "
            "(array_append('[0:1]={a,b}'::text[], 'c'))::text",
        ) == [(False, "[0:2]={a,b,c}")]
        conn.execute("CREATE TABLE lb (id int, a int[])")
        conn.execute("INSERT INTO lb VALUES (1, '{1,2}'), (2, '[0:1]={5,6}')")
        conn.execute("UPDATE lb SET a[0] = 9 WHERE id = 1")
        conn.execute("UPDATE lb SET a[3] = 8 WHERE id = 2")
        assert _fetch(conn, "SELECT a::text FROM lb ORDER BY id") == [
            ("[0:2]={9,1,2}",),
            ("[0:3]={5,6,NULL,8}",),
        ]
        assert _fetch(conn, "SELECT id FROM lb WHERE a = '[0:3]={5,6,NULL,8}'") == [(2,)]


def test_view_and_subquery_privileges(home: Path) -> None:
    """A view is checked as the view, its base tables as its owner; a
    subquery anywhere in the statement is checked with it."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE ROLE vu LOGIN")
        conn.execute("CREATE TABLE vb (id int)")
        conn.execute("CREATE TABLE vo (id int)")
        conn.execute("INSERT INTO vb VALUES (1), (2)")
        conn.execute("CREATE VIEW vv AS SELECT id FROM vb")
        conn.execute("GRANT SELECT ON vv TO vu")
        conn.execute("SET ROLE vu")
        assert _fetch(conn, "SELECT id FROM vv ORDER BY id") == [(1,), (2,)]
        assert _sqlstate(conn, "SELECT id FROM vb") == "42501"
        assert _sqlstate(conn, "SELECT id FROM vv WHERE id IN (SELECT id FROM vo)") == "42501"
        assert _sqlstate(conn, "UPDATE vv SET id = 3") == "42501"


def test_sql_prepare_execute(home: Path) -> None:
    """SQL `PREPARE` / `EXECUTE` / `DEALLOCATE`: arguments coerced to the
    parameters' types, the wrong count refused, and the statement listed in
    `pg_prepared_statements` with `from_sql`."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE ps (id int, v text)")
        conn.execute("PREPARE ins(int, text) AS INSERT INTO ps VALUES ($1, $2)")
        conn.execute("EXECUTE ins(1, 'a')")
        conn.execute("PREPARE sel AS SELECT v FROM ps WHERE id = $1")
        assert _fetch(conn, "EXECUTE sel('1')") == [("a",)]
        assert _sqlstate(conn, "EXECUTE sel") == "42601"
        assert _sqlstate(conn, "EXECUTE nope") == "26000"
        assert _sqlstate(conn, "PREPARE sel AS SELECT 1") == "42P05"
        assert _fetch(
            conn,
            "SELECT name, parameter_types::text, from_sql FROM pg_prepared_statements ORDER BY 1",
        ) == [("ins", "{integer,text}", True), ("sel", "{integer}", True)]
        conn.execute("DEALLOCATE ALL")
        assert _fetch(conn, "SELECT count(*) FROM pg_prepared_statements") == [(0,)]


def test_autocommit_write_waits_and_reevaluates(home: Path) -> None:
    """An autocommit UPDATE that meets another transaction's uncommitted
    write to its row waits for it and then builds on the committed value --
    it does not overwrite it (a lost update). Simple and extended protocol."""
    with _Server(home) as server, server.connect() as a, server.connect() as b:
        a.execute("CREATE TABLE lu (id int PRIMARY KEY, n int)")
        a.execute("INSERT INTO lu VALUES (1, 0)")
        for sql, args in (
            ("UPDATE lu SET n = n + 1 WHERE id = 1", None),
            ("UPDATE lu SET n = n + %s WHERE id = %s", (1, 1)),
        ):
            a.execute("BEGIN")
            a.execute("UPDATE lu SET n = n + 100 WHERE id = 1")
            worker = threading.Thread(target=b.execute, args=(sql, args))
            worker.start()
            time.sleep(0.3)
            a.execute("COMMIT")
            worker.join(10)
            assert not worker.is_alive()
        assert _fetch(a, "SELECT n FROM lu") == [(202,)]


def test_statement_and_lock_timeouts(home: Path) -> None:
    """`statement_timeout` cancels a running statement (57014) and
    `lock_timeout` a lock wait (55P03); `LOCK TABLE` needs a block, and a
    table locked ACCESS EXCLUSIVE shuts out another session's reads."""
    with _Server(home) as server, server.connect() as a, server.connect() as b:
        a.execute("SET statement_timeout = 100")
        assert _sqlstate(a, "SELECT pg_sleep(1)") == "57014"
        a.execute("RESET statement_timeout")
        assert _fetch(a, "SHOW statement_timeout") == [("0",)]
        a.execute("CREATE TABLE lt (id int)")
        assert _sqlstate(a, "LOCK TABLE lt") == "25P01"
        a.execute("BEGIN")
        a.execute("LOCK TABLE lt IN ACCESS EXCLUSIVE MODE")
        b.execute("SET lock_timeout = 100")
        assert _sqlstate(b, "SELECT count(*) FROM lt") == "55P03"
        b.execute("BEGIN")
        assert _sqlstate(b, "LOCK TABLE lt IN SHARE MODE NOWAIT") == "55P03"
        b.execute("ROLLBACK")
        a.execute("COMMIT")
        assert _fetch(b, "SELECT count(*) FROM lt") == [(0,)]


def test_statements_in_a_block_hold_table_locks(home: Path) -> None:
    """A read or write inside a block holds ACCESS SHARE / ROW EXCLUSIVE to
    its end: another session's conflicting LOCK waits (55P03 under a
    lock_timeout), pg_locks lists the hold, a cycle of waits is 40P01, and
    the aborted side's holds go at once so the other proceeds."""
    with _Server(home) as server, server.connect() as a, server.connect() as b:
        a.execute("CREATE TABLE lk1 (id int)")
        a.execute("CREATE TABLE lk2 (id int)")
        a.execute("BEGIN")
        a.execute("SELECT count(*) FROM lk1")
        b.execute("SET lock_timeout = 200")
        b.execute("BEGIN")
        assert _sqlstate(b, "LOCK TABLE lk1 IN ACCESS EXCLUSIVE MODE") == "55P03"
        b.execute("ROLLBACK")
        assert _fetch(
            a,
            "SELECT mode, granted FROM pg_locks WHERE relation = 'lk1'::regclass",
        ) == [("AccessShareLock", True)]
        a.execute("INSERT INTO lk1 VALUES (1)")
        b.execute("BEGIN")
        assert _sqlstate(b, "LOCK TABLE lk1 IN SHARE MODE") == "55P03"
        b.execute("ROLLBACK")
        a.execute("ROLLBACK")
        b.execute("SET lock_timeout = 0")
        a.execute("BEGIN")
        a.execute("SELECT 1 FROM lk1")
        b.execute("BEGIN")
        b.execute("SELECT 1 FROM lk2")
        out: dict[str, str | None] = {}
        worker = threading.Thread(
            target=lambda: out.setdefault(
                "a", _sqlstate(a, "LOCK TABLE lk2 IN ACCESS EXCLUSIVE MODE")
            )
        )
        worker.start()
        time.sleep(0.3)
        mine = _sqlstate(b, "LOCK TABLE lk1 IN ACCESS EXCLUSIVE MODE")
        worker.join(10)
        assert sorted([str(out.get("a")), str(mine)]) == ["40P01", "None"]
        a.execute("ROLLBACK")
        b.execute("ROLLBACK")


def test_maintenance_statements_and_cluster(home: Path) -> None:
    """VACUUM / ANALYZE / CHECKPOINT / REINDEX validate what they name, and
    CLUSTER rewrites the table in an index's order, recording it."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE mc (id int PRIMARY KEY, v text)")
        conn.execute("INSERT INTO mc VALUES (3, 'c'), (1, 'a'), (2, 'b')")
        conn.execute("CREATE INDEX mc_v ON mc (v DESC)")
        for ok in ("VACUUM mc", "ANALYZE mc (v)", "CHECKPOINT", "REINDEX TABLE mc"):
            conn.execute(ok)
        assert _sqlstate(conn, "VACUUM nosuch") == "42P01"
        assert _sqlstate(conn, "ANALYZE mc (nope)") == "42703"
        conn.execute("CLUSTER mc USING mc_v")
        assert _fetch(conn, "SELECT id FROM mc") == [(3,), (2,), (1,)]
        conn.execute("CLUSTER mc USING mc_pkey")
        assert _fetch(conn, "SELECT id FROM mc") == [(1,), (2,), (3,)]
        conn.execute("BEGIN")
        assert _sqlstate(conn, "VACUUM mc") == "25001"
        conn.execute("ROLLBACK")


def test_ordinality_rows_from_and_values_clauses(home: Path) -> None:
    """`WITH ORDINALITY` numbers a function's rows, `ROWS FROM` zips
    functions, a bare VALUES takes ORDER BY / LIMIT, and one output name
    twice in a FROM subquery is still two columns."""
    with _Server(home) as server, server.connect() as conn:
        assert _fetch(conn, "SELECT * FROM unnest(ARRAY['a','b']) WITH ORDINALITY AS t(v, n)") == [
            ("a", 1),
            ("b", 2),
        ]
        assert _fetch(
            conn,
            "SELECT * FROM ROWS FROM (generate_series(1,2), unnest(ARRAY['x','y','z'])) AS t(a, b)",
        ) == [(1, "x"), (2, "y"), (None, "z")]
        assert _fetch(conn, "VALUES (1), (2), (3) ORDER BY 1 DESC LIMIT 2") == [(3,), (2,)]
        assert _fetch(conn, "SELECT * FROM (SELECT 1 AS a, 2 AS a) s") == [(1, 2)]


def test_binary_cursor_over_any_source_sends_binary(home: Path) -> None:
    """A binary server-side cursor over VALUES, an aggregate or a constant
    select FETCHes binary values, as a SELECT source does. It used to send
    the text bytes, which a binary client decoded as garbage integers."""
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        sources = {
            "VALUES (1, 'a'::text), (2, 'b')": [(1, "a"), (2, "b")],
            "SELECT count(*), max(x) FROM generate_series(1, 4) x": [(4, 4)],
            "SELECT 1::int, 2.5::numeric, 'q'::text": [(1, dc.Decimal("2.5"), "q")],
        }
        for source, expected in sources.items():
            with conn.cursor(name="bc", binary=True) as cur:
                cur.execute(source)
                assert cur.fetchall() == expected, source
                assert cur.pgresult is not None
                assert cur.pgresult.fformat(0) == 1, source
            conn.rollback()


def test_a_partitions_own_constraints_are_enforced(home: Path) -> None:
    """NOT NULL, CHECK, UNIQUE and PRIMARY KEY declared on a PARTITION hold
    for the rows it takes, whether written through the parent or the
    partition; before, a partition's own constraints were accepted and never
    checked, so duplicate keys went in silently."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("CREATE TABLE pk_p (id int, v int, s text) PARTITION BY RANGE (id)")
        conn.execute(
            "CREATE TABLE pk_p1 PARTITION OF pk_p (v NOT NULL, s DEFAULT 'dflt', "
            "CONSTRAINT pk_p1_v CHECK (v > 0)) FOR VALUES FROM (0) TO (10)"
        )
        conn.execute(
            "CREATE TABLE pk_p2 PARTITION OF pk_p (PRIMARY KEY (id)) FOR VALUES FROM (10) TO (20)"
        )
        conn.execute("CREATE TABLE pk_p3 PARTITION OF pk_p (s UNIQUE) FOR VALUES FROM (20) TO (30)")
        assert _sqlstate(conn, "INSERT INTO pk_p (id, v) VALUES (1, NULL)") == "23502"
        assert _sqlstate(conn, "INSERT INTO pk_p (id, v) VALUES (1, -1)") == "23514"
        conn.execute("INSERT INTO pk_p1 (id, v) VALUES (4, 4)")
        assert _fetch(conn, "SELECT s FROM pk_p1 WHERE id = 4") == [("dflt",)]
        assert _sqlstate(conn, "INSERT INTO pk_p VALUES (11, 1, 'a'), (11, 2, 'b')") == "23505"
        conn.execute("INSERT INTO pk_p VALUES (11, 1, 'a')")
        assert _sqlstate(conn, "INSERT INTO pk_p2 VALUES (11, 2, 'b')") == "23505"
        assert _sqlstate(conn, "UPDATE pk_p SET id = 11 WHERE id = 4") == "23505"
        assert _sqlstate(conn, "INSERT INTO pk_p VALUES (21, 1, 'x'), (22, 2, 'x')") == "23505"
        assert _fetch(conn, "SELECT count(*) FROM pk_p") == [(2,)]


def test_ddl_waits_for_a_readers_lock(home: Path) -> None:
    """ALTER / DROP / rename / CLUSTER take ACCESS EXCLUSIVE, so they wait
    for a session still reading the table (55P03 under a lock_timeout);
    CREATE INDEX takes SHARE, which a reader does not block. A DDL inside a
    block holds its lock, so another session's read waits in turn."""
    with _Server(home) as server, server.connect() as a, server.connect() as b:
        a.execute("CREATE TABLE dw_t (id int)")
        b.execute("SET lock_timeout = 300")
        for ddl in [
            "ALTER TABLE dw_t ADD COLUMN x int",
            "DROP TABLE dw_t",
            "ALTER TABLE dw_t RENAME TO dw_t2",
        ]:
            a.execute("BEGIN")
            a.execute("SELECT * FROM dw_t")
            assert _sqlstate(b, ddl) == "55P03", ddl
            a.execute("ROLLBACK")
        a.execute("BEGIN")
        a.execute("SELECT * FROM dw_t")
        b.execute("CREATE INDEX ON dw_t (id)")
        a.execute("ROLLBACK")
        a.execute("BEGIN")
        a.execute("ALTER TABLE dw_t ADD COLUMN y int")
        assert _sqlstate(b, "SELECT count(*) FROM dw_t") == "55P03"
        a.execute("ROLLBACK")
        assert _fetch(b, "SELECT count(*) FROM dw_t") == [(0,)]


def test_mixed_result_formats_are_honoured_per_column(home: Path) -> None:
    """A Bind asking for binary, text, binary gets each column in its own
    format, as PostgreSQL answers it; a format count that matches neither one
    nor the columns is 08P01."""
    import socket
    import struct

    def msg(t: bytes, body: bytes) -> bytes:
        return t + struct.pack("!I", len(body) + 4) + body

    def cstr(s: str) -> bytes:
        return s.encode() + b"\0"

    def read_until_ready(sock: socket.socket) -> list[tuple[bytes, bytes]]:
        out, buf = [], b""
        while True:
            while len(buf) < 5:
                buf += sock.recv(65536)
            n = struct.unpack("!I", buf[1:5])[0]
            while len(buf) < 1 + n:
                buf += sock.recv(65536)
            out.append((buf[:1], buf[5 : 1 + n]))
            buf = buf[1 + n :]
            if out[-1][0] == b"Z":
                return out

    def run(sock: socket.socket, sql: str, formats: tuple[int, ...]) -> list[tuple[bytes, bytes]]:
        m = msg(b"P", cstr("") + cstr(sql) + struct.pack("!H", 0))
        m += msg(
            b"B",
            cstr("")
            + cstr("")
            + struct.pack("!HH", 0, 0)
            + struct.pack("!H", len(formats))
            + struct.pack("!" + "H" * len(formats), *formats),
        )
        m += msg(b"E", cstr("") + struct.pack("!I", 0)) + msg(b"S", b"")
        sock.sendall(m)
        return read_until_ready(sock)

    with _Server(home) as server:
        sock = socket.create_connection(("127.0.0.1", server.port))
        body = (
            struct.pack("!I", 196608)
            + cstr("user")
            + cstr("test")
            + cstr("database")
            + cstr("postgres")
            + b"\0"
        )
        sock.sendall(struct.pack("!I", len(body) + 4) + body)
        read_until_ready(sock)
        replies = run(sock, "SELECT 1::int4, 'x'::text, 2::int8", (1, 0, 1))
        rows = [b for t, b in replies if t == b"D"]
        assert len(rows) == 1
        row = rows[0]
        assert row == (
            struct.pack("!H", 3)
            + struct.pack("!i", 4)
            + struct.pack("!i", 1)
            + struct.pack("!i", 1)
            + b"x"
            + struct.pack("!i", 8)
            + struct.pack("!q", 2)
        )
        errors = [b for t, b in run(sock, "SELECT 1, 2, 3", (1, 0)) if t == b"E"]
        assert errors and b"C08P01\0" in errors[0]
        sock.close()


def test_sqlalchemy_reflection_matches_postgresql(home: Path) -> None:
    """SQLAlchemy's inspector reads the catalogs psql does, and more
    (`pg_opclass`, `pg_index.indclass` / `indoption`, set-returning
    functions over `pg_index`). Every value here is what PostgreSQL 15
    answers for the same tables."""
    sa = pytest.importorskip("sqlalchemy")
    with _Server(home) as server:
        engine = sa.create_engine(f"postgresql+psycopg://test@127.0.0.1:{server.port}/postgres")
        with engine.begin() as c:
            for q in [
                "create table sa_p (id serial primary key, code varchar(10) unique not null)",
                "create table sa_c (id int primary key, pid int references sa_p(id) "
                "on delete cascade, amt numeric(8,2) check (amt >= 0), note text)",
                "create index sa_c_amt on sa_c (amt desc, note)",
                "comment on table sa_c is 'kids'",
            ]:
                c.execute(sa.text(q))
        i = sa.inspect(engine)
        assert [(c["name"], str(c["type"]), c["nullable"]) for c in i.get_columns("sa_c")] == [
            ("id", "INTEGER", False),
            ("pid", "INTEGER", True),
            ("amt", "NUMERIC(8, 2)", True),
            ("note", "TEXT", True),
        ]
        assert i.get_pk_constraint("sa_c")["constrained_columns"] == ["id"]
        fks = i.get_foreign_keys("sa_c")
        assert [(k["name"], k["referred_table"], k["options"]) for k in fks] == [
            ("sa_c_pid_fkey", "sa_p", {"ondelete": "CASCADE"})
        ]
        assert [
            (x["name"], x["column_names"], x.get("column_sorting")) for x in i.get_indexes("sa_c")
        ] == [("sa_c_amt", ["amt", "note"], {"amt": ("desc",)})]
        assert i.get_check_constraints("sa_c") == [
            {"name": "sa_c_amt_check", "sqltext": "amt >= 0::numeric", "comment": None}
        ]
        assert i.get_unique_constraints("sa_p")[0]["column_names"] == ["code"]
        assert i.get_table_comment("sa_c") == {"text": "kids"}
        engine.dispose()


def test_delete_using_a_derived_table_deletes_only_its_matches(home: Path) -> None:
    """A table named inside a derived table in USING is that table, not the target.

    `DELETE FROM t USING (SELECT v.id FROM v WHERE id = 1) s WHERE t.id = s.id`
    deleted EVERY row of `t`: the derived table's own relation was read as an
    outer reference, so its filter never applied. PostgreSQL deletes one row.
    """
    with _Server(home) as server, server.connect() as c:
        c.execute("create table dq_o (id int, n int)")
        c.execute("insert into dq_o values (1, 10), (2, 20), (3, 30)")
        c.execute("create view dq_v as select id, n from dq_o")
        cur = c.execute(
            "delete from dq_o using (select dq_v.id as o_id from dq_v where id = 1) as s "
            "where dq_o.id = s.o_id"
        )
        assert cur.statusmessage == "DELETE 1"
        assert c.execute("select id from dq_o order by id").fetchall() == [(2,), (3,)]


def test_a_rule_action_runs_once_per_statement_not_per_row(home: Path) -> None:
    """A rule rewrites the statement; it is not a trigger.

    An action naming neither NEW nor OLD runs once, and an UPDATE's action
    runs BEFORE the UPDATE, so an aggregate in it sees the old rows. Run as
    per-row triggers, both came out wrong with no error.
    """
    with _Server(home) as server, server.connect() as c:
        c.execute("create table rq_t (id int, v int)")
        c.execute("create table rq_log (n bigint)")
        c.execute("insert into rq_t values (1, 1), (2, 2), (3, 3)")
        c.execute("create rule rq_u as on update to rq_t do also insert into rq_log values (1)")
        c.execute("update rq_t set v = v + 1")
        assert c.execute("select count(*) from rq_log").fetchone() == (1,)
        c.execute(
            "create rule rq_d as on delete to rq_t do also "
            "insert into rq_log select count(*) from rq_t"
        )
        c.execute("delete from rq_t where id = 1")
        assert c.execute("select max(n) from rq_log").fetchone() == (3,)


def test_latin1_binary_text_arrays_and_untranslatable_names(home: Path) -> None:
    """A binary text-family array is transcoded ELEMENT by element.

    Its wire form interleaves big-endian length words with the text, so a
    whole-payload transcode would corrupt it; each element is converted and
    its length rewritten. A column NAME the client encoding cannot hold is
    22P05, as the RowDescription PostgreSQL writes refuses it -- not the
    name's UTF-8 bytes. And a 2-D ``varchar[]`` is a 2-D array in text, not a
    1-D array of the sub-arrays' text.
    """
    with _Server(home) as server, server.connect() as c:
        c.execute("SET client_encoding = 'LATIN1'")
        cur = c.execute("SELECT ARRAY['café', NULL, 'ü']::varchar[]", binary=True)
        assert cur.fetchone() == (["café", None, "ü"],)
        with pytest.raises(psycopg.errors.UntranslatableCharacter):
            c.execute("SELECT ARRAY[chr(20013)]::text[]", binary=True)
        with pytest.raises(psycopg.errors.UntranslatableCharacter):
            c.execute('SELECT 1 AS U&"\\20AC"')
        assert c.execute('SELECT 1 AS U&"\\00E9"').description[0].name == "é"
        cur = c.execute("SELECT '{{a,b},{c,\"d e\"}}'::varchar[]")
        assert cur.fetchone() == ([["a", "b"], ["c", "d e"]],)


def test_column_grants_are_shared_with_the_python_server(home: Path) -> None:
    """Grants live in the shared catalog, in the Python server's own shape.

    A column grant (`GRANT SELECT (v)`) is a row of `__sql_column_grants__`
    and a touched ACL a row of `__sql_relation_acl__`, so either server reads
    what the other wrote: the Python server reports the same `relacl` and
    `attacl` for a store the Rust server granted on. A column grant used to be
    recorded as a TABLE grant, which let the grantee read every column.
    """
    with _Server(home) as server, server.connect() as c:
        c.execute("create role cg_r")
        c.execute("create table cg_t (id int, v text)")
        c.execute("grant select (v) on cg_t to cg_r")
        c.execute("grant select on cg_t to cg_r")
        c.execute("revoke select on cg_t from cg_r")
        c.execute("grant select (v) on cg_t to cg_r")
        c.execute("set role cg_r")
        assert c.execute("select v from cg_t").fetchall() == []
        with pytest.raises(psycopg.errors.InsufficientPrivilege):
            c.execute("select id from cg_t")
        c.execute("reset role")
        rust_acl = c.execute(
            "select relacl::text, (select attacl::text from pg_attribute"
            " where attrelid = 'cg_t'::regclass and attname = 'v')"
            " from pg_class where relname = 'cg_t'"
        ).fetchone()
    owner = rust_acl[0].strip("{}").split("=")[0]
    assert rust_acl == (f"{{{owner}=arwdDxt/{owner}}}", f"{{cg_r=r/{owner}}}")
    python_grants = _python_sql(
        home,
        "select grantee, privilege_type from information_schema.column_privileges"
        " where table_name = 'cg_t' and grantee = 'cg_r'",
    )
    assert python_grants == [("cg_r", "SELECT")]


def test_procedure_transaction_control_and_do_block_atomicity(home: Path) -> None:
    """COMMIT / ROLLBACK in a procedure, and a DO block's writes, as on PG 15.

    Three bugs pinned together, each measured against PostgreSQL 15:

    * a DO block the function interpreter runs WROTE EACH STATEMENT ON ITS
      OWN, so one that raised after inserting left the insert committed;
    * a BEGIN ... EXCEPTION block did not undo its writes when its handler
      caught an error (PostgreSQL runs such a block as a subtransaction);
    * a procedure's COMMIT / ROLLBACK was refused everywhere. It is allowed in
      a CALL run alone outside a transaction block, and 2D000 inside a block
      or a multi-statement query string (an implicit block).
    """
    with _Server(home) as server, server.connect() as c:
        c.execute("create table pt_t (n int)")
        with pytest.raises(psycopg.errors.RaiseException):
            c.execute(
                "do $$ declare x int := 1; begin insert into pt_t values (x);"
                " raise exception 'boom'; end $$"
            )
        assert c.execute("select count(*) from pt_t").fetchone() == (0,)
        c.execute(
            "do $$ begin begin insert into pt_t values (1); raise exception 'x';"
            " exception when others then null; end; insert into pt_t values (2); end $$"
        )
        assert c.execute("select n from pt_t").fetchall() == [(2,)]
        c.execute(
            "create procedure pt_p() language plpgsql as $$ begin"
            " insert into pt_t values (10); commit; insert into pt_t values (11);"
            " rollback; insert into pt_t values (12); end $$"
        )
        c.execute("call pt_p()")
        assert c.execute("select n from pt_t order by 1").fetchall() == [(2,), (10,), (12,)]
        c.execute("begin")
        with pytest.raises(psycopg.errors.InvalidTransactionTermination):
            c.execute("call pt_p()")
        c.execute("rollback")
        with pytest.raises(psycopg.errors.InvalidTransactionTermination):
            c.execute("insert into pt_t values (20); call pt_p()")
        assert c.execute("select count(*) from pt_t").fetchone() == (3,)


@pytest.mark.parametrize(
    ("sql", "sqlstate", "position"),
    [
        # Raised while the rows stream, after planning: still positioned.
        ("select g + 'a' from generate_series(1, 2) g", "22P02", 12),
        # A record without that field: PostgreSQL points at the record.
        ("select (c).b + 1 from (select row(1,'x')::record as c) s", "42703", 9),
        # Judged statically, so even over no rows.
        ("select x::int + true from ep_t", "42883", 15),
        ("select (c).b + 1 from ep_t", "42883", 14),
    ],
)
def test_runtime_and_static_errors_carry_postgres_positions(
    home: Path, sql: str, sqlstate: str, position: int
) -> None:
    """Positions measured on PostgreSQL 15 for the same statements."""
    with _Server(home) as server, server.connect() as c:
        c.execute("create type ep_c as (a int, b text)")
        c.execute("create table ep_t (id int, c ep_c, x text)")
        with pytest.raises(psycopg.Error) as caught:
            c.execute(sql)
        assert caught.value.sqlstate == sqlstate
        assert caught.value.diag.statement_position == str(position)
