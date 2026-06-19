"""Run mongo-c-driver's ``test-libmongoc`` suite against a SecantusDB daemon.

End-to-end integration gauge for the official MongoDB **C** driver
(``libmongoc``) — the lowest-level official client, and (with the Go and
PHP-extension gauges) one of the strictest wire-protocol checks. The runner:

1. Builds the vendored driver's ``test-libmongoc`` binary once (CMake,
   ``ENABLE_TESTS=ON``), caching it under ``vendor/mongo-c-driver/_build``.
2. Spawns ``python -m secantus --host 127.0.0.1 --port <picked>
   --storage-path <tempdir>`` (or the Rust ``secantusdb`` binary, via
   ``gauge_common.for_server``) on a fresh ephemeral port.
3. Waits for the listener, verifies the ``secantus`` serverStatus marker.
4. Runs ``test-libmongoc`` over the curated ``-l`` prefixes in
   ``include_paths.py`` with ``MONGOC_TEST_URI`` pointed at the daemon,
   writing JSON results via ``-F``.
5. ``generate_report.py`` renders the JSON into
   ``docs/validation-report-c.md``.

The libmongoc test runner reads the target server from ``MONGOC_TEST_URI``
(``src/libmongoc/tests/test-libmongoc.c``), selects tests with repeatable
``-l '/Prefix/*'`` patterns, skips named tests listed in ``--skip-tests``,
and emits machine-readable JSON to ``-F <file>``
(``{"results": [{"status": "pass"|"fail"|"skip", "test_file": ...}]}``).

Run via ``uv run python -m invoke validate-c``. Requires ``cmake`` and a C
toolchain (``brew install cmake openssl@3`` on macOS; ``apt-get install
cmake libssl-dev`` on Debian/Ubuntu). The first run builds the driver
(~several minutes); later runs reuse the cached binary.
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

from .include_paths import INCLUDE, SKIP_TESTS

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-c-driver"
BUILD_DIR = VENDOR / "_build"
# test-libmongoc lands under the libmongoc subtree of the build dir.
TEST_BIN = BUILD_DIR / "src" / "libmongoc" / "test-libmongoc"
RAW_OUT = REPO_ROOT / ".validation" / f"c-raw{gauge_common.report_suffix()}.json"

# Hard wall-clock limit on the test-libmongoc invocation. A single live
# test that blocks on a cursor / getMore the server doesn't satisfy can pin
# the runner. Generous for the curated set; widen as the include list grows.
RUNTESTS_TIMEOUT_SECONDS = 900.0
# The one-time CMake configure + build can take several minutes cold.
BUILD_TIMEOUT_SECONDS = 1800.0


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


def _ensure_test_binary() -> int:
    """Build ``test-libmongoc`` if it isn't already cached. Returns a
    process-style exit code (0 ok, 2 missing toolchain, 1 build failed)."""
    if TEST_BIN.is_file():
        return 0

    cmake = shutil.which("cmake")
    if cmake is None:
        print(
            "c_validation: `cmake` not found on PATH; install it to build "
            "test-libmongoc (`brew install cmake openssl@3` on macOS, "
            "`apt-get install -y cmake libssl-dev` on Debian/Ubuntu)",
            file=sys.stderr,
        )
        return 2
    if shutil.which("cc") is None and shutil.which("clang") is None and shutil.which("gcc") is None:
        print(
            "c_validation: no C compiler found on PATH (cc / clang / gcc); "
            "install a C toolchain to build test-libmongoc",
            file=sys.stderr,
        )
        return 2

    print(
        f"c_validation: building test-libmongoc (one-time, ~several min) in {BUILD_DIR}",
        file=sys.stderr,
    )
    configure = [
        cmake,
        "-S",
        str(VENDOR),
        "-B",
        str(BUILD_DIR),
        "-DCMAKE_BUILD_TYPE=Release",
        "-DENABLE_TESTS=ON",
        # test-libmongoc links the static archives — ENABLE_TESTS requires
        # ENABLE_STATIC (mongo-c-driver CMakeLists guards this).
        "-DENABLE_STATIC=ON",
        # Skip the bits we don't exercise — trims build time and avoids
        # optional system deps (zstd/snappy, sasl, icu).
        "-DENABLE_EXAMPLES=OFF",
        "-DENABLE_SHM_COUNTERS=OFF",
        "-DBUILD_TESTING=OFF",  # the umbrella CTest harness; we run the binary directly
    ]
    build = [cmake, "--build", str(BUILD_DIR), "--target", "test-libmongoc", "--parallel"]
    try:
        r = subprocess.run(configure, timeout=BUILD_TIMEOUT_SECONDS)
        if r.returncode != 0:
            print("c_validation: cmake configure failed", file=sys.stderr)
            return 1
        r = subprocess.run(build, timeout=BUILD_TIMEOUT_SECONDS)
        if r.returncode != 0:
            print("c_validation: cmake build failed", file=sys.stderr)
            return 1
    except subprocess.TimeoutExpired:
        print(
            f"c_validation: build exceeded {BUILD_TIMEOUT_SECONDS:.0f}s budget; aborted",
            file=sys.stderr,
        )
        return 1
    if not TEST_BIN.is_file():
        print(
            f"c_validation: build completed but {TEST_BIN} is missing "
            "(target name or output layout changed?)",
            file=sys.stderr,
        )
        return 1
    return 0


def main() -> int:
    if not VENDOR.is_dir() or not (VENDOR / "CMakeLists.txt").is_file():
        print(
            f"vendor/mongo-c-driver/ missing or not initialised ({VENDOR}); "
            "run `git submodule update --init vendor/mongo-c-driver`",
            file=sys.stderr,
        )
        return 2

    rc = _ensure_test_binary()
    if rc != 0:
        return rc

    RAW_OUT.parent.mkdir(parents=True, exist_ok=True)
    RAW_OUT.unlink(missing_ok=True)

    host = "127.0.0.1"
    port = _pick_ephemeral_port()
    storage_dir = tempfile.mkdtemp(prefix="secantus-c-gauge-")
    print(
        f"c_validation: starting daemon on {host}:{port} "
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
    skip_file: Path | None = None
    try:
        _wait_for_listener(host, port)
        _verify_secantus_identity(host, port, "c_validation")

        env = os.environ.copy()
        env["MONGOC_TEST_URI"] = f"mongodb://{host}:{port}/"
        # Don't let the user's ambient libmongoc test env leak a real mongod
        # host/port through alongside the URI.
        env.pop("MONGOC_TEST_HOST", None)
        env.pop("MONGOC_TEST_PORT", None)
        # Skip the slow/large-data tests — they don't add wire-protocol
        # coverage and pad the wall clock.
        env["MONGOC_TEST_SKIP_SLOW"] = "on"

        cmd = [str(TEST_BIN), "-F", str(RAW_OUT)]
        if SKIP_TESTS:
            fd, skip_path = tempfile.mkstemp(prefix="c-gauge-skip-", suffix=".txt")
            with os.fdopen(fd, "w") as fh:
                fh.write("\n".join(SKIP_TESTS) + "\n")
            skip_file = Path(skip_path)
            cmd += ["--skip-tests", str(skip_file)]
        for pat in INCLUDE:
            cmd += ["-l", pat]

        print(
            f"c_validation: `{' '.join(cmd)}` in {VENDOR} "
            f"(MONGOC_TEST_URI={env['MONGOC_TEST_URI']}, results -> {RAW_OUT})",
            file=sys.stderr,
        )
        try:
            subprocess.run(cmd, cwd=VENDOR, env=env, timeout=RUNTESTS_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            print(
                f"c_validation: test-libmongoc exceeded "
                f"{RUNTESTS_TIMEOUT_SECONDS:.0f}s wall-clock budget; killed. "
                f"Partial JSON (if any) at {RAW_OUT}.",
                file=sys.stderr,
            )

        if not RAW_OUT.is_file() or RAW_OUT.stat().st_size == 0:
            print("c_validation: no JSON output (test-libmongoc error?)", file=sys.stderr)
            return 1
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=1)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
        shutil.rmtree(storage_dir, ignore_errors=True)
        if skip_file is not None:
            skip_file.unlink(missing_ok=True)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
