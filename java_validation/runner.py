"""Run mongo-java-driver's tests against a standalone SecantusDB daemon.

Same shape as `go_validation/runner.py` and `node_validation/runner.py`:
spawn SecantusDB as a subprocess, set the URI for the test JVM, then
invoke Gradle to run the in-scope modules.

The Java driver test bootstrap reads the URI from a JVM system
property, `org.mongodb.test.uri` (default `mongodb://localhost:27017`).
We pass it through Gradle with `-D` which Gradle forwards to the test
task's JVM via the `systemProperty` mechanism. We use the gradle
wrapper (`./gradlew`) the driver ships, so a system Gradle install
isn't required — only a JDK (>=8) on PATH.

Gradle writes JUnit XML output per test class to
`<module>/build/test-results/test/TEST-*.xml`. We collect those into
`.validation/java-results/` and let `generate_report.py` walk them.

First run is slow because Gradle has to download its distribution
(~150 MB) plus all driver dependencies. Subsequent runs reuse the
caches and are much faster.

Run via `uv run python -m invoke validate-java`.
"""

from __future__ import annotations

import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

import gauge_common

from .include_modules import INCLUDE


def _jstack_all_javas(jstack_dir: Path, java_home: str | None) -> None:
    """jstack every java PID currently visible. Writes one file per PID
    under `jstack_dir/jstack-<HHMMSS>-<pid>.txt`. Best-effort — silent on
    individual failures; surfaces a count to stderr."""
    jstack_bin = "jstack"
    if java_home:
        candidate = Path(java_home) / "bin" / "jstack"
        if candidate.is_file():
            jstack_bin = str(candidate)
    pids_proc = subprocess.run(["pgrep", "-f", "java"], capture_output=True, text=True)
    pids = [p for p in pids_proc.stdout.split() if p.strip()]
    if not pids:
        print("jstack-on-hang: no java PIDs to dump", file=sys.stderr)
        return
    ts = time.strftime("%H%M%S")
    written = 0
    for pid in pids:
        out_path = jstack_dir / f"jstack-{ts}-{pid}.txt"
        try:
            with out_path.open("w") as fh:
                subprocess.run(
                    [jstack_bin, pid],
                    stdout=fh,
                    stderr=subprocess.STDOUT,
                    timeout=10,
                )
            written += 1
        except Exception as exc:
            print(f"jstack-on-hang: failed for pid {pid}: {exc}", file=sys.stderr)
    print(
        f"jstack-on-hang: wrote {written} thread dump(s) to {jstack_dir}",
        file=sys.stderr,
    )


REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-java-driver"
RESULTS_DIR = REPO_ROOT / ".validation" / f"java-results{gauge_common.report_suffix()}"

# Test users mongo-java-driver's ClusterFixture connection string
# expects when ``-Dorg.mongodb.test.uri`` carries credentials.
ROOT_USER = "root-user"
ROOT_PASSWORD = "password"


def _java_major(java_home: str | None) -> int | None:
    """Major version of the ``javac`` under ``java_home`` (or on PATH when
    ``java_home`` is None), or None if it can't be determined.

    ``javac 17.0.19`` -> 17; ``javac 21.0.11`` -> 21; the old
    ``javac 1.8.0_431`` form -> 8.
    """
    javac = str(Path(java_home) / "bin" / "javac") if java_home else "javac"
    try:
        out = subprocess.run([javac, "-version"], capture_output=True, text=True, timeout=15)
    except (OSError, subprocess.SubprocessError):
        return None
    m = re.search(r"\b(\d+)(?:\.(\d+))?", out.stdout + out.stderr)
    if not m:
        return None
    major = int(m.group(1))
    if major == 1 and m.group(2):  # legacy 1.8 -> 8
        major = int(m.group(2))
    return major


def _pick_ephemeral_port() -> int:
    """Ask the kernel for a free ephemeral TCP port. See ``go_validation.runner``."""
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
    if shutil.which("java") is None or shutil.which("javac") is None:
        # `javac` is the JDK compiler — Gradle needs a full JDK, not just a
        # JRE. The error from gradle without javac is opaque ("No Java
        # compiler found, please ensure you are running Gradle with a JDK"),
        # so check up front and give the user a useful pointer.
        print(
            "javac not found on PATH; install a full JDK (>=8) to run "
            "java_validation. macOS: `brew install openjdk@21`. Linux: "
            "`apt install default-jdk` or `dnf install java-21-openjdk-devel`.",
            file=sys.stderr,
        )
        return 2

    # The driver pins gradle 8.12 (caps at JDK 23) and its buildSrc
    # toolchain requires `languageVersion=17` exactly — JDK 17 works, 21
    # fails the toolchain query in buildSrc, and 24+ can't even parse the
    # version string (`IllegalArgumentException: 25.0.2`, build aborts
    # before any test runs). Pick a supported JDK *robustly*: check the
    # EFFECTIVE jdk (JAVA_HOME if set, else PATH), and override with a
    # 17/21 candidate whenever it's unsupported (>=24) or undetectable.
    # The old code only auto-selected when JAVA_HOME was unset, so a
    # machine whose shell preset JAVA_HOME to a current JDK (brew's
    # default `openjdk` is 25) sailed straight into a doomed build. CI
    # sets JAVA_HOME to 17 via actions/setup-java, which is kept as-is.
    _JDK_GRADLE_MAX = 23
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
                "java_validation: no Gradle-supported JDK (8-23) found "
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
            "from the repo root — without the spec data the bson-corpus and "
            "binary-vector tests fail with `initializationError` on missing files.",
            file=sys.stderr,
        )
        return 2

    # Wipe any prior JUnit XML so the report doesn't double-count.
    if RESULTS_DIR.exists():
        shutil.rmtree(RESULTS_DIR)
    RESULTS_DIR.mkdir(parents=True)

    host = "127.0.0.1"

    # Tempdir storage so the user records we seed in phase 1 survive
    # the daemon restart in phase 2 with --auth.
    storage_dir = tempfile.mkdtemp(prefix="secantus-java-gauge-")
    print(
        f"java_validation: storage tempdir {storage_dir} (will be cleaned up)",
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
            # Java's ``ClusterFixture.getSecondary()`` is an unbounded
            # sleep loop on non-RS deployments — and our default
            # ``hello`` reply advertises us as a single-node RS primary
            # (so pymongo's change-stream machinery accepts the
            # topology). For the Java gauge specifically we want
            # standalone classification so the driver's
            # ``assumeTrue(isReplicaSet())`` gate skips the multi-node
            # tests instead of hanging.
            "--standalone",
        ]
        if with_auth:
            cmd.append("--auth")
        # Race-free spawn on a kernel-assigned port (see gauge_common.spawn_daemon).
        # Each phase binds its own port; seeded users persist via storage_dir.
        return gauge_common.spawn_daemon(cmd, label="java_validation")

    print("java_validation: phase 1 — seeding daemon (no --auth)", file=sys.stderr)
    daemon, host, port = _spawn_daemon(with_auth=False)
    try:
        _verify_secantus_identity(host, port, "java_validation")
        print("java_validation: seeding root-user", file=sys.stderr)
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

    print("java_validation: phase 2 — running gauge with --auth", file=sys.stderr)
    daemon, host, port = _spawn_daemon(with_auth=True)
    try:
        uri = f"mongodb://{ROOT_USER}:{ROOT_PASSWORD}@{host}:{port}/?authSource=admin"
        env = os.environ.copy()
        if java_home_override:
            env["JAVA_HOME"] = java_home_override
            env["PATH"] = f"{java_home_override}/bin:" + env.get("PATH", "")
            print(
                f"java_validation: using JAVA_HOME={java_home_override} "
                f"(gradle 8.12 doesn't support JDK 24+)",
                file=sys.stderr,
            )

        # Per-module configuration. Hard wall-clock timeout (env-tunable),
        # plus jstack-on-hang: if gradle stops emitting stdout for
        # VALIDATE_JAVA_JSTACK_IDLE_S seconds (default 120), we jstack
        # every java PID and write the dumps under .validation/java-stacks/.
        # Repeat up to MAX_DUMPS times so we get snapshots even if the
        # hang is "slow drift" rather than fully wedged.
        timeout_s = int(os.environ.get("VALIDATE_JAVA_TIMEOUT_S", "1800"))
        idle_s = int(os.environ.get("VALIDATE_JAVA_JSTACK_IDLE_S", "120"))
        max_dumps = int(os.environ.get("VALIDATE_JAVA_JSTACK_MAX", "5"))
        jstack_dir = REPO_ROOT / ".validation" / "java-stacks"
        if jstack_dir.exists():
            shutil.rmtree(jstack_dir)
        jstack_dir.mkdir(parents=True)

        # One gradle invocation per module so a `--tests` filter applied
        # to one module (driver-core) doesn't accidentally clamp another
        # module (bson). Each invocation pays gradle's startup cost (~10s
        # without --daemon), but the alternative is a single multi-task
        # invocation where Gradle CLI's `--tests` would apply globally.
        # Phase 3: parallel JVM forks for the test tasks. The driver's
        # ``conventions/testing-base.gradle.kts`` hardcodes
        # ``maxParallelForks = 1``; we override via an init-script
        # (vendored tree stays unmodified). Worker count is the runner's
        # CPU count by default; ``SECANTUS_GAUGE_PARALLEL_FORKS`` overrides.
        init_script = REPO_ROOT / "java_validation" / "init.gradle.kts"
        forks = os.environ.get("SECANTUS_GAUGE_PARALLEL_FORKS")
        if forks is None:
            env["SECANTUS_GAUGE_PARALLEL_FORKS"] = str(os.cpu_count() or 1)

        # Wipe stale JUnit XML before running so a failed build can't
        # masquerade as a passing run. If Gradle aborts (unsupported JDK,
        # a buildSrc compile error) it writes NO fresh XML — and without
        # this, the harvest below would copy a *previous* run's results
        # and emit a stale report showing a false pass rate (this is how
        # a JDK-25 build failure silently kept reporting "100%"). Clearing
        # both the harvest dir and the in-tree results dirs guarantees the
        # `copied == 0` guard fires loudly when nothing actually ran.
        shutil.rmtree(RESULTS_DIR, ignore_errors=True)
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
                # ``--rerun-tasks`` forces Gradle to re-execute the
                # test task even when its inputs (Java sources, system
                # properties) haven't changed. Without it, server-side
                # SecantusDB changes don't invalidate Gradle's
                # test-task cache and the same stale JUnit XML is read
                # back, producing identical "171/183" numbers no
                # matter what we fix on the server.
                "--rerun-tasks",
                f"-Dorg.mongodb.test.uri={uri}",
                # Per-test wall-clock timeout. JUnit 5 (Jupiter)
                # honours these system properties; with the default
                # 15s budget, a single test that hangs on a tailable
                # getMore / change-stream resume / SDAM retry loop
                # is killed with TimeoutException and the run keeps
                # moving instead of pinning the whole gauge.
                # ``mode=separate_thread`` ensures the timeout fires
                # even if the test is blocked on a native call.
                "-Djunit.jupiter.execution.timeout.default=15s",
                "-Djunit.jupiter.execution.timeout.test.method.default=15s",
                "-Djunit.jupiter.execution.timeout.testable.method.default=15s",
                "-Djunit.jupiter.execution.timeout.mode=enabled_on_non_parallel_tests",
                # Spock 1.x reads its own timeout config. We can't
                # set per-test from outside without editing the spec
                # tree, but the driver doesn't use slow Spock tests
                # in our include set (those are in ``ExcludeSlow``
                # already, gated by ``-Dspock.configuration``).
                spec.task,
            ]
            for fqn in spec.test_classes:
                cmd.extend(["--tests", fqn])
            filter_msg = f" ({len(spec.test_classes)} --tests filters)" if spec.test_classes else ""
            print(
                f"java_validation: running {spec.task}{filter_msg} in {VENDOR} (URI={uri})",
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
                    rc = proc.wait(timeout=15)
                    gradle_rc = rc
                    break
                except subprocess.TimeoutExpired:
                    pass
                now = time.monotonic()
                if now >= deadline:
                    print(
                        f"java_validation: gradle exceeded {timeout_s}s wall clock "
                        f"on {spec.task}; SIGTERM-ing and harvesting partial JUnit "
                        "XML. Override with VALIDATE_JAVA_TIMEOUT_S=<seconds>.",
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

        # Copy JUnit XML out of the source tree to keep our parser simple
        # and avoid touching the submodule.
        copied = 0
        for module_dir in VENDOR.iterdir():
            results = module_dir / "build" / "test-results" / "test"
            if not results.is_dir():
                continue
            dest = RESULTS_DIR / module_dir.name
            dest.mkdir(parents=True, exist_ok=True)
            for xml in results.glob("TEST-*.xml"):
                shutil.copy(xml, dest / xml.name)
                copied += 1

        if copied == 0:
            print(
                f"java_validation: no JUnit XML written (gradle exit {gradle_rc}; build error?)",
                file=sys.stderr,
            )
            return 1

        # Quick one-line summary.
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
            f"java_validation: {passed} passed, {failed} failed, "
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
