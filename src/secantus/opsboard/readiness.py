"""Release readiness checks.

A release pushes a tag that triggers publication — it is irreversible and
outward-facing, so the Ops Board refuses to start one until the repo is in a
releasable state. These checks mirror what ``release-prepare`` itself asserts,
surfaced *before* you commit to the run rather than as a mid-flight abort.

Every check is local (git + file reads) except the optional CI check, which is
advisory: if ``gh`` is unavailable the check reports ``unknown`` and does not
block, because absence of evidence isn't evidence of red.

**Fail-safe policy.** A *blocking* check must come back definitively ``ok`` to
permit a release; ``unknown`` blocks just as ``bad`` does. If we can't read the
branch, the tree state or the changelog, we don't know it's safe to publish —
and the failure mode of guessing wrong is an irreversible tag push. Advisory
checks never block.
"""

from __future__ import annotations

import subprocess
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from pathlib import Path

Runner = Callable[[Sequence[str], str], tuple[int, str]]


def _git(argv: Sequence[str], cwd: str) -> tuple[int, str]:
    try:
        proc = subprocess.run(["git", *argv], cwd=cwd, capture_output=True, text=True, timeout=15)
    except (OSError, subprocess.SubprocessError):
        return 1, ""
    return proc.returncode, proc.stdout


@dataclass(frozen=True)
class Check:
    name: str
    state: str  # "ok" | "bad" | "unknown"
    detail: str
    blocking: bool = True

    @property
    def blocks(self) -> bool:
        """Fail-safe: a blocking check must be definitively ``ok`` to permit a
        release. ``unknown`` blocks too — a release is irreversible and
        outward-facing, so "we couldn't verify this" must not silently proceed.
        Advisory checks (``blocking=False``, e.g. CI) never block.
        """
        return self.blocking and self.state != "ok"


def _branch_check(root: str, run: Runner) -> Check:
    code, out = run(["branch", "--show-current"], root)
    branch = out.strip()
    if code != 0:
        return Check("On main", "unknown", "could not read current branch")
    if branch == "main":
        return Check("On main", "ok", "main")
    return Check("On main", "bad", f"on {branch!r} — releases must run on main")


def _clean_tree_check(root: str, run: Runner) -> Check:
    code, out = run(["status", "--porcelain"], root)
    if code != 0:
        return Check("Working tree clean", "unknown", "could not read git status")
    # Vendored-submodule drift is tolerated: it never enters the release commit
    # (release-prepare stages only the version files). Anything else is real.
    bad = [
        line
        for line in out.splitlines()
        if line and not (line.startswith((" m ", " M ")) and "vendor/" in line)
    ]
    if not bad:
        return Check("Working tree clean", "ok", "no uncommitted changes")
    return Check(
        "Working tree clean",
        "bad",
        f"{len(bad)} uncommitted change(s): " + ", ".join(x[3:] for x in bad[:3]),
    )


def _sync_check(root: str, run: Runner) -> Check:
    code, out = run(["rev-list", "--left-right", "--count", "main...origin/main"], root)
    if code != 0 or not out.strip():
        return Check("In sync with origin", "unknown", "could not compare with origin/main")
    try:
        ahead, behind = (int(x) for x in out.split())
    except ValueError:
        return Check("In sync with origin", "unknown", "unexpected rev-list output")
    if ahead == 0 and behind == 0:
        return Check("In sync with origin", "ok", "main == origin/main")
    return Check(
        "In sync with origin",
        "bad",
        f"{ahead} ahead / {behind} behind origin/main — fetch and reconcile first",
    )


def _changelog_check(root: str) -> Check:
    d = Path(root) / "changelog.d"
    if not d.is_dir():
        return Check("Changelog fragments", "unknown", "no changelog.d/ directory")
    fragments = [p.name for p in d.glob("*.md") if p.name.lower() != "readme.md"]
    if fragments:
        return Check(
            "Changelog fragments",
            "ok",
            f"{len(fragments)} pending: " + ", ".join(sorted(fragments)[:3]),
        )
    return Check(
        "Changelog fragments",
        "bad",
        "none pending — a release with no changelog entry isn't ready",
    )


def _ci_check(github: object | None) -> Check:
    if github is None:
        return Check("Recent CI on main", "unknown", "no GitHub client", blocking=False)
    try:
        runs = github.recent_runs(limit=20)  # type: ignore[attr-defined]
    except Exception:  # pragma: no cover - defensive
        runs = []
    main_runs = [r for r in runs if getattr(r, "branch", "") == "main"]
    if not main_runs:
        return Check(
            "Recent CI on main",
            "unknown",
            "no data (is gh installed and authenticated?)",
            blocking=False,
        )
    failures = [r for r in main_runs if r.bucket == "failure"]
    if failures:
        return Check(
            "Recent CI on main",
            "bad",
            f"{len(failures)} recent failing run(s): "
            + ", ".join(sorted({r.name for r in failures})[:3]),
            blocking=False,
        )
    return Check("Recent CI on main", "ok", f"{len(main_runs)} recent run(s), none failing")


def collect(
    repo_root: str | Path, *, github: object | None = None, runner: Runner | None = None
) -> list[Check]:
    root = str(repo_root)
    run = runner or _git
    return [
        _branch_check(root, run),
        _clean_tree_check(root, run),
        _sync_check(root, run),
        _changelog_check(root),
        _ci_check(github),
    ]


def blockers(checks: Sequence[Check]) -> list[Check]:
    return [c for c in checks if c.blocks]


__all__ = ["Check", "collect", "blockers"]
