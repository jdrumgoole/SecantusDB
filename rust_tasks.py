"""Invoke tasks for the Rust side of SecantusDB.

Imported by the root ``tasks.py`` (``from rust_tasks import *``) so every
``rust-*`` task — plus ``rust-gate`` (the full pre-commit sequence) and
``rust-ship`` (gate → commit → push) — is available as ``invoke rust-…`` without
hand-assembling the underlying commands.

The Rust side is a Cargo workspace under ``crates/``:
  * ``crates/secantus-core``    — pure-Rust operator engines (no PyO3).
  * ``crates/secantus-core-py`` — PyO3 bindings → the ``_secantus_core`` abi3
                                  extension (the ``secantus-core`` wheel).
The WiredTiger-linked crates (``-wt`` / ``-storage`` / ``-storage-adapter`` /
``secantusdb`` / the embedded ``_secantus_server``) are excluded from the clean
workspace, so they fmt/clippy/test on their own manifests — see the per-crate
tasks below. Plan / status: tasks/rust-server-plan.md, tasks/rust-rewrite-plan.md.
"""

from __future__ import annotations

import glob
import os
import pathlib
import re
import shlex

from invoke.context import Context
from invoke.tasks import task

_RUST_WORKSPACE_DIR = "crates"
_RUST_BINDINGS_DIR = "crates/secantus-core-py"
_RUST_WT_DIR = "crates/secantus-wt"
_RUST_STORAGE_DIR = "crates/secantus-storage"
_RUST_ADAPTER_DIR = "crates/secantus-storage-adapter"
_RUST_STORAGE_PY_DIR = "crates/secantus-storage-py"
_RUST_BINARY_DIR = "crates/secantusdb"

# (No default deselect. The PITR cross-server tests once appeared to be a
# local-only failure and were deselected; the real cause was a connection-drain
# bug in the Rust server's stop() — its final WiredTiger checkpoint raced the
# data-dir reopen under load. Fixed in 0.5.3-beta.48, so the full suite is green
# under -n auto. Per CLAUDE.md "Never ignore or discount an error", we do not
# carry a standing deselect — pass --deselect explicitly if you ever need one.)


def _rust_env() -> dict[str, str]:
    """Build-environment defaults for the WiredTiger-linking Rust crates.

    The WT-linked crates (``secantus-wt`` / ``-storage`` / ``-storage-adapter`` /
    ``secantusdb`` / the embedded ``_secantus_server``) need a vendored
    WiredTiger build dir and libclang for bindgen. CI exports these explicitly;
    locally they're easy to forget, so this fills in the conventional values
    (``/tmp/wt-build`` + Xcode's libclang) **only** when the caller hasn't
    already set them. Returns a dict to hand to ``c.run(..., env=...)`` (invoke
    merges it over the inherited environment).
    """
    env: dict[str, str] = {}
    if not os.environ.get("WT_BUILD_DIR") and pathlib.Path("/tmp/wt-build").exists():
        env["WT_BUILD_DIR"] = "/tmp/wt-build"
    if not os.environ.get("LIBCLANG_PATH"):
        for cand in (
            "/Library/Developer/CommandLineTools/usr/lib",  # macOS / Xcode CLT
            "/usr/lib/llvm-14/lib",  # common Linux
        ):
            if pathlib.Path(cand).exists():
                env["LIBCLANG_PATH"] = cand
                break
    return env


@task(name="rust-test")
def rust_test(c: Context) -> None:
    """cargo fmt --check, clippy (warnings-as-errors), unit tests (whole workspace)."""
    c.run(f"cd {_RUST_WORKSPACE_DIR} && cargo fmt --check", pty=True)
    c.run(f"cd {_RUST_WORKSPACE_DIR} && cargo clippy --all-targets -- -D warnings", pty=True)
    c.run(f"cd {_RUST_WORKSPACE_DIR} && cargo test", pty=True)


@task(name="rust-build")
def rust_build(c: Context) -> None:
    """Build the abi3 wheel for the Rust core into target/wheels/."""
    c.run(
        f"cd {_RUST_BINDINGS_DIR} && uv tool run maturin build --release",
        pty=True,
        env={"VIRTUAL_ENV": ""},
    )


@task(name="rust-parity")
def rust_parity(c: Context) -> None:
    """Build the Rust core and run the leaf-engine parity suites against it.

    Builds the extension, then runs the parity tests (sortkey + query) in an
    isolated interpreter (pymongo + the freshly built wheel) so they do not
    require the WiredTiger C extension to be installed. This mirrors how the
    parity gate runs in a WiredTiger-less environment; full CI also runs them
    via the normal pytest suite once the project wheel is built.

    ``--reinstall-package`` busts uv's cache: the wheel keeps the same
    name/version across rebuilds, so without it a stale build would be reused.
    """
    c.run(
        f"cd {_RUST_BINDINGS_DIR} && uv tool run maturin build --release --out dist",
        pty=True,
    )
    wheels = sorted(glob.glob(f"{_RUST_BINDINGS_DIR}/dist/*.whl"))
    if not wheels:
        raise SystemExit("no wheel produced by maturin")
    c.run(
        "uv run --no-project --reinstall-package secantus-core "
        f"--with pymongo --with pytest --with {shlex.quote(wheels[-1])} "
        "python -m pytest tests/test_rust_sortkey_parity.py tests/test_rust_query_parity.py "
        "tests/test_rust_update_parity.py tests/test_rust_expressions_parity.py "
        "tests/test_rust_projection_parity.py tests/test_rust_diff_parity.py "
        "tests/test_rust_aggregate_parity.py "
        "-o addopts= -p no:cacheprovider -q",
        pty=True,
    )


@task(name="rust-wt-test")
def rust_wt_test(c: Context) -> None:
    """fmt/clippy/test the secantus-wt WiredTiger FFI crate.

    Needs WiredTiger present: either set SECANTUS_WT_INCLUDE / SECANTUS_WT_LIB,
    or have it under build/*/wt-build (the project CMake output) or /tmp/wt-build.
    bindgen needs libclang (set LIBCLANG_PATH if not auto-found; ``_rust_env``
    fills in the conventional values when unset).
    """
    env = _rust_env()
    c.run(f"cd {_RUST_WT_DIR} && cargo fmt --check", pty=True, env=env)
    c.run(f"cd {_RUST_WT_DIR} && cargo clippy --all-targets -- -D warnings", pty=True, env=env)
    c.run(f"cd {_RUST_WT_DIR} && cargo test", pty=True, env=env)


@task(name="rust-storage-test")
def rust_storage_test(c: Context) -> None:
    """fmt/clippy/test the secantus-storage crate (the Rust Storage layer).

    Same WiredTiger / libclang prerequisites as ``rust-wt-test`` (it links
    WiredTiger transitively through secantus-wt).
    """
    env = _rust_env()
    c.run(f"cd {_RUST_STORAGE_DIR} && cargo fmt --check", pty=True, env=env)
    c.run(f"cd {_RUST_STORAGE_DIR} && cargo clippy --all-targets -- -D warnings", pty=True, env=env)
    c.run(f"cd {_RUST_STORAGE_DIR} && cargo test", pty=True, env=env)


@task(name="rust-adapter-test")
def rust_adapter_test(c: Context) -> None:
    """fmt/clippy/test the secantus-storage-adapter crate.

    Excluded from the clean workspace (links WiredTiger transitively), so the
    clean-workspace ``rust-test`` never covers it — run this after adapter
    changes. Same WiredTiger / libclang prerequisites as ``rust-wt-test``.
    """
    env = _rust_env()
    c.run(f"cd {_RUST_ADAPTER_DIR} && cargo fmt --check", pty=True, env=env)
    c.run(f"cd {_RUST_ADAPTER_DIR} && cargo clippy --all-targets -- -D warnings", pty=True, env=env)
    c.run(f"cd {_RUST_ADAPTER_DIR} && cargo test", pty=True, env=env)


@task(name="rust-storage-py")
def rust_storage_py(c: Context) -> None:
    """Build the _secantus_storage extension and run its Python smoke test.

    Builds the WiredTiger-linking wheel with maturin, then runs the smoke test in
    an isolated interpreter (pymongo + the freshly built wheel) so it doesn't need
    the project's own WiredTiger extension installed. Same WiredTiger / libclang
    prerequisites as ``rust-wt-test``.
    """
    c.run(
        f"cd {_RUST_STORAGE_PY_DIR} && uv tool run maturin build --release --out dist",
        pty=True,
        env=_rust_env(),
    )
    wheels = sorted(glob.glob(f"{_RUST_STORAGE_PY_DIR}/dist/*.whl"))
    if not wheels:
        raise SystemExit("no wheel produced by maturin")
    c.run(
        "uv run --no-project --reinstall-package secantus-storage "
        f"--with pymongo --with pytest --with {shlex.quote(wheels[-1])} "
        "python -m pytest tests/test_rust_storage_smoke.py "
        "-o addopts= -p no:cacheprovider -q",
        pty=True,
    )


@task(name="rust-binary-test")
def rust_binary_test(c: Context) -> None:
    """Build the standalone ``secantusdb`` binary and run its smoke test.

    Builds the WiredTiger-linking bin crate, then launches it from
    tests/test_rust_binary_smoke.py (ephemeral port, pymongo round-trip,
    clean SIGTERM exit) in an isolated interpreter. Same WiredTiger /
    libclang prerequisites as ``rust-wt-test``.
    """
    c.run(f"cd {_RUST_BINARY_DIR} && cargo build", pty=True, env=_rust_env())
    c.run(
        "uv run --no-project --with pymongo --with pytest "
        "python -m pytest tests/test_rust_binary_smoke.py "
        "-o addopts= -p no:cacheprovider -q",
        pty=True,
    )


@task(name="rust-binary-build")
def rust_binary_build(c: Context, release: bool = False) -> None:
    """Build the standalone ``secantusdb`` binary (no smoke test) and print its path.

    The fast inner-loop build for the ten non-pymongo driver gauges, which run
    against the daemon binary. ``--release`` for an optimised build. WiredTiger /
    libclang prerequisites as ``rust-wt-test`` (auto-filled when unset).
    """
    flag = " --release" if release else ""
    c.run(f"cd {_RUST_BINARY_DIR} && cargo build{flag}", pty=True, env=_rust_env())
    sub = "release" if release else "debug"
    print(f"binary: {_RUST_BINARY_DIR}/target/{sub}/secantusdb")


@task(name="rust-server-build")
def rust_server_build(c: Context) -> None:
    """Rebuild the embedded ``_secantus_server`` extension into the project venv.

    Required before ``invoke validate --server rust`` / ``validate-one`` can see
    Rust-server code changes — the pymongo gauge imports the in-process
    ``RustServer`` from this extension, so a stale build silently measures old
    code. Rebuilds the WiredTiger-linking storage engine and reinstalls the
    editable package. WiredTiger / libclang prerequisites as ``rust-wt-test``.
    """
    c.run(
        "SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON "
        "uv sync --extra dev --extra admin --reinstall-package secantusdb",
        pty=True,
        env=_rust_env(),
    )


@task(
    name="rust-stress",
    help={
        "workers": "Concurrent worker threads (default 16).",
        "iters": "Write-stop-restore cycles per worker (default 5).",
    },
)
def rust_stress(c: Context, workers: int = 16, iters: int = 5) -> None:
    """Hammer many concurrent embedded WiredTiger instances and assert no panic.

    Runs ``bench/wt_stress.py``: each worker spins up an embedded Rust server,
    writes CRUD history, stops it, and rebuilds the DB with the Python restore
    tool — two WT connections per cycle, ``workers`` in parallel. Exercises the
    cross-server load that made WiredTiger's eviction thread panic under
    ``pytest -n auto``. Needs the ``_secantus_server`` extension (run
    ``rust-server-build`` first). Exits non-zero if any cycle fails.
    """
    c.run(
        f"uv run --no-sync python -m bench.wt_stress --workers {int(workers)} --iters {int(iters)}",
        pty=True,
        env=_rust_env(),
    )


@task(name="rust-bump")
def rust_bump(c: Context, to: str) -> None:
    """Bump every Rust crate (Cargo.toml + Cargo.lock) to ``--to`` in lockstep.

    Pass the full new version, e.g. ``invoke rust-bump --to 0.5.3-beta.46``.
    All Rust crates carry the same version (the WT-linked crates can't inherit a
    ``workspace.package`` version), so they're rewritten together. Reads the
    current version from ``secantus-core/Cargo.toml``. Remember: bumping the
    patch/minor/major component resets the beta label to 0
    (``0.5.3-beta.46`` → ``0.5.4-beta.0``).
    """
    core = pathlib.Path("crates/secantus-core/Cargo.toml")
    m = re.search(r'^version = "([^"]+)"', core.read_text(), re.M)
    if not m:
        raise SystemExit("could not read current version from secantus-core/Cargo.toml")
    old = m.group(1)
    if old == to:
        print(f"already at {to}")
        return
    changed = 0
    crates = pathlib.Path("crates")
    files = list(crates.rglob("Cargo.toml")) + list(crates.rglob("Cargo.lock"))
    for p in files:
        # Skip anything under a build-output target/ dir.
        if "target" in p.parts:
            continue
        text = p.read_text()
        if old in text:
            p.write_text(text.replace(old, to))
            changed += 1
    print(f"bumped {changed} files: {old} -> {to}")


@task(
    name="rust-gate",
    help={
        "pytest": "Also run the Python test suite (default True; --no-pytest to skip).",
        "deselect": "Comma-separated pytest node ids to deselect from the suite (default none).",
    },
)
def rust_gate(c: Context, pytest: bool = True, deselect: str = "") -> None:
    """Full pre-commit gate for Rust-server changes, in one task.

    Runs, in order: the clean-workspace fmt/clippy/test (``rust-test``); each
    WiredTiger-linked crate's fmt/clippy/test that the clean workspace can't
    cover (``rust-wt-test`` / ``rust-storage-test`` / ``rust-adapter-test``); the
    leaf-engine parity suites (``rust-parity``); and — unless ``--no-pytest`` —
    the full Python suite. This is the sequence that must be green before
    committing Rust work; previously assembled by hand every time.
    """
    rust_test(c)
    rust_wt_test(c)
    rust_storage_test(c)
    rust_adapter_test(c)
    rust_parity(c)
    if pytest:
        cmd = "uv run --no-sync --extra dev --extra admin python -m pytest -q"
        for nodeid in (d for d in deselect.split(",") if d.strip()):
            cmd += f" --deselect {shlex.quote(nodeid.strip())}"
        c.run(cmd, pty=True, env=_rust_env())


@task(
    name="rust-ship",
    help={
        "message": "Commit message (include the Co-Authored-By trailer yourself).",
        "push": "Push HEAD:main after committing (default True; --no-push to stop at commit).",
        "pytest": "Run the Python suite as part of the gate (default True).",
        "deselect": "Passed through to rust-gate (see its help).",
    },
)
def rust_ship(
    c: Context,
    message: str,
    push: bool = True,
    pytest: bool = True,
    deselect: str = "",
) -> None:
    """One-shot: run the full gate, then commit tracked changes and push HEAD:main.

    Runs ``rust-gate`` (aborts the ship if anything is red), stages all tracked
    modifications **except** the vendored submodules and untracked gauge
    artefacts, commits with ``--message``, and — unless ``--no-push`` — pushes
    the current branch to ``main`` (this repo's feature-branch → main flow). New
    (untracked) files must be ``git add``-ed first; ``-u``-style staging only
    captures changes to already-tracked files plus deletions.
    """
    rust_gate(c, pytest=pytest, deselect=deselect)
    # Stage tracked changes only, excluding the vendored submodule pointers and
    # the untracked gauge outputs / scratch dirs that litter a dev tree.
    c.run(
        "git add -A -- . "
        "':(exclude)vendor' "
        "':(exclude)secantus-data' "
        "':(exclude,glob)docs/validation-report-*-rust-server.md'",
        pty=True,
    )
    c.run(f"git commit -m {shlex.quote(message)}", pty=True)
    if push:
        c.run("git push origin HEAD:main", pty=True)
