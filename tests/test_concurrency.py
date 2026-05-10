"""Concurrency-scaling regression test (Phase 0 of the WT concurrency plan).

This test is **intentionally red on `main`**. It encodes the Phase 2
exit criterion from ``tasks/wt-concurrency-plan.md``: 2 concurrent
writers must deliver at least 0.7x the throughput of a single writer
on the same hardware. Today's measured ratio is around 0.35x — the
global ``Storage._lock`` collapses every write to one in-flight
operation, then adds context-switch tax on top.

The test is marked ``slow`` so the default pytest run skips it (it
spawns subprocesses and runs for ~12s wall-clock). Trigger it
explicitly:

    uv run python -m pytest -m slow tests/test_concurrency.py

When Phase 2 lands (decomposed locks, schema-snapshot pattern,
WT-MVCC for the data path), this test should turn green.
"""

from __future__ import annotations

import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import pytest

from bench.concurrency import _parse_writer_log, _wait_listen


# Single-writer floor we expect per-writer when scaling is healthy.
# A writer that gets shut out completely by lock contention will
# fall well below this; a writer that has any concurrency at all
# will clear it comfortably.
_MIN_SCALING_RATIO = 0.7

# Per-run wall-clock duration — long enough to amortise spin-up,
# short enough that the test fits inside a normal CI budget.
_DURATION_S = 5.0


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _spawn_server(port: int, storage: Path) -> subprocess.Popen[bytes]:
    return subprocess.Popen(
        [
            sys.executable, "-m", "secantus",
            "--host", "127.0.0.1",
            "--port", str(port),
            "--storage-path", str(storage),
            "--log-level", "WARNING",
        ],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def _spawn_writers(uri: str, n: int, batch: int) -> list[tuple[subprocess.Popen[bytes], Path]]:
    procs: list[tuple[subprocess.Popen[bytes], Path]] = []
    for i in range(n):
        log_path = Path(tempfile.mkstemp(prefix=f"writer-{i}-", suffix=".log")[1])
        log_f = log_path.open("w")
        argv = [
            sys.executable, "-m", "bench.load_writer",
            "--uri", uri,
            "--db", "concurrency_test",
            "--collection", f"w{i}",
            "--batch-size", str(batch),
            "--progress-every", "0",
        ]
        if i == 0:
            argv.append("--drop")
        p = subprocess.Popen(
            argv,
            stdin=subprocess.DEVNULL,
            stdout=log_f,
            stderr=subprocess.STDOUT,
        )
        procs.append((p, log_path))
    return procs


def _drain_writers(
    procs: list[tuple[subprocess.Popen[bytes], Path]],
) -> list[tuple[int, int] | None]:
    for p, _ in procs:
        try:
            p.send_signal(signal.SIGTERM)
        except ProcessLookupError:
            pass
    stats: list[tuple[int, int] | None] = []
    for p, log_path in procs:
        try:
            p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait()
        stats.append(_parse_writer_log(log_path.read_text()))
        log_path.unlink(missing_ok=True)
    return stats


def _measure(uri: str, n: int) -> float:
    """Run ``n`` writers for ``_DURATION_S`` and return aggregate docs/s."""
    procs = _spawn_writers(uri, n, batch=100)
    t0 = time.monotonic()
    time.sleep(_DURATION_S)
    elapsed = time.monotonic() - t0
    stats = _drain_writers(procs)
    if any(s is None for s in stats):
        pytest.fail(
            f"writer subprocess produced no parseable summary "
            f"(stats={stats}); benchmark harness is broken"
        )
    total = sum(s[1] for s in stats if s is not None)
    return total / elapsed if elapsed > 0 else 0.0


@pytest.mark.slow
def test_two_writers_scale_above_single_writer(tmp_path) -> None:
    """N=2 aggregate throughput >= 0.7 x N=1 single-writer throughput.

    See ``tasks/wt-concurrency-plan.md`` for context. This test is
    intentionally failing on `main` and the success criterion for
    Phase 2 of the WT concurrency work.
    """
    storage = tmp_path / "wt"
    port = _free_port()
    server = _spawn_server(port, storage)
    try:
        assert _wait_listen("127.0.0.1", port, timeout=30), "server didn't come up"
        uri = f"mongodb://127.0.0.1:{port}/"

        rate_1 = _measure(uri, n=1)
        assert rate_1 > 0, "single-writer baseline measured zero throughput"

        rate_2 = _measure(uri, n=2)

        ratio = rate_2 / rate_1
        # Useful in CI logs even when the assertion passes.
        print(
            f"\nconcurrency scaling: 1-writer={rate_1:,.0f} docs/s, "
            f"2-writer={rate_2:,.0f} docs/s, ratio={ratio:.2f}x"
        )
        assert ratio >= _MIN_SCALING_RATIO, (
            f"2-writer aggregate throughput is {ratio:.2f}x the 1-writer "
            f"baseline (need >= {_MIN_SCALING_RATIO}x). Storage._lock "
            f"contention is preventing real concurrency. See "
            f"tasks/wt-concurrency-plan.md."
        )
    finally:
        server.terminate()
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait()
        if storage.exists():
            shutil.rmtree(storage, ignore_errors=True)
