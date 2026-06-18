"""Run mongo-php-library's PHPUnit suite against a SecantusDB daemon.

End-to-end integration gauge: SecantusDB and the high-level PHP library
(``mongodb/mongodb``) exchange real wire commands over TCP. The runner:

1. Spawns ``python -m secantus --host 127.0.0.1 --port <picked>
   --storage-path <tempdir>`` as a subprocess on a fresh kernel-assigned
   ephemeral port (so gauges can run in parallel).
2. Waits for the listener, verifies the ``secantus`` serverStatus marker.
3. Runs ``composer install`` once per checkout to materialise
   ``vendor/bin/phpunit`` + dependencies.
4. Runs ``vendor/bin/phpunit --log-junit <xml> <include dirs>`` with
   ``MONGODB_URI`` + ``MONGODB_DATABASE`` set — the env vars
   ``tests/TestCase.php`` reads (``getUri`` / ``getDatabaseName``).
5. ``generate_report.py`` renders the JUnit XML into
   ``docs/validation-report-php-lib.md``.

Run via ``uv run python -m invoke validate-php-lib``. Requires PHP >= 8.1
with the ``mongodb`` extension (>= 2.3) loaded and ``composer`` on PATH
(``brew install php composer`` on macOS).
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
VENDOR = REPO_ROOT / "vendor" / "mongo-php-library"
JUNIT_OUT = REPO_ROOT / ".validation" / f"php-lib-junit{gauge_common.report_suffix()}.xml"

# Hard wall-clock limit on the phpunit invocation. A single test that waits
# on a server behaviour SecantusDB doesn't emulate the way the library
# expects can otherwise pin the runner. Generous enough for the curated
# functional include set; widen as the include list grows.
PHPUNIT_TIMEOUT_SECONDS = 600.0

TEST_DB = "phplib_test"


def _pick_ephemeral_port() -> int:
    """Ask the kernel for a free ephemeral TCP port."""
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


def _resolve_php_bins() -> tuple[str, str] | None:
    """Locate ``php`` and ``composer`` on PATH (or Homebrew). Returns
    ``(php, composer)`` or ``None`` if either is missing."""
    php = shutil.which("php") or _first_existing(["/opt/homebrew/bin/php", "/usr/local/bin/php"])
    composer = shutil.which("composer") or _first_existing(
        ["/opt/homebrew/bin/composer", "/usr/local/bin/composer"]
    )
    if php and composer:
        return php, composer
    return None


def _first_existing(paths: list[str]) -> str | None:
    for p in paths:
        if Path(p).is_file():
            return p
    return None


def _has_mongodb_extension(php_bin: str) -> bool:
    proc = subprocess.run(
        [php_bin, "-m"],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    return "mongodb" in proc.stdout.lower().split()


def _ensure_composer_install(composer_bin: str, php_bin: str) -> int:
    """Run ``composer install`` if vendor/autoload.php is missing."""
    if (VENDOR / "vendor" / "autoload.php").is_file() and (
        VENDOR / "vendor" / "bin" / "phpunit"
    ).is_file():
        return 0
    print(
        "php_lib_validation: running `composer install` (first time only)",
        file=sys.stderr,
    )
    env = {**os.environ, "PATH": f"{Path(php_bin).parent}:{os.environ.get('PATH', '')}"}
    proc = subprocess.run(
        [composer_bin, "install", "--no-interaction", "--no-progress", "--quiet"],
        cwd=VENDOR,
        env=env,
    )
    return proc.returncode


def _verify_secantus_identity(host: str, port: int, gauge: str) -> None:
    """Abort unless the daemon at ``host:port`` is SecantusDB (serverStatus
    carries a ``secantus`` subdocument a real ``mongod`` never emits)."""
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
    resolved = _resolve_php_bins()
    if resolved is None:
        print(
            "php / composer: not found on PATH; install PHP (>= 8.1) with the "
            "mongodb extension and composer to run php_lib_validation "
            "(e.g. `brew install php composer` on macOS)",
            file=sys.stderr,
        )
        return 2
    php_bin, composer_bin = resolved

    if not _has_mongodb_extension(php_bin):
        print(
            f"php_lib_validation: the `mongodb` extension is not loaded in {php_bin}; "
            "install it (`pecl install mongodb` or `brew install php` ships it)",
            file=sys.stderr,
        )
        return 2

    if not VENDOR.is_dir() or not (VENDOR / "composer.json").is_file():
        print(
            f"vendor/mongo-php-library/ missing or not initialised ({VENDOR}); "
            "run `git submodule update --init vendor/mongo-php-library`",
            file=sys.stderr,
        )
        return 2

    rc = _ensure_composer_install(composer_bin, php_bin)
    if rc != 0:
        print(f"php_lib_validation: composer install exited {rc}", file=sys.stderr)
        return rc

    JUNIT_OUT.parent.mkdir(parents=True, exist_ok=True)
    # Wipe any prior JUnit so an aborted run can't masquerade as fresh.
    JUNIT_OUT.unlink(missing_ok=True)

    host = "127.0.0.1"
    port = _pick_ephemeral_port()
    storage_dir = tempfile.mkdtemp(prefix="secantus-php-lib-gauge-")
    print(
        f"php_lib_validation: starting daemon on {host}:{port} "
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
        _verify_secantus_identity(host, port, "php_lib_validation")

        env = os.environ.copy()
        env["MONGODB_URI"] = f"mongodb://{host}:{port}/?serverSelectionTimeoutMS=2000"
        env["MONGODB_DATABASE"] = TEST_DB
        env["PATH"] = f"{Path(php_bin).parent}:{env.get('PATH', '')}"

        phpunit = str(VENDOR / "vendor" / "bin" / "phpunit")
        cmd = [php_bin, phpunit, "--log-junit", str(JUNIT_OUT), *INCLUDE]
        print(
            f"php_lib_validation: `{' '.join(cmd)}` in {VENDOR} (MONGODB_URI={env['MONGODB_URI']})",
            file=sys.stderr,
        )
        try:
            subprocess.run(
                cmd,
                cwd=VENDOR,
                env=env,
                timeout=PHPUNIT_TIMEOUT_SECONDS,
            )
        except subprocess.TimeoutExpired:
            print(
                f"php_lib_validation: phpunit exceeded {PHPUNIT_TIMEOUT_SECONDS:.0f}s "
                f"wall-clock budget; killed. Partial JUnit (if any) at {JUNIT_OUT}.",
                file=sys.stderr,
            )

        if not JUNIT_OUT.is_file() or JUNIT_OUT.stat().st_size == 0:
            print("php_lib_validation: no JUnit output (build/run error?)", file=sys.stderr)
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
