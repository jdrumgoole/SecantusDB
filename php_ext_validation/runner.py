"""Run mongo-php-driver's ``.phpt`` suite against a SecantusDB daemon.

End-to-end integration gauge for the low-level C extension (the PECL
``mongodb`` package that wraps libmongoc). The runner:

1. Spawns ``python -m secantus --host 127.0.0.1 --port <picked>
   --storage-path <tempdir>`` as a subprocess on a fresh ephemeral port.
2. Waits for the listener, verifies the ``secantus`` serverStatus marker.
3. Runs PHP's ``run-tests.php`` over the curated ``.phpt`` directories in
   ``include_paths.py``, against the **already-installed** ``mongodb``
   extension (no rebuild — the submodule is pinned to the installed
   extension's version so the version-sensitive tests stay aligned).
4. ``run-tests.php`` writes JUnit XML via the ``TEST_PHP_JUNIT`` env var;
   ``generate_report.py`` renders it into
   ``docs/validation-report-php-ext.md``.

The ``.phpt`` tests read the server URI from ``MONGODB_URI`` (see
``tests/utils/basic.inc``: ``define('URI', getenv('MONGODB_URI') ?: ...)``).

Run via ``uv run python -m invoke validate-php-ext``. Requires PHP >= 8.1
with the ``mongodb`` extension loaded (``brew install php`` ships it on
macOS). The submodule must be pinned to the installed extension's version
(``php --ri mongodb``) to avoid version skew in the tests.
"""

from __future__ import annotations

import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import gauge_common

from .include_paths import INCLUDE

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-php-driver"
JUNIT_OUT = REPO_ROOT / ".validation" / f"php-ext-junit{gauge_common.report_suffix()}.xml"

# Hard wall-clock limit on the run-tests.php invocation. A single .phpt that
# waits on a cursor / getMore the server doesn't satisfy as expected can pin
# the runner. Generous for the curated set; widen as the include list grows.
RUNTESTS_TIMEOUT_SECONDS = 600.0

TEST_DB = "phongo"  # tests/utils/basic.inc default DATABASE_NAME


def _pick_ephemeral_port() -> int:
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


def _resolve_php_bin() -> str | None:
    return shutil.which("php") or _first_existing(["/opt/homebrew/bin/php", "/usr/local/bin/php"])


def _first_existing(paths: list[str]) -> str | None:
    for p in paths:
        if Path(p).is_file():
            return p
    return None


def _has_mongodb_extension(php_bin: str) -> bool:
    proc = subprocess.run(
        [php_bin, "-m"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True
    )
    return "mongodb" in proc.stdout.lower().split()


def _locate_run_tests(php_bin: str) -> Path | None:
    """Find PHP's ``run-tests.php`` for the given interpreter.

    It ships under ``<prefix>/lib/php/build/run-tests.php`` in a normal PHP
    install. Derive ``<prefix>`` from the interpreter so we use the
    run-tests.php that matches the PHP we're testing with.
    """
    try:
        prefix = subprocess.run(
            [php_bin, "-r", "echo PHP_PREFIX;"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        ).stdout.strip()
    except OSError:
        prefix = ""
    # Homebrew puts it at <prefix>/lib/php/build/run-tests.php; Debian/Ubuntu
    # (the CI runners + ondrej/php PPA) nest it under an API-date subdir,
    # <prefix>/lib/php/<YYYYMMDD>/build/run-tests.php. Try the exact paths
    # first, then glob the API-date layout.
    roots = [p for p in (prefix, "/opt/homebrew/opt/php", "/usr/local/opt/php", "/usr") if p]
    for root in roots:
        exact = Path(root) / "lib" / "php" / "build" / "run-tests.php"
        if exact.is_file():
            return exact
        for hit in sorted(Path(root).glob("lib/php/*/build/run-tests.php"), reverse=True):
            if hit.is_file():
                return hit
    return None


def _verify_secantus_identity(host: str, port: int, gauge: str) -> None:
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
    php_bin = _resolve_php_bin()
    if php_bin is None:
        print(
            "php: not found on PATH; install PHP (>= 8.1) with the mongodb "
            "extension to run php_ext_validation (e.g. `brew install php`)",
            file=sys.stderr,
        )
        return 2

    if not _has_mongodb_extension(php_bin):
        print(
            f"php_ext_validation: the `mongodb` extension is not loaded in {php_bin}; "
            "install it (`pecl install mongodb` or `brew install php` ships it)",
            file=sys.stderr,
        )
        return 2

    if not VENDOR.is_dir() or not (VENDOR / "tests" / "utils" / "basic.inc").is_file():
        print(
            f"vendor/mongo-php-driver/ missing or not initialised ({VENDOR}); "
            "run `git submodule update --init vendor/mongo-php-driver`",
            file=sys.stderr,
        )
        return 2

    run_tests = _locate_run_tests(php_bin)
    if run_tests is None:
        print(
            "php_ext_validation: could not locate run-tests.php for "
            f"{php_bin} (looked under <PHP_PREFIX>/lib/php/build/). It ships "
            "with a normal PHP source install; on macOS `brew install php` "
            "provides it.",
            file=sys.stderr,
        )
        return 2

    JUNIT_OUT.parent.mkdir(parents=True, exist_ok=True)
    JUNIT_OUT.unlink(missing_ok=True)

    host = "127.0.0.1"
    port = _pick_ephemeral_port()
    storage_dir = tempfile.mkdtemp(prefix="secantus-php-ext-gauge-")
    print(
        f"php_ext_validation: starting daemon on {host}:{port} "
        f"(storage {storage_dir}, will be cleaned up)",
        file=sys.stderr,
    )

    daemon = subprocess.Popen(
        gauge_common.for_server(
            [
                sys.executable,
                "-m",
                "secantus",
                "--host",
                host,
                "--port",
                str(port),
                "--storage-path",
                storage_dir,
                "--log-level",
                "WARNING",
            ]
        ),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    try:
        _wait_for_listener(host, port)
        _verify_secantus_identity(host, port, "php_ext_validation")

        env = os.environ.copy()
        env["MONGODB_URI"] = f"mongodb://{host}:{port}/"
        env["MONGODB_DATABASE"] = TEST_DB
        env["TEST_PHP_EXECUTABLE"] = php_bin
        env["TEST_PHP_JUNIT"] = str(JUNIT_OUT)
        # Non-interactive; show diffs; don't treat skips/xfails as failures.
        env["NO_INTERACTION"] = "1"
        env["PATH"] = f"{Path(php_bin).parent}:{env.get('PATH', '')}"

        cmd = [
            php_bin,
            str(run_tests),
            "-p",
            php_bin,
            "-q",
            "-g",
            "FAIL,XFAIL,BORK,WARN,LEAK",
            *INCLUDE,
        ]
        print(
            f"php_ext_validation: `{' '.join(cmd)}` in {VENDOR} "
            f"(MONGODB_URI={env['MONGODB_URI']}, TEST_PHP_JUNIT={JUNIT_OUT})",
            file=sys.stderr,
        )
        try:
            subprocess.run(
                cmd,
                cwd=VENDOR,
                env=env,
                timeout=RUNTESTS_TIMEOUT_SECONDS,
            )
        except subprocess.TimeoutExpired:
            print(
                f"php_ext_validation: run-tests.php exceeded "
                f"{RUNTESTS_TIMEOUT_SECONDS:.0f}s wall-clock budget; killed. "
                f"Partial JUnit (if any) at {JUNIT_OUT}.",
                file=sys.stderr,
            )

        if not JUNIT_OUT.is_file() or JUNIT_OUT.stat().st_size == 0:
            print("php_ext_validation: no JUnit output (run-tests.php error?)", file=sys.stderr)
            return 1
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
