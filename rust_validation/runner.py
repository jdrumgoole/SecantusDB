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

import gauge_common

from .include_paths import CARGO_FEATURES, INCLUDE

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-rust-driver"
RAW_OUT = REPO_ROOT / ".validation" / f"rust-raw{gauge_common.report_suffix()}.json"

# Hard wall-clock limit on each cargo test invocation. The Rust
# driver's tests are async and rely on tokio timeouts internally;
# 600s is generous headroom for the first-cut include set
# (~14 tests) and grows comfortably as the set widens. The compile
# is paid ONCE up front (`cargo test --no-run`, below) under its own
# budget so this limit only ever bounds test runtime.
CARGO_TEST_TIMEOUT_SECONDS = 600.0

# Wall for the one-off test-binary build. A cold cargo cache on a
# 2-core CI runner takes tens of minutes for the driver + dev-deps;
# a warm target/ makes this a no-op.
CARGO_BUILD_TIMEOUT_SECONDS = 3600.0


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
    storage = Path(tempfile.mkdtemp(prefix="secantus-rust-gauge-"))

    daemon_cmd = [
        sys.executable,
        "-m",
        "secantus",
        "--host",
        "127.0.0.1",
        "--port",
        "0",
        "--storage-path",
        str(storage),
        "--log-level",
        "WARNING",
    ]
    # Race-free spawn on a kernel-assigned port (see gauge_common.spawn_daemon).
    daemon, host, port = gauge_common.spawn_daemon(daemon_cmd, label="rust_validation")
    uri = f"mongodb://{host}:{port}/"
    print(
        f"rust_validation: launched {gauge_common.gauge_server()} SecantusDB → {uri}",
        file=sys.stderr,
    )
    try:
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
    # Compile the test binary BEFORE the per-filter loop, outside the 600s
    # per-invocation budget. On a cold cargo cache the driver's test build
    # alone can exceed 600s (2-core CI runner), and letting the first filter's
    # timeout kill cargo mid-compile forces every later invocation to resume
    # a partial build inside its own 600s window — the observed failure mode
    # was the weekly CI job crawling to the 6-hour kill (7 min with a warm
    # cache). The build gets its own generous wall; the per-filter timeout
    # then bounds only test RUNTIME, which is what it was meant to bound.
    build_cmd = ["cargo", "test", "--lib", "-p", "mongodb", "--no-run"]
    if CARGO_FEATURES:
        build_cmd.extend(["--features", ",".join(CARGO_FEATURES)])
    print(
        f"rust_validation: pre-building the driver test binary in {VENDOR}",
        file=sys.stderr,
    )
    build = subprocess.run(
        build_cmd,
        cwd=VENDOR,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=CARGO_BUILD_TIMEOUT_SECONDS,
    )
    if build.returncode != 0:
        # A build failure would fail every invocation identically — surface
        # it once, loudly, instead of 88 repeated compile errors.
        return build.returncode, (
            f"\n=== rust_validation: cargo test --no-run failed "
            f"(rc={build.returncode}) ===\n{build.stdout}"
        )
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
        # Per-filter progress goes to stderr AS IT HAPPENS. The captured
        # stdout is only assembled into the raw artifact at the END of the
        # loop, so when a CI job is killed mid-gauge the log otherwise
        # records nothing about which filters ran, hung, or how long each
        # took — exactly the blind spot that made the 2026-07/08 weekly
        # wedge (filters hanging to their 600s timeout on the CI runner,
        # against the Python server only) undiagnosable from its logs.
        print(
            f"rust_validation: [{idx}/{len(INCLUDE)}] {filt} ...",
            file=sys.stderr,
            flush=True,
        )
        t0 = time.monotonic()
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
            print(
                f"rust_validation: [{idx}/{len(INCLUDE)}] {filt} TIMEOUT "
                f"after {CARGO_TEST_TIMEOUT_SECONDS:.0f}s",
                file=sys.stderr,
                flush=True,
            )
            parts.append(
                f"\n=== rust_validation: filter {idx}/{len(INCLUDE)} {filt!r} "
                f"TIMEOUT after {CARGO_TEST_TIMEOUT_SECONDS}s ===\n"
                f"{(exc.stdout or b'').decode(errors='replace')}\n"
            )
            worst_rc = max(worst_rc, 124)
            continue
        print(
            f"rust_validation: [{idx}/{len(INCLUDE)}] {filt} rc={proc.returncode} "
            f"in {time.monotonic() - t0:.1f}s",
            file=sys.stderr,
            flush=True,
        )
        parts.append(
            f"\n=== rust_validation: filter {idx}/{len(INCLUDE)} {filt!r} "
            f"(rc={proc.returncode}) ===\n{proc.stdout}"
        )
        if proc.returncode != 0:
            worst_rc = max(worst_rc, proc.returncode)
    return worst_rc, "".join(parts)


_TEST_LINE = re.compile(r"^test (\S+) \.\.\. (ok|FAILED|ignored)\b")
_FAILURE_BLOCK = re.compile(r"^---- (\S.+) stdout ----$")


def _parse_cargo_output(raw: str) -> dict:
    """Walk cargo test's plain-text output and extract per-test
    pass/fail/ignored outcomes plus the failure-line annotations.

    Cargo's text output is line-oriented; clean per-test result
    lines look like ``test test::client::list_databases ... ok``
    or ``... FAILED``. Tests that print to stdout during running
    (e.g. unified-spec runners that log per-subtest progress)
    break the simple matcher because cargo flushes the test's
    stdout between the ``...`` and the outcome — the outcome may
    end up on its own line. Cargo also emits a
    ``---- <name> stdout ----`` block for every FAILED test,
    which is the authoritative source of truth: we use both, the
    test-line regex catches the clean cases and the failure-block
    matcher catches the noisy ones (de-duping by test name so a
    failure isn't double-counted when both forms appear).

    JSON output (``cargo test -- --format json``) is unstable
    nightly-only.
    """
    tests: list[dict] = []
    failures: list[dict] = []
    summary = {"passed": 0, "failed": 0, "ignored": 0}
    seen_names: set[str] = set()
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
            if name not in seen_names:
                seen_names.add(name)
                tests.append({"name": name, "outcome": outcome_norm})
                summary[outcome_norm] += 1
            continue
        # ``---- <test_name> stdout ----`` only appears for FAILED
        # tests. Authoritative for failure detection — counted here
        # if the inline outcome line was split or missed.
        fb = _FAILURE_BLOCK.match(line)
        if fb:
            in_failure_block = True
            name = fb.group(1)
            current_failure = {"name": name, "message": ""}
            failures.append(current_failure)
            if name not in seen_names:
                seen_names.add(name)
                tests.append({"name": name, "outcome": "failed"})
                summary["failed"] += 1
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
