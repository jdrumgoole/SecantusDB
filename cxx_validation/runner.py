"""Run mongo-cxx-driver's Catch2 test suite against a SecantusDB daemon.

End-to-end integration gauge for the official MongoDB **C++** driver
(``mongocxx``). The runner:

1. Builds and installs the vendored libmongoc (the C driver mongocxx links)
   into ``vendor/mongo-c-driver/_install`` if not already present.
2. Configures and builds the vendored mongocxx ``test_driver`` Catch2 binary
   against that install (cached under ``vendor/mongo-cxx-driver/_build``).
3. Binds a SecantusDB daemon on ``127.0.0.1:27017`` — mongocxx's core tests
   construct ``client{uri{}}``, hard-wired to ``mongodb://localhost:27017``
   with **no** env override (unlike libmongoc's ``MONGOC_TEST_URI``), so the
   gauge must serve the driver's default port. If something already holds
   27017 the gauge refuses to run (it won't gauge a foreign server).
4. Runs each curated Catch2 binary with the JUnit reporter, writing
   ``.validation/cxx-raw.xml``.
5. ``generate_report.py`` renders the JUnit into
   ``docs/validation-report-cxx.md``.

Run via ``uv run python -m invoke validate-cxx``. Requires ``cmake``, a C++17
toolchain, and OpenSSL (transitively, via libmongoc). The first run builds the
C driver + mongocxx (~10-15 min); later runs reuse the cached builds.
"""

from __future__ import annotations

import os
import shutil
import socket
import subprocess
import sys
import tempfile
from pathlib import Path

import gauge_common

from .include_paths import EXCLUDE_SPECS, TEST_BINARIES

REPO_ROOT = Path(__file__).resolve().parent.parent
C_VENDOR = REPO_ROOT / "vendor" / "mongo-c-driver"
C_INSTALL = C_VENDOR / "_install"
C_INSTALL_BUILD = C_VENDOR / "_install_build"
CXX_VENDOR = REPO_ROOT / "vendor" / "mongo-cxx-driver"
CXX_BUILD = CXX_VENDOR / "_build"
TEST_DIR = CXX_BUILD / "src" / "mongocxx" / "test"
RAW_OUT = REPO_ROOT / ".validation" / f"cxx-raw{gauge_common.report_suffix()}.xml"

# mongocxx's core tests connect to the driver default — 127.0.0.1:27017.
MONGOCXX_PORT = 27017

RUNTESTS_TIMEOUT_SECONDS = 900.0
BUILD_TIMEOUT_SECONDS = 2400.0


def _wait_for_listener(host: str, port: int, timeout: float = 10.0) -> None:
    import time

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"daemon at {host}:{port} did not become ready within {timeout}s")


def _port_is_free(host: str, port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        try:
            s.bind((host, port))
            return True
        except OSError:
            return False


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


def _toolchain_ok() -> int | None:
    """Return None if cmake + a C/C++ compiler are present, else an exit code."""
    if shutil.which("cmake") is None:
        print(
            "cxx_validation: `cmake` not found on PATH; install it (and a C++17 "
            "toolchain + OpenSSL) to build mongocxx (`brew install cmake "
            "openssl@3` on macOS, `apt-get install -y cmake libssl-dev` on "
            "Debian/Ubuntu)",
            file=sys.stderr,
        )
        return 2
    if not any(shutil.which(cc) for cc in ("c++", "clang++", "g++")):
        print(
            "cxx_validation: no C++ compiler found on PATH (c++ / clang++ / g++)",
            file=sys.stderr,
        )
        return 2
    return None


def _run_build(cmd: list[str], label: str) -> int:
    try:
        r = subprocess.run(cmd, timeout=BUILD_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        print(f"cxx_validation: {label} exceeded {BUILD_TIMEOUT_SECONDS:.0f}s budget", file=sys.stderr)
        return 1
    if r.returncode != 0:
        print(f"cxx_validation: {label} failed", file=sys.stderr)
    return r.returncode


def _ensure_c_driver_install() -> int:
    """Build + install libmongoc into ``_install`` if its CMake config is absent."""
    if (C_INSTALL / "lib" / "cmake" / "mongoc-1.0").is_dir():
        return 0
    cmake = shutil.which("cmake")
    assert cmake  # guarded by _toolchain_ok
    print(f"cxx_validation: building + installing libmongoc into {C_INSTALL}", file=sys.stderr)
    rc = _run_build(
        [
            cmake,
            "-S",
            str(C_VENDOR),
            "-B",
            str(C_INSTALL_BUILD),
            "-DCMAKE_BUILD_TYPE=Release",
            "-DENABLE_TESTS=OFF",
            "-DENABLE_STATIC=OFF",
            "-DENABLE_EXAMPLES=OFF",
            "-DENABLE_SHM_COUNTERS=OFF",
            "-DBUILD_TESTING=OFF",
            f"-DCMAKE_INSTALL_PREFIX={C_INSTALL}",
        ],
        "libmongoc configure",
    )
    if rc != 0:
        return 1
    return _run_build(
        [cmake, "--build", str(C_INSTALL_BUILD), "--target", "install", "--parallel"],
        "libmongoc install",
    ) and 1 or 0


def _ensure_test_binaries() -> int:
    """Configure + build the curated mongocxx Catch2 binaries if absent."""
    missing = [b for b in TEST_BINARIES if not (TEST_DIR / b).is_file()]
    if not missing:
        return 0
    cmake = shutil.which("cmake")
    assert cmake
    if not CXX_BUILD.exists() or not (CXX_BUILD / "CMakeCache.txt").is_file():
        print(f"cxx_validation: configuring mongocxx in {CXX_BUILD}", file=sys.stderr)
        rc = _run_build(
            [
                cmake,
                "-S",
                str(CXX_VENDOR),
                "-B",
                str(CXX_BUILD),
                "-DCMAKE_BUILD_TYPE=Release",
                f"-DCMAKE_PREFIX_PATH={C_INSTALL}",
                "-DCMAKE_CXX_STANDARD=17",
                "-DENABLE_TESTS=ON",
                "-DBUILD_SHARED_LIBS=ON",
                "-DBUILD_TESTING=OFF",
            ],
            "mongocxx configure",
        )
        if rc != 0:
            return 1
    print(f"cxx_validation: building {', '.join(missing)} (one-time, several min)", file=sys.stderr)
    for binary in missing:
        rc = _run_build(
            [cmake, "--build", str(CXX_BUILD), "--target", binary, "--parallel"],
            f"build {binary}",
        )
        if rc != 0:
            return 1
    still_missing = [b for b in TEST_BINARIES if not (TEST_DIR / b).is_file()]
    if still_missing:
        print(
            f"cxx_validation: built but missing {still_missing} under {TEST_DIR} "
            "(target name / layout changed?)",
            file=sys.stderr,
        )
        return 1
    return 0


def _runtime_env() -> dict[str, str]:
    """Env with the C-driver + mongocxx/bsoncxx shared libs on the loader path.

    The build-tree rpaths usually suffice, but set the search paths too so the
    binary loads regardless of how CMake configured rpath on the platform.
    """
    env = os.environ.copy()
    lib_dirs = [
        str(C_INSTALL / "lib"),
        str(CXX_BUILD / "src" / "mongocxx"),
        str(CXX_BUILD / "src" / "bsoncxx"),
        str(CXX_BUILD / "_deps" / "catch2-build" / "src"),
    ]
    joined = os.pathsep.join(d for d in lib_dirs if Path(d).is_dir())
    for var in ("DYLD_LIBRARY_PATH", "LD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"):
        env[var] = os.pathsep.join(filter(None, [joined, env.get(var, "")]))
    return env


def main() -> int:
    if not CXX_VENDOR.is_dir() or not (CXX_VENDOR / "CMakeLists.txt").is_file():
        print(
            f"vendor/mongo-cxx-driver/ missing or not initialised ({CXX_VENDOR}); "
            "run `git submodule update --init vendor/mongo-cxx-driver`",
            file=sys.stderr,
        )
        return 2
    if not C_VENDOR.is_dir() or not (C_VENDOR / "CMakeLists.txt").is_file():
        print(
            f"vendor/mongo-c-driver/ missing ({C_VENDOR}); mongocxx links libmongoc — "
            "run `git submodule update --init vendor/mongo-c-driver`",
            file=sys.stderr,
        )
        return 2

    tc = _toolchain_ok()
    if tc is not None:
        return tc
    rc = _ensure_c_driver_install()
    if rc != 0:
        return rc
    rc = _ensure_test_binaries()
    if rc != 0:
        return rc

    host = "127.0.0.1"
    if not _port_is_free(host, MONGOCXX_PORT):
        print(
            f"cxx_validation: {host}:{MONGOCXX_PORT} is already in use. mongocxx's "
            "tests hard-wire the driver default port and can't be redirected, so "
            "the gauge needs 27017 free to bind its own SecantusDB daemon. Stop "
            "whatever is listening there (e.g. a real mongod) and retry.",
            file=sys.stderr,
        )
        return 2

    RAW_OUT.parent.mkdir(parents=True, exist_ok=True)
    RAW_OUT.unlink(missing_ok=True)

    storage_dir = tempfile.mkdtemp(prefix="secantus-cxx-gauge-")
    print(
        f"cxx_validation: starting daemon on {host}:{MONGOCXX_PORT} "
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
                str(MONGOCXX_PORT),
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
        _wait_for_listener(host, MONGOCXX_PORT)
        _verify_secantus_identity(host, MONGOCXX_PORT, "cxx_validation")

        env = _runtime_env()
        # Some test cases (search-index probes etc.) read MONGODB_URI directly;
        # point it at the daemon too even though the core tests use uri{}.
        env["MONGODB_URI"] = f"mongodb://{host}:{MONGOCXX_PORT}/"

        suites: list[Path] = []
        for i, binary in enumerate(TEST_BINARIES):
            out = RAW_OUT if len(TEST_BINARIES) == 1 else RAW_OUT.with_suffix(f".{binary}.xml")
            cmd = [str(TEST_DIR / binary), "--reporter", "junit", "--out", str(out), *EXCLUDE_SPECS]
            print(f"cxx_validation: `{' '.join(cmd)}` (MONGODB_URI={env['MONGODB_URI']})", file=sys.stderr)
            try:
                subprocess.run(cmd, cwd=TEST_DIR, env=env, timeout=RUNTESTS_TIMEOUT_SECONDS)
            except subprocess.TimeoutExpired:
                print(
                    f"cxx_validation: {binary} exceeded {RUNTESTS_TIMEOUT_SECONDS:.0f}s; killed. "
                    f"Partial JUnit (if any) at {out}.",
                    file=sys.stderr,
                )
            if out.is_file() and out.stat().st_size > 0:
                suites.append(out)

        if not RAW_OUT.is_file() or RAW_OUT.stat().st_size == 0:
            print("cxx_validation: no JUnit output (test binary error?)", file=sys.stderr)
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
