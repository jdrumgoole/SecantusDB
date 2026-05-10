"""Run mongo-node-driver's tests against a standalone SecantusDB daemon.

End-to-end integration gauge: SecantusDB and the Node.js driver
exchange real wire commands over TCP. The runner:

1. Spawns ``python -m secantus --port 27018 --storage-path <tempdir>
   --standalone`` without ``--auth``; uses pymongo to ``createUser``
   ``root-user`` (``root`` role).
2. Stops that daemon and restarts on the same tempdir **with
   ``--auth``** — user record persists, server now enforces auth.
3. Runs ``npx mocha --config test/mocha_mongodb.js --reporter json
   <paths>`` with ``MONGODB_URI=mongodb://root-user:password@127.0.0.1:27018/
   ?authSource=admin`` so the driver authenticates against the
   freshly-seeded user.
4. ``generate_report.py`` renders the per-category breakdown into
   ``docs/validation-report-node.md``.

First run does ``npm install`` (~1-2 min). Subsequent runs reuse
``node_modules/`` and complete in seconds plus mocha startup.

Run via ``uv run python -m invoke validate-node``. Requires Node.js
>= 20 and ``npm`` on PATH.
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

from .include_paths import INCLUDE

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "node-mongodb-native"
RAW_OUT = REPO_ROOT / ".validation" / "node-raw.json"

# Project-wide convention — see CLAUDE.md ``Tooling`` section.
DAEMON_PORT = 27018

ROOT_USER = "root-user"
ROOT_PASSWORD = "password"

# Hard wall-clock budget on the mocha run. mongo-node-driver
# integration tests can hang on tailable getMore polls or change-stream
# resumes when the server doesn't deliver the exact event shape they
# expect; without a guard, a single broken test would pin the runner.
MOCHA_TIMEOUT_SECONDS = 600.0


def _wait_for_listener(host: str, port: int, timeout: float = 10.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"daemon at {host}:{port} did not become ready within {timeout}s")


def _ensure_npm_install() -> int:
    if (VENDOR / "node_modules" / "mocha").is_dir():
        return 0
    print("node_validation: running `npm install` (first time only, ~1-2 min)", file=sys.stderr)
    proc = subprocess.run(
        ["npm", "install", "--no-audit", "--no-fund", "--ignore-scripts"],
        cwd=VENDOR,
    )
    return proc.returncode


def _ensure_bundle_built() -> int:
    """Build the test bundle if test/mongodb.ts hasn't been generated.

    ``npm run build:bundle`` produces ``test/tools/runner/bundle/driver-bundle.js``
    and rewrites ``test/mongodb.ts`` to re-export from it. Integration tests
    import from ``../../mongodb`` and need this bundle.
    """
    bundle = VENDOR / "test" / "tools" / "runner" / "bundle" / "driver-bundle.js"
    if bundle.is_file():
        return 0
    print(
        "node_validation: running `npm run build:bundle` (first time only, ~30s)",
        file=sys.stderr,
    )
    proc = subprocess.run(["npm", "run", "build:bundle"], cwd=VENDOR)
    return proc.returncode


def main() -> int:
    if shutil.which("node") is None or shutil.which("npm") is None:
        print(
            "node / npm: not found on PATH; install Node.js (>= 20) to run node_validation",
            file=sys.stderr,
        )
        return 2
    if not VENDOR.is_dir() or not (VENDOR / "package.json").is_file():
        print(
            f"vendor/node-mongodb-native/ missing or not initialised "
            f"({VENDOR}); run `git submodule update --init --recursive`",
            file=sys.stderr,
        )
        return 2

    rc = _ensure_npm_install()
    if rc != 0:
        print(f"node_validation: npm install exited {rc}", file=sys.stderr)
        return rc
    rc = _ensure_bundle_built()
    if rc != 0:
        print(f"node_validation: npm run build:bundle exited {rc}", file=sys.stderr)
        return rc

    RAW_OUT.parent.mkdir(parents=True, exist_ok=True)

    host = "127.0.0.1"
    port = DAEMON_PORT

    storage_dir = tempfile.mkdtemp(prefix="secantus-node-gauge-")
    print(
        f"node_validation: storage tempdir {storage_dir} (will be cleaned up)",
        file=sys.stderr,
    )

    def _spawn_daemon(*, with_auth: bool) -> subprocess.Popen:
        cmd = [
            sys.executable, "-m", "secantus",
            "--host", host,
            "--port", str(port),
            "--storage-path", storage_dir,
            "--log-level", "WARNING",
            "--standalone",
        ]
        if with_auth:
            cmd.append("--auth")
        return subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

    print(f"node_validation: phase 1 — seeding daemon (no --auth) on {host}:{port}", file=sys.stderr)
    daemon = _spawn_daemon(with_auth=False)
    try:
        _wait_for_listener(host, port)
        print("node_validation: seeding root-user", file=sys.stderr)
        import pymongo
        client = pymongo.MongoClient(
            f"mongodb://{host}:{port}/", directConnection=True, serverSelectionTimeoutMS=5_000
        )
        client.admin.command(
            "createUser", ROOT_USER, pwd=ROOT_PASSWORD, roles=["root"]
        )
        client.close()
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=5)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()

    print(
        f"node_validation: phase 2 — running gauge with --auth on {host}:{port}",
        file=sys.stderr,
    )
    daemon = _spawn_daemon(with_auth=True)
    try:
        _wait_for_listener(host, port)

        env = os.environ.copy()
        env["MONGODB_URI"] = (
            f"mongodb://{ROOT_USER}:{ROOT_PASSWORD}@{host}:{port}/"
            f"?authSource=admin"
        )
        # Required by ``test/tools/runner/hooks/configuration.ts`` —
        # if AUTH != 'noauth' the bootstrap insists on the
        # ``bob:pwd123`` default URI, overriding ours. Setting AUTH=auth
        # tells the bootstrap to honour ``MONGODB_URI`` verbatim.
        env["AUTH"] = "auth"
        # The driver's tests use ESM-style imports without extensions
        # (`from '../../mongodb'`). Node 22+'s
        # ``--experimental-strip-types`` resolves them at runtime.
        existing = env.get("NODE_OPTIONS", "")
        env["NODE_OPTIONS"] = (
            existing + " --experimental-strip-types"
        ).strip()

        cmd = [
            "npx", "mocha",
            "--config", "test/mocha_mongodb.js",
            "--reporter", "json",
            *INCLUDE,
        ]
        print(
            f"node_validation: `{' '.join(cmd)}` in {VENDOR} "
            f"(MONGODB_URI={env['MONGODB_URI']})",
            file=sys.stderr,
        )
        try:
            with RAW_OUT.open("w") as out:
                proc = subprocess.run(
                    cmd, cwd=VENDOR, env=env, stdout=out, stderr=subprocess.PIPE,
                    timeout=MOCHA_TIMEOUT_SECONDS,
                )
            stderr = proc.stderr
        except subprocess.TimeoutExpired as exc:
            print(
                f"node_validation: mocha exceeded {MOCHA_TIMEOUT_SECONDS:.0f}s "
                "wall-clock budget; killed. Partial JSON (if any) is in "
                f"{RAW_OUT}.",
                file=sys.stderr,
            )
            stderr = exc.stderr or b""
        if stderr:
            sys.stderr.buffer.write(stderr)

        if RAW_OUT.stat().st_size == 0:
            print("node_validation: empty mocha output (build error?)", file=sys.stderr)
            return 1

        import json as _json
        try:
            raw = _json.loads(RAW_OUT.read_text())
        except _json.JSONDecodeError:
            print("node_validation: mocha JSON parse failed", file=sys.stderr)
            return 1
        stats = raw.get("stats", {})
        print(
            f"node_validation: {stats.get('passes', 0)} passed, "
            f"{stats.get('failures', 0)} failed, "
            f"{stats.get('pending', 0)} pending "
            f"({stats.get('tests', 0)} total)"
        )
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=5)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
        shutil.rmtree(storage_dir, ignore_errors=True)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
