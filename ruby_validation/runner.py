"""Run mongo-ruby-driver's RSpec suite against a SecantusDB daemon.

Same daemon-subprocess pattern as ``go_validation`` / ``node_validation``:

1. Spawn ``python -m secantus --port <free> --storage-path :memory:``
   as a subprocess.
2. Wait for the listener to come up.
3. Set ``MONGODB_URI`` so the driver's ``spec/support/spec_config.rb``
   reader points at our daemon.
4. Run the curated test files in ``include_paths.INCLUDE`` via
   ``bundle exec rspec --format json``.
5. Capture the JSON output to ``.validation/ruby-raw.json``;
   ``generate_report.py`` renders it to
   ``docs/validation-report-ruby.md``.

First run does ``bundle install`` (slow). Subsequent runs reuse the
installed gem cache and complete in under a minute for the lite
include set.

Run via ``uv run python -m invoke validate-ruby``.
"""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

from .include_paths import INCLUDE, SKIP_TAGS

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "mongo-ruby-driver"
RAW_OUT = REPO_ROOT / ".validation" / "ruby-raw.json"


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
    """Install gem dependencies via ``bundle install`` if needed.

    Detects "already installed" by probing for ``vendor/bundle`` (we
    install path-locally so the system gemset isn't polluted). First
    run takes ~1-2 min; subsequent runs are no-ops.
    """
    bundle_dir = VENDOR / ".bundle"
    if bundle_dir.is_dir() and (VENDOR / "Gemfile.lock").exists():
        # Already configured; verify gems are actually installed.
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
    port = _find_free_port()
    daemon_cmd = [
        sys.executable,
        "-m",
        "secantus",
        "--host",
        host,
        "--port",
        str(port),
        "--storage-path",
        ":memory:",
        "--log-level",
        "WARNING",
    ]
    print(f"ruby_validation: starting daemon on {host}:{port}", file=sys.stderr)
    daemon = subprocess.Popen(daemon_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        _wait_for_listener(host, port)

        env = os.environ.copy()
        env["MONGODB_URI"] = f"mongodb://{host}:{port}"
        env["PATH"] = f"{Path(ruby_bin).parent}:{env.get('PATH', '')}"

        # Use rspec ``--out`` so the JSON lands in a file directly.
        # Capturing stdout doesn't work — the mongo-ruby-driver's
        # Mongo::Logger writes warnings to STDOUT (the lite_spec_helper
        # sets ``STDOUT.sync = true`` for that purpose), which would
        # corrupt the JSON we tried to capture.
        cmd = [bundle_bin, "exec", "rspec", "--format", "json", "--out", str(RAW_OUT)]
        for tag in SKIP_TAGS:
            cmd.extend(["--tag", f"~{tag}"])
        cmd.extend(INCLUDE)
        print(
            f"ruby_validation: `{' '.join(cmd)}` in {VENDOR} "
            f"(MONGODB_URI={env['MONGODB_URI']})",
            file=sys.stderr,
        )
        proc = subprocess.run(
            cmd,
            cwd=VENDOR,
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        if proc.stderr:
            sys.stderr.buffer.write(proc.stderr)

        if RAW_OUT.stat().st_size == 0:
            print("ruby_validation: empty rspec output (build error?)", file=sys.stderr)
            return 1

        try:
            raw = json.loads(RAW_OUT.read_text())
        except json.JSONDecodeError:
            print("ruby_validation: rspec JSON parse failed", file=sys.stderr)
            return 1
        summary = raw.get("summary", {})
        print(
            f"ruby_validation: {summary.get('example_count', 0) - summary.get('failure_count', 0) - summary.get('pending_count', 0)} passed, "
            f"{summary.get('failure_count', 0)} failed, "
            f"{summary.get('pending_count', 0)} pending "
            f"({summary.get('example_count', 0)} total) "
            f"in {summary.get('duration', 0):.1f}s"
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
