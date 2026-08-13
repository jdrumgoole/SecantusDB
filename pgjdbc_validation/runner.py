"""Run pgjdbc's vendored test suite against a SecantusPGServer daemon.

1. Spawn a fresh daemon on an ephemeral port; verify it is SecantusDB.
2. Write ``vendor/pgjdbc/build.local.properties`` pointing at it — pgjdbc's
   own config mechanism, gitignored upstream, so the submodule stays
   unmodified.
3. Run ``./gradlew :postgresql:test`` with the include patterns from
   ``include_paths.py`` and a 60s JUnit-5 default timeout, using a JDK 21
   (pgjdbc's Gradle toolchain requirement).
4. Aggregate the JUnit XML results into ``.validation/pgjdbc-raw.json`` for
   ``generate_report.py``.

Run via ``uv run python -m invoke validate-pgjdbc``.
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
import xml.etree.ElementTree as ET
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "pgjdbc"
RESULTS = VENDOR / "pgjdbc" / "build" / "test-results" / "test"
RAW_OUT = REPO_ROOT / ".validation" / "pgjdbc-raw.json"

#: Wall-clock budget for the whole gradle run. Overridable for slow hardware —
#: CI runners are several times slower than a dev machine, and the suite grew
#: materially once the crashes that used to end tests in milliseconds were
#: fixed (a full run is 75 classes / ~5.5k tests, where a server that bailed
#: early only reached 50). A run that exceeds this is TRUNCATED, not failed:
#: see ``_aggregate`` for why that distinction has to survive into the report.
GRADLE_TIMEOUT_SECONDS = float(os.environ.get("SECANTUS_PGJDBC_TIMEOUT", 7200.0))
JUNIT_DEFAULT_TIMEOUT = "60s"


def _shard_spec() -> tuple[int, int] | None:
    """The ``SECANTUS_PGJDBC_SHARD`` env as ``(index, of)``, e.g. ``2/4`` →
    ``(2, 4)``; ``None`` when unset (run everything, the local default). The
    CI lane splits the class list across parallel jobs this way — the suite
    itself stays byte-for-byte unmodified, each shard just runs a
    deterministic round-robin slice of the class list."""
    raw = os.environ.get("SECANTUS_PGJDBC_SHARD", "").strip()
    if not raw:
        return None
    m = re.fullmatch(r"(\d+)/(\d+)", raw)
    if m is None or not (1 <= int(m.group(1)) <= int(m.group(2))):
        print(f"bad SECANTUS_PGJDBC_SHARD {raw!r} — expected K/N with 1<=K<=N", file=sys.stderr)
        raise SystemExit(2)
    return int(m.group(1)), int(m.group(2))


#: Rough per-class duration weights for shard balancing, minutes-scale, from
#: CI runs of the unsharded lane (everything absent defaults to 1). Only the
#: RATIOS matter, and only for the heavy classes: the first sharded run used a
#: plain round-robin split and the three heaviest classes all landed in the
#: same shard (alphabetical indexes 1, 5, and 21 — all ≡ 1 mod 4), making it
#: a 44-minute straggler next to three ~15-minute siblings. Stale weights
#: degrade balance, never correctness — the partition stays exact.
#: Calibrated from sharded run 31451326038 (shard walls 11/14/24/38m with the
#: previous guesses): BatchFailureTest dominates (its shard ran 38m), while
#: CopyLargeFileTest went light once sequences allocate in batches. Per-class
#: ``seconds`` now ride in the raw JSON, so recalibration is a read of the
#: latest shard raws rather than a guess.
_CLASS_WEIGHTS = {
    "BatchFailureTest": 24,
    "AutoRollbackTest": 7,
    "CopyLargeFileTest": 4,
    "DateTest": 3,
    "BatchExecuteTest": 2,
    "TimestampTest": 2,
    "ResultSetTest": 2,
    "PreparedStatementTest": 2,
}


def _shard_classes(classes: list[str], n: int) -> list[list[str]]:
    """Deterministically partition ``classes`` into ``n`` balanced shards:
    heaviest-first greedy assignment to the least-loaded shard (LPT), weights
    from ``_CLASS_WEIGHTS``. Every class lands in exactly one shard."""
    order = sorted(
        classes,
        key=lambda c: (-_CLASS_WEIGHTS.get(c.rsplit(".", 1)[-1], 1), c),
    )
    shards: list[list[str]] = [[] for _ in range(n)]
    loads = [0] * n
    for cls in order:
        i = loads.index(min(loads))
        shards[i].append(cls)
        loads[i] += _CLASS_WEIGHTS.get(cls.rsplit(".", 1)[-1], 1)
    return [sorted(s) for s in shards]


def _raw_out_path(shard: tuple[int, int] | None) -> Path:
    if shard is None:
        return RAW_OUT
    return REPO_ROOT / ".validation" / f"pgjdbc-raw-shard-{shard[0]}.json"


def _pick_ephemeral_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait_for_listener(host: str, port: int, timeout: float = 15.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"pg daemon at {host}:{port} did not become ready within {timeout}s")


def _verify_secantus_identity(host: str, port: int) -> None:
    import psycopg

    with psycopg.connect(
        host=host, port=port, dbname="postgres", user="postgres", connect_timeout=5
    ) as conn:
        (version,) = conn.execute("select version()").fetchone()
    if "SecantusDB" not in version:
        raise RuntimeError(
            f"daemon at {host}:{port} does not identify as SecantusDB "
            f"(version(): {version!r}) — refusing to run the gauge against it"
        )


def _is_jdk21(home: str) -> bool:
    """True when ``home`` really is a JDK 21 (verified by running java
    -version — path heuristics lie; macOS's ``java_home -v 21`` happily
    returns the default JDK when no 21 is registered)."""
    java = Path(home) / "bin" / "java"
    if not java.exists():
        return False
    try:
        out = subprocess.run([str(java), "-version"], capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return False
    return ' "21' in out.stderr or ' "21' in out.stdout


def _find_jdk21() -> str | None:
    """A verified JDK 21 home: JAVA_HOME when it is one (CI's setup-java),
    the homebrew keg, else macOS's ``java_home -v 21``."""
    candidates = [
        os.environ.get("JAVA_HOME", ""),
        "/opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home",
    ]
    try:
        out = subprocess.run(
            ["/usr/libexec/java_home", "-v", "21"], capture_output=True, text=True, timeout=10
        )
        if out.returncode == 0 and out.stdout.strip():
            candidates.append(out.stdout.strip())
    except (OSError, subprocess.SubprocessError):
        pass
    for home in candidates:
        if home and _is_jdk21(home):
            return home
    return None


def _test_classes() -> list[str]:
    """Fully-qualified ``--tests`` patterns: every ``*Test`` class in the
    included packages of the vendored tree, minus the excluded ones."""
    from .include_paths import EXCLUDE_CLASSES, INCLUDE_PACKAGES

    out: list[str] = []
    base = VENDOR / "pgjdbc" / "src" / "test" / "java" / "org" / "postgresql" / "test"
    for pkg in INCLUDE_PACKAGES:
        for f in sorted((base / pkg).glob("*Test.java")):
            if f.stem in EXCLUDE_CLASSES:
                continue
            out.append(f"org.postgresql.test.{pkg}.{f.stem}")
    return out


def main() -> int:

    if not (VENDOR / "gradlew").exists():
        print(
            "vendor/pgjdbc is missing — run `git submodule update --init vendor/pgjdbc`",
            file=sys.stderr,
        )
        return 2
    jdk = _find_jdk21()
    if not jdk:
        print("the pgjdbc gauge requires a JDK 21 (JAVA_HOME or openjdk@21)", file=sys.stderr)
        return 2

    shard = _shard_spec()
    raw_out = _raw_out_path(shard)
    raw_out.parent.mkdir(exist_ok=True)
    if RESULTS.exists():
        shutil.rmtree(RESULTS)

    host = "127.0.0.1"
    port = _pick_ephemeral_port()
    storage_dir = tempfile.mkdtemp(prefix="secantus-pgjdbc-gauge-")
    daemon = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "secantus.sql.pgserver",
            "--host",
            host,
            "--port",
            str(port),
            "--storage-path",
            storage_dir,
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        _wait_for_listener(host, port)
        _verify_secantus_identity(host, port)

        # pgjdbc's stock local-config mechanism; *.local.properties is in the
        # submodule's own .gitignore, so this never dirties the vendored tree.
        (VENDOR / "build.local.properties").write_text(
            f"test.url.PGHOST={host}\n"
            f"test.url.PGPORT={port}\n"
            "test.url.PGDBNAME=test\n"
            "user=test\n"
            "password=test\n"
            "privilegedUser=postgres\n"
            "privilegedPassword=\n"
        )

        cmd = [
            "./gradlew",
            f"-Dorg.gradle.java.installations.paths={jdk}",
            # testExtraJvmArgs is pgjdbc's build parameter for extra TEST-JVM
            # args — a plain -D here would land on Gradle's JVM instead.
            f"-PtestExtraJvmArgs=-Djunit.jupiter.execution.timeout.default={JUNIT_DEFAULT_TIMEOUT}",
            "--console=plain",
            ":postgresql:test",
        ]
        classes = _test_classes()
        if shard is not None:
            k, n = shard
            classes = _shard_classes(classes, n)[k - 1]
            print(f"pgjdbc shard {k}/{n}: {len(classes)} classes")
        for pattern in classes:
            cmd += ["--tests", pattern]
        env = {**os.environ, "JAVA_HOME": jdk}
        try:
            proc = subprocess.run(cmd, cwd=VENDOR, env=env, timeout=GRADLE_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            # Aggregate what did run rather than letting the exception escape.
            # RESULTS is wiped at startup, so without this a timed-out run
            # reports *zero tests* — which reads like a clean sweep instead of
            # "nothing was measured". The truncated flag rides into the JSON so
            # generate_report refuses to publish a partial denominator.
            _aggregate(raw_out, shard=shard, truncated=True)
            print(
                f"pgjdbc gauge TRUNCATED: gradle exceeded "
                f"{GRADLE_TIMEOUT_SECONDS:.0f}s and was killed. Partial results "
                f"aggregated to {raw_out}; the run is NOT a measurement. Raise "
                f"SECANTUS_PGJDBC_TIMEOUT to give it more room.",
                file=sys.stderr,
            )
            return 124  # conventional shell exit for "timed out"
        _aggregate(raw_out, shard=shard)
        # Baseline-aware verdict: gradle exits non-zero while ANY test fails,
        # which made the lane red by construction until 100% conformance —
        # its conclusion carried no signal. Fail only on regression vs the
        # committed baseline (see pgjdbc_validation/baseline.py). A gradle
        # failure that produced no XML at all (build/daemon breakage) is
        # still a hard failure — zero classes aggregated is not a clean run.
        raw = json.loads(raw_out.read_text())
        if proc.returncode != 0 and not raw.get("classes"):
            print(
                "pgjdbc gauge: gradle failed before producing any test results",
                file=sys.stderr,
            )
            return proc.returncode
        from pgjdbc_validation.baseline import verdict

        return verdict(raw_out)
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()


def _aggregate(
    raw_out: Path | None = None,
    *,
    shard: tuple[int, int] | None = None,
    truncated: bool = False,
) -> None:
    """JUnit XML → one JSON blob (per-class counts + failing test names).

    ``truncated`` marks a run gradle did not finish. It has to be recorded
    rather than inferred: the per-class numbers of a partial run look entirely
    normal, and only the *denominator* is wrong, so a truncated run renders a
    plausible-looking conformance rate that is simply measuring less of the
    suite. ``generate_report`` refuses to publish one.
    """
    if raw_out is None:
        raw_out = RAW_OUT  # late-bound so tests can monkeypatch the module attr
    classes = []
    for xml_file in sorted(RESULTS.glob("*.xml")) if RESULTS.exists() else []:
        try:
            root = ET.parse(xml_file).getroot()
        except ET.ParseError:
            continue
        name = root.get("name", xml_file.stem)
        entry = {
            "class": name,
            "tests": int(root.get("tests", 0)),
            "failures": int(root.get("failures", 0)) + int(root.get("errors", 0)),
            "skipped": int(root.get("skipped", 0)),
            # Wall seconds for this class, straight from the JUnit XML — the
            # evidence base for recalibrating _CLASS_WEIGHTS when shard walls
            # drift apart.
            "seconds": round(float(root.get("time", 0.0)), 1),
            "failed_tests": [],
        }
        slow: dict[str, float] = {}
        for tc in root.iter("testcase"):
            if tc.find("failure") is not None or tc.find("error") is not None:
                entry["failed_tests"].append(tc.get("name"))
            tc_secs = float(tc.get("time", 0.0))
            if tc_secs >= 1.0:
                # Per-test wall seconds for the slow tail only (the raw stays
                # compact). This is the evidence base for CI-vs-local timing
                # anomalies: BatchFailureTest measures 44s locally but 1260s
                # in CI, and the per-CLASS number can't say which of its 184
                # parameterized variants stall.
                key = f"{tc.get('classname', '')}::{tc.get('name', '')}"
                slow[key] = round(slow.get(key, 0.0) + tc_secs, 1)
        if slow:
            entry["slow_tests"] = dict(sorted(slow.items(), key=lambda kv: -kv[1])[:25])
        classes.append(entry)
    payload: dict = {"classes": classes}
    if shard is not None:
        # Recorded so the report merge can verify it holds a COMPLETE shard
        # set (every index 1..of exactly once) before publishing a rate.
        payload["shard"] = {"index": shard[0], "of": shard[1]}
    if truncated:
        payload["truncated"] = True
    raw_out.parent.mkdir(exist_ok=True)
    raw_out.write_text(json.dumps(payload, indent=1))


if __name__ == "__main__":
    raise SystemExit(main())
