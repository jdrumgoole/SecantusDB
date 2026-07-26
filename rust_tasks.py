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
import subprocess

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


def _find_wt_build() -> pathlib.Path | None:
    """Locate a *complete* vendored WiredTiger build dir — one holding BOTH
    ``include/wiredtiger.h`` and a ``libwiredtiger.{a,dylib,so}``.

    ``secantus-wt``'s build.rs needs both; a dir with only the archive (as a bare
    ``/tmp/wt-build`` sometimes is) fails its header probe. We check, in order:
    the dev-sandbox ``/tmp/wt-build``; this checkout's CMake output
    (``build/*/wt-build``); and — crucially for a git worktree, which never has
    its own ``build/`` — the *main* worktree's output (found via
    ``--git-common-dir``), so ``./inv rust-*`` works in a worktree by reusing the
    WiredTiger the primary checkout already compiled.
    """
    roots = ["/tmp/wt-build"]
    here = pathlib.Path.cwd()
    roots += sorted(glob.glob(str(here / "build" / "*" / "wt-build")))
    # The main worktree's checkout root is the parent of the common git dir
    # (``<main>/.git``); a linked worktree's ``.git`` is a file, so this resolves
    # to the primary checkout even when we're running inside a worktree.
    common = subprocess.run(
        ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()
    if common:
        main_root = pathlib.Path(common).parent
        if main_root != here:
            roots += sorted(glob.glob(str(main_root / "build" / "*" / "wt-build")))
    for root in roots:
        d = pathlib.Path(root)
        header = d / "include" / "wiredtiger.h"
        lib = any((d / f"libwiredtiger{ext}").exists() for ext in (".a", ".dylib", ".so"))
        if header.exists() and lib:
            return d
    return None


def _rust_env() -> dict[str, str]:
    """Build-environment defaults for the WiredTiger-linking Rust crates.

    The WT-linked crates (``secantus-wt`` / ``-storage`` / ``-storage-adapter`` /
    ``secantusdb`` / the embedded ``_secantus_server``) need a vendored
    WiredTiger build and libclang for bindgen. CI exports these explicitly;
    locally they're easy to forget, so this fills in conventional values **only**
    when the caller hasn't already set them. Returns a dict to hand to
    ``c.run(..., env=...)`` (invoke merges it over the inherited environment).

    build.rs honours ``SECANTUS_WT_INCLUDE`` / ``SECANTUS_WT_LIB`` (a bare
    ``WT_BUILD_DIR`` is *not* read by it), so we resolve a complete WT build via
    ``_find_wt_build`` and export those two — which is what makes ``./inv
    rust-*`` build inside a git worktree (it reuses the main checkout's WT).
    """
    env: dict[str, str] = {}
    if not (os.environ.get("SECANTUS_WT_INCLUDE") and os.environ.get("SECANTUS_WT_LIB")):
        wt = _find_wt_build()
        if wt is not None:
            env["SECANTUS_WT_INCLUDE"] = str(wt / "include")
            env["SECANTUS_WT_LIB"] = str(wt)
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
        # shapely / s2sphere / dateutil: the pure-Python geo + date paths the
        # aggregate/query parity corpora exercise — without them the Python
        # side of the geo curated cases errors with ModuleNotFoundError.
        "--with shapely --with s2sphere --with python-dateutil "
        "python -m pytest tests/test_rust_sortkey_parity.py tests/test_rust_query_parity.py "
        "tests/test_rust_update_parity.py tests/test_rust_expressions_parity.py "
        "tests/test_rust_projection_parity.py tests/test_rust_diff_parity.py "
        "tests/test_rust_aggregate_parity.py "
        "tests/test_rust_group_field_pushdown.py "
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


@task(name="rust-test-one")
def rust_test_one(
    c: Context,
    crate: str = "secantus-storage-adapter",
    test: str = "",
    name: str = "",
    nocapture: bool = False,
) -> None:
    """Run a single Rust test in a WiredTiger-linked crate (no fmt/clippy).

    The fast inner loop for iterating on one test. ``--crate`` is the dir under
    ``crates/`` (default the adapter crate, where the command-layer integration
    tests live); ``--test`` selects one integration-test binary (the file stem
    under that crate's ``tests/``); ``--name`` filters by test name; ``--nocapture``
    shows stdout / ``eprintln!``. WiredTiger / libclang prerequisites are the same
    as ``rust-wt-test`` (auto-filled when unset).

    Examples::

        inv rust-test-one --test command_crud_wt --name update_bit_operator
        inv rust-test-one --crate secantus-storage --test natural_order --nocapture
    """
    cmd = f"cd crates/{shlex.quote(crate)} && cargo test"
    if test:
        cmd += f" --test {shlex.quote(test)}"
    if name:
        cmd += f" {shlex.quote(name)}"
    if nocapture:
        cmd += " -- --nocapture"
    c.run(cmd, pty=True, env=_rust_env())


@task(name="rust-fmt")
def rust_fmt(c: Context) -> None:
    """`cargo fmt` the clean workspace + the WiredTiger-linked crates.

    The WT-linked crates (``-wt`` / ``-storage`` / ``-storage-adapter``) are
    excluded from the ``crates`` workspace, so a single workspace ``cargo fmt``
    doesn't reach them — format each separately. WiredTiger / libclang
    prerequisites as ``rust-wt-test`` (auto-filled).
    """
    env = _rust_env()
    c.run(f"cd {_RUST_WORKSPACE_DIR} && cargo fmt", pty=True, env=env)
    for d in (_RUST_WT_DIR, _RUST_STORAGE_DIR, _RUST_ADAPTER_DIR):
        c.run(f"cd {d} && cargo fmt", pty=True, env=env)


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
    print(f"binary: {_RUST_BINARY_DIR}/target/{sub}/secantusd-rs")


@task(name="rust-server-build")
def rust_server_build(c: Context) -> None:
    """Rebuild the embedded ``_secantus_server`` extension into the project venv.

    Required before ``invoke validate --server rust`` / ``validate-one`` can see
    Rust-server code changes — the pymongo gauge imports the in-process
    ``RustServer`` from this extension, so a stale build silently measures old
    code. Rebuilds the WiredTiger-linking storage engine and reinstalls the
    editable package. WiredTiger / libclang prerequisites as ``rust-wt-test``.

    ``--inexact`` is load-bearing: this task's job is to rebuild ONE package,
    but a bare ``uv sync --extra dev --extra admin`` also *prunes* everything
    outside that extra set. That silently removed pelican / boto3 (breaking the
    website publish) and ``secantus-core`` (making the parity suite skip) for
    anyone who also had the sql / rust / website extras installed. ``--inexact``
    leaves extraneous packages alone so the rebuild is purely additive.
    """
    c.run(
        "SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON "
        "uv sync --inexact --extra dev --extra admin --reinstall-package secantusdb",
        pty=True,
        env=_rust_env(),
    )


def _llvm_profdata() -> str:
    """Locate ``llvm-profdata`` from the active Rust toolchain (llvm-tools-preview)."""
    from shutil import which

    sysroot = subprocess.run(
        ["rustc", "--print", "sysroot"], capture_output=True, text=True, check=True
    ).stdout.strip()
    for p in glob.glob(f"{sysroot}/lib/rustlib/*/bin/llvm-profdata"):
        return p
    found = which("llvm-profdata")
    if found:
        return found
    raise SystemExit(
        "llvm-profdata not found — run: rustup component add llvm-tools-preview"
    )


@task(name="rust-pgo-refresh")
def rust_pgo_refresh(c: Context) -> None:
    """Regenerate the committed PGO profile for the embedded ``_secantus_server``.

    Two-stage profile-guided optimization (the checked-in-profile half of the
    PGO split): build the extension instrumented, drive the six-workload
    benchmark against it to collect a profile, merge it ``--sparse``, and commit
    it as ``crates/pgo/_secantus_server.profdata.tar.gz``. CMake feeds that
    profile to the normal release build via ``-Cprofile-use`` (see the server-ext
    block in ``CMakeLists.txt``). Measured ~12-19% on the write/aggregate paths
    over thin-LTO alone (``tasks/rust-perf-findings.md`` Finding 8).

    Re-run when the Rust hot paths change materially — the profile is only a
    hint (unmatched functions are ignored via ``-pgo-warn-missing-function``), so
    a stale profile is safe, just less optimal. Needs ``llvm-tools-preview``
    (``rustup component add llvm-tools-preview``) plus the WiredTiger / libclang
    build env (auto-filled like ``rust-server-build``).
    """
    import tarfile
    import tempfile

    profdata_tool = _llvm_profdata()
    committed = pathlib.Path("crates/pgo/_secantus_server.profdata.tar.gz")
    committed.parent.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="secantus-pgo-") as tmp:
        prof_dir = pathlib.Path(tmp)

        print("=== PGO stage 1: instrumented build (profile-generate) ===")
        env1 = _rust_env()
        env1["SECANTUS_PGO_GENERATE"] = str(prof_dir)
        c.run(
            "SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON "
            "uv sync --inexact --extra dev --extra admin --reinstall-package secantusdb",
            pty=True,
            env=env1,
        )

        print("=== PGO stage 2: collecting the profile (benchmark workloads) ===")
        env2 = _rust_env()
        env2["LLVM_PROFILE_FILE"] = str(prof_dir / "pgo-%p-%m.profraw")
        c.run(
            "uv run --no-sync python -m bench.compare_servers --n 10000 --reps 5 --no-mongod",
            pty=True,
            env=env2,
        )

        raws = glob.glob(str(prof_dir / "*.profraw"))
        if not raws:
            raise SystemExit(
                "PGO: no .profraw produced — the instrumented build may not have run."
            )
        merged = prof_dir / "merged.profdata"
        print(f"=== PGO: merging {len(raws)} profraw → sparse profdata ===")
        c.run(
            f"{shlex.quote(profdata_tool)} merge --sparse "
            f"-o {shlex.quote(str(merged))} "
            + " ".join(shlex.quote(r) for r in raws)
        )
        with tarfile.open(committed, "w:gz") as tf:
            tf.add(merged, arcname="_secantus_server.profdata")
        print(f"=== PGO: wrote {committed} ({committed.stat().st_size // 1024} KiB) ===")

    print("=== PGO stage 3: rebuild the shipped extension with -Cprofile-use ===")
    rust_server_build(c)


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
def rust_bump(c: Context, to: str = "") -> None:
    """Bump every Rust crate (Cargo.toml + Cargo.lock) in lockstep.

    With no ``--to``, increments the beta pre-release
    (``0.5.3-beta.N`` → ``0.5.3-beta.(N+1)``) — the common per-slice bump. Pass
    ``--to`` for an explicit version, e.g. ``invoke rust-bump --to 0.5.4-beta.0``
    after a patch bump (which resets the beta label to 0 per CLAUDE.md). All Rust
    crates carry the same version (the WT-linked crates can't inherit a
    ``workspace.package`` version), so they're rewritten together. Reads the
    current version from ``secantus-core/Cargo.toml``.
    """
    core = pathlib.Path("crates/secantus-core/Cargo.toml")
    m = re.search(r'^version = "([^"]+)"', core.read_text(), re.M)
    if not m:
        raise SystemExit("could not read current version from secantus-core/Cargo.toml")
    old = m.group(1)
    if not to:
        bm = re.match(r"^(.*-beta\.)(\d+)$", old)
        if not bm:
            raise SystemExit(f"current version {old!r} is not '-beta.N'; pass --to=<version>")
        to = f"{bm.group(1)}{int(bm.group(2)) + 1}"
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
    steps = 8 if pytest else 7
    # ``==> [k/N] label`` phase markers: drive the Ops Board progress stepper
    # (secantus.opsboard.progress) and give a clear CLI banner per sub-step.
    print(f"==> [1/{steps}] cargo (clean ws)", flush=True)
    rust_test(c)
    print(f"==> [2/{steps}] wt crate", flush=True)
    rust_wt_test(c)
    print(f"==> [3/{steps}] storage crate", flush=True)
    rust_storage_test(c)
    print(f"==> [4/{steps}] adapter crate", flush=True)
    rust_adapter_test(c)
    print(f"==> [5/{steps}] parity", flush=True)
    rust_parity(c)
    # Python lint + format — the parity suites are Python, but the cargo
    # fmt/clippy above only cover the Rust workspace, so a ruff slip in a parity
    # test (e.g. a too-long line) would pass the gate and red CI. Mirror CI's
    # `Lint` / `Format check` steps so it's caught before push.
    print(f"==> [6/{steps}] ruff check", flush=True)
    c.run("uv run ruff check src tests", pty=True)
    print(f"==> [7/{steps}] ruff format", flush=True)
    c.run("uv run ruff format --check src tests", pty=True)
    if pytest:
        print(f"==> [8/{steps}] pytest", flush=True)
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


# --- Canonical repro + PR-lifecycle tasks ---------------------------------
#
# These exist so the operations done on every rust-server slice — running an
# ad-hoc pymongo repro against the standalone binary, and the commit → push → PR
# → watch → merge lifecycle — are each a single ``./inv …`` command on the
# ``Bash(./inv *)`` allowlist, instead of bespoke compound shell lines (which
# re-prompt every time because each is textually unique). Build/repro env is
# auto-filled by ``_rust_env``, so never prefix these with ``export …``.


@task(
    name="rust-repro",
    help={
        "script": "path to a pymongo repro .py (run with `--binary <secantusdb>`)",
        "release": "build/use the release binary (default: True)",
    },
)
def rust_repro(c: Context, script: str, release: bool = True) -> None:
    """Build the standalone ``secantusdb`` binary, then run a pymongo repro script
    against it (the script is passed ``--binary <path>``). Write the repro with the
    editor, then ``./inv rust-repro <script>`` — no bespoke ``uv run python …``."""
    sub = "release" if release else "debug"
    flag = " --release" if release else ""
    c.run(f"cd {_RUST_BINARY_DIR} && cargo build{flag}", pty=True, env=_rust_env())
    binpath = f"{_RUST_BINARY_DIR}/target/{sub}/secantusd-rs"
    c.run(f"uv run python {shlex.quote(script)} --binary {binpath}", pty=True, env=_rust_env())


@task(name="gh-watch", help={"pr": "PR number"})
def gh_watch(c: Context, pr: str) -> None:
    """Watch a PR's CI checks to completion, then print the final states."""
    c.run(f"gh pr checks {shlex.quote(str(pr))} --watch --interval 30", pty=True, warn=True)
    c.run(f"gh pr checks {shlex.quote(str(pr))}", pty=True, warn=True)


@task(
    name="gh-merge",
    help={"pr": "PR number", "sync-branch": "local branch to reset to origin/main"},
)
def gh_merge(c: Context, pr: str, sync_branch: str = "rust-tasks") -> None:
    """Squash-merge a PR (keeping the remote branch), then fast-sync the local
    working branch to the new ``origin/main``. Replaces the bespoke
    ``gh pr merge … ; git fetch ; git checkout ; git reset --hard`` sequence."""
    c.run(f"gh pr merge {shlex.quote(str(pr))} --squash --delete-branch=false", pty=True)
    c.run("git fetch origin -q", pty=True)
    c.run(f"git checkout {shlex.quote(sync_branch)}", pty=True, warn=True)
    c.run("git reset --hard origin/main", pty=True)
    c.run("git log --oneline -2", pty=True)


@task(
    name="gh-ship",
    help={
        "branch": "feature branch to commit/push and open the PR from",
        "paths": "git pathspec to stage (default: everything but vendor / data / reports)",
        "msg-file": "file holding the commit message (line 1 becomes the PR title)",
        "body-file": "file holding the PR body markdown (PR skipped if absent)",
        "base": "PR base branch (default: main)",
    },
)
def gh_ship(
    c: Context,
    branch: str,
    paths: str = "",
    msg_file: str = "/tmp/secantus-commit.txt",
    body_file: str = "/tmp/secantus-pr.md",
    base: str = "main",
) -> None:
    """Stage ``paths``, commit with the message in ``msg_file``, push ``branch``,
    and open a PR (title = first line of ``msg_file``, body from ``body_file``).
    Write the two files with the editor first, then one ``./inv gh-ship -b
    <branch>`` runs the whole commit → push → PR lifecycle on the allowlist.

    The default ``paths`` stages **everything** tracked-or-new except ``vendor``,
    local ``secantus-data``, and the regenerated ``*-rust-server.md`` gauge
    reports — so a slice's crates + parity tests + backlog edits all go in
    together (passing ``--paths crates`` only staged crates and silently dropped
    sibling test/backlog changes)."""
    title = pathlib.Path(msg_file).read_text().splitlines()[0]
    if paths:
        c.run(f"git add {paths}", pty=True)
    else:
        c.run(
            "git add -A -- . "
            "':(exclude)vendor' "
            "':(exclude)secantus-data' "
            "':(exclude,glob)docs/validation-report-*-rust-server.md'",
            pty=True,
        )
    c.run(f"git commit -F {shlex.quote(msg_file)}", pty=True)
    c.run(f"git push -u origin {shlex.quote(branch)}", pty=True)
    if pathlib.Path(body_file).exists():
        c.run(
            f"gh pr create --base {shlex.quote(base)} --head {shlex.quote(branch)} "
            f"--title {shlex.quote(title)} --body-file {shlex.quote(body_file)}",
            pty=True,
        )
