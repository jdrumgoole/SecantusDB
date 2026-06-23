"""Run mongo-kotlin-driver's integration tests against a standalone SecantusDB.

Same two-phase shape as ``java_validation/runner.py`` (the Kotlin driver
lives in the same monorepo and reuses driver-sync's ``ClusterFixture``):
spawn SecantusDB without ``--auth`` and seed a root user, restart on the
same storage with ``--auth``, then invoke Gradle's
``:driver-kotlin-sync:integrationTest`` task with the seeded URI.

The Java driver test bootstrap reads the URI from the JVM system property
``org.mongodb.test.uri`` (default ``mongodb://localhost:27017``); we forward
it through Gradle with ``-D``. We use the gradle wrapper (``./gradlew``)
the driver ships, so only a JDK (8-23 — Gradle 8.12 caps at 23) is needed.

Gradle writes JUnit XML per test class to
``<module>/build/test-results/integrationTest/TEST-*.xml`` (note:
``integrationTest``, not ``test`` — that's the only structural difference
from the Java gauge). We copy those into ``.validation/kotlin-results/`` and
let ``generate_report.py`` walk them.

The pure JDK-selection / identity / jstack helpers are shared with the
Java gauge (imported from ``java_validation.runner``) rather than
duplicated.

Run via ``uv run python -m invoke validate-kotlin``.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

import gauge_common

# The Java gauge's pure helpers (JDK probe, identity tripwire,
# jstack-on-hang dumper) are imported lazily inside ``main`` from
# ``java_validation.runner`` — same JVM toolchain, so they're shared
# rather than duplicated.

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-java-driver"
RESULTS_DIR = REPO_ROOT / ".validation" / f"kotlin-results{gauge_common.report_suffix()}"

# Test users the driver's ClusterFixture connection string expects when
# ``-Dorg.mongodb.test.uri`` carries credentials (shared with the Java gauge).
ROOT_USER = "root-user"
ROOT_PASSWORD = "password"

_JDK_GRADLE_MAX = 23


def _harvest_subdir(task: str) -> str:
    """JUnit XML subdir name for a gradle task — its trailing segment.

    ``:driver-kotlin-sync:integrationTest`` -> ``integrationTest``;
    ``:driver-sync:test`` -> ``test``. Gradle writes
    ``<module>/build/test-results/<subdir>/TEST-*.xml``.
    """
    return task.rsplit(":", 1)[-1]


def main() -> int:
    from java_validation.runner import (
        _java_major,
        _jstack_all_javas,
        _verify_secantus_identity,
    )

    from .include_modules import INCLUDE

    if shutil.which("java") is None or shutil.which("javac") is None:
        print(
            "javac not found on PATH; install a full JDK (>=8) to run "
            "kotlin_validation. macOS: `brew install openjdk@21`. Linux: "
            "`apt install default-jdk` or `dnf install java-21-openjdk-devel`.",
            file=sys.stderr,
        )
        return 2

    # Same Gradle/JDK constraint as the Java gauge: the driver pins gradle
    # 8.12 (caps at JDK 23). Pick a supported JDK robustly — check the
    # effective JDK (JAVA_HOME if set, else PATH), override with a 17/21
    # candidate whenever it's unsupported (>=24) or undetectable.
    java_home_override: str | None = None
    effective_home = os.environ.get("JAVA_HOME") or None
    effective_major = _java_major(effective_home)
    if effective_major is None or effective_major > _JDK_GRADLE_MAX:
        for candidate in (
            "/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home",
            "/usr/lib/jvm/java-17-openjdk-amd64",
            "/opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home",
            "/usr/lib/jvm/java-21-openjdk-amd64",
        ):
            cand_major = _java_major(candidate) if Path(candidate).is_dir() else None
            if cand_major is not None and cand_major <= _JDK_GRADLE_MAX:
                java_home_override = candidate
                break
        if java_home_override is None:
            print(
                "kotlin_validation: no Gradle-supported JDK (8-23) found "
                f"(effective JDK major={effective_major}, JAVA_HOME={effective_home!r}). "
                "The mongo-java-driver's bundled Gradle 8.12 cannot run on JDK 24+. "
                "Install one, e.g. `brew install openjdk@17`, and re-run.",
                file=sys.stderr,
            )
            return 2
    if not VENDOR.is_dir() or not (VENDOR / "gradlew").is_file():
        print(
            f"vendor/mongo-java-driver/ missing or not initialised "
            f"({VENDOR}); run `git submodule update --init --recursive`",
            file=sys.stderr,
        )
        return 2
    if not (VENDOR / "testing" / "resources" / "specifications" / "source").is_dir():
        print(
            "vendor/mongo-java-driver/testing/resources/specifications/ is empty "
            "(nested submodule). Run `git submodule update --init --recursive` "
            "from the repo root — without the spec data the unified-spec runners "
            "fail with `initializationError` on missing JSON files.",
            file=sys.stderr,
        )
        return 2

    # Wipe any prior JUnit XML so the report doesn't double-count.
    if RESULTS_DIR.exists():
        shutil.rmtree(RESULTS_DIR)
    RESULTS_DIR.mkdir(parents=True)

    host = "127.0.0.1"
    storage_dir = tempfile.mkdtemp(prefix="secantus-kotlin-gauge-")
    print(
        f"kotlin_validation: storage tempdir {storage_dir} (will be cleaned up)",
        file=sys.stderr,
    )

    def _spawn_daemon(*, with_auth: bool) -> tuple[subprocess.Popen, str, int]:
        cmd = [
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
            # Standalone topology so the driver's ``getSecondary()`` isn't an
            # unbounded sleep loop — same rationale as the Java gauge.
            "--standalone",
        ]
        if with_auth:
            cmd.append("--auth")
        return gauge_common.spawn_daemon(cmd, label="kotlin_validation")

    print("kotlin_validation: phase 1 — seeding daemon (no --auth)", file=sys.stderr)
    daemon, host, port = _spawn_daemon(with_auth=False)
    try:
        _verify_secantus_identity(host, port, "kotlin_validation")
        print("kotlin_validation: seeding root-user", file=sys.stderr)
        import pymongo

        client = pymongo.MongoClient(
            f"mongodb://{host}:{port}/", directConnection=True, serverSelectionTimeoutMS=5_000
        )
        client.admin.command("createUser", ROOT_USER, pwd=ROOT_PASSWORD, roles=["root"])
        client.close()
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=1)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()

    print("kotlin_validation: phase 2 — running gauge with --auth", file=sys.stderr)
    daemon, host, port = _spawn_daemon(with_auth=True)
    try:
        uri = f"mongodb://{ROOT_USER}:{ROOT_PASSWORD}@{host}:{port}/?authSource=admin"
        env = os.environ.copy()
        if java_home_override:
            env["JAVA_HOME"] = java_home_override
            env["PATH"] = f"{java_home_override}/bin:" + env.get("PATH", "")
            print(
                f"kotlin_validation: using JAVA_HOME={java_home_override} "
                f"(gradle 8.12 doesn't support JDK 24+)",
                file=sys.stderr,
            )

        timeout_s = int(os.environ.get("VALIDATE_KOTLIN_TIMEOUT_S", "1800"))
        idle_s = int(os.environ.get("VALIDATE_KOTLIN_JSTACK_IDLE_S", "120"))
        max_dumps = int(os.environ.get("VALIDATE_KOTLIN_JSTACK_MAX", "5"))
        jstack_dir = REPO_ROOT / ".validation" / "kotlin-stacks"
        if jstack_dir.exists():
            shutil.rmtree(jstack_dir)
        jstack_dir.mkdir(parents=True)

        init_script = REPO_ROOT / "kotlin_validation" / "init.gradle.kts"
        if os.environ.get("SECANTUS_GAUGE_PARALLEL_FORKS") is None:
            env["SECANTUS_GAUGE_PARALLEL_FORKS"] = str(os.cpu_count() or 1)

        # Wipe stale JUnit XML before running so a failed build can't
        # masquerade as a passing run (see java_validation.runner for the
        # full rationale — a JDK-25 build failure once kept reporting 100%).
        shutil.rmtree(RESULTS_DIR, ignore_errors=True)
        RESULTS_DIR.mkdir(parents=True, exist_ok=True)
        for module_dir in VENDOR.iterdir():
            stale_results = module_dir / "build" / "test-results"
            if stale_results.is_dir():
                shutil.rmtree(stale_results, ignore_errors=True)

        gradle_rcs: list[int] = []
        for spec in INCLUDE:
            cmd = [
                "./gradlew",
                "--no-daemon",
                "--console=plain",
                "--init-script",
                str(init_script),
                "--rerun-tasks",
                f"-Dorg.mongodb.test.uri={uri}",
                "-Djunit.jupiter.execution.timeout.default=15s",
                "-Djunit.jupiter.execution.timeout.test.method.default=15s",
                "-Djunit.jupiter.execution.timeout.testable.method.default=15s",
                "-Djunit.jupiter.execution.timeout.mode=enabled_on_non_parallel_tests",
                spec.task,
            ]
            for fqn in spec.test_classes:
                cmd.extend(["--tests", fqn])
            filter_msg = f" ({len(spec.test_classes)} --tests filters)" if spec.test_classes else ""
            print(
                f"kotlin_validation: running {spec.task}{filter_msg} in {VENDOR} (URI={uri})",
                file=sys.stderr,
            )

            proc = subprocess.Popen(
                cmd,
                cwd=VENDOR,
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                bufsize=1,
                text=True,
            )
            last_output_ts = [time.monotonic()]
            forward_done = threading.Event()

            def _forward(p=proc, ts=last_output_ts, done=forward_done) -> None:
                assert p.stdout is not None
                for line in p.stdout:
                    sys.stdout.write(line)
                    sys.stdout.flush()
                    ts[0] = time.monotonic()
                done.set()

            threading.Thread(target=_forward, daemon=True).start()

            deadline = time.monotonic() + timeout_s
            dumps = 0
            timed_out = False
            while True:
                try:
                    gradle_rc = proc.wait(timeout=15)
                    break
                except subprocess.TimeoutExpired:
                    pass
                now = time.monotonic()
                if now >= deadline:
                    print(
                        f"kotlin_validation: gradle exceeded {timeout_s}s wall clock "
                        f"on {spec.task}; SIGTERM-ing and harvesting partial JUnit "
                        "XML. Override with VALIDATE_KOTLIN_TIMEOUT_S=<seconds>.",
                        file=sys.stderr,
                    )
                    proc.terminate()
                    try:
                        gradle_rc = proc.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        gradle_rc = proc.wait()
                    timed_out = True
                    break
                if now - last_output_ts[0] > idle_s and dumps < max_dumps:
                    print(
                        f"jstack-on-hang: no gradle output for "
                        f"{int(now - last_output_ts[0])}s on {spec.task}; "
                        f"dumping java threads ({dumps + 1}/{max_dumps})",
                        file=sys.stderr,
                    )
                    _jstack_all_javas(jstack_dir, java_home_override)
                    dumps += 1
                    last_output_ts[0] = now

            if timed_out:
                gradle_rc = 124
            forward_done.wait(timeout=2)
            gradle_rcs.append(gradle_rc)

        gradle_rc = max(gradle_rcs) if gradle_rcs else 0

        # Copy JUnit XML out of the source tree. Harvest from each spec's
        # task-specific subdir (``integrationTest`` for this gauge).
        subdirs = {_harvest_subdir(spec.task) for spec in INCLUDE}
        copied = 0
        for module_dir in VENDOR.iterdir():
            for subdir in subdirs:
                results = module_dir / "build" / "test-results" / subdir
                if not results.is_dir():
                    continue
                dest = RESULTS_DIR / module_dir.name
                dest.mkdir(parents=True, exist_ok=True)
                for xml in results.glob("TEST-*.xml"):
                    shutil.copy(xml, dest / xml.name)
                    copied += 1

        if copied == 0:
            print(
                f"kotlin_validation: no JUnit XML written (gradle exit {gradle_rc}; build error?)",
                file=sys.stderr,
            )
            return 1

        passed = failed = skipped = 0
        import xml.etree.ElementTree as ET

        for xml in RESULTS_DIR.rglob("TEST-*.xml"):
            try:
                root = ET.parse(xml).getroot()
            except ET.ParseError:
                continue
            tests = int(root.attrib.get("tests", 0))
            failures = int(root.attrib.get("failures", 0))
            errors = int(root.attrib.get("errors", 0))
            skipped_n = int(root.attrib.get("skipped", 0))
            failed += failures + errors
            skipped += skipped_n
            passed += tests - failures - errors - skipped_n
        print(
            f"kotlin_validation: {passed} passed, {failed} failed, "
            f"{skipped} skipped (per-module breakdown in the markdown report)"
        )
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=1)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
        shutil.rmtree(storage_dir, ignore_errors=True)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
