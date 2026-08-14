"""Aggressive concurrency suite for ``SecantusPGServer``.

Many psycopg connections hammer one server (one shared ``Storage``) at once,
with barriers to maximize simultaneous arrivals. Every test asserts a hard
integrity invariant — exact final counts, exactly-one-winner races, a conserved
total under concurrent transfers — plus error *hygiene*: the only errors a
loser may see are the typed SQLSTATEs a real Postgres would send (23505
unique_violation, 40001 serialization_failure), never ``XX000 internal error``
or a dropped connection.

These pinned three concurrency fixes:

* a storage-level write-write conflict (``WriteConflictError`` / WT_ROLLBACK)
  now maps to SQLSTATE 40001 instead of escaping as ``XX000 internal error``;
* DML statements (and bare ``nextval``) serialize per storage, closing the
  check-then-write races that let concurrent inserts double-satisfy a UNIQUE
  constraint and concurrent ``SET n = n + 1`` updates lose increments;
* the loser of a conflict keeps a usable connection and a clean session.

(The divergence formerly noted here — two open transactions both committing
the same UNIQUE value — was closed by #775/#778: SQL UNIQUE constraints are
backed by a storage unique index WiredTiger itself enforces.)
"""

from __future__ import annotations

import random
import threading

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")

WORKERS = 8


@pytest.fixture
def server(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()
        st.close()


def connect(srv, **kw):
    host, port = srv.address
    kw.setdefault("autocommit", True)
    return psycopg.connect(host=host, port=port, dbname="db", user="joe", **kw)


def run_workers(n: int, target) -> None:
    """Run ``target(i)`` on ``n`` threads; re-raise the first worker failure."""
    failures: list[BaseException] = []

    def guarded(i: int) -> None:
        try:
            target(i)
        except BaseException as exc:  # noqa: BLE001 — surface into pytest
            failures.append(exc)

    threads = [threading.Thread(target=guarded, args=(i,)) for i in range(n)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    if failures:
        raise failures[0]


def sqlstate(exc: BaseException) -> str | None:
    return getattr(getattr(exc, "diag", None), "sqlstate", None)


# --------------------------------------------------------------------------- #
# plain-write storms


def test_parallel_autocommit_inserts_all_land(server):
    with connect(server) as c:
        c.execute("CREATE TABLE t (id bigint primary key, who int)")
    rows_per = 40
    barrier = threading.Barrier(WORKERS)

    def worker(i: int) -> None:
        with connect(server) as c:
            barrier.wait()
            for r in range(rows_per):
                c.execute("INSERT INTO t VALUES (%s, %s)", (i * rows_per + r, i))

    run_workers(WORKERS, worker)
    with connect(server) as c:
        n, distinct = c.execute("SELECT count(*), count(distinct id) FROM t").fetchone()
    assert n == WORKERS * rows_per
    assert distinct == WORKERS * rows_per


def test_same_pk_insert_race_has_single_winner_per_round(server):
    with connect(server) as c:
        c.execute("CREATE TABLE pk (id bigint primary key, who int)")
    rounds = 20
    barrier = threading.Barrier(WORKERS)
    wins = [0] * WORKERS
    loser_states: set[str | None] = set()
    lock = threading.Lock()

    def worker(i: int) -> None:
        with connect(server) as c:
            for r in range(rounds):
                barrier.wait()
                try:
                    c.execute("INSERT INTO pk VALUES (%s, %s)", (r, i))
                    wins[i] += 1
                except psycopg.Error as exc:
                    with lock:
                        loser_states.add(sqlstate(exc))

    run_workers(WORKERS, worker)
    assert sum(wins) == rounds, f"want exactly one winner per round, got {sum(wins)}"
    assert loser_states == {"23505"}, f"losers must see unique_violation, saw {loser_states}"
    with connect(server) as c:
        assert c.execute("SELECT count(*) FROM pk").fetchone() == (rounds,)


def test_unique_constraint_race_has_single_winner_per_value(server):
    # The race that found the check-then-insert hole: distinct PKs, same UNIQUE
    # value, all arriving together. Exactly one row per value may land.
    with connect(server) as c:
        c.execute("CREATE TABLE uq (id bigint primary key, val int unique, who int)")
    rounds = 20
    barrier = threading.Barrier(WORKERS)
    wins = [0] * WORKERS
    loser_states: set[str | None] = set()
    lock = threading.Lock()

    def worker(i: int) -> None:
        with connect(server) as c:
            for r in range(rounds):
                barrier.wait()
                try:
                    c.execute("INSERT INTO uq VALUES (%s, %s, %s)", (i * 1000 + r, r, i))
                    wins[i] += 1
                except psycopg.Error as exc:
                    with lock:
                        loser_states.add(sqlstate(exc))

    run_workers(WORKERS, worker)
    assert sum(wins) == rounds
    assert loser_states == {"23505"}
    with connect(server) as c:
        n, distinct = c.execute("SELECT count(*), count(distinct val) FROM uq").fetchone()
    assert (n, distinct) == (rounds, rounds), "duplicate UNIQUE values were stored"


# --------------------------------------------------------------------------- #
# read-modify-write atomicity


def test_autocommit_computed_updates_lose_no_increments(server):
    # ``SET n = n + 1`` evaluates the RHS against the pre-image, so without
    # statement serialization two connections both read n and both write n+1.
    # (Pre-fix this measured 83 of 400.)
    with connect(server) as c:
        c.execute("CREATE TABLE ctr (id bigint primary key, n int)")
        c.execute("INSERT INTO ctr VALUES (1, 0)")
    per_worker = 50

    from secantus.sql import pgextended

    counters_at_start = dict(pgextended.COUNTERS)
    outcomes: dict[int, list[str]] = {}

    def worker(i: int) -> None:
        log = outcomes.setdefault(i, [])
        with connect(server) as c:
            for _ in range(per_worker):
                cur = c.execute("UPDATE ctr SET n = n + 1 WHERE id = 1")
                log.append(str(cur.rowcount))

    run_workers(WORKERS, worker)
    with connect(server) as c:
        n = c.execute("SELECT n FROM ctr").fetchone()[0]
        delta = {k: pgextended.COUNTERS[k] - counters_at_start[k] for k in counters_at_start}
        assert n == WORKERS * per_worker, (
            f"lost increments: n={n}, implicit-txn deltas THIS TEST={delta}, "
            f"per-worker rowcounts={outcomes}"
        )


def test_transactional_increments_retry_to_exact_total(server):
    # BEGIN → UPDATE → COMMIT loses first-updater-wins races; the loser must see
    # 40001 (and only 40001), and ROLLBACK + retry must converge on the exact
    # total — every increment happens exactly once.
    with connect(server) as c:
        c.execute("CREATE TABLE ctr (id bigint primary key, n int)")
        c.execute("INSERT INTO ctr VALUES (1, 0)")
    per_worker = 25
    error_states: set[str | None] = set()
    lock = threading.Lock()

    def worker(i: int) -> None:
        with connect(server, autocommit=False) as c:
            done = 0
            while done < per_worker:
                try:
                    c.execute("UPDATE ctr SET n = n + 1 WHERE id = 1")
                    c.commit()
                    done += 1
                except psycopg.Error as exc:
                    with lock:
                        error_states.add(sqlstate(exc))
                    c.rollback()

    run_workers(WORKERS, worker)
    assert error_states <= {"40001"}, f"only serialization_failure is retriable: {error_states}"
    with connect(server) as c:
        assert c.execute("SELECT n FROM ctr").fetchone() == (WORKERS * per_worker,)


def test_write_write_conflict_is_serialization_failure(server):
    # Deterministic two-transaction conflict: the loser gets a typed 40001 (not
    # XX000), can ROLLBACK, and the connection stays fully usable.
    with connect(server) as c:
        c.execute("CREATE TABLE t (id bigint primary key, n int)")
        c.execute("INSERT INTO t VALUES (1, 0)")
    with connect(server, autocommit=False) as a, connect(server, autocommit=False) as b:
        a.execute("UPDATE t SET n = 10 WHERE id = 1")
        with pytest.raises(psycopg.errors.SerializationFailure) as excinfo:
            b.execute("UPDATE t SET n = 20 WHERE id = 1")
        assert sqlstate(excinfo.value) == "40001"
        b.rollback()
        a.commit()
        assert b.execute("SELECT n FROM t WHERE id = 1").fetchone() == (10,)


def test_concurrent_transfers_conserve_the_total(server):
    # The classic bank invariant: random transfers between 10 accounts in
    # transactions, while readers continuously sum balances. Every committed
    # snapshot must show the full total — no torn transfer is ever visible —
    # and the final state is exact.
    accounts, opening = 10, 100
    with connect(server) as c:
        c.execute("CREATE TABLE acct (id bigint primary key, balance int)")
        for i in range(accounts):
            c.execute("INSERT INTO acct VALUES (%s, %s)", (i, opening))
    total = accounts * opening
    transfers_per, writer_n, reader_n = 30, 6, 2
    stop = threading.Event()
    torn_sums: list[int] = []

    def writer(i: int) -> None:
        rng = random.Random(1000 + i)
        with connect(server, autocommit=False) as c:
            done = 0
            while done < transfers_per:
                src, dst = rng.sample(range(accounts), 2)
                amount = rng.randint(1, 25)
                try:
                    c.execute("UPDATE acct SET balance = balance - %s WHERE id = %s", (amount, src))
                    c.execute("UPDATE acct SET balance = balance + %s WHERE id = %s", (amount, dst))
                    c.commit()
                    done += 1
                except psycopg.Error as exc:
                    assert sqlstate(exc) == "40001", f"unexpected transfer error: {exc}"
                    c.rollback()

    def reader(i: int) -> None:
        with connect(server) as c:
            while not stop.is_set():
                (s,) = c.execute("SELECT sum(balance) FROM acct").fetchone()
                if s != total:
                    torn_sums.append(int(s))
                    return

    readers = [threading.Thread(target=reader, args=(i,)) for i in range(reader_n)]
    for t in readers:
        t.start()
    try:
        run_workers(writer_n, writer)
    finally:
        stop.set()
        for t in readers:
            t.join()
    assert not torn_sums, f"a reader observed a torn transfer: sum={torn_sums[0]} != {total}"
    with connect(server) as c:
        n, s = c.execute("SELECT count(*), sum(balance) FROM acct").fetchone()
    assert (n, s) == (accounts, total)


def test_concurrent_nextval_never_repeats(server):
    with connect(server) as c:
        c.execute("CREATE SEQUENCE seq")
    draws_per = 30
    drawn: list[int] = []
    lock = threading.Lock()
    barrier = threading.Barrier(WORKERS)

    def worker(i: int) -> None:
        with connect(server) as c:
            barrier.wait()
            got = [c.execute("SELECT nextval('seq')").fetchone()[0] for _ in range(draws_per)]
        with lock:
            drawn.extend(got)

    run_workers(WORKERS, worker)
    assert len(drawn) == WORKERS * draws_per
    assert len(set(drawn)) == len(drawn), "nextval handed out a duplicate value"


# --------------------------------------------------------------------------- #
# mixed workloads


def test_ddl_churn_alongside_dml(server):
    with connect(server) as c:
        c.execute("CREATE TABLE shared (id bigint primary key, who int)")
    ddl_n, dml_n, cycles, rows_per = 4, 3, 6, 25

    def ddl_worker(i: int) -> None:
        with connect(server) as c:
            for cycle in range(cycles):
                name = f"scratch_{i}_{cycle}"
                c.execute(f"CREATE TABLE {name} (id bigint primary key, v text)")
                for r in range(10):
                    c.execute(f"INSERT INTO {name} VALUES (%s, %s)", (r, f"v{r}"))
                assert c.execute(f"SELECT count(*) FROM {name}").fetchone() == (10,)
                c.execute(f"DROP TABLE {name}")

    def dml_worker(i: int) -> None:
        with connect(server) as c:
            for r in range(rows_per):
                c.execute("INSERT INTO shared VALUES (%s, %s)", (i * rows_per + r, i))

    def worker(i: int) -> None:
        (ddl_worker if i < ddl_n else dml_worker)(i if i < ddl_n else i - ddl_n)

    run_workers(ddl_n + dml_n, worker)
    with connect(server) as c:
        assert c.execute("SELECT count(*) FROM shared").fetchone() == (dml_n * rows_per,)
        leftovers = c.execute(
            "SELECT count(*) FROM information_schema.tables WHERE table_name LIKE 'scratch_%'"
        ).fetchone()
    assert leftovers == (0,), "a dropped scratch table survived the churn"


def test_connection_churn_under_write_load(server):
    with connect(server) as c:
        c.execute("CREATE TABLE t (id bigint primary key, who int)")
    churners, cycles, writer_rows = 20, 5, 100
    stop = threading.Event()

    def background_writer() -> None:
        with connect(server) as c:
            for r in range(writer_rows):
                c.execute("INSERT INTO t VALUES (%s, 0)", (r,))
        stop.set()

    bg = threading.Thread(target=background_writer)
    bg.start()

    def churner(i: int) -> None:
        for _ in range(cycles):
            with connect(server) as c:
                assert c.execute("SELECT 1").fetchone() == (1,)
                (n,) = c.execute("SELECT count(*) FROM t").fetchone()
                assert 0 <= n <= writer_rows

    try:
        run_workers(churners, churner)
    finally:
        bg.join()
    with connect(server) as c:
        assert c.execute("SELECT count(*) FROM t").fetchone() == (writer_rows,)


def test_extended_protocol_prepared_statements_concurrently(server):
    # prepare_threshold=0 forces server-side prepared statements immediately, so
    # every thread drives Parse/Bind/Describe/Execute (and psycopg's DEALLOCATE
    # recycling) on the shared engine at once.
    with connect(server) as c:
        c.execute("CREATE TABLE t (id bigint primary key, who int, tag text)")
    rows_per = 30
    host, port = server.address

    def worker(i: int) -> None:
        with psycopg.connect(
            host=host, port=port, dbname="db", user="joe", autocommit=True, prepare_threshold=0
        ) as c:
            for r in range(rows_per):
                pk = i * rows_per + r
                c.execute("INSERT INTO t VALUES (%s, %s, %s)", (pk, i, f"tag{i}"))
                got = c.execute("SELECT who, tag FROM t WHERE id = %s", (pk,)).fetchone()
                assert got == (i, f"tag{i}")
            (mine,) = c.execute("SELECT count(*) FROM t WHERE who = %s", (i,)).fetchone()
            assert mine == rows_per

    run_workers(WORKERS, worker)
    with connect(server) as c:
        assert c.execute("SELECT count(*) FROM t").fetchone() == (WORKERS * rows_per,)


def test_dual_protocol_txn_vs_autocommit_stall_is_bounded(server):
    # An open transaction holding an uncommitted write on a row must not wedge
    # autocommit writers on *other* rows for long, and the txn's own commit must
    # land. Guards the statement-write-lock + storage-backoff interaction.
    with connect(server) as c:
        c.execute("CREATE TABLE t (id bigint primary key, n int)")
        c.execute("INSERT INTO t VALUES (1, 0)")
        c.execute("INSERT INTO t VALUES (2, 0)")
    from secantus.sql import pgextended

    counters_at_start = dict(pgextended.COUNTERS)
    outcomes: dict[int, list[str]] = {}
    with connect(server, autocommit=False) as txn:
        txn.execute("UPDATE t SET n = 1 WHERE id = 1")

        def other_row_writer(i: int) -> None:
            log = outcomes.setdefault(i, [])
            with connect(server) as c:
                for _ in range(10):
                    cur = c.execute("UPDATE t SET n = n + 1 WHERE id = 2")
                    # A lost increment on the CI lanes needs to name itself:
                    # record the reported rowcount per iteration so the assert
                    # below shows exactly which statements claimed success.
                    log.append(str(cur.rowcount))

        run_workers(4, other_row_writer)
        txn.commit()
    with connect(server) as c:
        assert c.execute("SELECT n FROM t WHERE id = 1").fetchone() == (1,)
        n = c.execute("SELECT n FROM t WHERE id = 2").fetchone()[0]
        delta = {k: pgextended.COUNTERS[k] - counters_at_start[k] for k in counters_at_start}
        assert n == 40, (
            f"lost increments: n={n}, implicit-txn deltas THIS TEST={delta}, "
            f"per-worker rowcounts={outcomes}"
        )


def test_sync_commit_serializes_with_bare_statements(tmp_path):
    # The proven lost-update mechanism, pinned deterministically: a pipelined
    # implicit transaction's Sync-commit lands *inside* a bare autocommit
    # computed-update's read-compute-write window. Pre-fix, the bare write's
    # fresh WT transaction included that commit in its snapshot, saw no
    # conflict, and overwrote it — a silent lost update. The fix wraps the
    # whole materialized update in one WT snapshot transaction
    # (executor._execute_update_materialized), so the mid-window commit
    # surfaces as a write conflict and the statement retries from a fresh
    # read. Final value must be 2: both increments survive.
    import struct

    from secantus.sql import run_sql
    from secantus.sql.pgextended import ExtendedSession
    from secantus.sql.session import Session

    st = Storage(str(tmp_path))
    try:
        boot = Session(database="d")
        run_sql(st, "d", "CREATE TABLE t (id int primary key, n int)", session=boot)
        run_sql(st, "d", "INSERT INTO t VALUES (2, 0)", session=boot)

        ext = ExtendedSession(st, Session(database="d"))
        parse = b"\x00UPDATE t SET n = n + 1 WHERE id = 2\x00" + struct.pack(">h", 0)
        ext.process("P", parse)
        ext.process("B", b"\x00\x00" + struct.pack(">h", 0) * 3)
        ext.process("E", b"\x00" + struct.pack(">i", 0))  # uncommitted until Sync

        read_done = threading.Event()
        commit_done = threading.Event()
        orig_find = Storage.find_matching
        first_bare_read = threading.Event()

        def gated_find(self, *args, **kwargs):
            out = orig_find(self, *args, **kwargs)
            if (
                args[1] == "t"
                and threading.current_thread().name == "BARE"
                and not first_bare_read.is_set()
            ):
                first_bare_read.set()
                read_done.set()
                commit_done.wait(10)
            return out

        bare_errors: list[BaseException] = []

        def bare() -> None:
            try:
                run_sql(
                    st,
                    "d",
                    "UPDATE t SET n = n + 1 WHERE id = 2",
                    session=Session(database="d"),
                )
            except BaseException as exc:  # noqa: BLE001 — surface into pytest
                bare_errors.append(exc)

        Storage.find_matching = gated_find
        try:
            t = threading.Thread(target=bare, name="BARE")
            t.start()
            assert read_done.wait(10), "bare statement never reached its read"
            ext.process("S", b"")  # implicit-txn commit lands mid-window
            commit_done.set()
            t.join(30)
            assert not t.is_alive(), "bare statement wedged"
        finally:
            Storage.find_matching = orig_find

        assert not bare_errors, f"bare statement errored: {bare_errors!r}"
        rows = run_sql(st, "d", "SELECT n FROM t WHERE id = 2", session=boot)[-1].rows
        assert rows == [(2,)], f"lost update: expected n=2, got rows={rows!r}"
    finally:
        st.close()
