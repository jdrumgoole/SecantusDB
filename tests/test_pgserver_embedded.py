"""The Rust PostgreSQL server, started in-process from Python.

The project's promise is that starting a server in a test is one or two lines
with no external processes to manage. This is that surface for the PG server:
``PgServer(path)`` binds a socket, ``.dsn`` is handed straight to psycopg, and
``.stop()`` closes the store.

The headline assertion is the durability one. ``stop()`` is where WiredTiger's
close-checkpoint runs, and it runs only because the handle OWNS the store
(``secantus_pgserver::bind`` takes ``Storage`` by value). Get that ownership
wrong and the checkpoint quietly does not happen: measured 2026-08-31, a
``CREATE TABLE`` + ``INSERT`` the client had been told succeeded was gone
afterwards. So the test writes over the real wire, stops, reopens the same home
in a NEW server, and reads the rows back.

Skipped unless the ``_secantus_server`` extension is built AND was built with
the (default-on) ``pgserver`` cargo feature:

    ./inv rust-server-build
"""

from __future__ import annotations

import contextlib
import time
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

psycopg = pytest.importorskip("psycopg")
_secantus_server = pytest.importorskip("_secantus_server")

pytestmark = pytest.mark.skipif(
    not getattr(_secantus_server, "HAS_PGSERVER", False),
    reason="_secantus_server built without the `pgserver` cargo feature",
)


@contextlib.contextmanager
def _server(home: Path, **kwargs: Any) -> Iterator[Any]:
    """A `PgServer` over `home`, always stopped again.

    Port 0 throughout: the kernel names the port and the handle reports it back.
    Probing for a free port and passing it in cannot be made race-free under
    ``-n auto`` -- the probe socket must close before the bind, and another
    worker can take it in that window.
    """
    server = _secantus_server.PgServer(str(home), **kwargs)
    try:
        yield server
    finally:
        server.stop()


def _connect(server: Any, *, dbname: str | None = None) -> psycopg.Connection:
    dsn = server.dsn
    if dbname is not None:
        dsn = f"host=127.0.0.1 port={server.port} dbname={dbname} user=postgres"
    return psycopg.connect(dsn, autocommit=True, connect_timeout=10)


def test_binds_an_ephemeral_port_and_reports_it(tmp_path: Path) -> None:
    with _server(tmp_path / "home") as server:
        host, port = server.address
        assert host == "127.0.0.1"
        assert port != 0, "kernel-assigned port not reported back"
        assert server.port == port
        assert server.dsn == f"host=127.0.0.1 port={port} dbname=postgres user=postgres"
        # Same version line the Mongo-side handle reports; the crates move in
        # lockstep, so a mismatch would mean two builds got mixed.
        assert server.version == _secantus_server.__version__


def test_serves_psycopg_in_process(tmp_path: Path) -> None:
    with _server(tmp_path / "home") as server, _connect(server) as conn:
        conn.execute("CREATE TABLE widgets (id int, name text)")
        conn.execute("INSERT INTO widgets VALUES (1, 'left'), (2, 'right')")
        rows = conn.execute("SELECT id, name FROM widgets ORDER BY id").fetchall()
    assert rows == [(1, "left"), (2, "right")]


def test_acknowledged_writes_survive_stop_and_reopen(tmp_path: Path) -> None:
    """The durability contract: `stop()` checkpoints, so a reopen sees the rows.

    Two things have to survive, and losing either is silent data loss because
    the client was told both writes succeeded: the catalog entry for the table,
    and its rows. The reopen also proves the store was RELEASED -- WiredTiger
    refuses a second open on a home this process still holds.
    """
    home = tmp_path / "home"
    with _server(home) as server, _connect(server) as conn:
        conn.execute("CREATE TABLE survivors (n int)")
        conn.execute("INSERT INTO survivors VALUES (1), (2), (3)")

    with _server(home) as reopened, _connect(reopened) as conn:
        rows = conn.execute("SELECT n FROM survivors ORDER BY n").fetchall()
    assert rows == [(1,), (2,), (3,)], "rows acknowledged before stop() did not survive"


def test_context_manager_stops_the_server(tmp_path: Path) -> None:
    home = tmp_path / "home"
    with _secantus_server.PgServer(str(home)) as server:
        port = server.port
        with _connect(server) as conn:
            conn.execute("CREATE TABLE cm (n int)")
            conn.execute("INSERT INTO cm VALUES (7)")

    # Stopped: nothing is listening on that port any more.
    with pytest.raises(psycopg.OperationalError):
        psycopg.connect(
            f"host=127.0.0.1 port={port} dbname=postgres user=postgres",
            connect_timeout=5,
        ).close()

    # And __exit__ checkpointed, exactly as an explicit stop() would.
    with _server(home) as reopened, _connect(reopened) as conn:
        assert conn.execute("SELECT n FROM cm").fetchall() == [(7,)]


def test_context_manager_stops_the_server_on_an_exception(tmp_path: Path) -> None:
    """`__exit__` must not swallow the exception -- and must still stop."""
    home = tmp_path / "home"
    with pytest.raises(ZeroDivisionError), _secantus_server.PgServer(str(home)) as server:
        with _connect(server) as conn:
            conn.execute("CREATE TABLE boom (n int)")
            conn.execute("INSERT INTO boom VALUES (1)")
        raise ZeroDivisionError("boom")

    # The store was released and checkpointed on the way out.
    with _server(home) as reopened, _connect(reopened) as conn:
        assert conn.execute("SELECT n FROM boom").fetchall() == [(1,)]


def test_stop_does_not_wait_on_an_open_connection(tmp_path: Path) -> None:
    """Leaving a connection open must not make `stop()` crawl.

    A pooled PostgreSQL connection never hangs up on its own -- a real backend
    does not close an idle one -- so `stop()` has to TELL its connections to
    finish rather than wait for them. Before it did, an open connection made
    every stop block for the whole drain timeout: measured at 10.8s here, which
    is not a one-or-two-line ergonomic, and forgetting to close a connection is
    the common case in a test.
    """
    home = tmp_path / "home"
    server = _secantus_server.PgServer(str(home))
    conn = _connect(server)  # deliberately left OPEN across the stop
    conn.execute("CREATE TABLE held (n int)")
    conn.execute("INSERT INTO held VALUES (1)")

    started = time.monotonic()
    server.stop()
    elapsed = time.monotonic() - started
    conn.close()
    assert elapsed < 3.0, f"stop() waited {elapsed:.1f}s on an open connection"

    # And it still checkpointed on the way out.
    with _server(home) as reopened, _connect(reopened) as check:
        assert check.execute("SELECT n FROM held").fetchall() == [(1,)]


def test_stop_is_idempotent(tmp_path: Path) -> None:
    server = _secantus_server.PgServer(str(tmp_path / "home"))
    server.stop()
    server.stop()
    server.stop()
    # The handle is dead but still safe to interrogate and to drop.
    assert server.port != 0
    del server


def test_extra_databases_are_connectable(tmp_path: Path) -> None:
    """`databases=` pre-registers names, as `secantusd-pg --database` does."""
    with (
        _server(tmp_path / "home", databases=["analytics"]) as server,
        _connect(server, dbname="analytics") as conn,
    ):
        conn.execute("CREATE TABLE only_here (n int)")
        conn.execute("INSERT INTO only_here VALUES (42)")
        assert conn.execute("SELECT n FROM only_here").fetchall() == [(42,)]


def test_two_embedded_servers_coexist(tmp_path: Path) -> None:
    """Separate homes, separate ports -- the ephemeral-port form has to hold."""
    with _server(tmp_path / "a") as one, _server(tmp_path / "b") as two:
        assert one.port != two.port
        with _connect(one) as conn:
            conn.execute("CREATE TABLE t (n int)")
            conn.execute("INSERT INTO t VALUES (1)")
        with _connect(two) as conn:
            conn.execute("CREATE TABLE t (n int)")
            conn.execute("INSERT INTO t VALUES (2)")
        with _connect(one) as conn:
            assert conn.execute("SELECT n FROM t").fetchall() == [(1,)]
        with _connect(two) as conn:
            assert conn.execute("SELECT n FROM t").fetchall() == [(2,)]
