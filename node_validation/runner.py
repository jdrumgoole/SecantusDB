"""Run mongo-node-driver's tests against a standalone SecantusDB daemon.

End-to-end integration gauge: SecantusDB and the Node.js driver
exchange real wire commands over TCP. The runner:

1. Spawns ``python -m secantus --port <picked> --storage-path <tempdir>
   --standalone`` without ``--auth``; uses pymongo to ``createUser``
   ``root-user`` (``root`` role). The port is a fresh kernel-assigned
   ephemeral one so multiple gauges can run in parallel (see Phase 2
   of the parallelization plan).
2. Stops that daemon and restarts on the same tempdir+port **with
   ``--auth``** — user record persists, server now enforces auth.
3. Runs ``npx mocha --config test/mocha_mongodb.js --reporter json
   <paths>`` with ``MONGODB_URI=mongodb://root-user:password@127.0.0.1:<picked>/
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

import gauge_common

from .include_paths import INCLUDE

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "node-mongodb-native"
RAW_OUT = REPO_ROOT / ".validation" / f"node-raw{gauge_common.report_suffix()}.json"


def _pick_ephemeral_port() -> int:
    """Ask the kernel for a free ephemeral TCP port. See ``go_validation.runner``."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


ROOT_USER = "root-user"
ROOT_PASSWORD = "password"

# Hard wall-clock budget on the mocha run. mongo-node-driver
# integration tests can hang on tailable getMore polls or change-stream
# resumes when the server doesn't deliver the exact event shape they
# expect; without a guard, a single broken test would pin the runner.
MOCHA_TIMEOUT_SECONDS = 300.0

# Per-test budget passed to mocha as ``--timeout`` (in milliseconds).
# Catches single-test hangs (tailable getMore polls that never
# complete, change-stream resumes that never fire). mocha's default
# is 60s for the mongodb config; we drop it to 15s so a runaway
# test fails fast instead of pinning the whole gauge until the
# total-wall-clock guard fires.
MOCHA_PER_TEST_TIMEOUT_MS = 15_000


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
    print("node_validation: running `npm ci` (first time only, ~1-2 min)", file=sys.stderr)
    # ``npm ci`` (not ``install``) installs strictly from the committed
    # lockfile and never mutates ``package-lock.json`` — ``npm install``
    # prunes a redundant lockfile entry, dirtying the submodule and
    # breaking the zero-local-edits gauge invariant.
    proc = subprocess.run(
        ["npm", "ci", "--no-audit", "--no-fund", "--ignore-scripts"],
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

    storage_dir = tempfile.mkdtemp(prefix="secantus-node-gauge-")
    print(
        f"node_validation: storage tempdir {storage_dir} (will be cleaned up)",
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
            "--standalone",
        ]
        if with_auth:
            cmd.append("--auth")
        # Race-free spawn on a kernel-assigned port (see gauge_common.spawn_daemon).
        # Each phase binds its own port; seeded users persist via storage_dir.
        return gauge_common.spawn_daemon(cmd, label="node_validation")

    print("node_validation: phase 1 — seeding daemon (no --auth)", file=sys.stderr)
    daemon, host, port = _spawn_daemon(with_auth=False)
    try:
        _verify_secantus_identity(host, port, "node_validation")
        print("node_validation: seeding root-user", file=sys.stderr)
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

    print("node_validation: phase 2 — running gauge with --auth", file=sys.stderr)
    daemon, host, port = _spawn_daemon(with_auth=True)
    try:
        env = os.environ.copy()
        env["MONGODB_URI"] = (
            f"mongodb://{ROOT_USER}:{ROOT_PASSWORD}@{host}:{port}/?authSource=admin"
        )
        # Required by ``test/tools/runner/hooks/configuration.ts`` —
        # if AUTH != 'noauth' the bootstrap insists on the
        # ``bob:pwd123`` default URI, overriding ours. Setting AUTH=auth
        # tells the bootstrap to honour ``MONGODB_URI`` verbatim.
        env["AUTH"] = "auth"
        # TypeScript is transpiled by ``ts-node/register`` (already in
        # ``test/mocha_mongodb.js``'s ``require`` list) — CommonJS +
        # ``transpileOnly``, which resolves the driver's extensionless
        # imports (`from '../../mongodb'`) and ``import x = require(...)``
        # forms. Do **not** inject ``--experimental-strip-types``: Node's
        # strip-only loader can't transform ``import = require`` and fights
        # ts-node. On Node >= 23 the strip-types loader is on by default and
        # ts-node's own mocha config disables it, so pass it through too.

        cmd = [
            "npx",
            "mocha",
            "--config",
            "test/mocha_mongodb.js",
            "--reporter",
            "json",
            "--timeout",
            str(MOCHA_PER_TEST_TIMEOUT_MS),
            *INCLUDE,
        ]
        print(
            f"node_validation: `{' '.join(cmd)}` in {VENDOR} (MONGODB_URI={env['MONGODB_URI']})",
            file=sys.stderr,
        )
        try:
            with RAW_OUT.open("w") as out:
                proc = subprocess.run(
                    cmd,
                    cwd=VENDOR,
                    env=env,
                    stdout=out,
                    stderr=subprocess.PIPE,
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
            daemon.wait(timeout=1)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
        shutil.rmtree(storage_dir, ignore_errors=True)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
