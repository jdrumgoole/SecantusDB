"""Run mongo-node-driver's tests against a standalone SecantusDB daemon.

Same shape as `go_validation/runner.py`: spawn SecantusDB as a
subprocess, set `MONGODB_URI` (and `AUTH=noauth` so the driver's
test bootstrap doesn't fall back to the auth-enabled default), then
run the in-scope mocha test paths.

mocha's JSON reporter writes the full test result to stdout as one
big JSON document. We capture it to `.validation/node-raw.json` and
let `generate_report.py` turn it into `docs/validation-report-node.md`.

First run is slow because `npm install` has to fetch the driver's
dev dependencies (~minute). Subsequent runs reuse the installed
node_modules and complete in seconds.

Run via `uv run python -m invoke validate-node`.
"""

from __future__ import annotations

import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

from .include_paths import INCLUDE

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "node-mongodb-native"
RAW_OUT = REPO_ROOT / ".validation" / "node-raw.json"


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


def _ensure_npm_install() -> int:
    """Install node-mongodb-native's dev deps if node_modules is missing."""
    if (VENDOR / "node_modules" / "mocha").is_dir():
        return 0
    print("node_validation: running `npm install` (first time only, ~1-2 min)", file=sys.stderr)
    proc = subprocess.run(
        ["npm", "install", "--no-audit", "--no-fund", "--ignore-scripts"],
        cwd=VENDOR,
    )
    return proc.returncode


def _ensure_bundle_built() -> int:
    """Build the test bundle if test/mongodb.ts hasn't been generated yet.

    `npm run build:bundle` produces test/tools/runner/bundle/driver-bundle.js
    and rewrites test/mongodb.ts to re-export from it. The unit tests
    import `test/mongodb` so without this step every test file fails to
    resolve at module load.
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
    port = _find_free_port()
    daemon_cmd = [
        sys.executable, "-m", "secantus",
        "--host", host,
        "--port", str(port),
        "--storage-path", ":memory:",
        "--log-level", "WARNING",
    ]
    print(f"node_validation: starting daemon on {host}:{port}", file=sys.stderr)
    daemon = subprocess.Popen(
        daemon_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE
    )
    try:
        _wait_for_listener(host, port)

        env = os.environ.copy()
        env["MONGODB_URI"] = f"mongodb://{host}:{port}"
        # Without this, the test bootstrap defaults the URI to
        # `mongodb://bob:pwd123@localhost:27017` (auth=auth).
        env["AUTH"] = "noauth"
        # The driver's tests use ESM-style `import` statements without
        # extensions (`from '../../mongodb'`). ts-node/register from .mocharc
        # only handles CJS; ESM resolution refuses to auto-pick `.ts`.
        # `--experimental-strip-types` (Node 22.6+) strips types at runtime
        # so the imports resolve. Append rather than overwrite NODE_OPTIONS
        # so user-supplied options (debug flags, etc.) survive.
        existing = env.get("NODE_OPTIONS", "")
        env["NODE_OPTIONS"] = (
            existing + " --experimental-strip-types"
        ).strip()

        cmd = ["npx", "mocha", "--reporter", "json", *INCLUDE]
        print(
            f"node_validation: `{' '.join(cmd)}` in {VENDOR} "
            f"(MONGODB_URI={env['MONGODB_URI']})",
            file=sys.stderr,
        )
        with RAW_OUT.open("w") as out:
            proc = subprocess.run(
                cmd, cwd=VENDOR, env=env, stdout=out, stderr=subprocess.PIPE
            )
        if proc.stderr:
            sys.stderr.buffer.write(proc.stderr)

        if RAW_OUT.stat().st_size == 0:
            print(
                "node_validation: empty mocha output (build error?)", file=sys.stderr
            )
            return 1

        # Quick one-line summary; full breakdown lives in the markdown report.
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

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
