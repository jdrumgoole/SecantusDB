"""Run mongo-csharp-driver's xUnit suite against a SecantusDB daemon.

End-to-end integration gauge for the official MongoDB **C# / .NET** driver. The
runner:

1. Spawns ``python -m secantus`` (or the Rust ``secantusdb`` binary, via
   ``gauge_common.for_server``) on a fresh ephemeral port.
2. Waits for the listener, verifies the ``secantus`` serverStatus marker.
3. Runs the curated ``MongoDB.Driver.Tests`` xUnit project via ``dotnet test``
   with ``MONGODB_URI`` pointed at the daemon and the in-scope ``--filter`` from
   ``include_paths.py``, writing TRX results to ``.validation/dotnet-raw.trx``.
4. ``generate_report.py`` renders the TRX into ``docs/validation-report-dotnet.md``.

mongo-csharp-driver's tests read the server connection string from the
``MONGODB_URI`` environment variable (``CoreTestConfiguration``), and the
``[RequireServer]`` xUnit attribute self-skips tests whose server-version /
topology requirements SecantusDB doesn't meet.

Run via ``uv run python -m invoke validate-dotnet``. Requires the .NET SDK
(``brew install dotnet``); the test project targets ``net10.0``. The first run
restores NuGet packages and builds (~several min); later runs reuse the build.
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

from .include_paths import FRAMEWORK, FILTER, TEST_PROJECT

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-csharp-driver"
CSPROJ = VENDOR / TEST_PROJECT
RESULTS_DIR = REPO_ROOT / ".validation"
RAW_NAME = f"dotnet-raw{gauge_common.report_suffix()}.trx"
RAW_OUT = RESULTS_DIR / RAW_NAME

# `dotnet test` restore+build+run. The curated CRUD-spec filter keeps the run
# bounded, but the cold restore+build dominates the first invocation.
RUNTESTS_TIMEOUT_SECONDS = 1800.0


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


def _resolve_dotnet() -> str | None:
    return shutil.which("dotnet") or next(
        (p for p in ("/opt/homebrew/bin/dotnet", "/usr/local/bin/dotnet") if Path(p).is_file()),
        None,
    )


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
    dotnet = _resolve_dotnet()
    if dotnet is None:
        print(
            "dotnet: not found on PATH; install the .NET SDK to run "
            "dotnet_validation (`brew install dotnet` on macOS, or "
            "https://dotnet.microsoft.com/download)",
            file=sys.stderr,
        )
        return 2
    if not CSPROJ.is_file():
        print(
            f"vendor/mongo-csharp-driver/ missing or not initialised ({CSPROJ}); "
            "run `git submodule update --init vendor/mongo-csharp-driver`",
            file=sys.stderr,
        )
        return 2

    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    RAW_OUT.unlink(missing_ok=True)

    host = "127.0.0.1"
    port = _pick_ephemeral_port()
    storage_dir = tempfile.mkdtemp(prefix="secantus-dotnet-gauge-")
    print(
        f"dotnet_validation: starting daemon on {host}:{port} "
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
        _verify_secantus_identity(host, port, "dotnet_validation")

        env = os.environ.copy()
        env["MONGODB_URI"] = f"mongodb://{host}:{port}/"
        env["DOTNET_CLI_TELEMETRY_OPTOUT"] = "1"
        env["DOTNET_NOLOGO"] = "1"
        env["PATH"] = f"{Path(dotnet).parent}:{env.get('PATH', '')}"

        cmd = [
            dotnet,
            "test",
            str(CSPROJ),
            "-c",
            "Release",
            "-f",
            FRAMEWORK,
            "--logger",
            f"trx;LogFileName={RAW_OUT}",
            "--results-directory",
            str(RESULTS_DIR),
        ]
        if FILTER:
            cmd += ["--filter", FILTER]
        print(
            f"dotnet_validation: `{' '.join(cmd)}` (MONGODB_URI={env['MONGODB_URI']})",
            file=sys.stderr,
        )
        try:
            subprocess.run(cmd, cwd=VENDOR, env=env, timeout=RUNTESTS_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            print(
                f"dotnet_validation: dotnet test exceeded "
                f"{RUNTESTS_TIMEOUT_SECONDS:.0f}s wall-clock budget; killed. "
                f"Partial TRX (if any) at {RAW_OUT}.",
                file=sys.stderr,
            )

        if not RAW_OUT.is_file() or RAW_OUT.stat().st_size == 0:
            print("dotnet_validation: no TRX output (dotnet test error?)", file=sys.stderr)
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
