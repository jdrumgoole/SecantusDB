"""Run mongo-rust-driver's test suite against a SecantusDB daemon.

End-to-end integration gauge: SecantusDB and the Rust driver
exchange real wire commands over TCP. The runner:

1. Picks a fresh kernel-assigned ephemeral port and spawns
   ``python -m secantus --host 127.0.0.1 --port <picked>
   --storage-path <tempdir>`` as a subprocess. Random port lets
   multiple gauges run in parallel under ``validate-all``.
2. Waits for the listener to come up.
3. Runs ``cargo test --lib -p mongodb <filters>`` with
   ``MONGODB_URI=mongodb://127.0.0.1:<picked>/`` explicitly set in
   the subprocess env so the user's ambient ``MONGODB_URI`` (which
   might point at a real mongod) can't leak through. The Rust
   driver's fallback chain — ``$MONGODB_URI`` → ``~/.mongodb_uri``
   → ``localhost:27017`` — is short-circuited at the first step
   when we set the env explicitly.
4. Parses the cargo test output and writes JSON to
   ``.validation/rust-raw.json``.
5. ``generate_report.py`` renders the per-module breakdown into
   ``docs/validation-report-rust.md``.

First-time cargo build is ~1-2 minutes for the driver + test deps;
subsequent runs reuse the ``target/`` directory and complete in
seconds for the curated include set.

Run via ``uv run python -m invoke validate-rust``. Requires rustc /
cargo on ``PATH`` (``brew install rust`` on macOS; ``rustup`` on
linux).
"""

from __future__ import annotations

import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from .include_paths import CARGO_FEATURES, INCLUDE

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-rust-driver"
RAW_OUT = REPO_ROOT / ".validation" / "rust-raw.json"

# Hard wall-clock limit on the cargo test invocation. The Rust
# driver's tests are async and rely on tokio timeouts internally;
# 600s is generous headroom for the first-cut include set
# (~14 tests) and grows comfortably as the set widens.
CARGO_TEST_TIMEOUT_SECONDS = 600.0


def _free_port() -> int:
    """Pick an unused TCP port. Lets ``validate-all`` run multiple
    gauges in parallel without port collisions."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait_for_listener(port: int, timeout_s: float = 10.0) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.settimeout(0.5)
            try:
                s.connect(("127.0.0.1", port))
                return
            except OSError:
                time.sleep(0.05)
    raise RuntimeError(f"SecantusDB daemon never bound 127.0.0.1:{port}")


def _verify_secantus(uri: str) -> None:
    """Confirm the daemon at ``uri`` is SecantusDB, not a real mongod
    that's somehow squatting the port. Belt-and-braces against the
    user's ``MONGODB_URI`` leaking through; we already override the
    env, but a hardcoded ``localhost:27017`` in someone's config
    couldn't hurt to double-check."""
    import pymongo

    client = pymongo.MongoClient(uri, serverSelectionTimeoutMS=2000)
    try:
        info = client["admin"].command("hello")
    finally:
        client.close()
    set_name = info.get("setName", "")
    if set_name != "secantus":
        raise RuntimeError(
            f"daemon at {uri} is not SecantusDB (hello.setName={set_name!r}, "
            f"expected 'secantus'). Aborting to avoid running the rust gauge "
            f"against a real mongod."
        )


def main() -> int:
    if not VENDOR.exists():
        print(
            f"rust_validation: vendor/mongo-rust-driver missing — "
            f"run ``git submodule update --init {VENDOR.relative_to(REPO_ROOT)}``",
            file=sys.stderr,
        )
        return 2

    if shutil.which("cargo") is None or shutil.which("rustc") is None:
        print(
            "rust_validation: cargo / rustc not on PATH — install Rust "
            "(`brew install rust` on macOS, `rustup` on linux)",
            file=sys.stderr,
        )
        return 2

    RAW_OUT.parent.mkdir(exist_ok=True)
    port = _free_port()
    storage = Path(tempfile.mkdtemp(prefix="secantus-rust-gauge-"))
    uri = f"mongodb://127.0.0.1:{port}/"

    daemon_cmd = [
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
    ]
    print(f"rust_validation: launching SecantusDB → {uri}", file=sys.stderr)
    daemon = subprocess.Popen(daemon_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        _wait_for_listener(port)
        _verify_secantus(uri)
        rc, raw = _run_cargo_tests(uri)
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=5)
        except subprocess.TimeoutExpired:
            daemon.kill()
        shutil.rmtree(storage, ignore_errors=True)

    parsed = _parse_cargo_output(raw)
    RAW_OUT.write_text(json.dumps(parsed, indent=2))
    print(
        f"rust_validation: wrote {RAW_OUT.relative_to(REPO_ROOT)} "
        f"({parsed['summary']['passed']} passed, "
        f"{parsed['summary']['failed']} failed, "
        f"{parsed['summary']['ignored']} ignored)",
        file=sys.stderr,
    )
    # Exit 0 even on test failures — the report tells the story; rc
    # propagates to the caller via the generated report's failed
    # count.
    return 0


def _run_cargo_tests(uri: str) -> tuple[int, str]:
    """One cargo invocation per filter.

    Cargo's CLI rejects multiple positional filters; libtest's
    multi-filter mode treats them as an AND substring match (the
    intersection), not the union we want. Per-filter invocations are
    the simplest way to express "run any of these test names." The
    cargo ``target/`` directory is reused across calls, so the
    compile cost is paid once.

    Returns the aggregated stdout from every invocation and a worst-
    case return code (any non-zero from any sub-run wins, since a
    single failing test is enough to mark the gauge non-green; we
    still continue past failures to collect the full picture).
    """
    env = {**os.environ, "MONGODB_URI": uri}
    parts: list[str] = []
    worst_rc = 0
    print(
        f"rust_validation: running cargo test ({len(INCLUDE)} per-filter invocations) in {VENDOR}",
        file=sys.stderr,
    )
    for idx, filt in enumerate(INCLUDE, 1):
        cmd = ["cargo", "test", "--lib", "-p", "mongodb"]
        if CARGO_FEATURES:
            cmd.extend(["--features", ",".join(CARGO_FEATURES)])
        cmd.append(filt)
        cmd.extend(["--", "--test-threads=1", "--format=pretty"])
        try:
            proc = subprocess.run(
                cmd,
                cwd=VENDOR,
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                timeout=CARGO_TEST_TIMEOUT_SECONDS,
            )
        except subprocess.TimeoutExpired as exc:
            parts.append(
                f"\n=== rust_validation: filter {idx}/{len(INCLUDE)} {filt!r} "
                f"TIMEOUT after {CARGO_TEST_TIMEOUT_SECONDS}s ===\n"
                f"{(exc.stdout or b'').decode(errors='replace')}\n"
            )
            worst_rc = max(worst_rc, 124)
            continue
        parts.append(
            f"\n=== rust_validation: filter {idx}/{len(INCLUDE)} {filt!r} "
            f"(rc={proc.returncode}) ===\n{proc.stdout}"
        )
        if proc.returncode != 0:
            worst_rc = max(worst_rc, proc.returncode)
    return worst_rc, "".join(parts)


_TEST_LINE = re.compile(r"^test (\S+) \.\.\. (ok|FAILED|ignored)\b")


def _parse_cargo_output(raw: str) -> dict:
    """Walk cargo test's plain-text output and extract per-test
    pass/fail/ignored outcomes plus the failure-line annotations.

    Cargo's text output is line-oriented; the per-test result lines
    look like ``test test::client::list_databases ... ok`` or
    ``... FAILED``. We don't use ``cargo test -- --format json``
    because the JSON format is unstable nightly-only.
    """
    tests: list[dict] = []
    failures: list[dict] = []
    summary = {"passed": 0, "failed": 0, "ignored": 0}
    current_failure: dict | None = None
    in_failure_block = False
    for line in raw.splitlines():
        m = _TEST_LINE.match(line)
        if m:
            name, outcome = m.group(1), m.group(2)
            outcome_norm = {
                "ok": "passed",
                "FAILED": "failed",
                "ignored": "ignored",
            }[outcome]
            tests.append({"name": name, "outcome": outcome_norm})
            summary[outcome_norm] += 1
            continue
        # Cargo prints a "---- <test_name> stdout ----" header for
        # each failed test followed by the panic message; capture the
        # first non-empty line after the header as the failure
        # summary.
        if line.startswith("---- ") and line.endswith(" stdout ----"):
            in_failure_block = True
            name = line[len("---- ") : -len(" stdout ----")]
            current_failure = {"name": name, "message": ""}
            failures.append(current_failure)
            continue
        if in_failure_block and current_failure is not None:
            stripped = line.strip()
            if not stripped:
                continue
            # First non-empty body line is the panic / assertion
            # message; subsequent lines (backtrace etc.) are skipped
            # for the report.
            if not current_failure["message"]:
                current_failure["message"] = stripped
                in_failure_block = False
                current_failure = None
    return {"summary": summary, "tests": tests, "failures": failures}


if __name__ == "__main__":
    sys.exit(main())
