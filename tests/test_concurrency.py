"""Concurrency-scaling diagnostic — known to fail at the WT-level ceiling.

This test was originally written as a Phase-2 exit criterion: ``2
concurrent writers >= 0.7x of one``. The Phase-3 spike
(``tasks/wt-bindings-plan.md`` + ``bench/wt_poc/``) proved that ceiling
is unreachable on WiredTiger: even pure-C pthread writes through
``libwiredtiger`` cap at ~1.3x at N=2 and either flatline or regress at
higher N. The bottleneck is in WT's C library (page locks, log-write
serialisation, eviction lock); no amount of Python-side rebinding lifts
it.

The test is therefore marked ``xfail`` — *not* because the
implementation is broken, but because the assertion encodes a goal the
storage backend cannot deliver. Useful as a regression *detector* if WT
ever ships a higher-concurrency story upstream: when this test
unexpectedly passes, that's news worth investigating.

Marked ``slow`` so the default pytest run skips it (it spawns
subprocesses and runs for ~12s wall-clock). Trigger explicitly:

    uv run python -m pytest -m slow tests/test_concurrency.py

See ``docs/concurrency.md`` for the full architectural ceiling story.
"""

from __future__ import annotations

import contextlib
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
            sys.executable,
            "-m",
            "secantus",
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
            "--storage-path",
            str(storage),
            "--log-level",
            "WARNING",
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
            sys.executable,
            "-m",
            "bench.load_writer",
            "--uri",
            uri,
            "--db",
            "concurrency_test",
            "--collection",
            f"w{i}",
            "--batch-size",
            str(batch),
            "--progress-every",
            "0",
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
        with contextlib.suppress(ProcessLookupError):
            p.send_signal(signal.SIGTERM)
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
@pytest.mark.xfail(
    reason="WT-level concurrency ceiling: pure-C pthread writes also cap "
    "at ~1.3x at N=2. See bench/wt_poc/ + docs/concurrency.md.",
    strict=False,
)
def test_two_writers_scale_above_single_writer(tmp_path) -> None:
    """N=2 aggregate throughput >= 0.7 x N=1 single-writer throughput.

    Encoded the original Phase-2 exit criterion. Now expected to fail
    until WiredTiger ships a higher-concurrency story upstream — see
    ``docs/concurrency.md`` for the architectural ceiling and
    ``tasks/wt-bindings-plan.md`` for the spike that proved it. If
    this test ever unexpectedly passes, ``strict=False`` xfail won't
    fail the suite, but the surprise is worth investigating.
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
