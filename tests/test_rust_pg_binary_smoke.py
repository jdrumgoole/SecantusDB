"""Smoke test for the standalone ``secantusd-pg`` Rust PostgreSQL-wire binary.

Launches the compiled binary on an ephemeral port, reads the bound address from
its readiness line, drives a psycopg connect + DDL/DML/query round-trip over
real TCP, then asks it to stop and asserts a clean exit. The PostgreSQL-side
twin of ``tests/test_rust_binary_smoke.py``, and what the release workflow runs
against the exact artifact it is about to publish.

Skipped unless the binary exists: build it with
``cargo build --manifest-path crates/secantus-pgserver/Cargo.toml`` (WiredTiger
required — set SECANTUS_WT_INCLUDE / SECANTUS_WT_LIB), or point
``SECANTUSD_PG_BIN`` at a prebuilt one.
"""

from __future__ import annotations

import os
import pathlib
import re
import signal
import subprocess
import sys
import threading

import pytest

psycopg = pytest.importorskip("psycopg")

_REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
# The server prints the address it actually BOUND, which is what makes
# `--port 0` usable: the kernel picks the port and this line is how the caller
# learns it. Probing for a free port and passing it in cannot be made race-free.
_BANNER = re.compile(r"secantusd-pg listening on (\S+):(\d+)")


def _binary_path() -> pathlib.Path | None:
    env = os.environ.get("SECANTUSD_PG_BIN")
    if env:
        p = pathlib.Path(env)
        if not p.exists() and sys.platform == "win32" and not p.suffix:
            p = p.with_suffix(".exe")
        if not p.exists():
            # Deliberately fatal, not a skip -- the same rule the mongo binary
            # smoke test learned the hard way. Setting this variable means
            # "smoke THIS artifact"; it is how the release workflow points the
            # suite at the binary it is about to publish. Degrading to a skip
            # there lets a release ship a binary that was never exercised, with
            # the step still green.
            raise RuntimeError(
                f"SECANTUSD_PG_BIN={env!r} does not exist"
                + (f" (nor {p})" if str(p) != env else "")
                + ". Point it at a built secantusd-pg binary, or unset it to let "
                "the suite discover one under crates/secantus-pgserver/target/."
            )
        return p
    for profile in ("release", "debug"):
        p = _REPO_ROOT / "crates" / "secantus-pgserver" / "target" / profile / "secantusd-pg"
        if sys.platform == "win32":
            p = p.with_suffix(".exe")
        if p.exists():
            return p
    return None


_BIN = _binary_path()
pytestmark = pytest.mark.skipif(
    _BIN is None,
    reason="secantusd-pg not built (cargo build --manifest-path "
    "crates/secantus-pgserver/Cargo.toml, or set SECANTUSD_PG_BIN)",
)

_WINDOWS = sys.platform == "win32"
_SPAWN_KWARGS: dict[str, object] = (
    {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if _WINDOWS else {}
)


def _request_shutdown(proc: subprocess.Popen[str]) -> None:
    proc.send_signal(signal.CTRL_BREAK_EVENT if _WINDOWS else signal.SIGTERM)


@pytest.fixture
def daemon(tmp_path: pathlib.Path):
    assert _BIN is not None
    proc = subprocess.Popen(
        [str(_BIN), str(tmp_path / "data"), "127.0.0.1:0"],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        **_SPAWN_KWARGS,
    )
    try:
        yield proc
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=10)


def _bound_address(proc: subprocess.Popen[str]) -> tuple[str, int]:
    """The address from the readiness line, with a deadline.

    A bare `readline()` would hang forever if the server died before printing,
    which in CI shows up as a job that burns its whole timeout rather than a
    test that fails.
    """
    line: list[str] = []
    reader = threading.Thread(
        target=lambda: line.append(proc.stdout.readline() if proc.stdout else ""),
        daemon=True,
    )
    reader.start()
    reader.join(60)
    first = line[0] if line else ""
    match = _BANNER.search(first)
    assert match, f"no listening banner in first stdout line: {first!r}"
    return match.group(1), int(match.group(2))


def test_binary_reports_its_version() -> None:
    """`--version` must answer, not try to open a database called `--version`.

    It used to fall through to the positional storage-path argument, so the
    server attempted a WiredTiger open in a directory of that name and reported
    `WT_TRY_SALVAGE: database corruption detected`. The release workflow
    sanity-checks the artifact with this flag.
    """
    assert _BIN is not None
    out = subprocess.run([str(_BIN), "--version"], capture_output=True, text=True, timeout=60)
    assert out.returncode == 0, out.stderr
    assert out.stdout.startswith("secantusd-pg "), out.stdout


def test_binary_serves_psycopg_and_exits_cleanly(daemon: subprocess.Popen[str]) -> None:
    host, port = _bound_address(daemon)
    conn = psycopg.connect(
        f"host={host} port={port} dbname=postgres user=smoke",
        autocommit=True,
        connect_timeout=30,
    )
    try:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, name text, n int)")
        cur.execute("INSERT INTO t VALUES (1, 'a', 10), (2, 'b', 20), (3, 'c', 30)")

        cur.execute("SELECT id, name, n FROM t ORDER BY id")
        assert cur.fetchall() == [(1, "a", 10), (2, "b", 20), (3, "c", 30)]

        cur.execute("SELECT count(*) FROM t WHERE n > %s", (15,))
        assert cur.fetchone() == (2,)

        cur.execute("UPDATE t SET n = n + 5 WHERE id = 1")
        cur.execute("SELECT n FROM t WHERE id = 1")
        assert cur.fetchone() == (15,)

        cur.execute("DELETE FROM t WHERE id = 3")
        cur.execute("SELECT count(*) FROM t")
        assert cur.fetchone() == (2,)
    finally:
        conn.close()

    _request_shutdown(daemon)
    assert daemon.wait(timeout=60) == 0, "the daemon did not stop cleanly"


def test_data_survives_a_restart(tmp_path: pathlib.Path) -> None:
    """The storage path is a real WiredTiger home, not a scratch buffer.

    A binary that serves correctly but loses everything on restart would pass
    the round-trip above, which is the whole reason this one exists.
    """
    assert _BIN is not None
    home = tmp_path / "data"

    def run_once(sql: str) -> list[tuple]:
        proc = subprocess.Popen(
            [str(_BIN), str(home), "127.0.0.1:0"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            **_SPAWN_KWARGS,
        )
        try:
            host, port = _bound_address(proc)
            with psycopg.connect(
                f"host={host} port={port} dbname=postgres user=smoke",
                autocommit=True,
                connect_timeout=30,
            ) as conn:
                cur = conn.cursor()
                cur.execute(sql)
                return cur.fetchall() if cur.description else []
        finally:
            _request_shutdown(proc)
            try:
                proc.wait(timeout=60)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)

    run_once("CREATE TABLE persisted (id int PRIMARY KEY, v text)")
    run_once("INSERT INTO persisted VALUES (1, 'kept')")
    assert run_once("SELECT id, v FROM persisted") == [(1, "kept")]
