"""Run mongo-ruby-driver's RSpec suite against a SecantusDB daemon.

End-to-end integration gauge: SecantusDB and the Ruby driver
exchange real wire commands over TCP. The runner:

1. Spawns ``python -m secantus --host 127.0.0.1 --port <picked>
   --storage-path <tempdir>`` as a subprocess. The port is a fresh
   kernel-assigned ephemeral one so multiple gauges can run in
   parallel (see Phase 2 of the parallelization plan).
2. Waits for the listener to come up.
3. Pre-provisions ``root-user`` and ``ruby-test-user`` via a setup
   pymongo client. mongo-ruby-driver's spec_helper assumes both
   exist and authenticates as them when opening
   ``authorized_client`` / ``root_authorized_client`` builders.
4. Runs ``bundle exec rspec --format json --out <raw.json>`` with
   ``MONGODB_URI=mongodb://root-user:password@127.0.0.1:20718/?authSource=admin``
   so the driver authenticates against the freshly-seeded users.
5. ``generate_report.py`` renders the per-spec breakdown into
   ``docs/validation-report-ruby.md``.

First run does ``bundle install`` (~1-2 min). Subsequent runs reuse
the gem cache and complete in seconds for the curated integration
include set.

Run via ``uv run python -m invoke validate-ruby``. Requires Ruby
>= 2.7 with bundler on PATH (``brew install ruby`` on macOS, then
prepend ``/opt/homebrew/opt/ruby/bin``).
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
from pathlib import Path

from .include_paths import INCLUDE, SKIP_TAGS

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-ruby-driver"
RAW_OUT = REPO_ROOT / ".validation" / "ruby-raw.json"

# Hard wall-clock limit on the rspec invocation. The Ruby driver's
# integration suite has tests that wait indefinitely on tailable
# cursors / change-stream getMore round-trips when the server doesn't
# return the exact event shape they expect — a single broken test can
# pin the runner forever. Kill the rspec subprocess after this many
# seconds and report the partial results that did make it to the JSON
# file. Generous enough that a clean ``database_spec.rb`` (~30 s)
# completes with comfortable margin; widen as the include set grows.
RSPEC_TIMEOUT_SECONDS = 300.0

# Test users mongo-ruby-driver's spec/support/spec_config.rb expects.
# Mirrors the names in their ``rake spec:prepare`` step. The roles
# match what the test_user provisioning code in spec_setup.rb wants.
ROOT_USER = "root-user"
ROOT_PASSWORD = "password"
TEST_USER = "ruby-test-user"
TEST_PASSWORD = "password"
TEST_DB = "ruby-driver"


def _pick_ephemeral_port() -> int:
    """Ask the kernel for a free ephemeral TCP port. See ``go_validation.runner``."""
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


def _resolve_ruby_bin() -> tuple[str, str] | None:
    """Locate a Ruby >= 2.7 with bundler on the PATH or via Homebrew.

    System Ruby on macOS is 2.6, which mongo-ruby-driver 2.24 doesn't
    support. Prefer brew's ``/opt/homebrew/opt/ruby/bin`` when present.
    Returns ``(ruby_path, bundle_path)`` or ``None`` if no usable ruby
    can be found.
    """
    candidates = [
        "/opt/homebrew/opt/ruby/bin",
        "/usr/local/opt/ruby/bin",
    ]
    for c in candidates:
        ruby = Path(c) / "ruby"
        bundle = Path(c) / "bundle"
        if ruby.is_file() and bundle.is_file():
            return str(ruby), str(bundle)

    ruby = shutil.which("ruby")
    bundle = shutil.which("bundle") or shutil.which("bundler")
    if ruby and bundle:
        return ruby, bundle
    return None


def _ensure_bundle_install(bundle_bin: str, ruby_bin: str) -> int:
    """Install gem dependencies via ``bundle install`` if needed."""
    bundle_dir = VENDOR / ".bundle"
    if bundle_dir.is_dir() and (VENDOR / "Gemfile.lock").exists():
        check = subprocess.run(
            [bundle_bin, "check"],
            cwd=VENDOR,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            env={**os.environ, "PATH": f"{Path(ruby_bin).parent}:{os.environ.get('PATH', '')}"},
        )
        if check.returncode == 0:
            return 0
    print(
        "ruby_validation: running `bundle install` (first time only, ~1-2 min)",
        file=sys.stderr,
    )
    env = {**os.environ, "PATH": f"{Path(ruby_bin).parent}:{os.environ.get('PATH', '')}"}
    proc = subprocess.run(
        [bundle_bin, "install", "--quiet"],
        cwd=VENDOR,
        env=env,
    )
    return proc.returncode


def _seed_users(host: str, port: int) -> None:
    """Pre-create root-user + ruby-test-user via a pymongo setup client.

    The Ruby driver's ``spec_helper`` opens an ``authorized_client``
    that SCRAM-authenticates as one of these. SecantusDB's SCRAM
    handler validates against actual stored credentials, so the users
    must exist before rspec invokes any per-test client. We do this
    against an unauthenticated daemon (no ``--auth`` flag) so the
    ``createUser`` commands themselves don't need credentials.
    """
    import pymongo  # delayed import: pymongo isn't a hard runner-side dep

    client = pymongo.MongoClient(
        f"mongodb://{host}:{port}/", directConnection=True, serverSelectionTimeoutMS=5_000
    )
    admin = client.admin
    # ``root`` covers user-admin + cluster-admin + read/write on every
    # db, matching the scope of mongo-ruby-driver's hardcoded root_user.
    admin.command("createUser", ROOT_USER, pwd=ROOT_PASSWORD, roles=["root"])
    # The test user needs read/write on the primary test database +
    # the various per-feature dbs spec_config.rb names. Granting
    # ``readWriteAnyDatabase`` + ``dbAdminAnyDatabase`` covers them
    # all without enumerating each one — driver tests don't actually
    # rely on the specific role grants, just on having sufficient
    # privilege.
    admin.command(
        "createUser",
        TEST_USER,
        pwd=TEST_PASSWORD,
        roles=["readWriteAnyDatabase", "dbAdminAnyDatabase"],
    )
    client.close()


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
    resolved = _resolve_ruby_bin()
    if resolved is None:
        print(
            "ruby / bundle: not found on PATH; install Ruby (>= 2.7) with bundler "
            "to run ruby_validation (e.g. `brew install ruby` on macOS)",
            file=sys.stderr,
        )
        return 2
    ruby_bin, bundle_bin = resolved

    if not VENDOR.is_dir() or not (VENDOR / "mongo.gemspec").is_file():
        print(
            f"vendor/mongo-ruby-driver/ missing or not initialised "
            f"({VENDOR}); run `git submodule update --init --recursive`",
            file=sys.stderr,
        )
        return 2
    if not (VENDOR / "spec" / "shared" / "lib").is_dir():
        print(
            "vendor/mongo-ruby-driver/spec/shared/ is empty (nested submodule). "
            "Run `git submodule update --init --recursive` from the repo root.",
            file=sys.stderr,
        )
        return 2

    rc = _ensure_bundle_install(bundle_bin, ruby_bin)
    if rc != 0:
        print(f"ruby_validation: bundle install exited {rc}", file=sys.stderr)
        return rc

    RAW_OUT.parent.mkdir(parents=True, exist_ok=True)

    host = "127.0.0.1"
    port = _pick_ephemeral_port()

    # Use an on-disk tempdir (NOT ``:memory:``) so the user records we
    # seed survive the auth-mode flip below. ``:memory:`` would lose
    # them when the daemon stops, defeating the whole point of running
    # the auth-on phase against pre-provisioned users.
    storage_dir = tempfile.mkdtemp(prefix="secantus-ruby-gauge-")
    print(
        f"ruby_validation: storage tempdir {storage_dir} (will be cleaned up)",
        file=sys.stderr,
    )

    def _spawn_daemon(*, with_auth: bool) -> subprocess.Popen:
        cmd = [
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
        if with_auth:
            cmd.append("--auth")
        return subprocess.Popen(
            cmd,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )

    print(f"ruby_validation: phase 1 — seeding daemon (no --auth) on {host}:{port}", file=sys.stderr)
    daemon = _spawn_daemon(with_auth=False)
    try:
        _wait_for_listener(host, port)
        _verify_secantus_identity(host, port, "ruby_validation")
        print("ruby_validation: seeding root-user + ruby-test-user", file=sys.stderr)
        _seed_users(host, port)
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=1)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()

    print(
        f"ruby_validation: phase 2 — running gauge with --auth on {host}:{port}",
        file=sys.stderr,
    )
    daemon = _spawn_daemon(with_auth=True)
    try:
        _wait_for_listener(host, port)

        env = os.environ.copy()
        env["MONGODB_URI"] = (
            f"mongodb://{ROOT_USER}:{ROOT_PASSWORD}@{host}:{port}/?authSource=admin"
        )
        env["PATH"] = f"{Path(ruby_bin).parent}:{env.get('PATH', '')}"

        # Use rspec ``--out`` so JSON lands in a file directly. The
        # mongo-ruby-driver's ``Mongo::Logger`` writes warnings to
        # STDOUT, which would corrupt the JSON if we captured stdout.
        cmd = [bundle_bin, "exec", "rspec", "--format", "json", "--out", str(RAW_OUT)]
        for tag in SKIP_TAGS:
            cmd.extend(["--tag", f"~{tag}"])
        cmd.extend(INCLUDE)
        print(
            f"ruby_validation: `{' '.join(cmd)}` in {VENDOR} "
            f"(MONGODB_URI={env['MONGODB_URI']})",
            file=sys.stderr,
        )
        try:
            proc = subprocess.run(
                cmd,
                cwd=VENDOR,
                env=env,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                timeout=RSPEC_TIMEOUT_SECONDS,
            )
            stderr = proc.stderr
        except subprocess.TimeoutExpired as exc:
            print(
                f"ruby_validation: rspec exceeded "
                f"{RSPEC_TIMEOUT_SECONDS:.0f}s wall-clock budget; "
                "killed. Partial results (if any) are in "
                f"{RAW_OUT}.",
                file=sys.stderr,
            )
            stderr = exc.stderr or b""
        if stderr:
            sys.stderr.buffer.write(stderr)

        if RAW_OUT.stat().st_size == 0:
            print("ruby_validation: empty rspec output (build error?)", file=sys.stderr)
            return 1

        try:
            raw = json.loads(RAW_OUT.read_text())
        except json.JSONDecodeError:
            print("ruby_validation: rspec JSON parse failed", file=sys.stderr)
            return 1
        summary = raw.get("summary", {})
        passed = (
            summary.get("example_count", 0)
            - summary.get("failure_count", 0)
            - summary.get("pending_count", 0)
        )
        print(
            f"ruby_validation: {passed} passed, "
            f"{summary.get('failure_count', 0)} failed, "
            f"{summary.get('pending_count', 0)} pending "
            f"({summary.get('example_count', 0)} total) "
            f"in {summary.get('duration', 0):.1f}s"
        )
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=1)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
        # Storage tempdir is single-run scratch — drop it so we don't
        # leak a wiredtiger directory per gauge invocation.
        shutil.rmtree(storage_dir, ignore_errors=True)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
