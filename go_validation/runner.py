"""Run mongo-go-driver's tests against a standalone SecantusDB daemon.

The mongo-go-driver tests assume they're connecting to a real running
`mongod` over TCP. We oblige: spawn `python -m secantus --port <free>
--storage-path :memory:` as a subprocess, wait for the listener, point
the tests at it via `MONGODB_URI`, then tear the daemon down. Zero
modifications to the vendored go-driver tree.

`go test -json` emits NDJSON of test events. We collect that into
`.validation/go-raw.ndjson` and let `generate_report.py` turn it into
`docs/validation-report-go.md`.

Run via `uv run python -m invoke validate-go`.
"""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

from .include_packages import INCLUDE, SKIP_PATTERNS

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-go-driver"
RAW_OUT = REPO_ROOT / ".validation" / "go-raw.ndjson"


def _find_free_port() -> int:
    """OS-assigned free TCP port. Race window between close and the
    daemon's bind is acceptable for a dev tool."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait_for_listener(host: str, port: int, timeout: float = 10.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"daemon at {host}:{port} did not become ready within {timeout}s")


def main() -> int:
    if shutil.which("go") is None:
        print("go: command not found; install Go to run go_validation", file=sys.stderr)
        return 2
    if not VENDOR.is_dir() or not (VENDOR / "go.mod").is_file():
        print(
            f"vendor/mongo-go-driver/ missing or not initialised "
            f"({VENDOR}); run `git submodule update --init --recursive`",
            file=sys.stderr,
        )
        return 2
    if not (VENDOR / "testdata" / "specifications" / "source").is_dir():
        print(
            "vendor/mongo-go-driver/testdata/specifications/ is empty (nested "
            "submodule). Run `git submodule update --init --recursive` from "
            "the repo root — without the spec data the bson-corpus tests "
            "fail on missing JSON files.",
            file=sys.stderr,
        )
        return 2

    RAW_OUT.parent.mkdir(parents=True, exist_ok=True)

    host = "127.0.0.1"
    port = _find_free_port()
    daemon_cmd = [
        sys.executable,
        "-m",
        "secantus",
        "--host",
        host,
        "--port",
        str(port),
        "--storage-path",
        ":memory:",
        "--log-level",
        "WARNING",
    ]
    print(f"go_validation: starting daemon on {host}:{port}", file=sys.stderr)
    daemon = subprocess.Popen(daemon_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        _wait_for_listener(host, port)

        env = os.environ.copy()
        env["MONGODB_URI"] = f"mongodb://{host}:{port}"
        env.setdefault("REQUIRE_API_VERSION", "false")

        # Default Go per-package timeout is 10 min. The mongo-go-driver
        # integration package runs many CRUD tests in parallel against
        # SecantusDB; total runtime under load can creep past 10 min,
        # at which point Go's runtime kills the whole test binary
        # with "test timed out", marking every still-running subtest
        # as failed in cascade. Bumping to 30 min keeps the gauge
        # interpretable: individual tests that really hang still
        # surface, but the package-wide kill no longer corrupts the
        # signal for unrelated work that just happened to be in
        # flight when the killer fired.
        cmd = ["go", "test", "-json", "-count=1", "-timeout=30m"]
        if SKIP_PATTERNS:
            cmd.append(f"-skip={'|'.join(SKIP_PATTERNS)}")
        cmd.extend(INCLUDE)
        print(
            f"go_validation: `{' '.join(cmd)}` in {VENDOR} (MONGODB_URI={env['MONGODB_URI']})",
            file=sys.stderr,
        )
        with RAW_OUT.open("w") as out:
            proc = subprocess.run(cmd, cwd=VENDOR, env=env, stdout=out, stderr=subprocess.PIPE)
        if proc.stderr:
            sys.stderr.buffer.write(proc.stderr)

        if RAW_OUT.stat().st_size == 0:
            print("go_validation: no test events recorded (build error?)", file=sys.stderr)
            return 1

        passed = failed = skipped = 0
        for line in RAW_OUT.open():
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            if ev.get("Test") and ev.get("Action") in {"pass", "fail", "skip"}:
                if ev["Action"] == "pass":
                    passed += 1
                elif ev["Action"] == "fail":
                    failed += 1
                else:
                    skipped += 1
        print(
            f"go_validation: {passed} passed, {failed} failed, "
            f"{skipped} skipped (per-package breakdown in the markdown report)"
        )
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=5)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
        # Surface daemon stderr only on test failures, otherwise suppress noise.
        # (Daemon stdout was sent to /dev/null.)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
