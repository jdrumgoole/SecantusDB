"""Is the artifact you are about to run built from the tree you are testing?

**Why this is not in `tests/conftest.py`.** It used to be. PR #1586 put a
collection-time check there covering all four built artifacts — `_secantus_core`,
`_secantus_server`, `secantusd-pg`, `secantusd-rs` — after nine false-regression
diagnoses in one day. That check works, and it only ever runs under pytest.

The trouble is that this repo's primary bug-finding method is NOT pytest. CLAUDE.md
says it plainly: *run it against the reference server, don't reason about the
source.* An ad-hoc differential probe, a driver gauge and a benchmark all launch
these artifacts directly, and every one of them was unprotected — so on
2026-09-28 a probe of `secantusd-pg` ran against a binary built from a different
`crates/` tree, and the only thing that caught it was someone reading
`--version` by hand. That is exactly the state #1586's own docstring complains
about: *"the evidence was sitting in `--version` output that no automated thing
read."*

So the comparison lives here, importable, and `tests/conftest.py` is one caller
among several rather than the owner.

**What it compares.** A binary stamps the git tree hash of `crates/` it was
built from (#1492) and prints it as a ``tree:`` line in ``--version``; the
extensions carry the same thing as ``__source_tree__``. Against that goes
``git rev-parse HEAD:crates``.

Two properties are deliberate and worth keeping:

- **HEAD, not the working copy.** Someone mid-edit in `crates/` legitimately has
  an artifact that differs from their uncommitted changes. Failing their run for
  that teaches people to disable the check, which is worse than no check.
- **Silence when either side is unknown.** An artifact predating the stamp, or a
  checkout git cannot read, must not fail anybody's run — it abstains, because
  it cannot judge.

**mtime is not a substitute, in either direction.** The stale binary of
2026-09-28 was dated three days *after* the last commit touching its crates: it
had been relinked without its sources changing. Only the stamp answers.

And read the stamp as what it is — a TREE hash, not a commit. ``git log
<tree>..HEAD`` accepts one and prints nonsense, which is how "a different crates
tree" got mis-reported that day as "2769 commits behind".
"""

from __future__ import annotations

import pathlib
import subprocess

#: The repository this file lives in.
REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent

#: Rebuild commands per artifact — the official invoke tasks, verified against
#: `rust_tasks.py` rather than guessed. An earlier draft of this table told
#: anyone with a stale `secantusd-rs` to run `rust-server-build`, which rebuilds
#: the embedded EXTENSION instead; a check that prints the wrong remedy sends
#: people down a ten-minute detour and teaches them to distrust it.
REBUILD_CORE_CMD = "uv run python -m invoke sync"
REBUILD_SERVER_CMD = "./inv rust-server-build"
REBUILD_PGSERVER_CMD = "./inv rust-pgserver-build"
REBUILD_RS_CMD = "./inv rust-binary-build"

#: Where each binary lands, relative to the repo root.
PGSERVER_REL = "crates/secantus-pgserver/target/debug/secantusd-pg"
RS_REL = "crates/secantusdb/target/debug/secantusd-rs"

#: What a stale one has actually cost, per artifact. Carried in the failure text
#: because an abstract warning gets waved past and a specific one does not.
COST_SERVER = (
    "On 2026-09-27 a stale one produced 3 failures and a hang in a colleague's "
    "test file, reported as their bug; 13/13 passed after a rebuild with no "
    "code change."
)
COST_BINARY = (
    "On 2026-09-27 a stale one caused six separate false-regression diagnoses in one session."
)


def committed_crates_tree(repo_root: pathlib.Path | None = None) -> str:
    """The checkout's tree hash for all of `crates/`, or "" if git can't say."""
    root = repo_root or REPO_ROOT
    try:
        out = subprocess.run(
            ["git", "rev-parse", "HEAD:crates"],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError):
        return ""
    return out.stdout.strip() if out.returncode == 0 else ""


def binary_source_tree(path: pathlib.Path) -> str:
    """The tree hash a built binary reports via ``--version``, or "".

    An older binary predating the stamp simply has no ``tree:`` line, and the
    check abstains rather than guessing.
    """
    try:
        out = subprocess.run([str(path), "--version"], capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return ""
    if out.returncode != 0:
        return ""
    for line in out.stdout.splitlines():
        if line.startswith("tree:"):
            return line.split(":", 1)[1].strip()
    return ""


def stale_artifact_message(
    artifact: str, built: str, current: str, rebuild: str, cost: str
) -> str | None:
    """The failure text when `built` and `current` disagree, else ``None``.

    Silence when either side is unknown is the important half — see the module
    docstring.
    """
    if not built or not current or built == current:
        return None
    return (
        f"the installed `{artifact}` was built from different sources than this "
        f"checkout:\n"
        f"    artifact: {built}\n"
        f"    checkout: {current}\n"
        f"Running against a stale artifact produces failures that look like real "
        f"regressions and are not. {cost}\nRebuild it:\n"
        f"    {rebuild}\n"
        "(A worktree without `vendor/wiredtiger` cannot build the WT-linked "
        "artifacts at all — `git submodule update --init --depth 1 "
        "vendor/wiredtiger` first, or delete the stale one, which makes its "
        "tests skip honestly instead of failing falsely.)"
    )


def resolve_binary(path: pathlib.Path) -> pathlib.Path:
    """`path`, or its `.exe` sibling on Windows.

    Cargo emits `.exe` on Windows, so the bare name NEVER exists there. That one
    missing suffix made the pytest guard skip both binaries entirely on the one
    platform where staleness had already bitten, and made all 1,194 tests in
    `test_rust_pgserver_slice.py` skip. It cost an eighth false diagnosis on
    2026-09-28. Every caller resolving a binary path goes through here, so the
    suffix is handled once rather than remembered four times.
    """
    if path.exists():
        return path
    candidate = path.with_suffix(".exe")
    return candidate if candidate.exists() else path


def binary_staleness(
    path: pathlib.Path,
    artifact: str,
    rebuild: str,
    *,
    cost: str = COST_BINARY,
    repo_root: pathlib.Path | None = None,
) -> str | None:
    """The failure text for a BINARY at `path`, or ``None`` if it is fine.

    Abstains — returns ``None`` — when the binary does not exist. A caller that
    needs it to exist is already saying so with its own clearer message; this
    check has no business being the one that reports a missing build. The `.exe`
    suffix is resolved for you, so a Windows caller cannot silently check a
    filename that cannot exist.
    """
    binary = resolve_binary(path)
    if not binary.exists():
        return None
    return stale_artifact_message(
        artifact, binary_source_tree(binary), committed_crates_tree(repo_root), rebuild, cost
    )


def extension_staleness(
    module: object,
    artifact: str,
    rebuild: str,
    *,
    cost: str = COST_SERVER,
    repo_root: pathlib.Path | None = None,
) -> str | None:
    """The failure text for an already-imported EXTENSION, or ``None``.

    Takes the module rather than importing it, so the caller decides what a
    missing extension means — for most callers it is a normal, deliberate
    configuration.
    """
    return stale_artifact_message(
        artifact,
        getattr(module, "__source_tree__", ""),
        committed_crates_tree(repo_root),
        rebuild,
        cost,
    )


def require_fresh_pgserver(binary: pathlib.Path, *, repo_root: pathlib.Path | None = None) -> None:
    """Abort when `secantusd-pg` is stale. For probes, gauges and benchmarks.

    Raises ``SystemExit`` because that is what the probes already use for a
    binary they cannot run, and because the alternative — a warning — is what
    the last seven incidents proved nobody reads.
    """
    message = binary_staleness(binary, "secantusd-pg", REBUILD_PGSERVER_CMD, repo_root=repo_root)
    if message is not None:
        raise SystemExit(message)


def require_fresh_server_extension(module: object) -> None:
    """Abort when the embedded `_secantus_server` extension is stale."""
    message = extension_staleness(module, "_secantus_server", REBUILD_SERVER_CMD)
    if message is not None:
        raise SystemExit(message)
