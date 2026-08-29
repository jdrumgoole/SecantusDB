"""Run CockroachDB's pgtest wire corpus against a SecantusPGServer daemon.

1. Fetch the corpus (``pkg/sql/pgwire/testdata/pgtest``) and the upstream
   runner (``pkg/testutils/pgtest``) at the pinned commit via a sparse,
   blob-filtered clone cached under ``.validation/pgtest-checkout``.
2. Stage the runner files verbatim into the committed Go driver module
   (``pgtest_validation/go/crdbshim/pkg/testutils/pgtest`` — gitignored) and
   the include-filtered corpus into ``.validation/pgtest-corpus``.
3. For EACH corpus file, spawn a fresh daemon and run
   ``go test -json -run 'TestPGTest/^<file>$'`` with ``PGTEST_DATADIR`` /
   ``PGTEST_ADDR`` / ``PGTEST_USER``; the JSON streams are concatenated into
   ``.validation/pgtest-raw.json`` for ``generate_report.py``.

   A daemon per FILE is required, not an optimisation: the corpus assumes a
   clean database per file (several files ``CREATE TABLE t0`` / ``t`` with no
   preceding DROP), so one shared server makes later files fail on debris from
   earlier ones — ``execute`` leaves ``t0`` behind and ``prepare`` then failed
   42P07 even though it passes alone. Upstream crdb runs its own suite with
   ``WalkWithNewServer`` (a server per file) for exactly this reason; our Go
   driver can't spawn the Python daemon itself, so the loop lives here.

Run via ``uv run python -m invoke validate-pgtest``.
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

from . import CRDB_COMMIT

REPO_ROOT = Path(__file__).resolve().parent.parent
GO_DIR = Path(__file__).resolve().parent / "go"
CHECKOUT = REPO_ROOT / ".validation" / "pgtest-checkout"
CORPUS = REPO_ROOT / ".validation" / "pgtest-corpus"
RAW_OUT = REPO_ROOT / ".validation" / "pgtest-raw.json"

CRDB_REMOTE = "https://github.com/cockroachdb/cockroach"
CORPUS_PATH = "pkg/sql/pgwire/testdata/pgtest"
RUNNER_PATH = "pkg/testutils/pgtest"

GO_TEST_TIMEOUT = "600s"
SUBPROCESS_TIMEOUT_SECONDS = 1800.0


def _pick_ephemeral_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait_for_listener(host: str, port: int, timeout: float = 15.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"pg daemon at {host}:{port} did not become ready within {timeout}s")


def _verify_secantus_identity(host: str, port: int) -> None:
    import psycopg

    with psycopg.connect(
        host=host, port=port, dbname="postgres", user="postgres", connect_timeout=5
    ) as conn:
        (version,) = conn.execute("select version()").fetchone()
    if "SecantusDB" not in version:
        raise RuntimeError(
            f"daemon at {host}:{port} does not identify as SecantusDB "
            f"(version(): {version!r}) — refusing to run the gauge against it"
        )


def _git(args: list[str], cwd: Path) -> None:
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, timeout=600)


def _fetch_checkout() -> None:
    """Sparse, blob-filtered checkout of the pinned commit (cached)."""
    stamp = CHECKOUT / ".pinned-commit"
    if stamp.exists() and stamp.read_text().strip() == CRDB_COMMIT:
        return
    if CHECKOUT.exists():
        shutil.rmtree(CHECKOUT)
    CHECKOUT.mkdir(parents=True)
    _git(["init", "-q"], CHECKOUT)
    _git(["remote", "add", "origin", CRDB_REMOTE], CHECKOUT)
    _git(["config", "extensions.partialClone", "origin"], CHECKOUT)
    _git(["sparse-checkout", "set", CORPUS_PATH, RUNNER_PATH], CHECKOUT)
    _git(
        ["fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", CRDB_COMMIT],
        CHECKOUT,
    )
    _git(["checkout", "-q", "FETCH_HEAD"], CHECKOUT)
    stamp.write_text(CRDB_COMMIT + "\n")


def _stage() -> None:
    """Runner files into the Go module (verbatim); corpus filtered by EXCLUDE."""
    from .include_paths import EXCLUDE

    runner_dst = GO_DIR / "crdbshim" / RUNNER_PATH
    runner_dst.mkdir(parents=True, exist_ok=True)
    for f in ("datadriven.go", "pgtest.go"):
        shutil.copyfile(CHECKOUT / RUNNER_PATH / f, runner_dst / f)
    if CORPUS.exists():
        shutil.rmtree(CORPUS)
    CORPUS.mkdir(parents=True)
    for f in sorted((CHECKOUT / CORPUS_PATH).iterdir()):
        if f.is_file() and f.name not in EXCLUDE:
            shutil.copyfile(f, CORPUS / f.name)


def _run_pattern(name: str) -> str:
    """The ``go test -run`` pattern selecting exactly corpus file ``name``.

    Anchored on purpose: an unanchored ``TestPGTest/copy`` also selects
    ``copy_file_upload``, which would run that file against the wrong
    (already-dirtied) daemon and double-report it.
    """
    return f"TestPGTest/^{name}$"


def _corpus_files() -> list[str]:
    """The corpus file names the gauge runs, in stable (sorted) order."""
    return sorted(p.name for p in CORPUS.iterdir() if p.is_file())


def _run_one(name: str, out) -> int:
    """Run one corpus file against a FRESH daemon, appending its ``go test
    -json`` stream to ``out``. Returns the go-test exit code (1 = the file
    failed, which the report consumes; other codes are infrastructure)."""
    host = "127.0.0.1"
    port = _pick_ephemeral_port()
    storage_dir = tempfile.mkdtemp(prefix="secantus-pgtest-gauge-")
    daemon = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "secantus.sql.pgserver",
            "--host",
            host,
            "--port",
            str(port),
            "--storage-path",
            storage_dir,
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        _wait_for_listener(host, port)
        _verify_secantus_identity(host, port)
        env = {
            **os.environ,
            "PGTEST_DATADIR": str(CORPUS),
            "PGTEST_ADDR": f"{host}:{port}",
            "PGTEST_USER": "postgres",
        }
        cmd = [
            "go",
            "test",
            "-count=1",
            f"-timeout={GO_TEST_TIMEOUT}",
            "-json",
            "-run",
            _run_pattern(name),
            "./...",
        ]
        proc = subprocess.run(
            cmd, cwd=GO_DIR, env=env, stdout=out, timeout=SUBPROCESS_TIMEOUT_SECONDS
        )
        return proc.returncode
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()
        shutil.rmtree(storage_dir, ignore_errors=True)


def main() -> int:
    if shutil.which("go") is None:
        print("the pgtest gauge requires the Go toolchain (`go` on PATH)", file=sys.stderr)
        return 2

    _fetch_checkout()
    _stage()
    RAW_OUT.parent.mkdir(exist_ok=True)

    files = _corpus_files()
    if not files:
        print(f"no corpus files under {CORPUS}", file=sys.stderr)
        return 2
    worst = 0
    with RAW_OUT.open("wb") as out:
        for name in files:
            rc = _run_one(name, out)
            # A failing FILE is rc 1 and expected; anything else (build error,
            # timeout) is infrastructure and should surface as the exit code.
            worst = rc if rc not in (0, 1) else max(worst, rc)
    return worst


if __name__ == "__main__":
    raise SystemExit(main())
