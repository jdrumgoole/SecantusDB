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

GRADLE_TIMEOUT_SECONDS = 3600.0
JUNIT_DEFAULT_TIMEOUT = "60s"


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
        out = subprocess.run(
            [str(java), "-version"], capture_output=True, text=True, timeout=10
        )
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

    RAW_OUT.parent.mkdir(exist_ok=True)
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
        for pattern in _test_classes():
            cmd += ["--tests", pattern]
        env = {**os.environ, "JAVA_HOME": jdk}
        proc = subprocess.run(cmd, cwd=VENDOR, env=env, timeout=GRADLE_TIMEOUT_SECONDS)
        _aggregate()
        return proc.returncode
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()


def _aggregate() -> None:
    """JUnit XML → one JSON blob (per-class counts + failing test names)."""
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
            "failed_tests": [],
        }
        for tc in root.iter("testcase"):
            if tc.find("failure") is not None or tc.find("error") is not None:
                entry["failed_tests"].append(tc.get("name"))
        classes.append(entry)
    RAW_OUT.write_text(json.dumps({"classes": classes}, indent=1))


if __name__ == "__main__":
    raise SystemExit(main())
