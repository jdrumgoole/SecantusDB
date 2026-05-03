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

import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

from .include_modules import INCLUDE

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-java-driver"
RESULTS_DIR = REPO_ROOT / ".validation" / "java-results"


def _find_free_port() -> int:
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
    if not VENDOR.is_dir() or not (VENDOR / "gradlew").is_file():
        print(
            f"vendor/mongo-java-driver/ missing or not initialised "
            f"({VENDOR}); run `git submodule update --init --recursive`",
            file=sys.stderr,
        )
        return 2

    # Wipe any prior JUnit XML so the report doesn't double-count.
    if RESULTS_DIR.exists():
        shutil.rmtree(RESULTS_DIR)
    RESULTS_DIR.mkdir(parents=True)

    host = "127.0.0.1"
    port = _find_free_port()
    daemon_cmd = [
        sys.executable, "-m", "secantus",
        "--host", host,
        "--port", str(port),
        "--storage-path", ":memory:",
        "--log-level", "WARNING",
    ]
    print(f"java_validation: starting daemon on {host}:{port}", file=sys.stderr)
    daemon = subprocess.Popen(
        daemon_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE
    )
    try:
        _wait_for_listener(host, port)

        uri = f"mongodb://{host}:{port}"
        # `--no-daemon` so Gradle doesn't leave a long-lived process behind
        # after the run; matters more for CI than dev but the cost is low.
        cmd = [
            "./gradlew", "--no-daemon", "--console=plain",
            f"-Dorg.mongodb.test.uri={uri}",
            *INCLUDE,
        ]
        print(
            f"java_validation: `{' '.join(cmd)}` in {VENDOR} "
            f"(URI={uri})",
            file=sys.stderr,
        )
        # Gradle returns non-zero on any test failure but still writes the
        # JUnit XML; capture stderr to surface real build errors but don't
        # fail the whole step.
        proc = subprocess.run(cmd, cwd=VENDOR)
        gradle_rc = proc.returncode

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
                "java_validation: no JUnit XML written "
                f"(gradle exit {gradle_rc}; build error?)",
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
            daemon.wait(timeout=5)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
