"""Run mongo-go-driver's tests against a standalone SecantusDB daemon.

The mongo-go-driver tests assume they're connecting to a real running
`mongod` over TCP. We oblige: spawn `python -m secantus --port <free>
--storage-path <tempdir>` as a subprocess, wait for the listener, point
the tests at it via `MONGODB_URI`, then tear the daemon down and remove
the tempdir. Zero modifications to the vendored go-driver tree.

Storage is on-disk (a fresh `tempfile.mkdtemp()`), not `:memory:`. Per
project policy (`CLAUDE.md` → Tooling) the conformance gauges exercise
the real WiredTiger persistence path — schema, journal, close-and-
reopen — same as the default test suite. Only the perf-regression
suite stays on `:memory:` for stable baselines.

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
import tempfile
import time
from pathlib import Path

import gauge_common

from .include_packages import INCLUDE, SKIP_PATTERNS

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-go-driver"
# Per-server raw-output path so a Rust-server run doesn't clobber the Python one.
RAW_OUT = REPO_ROOT / ".validation" / f"go-raw{gauge_common.report_suffix()}.ndjson"


def _pick_ephemeral_port() -> int:
    """Ask the kernel for a free ephemeral TCP port, close the socket, return
    the number. Lets the daemon and validate-all fan-out run in parallel
    without colliding on a fixed port. Tiny TOCTOU window between close and
    the daemon's own bind — fine on a clean CI runner where nothing else
    grabs the port; locally the worst case is a retry."""
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


def _verify_secantus_identity(host: str, port: int, gauge: str) -> None:
    """Abort unless the daemon at ``host:port`` is SecantusDB.

    SecantusDB's ``serverStatus`` carries a ``secantus`` subdocument that
    a real ``mongod`` never emits, so a stray ``mongod`` (or any foreign
    server) can never sit silently behind the gauge — mirrors the pymongo
    plugin's tripwire. Runs against the unauthenticated phase-1 daemon
    (before ``--auth``), so ``serverStatus`` needs no credentials.
    """
    import pymongo

    client = pymongo.MongoClient(
        f"mongodb://{host}:{port}/", directConnection=True, serverSelectionTimeoutMS=10_000
    )
    try:
        status = client.admin.command("serverStatus")
    finally:
        client.close()
    marker = status.get("secantus")
    if not isinstance(marker, dict) or "server" not in marker:
        raise SystemExit(
            f"{gauge}: the server at {host}:{port} is not SecantusDB "
            f"(serverStatus has no 'secantus' marker — "
            f"process={status.get('process')!r}, version={status.get('version')!r}). "
            "Refusing to run the gauge against a foreign server."
        )
    print(f"{gauge}: target verified — secantus {marker['server']} server", file=sys.stderr)


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
    storage_dir = tempfile.mkdtemp(prefix="secantus-go-gauge-")
    print(
        f"go_validation: storage tempdir {storage_dir}"
        + ("" if os.environ.get("SECANTUS_GAUGE_KEEP_STORAGE") == "1" else " (will be cleaned up)"),
        file=sys.stderr,
    )
    daemon_cmd = [
        sys.executable,
        "-m",
        "secantus",
        "--host",
        host,
        "--port",
        "0",
        "--storage-path",
        storage_dir,
        "--log-level",
        "WARNING",
        # Mongod runs a 10s ``periodicNoopIntervalSecs`` heartbeat so
        # change-stream resume tokens advance on quiet collections.
        # The go-driver's TestChangeStream_ReplicaSet/
        # resume_token_updated_on_empty_batch test asserts exactly
        # this. Match mongod's default for a faithful gauge.
        "--noop-heartbeat-seconds",
        "10",
    ]
    # Spawn on a kernel-assigned port and read it back (race-free; see
    # gauge_common.spawn_daemon). Targets the Python or Rust server per
    # SECANTUS_GAUGE_SERVER.
    daemon, host, port = gauge_common.spawn_daemon(daemon_cmd, label="go_validation")
    print(
        f"go_validation: started {gauge_common.gauge_server()} daemon on {host}:{port}",
        file=sys.stderr,
    )
    try:
        _verify_secantus_identity(host, port, "go_validation")

        env = os.environ.copy()
        # serverSelectionTimeoutMS is bumped from the driver's 30s default to
        # 60s. Under `validate-all`'s multi-gauge CPU / socket-buffer
        # contention the daemon can briefly miss a heartbeat, and
        # `TestIndexView/drop_one` / `drop_all` then trip the 30s
        # server-selection deadline mid-test (`context deadline exceeded`,
        # topology `Type: Unknown`) — a documented flake, not a server bug
        # (tasks/backlog.md). The longer floor rides out the transient blip; a
        # genuinely unreachable daemon still fails well inside the 30m package
        # timeout.
        env["MONGODB_URI"] = f"mongodb://{host}:{port}/?serverSelectionTimeoutMS=60000"
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
        # ``-p 1``: run one test PACKAGE at a time.
        #
        # ``./internal/integration/...`` expands to three packages
        # (``integration``, ``integration/mtest``, ``integration/unified``)
        # and Go runs packages concurrently by default. All three share
        # ``mtest.TestDB == "test"``, and ``mtest.Teardown()`` DROPS that
        # database (``mtest/setup.go:239``).
        #
        # Because ``TestUnifiedSpec`` is in SKIP_PATTERNS, the ``unified``
        # package has no work to do: its ``TestMain`` runs ``Setup()`` then
        # ``Teardown()`` back to back and drops ``test`` within a second of
        # starting -- concurrently with whatever ``integration`` is doing.
        #
        # That is what made ``TestChangeStream_ReplicaSet/try_next/
        # one_getMore_sent`` fail ~1 run in 3: its ``TryNext`` sends a getMore
        # with no ``maxTimeMS``, so the server waits its 1s default, and the
        # other package's teardown ``dropDatabase`` landed inside that window.
        # The server then correctly returned ``drop`` + ``invalidate`` for a
        # drop that really had happened -- so ``TryNext`` returned true and the
        # test's ``Should be false`` assertion fired. Verified by tracing every
        # dispatched command during a reproduced failure: exactly one getMore,
        # no overlapping cursors, and a ``getParameter '*'`` + ``dropDatabase``
        # pair from a second connection interleaved mid-wait.
        #
        # This was NOT a server bug -- a mongod probe recorded in
        # tasks/backlog.md shows mongod returns a mid-wait drop the same way.
        # Serialising packages removes the cross-package interference without
        # weakening any assertion or dropping the ``unified`` package's two
        # genuine unit tests (TestEntityMap / TestMatches).
        cmd = ["go", "test", "-json", "-count=1", "-timeout=30m", "-p", "1"]
        if SKIP_PATTERNS:
            # Go test's ``-skip`` is treated like ``-run``: at flag-
            # parse time the regexp is split at unbracketed-unparen'd
            # ``/`` characters into one regex per hierarchy level;
            # at match time each level of the test identifier must
            # match its corresponding part, and a test is *only*
            # skipped when the regex has *no extra* parts past the
            # test's depth (see ``testing/match.go``'s
            # ``simpleMatch.matches`` returning
            # ``partial = len(name) < len(m)`` and ``fullName``'s
            # ``skip && !partialSkip`` guard).
            #
            # Two consequences for pipe-joining patterns naively:
            #   1. Each pattern's substring-match semantics let a
            #      shorter sibling like ``resume_token`` match a
            #      longer real subtest name like
            #      ``resume_token_updated_on_empty_batch``. The
            #      shorter sibling then returns ``ok=true`` with
            #      ``partial=true`` (regex 3-parts vs name 2-parts),
            #      and ``alternationMatch.matches`` short-circuits
            #      on the first ``ok`` it sees — the longer pattern
            #      we actually wrote never gets tested.
            #   2. A 0-slash pattern alongside a multi-slash pattern
            #      in the same flag value means the multi-slash
            #      one's depth is what's used for the splittin /
            #      partial-match logic, which silently breaks the
            #      bare top-level skip.
            #
            # Fix: anchor *each part* of every multi-level pattern
            # with ``^…$``. Top-level-only patterns (no ``/``) stay
            # unanchored on purpose — they're prefix-style matches
            # intended to catch both the parent and all its subtests
            # (``TestGridFS`` is meant to also catch
            # ``TestGridFS/download/...``, etc.). For multi-level
            # patterns, anchoring per part makes the substring-match
            # bug disappear and the ``partial`` accounting precise.
            def _anchor(p: str) -> str:
                # Walk only unbracketed / unparen'd slashes, mirroring
                # Go's splitRegexp logic (see testing/match.go).
                # Patterns without an unbracketed ``/`` are returned
                # unchanged so the existing prefix-style top-level
                # skips (``TestCSOT_``, ``TestGridFS``, etc.) keep
                # working.
                cs = 0
                cp = 0
                has_slash = False
                for ch in p:
                    if ch == "\\":
                        continue
                    if ch == "[":
                        cs += 1
                    elif ch == "]":
                        cs = max(cs - 1, 0)
                    elif ch == "(" and cs == 0:
                        cp += 1
                    elif ch == ")" and cs == 0:
                        cp = max(cp - 1, 0)
                    elif ch == "/" and cs == 0 and cp == 0:
                        has_slash = True
                        break
                if not has_slash:
                    return p
                # Walk again, splitting at unbracketed-unparen'd ``/``
                # and wrapping each piece with ``^…$``.
                out: list[str] = ["^"]
                cs = 0
                cp = 0
                i = 0
                while i < len(p):
                    ch = p[i]
                    if ch == "\\" and i + 1 < len(p):
                        out.append(p[i : i + 2])
                        i += 2
                        continue
                    if ch == "[":
                        cs += 1
                    elif ch == "]":
                        cs = max(cs - 1, 0)
                    elif ch == "(" and cs == 0:
                        cp += 1
                    elif ch == ")" and cs == 0:
                        cp = max(cp - 1, 0)
                    elif ch == "/" and cs == 0 and cp == 0:
                        out.append("$/^")
                        i += 1
                        continue
                    out.append(ch)
                    i += 1
                out.append("$")
                return "".join(out)

            anchored = [_anchor(p) for p in SKIP_PATTERNS]
            cmd.append(f"-skip={'|'.join(anchored)}")
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
            daemon.wait(timeout=1)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
        # Surface daemon stderr only on test failures, otherwise suppress noise.
        # (Daemon stdout was sent to /dev/null.)
        #
        # SECANTUS_GAUGE_KEEP_STORAGE=1 leaves the data directory behind. A
        # driver-side assertion tells you a test failed but not what the server
        # sent, and some failures only reproduce under the *whole* gauge run --
        # so by the time you have a failure worth explaining, the oplog that
        # produced it has already been deleted. Keeping it is the difference
        # between reading the offending entry and guessing at it.
        if os.environ.get("SECANTUS_GAUGE_KEEP_STORAGE") == "1":
            print(
                f"go_validation: KEEPING storage tempdir {storage_dir}",
                file=sys.stderr,
            )
        else:
            shutil.rmtree(storage_dir, ignore_errors=True)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
